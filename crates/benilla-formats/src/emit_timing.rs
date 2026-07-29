//! Per-sequence **particle emission timing** — the FN1 bake of the emitter's two
//! per-frame-sampled M2Tracks (spawn rate `+0xdc`, enabled gate `+0x1dc`), one loop per FILE
//! sequence slot. Split from [`crate::particles`] (the raw record parse) because it is the
//! *runtime sampling* face: decision 0641's material-alpha structure, one channel over.

use benilla_m2::M2ScalarTrack;

use crate::models::{bake_track, ScalarAnim, SeqSlot};

/// Per-sequence **emission timing**: the emitter's two per-frame-sampled M2Tracks — spawn rate
/// (`+0xdc`) and the enabled gate (`+0x1dc`) — baked one loop per FILE sequence slot through the
/// FN1 kernel ([`crate::models::bake_track`]), exactly the material-alpha structure of decision
/// 0641 one channel over. The reference's emitter phase of `m2_animate` samples both through the
/// **playing** sequence's key window every frame and forces the spawn rate to 0 while the gate is
/// off (wow-re `part-emission-rate-animated.md` §2/§3, byte-verified `0x717d90`/`0x718f32`).
///
/// The clock law rides the slot: a **looping** sequence wraps its band (`t mod period` — a
/// windowed gate re-fires every pass), a **clamped** one parks at the band end and holds the
/// tail value (never aliasing back to the band start). Consumers resolve `seq` from whatever
/// clock they run: a placed doodad passes `None` (slot 0 — its one-time arm), an effect its
/// armed slot, a unit/GameObject its live playing sequence.
#[derive(Debug, Clone, Default)]
pub struct EmitTiming {
    /// Baked rate loop per file slot; `None` = the track keys nothing there (spawn rate 0).
    rate: Vec<Option<ScalarAnim>>,
    /// Baked gate per file slot (step, 0/1); `None` = no gate authored — the loader default is
    /// ON (`0x710092`: `block+0x14c = 1` for every emitter).
    enabled: Vec<Option<ScalarAnim>>,
    /// Per file slot: sequence flags bit 0 CLEAR = the band loops (the kernel's modulo wrap).
    looping: Vec<bool>,
}

impl EmitTiming {
    /// Bake both tracks against every file sequence slot. `slots`/`looping` are parallel
    /// (one per file sequence); `gseq` is the global-sequence duration table.
    pub(crate) fn bake(
        rate: &M2ScalarTrack,
        enabled: &M2ScalarTrack,
        slots: &[SeqSlot],
        looping: Vec<bool>,
        gseq: &[u32],
    ) -> Self {
        // Keep every baked shape, constants included — this channel has no static fallback path,
        // so a held value must survive the bake (`|_| false` on both predicates).
        let per_slot = |t: &M2ScalarTrack| -> Vec<Option<ScalarAnim>> {
            slots
                .iter()
                .map(|&s| bake_track(t, gseq, Some(s), |v| v, |_| false, |_| false))
                .collect()
        };
        Self {
            rate: per_slot(rate),
            enabled: per_slot(enabled),
            looping,
        }
    }

    /// Resolve a consumer's sequence to a file slot: out-of-range / unknown degrades to slot 0
    /// (the doodad lane's one-time arm, and the old single-band behaviour).
    fn idx(&self, seq: Option<usize>) -> usize {
        match seq {
            Some(i) if i < self.looping.len() => i,
            _ => 0,
        }
    }

    /// Is the gate ON, `elapsed` seconds into sequence slot `seq`? A slot with no baked gate is
    /// ON (the loader default).
    pub fn emitting(&self, seq: Option<usize>, elapsed: f32) -> bool {
        let i = self.idx(seq);
        let wrap = self.looping.get(i).copied().unwrap_or(true);
        self.enabled
            .get(i)
            .and_then(|o| o.as_ref())
            .is_none_or(|a| a.sample_clocked(elapsed, wrap, 1.0) > 0.5)
    }

    /// The spawn rate (particles/sec), `elapsed` seconds into sequence slot `seq`. A slot with no
    /// baked rate spawns nothing. Floored at 0 (a track tail may legitimately go negative).
    pub fn rate(&self, seq: Option<usize>, elapsed: f32) -> f32 {
        let i = self.idx(seq);
        let wrap = self.looping.get(i).copied().unwrap_or(true);
        self.rate
            .get(i)
            .and_then(|o| o.as_ref())
            .map_or(0.0, |a| a.sample_clocked(elapsed, wrap, 0.0))
            .max(0.0)
    }

    /// The rate track's peak over every slot — the "can this emitter ever contribute" spawn gate
    /// (a burst emitter keys `0 → peak → 0`, so its first key is 0 but it absolutely emits).
    pub fn peak_rate(&self) -> f32 {
        self.rate
            .iter()
            .flatten()
            .flat_map(|a| a.keys.iter().map(|&(_, v)| v))
            .fold(0.0, f32::max)
    }

    /// `Some(rate)` when every slot bakes the same single-key rate — the overwhelmingly common
    /// shape, and the dump instruments' quiet case.
    pub fn constant_rate(&self) -> Option<f32> {
        let mut it = self.rate.iter();
        let first = it.next()?.as_ref()?;
        let &(_, v) = (first.keys.len() == 1).then(|| first.keys.first())??;
        it.all(|a| a.as_ref().is_some_and(|a| a.keys == first.keys))
            .then_some(v)
    }

    /// Per-slot read view for the dump instruments: `(looping, rate keys, enabled keys)`, one per
    /// file sequence slot (`None` = the track keys nothing in that slot). Times are seconds from
    /// the slot's band start — the values the runtime actually samples, not the raw file keys.
    #[allow(clippy::type_complexity)] // a read-only tuple view for the dumps
    pub fn slot_views(&self) -> Vec<(bool, Option<&[(f32, f32)]>, Option<&[(f32, f32)]>)> {
        fn keys(list: &[Option<ScalarAnim>], i: usize) -> Option<&[(f32, f32)]> {
            list.get(i)
                .and_then(|o| o.as_ref())
                .map(|a| a.keys.as_slice())
        }
        (0..self.looping.len())
            .map(|i| (self.looping[i], keys(&self.rate, i), keys(&self.enabled, i)))
            .collect()
    }

    /// Test/tool constructor: a single always-on slot at a constant `rate`, looping.
    pub fn constant(rate: f32) -> Self {
        Self {
            rate: vec![Some(ScalarAnim {
                period: 0.0,
                step: true,
                keys: vec![(0.0, rate)],
            })],
            enabled: vec![None],
            looping: vec![true],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(interp: u16, keys: &[(u32, f32)], ranges: &[(u32, u32)]) -> M2ScalarTrack {
        M2ScalarTrack {
            interp,
            gseq: 0xffff,
            ranges: ranges.to_vec(),
            keys: keys.to_vec(),
        }
    }

    fn slots(bands: &[(u32, u32)]) -> Vec<SeqSlot> {
        bands
            .iter()
            .enumerate()
            .map(|(index, &band)| SeqSlot { index, band })
            .collect()
    }

    /// The STEP rate law: a `{0:0, 67:30}` track is silent before its 67 ms key — the burst
    /// fires AT the key, full-count — and holds 30 after (moved here from the runtime's
    /// `accumulate_emission` tests when the sampling moved into the bake).
    #[test]
    fn step_rate_is_silent_before_its_key_and_holds_after() {
        let t = EmitTiming::bake(
            &track(0, &[(0, 0.0), (67, 30.0)], &[]),
            &M2ScalarTrack::default(),
            &slots(&[(0, 1000)]),
            vec![true],
            &[],
        );
        assert_eq!(t.rate(None, 0.050), 0.0, "before the key: step holds 0");
        assert_eq!(t.rate(None, 0.070), 30.0, "at/after the key: 30");
        assert_eq!(t.rate(None, 0.500), 30.0, "held to the band end");
        assert!(t.emitting(None, 0.5), "no gate track = always on");
    }

    /// The LINEAR ramp law (the BloodSpurt shape `0 → 100 → 0`, interp 1): mid-ramp pours the
    /// interpolated rate, and the self-closing tail really falls back to 0.
    #[test]
    fn lerp_ramp_interpolates_and_self_closes() {
        let t = EmitTiming::bake(
            &track(1, &[(0, 0.0), (100, 100.0), (200, 0.0)], &[]),
            &M2ScalarTrack::default(),
            &slots(&[(0, 1000)]),
            vec![true],
            &[],
        );
        assert!((t.rate(None, 0.050) - 50.0).abs() < 1e-4, "rising mid-ramp");
        assert!(
            (t.rate(None, 0.150) - 50.0).abs() < 1e-4,
            "falling mid-ramp"
        );
        assert_eq!(t.rate(None, 0.500), 0.0, "the ramp self-closes");
    }

    /// The per-sequence window law — the B27 shape, synthesized: an enabled gate authored ON
    /// inside a clamped one-shot clip (slot 0) whose window in the idle loop (slot 1) resolves
    /// to OFF. The old seq-0-only rebase parked the clamped clock at its end value; the bake
    /// must read OFF at idle, run slot 0's window, and HOLD (not wrap) slot 0's tail.
    #[test]
    fn idle_window_is_off_and_a_clamped_clip_holds_its_tail() {
        // Absolute timeline: clip A (one-shot) band 1000..2000, idle band 2333..2667.
        // Gate keys: on@1000, off@1333, on@3800 (a later clip's window, outside both bands).
        let gate = track(
            0,
            &[(1000, 1.0), (1333, 0.0), (3800, 1.0)],
            &[(0, 1), (1, 1), (2, 2)],
        );
        let t = EmitTiming::bake(
            &track(0, &[(0, 20.0)], &[]),
            &gate,
            &slots(&[(1000, 2000), (2333, 2667), (3800, 4100)]),
            vec![false, true, false],
            &[],
        );
        // Idle (slot 1): the collapsed window (1,1) resolves to keys[1] = OFF — at every time,
        // wrap included.
        assert!(!t.emitting(Some(1), 0.0));
        assert!(!t.emitting(Some(1), 0.25));
        assert!(!t.emitting(Some(1), 400.0));
        // The one-shot clip (slot 0): ON at its start, OFF from 333 ms — and the clamped clock
        // HOLDS that tail at/after the band end instead of aliasing back to the ON start.
        assert!(t.emitting(Some(0), 0.1));
        assert!(!t.emitting(Some(0), 0.5));
        assert!(!t.emitting(Some(0), 1.0), "t == period must not alias to 0");
        assert!(
            !t.emitting(Some(0), 5.0),
            "parked long past the end: still off"
        );
        // The later clip (slot 2): its degenerate window is the ON key.
        assert!(t.emitting(Some(2), 0.05));
        // Unknown/out-of-range degrades to slot 0 (the doodad law).
        assert!(t.emitting(None, 0.1));
        assert!(!t.emitting(Some(9), 0.5));
        // The rate is a whole-track constant: same in every slot.
        assert_eq!(t.constant_rate(), Some(20.0));
        assert_eq!(t.peak_rate(), 20.0);
    }

    /// A LOOPING slot wraps its band — a windowed gate re-fires every pass (the precast hold's
    /// pulsing hand flash), where a clamped slot would have parked.
    #[test]
    fn a_looping_band_wraps_its_gate_window() {
        let gate = track(0, &[(0, 1.0), (200, 0.0)], &[]);
        let t = EmitTiming::bake(
            &track(0, &[(0, 40.0)], &[]),
            &gate,
            &slots(&[(0, 1000)]),
            vec![true],
            &[],
        );
        assert!(t.emitting(None, 0.1), "first pass: inside the window");
        assert!(!t.emitting(None, 0.8), "first pass: past it");
        assert!(t.emitting(None, 1.1), "second pass: the window re-fires");
    }

    /// **The dead-slot-0 shape** — `BlastedLandsLightningbolt01.m2`'s emitter 2, synthesized
    /// (decision 0760, bug B63). Two sequences, both anim id 0: a variation chain. Slot 0 keys a
    /// single 0 — a flat silence for its whole band — while the strike itself, `0 → 30 → 0`, is
    /// keyed only in slot 1, which the arm's frequency-weighted roll reaches ~5 % of the time.
    ///
    /// A consumer that PINS slot 0 therefore emits **nothing at all, for ever**, on every
    /// placement: the emitter builds, pools and ticks, and never births a particle. That is what
    /// the placed-doodad lane did until it started passing the slot its own arm actually rolled.
    /// `peak_rate()` folding across all slots is why such an emitter survives the spawn cull — it
    /// looks alive to the build and is dead to the clock. `partslotscan` counts 947 of these.
    #[test]
    fn a_burst_keyed_only_in_a_later_variation_is_silent_in_slot_0() {
        // Absolute timeline: slot 0's band 0..1333, slot 1's band 1367..2667 (the real model's).
        let rate = track(
            1,
            &[
                (0, 0.0),
                (1633, 0.0),
                (1667, 30.0),
                (1800, 30.0),
                (1833, 0.0),
            ],
            &[],
        );
        let t = EmitTiming::bake(
            &rate,
            &M2ScalarTrack::default(),
            &slots(&[(0, 1333), (1367, 2667)]),
            vec![true, true],
            &[],
        );
        // Slot 0 — what the old pinned consumer sampled. Silent across its whole band.
        for s in [0.0, 0.3, 0.6, 0.9, 1.2] {
            assert_eq!(t.rate(Some(0), s), 0.0, "slot 0 is a flat zero at {s}s");
        }
        assert_eq!(
            t.rate(None, 0.5),
            0.0,
            "`None` degrades to slot 0 — also silent"
        );
        // Slot 1 — what the arm actually rolled. The strike is here, and only here.
        assert_eq!(t.rate(Some(1), 0.0), 0.0, "slot 1 opens closed");
        assert_eq!(t.rate(Some(1), 0.316), 30.0, "the burst fires mid-band");
        assert_eq!(t.rate(Some(1), 0.5), 0.0, "and self-closes");
        // The trap that hid it: the build-time cull sees a live emitter either way.
        assert_eq!(
            t.peak_rate(),
            30.0,
            "peak folds ACROSS slots — never culled"
        );
        assert_eq!(
            t.constant_rate(),
            None,
            "and it is not a constant-rate emitter"
        );
    }
}
