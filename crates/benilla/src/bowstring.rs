//! The engine-drawn **bowstring** (wow-re `nocked-ammo-cancel.md` §G2, byte-verified): bow M2s
//! carry NO string geometry — the real client registers a bow-only per-frame draw callback
//! (`0x611ff0`) that spans the bow's `$WTT`/`$WTB` limb-tip event markers with a **2-segment
//! line list**, middle vertex at the character's HandArrow attach while the nock latch
//! (`[+0xd58] & 0x4000`) holds, else the tip midpoint — so the string tracks the draw hand while
//! nocked and relaxes to a straight chord at rest.
//!
//! Benilla's transcription draws the two segments through Bevy's gizmo lines — the same
//! immediate-mode screen-space-width primitive class as the client's GX lines. Named
//! deviations (decision record): the string color/width are INFERRED (the callback emits a
//! packed vertex color the round didn't decode — a dark cord is used); the bow prop is not yet
//! animated (BowPull/BowRelease bend the limbs and carry the tips with them in the ref — our
//! anchors ride the static prop frame, so the limbs stay straight while the middle draws).

use bevy::prelude::*;

use crate::creature_anim::NockLatch;
use crate::entities::BoneAttach;

/// The HandArrow attach id (35 — wow-re §E2/G2): the string's middle control point while nocked,
/// the same point the nocked-arrow model rides.
const HAND_ARROW: u16 = 0x23;

/// Marks a spawned **bow prop root** (the held-item child of the hand joint) whose model authors
/// the `$WTT`/`$WTB` string anchors. Inserted by the held-item attach; despawned with the prop
/// (a sheath change / weapon swap tears the prop down, which is the client's clear too).
#[derive(Component)]
pub(crate) struct Bowstring {
    /// The unit wearing the bow — the HandArrow middle vertex and nock latch live on it.
    pub(crate) owner: Entity,
    /// The `$WTT` top / `$WTB` bottom anchors, model-local Bevy space (the prop root's frame).
    pub(crate) top: Vec3,
    pub(crate) bottom: Vec3,
}

/// Draw every visible bow's string (per frame, post-propagation so the prop/joint frames are
/// this frame's). Two segments: tip → middle → tip.
fn draw_bowstrings(
    bows: Query<(&Bowstring, &GlobalTransform, &InheritedVisibility)>,
    owners: Query<(&BoneAttach, Has<NockLatch>)>,
    joints: Query<&GlobalTransform>,
    mut gizmos: Gizmos,
) {
    // A dark waxed-cord tone; the ref's packed vertex color is not decoded (INFERRED, §G2).
    const STRING_COLOR: Color = Color::srgb(0.12, 0.10, 0.08);
    for (bs, prop, vis) in &bows {
        if !vis.get() {
            continue;
        }
        let top = prop.transform_point(bs.top);
        let bottom = prop.transform_point(bs.bottom);
        // The middle control point: the owner's HandArrow attach world position while the nock
        // latch holds (the drawn string follows the hand), else the relaxed straight chord.
        let middle = owners
            .get(bs.owner)
            .ok()
            .filter(|(_, latched)| *latched)
            .and_then(|(bones, _)| {
                let &(bone, offset) = bones.points.get(&HAND_ARROW)?;
                let joint = bones.anchor(bone)?;
                Some(joints.get(joint).ok()?.transform_point(offset))
            })
            .unwrap_or_else(|| (top + bottom) / 2.0);
        gizmos.line(top, middle, STRING_COLOR);
        gizmos.line(middle, bottom, STRING_COLOR);
    }
}

/// Registers the string drawer after the palette pass (the middle vertex reads the HandArrow
/// joint's frame — its translation is palette-invariant, but same-frame ordering keeps every
/// consumer of joint frames on one side of the rewrite).
pub(crate) struct BowstringPlugin;

impl Plugin for BowstringPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            draw_bowstrings.in_set(crate::billboard::BillboardPlace),
        );
    }
}
