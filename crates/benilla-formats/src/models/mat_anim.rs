//! The animated **material-alpha** bake (decision 0130 phase 2): each M2 batch's colour-alpha and
//! transparency-weight tracks, baked to loopable second-domain keys the runtime samples per instance
//! — **one loop per sequence**, because which loop plays is a function of the playing animation.
//!
//! Byte ground (wow-re `m2-alpha-combine-cull.md`, VERIFIED): the per-batch alpha is
//! `A = instanceAlpha × colors[colorIndex].alpha × transparency[transLookup[idx]].weight`, both
//! tracks animation-evaluated each frame; `A ≤ 0` skips the batch before the blend mode is read, and
//! an Opaque batch with `0 < A < 1` still draws opaque (no blend promotion). Clocks (wow-re
//! `eval.md`/`doodad-anim-host.md`): a `gseq`-tagged track wraps `global_sequences[gseq]`; an
//! ordinary track keys inside the **playing** sequence's absolute time band — for a placed doodad
//! that is the file-order-first sequence looping forever, but a creature changes sequence constantly
//! and its batches' visibility changes with it (a voidwalker's upper armour pair is weight 0 in
//! Stand/Walk/Run and 1 only in Death).
//!
//! The sampler + clock resolution live in [`super::key_anim`] (shared with the texture-transform
//! bake); this module owns only the alpha channel's semantics.

use benilla_m2::{M2ScalarTrack, M2Vec3Track};

use super::key_anim::{bake_track, KeyAnim, SeqSlot};

/// One baked scalar loop, seconds — see [`KeyAnim`]. The alpha channels' instantiation.
pub type ScalarAnim = KeyAnim<f32>;

impl KeyAnim<f32> {
    /// Sample at `elapsed` seconds on the loop clock (`1.0` — the multiplicative identity — for a
    /// defensively-empty loop the bake never emits).
    pub fn sample(&self, elapsed: f32) -> f32 {
        self.sample_or(elapsed, 1.0)
    }
}

/// One baked RGB tint loop — the M2Color **colour** track's instantiation (the per-batch tint the
/// real client multiplies into the vertex colour, animation-evaluated like the alpha).
pub type RgbAnim = KeyAnim<[f32; 3]>;

impl KeyAnim<[f32; 3]> {
    /// Sample at `elapsed` seconds on the loop clock (white — the tint identity — for a
    /// defensively-empty loop the bake never emits).
    pub fn sample(&self, elapsed: f32) -> [f32; 3] {
        self.sample_or(elapsed, [1.0, 1.0, 1.0])
    }
}

/// A batch's animated-alpha pair **for one sequence**: the colour-alpha factor and the
/// transparency-weight factor — multiplied together (and by the instance alpha) at sample time, per
/// the verified combine. Either side absent ⇒ that factor is constant `1` (or already baked/culled
/// statically) in this sequence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlphaSeq {
    pub color: Option<ScalarAnim>,
    pub weight: Option<ScalarAnim>,
}

impl AlphaSeq {
    /// The combined factor at `elapsed` seconds into this sequence.
    pub fn sample(&self, elapsed: f32) -> f32 {
        let c = self.color.as_ref().map_or(1.0, |a| a.sample(elapsed));
        let w = self.weight.as_ref().map_or(1.0, |a| a.sample(elapsed));
        c * w
    }

    /// Nothing to evaluate — both factors are the identity in this sequence.
    fn is_empty(&self) -> bool {
        self.color.is_none() && self.weight.is_none()
    }
}

/// A batch's animated alpha across the **whole model**: one [`AlphaSeq`] per sequence, indexed by
/// the model's **file** sequence slot (the same slot [`benilla_m2::M2Track::ranges`] is indexed by,
/// and the one [`crate::ModelAnimation::seq_index`] carries to the runtime).
///
/// Per-sequence and not per-model because the reference re-reads the tracks from the *playing*
/// sequence's key window every frame: the same batch is hidden in Stand and drawn in Death. A
/// consumer that doesn't know which sequence is playing (a placed doodad, which arms
/// `animations[0]` once and loops it forever) passes `None` and gets slot 0 — the old behaviour,
/// still correct there.
#[derive(Clone, Debug, PartialEq)]
pub struct AlphaAnim {
    per_seq: Vec<AlphaSeq>,
}

impl AlphaAnim {
    /// Build from one entry per file sequence slot, or `None` if no sequence animates either factor
    /// (the overwhelming majority of batches — the caller then carries no anim at all). Public so a
    /// consumer's tests can build the shape the bake emits without a real M2.
    pub fn new(per_seq: Vec<AlphaSeq>) -> Option<Self> {
        per_seq
            .iter()
            .any(|s| !s.is_empty())
            .then_some(Self { per_seq })
    }

    /// This batch's factors in sequence slot `seq` (slot 0 when unknown / out of range — the
    /// doodad lane's one-time arm, and the safe degrade for a model whose sequence list and track
    /// ranges disagree).
    pub fn seq(&self, seq: Option<usize>) -> &AlphaSeq {
        const IDENTITY: &AlphaSeq = &AlphaSeq {
            color: None,
            weight: None,
        };
        seq.and_then(|i| self.per_seq.get(i))
            .or_else(|| self.per_seq.first())
            .unwrap_or(IDENTITY)
    }

    /// The combined factor `elapsed` seconds into sequence slot `seq`.
    pub fn sample(&self, seq: Option<usize>, elapsed: f32) -> f32 {
        self.seq(seq).sample(elapsed)
    }

    /// Whether ANY sequence can drive this batch's alpha to zero — i.e. whether the batch is ever
    /// culled. The spawn-side gate for "does this part need a live sampler at all".
    pub fn ever_hides(&self) -> bool {
        self.per_seq.iter().any(|s| {
            [s.color.as_ref(), s.weight.as_ref()]
                .into_iter()
                .flatten()
                .any(|a| a.keys.iter().any(|&(_, v)| v <= 0.0))
        })
    }
}

/// Bake one scalar track to a [`ScalarAnim`], or `None` when it contributes nothing the static path
/// doesn't already handle: no keys (factor doesn't apply), a constant 1 (identity), or a constant
/// ≤ 0 (the static cull already dropped the batch). A dimming constant IS kept — the real combine
/// dims the batch by it, which the static vertex bake never did — and a band-empty track holding a
/// non-1 value bakes that hold, **including a held 0** (the batch must stay hidden every frame; the
/// static cull never saw it because the full track isn't constant).
pub(super) fn bake_scalar_anim(
    track: &M2ScalarTrack,
    gseq_durations: &[u32],
    seq: Option<SeqSlot>,
) -> Option<ScalarAnim> {
    bake_track(
        track,
        gseq_durations,
        seq,
        |v| v,
        |c| (c - 1.0).abs() < f32::EPSILON || c <= 0.0,
        |v| (v - 1.0).abs() < f32::EPSILON,
    )
}

/// Bake one M2Color **RGB** track to an [`RgbAnim`], or `None` unless the track genuinely varies
/// inside the model's clock (a spell effect's white-hot flash cooling to red). Constants — and
/// band-empty/single-key holds — stay `None` so the **static vertex-colour bake** (`m2_batches`,
/// decision 0029) keeps owning them unchanged; only a time-varying track moves the tint to the
/// animated material channel (and skips the vertex bake, so the two never double-apply).
pub(super) fn bake_rgb_anim(
    track: &M2Vec3Track,
    gseq_durations: &[u32],
    seq: Option<SeqSlot>,
) -> Option<RgbAnim> {
    bake_track(track, gseq_durations, seq, |v| v, |_| true, |_| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo root's `WoW/Data` (gitignored; the real-data test skips when absent).
    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// The Stormwind mage portal, straight off the real client data: its shimmer is authored as
    /// animated transparency-weight tracks (3 of its 4 records are time-varying — `doodadscan`'s
    /// material-channel listing), so `parse_m2_render_submeshes` must emit at least one batch with a
    /// live weight loop whose sampled value actually varies. Guards the whole bake chain — the full
    /// scalar-track parse (benilla-m2), the transLookup two-hop, the gseq/band clock resolution —
    /// against a silent regression. Skips when the client data isn't present.
    #[test]
    fn mage_portal_bakes_a_time_varying_weight_loop() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("World\\generic\\activedoodads\\mageportals\\StormwindMagePortal01.m2")
            .expect("read StormwindMagePortal01.m2");
        let subs = super::super::parse_m2_render_submeshes(&bytes, "", &[]).expect("parse");
        let animated: Vec<&ScalarAnim> = subs
            .iter()
            .filter_map(|s| s.alpha_anim.as_ref())
            .filter_map(|a| a.seq(None).weight.as_ref())
            .filter(|w| w.period > 0.0 && w.keys.len() > 1)
            .collect();
        assert!(
            !animated.is_empty(),
            "the portal's shimmer batches carry time-varying weight loops"
        );
        let w = animated[0];
        // The loop genuinely moves: two samples a quarter-period apart differ.
        let (a, b) = (w.sample(0.0), w.sample(w.period * 0.25));
        assert!(
            (a - b).abs() > 1e-3,
            "sampled weight varies over the loop (got {a} vs {b}, period {})",
            w.period
        );
        assert!(
            w.keys.iter().all(|&(_, v)| (0.0..=1.0).contains(&v)),
            "fix16 weights decode into [0, 1]"
        );
    }

    /// Battle Shout's ground model, straight off the real client data: six additive crescent
    /// quads whose whole look is material animation — staggered colour-alpha pulses, dimming
    /// weight constants (0.3/0.2/0.4), and RGB tracks cooling white→red over the 0.9 s clip.
    /// Every batch must bake a time-varying colour-alpha loop AND a time-varying RGB loop, with
    /// the static vertex tint skipped (the material channel owns it — a first-key vertex bake
    /// would freeze four of six crescents white). Guards the RGB half of the bake chain end to
    /// end. Skips when the client data isn't present.
    #[test]
    fn battle_shout_base_bakes_alpha_and_rgb_loops() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Spells\\BattleShout_Cast_Base.m2")
            .expect("read BattleShout_Cast_Base.m2");
        let subs = super::super::parse_m2_render_submeshes(&bytes, "", &[]).expect("parse");
        assert_eq!(subs.len(), 6, "six crescent batches");
        for (i, s) in subs.iter().enumerate() {
            let a = s.alpha_anim.as_ref().unwrap_or_else(|| {
                panic!("batch {i}: the colour-alpha/weight loops must bake");
            });
            let seq0 = a.seq(None);
            let c = seq0.color.as_ref().expect("time-varying colour-alpha");
            assert!(
                c.period > 0.0 && c.keys.len() > 1,
                "batch {i}: alpha varies"
            );
            let w = seq0.weight.as_ref().expect("dimming weight constant");
            let wv = w.sample(0.0);
            assert!(
                (0.19..=0.41).contains(&wv),
                "batch {i}: weight dims to 0.2–0.4 (got {wv})"
            );
            let rgb = s.rgb_anim.as_ref().unwrap_or_else(|| {
                panic!("batch {i}: the time-varying RGB tint must bake");
            });
            assert!(
                rgb.period > 0.0 && rgb.keys.len() > 1,
                "batch {i}: RGB varies"
            );
            // The authored cool-down: red-dominant by mid-life on every crescent.
            let mid = rgb.sample(rgb.period * 0.5);
            assert!(
                mid[0] > 0.6 && mid[1] < 0.1 && mid[2] < 0.1,
                "batch {i}: mid-life tint is red (got {mid:?})"
            );
            assert!(
                s.vertex_colors.is_empty(),
                "batch {i}: the static vertex tint is skipped when the RGB animates"
            );
        }
    }

    /// **B16 — the voidwalker's floating armour.** Straight off the real client data: skin batches
    /// 0 and 1 (two 95-vertex shoulder pieces at z≈2.99/2.59) carry transparency weight **0** in
    /// every ordinary sequence and 1 only in Death/131/132 — authored to appear on death. Their
    /// twins at z≈1.46 are weight 1 throughout, so drawing all four reads as armour floating above
    /// the body, which is exactly what the reporter saw.
    ///
    /// Pins the whole per-sequence chain: the ranges parse (benilla-m2), the per-slot bake, and the
    /// combine. Before it, the bake read sequence 0's band once and the runtime had no way to ask a
    /// different sequence — so the pair drew in every animation.
    #[test]
    fn voidwalker_hides_its_death_only_armour_in_every_other_sequence() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Creature\\VoidWalker\\VoidWalker.m2")
            .expect("read VoidWalker.m2");
        let subs = super::super::parse_m2_render_submeshes(&bytes, "", &[]).expect("parse");
        // File slots, from `benilla-extract m2seq`: 0 = Stand (3333..6000 ms), 1 = Walk, 2 = Run,
        // 5/6 = the Stand variations, 10 = Death (anim 1, 43333..46333 ms).
        const STAND: usize = 0;
        const DEATH: usize = 10;
        for batch in [0, 1] {
            let a = subs[batch]
                .alpha_anim
                .as_ref()
                .unwrap_or_else(|| panic!("batch {batch} carries the weight track"));
            // Hidden for the WHOLE Stand loop, not merely at t=0.
            for step in 0..16u16 {
                let t = 2.667 * f32::from(step) / 16.0;
                assert_eq!(
                    a.sample(Some(STAND), t),
                    0.0,
                    "batch {batch} is hidden {t}s into Stand"
                );
            }
            for slot in [1, 2, 7, 8, 9, 11] {
                assert_eq!(
                    a.sample(Some(slot), 0.3),
                    0.0,
                    "batch {batch}, sequence {slot}"
                );
            }
            // Death brings it in — the authored reason the geometry exists at all.
            let peak = (0..32u16)
                .map(|s| a.sample(Some(DEATH), 3.0 * f32::from(s) / 32.0))
                .fold(0.0f32, f32::max);
            assert!(
                peak > 0.9,
                "batch {batch} appears during Death (peak {peak})"
            );
        }
        // The body twins the reference DOES draw while standing stay drawn.
        for batch in [4, 5] {
            let drawn = subs[batch]
                .alpha_anim
                .as_ref()
                .is_none_or(|a| a.sample(Some(STAND), 0.5) > 0.0);
            assert!(drawn, "batch {batch} draws in Stand");
        }
        // **The nearest-key trap.** Batch 2 (the 271-vertex body) keys 1.0 at 3333 ms and 0.0 at
        // 44200 ms on a STEP track, and the Stand *variations* (slots 5/6, 23333..29333 ms) key
        // nothing — they sit nearer the 0. Clamping to the nearest authored key would hide the
        // whole body in every variation, attack and emote; the reference's window says key 0, so
        // it stays visible. This is the assertion that fails if the empty-band rule regresses to
        // "nearest".
        let body = subs[2].alpha_anim.as_ref();
        for slot in [5, 6, 7, 8, 9, 12] {
            let v = body.map_or(1.0, |a| a.sample(Some(slot), 0.5));
            assert!(
                v > 0.0,
                "the voidwalker body draws in sequence {slot} (got {v})"
            );
        }
    }

    /// **B20 — the banshee.** Same shape, different model: render batches **20–25** are weight 0
    /// in every sequence a live banshee ever plays and only come in across the Death band — among
    /// them a 57-vertex piece parked 1.18 yd to the SIDE of the body, which drawn permanently is
    /// precisely the reported "parts floating in the air". Two of the six are further dimmed by a
    /// constant-0.4 colour alpha, so they never reach full even on death.
    ///
    /// Indices are render batches (a billboard batch splits per bone, so this list runs longer than
    /// the skin list — `benilla-extract m2alpha` prints both views). The test re-checks each one's
    /// identity by vertex count so a change in batch ordering fails loudly instead of quietly
    /// asserting about some other geometry.
    #[test]
    fn banshee_hides_its_death_only_batches() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Creature\\Banshee\\Banshee.m2")
            .expect("read Banshee.m2");
        let subs = super::super::parse_m2_render_submeshes(&bytes, "", &[]).expect("parse");
        assert_eq!(subs.len(), 26, "the banshee's render batch list");
        // Slots from `m2seq`: 0 = Stand (10000..13333 ms), 3 = Walk, 4 = Run, 12 = Death.
        const LIVE: [usize; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        const DEATH: usize = 12;
        // (render batch, vertex count) — the identity re-check.
        for (batch, verts) in [(20, 57), (21, 60), (22, 4), (23, 4), (24, 4), (25, 4)] {
            assert_eq!(subs[batch].positions.len(), verts, "batch {batch} identity");
            let a = subs[batch]
                .alpha_anim
                .as_ref()
                .unwrap_or_else(|| panic!("batch {batch} is alpha-keyed"));
            for slot in LIVE {
                for step in 0..8u16 {
                    let t = f32::from(step) * 0.4;
                    assert_eq!(
                        a.sample(Some(slot), t),
                        0.0,
                        "batch {batch} is hidden {t}s into sequence {slot}"
                    );
                }
            }
            let peak = (0..64u16)
                .map(|k| a.sample(Some(DEATH), 4.167 * f32::from(k) / 64.0))
                .fold(0.0f32, f32::max);
            assert!(peak > 0.3, "batch {batch} appears on death (peak {peak})");
        }
        // Everything the reference DOES draw on a live banshee stays drawn — the fix must not turn
        // into a wholesale hide. Batch 8 is the one authored exception among these: the
        // `BITCHFACE5.BLP` face, weight 0 while standing and pulsed to 0.6 by the attack/emote
        // sequences — the scream face, not a bug.
        for (i, sub) in subs.iter().enumerate().take(20).filter(|(i, _)| *i != 8) {
            let v = sub
                .alpha_anim
                .as_ref()
                .map_or(1.0, |a| a.sample(Some(0), 0.5));
            assert!(v > 0.0, "batch {i} draws while standing (got {v})");
        }
        let face = subs[8]
            .alpha_anim
            .as_ref()
            .expect("the face batch is alpha-keyed");
        assert_eq!(
            face.sample(Some(0), 0.5),
            0.0,
            "the scream face hides while standing"
        );
        let scream = (0..64u16)
            .map(|k| face.sample(Some(1), f32::from(k) / 64.0))
            .fold(0.0f32, f32::max);
        assert!(
            scream > 0.5,
            "and pulses in during the emote (peak {scream})"
        );
    }

    fn track(gseq: u16, interp: u16, keys: &[(u32, f32)]) -> M2ScalarTrack {
        M2ScalarTrack {
            interp,
            gseq,
            ranges: Vec::new(),
            keys: keys.to_vec(),
        }
    }

    /// The same track with explicit per-sequence key windows — the array the reference's search
    /// indexes by the playing sequence's file slot, and the only thing that says what a band with
    /// no keys of its own holds.
    fn ranged(interp: u16, keys: &[(u32, f32)], ranges: &[(u32, u32)]) -> M2ScalarTrack {
        M2ScalarTrack {
            interp,
            gseq: 0xffff,
            ranges: ranges.to_vec(),
            keys: keys.to_vec(),
        }
    }

    /// Bake against sequence file slot `index` with band `band`.
    fn bake_at(t: &M2ScalarTrack, index: usize, band: (u32, u32)) -> Option<ScalarAnim> {
        bake_scalar_anim(t, &[], Some(SeqSlot { index, band }))
    }

    /// The bake's contribution gate: keyless and constant-1 tracks vanish; a dimming constant is
    /// kept as period-0 (the combine the static vertex bake never applied); constant-0 is the
    /// static cull's job, not a runtime anim.
    #[test]
    fn bake_keeps_only_what_the_static_path_cannot_do() {
        assert_eq!(bake_scalar_anim(&track(0xffff, 1, &[]), &[], None), None);
        assert_eq!(
            bake_scalar_anim(&track(0xffff, 1, &[(0, 1.0)]), &[], None),
            None
        );
        assert_eq!(
            bake_scalar_anim(&track(0xffff, 1, &[(0, 0.0), (500, 0.0)]), &[], None),
            None
        );
        let dim = bake_scalar_anim(&track(0xffff, 1, &[(0, 0.4)]), &[], None).unwrap();
        assert_eq!(dim.period, 0.0);
        assert_eq!(dim.sample(123.0), 0.4);
    }

    /// A gseq-tagged track wraps the global-sequence duration — the fire-flicker shape: value
    /// pulses over a 1.5 s loop regardless of any playing sequence.
    #[test]
    fn gseq_track_wraps_the_table_duration() {
        let a = bake_scalar_anim(
            &track(1, 1, &[(0, 0.2), (750, 1.0), (1500, 0.2)]),
            &[9999, 1500],
            None,
        )
        .unwrap();
        assert_eq!(a.period, 1.5);
        assert!((a.sample(0.375) - 0.6).abs() < 1e-4); // linear midpoint
        assert!((a.sample(1.5 + 0.375) - 0.6).abs() < 1e-4); // wraps
    }

    /// A sequence-timeline track keeps only the first sequence's band, rebased to seconds — keys
    /// belonging to other sequences (outside the band) don't bleed in.
    #[test]
    fn sequence_track_bakes_one_band_at_a_time() {
        // Keys 0..2; sequence 0's window is keys 0-1, sequence 1's is keys 1-2.
        let t = ranged(
            1,
            &[(1000, 0.0), (1500, 1.0), (5000, 0.3)],
            &[(0, 1), (1, 2)],
        );
        let a = bake_at(&t, 0, (1000, 2000)).unwrap();
        assert_eq!(a.period, 1.0);
        assert!((a.sample(0.25) - 0.5).abs() < 1e-6);
        // Past the band's last key the value keeps ramping toward the NEXT key — the reference
        // takes `k1 = k0 + 1` bounded only by the total key count, never by the window's `hi`
        // (wow-re `eval.md` FN1 §4/§5), so key 2 at 5000 ms is a live lerp endpoint here even
        // though it belongs to a later sequence. It never wrap-lerps back to key 0.
        let expect = 1.0 + (0.3 - 1.0) * (1900.0 - 1500.0) / (5000.0 - 1500.0);
        assert!(
            (a.sample(0.9) - expect).abs() < 1e-4,
            "got {} want {expect}",
            a.sample(0.9)
        );
        // A different sequence over the same track reads a different window entirely: band 1 opens
        // mid-segment (the 1.0→0.3 ramp is 5/7 of the way through at 4000 ms) and closes on key 2.
        // Sampled just under the period — at exactly `period` the loop has already wrapped.
        let b = bake_at(&t, 1, (4000, 5000)).unwrap();
        assert!((b.sample(0.0) - 0.5).abs() < 1e-4, "got {}", b.sample(0.0));
        assert!(
            (b.sample(0.999) - 0.3).abs() < 1e-3,
            "got {}",
            b.sample(0.999)
        );
    }

    /// **The per-sequence bake, and the bug it fixes.** One track, two bands: a batch keyed 0 in
    /// the first sequence and 1 in the second (the voidwalker armour shape — hidden while standing,
    /// shown on death). Baking band 0 and calling it the model's answer freezes the batch hidden
    /// in every animation; each band must bake its own value.
    #[test]
    fn each_sequence_bakes_its_own_band() {
        let t = ranged(
            1,
            &[(1000, 0.0), (2000, 0.0), (4000, 1.0), (5000, 1.0)],
            &[(0, 1), (2, 3)],
        );
        let stand = bake_at(&t, 0, (1000, 2000)).unwrap();
        assert_eq!(stand.sample(0.0), 0.0, "hidden for the whole first band");
        assert_eq!(stand.sample(0.9), 0.0);
        let death = bake_at(&t, 1, (4000, 5000));
        // Constant 1 in that band is the identity — nothing to animate, the batch just draws.
        assert_eq!(death, None, "the second band is a plain visible batch");
    }

    /// A band that keys nothing holds `keys[ranges[slot].lo]` — the reference's collapsed key
    /// window (`lo >= hi` ⇒ the degenerate `{lo, lo, 0}` result, wow-re `eval.md` FN1). Including a
    /// held **0**, which must keep hiding the batch every frame of that sequence: the static cull
    /// never saw it, because the full track isn't constant.
    #[test]
    fn band_empty_track_holds_its_bracket_key() {
        // Slot 1's window has collapsed onto key 0 (value 0) — the band is hidden.
        let t = ranged(1, &[(100, 0.0), (5000, 1.0)], &[(0, 1), (0, 0)]);
        let a = bake_at(&t, 1, (1000, 2000)).unwrap();
        assert_eq!(a.period, 0.0);
        assert_eq!(a.sample(42.0), 0.0);
        // A collapsed window onto a 1 contributes nothing — the batch simply draws.
        let one = ranged(1, &[(100, 1.0), (5000, 0.3)], &[(0, 1), (0, 0)]);
        assert_eq!(bake_at(&one, 1, (1000, 2000)), None);
    }

    /// The bracket-key rule is NOT "the nearest authored key": a **step** track whose band sits
    /// between a 1 and a later 0 holds the 1, because the window's low index is the search result
    /// for every time in the band. Nearest-key would pick the 0 and hide a batch the reference
    /// draws — measured on `VoidWalker.m2`, where it hides the whole 271-vertex body in every
    /// Stand variation, attack and emote.
    #[test]
    fn empty_band_uses_the_window_low_key_not_the_nearest() {
        let t = ranged(0, &[(3333, 1.0), (44200, 0.0)], &[(0, 1), (0, 1)]);
        // Band 23333..26000 keys nothing and sits NEARER the 0 — but the window says key 0.
        let a = bake_at(&t, 1, (23333, 26000));
        assert_eq!(a, None, "the batch stays visible (constant 1 = identity)");
    }

    /// No ranges array at all is the reference's `[track+4] == 0` fallback: the window is the whole
    /// key list, so the band samples the track's ordinary value at its own times.
    #[test]
    fn missing_ranges_search_the_whole_key_list() {
        let t = track(0xffff, 0, &[(0, 1.0), (10_000, 0.0)]);
        let a = bake_at(&t, 7, (20_000, 21_000)).unwrap();
        assert_eq!(a.sample(0.5), 0.0, "past the last key: hold it");
    }

    /// Step interpolation holds each key until the next (the kernel's `interp == 0` leg).
    #[test]
    fn step_tracks_hold_between_keys() {
        let a = bake_scalar_anim(&track(0, 0, &[(0, 0.2), (1000, 1.0)]), &[2000], None).unwrap();
        assert!(a.step);
        assert_eq!(a.sample(0.999), 0.2);
        assert_eq!(a.sample(1.0), 1.0);
    }

    fn dim(v: f32) -> Option<ScalarAnim> {
        Some(ScalarAnim {
            period: 0.0,
            step: false,
            keys: vec![(0.0, v)],
        })
    }

    /// The combined pair multiplies its two factors — the verified combine's track half — and an
    /// all-identity model bakes to no anim at all.
    #[test]
    fn alpha_anim_multiplies_color_and_weight() {
        let both = AlphaAnim::new(vec![AlphaSeq {
            color: dim(0.5),
            weight: dim(0.5),
        }])
        .expect("a dimming pair is worth carrying");
        assert!((both.sample(None, 7.0) - 0.25).abs() < 1e-6);
        assert_eq!(AlphaAnim::new(vec![AlphaSeq::default()]), None);
    }

    /// Sequence addressing: each slot samples its own pair, an unknown/out-of-range slot degrades
    /// to slot 0 (the doodad lane's one-time arm), and `ever_hides` reports whether ANY sequence
    /// can cull the batch.
    #[test]
    fn alpha_anim_addresses_by_sequence_slot() {
        let a = AlphaAnim::new(vec![
            AlphaSeq {
                color: None,
                weight: dim(0.0),
            },
            AlphaSeq::default(),
            AlphaSeq {
                color: None,
                weight: dim(0.5),
            },
        ])
        .unwrap();
        assert_eq!(a.sample(Some(0), 0.0), 0.0);
        assert_eq!(a.sample(Some(1), 0.0), 1.0);
        assert_eq!(a.sample(Some(2), 0.0), 0.5);
        assert_eq!(a.sample(Some(99), 0.0), 0.0, "out of range ⇒ slot 0");
        assert_eq!(a.sample(None, 0.0), 0.0, "unknown sequence ⇒ slot 0");
        assert!(a.ever_hides());
        let never = AlphaAnim::new(vec![AlphaSeq {
            color: None,
            weight: dim(0.5),
        }])
        .unwrap();
        assert!(!never.ever_hides());
    }
}
