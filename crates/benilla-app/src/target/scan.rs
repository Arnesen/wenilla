//! TAB / nearest-enemy targeting + combat auto-target — the **Classic-priority selection**
//! (decision 0567) over the 1.12 wire laws.
//!
//! The 1.12 byte algorithm (±30° cone about the *character's* facing, cone-first-then-nearest
//! sort, 10-yd out-of-cone bubble, snapshot list + cursor cycling — wow-re
//! `object-layer/scratch/targeting-nearest-and-autoacquire.md`, §5-verified twice) was
//! implemented faithfully, live-reproduced, and then **replaced at the director's call**: the
//! authentic law skips a close mob that fills your screen (at 2 yd the cone is ~±1 yd wide, and
//! the camera sits ~9 yd behind — "right in front of my cam" is routinely 40°+ off the
//! character axis), and its snapshot cycle walks into behind-you mobs. Blizzard hit the same
//! wall and rewrote targeting for Legion 7.2 — the system Classic Era ships. We follow that
//! design, pinned from the Classic binary's own cvar-description strings (statically recovered;
//! the code section is packer-encrypted) and the 7.2 patch note:
//!
//! - **Screen-space tiering** — a candidate whose projected point lands inside the camera
//!   frustum with each screen edge pulled in (`TargetPriorityFrustumPullIn{Sides,Top,Bot}`:
//!   "Percentage towards center to pull in [edge] of the screen for in-view checks") is a
//!   tier-1 ("100% correct") pick; off-screen candidates are reachable **only when no
//!   on-screen candidate exists** (`TargetPriorityAllowAnyOnScreen` = 1 semantics).
//! - **A weighted score, not a lexicographic sort** (`TargetPriorityValueBank`): screen
//!   off-centerness + normalized distance − an "attacking me" bonus; lowest wins.
//! - **A fighting-me score bonus, not Classic's hard combat lock**: units attacking me outrank
//!   everything peaceful, but TAB can always walk off them onto a fresh target. Classic's
//!   `TargetPriorityCombatLock` (pool restricted to in-combat units) was implemented and then
//!   retired by the director's feel — decision 0568: with a single attacker it pinned TAB to
//!   that mob with no way off.
//! - **History cycling, no snapshot cursor** (`TargetPriorityHighlightHistoryMs` — "time target
//!   history should be maintained for repeated activations"): every press re-scores the live
//!   world and skips recently-tabbed guids; exhausting the pool clears the history (the wrap).
//!   Shift-TAB steps back through the history. A clicked target joins the history when tabbed
//!   away from (`TargetPriorityContinueFromManualTarget`'s observable).
//!
//! The **numbers** here (pull-in margins, weights, the history window) are tunable stand-ins —
//! Blizzard's defaults live in the encrypted code section; each const names its cvar so a later
//! pin (the director's in-game `GetCVarInfo` dump) slots straight in. Not modeled, disclosed:
//! `TargetPriorityUpdateDelay` (a timer-refreshed list feeding the hold-to-highlight preview we
//! don't have — per-press rebuild covers selection), `TargetPriorityAutoTargetIgnoreWindow`,
//! `AllowAnyOnScreen=2` (out-of-range picks), PvP weighting, and the 7.2 note's
//! character-OR-camera visibility union (we test the camera only).
//!
//! **What stays byte-law from 1.12** (wire/data semantics, unchanged): the validity filters
//! (dead / [`can_attack`] `0x606980` / CreatureType flag-bit-0 critters), the [`commit`]
//! stop→select→re-swing SetSelection law (`0x493540`), attack-with-no-target acquiring the best
//! candidate and swinging (`0x612df0` @ `6130b5`), and the ATTACKERSTATEUPDATE auto-acquire
//! (`6259c9`–`6259fe`). The 41-yd range keeps 1.12's `targetNearestDistance` default — Classic
//! still ships that cvar ("limited to tab targeting range").

use benilla_formats::CreatureTypeFlags;
use benilla_protocol::{guid, EntityKind};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::creature_anim::{Engaged, SwingMessage};
use crate::names::NameCache;
use crate::net::{ClientCommand, Guid, NetEntity, ObjectStore, Reputations, SelfPlayer};
use benilla_assets::{LockRecover, WorldAssets};

use super::relations::can_attack;
use super::{ring_reaction, Factions, Selection};

// == The Classic-priority dials ==
// Each names the Classic Era cvar it stands in for. The VALUES are tuned, not pinned — the
// binary's defaults are packer-encrypted; when the director's in-game `GetCVarInfo` dump lands,
// replace these with the real defaults and note it in the 0567 lineage.

/// `targetNearestDistance` — the tab-targeting range (yd). 1.12's byte-verified default 41
/// (`[0x804510]`); Classic still ships the cvar ("limited to tab targeting range").
const TAB_RANGE: f32 = 41.0;
/// `TargetPriorityFrustumPullInSides` — fraction of the viewport width pulled toward center
/// from EACH side edge for the in-view check.
const FRUSTUM_PULL_SIDES: f32 = 0.10;
/// `TargetPriorityFrustumPullInTop` — fraction of the viewport height pulled down from the top.
const FRUSTUM_PULL_TOP: f32 = 0.10;
/// `TargetPriorityFrustumPullInBot` — fraction of the viewport height pulled up from the bottom.
const FRUSTUM_PULL_BOT: f32 = 0.10;
/// `TargetPriorityHighlightHistoryMs` — how long a tabbed guid stays "recently visited" (the
/// skip set for repeated presses).
const HISTORY_SECS: f64 = 4.0;
/// The score's screen term weight: per unit of normalized off-centerness (0 = screen center,
/// 1 ≈ a viewport corner). One slot of the `TargetPriorityValueBank` stand-in.
const W_SCREEN: f32 = 1.0;
/// The score's distance term weight: per unit of `dist / TAB_RANGE`.
const W_DIST: f32 = 1.0;
/// The "attacking me" bonus (`TargetPriorityCombatLock`'s spirit as a score term — the hard
/// lock itself is retired, decision 0568): a mob whose UNIT_FIELD_TARGET is me and whose
/// in-combat flag is set outranks anything peaceful at any screen position (the bonus exceeds
/// the terms' max sum), but never pins the cycle.
const COMBAT_WITH_ME_BONUS: f32 = 3.0;

/// `WOW_TAB_TRACE=1` — the TAB-target field instrument: every press logs each known unit's
/// verdict (the reject reason, or its distance / screen position / score), the sorted pool,
/// the history, and the pick — so a "TAB won't take the mob next to me"
/// report is diagnosable from the log instead of re-guessed. One `OnceLock` read per press when
/// unset; the lines ride `info!` under the `tab-trace:` prefix.
fn tab_trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WOW_TAB_TRACE").is_some())
}

/// One scored candidate. `score`: lower is better. `on_screen`: inside the pulled-in frustum
/// (the tier-1 gate). The fighting-me bonus is already folded into `score` at build time.
#[derive(Clone, Copy, Debug)]
struct Candidate {
    entity: Entity,
    guid: u64,
    on_screen: bool,
    score: f32,
}

/// The priority value (`TargetPriorityValueBank` stand-in): screen off-centerness (absent for
/// off-screen candidates — the tier gate keeps them apart, so their mutual order is distance +
/// combat only) + normalized distance − the attacking-me bonus. Lowest wins.
fn priority_score(off_center: Option<f32>, dist: f32, combat_with_me: bool) -> f32 {
    off_center.map_or(0.0, |c| W_SCREEN * c) + W_DIST * (dist / TAB_RANGE)
        - if combat_with_me {
            COMBAT_WITH_ME_BONUS
        } else {
            0.0
        }
}

/// The sort: tier first (on-screen before off-screen — `AllowAnyOnScreen` keeps the fallback
/// tier from ever outranking a "100% correct" pick), then ascending score.
fn candidate_order(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.on_screen
        .cmp(&a.on_screen)
        .then(a.score.total_cmp(&b.score))
}

/// The press's pick pool from the sorted candidates: the on-screen tier when it exists, else
/// everything (`AllowAnyOnScreen=1` — "if no 100% correct target is available, allow selecting
/// any valid in-range target"). Classic's HARD combat lock (`TargetPriorityCombatLock` — the
/// pool restricted to in-combat units while starting from one) was implemented and then
/// **retired by the director's feel** (decision 0568): with a single attacker it pinned TAB to
/// that mob with no way off. The fighting-me *score bonus* stays — attackers come first, but
/// the history walks you off them.
fn select_pool(cands: &[Candidate]) -> Vec<Candidate> {
    if cands.iter().any(|c| c.on_screen) {
        cands.iter().filter(|c| c.on_screen).copied().collect()
    } else {
        cands.to_vec()
    }
}

/// The forward pick over a sorted pool: the best candidate that is neither the current
/// selection nor recently visited; all visited → the wrap (best non-current, history to be
/// cleared by the caller); only the current one left → itself (the commit dedups to a no-op).
/// Returns `(index, wrapped)`.
fn pick_forward(
    pool: &[Candidate],
    visited: &[u64],
    current: Option<u64>,
) -> Option<(usize, bool)> {
    if pool.is_empty() {
        return None;
    }
    if let Some(i) = pool
        .iter()
        .position(|c| Some(c.guid) != current && !visited.contains(&c.guid))
    {
        return Some((i, false));
    }
    if let Some(i) = pool.iter().position(|c| Some(c.guid) != current) {
        return Some((i, true));
    }
    Some((0, true))
}

/// "Fighting me": the unit's UNIT_FIELD_TARGET is my guid AND its in-combat flag
/// (UNIT_FLAG_IN_COMBAT, bit 19) is set — the `TargetPriorityCombatLock=2` notion ("in-combat
/// with player"), the one that matters solo.
fn combat_with_me(store: &ObjectStore, me: Option<u64>) -> bool {
    me.is_some() && store.0.unit_target() == me && store.0.unit_flags() & (1 << 19) != 0
}

/// The CreatureType.dbc flags table (see `benilla-formats`); absent (load failure) = no
/// critter/totem filtering, like the missing-catalog fallbacks elsewhere.
#[derive(Resource)]
pub(crate) struct CreatureTypes(CreatureTypeFlags);

/// Startup (after the MPQ chain opens): load CreatureType.dbc for the critter/totem TAB filter.
pub(super) fn load_creature_types(mut commands: Commands, world_assets: Option<Res<WorldAssets>>) {
    let Some(world_assets) = world_assets else {
        return;
    };
    let mut chain = world_assets.chain.lock_recover();
    match benilla_formats::load_creature_type_flags(&mut chain) {
        Ok(flags) => {
            info!("creature types: {} rows", flags.len());
            commands.insert_resource(CreatureTypes(flags));
        }
        Err(e) => warn!("CreatureType.dbc unavailable, critters stay TAB-able: {e:#}"),
    }
}

/// Everything the scan core reads, bundled (the TAB and attack-acquire systems share it).
///
/// `pub(crate)` rather than `pub(super)` because the **pet bar's** ATTACK arm runs the same
/// `TargetNearestEnemy()` the player's does ([`attack_order_target`]), and its drain lives in
/// `ui_pet`.
#[derive(SystemParam)]
#[allow(clippy::type_complexity)] // one bundled system param — the app's convention for big query sets
pub(crate) struct EnemyScan<'w, 's> {
    units: Query<
        'w,
        's,
        (
            Entity,
            &'static NetEntity,
            &'static Guid,
            &'static Transform,
            Option<&'static ObjectStore>,
            Option<&'static Visibility>,
        ),
        Without<SelfPlayer>,
    >,
    self_q: Query<
        'w,
        's,
        (
            &'static Transform,
            Option<&'static ObjectStore>,
            Option<&'static Guid>,
        ),
        With<SelfPlayer>,
    >,
    /// The live world camera — the in-view check's frustum. Absent (glue screens, headless):
    /// nothing projects, everything is fallback-tier, and the pick degrades to distance+combat.
    camera: Query<
        'w,
        's,
        (&'static Camera, &'static Transform),
        (With<benilla_world::view::WorldCamera>, Without<SelfPlayer>),
    >,
    factions: Option<Res<'w, Factions>>,
    reputations: Res<'w, Reputations>,
    names: Res<'w, NameCache>,
    creature_types: Option<Res<'w, CreatureTypes>>,
}

impl EnemyScan<'_, '_> {
    /// The `0x493e40` mode-1 validity filter — liveness + hostility (kept byte-law).
    fn is_valid(&self, store: Option<&ObjectStore>, self_store: Option<&ObjectStore>) -> bool {
        // The liveness leg is the reference's own reads-dead triple `0x605f90` — health, the
        // `UNIT_DYNFLAG_DEAD` bit (feign death) and stand state 7 — now the shared predicate
        // rather than a third transcription of it (decision 1022).
        if store.is_some_and(|s| s.0.unit_reads_dead()) {
            return false;
        }
        can_attack(
            store,
            self.factions.as_deref(),
            &self.reputations,
            self_store,
        )
    }

    /// Our own descriptor, for whichever leg needs the reaction's second party outside [`build`].
    fn self_store(&self) -> Option<&ObjectStore> {
        self.self_q.single().ok().and_then(|(_, store, _)| store)
    }

    /// `0x6130a3`'s keep-or-drop test on a guid we are already holding: is the actor hostile to it?
    ///
    /// The reference reads `0x6061e0(actor, target) >= 4` — the **reaction alone**, not the full
    /// `0x606980` CanAttack, which it saves for the final gate at `0x613167`. So this is
    /// [`ring_reaction`] and nothing else. A guid whose unit is not streamed answers `false`,
    /// which is the same exit `0x613099` takes when its object lookup misses.
    ///
    /// **The actor is the player here, and in the reference it is the caller's** — the pet on the
    /// pet arm (`0x4bd40d` passes the pet object). The two agree: vmangos gives a pet its owner's
    /// faction template outright (`Pet.cpp:248`), and the reputation leg is the owner's by
    /// definition. Reading the pet's own store instead would also fight [`build`], whose entire
    /// candidate scan is player-relative (screen, distance, "fighting me").
    fn reaction_hostile(&self, guid: u64) -> bool {
        let self_store = self.self_store();
        self.units
            .iter()
            .find(|(_, _, g, _, _, _)| g.0 == guid)
            .is_some_and(|(_, _, _, _, store, _)| {
                ring_reaction(
                    self.factions.as_deref(),
                    &self.reputations,
                    store,
                    self_store,
                ) <= 3
            })
    }

    /// The streamed unit behind a guid, as `(entity, store)` — the final gate's input.
    fn unit_by_guid(&self, guid: u64) -> Option<(Entity, Option<&ObjectStore>)> {
        self.units
            .iter()
            .find(|(_, _, g, _, _, _)| g.0 == guid)
            .map(|(e, _, _, _, store, _)| (e, store))
    }

    /// The full build: walk every known unit, filter (the kept 1.12 legality laws), project
    /// through the live camera, score, sort (tier, then score). Fresh every press — the live
    /// world is the list (no snapshot to go stale).
    fn build(&self) -> Vec<Candidate> {
        let Ok((self_tf, self_store, self_guid)) = self.self_q.single() else {
            return Vec::new();
        };
        let me = self_guid.map(|g| g.0);
        let cam = self.camera.single().ok();
        // Normalized off-centerness of a world point on the pulled-in screen: `None` when
        // off-screen / behind the camera / outside the pulled-in frustum; `Some(0)` at the
        // viewport center, ~1 at a corner. The point is the unit's root + 1 yd (chest-ish) so
        // a mob whose feet sit just below the pulled-in bottom edge still reads as on-screen.
        let project = |world: Vec3| -> Option<f32> {
            let (camera, cam_pose) = cam?;
            let cam_tf = GlobalTransform::from(*cam_pose);
            let vp = camera.logical_viewport_size()?;
            let screen = camera.world_to_viewport(&cam_tf, world).ok()?;
            let x0 = vp.x * FRUSTUM_PULL_SIDES;
            let x1 = vp.x * (1.0 - FRUSTUM_PULL_SIDES);
            let y0 = vp.y * FRUSTUM_PULL_TOP;
            let y1 = vp.y * (1.0 - FRUSTUM_PULL_BOT);
            if screen.x < x0 || screen.x > x1 || screen.y < y0 || screen.y > y1 {
                return None;
            }
            Some(screen.distance(vp * 0.5) / (vp.length() * 0.5))
        };
        let trace = tab_trace_on();
        let trace_unit = |guid: u64, tf: &Transform, verdict: &str| {
            if !trace {
                return;
            }
            info!(
                "tab-trace:   {guid:#x} \"{}\" {:.1} yd — {verdict}",
                self.names.peek(guid).unwrap_or("?"),
                (tf.translation - self_tf.translation).length()
            );
        };
        let mut out = Vec::new();
        for (entity, net, guid_c, tf, store, vis) in &self.units {
            if !matches!(net.kind, EntityKind::Unit | EntityKind::Player) {
                continue;
            }
            if !self.is_valid(store, self_store) {
                trace_unit(guid_c.0, tf, "REJECT dead-or-unattackable");
                continue;
            }
            // The creature-type gate (CreatureType.dbc flag bit 0 — critters; kept byte-law).
            // An unresolved type (name not yet queried) passes, like the client's
            // out-of-range table index.
            if guid::is_creature_or_pet(guid_c.0) {
                if let (Some(types), Some(ty)) = (
                    self.creature_types.as_deref(),
                    guid::entry(guid_c.0).and_then(|e| self.names.creature_type(e)),
                ) {
                    if types.0.no_tab_target(ty) {
                        trace_unit(guid_c.0, tf, "REJECT critter-type");
                        continue;
                    }
                }
            }
            // The scene-attach/hide notion (kept): an explicitly hidden root is out.
            if vis == Some(&Visibility::Hidden) {
                trace_unit(guid_c.0, tf, "REJECT hidden");
                continue;
            }
            let dist = (tf.translation - self_tf.translation).length();
            if dist > TAB_RANGE {
                trace_unit(guid_c.0, tf, "REJECT range (41 yd)");
                continue;
            }
            let off_center = project(tf.translation + Vec3::Y);
            let cwm = store.is_some_and(|s| combat_with_me(s, me));
            let score = priority_score(off_center, dist, cwm);
            if trace {
                let screen = off_center
                    .map(|c| format!("center+{c:.2}"))
                    .unwrap_or_else(|| "OFF-SCREEN".into());
                trace_unit(
                    guid_c.0,
                    tf,
                    &format!(
                        "{screen}{} score {score:+.2}",
                        if cwm { " FIGHTING-ME" } else { "" }
                    ),
                );
            }
            out.push(Candidate {
                entity,
                guid: guid_c.0,
                on_screen: off_center.is_some(),
                score,
            });
        }
        out.sort_by(candidate_order);
        if trace {
            for (i, c) in out.iter().enumerate() {
                info!(
                    "tab-trace: list[{i}] {:#x} \"{}\" {} score {:+.2}",
                    c.guid,
                    self.names.peek(c.guid).unwrap_or("?"),
                    if c.on_screen { "on-screen" } else { "fallback" },
                    c.score
                );
            }
        }
        out
    }
}

/// The recent-TAB history (`TargetPriorityHighlightHistoryMs`): guids picked within
/// [`HISTORY_SECS`], skipped by the forward pick so repeated presses walk fresh targets; a wrap
/// clears it. Replaces the 1.12 snapshot list + cursor (decision 0567).
#[derive(Resource, Default)]
pub(super) struct TabHistory {
    /// `(guid, when)` — insertion-ordered; the back is the most recent (Shift-TAB's walk).
    visited: Vec<(u64, f64)>,
}

impl TabHistory {
    fn prune(&mut self, now: f64) {
        self.visited.retain(|&(_, t)| now - t < HISTORY_SECS);
    }
    fn guids(&self) -> Vec<u64> {
        self.visited.iter().map(|&(g, _)| g).collect()
    }
    /// Record a visit (re-visiting moves the guid to most-recent).
    fn push(&mut self, guid: u64, now: f64) {
        self.visited.retain(|&(g, _)| g != guid);
        self.visited.push((guid, now));
    }
}

/// What one [`commit`] did: whether the selection changed, and whether the engaged-switch law
/// already re-pointed the swing (`CMSG_ATTACKSWING` at the new target) — the attack branches
/// read `swung` to avoid double-sending their own swing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CommitOutcome {
    pub(super) changed: bool,
    pub(super) swung: bool,
}

/// Commit a target through the SetSelection path (`0x493540`), byte-complete: dedup, then — on a
/// switch while auto-attacking — the **stop → select → re-swing** law read directly from the
/// binary (wow-re disasm, 2026-07-14):
///
/// 1. `0x493637–0x4936ac` latches "attacking AND the old target is still a live attackable unit"
///    into `[ebp-1]` before anything changes;
/// 2. the silent old-target clear (`0x493910(current, ecx=0)`) calls **StopAttack `0x5ecac0`
///    unconditionally at `0x493a08`** (engaged-gated internally) → `CMSG_ATTACKSTOP`, and its
///    builder `0x624370` sets `[player+0xc54] = 1` ("stop in flight");
/// 3. the new guid lands in the globals and `CMSG_SET_SELECTION` goes out (`0x493857`);
/// 4. the tail `0x4938a1–0x4938c8`: iff `[ebp-1]` and the new selection is not MYSELF →
///    `Attack 0x5ecb70`, which validates the NEW target (alive-or-feign + `CanAttack 0x606980`,
///    `5ecc10–5ecc35` — an invalid one STOPS instead, `5ecc37`) and then **sends
///    `CMSG_ATTACKSWING` at it** — `[+0xc54]` bypasses the already-attacking gate (`5eccda`) so
///    the swing goes out despite the still-set old lock, and the lock re-stores to the new guid
///    (`5ecd4b`). The swing FOLLOWS the switch (no sheath ceremony: engaged means the weapon is
///    already out). The `new_attackable` param carries Attack's new-target validation — callers
///    pass their already-computed classification (the cursor's Attack kind / the scan's
///    alive+hostile) rather than re-deriving it here.
///
/// Approximations, disclosed: `[ebp-1]`'s old-target legs (alive-or-feign + `CanAttack` + the
/// `0x605f30`/`+0x1fc` player-state gates) collapse to "an old selection existed" — on vmangos
/// the victim's death tears `Engaged` down via the echoed `SMSG_ATTACKSTOP` before a human can
/// TAB, so `engaged && had_old` covers the same observable; our `engaged` is the server-echoed
/// [`Engaged`], not the ref's instant local lock (one-RTT lag on a switch fired mid-attack-
/// start); and the ref's invalid-new-target Attack sends a second (duplicate) ATTACKSTOP we
/// don't. Shared with the UI's `TargetUnit` drain ([`super::target_unit_requests`]) so every
/// non-mouse selection writer commits identically — a `TargetUnit("player")` mid-combat stops
/// the swing and does NOT re-point (the tail's self exception).
pub(super) fn commit(
    selection: &mut Selection,
    seam: &mut crate::creature_anim::AttackSeam,
    entity: Entity,
    guid: u64,
    engaged: bool,
    self_guid: Option<u64>,
    new_attackable: bool,
) -> CommitOutcome {
    if selection.guid == Some(guid) {
        return CommitOutcome::default(); // the setter's dedup: bail if already current
    }
    let had_old = selection.guid.is_some();
    selection.target = Some(entity);
    selection.guid = Some(guid);
    let stop_and_repoint = engaged && had_old;
    if stop_and_repoint {
        // `0x493a08 call 0x5ecac0` — the real StopAttack, so the switch also cancels a queued
        // on-next-swing strike (the seam's `0x6e6f30` tail). Before this routed through the seam
        // it was a bare `CMSG_ATTACKSTOP` and a queued Heroic Strike survived the switch.
        seam.stop(engaged);
    }
    let _ = seam.net.0.send(ClientCommand::SetSelection { guid });
    let swung = stop_and_repoint && new_attackable && self_guid != Some(guid);
    if swung {
        // `0x4938c8 call 0x5ecb70` with a stop in flight (`[+0xc54]`, set by the call above), so
        // the swing goes out despite the still-set lock. Its tail cancels a running auto-repeat —
        // wow-re `melee-autorepeat-exclusion.md` §6 REFUTES `nocked-ammo-cancel.md`'s
        // direct-callers-only census, which had this chain never reaching `0x6ea080`.
        seam.start(guid, engaged, true);
    }
    CommitOutcome {
        changed: true,
        swung,
    }
}

/// TARGETNEARESTENEMY / TARGETPREVIOUSENEMY (0997: two commands through the binding table now,
/// defaults TAB / SHIFT-TAB — no shift fork here anymore), Classic-priority style: re-score the
/// live world, pool by tier, skip the recent history forward (or walk it backward), commit
/// through the byte-law [`commit`]. The dispatch already applied the typing gate (a focused
/// EditBox owns TAB) and the exact-modifier law.
pub(super) fn tab_target(
    binds: Res<crate::bindings::BindingsState>,
    time: Res<Time>,
    scan: EnemyScan,
    mut history: ResMut<TabHistory>,
    mut selection: ResMut<Selection>,
    mut seam: crate::creature_anim::AttackSeam,
    engaged: Query<(), (With<Engaged>, With<SelfPlayer>)>,
) {
    let reverse = binds.fired(crate::bindings::cmd::TARGET_PREVIOUS_ENEMY);
    if !reverse && !binds.fired(crate::bindings::cmd::TARGET_NEAREST_ENEMY) {
        return;
    }
    let now = time.elapsed_secs_f64();
    history.prune(now);
    let trace = tab_trace_on();
    if trace {
        info!(
            "tab-trace: TAB press (reverse={reverse}), selection {:?}, history {}",
            selection.guid.map(|g| format!("{g:#x}")),
            history.visited.len()
        );
    }
    let cands = scan.build();
    if cands.is_empty() {
        if trace {
            info!("tab-trace: no candidates — target unchanged");
        }
        return;
    }
    let pool = select_pool(&cands);
    if trace {
        info!(
            "tab-trace: pool {}/{} ({} tier)",
            pool.len(),
            cands.len(),
            if cands.iter().any(|c| c.on_screen) {
                "on-screen"
            } else {
                "fallback"
            }
        );
    }
    // The pick. Reverse walks the history back (most recent visited that is not current and
    // still in the pool); an empty walk falls through to the forward rule.
    let visited = history.guids();
    let back = reverse
        .then(|| {
            history.visited.iter().rev().find_map(|&(g, _)| {
                (Some(g) != selection.guid)
                    .then(|| pool.iter().find(|c| c.guid == g).copied())
                    .flatten()
            })
        })
        .flatten();
    let (entity, guid, wrapped) = match back {
        Some(c) => (c.entity, c.guid, false),
        None => {
            let Some((i, wrapped)) = pick_forward(&pool, &visited, selection.guid) else {
                return;
            };
            (pool[i].entity, pool[i].guid, wrapped)
        }
    };
    if wrapped {
        history.visited.clear();
    }
    // The outgoing selection joins the history — a clicked target counts as visited once
    // tabbed away from (`TargetPriorityContinueFromManualTarget`'s observable).
    if let Some(old) = selection.guid {
        history.push(old, now);
    }
    let out = commit(
        &mut selection,
        &mut seam,
        entity,
        guid,
        !engaged.is_empty(),
        None,
        true,
    );
    if out.changed {
        history.push(guid, now);
    }
    if trace {
        info!(
            "tab-trace: pick {guid:#x} \"{}\"{} — {}",
            scan.names.peek(guid).unwrap_or("?"),
            if wrapped { " (wrapped)" } else { "" },
            if out.changed {
                "selection changed"
            } else {
                "NO-OP (already the selection)"
            }
        );
    }
}

/// `0x6130b5` — **`TargetNearestEnemy()` itself**, and the whole of "pressing Attack at nothing
/// finds something in front of you".
///
/// The acquire the validator runs is not a private helper: it is `0x493f60(ecx = 0, edx = 1)`, the
/// same cycler the four Lua `TargetNearest*` bindings drive (`0x489a80`–`0x489af3` are the same
/// call with `edx = 1..4` for enemy / friend / party / raid, and `ecx` the *reverse* flag). So
/// "in front of me" is not a separate rule with its own cone — it is this module's priority scan,
/// which is why the acquire commits through [`commit`] like any press: `CMSG_SET_SELECTION` goes
/// out and **your** target really moves, which is what lets the pet's order and your own swing
/// aim at the same thing.
///
/// Returns the committed `(entity, guid)`, or `None` after raising `0x6130d9`'s
/// `ERR_NO_ATTACK_TARGET`.
///
/// Divergence, deliberate and inherited: the reference's cycler keeps a cached, time-expiring
/// candidate list and a cursor into it, so a *repeated* acquire walks; ours re-scores the live
/// world every call and always hands back the head. That is decision 0567's trade for the whole
/// module (history cycling replaces the snapshot cursor), and an acquire is a fresh pick in both.
fn acquire_nearest_enemy(
    scan: &EnemyScan,
    selection: &mut Selection,
    seam: &mut crate::creature_anim::AttackSeam,
    errors: &mut crate::ui_action::UiErrorKeys,
) -> Option<(Entity, u64)> {
    let cands = scan.build();
    let Some(c) = cands.first() else {
        // `0x6130d9` — the acquire ran and came back empty, so the selection re-read at `0x6130c1`
        // still finds nothing: errorId `0xa0` `ERR_NO_ATTACK_TARGET`.
        debug!("attack acquire: nothing to attack (ERR_NO_ATTACK_TARGET)");
        errors
            .0
            .push(crate::ui_action::UiError::key("ERR_NO_ATTACK_TARGET"));
        return None;
    };
    // `engaged = false`: the switch law only fires when we were already swinging at the OLD
    // selection, and every path that reaches here had no selection or a non-hostile one — neither
    // is a thing you can be engaged with.
    commit(selection, seam, c.entity, c.guid, false, None, false);
    Some((c.entity, c.guid))
}

/// `0x612df0`'s **Phase B** — the attack order's *target* arm, and the reason pressing the pet's
/// Attack button at nothing still sends the pet at something.
///
/// Phase A (the actor's own eligibility — [`crate::ui_action::attack_actor_refusal`]) has already
/// run by the time this is reached; what is left is choosing **what** to attack. The candidate the
/// caller passes is `{0,0}` for a bare press, and it gets replaced by the current selection twice
/// over — once by `CastPetAction` itself for a nil Lua argument (`0x4bd212`), once by the
/// validator for a `{0,0}` candidate (`0x61306b`) — so the candidate *is* the selection:
///
/// ```text
/// 0x61306b  candidate == 0             -> candidate = the current selection
/// 0x61309b  0x6061e0(actor, candidate) -> the actor's reaction toward it
/// 0x6130a3  reaction >= 4              -> candidate = 0   (friendly or neutral is not a target)
/// 0x613100  else 0x493540(candidate)   ; promote it into the selection (a dedup when it came from there)
/// 0x6130b5  candidate == 0             -> 0x493f60(0, 1) == TargetNearestEnemy()
/// 0x6130c1  re-read the selection      ; whatever the acquire committed
/// 0x6130d9  still nothing              -> ERR_NO_ATTACK_TARGET, EAX = 0, no packet
/// 0x613167  final gate                 -> ERR_INVALID_ATTACK_TARGET, EAX = 0, no packet
/// ```
///
/// Two things fall out that are worth knowing before looking at it in game. **Pressing Attack
/// while you hold a friendly target retargets you** — the reaction test drops it and the acquire
/// runs, so your selection moves to the nearest enemy. And the guid this returns is what goes in
/// the packet: the validator writes its answer back through the caller's out-param (`0x6130c6`),
/// and `0x4bd491` reads that slot when it builds `CMSG_PET_ACTION`.
///
/// The final gate is transcribed rather than folded into the reaction test above it, because the
/// reference deliberately keeps them apart: a *dead* selection is `ERR_INVALID_ATTACK_TARGET`, not
/// a reason to go find something else. `0x613159`'s odd second leg is verbatim — a zero-health
/// target passes iff `UNIT_DYNAMIC_FLAGS` bit 5 is set.
pub(crate) fn attack_order_target(
    scan: &EnemyScan,
    selection: &mut Selection,
    seam: &mut crate::creature_anim::AttackSeam,
    errors: &mut crate::ui_action::UiErrorKeys,
) -> Option<u64> {
    // `0x61306b` + `0x6130a3`: the selection is the candidate, and it survives only if the actor is
    // hostile to it. Otherwise `0x6130b5` acquires.
    let guid = match keeps_held_target(selection.guid, |g| scan.reaction_hostile(g)) {
        Some(kept) => kept,
        None => acquire_nearest_enemy(scan, selection, seam, errors)?.1,
    };
    // `0x613167`'s final gate on whatever we ended up with. `0x61312e`'s legs are the ACTOR's and
    // are Phase A's, already run by the caller; these two are the target's.
    let store = scan.unit_by_guid(guid).and_then(|(_, s)| s);
    if !attack_target_valid(
        store,
        scan.factions.as_deref(),
        &scan.reputations,
        scan.self_store(),
    ) {
        debug!("attack order: {guid:#x} fails the final gate (ERR_INVALID_ATTACK_TARGET)");
        errors
            .0
            .push(crate::ui_action::UiError::key("ERR_INVALID_ATTACK_TARGET"));
        return None;
    }
    Some(guid)
}

/// `0x6130a3`'s fork on its own: keep what you are holding, or fall through to the acquire.
///
/// The whole content is that **the reaction alone decides**, and it decides against the *actor*.
/// A friendly or neutral selection is not "an invalid target" — the reference zeroes the candidate
/// (`0x6130a8`) and goes looking, which is why pressing pet-Attack while you have a quest giver
/// selected moves your target to the nearest mob instead of raising an error.
fn keeps_held_target(selection: Option<u64>, hostile: impl Fn(u64) -> bool) -> Option<u64> {
    selection.filter(|&g| hostile(g))
}

/// `0x613152`–`0x613169` — the final gate's two **target** legs, whatever route the guid arrived
/// by.
///
/// - alive, transcribed verbatim including its odd second half: a zero-health target passes iff
///   `UNIT_DYNAMIC_FLAGS` bit 5 is set (`0x613159 shr edx,5; test dl,1`);
/// - `CanAttack 0x606980` — the full predicate this time, not the bare reaction the fork above uses.
///
/// A target with no streamed descriptor passes both, matching [`can_attack`]'s own missing-data
/// posture: nothing known to disqualify is not a disqualification.
fn attack_target_valid(
    store: Option<&ObjectStore>,
    factions: Option<&Factions>,
    reputations: &Reputations,
    self_store: Option<&ObjectStore>,
) -> bool {
    let alive = store.is_none_or(|s| {
        s.0.unit_health().is_none_or(|h| h > 0) || s.0.unit_dynamic_flags() & (1 << 5) != 0
    });
    alive && can_attack(store, factions, reputations, self_store)
}

/// Request from the action layer: the attack action fired with NO selection — pick the best
/// candidate and swing at it (`0x612df0` @ `6130b5`).
#[derive(Message)]
pub(crate) struct AttackNearestRequest;

/// Auto-acquire on attack (behavior 1): a fresh scan, commit the best candidate (the sort's
/// head — best on-screen, else best overall), then the attack-start pair (auto-draw +
/// `CMSG_ATTACKSWING`, exactly the action-bar attack's path). None found ⇒ the reference shows
/// error `0xa0` "There is nothing to attack" (we log; the red error banner is its own arc). The
/// TAB history is not touched — an auto-pick is not a press (Classic's
/// `TargetPriorityAutoTargetIgnoreWindow` nuance, disclosed unmodeled).
#[allow(clippy::too_many_arguments)] // one Bevy system's full input set
pub(super) fn acquire_and_attack(
    mut requests: MessageReader<AttackNearestRequest>,
    scan: EnemyScan,
    mut selection: ResMut<Selection>,
    mut seam: crate::creature_anim::AttackSeam,
    self_store: Query<&crate::net::ObjectStore, With<SelfPlayer>>,
    mut ui_error_keys: ResMut<crate::ui_action::UiErrorKeys>,
) {
    if requests.read().last().is_none() {
        return;
    }
    if selection.guid.is_some() {
        return; // something got selected between the action and this frame — the normal path owns it
    }
    // The actor-eligibility block (decision 0481, widened to `0x612df0`'s full Phase A): every one
    // of its refusals sits BEFORE the nearest-core `0x6130b5` — a mounted, stunned or dead press
    // never even scans. Gated at the responder so every requester (the Attack button's no-target
    // arm, the melee probe) shares it. The actor here is us.
    let self_guid = scan
        .self_q
        .iter()
        .next()
        .and_then(|(_, _, g)| g)
        .map(|g| g.0);
    if crate::ui_action::attack_actor_refusal(
        self_store.iter().next(),
        self_guid,
        &mut ui_error_keys,
    ) {
        return;
    }
    // `0x6130b5` and `0x6130d9`, shared with the pet bar's ATTACK arm — see
    // [`acquire_nearest_enemy`]. The selection is empty (checked above), which is exactly the
    // `candidate == 0` leg of [`attack_order_target`]; the follow-through below is what differs.
    let Some((_, guid)) =
        acquire_nearest_enemy(&scan, &mut selection, &mut seam, &mut ui_error_keys)
    else {
        return;
    };
    debug!("attack acquire: best candidate {guid:#x} → select + swing");
    // `0x6131a0`'s continuation — StartAttack through the one seam, so the acquire path gets the
    // auto-draw AND the auto-repeat cancel its hand-rolled copy used to miss. The selection was
    // empty on entry, so we cannot have been engaged: no stop is in flight.
    seam.start(guid, false, false);
}

/// Auto-acquire the attacker (behavior 2): the ATTACKERSTATEUPDATE victim handler's
/// self-defense block (`6259c9`–`6259fe`) — packet victim == me AND my selection strictly
/// empty → `SetSelection(attacker)`. Selection only: no counter-attack, never overwrites.
pub(super) fn auto_acquire_attacker(
    mut swings: MessageReader<SwingMessage>,
    self_player: Query<Entity, With<SelfPlayer>>,
    guids: Query<&Guid>,
    mut selection: ResMut<Selection>,
    mut seam: crate::creature_anim::AttackSeam,
) {
    for s in swings.read() {
        if selection.target.is_some() {
            continue;
        }
        let Ok(me) = self_player.single() else {
            continue;
        };
        if s.victim != Some(me) {
            continue;
        }
        let Ok(guid) = guids.get(s.attacker) else {
            continue;
        };
        debug!(
            "auto-target: attacker {:#x} (victim = me, no target)",
            guid.0
        );
        // Selection is empty here (checked above): no engaged-switch law, selection only.
        commit(
            &mut selection,
            &mut seam,
            s.attacker,
            guid.0,
            false,
            None,
            false,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::NetCommands;

    /// The `SetSelection` wire law, byte-read from `0x493540` (the director's "TAB while
    /// auto-attacking kills the attack" bug): an engaged SWITCH is **stop → select → re-swing**
    /// — the attack follows the new target — while a plain select sends only the selection, a
    /// self-target stops without re-pointing, and an unattackable new target (Attack `0x5ecb70`'s
    /// validation) switches and stops but never swings.
    ///
    /// Driven through a World because `commit` now runs the two real seams
    /// ([`crate::creature_anim::AttackSeam`]) rather than emitting bare packets — which is what
    /// the second half of this test pins: the switch's stop un-queues an on-next-swing strike
    /// (`0x493a08 call 0x5ecac0` → `0x6e6f30`) and its re-swing cancels a running auto-repeat
    /// (`0x4938c8 call 0x5ecb70` → `0x5ecd8c`). Both were missing while this path hand-rolled
    /// its own packets.
    #[test]
    fn commit_follows_the_stop_select_reswing_law() {
        use bevy::ecs::system::RunSystemOnce;

        let (tx, rx) = crossbeam_channel::unbounded();
        let mut world = World::new();
        world.insert_resource(NetCommands(tx));
        world.init_resource::<crate::ui_cast::QueuedMeleeSpell>();
        world.init_resource::<crate::ui_action::AutoRepeatActive>();
        world.init_resource::<Messages<crate::creature_anim::SheathRequest>>();
        world.init_resource::<Selection>();
        world.spawn(SelfPlayer);

        let drain = |rx: &crossbeam_channel::Receiver<ClientCommand>| {
            rx.try_iter()
                .map(|c| match c {
                    ClientCommand::SetSelection { .. } => "select",
                    ClientCommand::AttackStop => "stop",
                    ClientCommand::AttackSwing { .. } => "swing",
                    ClientCommand::CancelCast { .. } => "cancel-cast",
                    ClientCommand::CancelAutoRepeat => "cancel-repeat",
                    _ => "other",
                })
                .collect::<Vec<_>>()
        };
        // One `commit` through a one-shot system, returning its outcome.
        fn go(
            world: &mut World,
            guid: u64,
            engaged: bool,
            self_guid: Option<u64>,
            attackable: bool,
        ) -> CommitOutcome {
            world
                .run_system_once(
                    move |mut selection: ResMut<Selection>,
                          mut seam: crate::creature_anim::AttackSeam| {
                        commit(
                            &mut selection,
                            &mut seam,
                            Entity::PLACEHOLDER,
                            guid,
                            engaged,
                            self_guid,
                            attackable,
                        )
                    },
                )
                .expect("commit runs as a one-shot system")
        }

        // Not engaged: first select and a switch are selection-only; a same-guid re-commit dedups.
        assert!(go(&mut world, 0xA, false, Some(1), true).changed);
        assert!(go(&mut world, 0xB, false, Some(1), true).changed);
        assert!(!go(&mut world, 0xB, false, Some(1), true).changed);
        assert_eq!(drain(&rx), ["select", "select"]);

        // Engaged switch onto an attackable unit: the byte order stop → select → swing.
        let out = go(&mut world, 0xC, true, Some(1), true);
        assert!(out.changed && out.swung);
        assert_eq!(drain(&rx), ["stop", "select", "swing"]);

        // Engaged switch onto MYSELF (TargetUnit("player") mid-combat): stop, no re-point.
        let out = go(&mut world, 0x1, true, Some(1), true);
        assert!(out.changed && !out.swung);
        assert_eq!(drain(&rx), ["stop", "select"]);

        // Engaged switch onto an unattackable unit (vendor/corpse): stop, no swing at it.
        let out = go(&mut world, 0xD, true, Some(1), false);
        assert!(out.changed && !out.swung);
        assert_eq!(drain(&rx), ["stop", "select"]);

        // Engaged FIRST select (no old target): the [ebp-1] latch is off — selection only.
        *world.resource_mut::<Selection>() = Selection::default();
        let out = go(&mut world, 0xE, true, Some(1), true);
        assert!(out.changed && !out.swung);
        assert_eq!(drain(&rx), ["select"]);

        // **The two edges the seam brought.** A queued strike and a running auto-repeat, then an
        // engaged switch onto an attackable unit: the stop un-queues the strike, the re-swing
        // kills the repeat.
        world
            .resource_mut::<crate::ui_cast::QueuedMeleeSpell>()
            .arm(78);
        world.resource_mut::<crate::ui_action::AutoRepeatActive>().0 = Some(75);
        let out = go(&mut world, 0xF, true, Some(1), true);
        assert!(out.changed && out.swung);
        assert_eq!(
            drain(&rx),
            ["stop", "cancel-cast", "select", "swing", "cancel-repeat"]
        );
        assert_eq!(
            world
                .resource::<crate::ui_cast::QueuedMeleeSpell>()
                .current(),
            None,
            "the switch un-queued Heroic Strike"
        );
        assert_eq!(
            world.resource::<crate::ui_action::AutoRepeatActive>().0,
            None,
            "and the re-swing killed Auto Shot"
        );
    }

    fn cand(guid: u64, score: f32, on_screen: bool) -> Candidate {
        Candidate {
            entity: Entity::PLACEHOLDER,
            guid,
            on_screen,
            score,
        }
    }

    /// The priority value: a centered far mob beats a screen-edge close one; a close central
    /// mob beats a far central one; the fighting-me bonus dominates both terms.
    #[test]
    fn priority_score_weighs_center_distance_and_combat() {
        // Dead-center at 30 yd vs near-the-edge at 3 yd: center wins.
        assert!(
            priority_score(Some(0.05), 30.0, false) < priority_score(Some(0.9), 3.0, false),
            "a centered far mob outranks a screen-edge close one"
        );
        // Same centrality: nearest wins.
        assert!(priority_score(Some(0.2), 5.0, false) < priority_score(Some(0.2), 35.0, false));
        // Fighting-me beats a perfectly centered peaceful mob at any position in range.
        assert!(
            priority_score(Some(0.9), 40.0, true) < priority_score(Some(0.0), 1.0, false),
            "the combat bonus dominates the geometric terms"
        );
    }

    /// The sort and the pool: on-screen candidates always precede off-screen ones; the pool is
    /// the on-screen tier when it exists (AllowAnyOnScreen=1), everything otherwise. NO combat
    /// lock (decision 0568): a fighting-me attacker in the pool never restricts it — TAB can
    /// always walk off an attacker onto a fresh target (the bonus orders, the history moves).
    #[test]
    fn tier_pool_never_locks() {
        let mut v = [cand(1, 0.1, false), cand(2, 5.0, true), cand(3, 0.5, true)];
        v.sort_by(candidate_order);
        assert_eq!(
            v.map(|c| c.guid),
            [3, 2, 1],
            "on-screen first (even at a worse score), then ascending score"
        );

        // Tier: with any on-screen candidate, off-screen ones drop out of the pool.
        let pool = select_pool(&v);
        assert_eq!(pool.iter().map(|c| c.guid).collect::<Vec<_>>(), [3, 2]);
        // No on-screen candidates at all → the fallback tier is everything.
        let off = [cand(1, 0.3, false), cand(2, 0.1, false)];
        assert_eq!(select_pool(&off).len(), 2);

        // An attacker (fighting-me) in the pool does NOT shrink it — the 0568 unlock: with the
        // attacker current and visited, the pick walks onto the peaceful mob.
        let mixed = [cand(1, -2.0, true), cand(2, 0.2, true)];
        let pool = select_pool(&mixed);
        assert_eq!(pool.len(), 2, "no combat lock — the full tier stays");
        assert_eq!(
            pick_forward(&pool, &[1], Some(1)),
            Some((1, false)),
            "TAB moves off the attacker onto the fresh target"
        );
    }

    /// The forward pick: best unvisited non-current first; exhausted history wraps (caller
    /// clears it) to the best non-current; a pool of only the current selection returns it
    /// (the commit dedups to a no-op).
    #[test]
    fn forward_pick_skips_history_then_wraps() {
        let pool = [
            cand(0xA, 0.1, true),
            cand(0xB, 0.2, true),
            cand(0xC, 0.3, true),
        ];
        // Fresh: best.
        assert_eq!(pick_forward(&pool, &[], None), Some((0, false)));
        // Current is best, nothing visited: next-best.
        assert_eq!(pick_forward(&pool, &[0xA], Some(0xA)), Some((1, false)));
        // Two visited: the third.
        assert_eq!(
            pick_forward(&pool, &[0xA, 0xB], Some(0xB)),
            Some((2, false))
        );
        // All visited: wrap to the best non-current.
        assert_eq!(
            pick_forward(&pool, &[0xA, 0xB, 0xC], Some(0xC)),
            Some((0, true))
        );
        // Only the current selection in the pool: itself, wrapped (dedup no-op downstream).
        let solo = [cand(0xA, 0.1, true)];
        assert_eq!(pick_forward(&solo, &[0xA], Some(0xA)), Some((0, true)));
        assert_eq!(pick_forward(&[], &[], None), None);
    }

    /// **Phase B's fork** (`0x6130a3`): a hostile selection is kept, everything else acquires.
    ///
    /// The case that reads wrong until you have read the binary is the *friendly* one — it is not
    /// an error, it is a reason to go find a real target. That is what makes "press Attack with no
    /// target and the pet goes for what is in front of you" the same code path as "press Attack
    /// while talking to a vendor and your target jumps to the wolf behind him".
    #[test]
    fn phase_b_keeps_only_a_hostile_selection() {
        let hostile = |g: u64| g == 0xBAD;
        assert_eq!(keeps_held_target(Some(0xBAD), hostile), Some(0xBAD));
        assert_eq!(keeps_held_target(Some(0x600D), hostile), None, "friendly");
        assert_eq!(keeps_held_target(None, hostile), None, "no selection");
    }

    /// The final gate's target legs (`0x613152`–`0x613169`), including the transcribed oddity: a
    /// zero-health target is invalid **unless** dynamic-flag bit 5 is set.
    #[test]
    fn the_attack_orders_final_gate_reads_health_then_can_attack() {
        use benilla_protocol::ObjectFields;
        const HEALTH: u16 = 22;
        const DYNFLAGS: u16 = 143;
        const FLAGS: u16 = 46;
        let reps = Reputations::default();
        let unit = |pairs: &[(u16, u32)]| ObjectStore(ObjectFields::from_pairs(pairs));
        let valid = |s: &ObjectStore| attack_target_valid(Some(s), None, &reps, None);

        assert!(valid(&unit(&[(HEALTH, 120)])), "a live mob");
        assert!(!valid(&unit(&[(HEALTH, 0)])), "a corpse");
        assert!(
            valid(&unit(&[(HEALTH, 0), (DYNFLAGS, 1 << 5)])),
            "0x613159's second leg, verbatim"
        );
        assert!(
            !valid(&unit(&[(HEALTH, 120), (FLAGS, 1 << 25)])),
            "NOT_SELECTABLE is one of CanAttack's disqualifiers"
        );
        // No descriptor is no disqualification — `can_attack`'s own posture, unchanged.
        assert!(attack_target_valid(None, None, &reps, None));
    }

    /// The history: pruning honors the window, a re-visit moves to most-recent.
    #[test]
    fn history_prunes_and_reorders() {
        let mut h = TabHistory::default();
        h.push(0xA, 0.0);
        h.push(0xB, 1.0);
        h.push(0xA, 2.0); // re-visit: A moves to the back
        assert_eq!(h.guids(), [0xB, 0xA]);
        h.prune(1.0 + HISTORY_SECS); // B (t=1.0) ages out at exactly the window
        assert_eq!(h.guids(), [0xA]);
    }
}
