//! The animated **material-alpha** bake (decision 0130 phase 2): each M2 batch's colour-alpha and
//! transparency-weight tracks, baked to loopable second-domain keys the runtime samples per instance.
//!
//! Byte ground (wow-re `m2-alpha-combine-cull.md`, VERIFIED): the per-batch alpha is
//! `A = instanceAlpha × colors[colorIndex].alpha × transparency[transLookup[idx]].weight`, both
//! tracks animation-evaluated each frame; `A ≤ 0` skips the batch before the blend mode is read, and
//! an Opaque batch with `0 < A < 1` still draws opaque (no blend promotion). Clocks (wow-re
//! `eval.md`/`doodad-anim-host.md`): a `gseq`-tagged track wraps `global_sequences[gseq]`; an
//! ordinary track keys inside the playing sequence's absolute time band — for a placed doodad that
//! is the file-order-first sequence, looping forever.
//!
//! The sampler + clock resolution live in [`super::key_anim`] (shared with the texture-transform
//! bake); this module owns only the alpha channel's semantics.

use benilla_m2::{M2ScalarTrack, M2Vec3Track};

use super::key_anim::{bake_track, KeyAnim};

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

/// A batch's animated-alpha pair: the colour-alpha factor and the transparency-weight factor —
/// multiplied together (and by the instance fade) at sample time, per the verified combine. Either
/// side absent ⇒ that factor is constant `1` (or already baked/culled statically).
#[derive(Clone, Debug, PartialEq)]
pub struct AlphaAnim {
    pub color: Option<ScalarAnim>,
    pub weight: Option<ScalarAnim>,
}

impl AlphaAnim {
    /// The combined factor at `elapsed` seconds since the instance's clock origin.
    pub fn sample(&self, elapsed: f32) -> f32 {
        let c = self.color.as_ref().map_or(1.0, |a| a.sample(elapsed));
        let w = self.weight.as_ref().map_or(1.0, |a| a.sample(elapsed));
        c * w
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
    seq0: Option<(u32, u32)>,
) -> Option<ScalarAnim> {
    bake_track(
        track,
        gseq_durations,
        seq0,
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
    seq0: Option<(u32, u32)>,
) -> Option<RgbAnim> {
    bake_track(track, gseq_durations, seq0, |v| v, |_| true, |_| true)
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
            .filter_map(|a| a.weight.as_ref())
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
            let c = a.color.as_ref().expect("time-varying colour-alpha");
            assert!(
                c.period > 0.0 && c.keys.len() > 1,
                "batch {i}: alpha varies"
            );
            let w = a.weight.as_ref().expect("dimming weight constant");
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

    fn track(gseq: u16, interp: u16, keys: &[(u32, f32)]) -> M2ScalarTrack {
        M2ScalarTrack {
            interp,
            gseq,
            keys: keys.to_vec(),
        }
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
    fn sequence_track_bakes_the_first_band_only() {
        let t = track(0xffff, 1, &[(1000, 0.0), (1500, 1.0), (5000, 0.3)]);
        let a = bake_scalar_anim(&t, &[], Some((1000, 2000))).unwrap();
        assert_eq!(a.period, 1.0);
        assert_eq!(a.keys, vec![(0.0, 0.0), (0.5, 1.0)]);
        assert!((a.sample(0.25) - 0.5).abs() < 1e-6);
        // Past the last in-band key: hold, no wrap-lerp.
        assert_eq!(a.sample(0.9), 1.0);
    }

    /// A band-empty track holds its nearest earlier key — including a held **0**, which must keep
    /// hiding the batch (the static cull never saw it: the full track isn't constant). The phase-2
    /// bake dropped this hold; the shared core keeps it.
    #[test]
    fn band_empty_track_holds_a_zero_and_keeps_hiding() {
        let t = track(0xffff, 1, &[(100, 0.0), (5000, 1.0)]);
        let a = bake_scalar_anim(&t, &[], Some((1000, 2000))).unwrap();
        assert_eq!(a.period, 0.0);
        assert_eq!(a.sample(42.0), 0.0);
        // A held 1 (or nothing held) still contributes nothing.
        let one = track(0xffff, 1, &[(100, 1.0), (5000, 0.3)]);
        assert_eq!(bake_scalar_anim(&one, &[], Some((1000, 2000))), None);
    }

    /// Step interpolation holds each key until the next (the kernel's `interp == 0` leg).
    #[test]
    fn step_tracks_hold_between_keys() {
        let a = bake_scalar_anim(&track(0, 0, &[(0, 0.2), (1000, 1.0)]), &[2000], None).unwrap();
        assert!(a.step);
        assert_eq!(a.sample(0.999), 0.2);
        assert_eq!(a.sample(1.0), 1.0);
    }

    /// The combined pair multiplies its two factors — the verified combine's track half.
    #[test]
    fn alpha_anim_multiplies_color_and_weight() {
        let both = AlphaAnim {
            color: Some(ScalarAnim {
                period: 0.0,
                step: false,
                keys: vec![(0.0, 0.5)],
            }),
            weight: Some(ScalarAnim {
                period: 0.0,
                step: false,
                keys: vec![(0.0, 0.5)],
            }),
        };
        assert!((both.sample(7.0) - 0.25).abs() < 1e-6);
        let neither = AlphaAnim {
            color: None,
            weight: None,
        };
        assert_eq!(neither.sample(0.0), 1.0);
    }
}
