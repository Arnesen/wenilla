//! World interaction foundation — *what is the player pointing at?*
//!
//! This is the shared base for every "point at the world" feature: the debug **object inspector**
//! (today), and tomorrow hover tooltips, the contextual cursor (gear over objects, sword over
//! attackable units), mouseover-targeting, and right-click-to-interact. Built once so we don't build
//! picking + identity twice.
//!
//! Three pieces:
//! - [`WorldObject`] — an **identity** component on every pickable world entity (a doodad/WMO model, a
//!   creature, a GameObject): its kind, a human label (model path or unit name), an id, and an optional
//!   detail line. Attached at the spawn sites.
//! - [`MouseoverTarget`] — the resource [`update_mouseover`] fills each frame with the nearest
//!   `WorldObject` under the cursor, found by ray-casting against the **actual mesh geometry** (so it
//!   works on colliderless props — most doodads, including the campfire — which a physics raycast
//!   misses).
//! - the **inspector surface** ([`inspect_ui`]) — a standalone, key-toggleable overlay (**Ctrl+Cmd+I**): a
//!   weak "armed" pill plus a small identity card that follows the cursor over any picked object. It's
//!   its own surface, *not* a section of the backtick debug panel, so identifying a thing costs one
//!   chord and no panel.
//! - the **cast journal** ([`journal`]) — the *temporal* half of the same instrument: a spell is an
//!   event, gone before a cursor could reach it, so every cast edge is recorded as it flows past and
//!   the same inspector overlay lists the recent ones, click-to-copy.
//!
//! Picking only runs while [`InspectMode`] is on (today's sole consumer, driven by the inspector
//! chord); when real player-facing consumers arrive it simply runs whenever any of them is active.

use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings, RayCastVisibility};
use bevy::platform::collections::HashSet;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};

use crate::debug_panel::{overlay_text, ModelKind, OVERLAY_FILL, OVERLAY_TEXT_DIM};
use crate::net::ObjectStore;
use crate::player::WorldCamera;
use crate::ui_script::PointerOverUi;

mod journal;

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

/// A clean left-*click* in the world — a press + release with no drag. Emitted by
/// [`crate::player::control`], which is the single arbiter of click-vs-drag because it owns the
/// left-drag camera orbit: a left *drag* engages the orbit and emits nothing, a left *click* emits this.
/// World-interaction consumers (target selection) read it instead of re-deciding the gesture; *where*
/// the cursor is is already tracked continuously (the hover pick), so this carries no position.
#[derive(Message, Clone, Copy)]
pub struct WorldClick;

/// A clean right-*click* in the world — same arbiter and drag test as [`WorldClick`], for the right
/// button (whose *drag* is the character turn). Vanilla's context action: attack a hostile under the
/// cursor (later: interact/gossip on a friendly).
#[derive(Message, Clone, Copy)]
pub struct WorldRightClick;

/// Whether mouseover picking runs. Today it's armed/disarmed by the **Ctrl+Cmd+I** inspector toggle
/// ([`toggle_inspect`]).
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
    mut ray_cast: MeshRayCast,
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
        pick_at_cursor(cursor, camera, cam_tf, &identified, &mut ray_cast)
    {
        target.entity = Some(entity);
        target.point = point;
        target.distance = distance;
    }
}

/// Cast a ray from the logical `cursor` position into the world and return the nearest hit among
/// `pickable`: `(entity, world point, distance)`. The `pickable` set restricts the cast (terrain,
/// particle billboards, and other un-identified meshes stay transparent), so callers choose *what* is
/// pickable while sharing *how* — the inspector's per-frame mouseover passes every [`WorldObject`], the
/// target picker passes only unit meshes (so a doodad in front doesn't block a click on a mob).
pub fn pick_at_cursor(
    cursor: Vec2,
    camera: &Camera,
    cam_tf: &GlobalTransform,
    pickable: &HashSet<Entity>,
    ray_cast: &mut MeshRayCast,
) -> Option<(Entity, Vec3, f32)> {
    let ray = camera.viewport_to_world(cam_tf, cursor).ok()?;
    let filter = |e: Entity| pickable.contains(&e);
    let settings = MeshRayCastSettings::default()
        .with_visibility(RayCastVisibility::VisibleInView)
        .with_filter(&filter);
    ray_cast
        .cast_ray(ray, &settings)
        .first()
        .map(|(e, hit)| (*e, hit.point, hit.distance))
}

/// **Ctrl+Cmd+I** (for *inspect*) arms/disarms the inspector — the dev-instrument chord, off the
/// bare-letter plane the game's own bindings own (decision 0585). Unmistakable as a chord, so unlike
/// the old bare `i` it needs no chat-bar/EditBox gate.
fn toggle_inspect(keys: Res<ButtonInput<KeyCode>>, mut inspect: ResMut<InspectMode>) {
    if crate::debug_panel::dev_chord(&keys, KeyCode::KeyI) {
        inspect.enabled = !inspect.enabled;
    }
}

/// How long the inspector card shows its "copied to clipboard" confirmation after a left-click.
const COPY_FLASH_SECS: f32 = 1.2;

/// A per-kind accent so the card's header is glanceable (which *sort* of thing am I over?) before you
/// even read the label.
fn kind_color(kind: ModelKind) -> egui::Color32 {
    match kind {
        ModelKind::Doodad => egui::Color32::from_rgb(140, 220, 140), // green — props/trees
        ModelKind::Wmo => egui::Color32::from_rgb(150, 185, 240),    // blue — buildings
        ModelKind::Creature => egui::Color32::from_rgb(240, 205, 130), // gold — NPCs
        ModelKind::GameObject => egui::Color32::from_rgb(220, 165, 220), // violet — GameObjects
    }
}

/// The inspector overlay, drawn only while armed: a weak top-centre "armed" pill (so it's obvious the
/// mode is on and how to leave it) and, whenever the cursor is over an identified object, a compact
/// identity card pinned to the cursor. No chrome, no panel — its own lightweight surface.
#[allow(clippy::too_many_arguments)]
fn inspect_ui(
    mut contexts: EguiContexts,
    inspect: Res<InspectMode>,
    mouseover: Res<MouseoverTarget>,
    objects: Query<&WorldObject>,
    // The pickable mesh is a child of the net entity; its descriptor store (`ObjectStore`) lives on the
    // parent, so the readout hops child → parent.
    parents: Query<&ChildOf>,
    // Bundled into one param (the same arity ceiling as `click_input` below): the descriptor
    // store + the coarse kind the line gates go by.
    stores: (Query<&ObjectStore>, Query<&crate::net::NetEntity>),
    guids: Query<&crate::net::Guid>,
    castings: Query<&crate::creature_anim::Casting>,
    drivers: Query<&crate::creature_anim::AnimDriver>,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    mut names: ResMut<crate::names::NameCache>,
    net_commands: Res<crate::net::NetCommands>,
    // Bundled into one param (Bevy's system-function arity ceiling): the copy-click button, and
    // the flag it must yield to — a left press this frame the UI already consumed as a
    // cursor-payload world drop (0216 §3) must not ALSO land as an inspector copy-click, the same
    // yield every other world left-press consumer gives it (see `PointerOverUi` above for the
    // hover-time twin).
    click_input: (
        Res<ButtonInput<MouseButton>>,
        Res<crate::ui_script::PlayerUiClickConsumed>,
    ),
    time: Res<Time>,
    mut copied_at: Local<Option<f32>>,
) -> Result {
    let (buttons, click_consumed) = (&click_input.0, &click_input.1);
    if !inspect.enabled {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;

    // Armed indicator — small + dim, so it states "inspect is on" without competing with the world.
    egui::Area::new(egui::Id::new("inspect_armed"))
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 8.0))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(8, 4))
                .corner_radius(5.0)
                .fill(OVERLAY_FILL)
                .show(ui, |ui| {
                    overlay_text(ui);
                    // Spelled out, not ⌃⌘ — egui's default font stack has no glyph for U+2303 and
                    // would draw tofu.
                    ui.label(egui::RichText::new("inspect · ctrl+cmd+I to exit").small());
                });
        });

    // The identity card: only when hovering a picked object, pinned just off the cursor tip.
    let Some(obj) = mouseover.entity.and_then(|e| objects.get(e).ok()) else {
        return Ok(());
    };
    let Some(cursor) = ctx.pointer_latest_pos() else {
        return Ok(());
    };

    // A unit's decoded server vitals from its descriptor store (`ObjectStore`), if the picked mesh's
    // parent has them — proof the descriptor pipeline (UpdateFields → ObjectValues → ECS) reached it.
    let net_entity = mouseover
        .entity
        .and_then(|e| parents.get(e).ok())
        .map(|c| c.parent());
    let (stores, kinds) = (&stores.0, &stores.1);
    let store = net_entity.and_then(|p| stores.get(p).ok());
    // The unit's server name through the query cache — asks on first hover, fills on a later frame
    // (the same ask-once path the unit frames use).
    let name_line = net_entity
        .and_then(|p| guids.get(p).ok())
        .and_then(|g| names.resolve(g.0, &net_commands))
        .map(str::to_string);
    // Line gates go by the entity's KIND, not field presence: a create-seeded store answers every
    // field (absent = 0, the descriptor truth), so "is the health field there" stopped meaning
    // "is this a unit".
    let kind = net_entity.and_then(|p| kinds.get(p).ok()).map(|n| n.kind);
    let is_unit = matches!(
        kind,
        Some(benilla_protocol::EntityKind::Unit | benilla_protocol::EntityKind::Player)
    );
    let is_player = kind == Some(benilla_protocol::EntityKind::Player);
    let vitals_line = store.filter(|_| is_unit).map(|s| {
        format!(
            "hp {}/{} · level {}",
            s.0.unit_health().unwrap_or(0),
            s.0.unit_max_health().unwrap_or(0),
            s.0.unit_level().unwrap_or(0)
        )
    });
    // Raw bytes (not name-mapped): creature race/class don't share the player-race enum, so a label
    // would mislead. The character model will name-map these for players specifically.
    let appearance_line = store.filter(|_| is_unit).map(|s| {
        format!(
            "race {} · class {} · sex {}",
            s.0.unit_race().unwrap_or(0),
            s.0.unit_class().unwrap_or(0),
            s.0.unit_gender().unwrap_or(0)
        )
    });
    // Player-only customization: the compositor's input, shown raw so we can
    // confirm the PLAYER_BYTES decode against an in-game character.
    let customization_line = store.filter(|_| is_player).map(|s| {
        format!(
            "skin {} · face {} · hair {}/{} · facial {}",
            s.0.player_skin().unwrap_or(0),
            s.0.player_face().unwrap_or(0),
            s.0.player_hair_style().unwrap_or(0),
            s.0.player_hair_color().unwrap_or(0),
            s.0.player_facial_hair().unwrap_or(0)
        )
    });

    // A unit mid-cast (`SMSG_SPELL_START` .. GO — the `Casting` wire seam): which spell, by id and
    // display name. The director's "what is it casting?" answered on hover; the finished cast's
    // trail lives in the journal.
    let casting_line = net_entity.and_then(|p| castings.get(p).ok()).map(|c| {
        match spells.as_ref().and_then(|s| s.catalog.get(c.spell_id)) {
            Some(d) => format!("casting {} \"{}\"", c.spell_id, d.name),
            None => format!("casting {}", c.spell_id),
        }
    });
    // The animation slots this frame (requested `AnimationData` ids — the selector's choice,
    // before missing-clip substitution): the full-body base + any masked upper-body overlay.
    let anim_line = net_entity.and_then(|p| drivers.get(p).ok()).map(|d| {
        let fmt = |id: u16| match anim_data.as_ref().and_then(|a| a.0.name(id)) {
            Some(name) => format!("{name}({id})"),
            None => format!("{id}"),
        };
        let (base, overlay) = d.playing();
        let base = base.map(&fmt).unwrap_or_else(|| "—".into());
        match overlay {
            Some(o) => format!("anim {base} + overlay {}", fmt(o)),
            None => format!("anim {base}"),
        }
    });

    // The lines shown in the card — also exactly what a left-click copies to the clipboard.
    let mut lines = vec![format!("{:?}", obj.kind), obj.label.clone()];
    if let Some(name) = &name_line {
        lines.push(format!("\"{name}\""));
    }
    if obj.id != 0 {
        lines.push(format!("id {}", obj.id));
    }
    if !obj.detail.is_empty() {
        lines.push(obj.detail.clone());
    }
    if let Some(line) = &vitals_line {
        lines.push(line.clone());
    }
    if let Some(line) = &appearance_line {
        lines.push(line.clone());
    }
    if let Some(line) = &customization_line {
        lines.push(line.clone());
    }
    if let Some(line) = &casting_line {
        lines.push(line.clone());
    }
    if let Some(line) = &anim_line {
        lines.push(line.clone());
    }
    lines.push(format!("{:.1} yd away", mouseover.distance));

    // The inspector owns left-click while armed (player::control suppresses left-orbit during inspect),
    // so a press over the hovered object copies the whole card to the clipboard.
    if buttons.just_pressed(MouseButton::Left) && !click_consumed.0 {
        ctx.copy_text(lines.join("\n"));
        *copied_at = Some(time.elapsed_secs());
    }
    let just_copied = copied_at.is_some_and(|t| time.elapsed_secs() - t < COPY_FLASH_SECS);

    egui::Area::new(egui::Id::new("inspect_card"))
        .fixed_pos(cursor + egui::vec2(18.0, 18.0))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(8, 6))
                .corner_radius(5.0)
                .fill(OVERLAY_FILL)
                .show(ui, |ui| {
                    overlay_text(ui);
                    ui.colored_label(kind_color(obj.kind), format!("{:?}", obj.kind));
                    ui.label(egui::RichText::new(&obj.label).monospace());
                    if obj.id != 0 {
                        ui.label(
                            egui::RichText::new(format!("id {}", obj.id)).color(OVERLAY_TEXT_DIM),
                        );
                    }
                    if !obj.detail.is_empty() {
                        ui.label(egui::RichText::new(&obj.detail).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &vitals_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &appearance_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &customization_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &casting_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    if let Some(line) = &anim_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
                    }
                    ui.label(
                        egui::RichText::new(format!("{:.1} yd away", mouseover.distance))
                            .color(OVERLAY_TEXT_DIM),
                    );
                    // Copy affordance, swapped for a brief confirmation after a left-click.
                    if just_copied {
                        ui.label(
                            egui::RichText::new("copied to clipboard")
                                .small()
                                .color(egui::Color32::from_rgb(140, 220, 140)),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("left-click to copy")
                                .small()
                                .color(OVERLAY_TEXT_DIM),
                        );
                    }
                });
        });
    Ok(())
}

/// Registers the mouseover foundation (the [`MouseoverTarget`] + [`InspectMode`] resources and the
/// per-frame pick), the standalone I-toggled inspector surface, and the cast journal (recording
/// always, drawing under the same toggle).
pub struct InteractPlugin;

impl Plugin for InteractPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MouseoverTarget>()
            .init_resource::<InspectMode>()
            .init_resource::<journal::CastJournal>()
            .add_message::<WorldClick>()
            .add_message::<WorldRightClick>()
            // After the UI keyboard feed because `update_mouseover` reads `PointerOverUi`, whose
            // player-UI half `UiInput` writes — the pick must see this frame's hover, not last
            // frame's. (`toggle_inspect` itself no longer needs the ordering: its Ctrl+Cmd+I chord
            // can't be typed text, so it reads no keyboard-capture flag — decision 0585.)
            .add_systems(
                Update,
                (toggle_inspect, update_mouseover)
                    .chain()
                    .after(crate::ui_script::UiInput),
            )
            // Always recording (messages persist two frames — no ordering constraint needed).
            .add_systems(Update, journal::record_casts)
            .add_systems(EguiPrimaryContextPass, (inspect_ui, journal::journal_ui));
    }
}
