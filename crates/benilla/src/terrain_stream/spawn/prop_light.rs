//! The WMO prop-light machinery — a placed prop's resolved lighting and the interior SH fold.
//!
//! Split out of `terrain_stream.rs` (0830 named the carve; 0832 executed it): this is spawn-side
//! state and math — consumed by the placement assembler, the WMO-gameobject prop lane
//! (`crate::entities`' `wmo_props`) and the GameObject footprint classifier (`crate::interior`) —
//! and none of it touches the streamer's residency logic. Paths outside the streamer are stable:
//! `terrain_stream` re-exports [`fold_interior_probe`] and [`PropLobeLight`].

use std::sync::Arc;

use benilla_assets::M2Model;
use bevy::prelude::*;

/// One WMO doodad prop instance: its M2 handle and the **world** transform (the WMO instance
/// transform composed with the doodad's WMO-local transform), spawned once the M2 asset loads.
pub(crate) struct WmoDoodadInst {
    pub(crate) handle: Handle<M2Model>,
    pub(crate) transform: Transform,
    /// Every WMO group whose MODR names this prop — the portal-cull key, so a prop is hidden with
    /// the rooms it furnishes and drawn while any one of them is visible. Empty for a MODD no group
    /// references (the reference never instantiates one at all; we still show it, uncullable,
    /// rather than change what draws today).
    pub(crate) groups: Arc<[u16]>,
    /// The prop's lighting ([`PropLight`], from `WmoModel::doodad_base` composed with this
    /// placement): exterior sky-lit, or the interior MODD-colour base + its owning group's MOLR
    /// lights placed in world space — folded into the prop's SH probe once its M2 loads (the fold
    /// reference point needs the M2 bounds).
    pub(crate) light: PropLight,
    pub(crate) spawned: bool,
}

/// A WMO prop's placement-resolved lighting: the asset-level [`DoodadBase`](benilla_assets::DoodadBase)
/// with the owning group's MOLR lights already transformed to WORLD (Bevy) space — so the
/// spawn-time SH fold needs only the loaded M2's bounds (its reference point) and nothing from the
/// WMO asset.
pub(crate) enum PropLight {
    Exterior,
    Interior {
        /// `cap96(MODD.colour)` — the ambient word (0–1 RGB).
        ambient: [f32; 3],
        /// `floor112(MODD.colour)` — the diffuse word, committed on the fixed interior axis.
        diffuse: [f32; 3],
        /// The owning group's MOLR omni lights: world (Bevy) position, colour × intensity, and the
        /// disk `attenStart`/`attenEnd` window (the fold's range gate).
        lights: Vec<PropLobeLight>,
    },
}

impl PropLight {
    /// The prop's lighting lane, as the mouseover inspector says it. A WMO prop that looks wrong is
    /// almost always on the wrong *lane* or carrying an unbaked base — and neither is visible from
    /// the model path alone, which is why Booty Bay's black entrance arch read as a texture bug for
    /// as long as it did (decision 0969). `sky-lit` is the exterior lane; the interior lane prints
    /// the MODD-colour words it actually commits, so a base of `#000000` names itself on hover.
    pub(crate) fn inspector_label(&self) -> String {
        let hex = |c: &[f32; 3]| {
            let b = c.map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8);
            format!("#{:02x}{:02x}{:02x}", b[0], b[1], b[2])
        };
        match self {
            PropLight::Exterior => "sky-lit".into(),
            PropLight::Interior {
                ambient,
                diffuse,
                lights,
            } => format!(
                "interior amb {} dif {} · {} MOLR",
                hex(ambient),
                hex(diffuse),
                lights.len()
            ),
        }
    }
}

/// One MOLR-referenced light as the interior fold consumes it (world Bevy space, colour
/// pre-multiplied by the authored intensity). Shared by the MODD prop spawn fold and the
/// GameObject footprint lane ([`crate::interior`] via [`crate::wmo_portal`]'s verdict).
pub(crate) struct PropLobeLight {
    pub(crate) pos: Vec3,
    pub(crate) color_i: [f32; 3],
    pub(crate) atten_start: f32,
    pub(crate) atten_end: f32,
}

/// Fold one interior committed light into its 7-row SH probe: the ambient word + the diffuse word
/// as a directional on the FIXED interior axis + each MOLR lobe windowed by its disk
/// attenStart/attenEnd from `ref_point` (the byte-verified `0x69e1c0` falloff: d ≤ start → 1;
/// d ≥ end → excluded; else linear). One definition for both interior lanes — the MODD prop
/// (spawn-time, MODD-colour words) and the GameObject footprint (classify-time, MOCV-derived
/// words); the SH closed form itself is [`prop_probe_coeffs`](crate::lighting::prop_probe_coeffs).
pub(crate) fn fold_interior_probe(
    ambient: [f32; 3],
    diffuse: [f32; 3],
    ref_point: Vec3,
    lights: &[PropLobeLight],
) -> [bevy::math::Vec4; 7] {
    // Toward-light, Bevy space: wow (0.30822, 0.30822, 0.9) → (−y, z, −x).
    let mut lobes: Vec<(Vec3, [f32; 3])> = vec![(Vec3::new(-0.30822, 0.9, -0.30822), diffuse)];
    for l in lights {
        let dv = l.pos - ref_point;
        let dist = dv.length();
        let gain = if dist <= l.atten_start {
            1.0
        } else if dist >= l.atten_end || l.atten_end <= l.atten_start {
            0.0
        } else {
            1.0 - (dist - l.atten_start) / (l.atten_end - l.atten_start)
        };
        if gain > 0.0 {
            lobes.push((dv / dist.max(1e-4), l.color_i.map(|c| c * gain)));
        }
    }
    crate::lighting::prop_probe_coeffs(ambient, &lobes)
}
