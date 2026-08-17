//! The **texture-transform** (UV-animation) bake (decision 0130 phase 3): each M2 batch's texture
//! transform — batch `textureTransformComboIndex` (texUnit `+0x16`) → texAnimLookup (header `0xac`,
//! `0xffff`/out-of-range = none) → the record's **translation** track, baked to a loopable UV-offset
//! loop on the same clocks as the material-alpha bake (gseq wrap / seq-0 band).
//!
//! Translation only, deliberately: the full 7968-model 1.12 corpus sweep found 281 of 286
//! transforms translation-keyed, and every rotation/scaling user is a UI/spell-effect/goober model
//! — **zero placed world doodads** — so the doodad lane (0130's scope) needs no rotation/scaling.
//! The baked offset is the track's raw x/y; how the real client maps it onto U/V (sign/axis, and
//! the application mechanism) is under RE in wow-re — the renderer applies the convention once,
//! when it consumes this.

use benilla_m2::M2Model;

use super::key_anim::{bake_track, KeyAnim, SeqLoops, SeqSlot};

/// One baked UV-offset loop, seconds — see [`KeyAnim`]. The texture-transform translation
/// channel's instantiation: values are the track's raw `(x, y)`.
pub type UvAnim = KeyAnim<[f32; 2]>;

impl KeyAnim<[f32; 2]> {
    /// The UV offset at `elapsed` seconds on the loop clock (`[0, 0]` — the additive identity —
    /// for a defensively-empty loop the bake never emits).
    pub fn sample(&self, elapsed: f32) -> [f32; 2] {
        self.sample_or(elapsed, [0.0, 0.0])
    }
}

/// `(x, y)` within a hair of zero — the offset that moves nothing.
fn is_zero(v: [f32; 2]) -> bool {
    v[0].abs() < 1e-6 && v[1].abs() < 1e-6
}

/// Bake the batch's UV-offset loop, or `None` when the batch has no texture transform (the
/// overwhelmingly common case), the looked-up record is absent (`0xffff` sentinel falls out of the
/// bounds check), or its translation track never moves the UVs.
///
/// Deliberately **one sequence**, unlike the material-alpha bake next door: a UV loop is consumed
/// through a shared per-MATERIAL registry (`doodad_anim::UvAnimMaterials` — one uniform for every
/// instance of a batch), which has no per-instance sequence to key on. Making it per-sequence needs
/// that registry to become per-instance first; every UV-animating model measured is a scroll that
/// runs the same in every sequence, so nothing observable rests on it (recorded, not assumed).
pub(super) fn bake_uv_anim(
    model: &M2Model,
    combo_index: u16,
    seq0: Option<SeqSlot>,
) -> Option<UvAnim> {
    let ti = *model.texture_transform_lookup.get(combo_index as usize)?;
    let t = model.texture_transforms.get(ti as usize)?;
    bake_track(
        &t.translation,
        &model.global_sequences,
        seq0,
        |v| [v[0], v[1]],
        is_zero,
        is_zero,
    )
}

/// Bake the batch's UV-offset loop **for every file sequence slot** — the per-sequence form of
/// [`bake_uv_anim`], and the answer to the question that function's own doc used to settle by
/// measurement ("every UV-animating model measured is a scroll that runs the same in every
/// sequence"). `benilla-extract uvslotscan` re-ran that measurement and it is false: 22 of the 32
/// multi-slot UV batch-channels in the 1.12 corpus have a **dead slot 0** beside a live later slot.
///
/// `None` when no slot animates at all. The caller keeps [`bake_uv_anim`]'s slot-0 loop for the
/// shared-material lane and reaches for this only when [`SeqLoops::uniform`] declines (decision
/// 1408).
pub(super) fn bake_uv_seqs(
    model: &M2Model,
    combo_index: u16,
    slots: &[SeqSlot],
) -> Option<SeqLoops<[f32; 2]>> {
    let ti = *model.texture_transform_lookup.get(combo_index as usize)?;
    let t = model.texture_transforms.get(ti as usize)?;
    SeqLoops::new(
        slots
            .iter()
            .map(|&slot| {
                bake_track(
                    &t.translation,
                    &model.global_sequences,
                    Some(slot),
                    |v| [v[0], v[1]],
                    is_zero,
                    is_zero,
                )
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_m2::M2Vec3Track;

    fn track(gseq: u16, interp: u16, keys: &[(u32, [f32; 3])]) -> M2Vec3Track {
        M2Vec3Track {
            interp,
            gseq,
            ranges: Vec::new(),
            keys: keys.to_vec(),
        }
    }

    fn bake(t: &M2Vec3Track, gseq: &[u32], seq0: Option<(u32, u32)>) -> Option<UvAnim> {
        // Slot 0 as the UV lane meets it: the loop a placed doodad's one-time arm plays.
        let slot = seq0.map(|band| SeqSlot {
            index: 0,
            band,
            looping: true,
        });
        super::super::key_anim::bake_track(t, gseq, slot, |v| [v[0], v[1]], is_zero, is_zero)
    }

    /// A keyless or never-moving track vanishes; a constant non-zero offset is kept period-0 (a
    /// static UV shift the vertex data doesn't carry).
    #[test]
    fn bake_drops_the_identity_and_keeps_a_static_shift() {
        assert_eq!(bake(&track(0xffff, 1, &[]), &[], None), None);
        assert_eq!(
            bake(
                &track(0xffff, 1, &[(0, [0.0; 3]), (500, [0.0; 3])]),
                &[],
                None
            ),
            None
        );
        let shift = bake(&track(0xffff, 1, &[(0, [0.25, 0.5, 9.0])]), &[], None).unwrap();
        assert_eq!(shift.period, 0.0);
        assert_eq!(shift.sample(77.0), [0.25, 0.5]); // z discarded
    }

    /// The fountain shape (gseq clock): a 2-key linear ramp wrapping the global-sequence duration —
    /// the offset accumulates to a full texture period each loop.
    #[test]
    fn gseq_scroll_wraps_and_lerps() {
        let a = bake(
            &track(3, 1, &[(0, [0.0; 3]), (1333, [0.0, -1.0, 0.0])]),
            &[1, 1, 1, 1333],
            None,
        )
        .unwrap();
        assert!((a.period - 1.333).abs() < 1e-6);
        let mid = a.sample(1.333 / 2.0);
        assert!((mid[1] + 0.5).abs() < 1e-3, "half-loop offset ≈ -0.5 V");
        assert!((a.sample(1.333 + 0.1)[1] - a.sample(0.1)[1]).abs() < 1e-4);
    }

    /// The real Elwynn waterfall, straight off the client data: both batches carry seq-0-band
    /// translation loops (V-scroll, −1.0 and −2.0 per loop — the 2× foam layer), wired batch →
    /// texAnimLookup → transform. Guards the whole chain: the vec3-track parse (benilla-m2), the
    /// 0xac lookup slot, the combo two-hop, and the band-clock bake. Skips without client data.
    #[test]
    fn elwynn_waterfall_bakes_two_v_scroll_loops() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file(
                "World\\Azeroth\\Elwynn\\PassiveDoodads\\Waterfall\\ElwynnTallWaterfall01.m2",
            )
            .expect("read ElwynnTallWaterfall01.m2");
        let subs = super::super::parse_m2_render_submeshes(&bytes, "", &[]).expect("parse");
        let anims: Vec<&UvAnim> = subs.iter().filter_map(|s| s.uv_anim.as_ref()).collect();
        assert_eq!(anims.len(), 2, "both waterfall batches carry UV loops");
        for a in &anims {
            assert!(a.period > 1.0, "a real loop, not a constant");
            let (v0, v1) = (a.sample(0.0), a.sample(a.period * 0.5));
            assert!(
                (v1[1] - v0[1]).abs() > 0.01,
                "the V offset moves over the loop ({v0:?} vs {v1:?})"
            );
        }
        // The two layers scroll at different rates (the −1.0 vs −2.0 authoring).
        let (e0, e1) = (
            anims[0].keys.last().unwrap().1[1],
            anims[1].keys.last().unwrap().1[1],
        );
        assert!(
            (e0 - e1).abs() > 0.5,
            "distinct layer rates survive the bake ({e0} vs {e1})"
        );
    }
}
