//! World interaction foundation — *what is the player pointing at?*
//!
//! This is the shared base for every "point at the world" feature: the debug **object inspector**,
//! hover tooltips, the contextual cursor (gear over objects, sword over attackable units),
//! mouseover-targeting, and right-click-to-interact. Built once so we don't build picking +
//! identity twice.
//!
//! The pieces:
//! - [`WorldObject`] — an **identity** component on every pickable world entity (a doodad/WMO model, a
//!   creature, a GameObject): its kind, a human label (model path or unit name), an id, and an optional
//!   detail line. Attached at the spawn sites.
//! - the **ray caster** ([`pick`]) — the pick-geometry declarations ([`PickMesh`] / [`PickBox`]) and
//!   the shared triangle-accurate cast against the **actual resident mesh geometry** (so it works on
//!   colliderless props — most doodads, including the campfire — which a physics raycast misses),
//!   plus the generous inflated pass the mouse pick retries with (decision 1071).
//! - [`MouseoverTarget`] — the resource [`update_mouseover`] fills each frame with the nearest
//!   `WorldObject` under the cursor, for the inspector.
//! - the **inspector surface** ([`inspect`]) — a standalone, key-toggleable overlay (**the dev
//!   chord + `I`**): a weak "armed" pill plus a small identity card that follows the cursor over any
//!   picked object. It's its own surface, *not* a section of the dev-chord debug panel, so
//!   identifying a thing costs one chord and no panel.
//! - the **cast journal** ([`journal`]) — the *temporal* half of the same instrument: a spell is an
//!   event, gone before a cursor could reach it, so every cast edge is recorded as it flows past and
//!   the same inspector overlay lists the recent ones, click-to-copy.
//!
//! The inspector's own picking only runs while [`InspectMode`] is on (the inspector chord); the
//! player-facing consumers (`crate::target`) run their picks through [`pick`] unconditionally.

use bevy::platform::collections::HashSet;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::EguiPrimaryContextPass;

use crate::debug_panel::ModelKind;
use crate::player::WorldCamera;
use crate::ui_script::PointerOverUi;

mod inspect;
mod journal;
mod pick;

pub use pick::{
    cast_pick_ray, cast_pick_ray_inflated, pick_at_cursor, PickBox, PickMesh, PickParts,
};
pub(crate) use pick::{ray_aabb, ray_triangle, world_aabb};

/// The identity of a pickable world thing, read by the inspector now and by tooltips/cursor/targeting
/// later. Attached to a thing's renderable mesh entities at spawn.
#[derive(Component, Clone)]
pub struct WorldObject {
    pub kind: ModelKind,
    /// Primary label — a model path (doodads/WMOs/GameObjects) or a unit name. Shown to the player/dev.
    pub label: String,
    /// Identifier — placement uniqueId, server guid, or display id (`0` if none).
    pub id: u32,
    /// Optional second line of kind-specific detail (e.g. `"emitters: 2"`); shown when non-empty.
    pub detail: String,
}

/// A left *select* gesture in the world. Emitted by [`crate::player::control`] on the button's
/// **release**, when the press satisfied the reference's click predicate — under 200 ms whatever the
/// mouse did, or under 800 ms having turned the camera less than 2.25° of yaw / 2.0° of pitch
/// ([`crate::player::camera::PressGesture`], decision 1122).
///
/// **A drag emits this too.** Orbit and select are independent in the reference: the press engages
/// the camera look immediately and arms this test alongside it, so a fast flick-and-click orbits the
/// camera *and* selects. This doc comment used to assert the opposite as design — *"a left drag
/// engages the orbit and emits nothing"* — and a reporter quoted it back at us with an A/B against
/// the real client proving it wrong (ledger B226).
///
/// It carries no position because the *pick* is latched separately, at the press
/// ([`crate::target::PressPick`]) — never the live hover, which is blank by the time a dragged
/// click lands.
#[derive(Message, Clone, Copy)]
pub struct WorldClick;

/// The right button's context gesture — same arbiter and same predicate as [`WorldClick`], for the
/// button whose drag is the character turn. Vanilla's context action: attack a hostile under the
/// cursor (later: interact/gossip on a friendly).
#[derive(Message, Clone, Copy)]
pub struct WorldRightClick;

/// The right button's **DOWN edge** in the world — emitted at the press, before the click-vs-drag
/// test even starts, whenever the press belongs to the world (in the viewport off the UI, or any
/// press while a look session already owns the hidden cursor). The reference's
/// `CGWorldFrame::OnMouseDown 0x483c40` analogue: ground-targeting's right-click cancel hangs off
/// this edge (`0x492c20`, wow-re `world-click-targeting.md` Q3), which fires whether the press
/// becomes a click OR a turn-drag, and consumes nothing (the ref handler returns 0, so the
/// BUTTON2 turn and the release's context click still run).
#[derive(Message, Clone, Copy)]
pub struct WorldRightPress;

/// Whether mouseover picking runs. Today it's armed/disarmed by the **dev chord + `I`** inspector toggle
/// ([`inspect::toggle_inspect`]).
#[derive(Resource, Default)]
pub struct InspectMode {
    pub enabled: bool,
}

/// The nearest [`WorldObject`] under the cursor this frame, or `None`. Consumers read `entity` and look
/// up its [`WorldObject`]; `point`/`distance` are the world-space hit.
#[derive(Resource, Default)]
pub struct MouseoverTarget {
    pub entity: Option<Entity>,
    pub point: Vec3,
    pub distance: f32,
}

/// Ray-cast from the cursor into the world and record the nearest [`WorldObject`] hit. Restricted to
/// entities carrying `WorldObject` (so terrain, particle billboards, and other un-identified meshes are
/// transparent to the pick), and skipped entirely unless inspection is active.
fn update_mouseover(
    inspect: Res<InspectMode>,
    pointer_over_ui: Res<PointerOverUi>,
    mut target: ResMut<MouseoverTarget>,
    window: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    objects: Query<Entity, With<WorldObject>>,
    parts: PickParts,
) {
    if !inspect.enabled {
        if target.entity.is_some() {
            *target = MouseoverTarget::default();
        }
        return;
    }
    target.entity = None;
    // The pointer is over the dev UI (e.g. the now-overlaid debug panel), not the world — don't pick
    // behind it. This replaces the old "is the cursor in the inset world viewport?" test, which no
    // longer means anything now the panel overlays a full-screen view.
    if pointer_over_ui.0 {
        return;
    }
    let Ok((camera, cam_tf)) = camera.single() else {
        return;
    };
    let Ok(window) = window.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return; // cursor left the window, or we're in mouselook (hidden)
    };
    let identified: HashSet<Entity> = objects.iter().collect();
    if let Some((entity, point, distance)) =
        pick_at_cursor(cursor, camera, cam_tf, &identified, &parts)
    {
        target.entity = Some(entity);
        target.point = point;
        target.distance = distance;
    }
}

/// Registers the mouseover foundation (the [`MouseoverTarget`] + [`InspectMode`] resources and the
/// per-frame pick), the standalone dev-chord `I` inspector surface, and the cast journal (recording
/// always, drawing under the same toggle).
pub struct InteractPlugin;

impl Plugin for InteractPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MouseoverTarget>()
            .init_resource::<InspectMode>()
            .init_resource::<journal::CastJournal>()
            .add_message::<WorldClick>()
            .add_message::<WorldRightClick>()
            .add_message::<WorldRightPress>()
            // After the UI keyboard feed because `update_mouseover` reads `PointerOverUi`, whose
            // player-UI half `UiInput` writes — the pick must see this frame's hover, not last
            // frame's. (`toggle_inspect` itself no longer needs the ordering: its dev chord
            // can't be typed text, so it reads no keyboard-capture flag — decision 0585.)
            .add_systems(
                Update,
                (inspect::toggle_inspect, update_mouseover)
                    .chain()
                    .after(crate::ui_script::UiInput),
            )
            // Always recording (messages persist two frames — no ordering constraint needed).
            .add_systems(Update, journal::record_casts)
            .add_systems(
                EguiPrimaryContextPass,
                (inspect::inspect_ui, journal::journal_ui),
            );
    }
}
