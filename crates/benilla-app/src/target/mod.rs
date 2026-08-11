//! Targeting — click a unit to select it, and mark it with a ground selection ring.
//!
//! The first real player-facing consumer of the [`benilla_world::interact`] picking foundation (whose header
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

use crate::creature_anim::Engaged;
use crate::net::{ClientCommand, Guid, NetCommands, ObjectStore, SelfPlayer};
use crate::ui_script::InspectMode;
use benilla_assets::AssetSet;
use benilla_world::interact::{WorldClick, WorldRightClick};
use benilla_world::schedule::WorldStage;

mod by_name;
mod click;
// `pub(crate)` for the hover inspector's interact-gate line (`interact/inspect.rs`): the overlay
// states the very predicate the cursor runs ([`cursor_mode::go_highlightable`]) rather than a
// re-derivation of it, so the readout can never drift from the behaviour it is reporting on.
pub(crate) mod cursor_mode;
mod flash;
mod highlight;
mod hover;
pub(crate) mod lock;
mod relations;
mod reticle;
// `pub(crate)` for the same reason as `cursor_mode`: the faction catalog is one of
// [`cursor_mode::go_highlightable`]'s three inputs, so the inspector needs it to run the real gate.
pub(crate) mod ring;
mod scan;

pub(crate) use cursor_mode::{CursorKind, WorldCursor, GO_TYPE_GENERIC, SERVICE_RANGE_SQ};
/// `GO_FLAG_LOCKED` — the wire bit the lock chain and the GO tooltip's "Locked" line both read.
pub(crate) use lock::GO_FLAG_LOCKED;
// This frame's combat-flash verdict — read by the ring's material pick (in `ring`) and by the
// nameplate colour gate (`crate::nameplates`), the flash's only two consumers (byte-verified).
pub(crate) use flash::CombatFlash;
// The action layer's "attack pressed with no target" request — the nearest-enemy auto-acquire
// (`scan`) answers it with the same core TAB uses. `attack_order_target` + `EnemyScan` are that
// same core called *synchronously*, by the pet bar's ATTACK arm: the pet's order has to leave in
// the frame it was pressed carrying the acquired guid, so it cannot go round through a request.
pub(crate) use relations::{can_assist, can_attack};
pub(crate) use scan::{attack_order_target, AttackNearestRequest, EnemyScan};
// The chat layer's by-name selection asks (`/target`, `/assist` — decision 0886), answered by the
// shared resolver the reference parameterises per caller.
pub(crate) use by_name::{AssistRequest, TargetByNameRequest};
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
#[derive(Resource, Default, Clone, Copy)]
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
#[derive(Resource, Default, Clone, Copy)]
pub(crate) struct HoveredObject {
    pub(crate) target: Option<Entity>,
    pub(crate) guid: Option<u64>,
    /// World-space ray distance to the GO mesh, compared against [`Hovered::distance`].
    pub(crate) distance: f32,
}

/// This frame's **world-occlusion distance** for the mouse pick: the distance along the cursor ray
/// to the nearest [`benilla_world::collision::PickOccluder`] hit (terrain, the WMO walk-bake ≈ `0x84`
/// reject-mask faces, static doodad hulls), or `f32::INFINITY` with no hit. The reference's scene
/// trace (`0x480df0`, wow-re selection-circle PART 3, §5-cross-checked) traces objects unbounded
/// and discards the object hit iff the world hit is *strictly* nearer — a unit or GameObject
/// behind a wall is not hoverable. Written by [`hover::update_pick_occlusion`] at the head of the
/// target chain; both picks post-compare their final hit against it.
#[derive(Resource, Clone, Copy)]
pub(crate) struct PickOcclusion {
    pub(crate) distance: f32,
    /// The hit itself — the world-space point `distance` names along the cursor ray, or `None`
    /// with no hit (sky, mouselook, no cursor). The ground-targeting machine's hover/commit point
    /// (decision 0792): the same world trace the reference's targeting position rides.
    pub(crate) point: Option<Vec3>,
}

impl Default for PickOcclusion {
    fn default() -> Self {
        Self {
            distance: f32::INFINITY,
            point: None,
        }
    }
}

/// **The pick latched on the button DOWN edge** — what a world click acts on, frozen at the press.
///
/// The reference picks **exactly once per press** and never again for that gesture: `0x481f00` has
/// one caller image-wide, on the down edge (wow-re `world-click-drag-arbitration.md` §2), and it
/// writes the whole result into the WorldFrame — pick state `+0x350`, guid `+0x358`, hit point
/// `+0x360`, distance `+0x36c`. Nothing re-picks on move or on release, so the release acts on what
/// the **press** was over. That single fact is what lets one gesture orbit the camera *and* select:
/// the mouse may travel anywhere in between, and the latched pick is untouched.
///
/// benilla had no latch — every click consumer read the *live* hover, which
/// [`hover::update_hover`] clears outright the moment a look session engages. So the release found
/// an empty hover and [`click::select_on_click`] fell into its deselect arm: a drag didn't merely
/// fail to select, it would have **cleared** the target (decision 1122, ledger B226).
///
/// Note this is a *different* thing from the hover highlight, which the reference **does** suppress
/// during freelook (`0x483e80` sets the WorldFrame cursor-suppress bit `[wf+0x38c]&2`, tested by the
/// hover classifier `0x4828d0`). Tooltip, brighten and cursor go quiet while you orbit; the pending
/// click's subject does not. Conflating those two was the root cause.
///
/// Written by [`latch_press_pick`] at the head of the target chain, which runs **before**
/// [`hover::update_hover`] can clear anything, so the values it copies are the pick as of the press.
#[derive(Resource, Default, Clone, Copy)]
pub(crate) struct PressPick {
    pub(crate) hovered: Hovered,
    pub(crate) object: HoveredObject,
    pub(crate) occlusion: PickOcclusion,
    /// The cursor's Attack classification at the press — the reference's new-target validation
    /// (`0x5ecb70`), which likewise reads the pick the gesture started on, not a live one.
    pub(crate) attack: bool,
}

/// Latch the frame's pick as a press begins — the [`PressPick`] writer.
///
/// Ordered at the **head** of the target chain, ahead of every pick refresh, for a reason that is
/// easy to lose: `player::control` (input) has already run this frame, so on the press frame the
/// look session is *already* engaged and [`hover::update_hover`] — two systems below — is about to
/// clear the hover. The values still standing here are last frame's, computed at the same cursor
/// position the press landed on, because the cursor cannot have moved between the two. That is the
/// press-time pick, and it is the last moment it exists.
pub(crate) fn latch_press_pick(
    buttons: Res<ButtonInput<MouseButton>>,
    hovered: Res<Hovered>,
    object: Res<HoveredObject>,
    occlusion: Res<PickOcclusion>,
    cursor: Res<cursor_mode::WorldCursor>,
    mut press: ResMut<PressPick>,
) {
    // Either primary button's down edge arms a pick, exactly as the reference's `0x514810` does for
    // clickMode 1 (CameraOrSelectOrMove) and 2 (TurnOrAction). A chord — the second button of a
    // both-button run — must NOT re-latch: `0x51481a` refuses to arm while another primary is held,
    // and the arbiter cancels the pending click outright anyway.
    let left = buttons.just_pressed(MouseButton::Left) && !buttons.pressed(MouseButton::Right);
    let right = buttons.just_pressed(MouseButton::Right) && !buttons.pressed(MouseButton::Left);
    if !(left || right) {
        return;
    }
    *press = PressPick {
        hovered: *hovered,
        object: *object,
        occlusion: *occlusion,
        attack: cursor.kind == cursor_mode::CursorKind::Attack,
    };
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

/// The click-behavior player knob (decision 0961): `deselectOnClick` is 1.12's own CVar behind
/// the "Sticky Targeting" Interface Options checkbox (inverted there AND in era — checked means
/// the CVar at 0). Default true = the reference default: an empty-world click clears the
/// target, which is exactly the behavior [`click::select_on_click`] shipped with (0571/0574's
/// terrain/nothing legs); the knob only makes its gate player-settable through the CVar store
/// (0954).
#[derive(Resource)]
pub(crate) struct ClickConfig {
    pub(crate) deselect_on_click: bool,
}

impl Default for ClickConfig {
    fn default() -> Self {
        Self {
            deselect_on_click: true,
        }
    }
}

/// Targeting: click-to-select + the ground selection ring.
pub(crate) struct TargetPlugin;

impl Plugin for TargetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Selection>()
            .init_resource::<ClickConfig>()
            .init_resource::<Hovered>()
            .init_resource::<HoveredObject>()
            .init_resource::<PickOcclusion>()
            .init_resource::<PressPick>()
            .init_resource::<WorldCursor>()
            .init_resource::<CombatFlash>()
            .init_resource::<scan::TabHistory>()
            .add_message::<AttackNearestRequest>()
            .add_message::<TargetByNameRequest>()
            .add_message::<AssistRequest>()
            .add_systems(
                Startup,
                (
                    ring::setup_ring,
                    reticle::setup_reticle,
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
                    // Grouped as one element (the outer chain is at Bevy's 20-tuple limit), and
                    // ordered: the latch FIRST, ahead of every pick refresh, to freeze what the
                    // press was over while it still exists (decision 1122 — see [`latch_press_pick`]).
                    (latch_press_pick, hover::update_pick_occlusion).chain(),
                    hover::update_hover,
                    hover::update_hovered_object,
                    cursor_mode::classify_cursor,
                    // The right button's down-edge cancel first (the ref's OnMouseDown hook,
                    // 0792): the frame the press lands, the cursor drive below already reads
                    // the mode cleared and the classifier's verdict stands.
                    crate::ui_action::targeting::cancel_targeting_on_right_press,
                    // The ground-targeting pre-empt (decision 0792): overwrites the classifier's
                    // verdict while the targeting cursor is up — the ref's dispatcher runs this
                    // branch before the object classifier; last-writer-wins reads the same.
                    crate::ui_action::targeting::drive_targeting_cursor,
                    // The AoE reticle reads that verdict (`WorldCursor.unable` IS the frame's
                    // range state — the ref's one CheckGroundPointInRange caller feeds both).
                    reticle::update_reticle,
                    click::world_right_click_payload,
                    click::select_on_click,
                    // AFTER the gated select (which holds while the mode is active): the commit
                    // may clear the mode, and the selection gate must have read it first. The two
                    // world legs are siblings, not a fallback chain — the pending spell's word
                    // decides which of them a click can even feed (decision 0939), so their order
                    // relative to each other never matters.
                    crate::ui_action::targeting::commit_ground_cast_on_click,
                    crate::ui_action::targeting::commit_object_cast_on_click,
                    click::act_on_right_click,
                    click::clear_target_requests,
                    // The two unit-token drains, grouped as one element (the outer chain is at
                    // Bevy's 20-tuple limit): `TargetUnit`'s selection commit, and
                    // `DropItemOnUnit`'s pet-leg feed (decision 1055). Independent of each other —
                    // one writes `Selection`, the other sends a cast — so they need no order.
                    (
                        click::target_unit_requests,
                        crate::ui_action::drop_item::drop_item_on_unit,
                    ),
                    // The chat layer's by-name asks (decision 0886), beside the UI's token asks:
                    // both are non-mouse selection writers committing through `scan::commit`.
                    // The chat layer's by-name asks, grouped as one chained element (the outer
                    // chain is at Bevy's 20-tuple limit): `/target` and `/assist` commit through
                    // `scan::commit` like every other non-mouse selection writer, and `/follow`
                    // resolves its subject for `crate::player` to move (decision 0890) without
                    // touching `Selection` at all.
                    (
                        by_name::target_by_name_requests,
                        by_name::assist_requests,
                        by_name::follow_requests,
                    )
                        .chain(),
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
            .add_systems(PostUpdate, highlight::apply_highlight)
            // The ring's stream push: after the frame's stream clear (the projection cache was
            // rebuilt by `update_ring` in Update; this is a tinted copy).
            .add_systems(
                PostUpdate,
                (ring::push_ring, reticle::push_reticle)
                    .after(benilla_world::particles::buffer::begin_effect_frame),
            );
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

    /// The latch survives the hover being cleared — which is the whole reason it exists.
    ///
    /// A drag engages the look session, and `hover::update_hover` then blanks [`Hovered`] for its
    /// whole duration (as the reference suppresses its own hover during freelook). Before decision
    /// 1122 every click consumer read that live hover, so a click ending a drag saw *nothing*
    /// hovered and [`click::select_on_click`] took its deselect arm: the gesture would have cleared
    /// the player's target rather than selecting. The press latch is what a click reads instead.
    #[test]
    fn the_press_latch_outlives_the_hover_a_drag_clears() {
        let mut world = World::new();
        world.init_resource::<Hovered>();
        world.init_resource::<HoveredObject>();
        world.init_resource::<PickOcclusion>();
        world.init_resource::<cursor_mode::WorldCursor>();
        world.init_resource::<PressPick>();
        world.init_resource::<ButtonInput<MouseButton>>();
        let id = world.register_system(latch_press_pick);

        // A boar under the cursor, and the cursor classified as Attack over it.
        const BOAR: u64 = 0xB0A2;
        world.resource_mut::<Hovered>().guid = Some(BOAR);
        world.resource_mut::<cursor_mode::WorldCursor>().kind = cursor_mode::CursorKind::Attack;

        // Press left. The latch takes the pick.
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        world.run_system(id).unwrap();
        assert_eq!(world.resource::<PressPick>().hovered.guid, Some(BOAR));
        assert!(world.resource::<PressPick>().attack, "Attack rode along");

        // The drag begins: the look session blanks the live hover, every frame, for as long as it
        // lasts. The latch must not follow it down.
        *world.resource_mut::<Hovered>() = Hovered::default();
        world.resource_mut::<ButtonInput<MouseButton>>().clear();
        world.run_system(id).unwrap();
        assert_eq!(
            world.resource::<PressPick>().hovered.guid,
            Some(BOAR),
            "the release must still know what the press was over"
        );
    }

    /// A chord — the second primary joining a held one — must not re-latch. The reference refuses
    /// to arm or pick while another primary is down (`0x51481a`), and kills the pending click
    /// outright (`0x514ac1`), so neither release of a both-button run can select. Re-latching here
    /// would hand a spurious pick to a gesture that is only ever a camera turn.
    #[test]
    fn a_chord_does_not_relatch_the_pick() {
        let mut world = World::new();
        world.init_resource::<Hovered>();
        world.init_resource::<HoveredObject>();
        world.init_resource::<PickOcclusion>();
        world.init_resource::<cursor_mode::WorldCursor>();
        world.init_resource::<PressPick>();
        world.init_resource::<ButtonInput<MouseButton>>();
        let id = world.register_system(latch_press_pick);

        const FIRST: u64 = 0x1111;
        const SECOND: u64 = 0x2222;
        world.resource_mut::<Hovered>().guid = Some(FIRST);
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        world.run_system(id).unwrap();
        assert_eq!(world.resource::<PressPick>().hovered.guid, Some(FIRST));

        // Right joins while left is still held, over something else entirely.
        world.resource_mut::<Hovered>().guid = Some(SECOND);
        let mut buttons = world.resource_mut::<ButtonInput<MouseButton>>();
        buttons.clear(); // drop last frame's just_pressed, keep `pressed`
        buttons.press(MouseButton::Right);
        world.run_system(id).unwrap();
        assert_eq!(
            world.resource::<PressPick>().hovered.guid,
            Some(FIRST),
            "the chord's second press must not steal the latch"
        );
    }
}
