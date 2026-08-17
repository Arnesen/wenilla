//! An M2's authored bounds ([`M2Bounds`]) — the header sphere/box, the vertex-derived radius our
//! renderer computes, and the selection-ring **footprint** read from the model's Stand-animation
//! bounding box (the exact model-local input the real client's living-unit selection ring uses).
//! No render-batch concern here — just the bounds/measurement reads [`load_m2_bounds`]/
//! [`parse_m2_bounds`] expose to placement + the selection ring.

use std::io::Cursor;

use anyhow::{Context, Result};
use benilla_bytes::ByteExt;
use benilla_m2::parse_m2;

use crate::Chain;

use super::model_path;

/// An M2's authored bounds (read straight from the model header) alongside the vertex-derived radius
/// our renderer currently computes — so the distance-fade size bucket can be checked against the value
/// the reference's placement builder actually uses (`FUN_00694bc0` → `rec+0x68`). `sphere_radius` and
/// `bbox_*` are in model-local yards (multiply by the placement scale for the world radius).
#[derive(Clone, Copy, Debug)]
pub struct M2Bounds {
    /// `M2Header.bounding_sphere_radius` — the authored bounding-sphere radius (the reference's source).
    pub sphere_radius: f32,
    /// `M2Header.bounding_box_min/max` — the authored AABB; its centre is the sphere centre.
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    /// Max vertex distance from the model **origin** — what `build_model` currently uses as the radius.
    pub vert_max_from_origin: f32,
    /// The selection-ring **footprint** (model-local, pre-scale): `sqrt(0.5 · sqrt(dx² + dy²))`, where
    /// `dx,dy` are the horizontal (X,Y) extents of the unit's **Stand** animation bounding box (the M2
    /// sequence CAaBox). This is the exact model-local input the real client's living-unit selection ring
    /// uses — byte-verified + Unicorn-emulated to the reference pixels (wow-re selection-ring RE,
    /// `0x608e00`/`0x60aee0`): the ring's world radius = this × `OBJECT_FIELD_SCALE_X`. The nested sqrt is
    /// a range-compressor (which is why the render-sphere never fit). Falls back to the header render-box
    /// XY extents for a model with no animation sequences.
    pub ring_footprint: f32,
    /// Model-space **Z** of attachment id 17 — the reference's follow-camera pivot height (wow-re
    /// `follow-camera`: `feet + (attach17.z + 0.0972)·scale`). `None` for a model with no slot-17
    /// attachment; the camera then falls back to a fraction of the box. See [`M2Bounds::pivot_z`].
    pub pivot_z: Option<f32>,
    /// The **Stand** animation box's vertical extent, `max.z − min.z` (model-local, pre-scale) — the
    /// chat bubble's anchor height above the unit's feet (× model scale, + 0.7 yd).
    ///
    /// The same `0x711a20` query the selection ring above is sized from, taking the box's Z instead
    /// of its XY: wow-re's chat-bubble anchor cross-check (2026-08-17) followed `0x4b0e38 call
    /// 0x711a20` into the model layer and found it reading the **MD20 header image** — file bytes,
    /// no bone matrix anywhere in the call tree — and returning `out+0x20 − out+0x14`, which is that
    /// CAaBox's Z extent. Benilla anchored the bubble on the posed PlayerName attachment instead,
    /// on a recorded INFERRED claim that `0x608640` and `0x711a20` were "both the head-region
    /// attachment height"; that equivalence is **REFUTED** — they differ precisely on
    /// animated-vs-static, `0x608640` reading the live posed palette (it bobs) and this reading a
    /// file constant. The overhead *name* keeps `0x608640`; only the bubble takes this. See 1406.
    ///
    /// Falls back to the header render-box Z extent for a model with no sequences, like the ring.
    pub stand_box_z: f32,
}

/// Read an M2's authored bounds + the vertex-derived radius (see [`M2Bounds`]). Uses the same path
/// normalisation as [`super::load_m2_mesh`] so `.mdx`/`.mdl` doodad paths resolve identically.
pub fn load_m2_bounds(chain: &mut Chain, raw_path: &str) -> Result<M2Bounds> {
    let path = model_path(raw_path);
    let bytes = chain
        .read_file(&path)
        .with_context(|| format!("reading M2 {path}"))?;
    parse_m2_bounds(&bytes).with_context(|| format!("parsing M2 bounds {path}"))
}

/// Read the horizontal (X,Y) extents of the **Stand** animation's bounding box from a raw M2 — the input
/// the real client's living-unit selection ring is sized from (wow-re selection-ring RE, `0x60aee0`).
/// The animation `M2Sequence` array is at MD20 `0x1c`(count)/`0x20`(offset), stride `0x44`, with the
/// sequence's `CAaBox` at record `+0x24` (min C3 `+0x24`, max C3 `+0x30`). Stand is animation **id 0**,
/// whose *sequence index* is `animationLookup[0]` (array at `0x24`/`0x28`, `u16` each) — NOT necessarily
/// record 0 (a chicken's record 0 is a flap, its Stand sits at index 2). Returns `(dx, dy, dz)`, or
/// `None` when the model has no sequences / lookup or the indices are out of range (caller falls back
/// to the header box). VERIFIED: reproduces the reference ring radii to ~1 mm.
///
/// The Z extent joins the pair the ring needs because the chat bubble's anchor is the **same box**
/// read on the other axis (1406) — one parse, one Stand-sequence resolution, two consumers, so the
/// ring and the bubble can never drift onto different animations.
fn stand_box(bytes: &[u8]) -> Option<(f32, f32, f32)> {
    let anim_count = bytes.u32_at(0x1c)? as usize;
    let anim_ofs = bytes.u32_at(0x20)? as usize;
    let lookup_count = bytes.u32_at(0x24)? as usize;
    let lookup_ofs = bytes.u32_at(0x28)? as usize;
    if anim_count == 0 {
        return None;
    }
    // Stand (anim id 0) → its sequence index via animationLookup[0]; fall back to record 0 if the lookup
    // is absent, out of bounds, or points nowhere (0xffff / out of range).
    let idx = if lookup_count > 0 {
        match bytes.u16_at(lookup_ofs) {
            Some(i) if (i as usize) != 0xffff && (i as usize) < anim_count => i as usize,
            _ => 0,
        }
    } else {
        0
    };
    let rec = anim_ofs.checked_add(idx * 0x44)?;
    // CAaBox @ rec+0x24: min C3 (+0x24), max C3 (+0x30). Horizontal (X,Y) extents; squared in the ring
    // formula so an unsorted box is harmless.
    let dx = bytes.f32_at(rec + 0x30)? - bytes.f32_at(rec + 0x24)?;
    let dy = bytes.f32_at(rec + 0x34)? - bytes.f32_at(rec + 0x28)?;
    let dz = bytes.f32_at(rec + 0x38)? - bytes.f32_at(rec + 0x2c)?;
    Some((dx, dy, dz))
}

/// Read an **in-memory** M2's authored bounds + vertex-derived radius (see [`M2Bounds`]). The bytes-in
/// entry point for the Bevy `AssetLoader`; [`load_m2_bounds`] is the chain-reading wrapper.
pub fn parse_m2_bounds(bytes: &[u8]) -> Result<M2Bounds> {
    let format =
        parse_m2(&mut Cursor::new(bytes)).map_err(|e| anyhow::anyhow!("parsing M2: {e}"))?;
    let model = format.model();
    let h = &model.header;
    let vert_max_from_origin = model
        .vertices
        .iter()
        .map(|v| {
            let p = v.position;
            (p.x * p.x + p.y * p.y + p.z * p.z).sqrt()
        })
        .fold(0.0_f32, f32::max);
    // Selection-ring footprint: `sqrt(0.5 · sqrt(dx² + dy²))` of the Stand animation box's horizontal
    // extents (the real client's living-unit ring input), falling back to the header render-box XY for a
    // model with no animation sequences (the client's static-model path).
    let (rx, ry, rz) = stand_box(bytes).unwrap_or((
        h.bounding_box_max[0] - h.bounding_box_min[0],
        h.bounding_box_max[1] - h.bounding_box_min[1],
        h.bounding_box_max[2] - h.bounding_box_min[2],
    ));
    let ring_footprint = (0.5 * (rx * rx + ry * ry).sqrt()).sqrt();
    Ok(M2Bounds {
        sphere_radius: h.bounding_sphere_radius,
        bbox_min: h.bounding_box_min,
        bbox_max: h.bounding_box_max,
        vert_max_from_origin,
        ring_footprint,
        pivot_z: model.pivot_attach_z,
        stand_box_z: rz.max(0.0),
    })
}
