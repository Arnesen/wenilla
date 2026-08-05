//! **When** a remote unit's relayed move replays — the reference's per-unit replay chain, byte-traced
//! (decision 0615; supersedes the invented estimator of 0601).
//!
//! A remote's inbound `MSG_MOVE_*` is not applied at arrival: it is given a client **fire-time** and
//! waits in the unit's queue until the clock reaches it, the dead-reckon covering the mover's own
//! timeline in between (decision 0601, wow-re `remote-apply-timing.md`). The law that picks that
//! fire-time lives in `0x618c30` + its window helper `0x618b50`, and it is **per unit** — five cells
//! on the unit's own CMovement, no manager cursor:
//!
//! | cell | here | meaning |
//! |------|------|---------|
//! | `+0xa8` | [`RelayChain::last_fire_ms`] | the fire-time the previous packet was given |
//! | `+0xac` | [`RelayChain::last_wire_ms`] | the previous packet's wire stamp |
//! | `+0xbc[32]`/`+0x13c` | [`RelayChain::ring`]/`ring_idx` | the lateness ring + its cursor |
//! | `+0x140` | [`RelayChain::base_ms`] | the lateness base the ring is stored against |
//!
//! The law, in one line: **a moving unit replays on the sender's own cadence** — `fire = prev fire +
//! (this wire stamp − prev wire stamp)`, so the spacing our client plays back is exactly the spacing
//! the stamps carry, no matter how the packets clumped in flight. The buffer that absorbs the clumping
//! is only ever re-sized while the mover is **idle with nothing queued**: there the chain re-bases by
//! the worst lateness seen over the last 32 packets, which is a de-jitter buffer sized by measured
//! jitter rather than by an estimate re-derived per packet. Either way the offset from arrival is held
//! in `[−500, +1000] ms`.
//!
//! Byte index: `0x618c30` (chain: seed `@0x618c54`, wire delta `@0x618ca0-cc2`, skew out `@0x618ccf`,
//! idle+empty gate `@0x618ce4`/`@0x618cf3`, re-base `@0x618cfc-d41`, clamps `@0x618d0d`/`@0x618d49`,
//! fire store `@0x618dcc`, due test `@0x618dd2`) · `0x618b50` (the ring) · `0x615c30` (the drain).

use benilla_protocol::{JumpInfo, TransportPose};

/// One relayed move exactly as it came off the wire — the payload of
/// [`benilla_protocol::SessionEvent::UnitMove`], before [`RelayChain`] decides *when* it applies.
#[derive(Clone)]
pub(crate) struct RelayMove {
    /// The `MovementInfo` time word — vmangos's own ms clock stamped at **receipt**
    /// (`MovementInfo::Read`: `stime = WorldTimer::getMSTime()`, a live `steady_clock`; relayed
    /// verbatim by `Write`), so every mover's stamps share one coherent server clock. The chain paces
    /// replay by the deltas between consecutive stamps.
    pub(crate) wire_ms: u32,
    pub(crate) position: [f32; 3],
    pub(crate) orientation: f32,
    pub(crate) flags: u32,
    pub(crate) pitch: f32,
    pub(crate) fall_time: u32,
    pub(crate) jump: Option<JumpInfo>,
    pub(crate) transport: Option<TransportPose>,
    /// `MSG_MOVE_HEARTBEAT` — excluded from the pre-fire reconcile lerp (the reference's `0x619090`
    /// skips tag `0x26`); it applies as an outright snap.
    pub(crate) heartbeat: bool,
}

/// One **scheduled** relayed move — the reference's queued move-event node (`0x617570`: fire-time at
/// `node[+8]`, pose at `node[+0x10]`).
#[derive(Clone)]
pub(crate) struct PendingMove {
    /// When to apply, on the real-time ms clock (the reference's `ev[+8]`).
    pub(crate) fire_ms: f64,
    /// What to apply.
    pub(crate) mv: RelayMove,
}

/// The reference's skew clamp (`0x618c30 @0x618d0d` and `@0x618d49`: `lea eax,[skew+0x1f4]; cmp
/// eax,0x5dc` saturating the biased value into `[0, 1500]`): a fire-time never lands more than 500 ms
/// **before** its packet's arrival, nor more than 1000 ms after.
const SKEW_MIN_MS: f64 = -500.0;
const SKEW_MAX_MS: f64 = 1000.0;

/// The lateness window: 32 packets — `0x618b50`'s ring at `CMovement+0xbc` with its cursor at `+0x13c`
/// masked `& 0x1f` (32 dwords is exactly `0x13c − 0xbc`).
const LATENESS_WINDOW: usize = 32;

/// The "this mover is mid-motion" mask the re-base gates on — `0x618c30 @0x618ce4`'s
/// `test [esi+0x40],0x20ff`: the four direction bits, both turn bits, both pitch bits (`0x40`/`0x80`),
/// and `FALLING` (`0x2000`). While any is set the chain keeps the sender's pacing untouched; only a
/// standing, un-queued mover re-sizes the buffer.
const BUSY_MASK: u32 = 0x20ff;

/// A remote unit's replay chain — the per-unit `CMovement` timing cells (see the module header).
/// Lives on [`super::RemoteMotion`]; one per mover, never shared (the pre-0615 estimator was a single
/// app-wide resource, which is structurally wrong: two movers behind different network paths must
/// carry different buffers).
#[derive(Clone, Default)]
pub(crate) struct RelayChain {
    /// Whether the chain has seen its first packet — the reference's `CMovement+0x40` bit 31, set
    /// alongside the first seed of the two cells below (`0x618c54-6a`).
    seeded: bool,
    /// `+0xa8` — the fire-time the previous packet was given (client ms). The whole chain hangs off
    /// this: the next fire is *this* plus the wire step.
    last_fire_ms: f64,
    /// `+0xac` — the previous packet's wire stamp. Advanced only by a **forward** step
    /// (`@0x618cb8`: `jle` skips the store), so a stale or duplicate stamp can't drag the chain back.
    last_wire_ms: u32,
    /// `+0xbc[32]` — the lateness ring: how late each of the last 32 packets ran against the chain,
    /// each stored **relative to [`Self::base_ms`] as it stood at that packet** (`0x618b50` writes
    /// `base + lateness`). That offset is what stops a spike already absorbed into the base from being
    /// charged a second time on the next re-base.
    ring: [f64; LATENESS_WINDOW],
    /// `+0x13c` — the ring write cursor, wrapped at 32.
    ring_idx: usize,
    /// `+0x140` — the lateness base the ring is stored against, i.e. the buffer the chain currently
    /// holds. Re-based to the window max whenever an idle mover with an empty queue takes a packet.
    base_ms: f64,
}

impl RelayChain {
    /// Give this relayed move its client fire-time (`0x618c30`), advancing the chain.
    ///
    /// `now_ms` is the packet's arrival on our real-time ms clock, `flags`/`queue_empty` the mover's
    /// state **before** this move applies (the reference reads `[esi+0x40]` and `[esi+0x150]` off the
    /// live CMovement at exactly this point). The caller applies the move at once if the returned
    /// fire-time is already due, else queues it for the drain — the reference's `mgr+0x128 ≥ fire`
    /// test (`@0x618dd2`), which for us is `fire ≤ now` because our arrival stamp *is* the frame clock
    /// the drain compares against.
    pub(crate) fn schedule(
        &mut self,
        wire_ms: u32,
        now_ms: f64,
        flags: u32,
        queue_empty: bool,
    ) -> f64 {
        // First packet for this mover: anchor both cells on it, so the step below is zero and the
        // move fires at arrival (`@0x618c54`).
        if !self.seeded {
            self.seeded = true;
            self.last_wire_ms = wire_ms;
            self.last_fire_ms = now_ms;
        }
        // The sender's own step between packets. Wrapping, because the server's ms clock is a `u32`
        // (vmangos uptime ms — it wraps every 49.7 days); a non-forward step contributes nothing and
        // leaves the reference stamp alone (`@0x618cb6-c2`).
        let step = wire_ms.wrapping_sub(self.last_wire_ms) as i32;
        let wire_delta = if step > 0 {
            self.last_wire_ms = wire_ms;
            f64::from(step)
        } else {
            0.0
        };
        // How much the flight took beyond the chain's own pacing. `skew` is what the reference writes
        // out at `@0x618ccf`; note `now + skew == last_fire + wire_delta`, which IS the law: replay
        // paced by the sender's stamps.
        let arrival_delta = now_ms - self.last_fire_ms;
        let mut skew = wire_delta - arrival_delta;
        let window_max = self.record_lateness(arrival_delta - wire_delta);
        // The buffer is only ever re-sized on a standing mover with nothing queued (`@0x618ce4`,
        // `@0x618cf3`): re-base by the worst lateness the window still remembers, never letting the
        // chain step backwards past the previous fire (`@0x618d3b`).
        if flags & BUSY_MASK == 0 && queue_empty {
            skew = (skew + window_max - self.base_ms).clamp(SKEW_MIN_MS, SKEW_MAX_MS);
            if now_ms + skew < self.last_fire_ms {
                skew = self.last_fire_ms - now_ms;
            }
            self.base_ms = window_max;
        }
        let fire_ms = now_ms + skew.clamp(SKEW_MIN_MS, SKEW_MAX_MS);
        self.last_fire_ms = fire_ms;
        fire_ms
    }

    /// Record this packet's lateness and return the window's worst (`0x618b50`): store
    /// `base + lateness` at the cursor, advance it mod 32, and return the max over all 32 slots (the
    /// entry just written included — the reference seeds its scan with it and walks the whole ring).
    /// An unfilled slot reads as the zero the reference's freshly-constructed CMovement holds.
    fn record_lateness(&mut self, lateness_ms: f64) -> f64 {
        self.ring[self.ring_idx] = self.base_ms + lateness_ms;
        self.ring_idx = (self.ring_idx + 1) % LATENESS_WINDOW;
        self.ring.iter().copied().fold(f64::NEG_INFINITY, f64::max)
    }

    /// The scheduling lead this move got — `fire − arrival`, i.e. how long the dead-reckon has to
    /// cover before the pose lands. Negative means the move was already due and applies at arrival.
    /// Trace/diagnostic only ([`super::remote::trace_schedule`]).
    pub(crate) fn lead_ms(&self, now_ms: f64) -> f64 {
        self.last_fire_ms - now_ms
    }
}
