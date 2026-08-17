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
//!
//! The **instruments** built on this base — the dev-chord `I` inspector surface, the cast journal,
//! and the mouseover pick that only ever ran while the inspector was armed — live in
//! `debug_panel::{inspect, journal}` (decision 1160's stage zero). They were here first, and they
//! were what made this module read as the most entangled thing on the engine/game line: between
//! them they named the whole `target::cursor_mode` GO-reaction vocabulary, the spell catalog, the
//! GameObject templates, the name cache and the net writer. What remains here — identity, the
//! ray-caster and the three click gestures — reaches for none of it.
//!
//! The player-facing consumers (`crate::target`) run their picks through [`pick`] unconditionally.

use bevy::prelude::*;

use crate::model_render::ModelKind;

mod pick;

pub use pick::{
    cast_pick_ray, cast_pick_ray_inflated, pick_at_cursor, PickBox, PickMesh, PickParts,
};
pub use pick::{ray_mesh_bounds, ray_posed_mesh};

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

/// Marker on every pickable **GameObject** part — the GO mouseover pick's population
/// (`target/hover.rs`). Inserted beside the part's `WorldObject { kind: GameObject }` at the
/// attach spawn sites, so building the pick set is an archetype-filtered query over the handful
/// of GO parts on screen instead of a kind-compare over every streamed `WorldObject` row (the
/// doodad/WMO population dwarfs it).
#[derive(Component, Clone, Copy)]
pub struct GoPickPart;

/// The creature twin of [`GoPickPart`]: every pickable unit/player part — the model-less fallback
/// cube included — the unit pick's skinless-fallback population. Doodad/WMO parts carry neither
/// marker.
#[derive(Component, Clone, Copy)]
pub struct CreaturePickPart;

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

/// Registers the world-interaction **foundation**: the identity component's three click messages
/// and the shared ray-caster's parameters. The inspector surface and the cast journal that used to
/// be registered here are instruments, and moved to `debug_panel` in decision 1160's stage zero —
/// along with the mouseover pick, which only ever ran while the inspector was armed.
pub struct InteractPlugin;

impl Plugin for InteractPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<WorldClick>()
            .add_message::<WorldRightClick>()
            .add_message::<WorldRightPress>();
    }
}
