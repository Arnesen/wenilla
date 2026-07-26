//! Targeting — click a unit to select it, and mark it with a ground selection ring.
//!
//! The first real player-facing consumer of the [`crate::interact`] picking foundation (whose header
//! long anticipated "mouseover-targeting"). This module owns the *selection state and input*; the
//! ring itself — the projected decal, its colour selector, and the reaction decode — lives in
//! [`ring`]. The pieces here:
//! - **Selection state** ([`Selection`]) — the entity + guid of our current target, tracked *locally*
//!   and set the instant we click (the faithful client behaviour: the ring appears immediately, no
//!   server round-trip). Sending [`ClientCommand::SetSelection`] just informs the server, which records
//!   it in our `UNIT_FIELD_TARGET` and relays it to observers.
//! - **Hover** ([`hover::update_hover`]) — each frame, the unit under the cursor, found the way the real
//!   client finds it (wow-re pick-volume RE `bd630be`, §5-verified: `CGWorldFrame` pick `0x481190` →
//!   `0x7089c0`): a **broad phase** ray-vs-sphere on the *current animation sequence's* bounds
//!   (world-placed + scaled, no pad), then a **narrow phase** ray-vs-triangle against the unit's
//!   **posed render mesh** — the drawn vertices skinned through the live joint pose. Clickable =
//!   the visible creature's silhouette at its pose. (The M2 bounding-triangle block is *not* the
//!   unit pick volume — that's the static-doodad path; an earlier gloss saying otherwise was
//!   refuted by asset data.) Hull-less/model-less units (cube fallbacks) keep the old render-mesh
//!   AABB test. Stored in [`Hovered`] — the state a mouseover highlight will read.
//! - **Click to select** ([`select_on_click`]) — a [`WorldClick`] (the clean left-click the player
//!   controller emits; a left *drag* orbits the camera instead) selects whatever is [`Hovered`].
//!   Clicking empty ground / a non-unit, or pressing **Esc**, clears the target; the target's death
//!   clears it too (in [`ring::update_ring`], where death is already read).
//!
//! The mouseover/target "light-up" has two consumers: the per-model **emissive lift**
//! ([`highlight`], the byte-verified additive term) and the V-plate's reaction-tinted **glow**
//! (`crate::vplates`, drawn later off [`Hovered`] + [`Selection`]). The plate rect is itself part
//! of the pick: hovering a plate makes its unit the mouseover *before* any world ray-test
//! ([`hover::update_hover`] consults `crate::vplates::PlateRects` first — the reference's
//! UI-blocks-world frame walk), so plates brighten the model and take clicks like the body does.
//! The real pick's **world-occlusion clamp** is modelled ([`PickOcclusion`]): the reference's scene
//! trace runs `CWorld::Intersect` first and the object pick only wins nearer than the world hit —
//! a unit or GameObject behind a wall is not hoverable. Not yet modelled: the header-sphere
//! fallback for a sequence with unauthored bounds (we pass the broad phase through instead —
//! strictly more permissive, never less).

use bevy::prelude::*;

use benilla_ui::script::UiScript;

use crate::assets::AssetSet;
use crate::creature_anim::Engaged;
use crate::interact::{InspectMode, WorldClick, WorldRightClick};
use crate::net::{ClientCommand, Guid, NetCommands, ObjectStore, SelfPlayer};
use crate::schedule::WorldStage;

mod click;
mod cursor_mode;
mod flash;
mod highlight;
mod hover;
mod ring;
mod scan;

pub(crate) use cursor_mode::{CursorKind, WorldCursor, SERVICE_RANGE_SQ};
// This frame's combat-flash verdict — read by the ring's material pick (in `ring`) and by the
// nameplate colour gate (`crate::nameplates`), the flash's only two consumers (byte-verified).
pub(crate) use flash::CombatFlash;
// The action layer's "attack pressed with no target" request — the nearest-enemy auto-acquire
// (`scan`) answers it with the same core TAB uses.
pub(crate) use scan::{can_attack, AttackNearestRequest};
// The byte-verified reaction decode + its faction catalog, reused by the unit-frame feed to tint the
// target's name plate (`TargetFrame_CheckFaction`) the same way the selection ring colours itself.
// `duel_rung` is the diagnostic face of the same walk, for `/reaction` (decision 0637).
pub(crate) use ring::{duel_rung, ring_reaction, ring_variant, Factions, RingVariant};

/// Our current target: the selected entity and its server guid, or `None`. Set the instant we click a
/// unit (client-authoritative for the ring — the real client doesn't wait for the server), cleared on
/// deselect or when the target streams out. The guid is what we send in `CMSG_SET_SELECTION` and, later,
/// what unit frames / other-unit rings key off.
#[derive(Resource, Default)]
pub(crate) struct Selection {
    pub(crate) target: Option<Entity>,
    pub(crate) guid: Option<u64>,
}

/// The unit under the cursor this frame (the net entity + its guid), or `None`. Recomputed every frame
/// by [`hover::update_hover`] (plate rects first, then the posed-mesh pick); a click selects it, and
/// the mouseover highlight — the model emissive + the plate glow — reads it.
#[derive(Resource, Default)]
pub(crate) struct Hovered {
    pub(crate) target: Option<Entity>,
    pub(crate) guid: Option<u64>,
    /// World-space ray distance to the picked mesh (a plate-rect hover is `0.0` — UI wins any tie).
    /// Meaningless when `target` is `None`. Lets the cursor/interact pick the **nearer** of a hovered
    /// unit vs. a hovered GameObject ([`go_is_nearest`]), the reference's single nearest-CGObject pick.
    pub(crate) distance: f32,
}

/// The **GameObject** under the cursor this frame — the interaction arc's separate hover authority
/// (decision 0236), kept out of the byte-verified unit [`Hovered`] path because a GameObject is
/// *usable but never selected*. Filled by [`hover::update_hovered_object`] (a GO-only mesh-ray pick);
/// read by the cursor classifier (the Interact gear) and the right-click USE action. Both compose it
/// with the unit hover by [`go_is_nearest`] — a GO acts only when it is the nearer object.
#[derive(Resource, Default)]
pub(crate) struct HoveredObject {
    pub(crate) target: Option<Entity>,
    pub(crate) guid: Option<u64>,
    /// World-space ray distance to the GO mesh, compared against [`Hovered::distance`].
    pub(crate) distance: f32,
}

/// This frame's **world-occlusion distance** for the mouse pick: the distance along the cursor ray
/// to the nearest [`crate::collision::PickOccluder`] hit (terrain, the WMO walk-bake ≈ `0x84`
/// reject-mask faces, static doodad hulls), or `f32::INFINITY` with no hit. The reference's scene
/// trace (`0x480df0`, wow-re selection-circle PART 3, §5-cross-checked) traces objects unbounded
/// and discards the object hit iff the world hit is *strictly* nearer — a unit or GameObject
/// behind a wall is not hoverable. Written by [`hover::update_pick_occlusion`] at the head of the
/// target chain; both picks post-compare their final hit against it.
#[derive(Resource)]
pub(crate) struct PickOcclusion {
    pub(crate) distance: f32,
}

impl Default for PickOcclusion {
    fn default() -> Self {
        Self {
            distance: f32::INFINITY,
        }
    }
}

/// Which of the two hovered CGObjects — the unit [`Hovered`] or the [`HoveredObject`] — is the
/// nearer thing under the cursor, and so the one a click acts on. The reference makes one pick over
/// all CGObjects and switches on type; benilla runs the two picks separately, then chooses the
/// nearer here. A GameObject wins only when it is strictly closer than a hovered unit (so a mailbox
/// in front of a distant NPC takes the click; an NPC in front of a chest keeps it).
pub(crate) fn go_is_nearest(unit: &Hovered, go: &HoveredObject) -> bool {
    match (unit.target, go.target) {
        (Some(_), Some(_)) => go.distance < unit.distance,
        (None, Some(_)) => true,
        _ => false,
    }
}

/// A unit's model-local selection radius — the **Stand-animation footprint** `sqrt(0.5 · sqrt(dx² + dy²))`
/// (dx/dy = the Stand sequence box's horizontal extents), the exact model-local input the real client's
/// living-unit ring uses, scaled by `OBJECT_FIELD_SCALE_X` (wow-re selection-ring RE, `0x608e00`/`0x60aee0`,
/// byte-verified + emulated to the reference pixels). Stamped at attach ([`crate::entities`]); the ring's
/// world radius is this × the unit's transform scale (which is SCALE_X). Absent on model-less (cube) units,
/// which fall back to the ring's fallback radius.
#[derive(Component, Clone, Copy)]
pub(crate) struct SelectionRadius(pub(crate) f32);

/// The Update set the whole targeting chain runs in. Consumers of the frame's verdicts —
/// [`Selection`], [`Hovered`], [`CombatFlash`] (the nameplate driver) — order after it; without
/// the constraint their order against the chain is ambiguous and they can read last frame's
/// verdict (one-frame flicker on select/flash edges).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TargetUpdate;

/// Targeting: click-to-select + the ground selection ring.
pub(crate) struct TargetPlugin;

impl Plugin for TargetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Selection>()
            .init_resource::<Hovered>()
            .init_resource::<HoveredObject>()
            .init_resource::<PickOcclusion>()
            .init_resource::<WorldCursor>()
            .init_resource::<CombatFlash>()
            .init_resource::<scan::TabHistory>()
            .add_message::<AttackNearestRequest>()
            .add_systems(
                Startup,
                (
                    ring::setup_ring,
                    (ring::load_factions, scan::load_creature_types).after(AssetSet::Open),
                ),
            )
            // All after input (so this frame's `WorldClick` from `player::control` is available), in
            // order: refresh the hovered unit, resolve its context cursor, route the right-click
            // cursor-payload leg (decisions 0571 + 0574), let a click select it, handle Esc,
            // drain the UI's `TargetUnit` requests (the PlayerFrame self-target), then the
            // selection writers that don't come from the mouse (the auto-acquires + TAB), then
            // the flash verdict and the ring off the frame's final selection.
            .add_systems(
                Update,
                (
                    hover::update_pick_occlusion,
                    hover::update_hover,
                    hover::update_hovered_object,
                    cursor_mode::classify_cursor,
                    click::world_right_click_payload,
                    click::select_on_click,
                    click::act_on_right_click,
                    click::clear_target_requests,
                    click::target_unit_requests,
                    scan::auto_acquire_attacker,
                    scan::tab_target,
                    scan::acquire_and_attack,
                    flash::drive_flash,
                    ring::update_ring,
                )
                    .chain()
                    .in_set(TargetUpdate)
                    .after(WorldStage::Input),
            )
            // The model brighten writes MeshTag bit 31 — PostUpdate, after every Update payload
            // writer (fade/interior/…), so their whole-tag overwrites can't strand the bit.
            .add_systems(PostUpdate, highlight::apply_highlight);
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gameobject_wins_the_click_only_when_the_nearer_object() {
        let hov = |t: bool, d: f32| Hovered {
            target: t.then_some(Entity::PLACEHOLDER),
            guid: None,
            distance: d,
        };
        let obj = |t: bool, d: f32| HoveredObject {
            target: t.then_some(Entity::PLACEHOLDER),
            guid: None,
            distance: d,
        };
        // No GameObject hovered → the unit path always keeps the click.
        assert!(!go_is_nearest(&hov(true, 5.0), &obj(false, 0.0)));
        assert!(!go_is_nearest(&hov(false, 0.0), &obj(false, 0.0)));
        // A GameObject with no competing unit → it takes the click.
        assert!(go_is_nearest(&hov(false, 0.0), &obj(true, 42.0)));
        // Both hovered → only a *strictly nearer* GameObject wins (a mailbox in front of a distant
        // NPC); a farther or tied GameObject yields to the unit (a plate hover sits at distance 0.0).
        assert!(go_is_nearest(&hov(true, 10.0), &obj(true, 4.0)));
        assert!(!go_is_nearest(&hov(true, 4.0), &obj(true, 10.0)));
        assert!(!go_is_nearest(&hov(true, 5.0), &obj(true, 5.0)));
    }
}
