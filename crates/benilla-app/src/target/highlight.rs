//! The mouseover / target **model brighten** — the real client's per-model highlight emissive
//! (wow-re `object-layer/scratch/selection-circle.md` PART 2, §5 cross-checked).
//!
//! The reference pushes it on hover/target *change*, never a per-frame compare: the mouseover
//! publisher and the target setter call `SetHighlight 0x614550` / `ClearHighlight 0x6144f0` with a
//! per-object reason bitmask (bit 0 = target, bit 1 = mouseover — hover + target **stack**, and the
//! glow drops only when the last reason clears). `SetHighlight` writes the config RGB — shipped
//! default `0xff404040` ⇒ **+64/255 per channel** — into the model, and the animate kernel adds it
//! to the material emissive (`glMaterialfv(GL_EMISSION)`): a flat additive lift inside the GL
//! lighting sum, pre-texture-modulate, riding every lit material of the model *and* its attachments.
//!
//! benilla carries the flag in **bit 31 of the per-instance `MeshTag`** (the convention home is
//! `benilla_world::mesh_tag`); `wow_model.wgsl` adds the 64/255 lift to its lighting factor when set. This
//! system is the bit's only writer: each frame (PostUpdate — after every Update payload writer, so
//! their whole-`u32` overwrites can't strand the bit) it ORs the flag onto every part of the
//! hovered + selected roots and clears it on roots that left the set. The reason bitmask collapses
//! to set membership: an entity is lit while it is hovered *or* targeted — same stacking result.
//! Scope tracks whatever hover/selection can resolve (the reference brightens any hoverable object
//! with a model — units today; GameObjects when they become hoverable).

use bevy::mesh::MeshTag;
use bevy::prelude::*;

use benilla_world::mesh_tag::HIGHLIGHT_BIT;

use super::{go_is_nearest, Hovered, HoveredObject, Selection};

/// OR/clear [`HIGHLIGHT_BIT`] on the hovered + targeted roots' part tags. `was_lit` is last frame's
/// root set, so a root that loses both reasons gets its bit cleared exactly once. A hovered
/// GameObject brightens exactly like a unit (the reference's reason bit 1 = mouseover covers any
/// hoverable CGObject — the signpost glow, director-matched 2026-07-13), gated by the same
/// nearer-pick the click router uses so only the object a click would act on lights.
pub(super) fn apply_highlight(
    hovered: Res<Hovered>,
    hovered_go: Res<HoveredObject>,
    selection: Res<Selection>,
    children: Query<&Children>,
    mut tags: Query<&mut MeshTag>,
    mut was_lit: Local<Vec<Entity>>,
) {
    let go = hovered_go
        .target
        .filter(|_| hovered.target.is_none() || go_is_nearest(&hovered, &hovered_go));
    let unit_hover = hovered.target.filter(|_| go.is_none());
    let mut want: Vec<Entity> = Vec::new();
    for root in [unit_hover, go, selection.target].into_iter().flatten() {
        if !want.contains(&root) {
            want.push(root);
        }
    }
    for &root in was_lit.iter() {
        if !want.contains(&root) {
            set_bit(root, false, &children, &mut tags);
        }
    }
    // Re-asserted every frame, not only on change: the Update payload writers (fade/interior/…)
    // overwrite the whole tag without the bit whenever they run.
    for &root in &want {
        set_bit(root, true, &children, &mut tags);
    }
    *was_lit = want;
}

/// Set/clear the highlight bit on `root` and every descendant carrying a `MeshTag` (the model's
/// parts — and its attachments, which the reference brightens with the body). A despawned root
/// simply yields no descendants.
fn set_bit(root: Entity, on: bool, children: &Query<&Children>, tags: &mut Query<&mut MeshTag>) {
    for e in std::iter::once(root).chain(children.iter_descendants(root)) {
        if let Ok(mut tag) = tags.get_mut(e) {
            let bits = if on {
                tag.0 | HIGHLIGHT_BIT
            } else {
                tag.0 & !HIGHLIGHT_BIT
            };
            if tag.0 != bits {
                tag.0 = bits;
            }
        }
    }
}
