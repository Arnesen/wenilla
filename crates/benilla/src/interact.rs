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
//! - the **inspector surface** ([`inspect_ui`]) — a standalone, key-toggleable overlay (**the dev
//!   chord + `I`**): a weak "armed" pill plus a small identity card that follows the cursor over any
//!   picked object. It's its own surface, *not* a section of the backtick debug panel, so
//!   identifying a thing costs one chord and no panel.
//! - the **cast journal** ([`journal`]) — the *temporal* half of the same instrument: a spell is an
//!   event, gone before a cursor could reach it, so every cast edge is recorded as it flows past and
//!   the same inspector overlay lists the recent ones, click-to-copy.
//!
//! Picking only runs while [`InspectMode`] is on (today's sole consumer, driven by the inspector
//! chord); when real player-facing consumers arrive it simply runs whenever any of them is active.

use benilla_assets::coords::wow_to_bevy;
use bevy::camera::primitives::Aabb;
use bevy::platform::collections::HashSet;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};

use crate::debug_panel::{overlay_text, ModelKind, DEV_CHORD, OVERLAY_FILL, OVERLAY_TEXT_DIM};
use crate::net::ObjectStore;
use crate::player::WorldCamera;
use crate::ui_script::PointerOverUi;

mod journal;

/// The resident **pick geometry** of one drawn batch: the model's decoded `RenderSubmesh`, `Arc`-shared
/// with the model asset itself (decision 0857). The render forms are `RENDER_WORLD`-only since 0834 —
/// their main-world vertex data is gone after extract, which is why Bevy's `MeshRayCast` silently
/// stopped hitting every static model (the GO hover, the inspector, `WOW_PICK`). So the pickers read
/// triangles from THIS instead, through the same WoW→Bevy bake the render form was built with
/// (`submesh_to_static_mesh`: `wow_to_bevy` per vertex, billboard cards centred at their pivot).
/// Attached beside `Mesh3d` at every spawn site that also attaches a pick key ([`WorldObject`] /
/// `ModelPart`); a keyed entity without one (the model-less cube fallback) is picked by its `Aabb`.
#[derive(Component, Clone)]
pub struct PickMesh(pub std::sync::Arc<benilla_formats::RenderSubmesh>);

/// One triangle-accurate hit from [`cast_pick_ray`]: the world-space point, the hit triangle's
/// geometric normal (two-sided — the orientation is the authored winding's), and the distance along
/// the ray.
pub struct RayHit {
    pub point: Vec3,
    pub normal: Vec3,
    pub distance: f32,
}

/// The one query every mesh picker casts against: a part's resident geometry, its world pose, the
/// render world's own visibility verdict (the cast honours it, as `RayCastVisibility::VisibleInView`
/// did), and its bound for the broad phase.
pub type PickParts<'w, 's> = Query<
    'w,
    's,
    (
        Option<&'static PickMesh>,
        &'static GlobalTransform,
        &'static ViewVisibility,
        Option<&'static Aabb>,
    ),
>;

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
    parts: &PickParts,
) -> Option<(Entity, Vec3, f32)> {
    let ray = camera.viewport_to_world(cam_tf, cursor).ok()?;
    cast_pick_ray(ray, pickable, parts, false)
        .into_iter()
        .next()
        .map(|(e, hit)| (e, hit.point, hit.distance))
}

/// Cast `ray` against `pickable`'s **resident geometry** ([`PickMesh`] — decision 0857: the render
/// meshes are `RENDER_WORLD`-only, so there is no main-world mesh data to cast against) and return
/// the hits nearest-first: one per entity (its nearest triangle), the whole list with `all_hits`
/// (the `WOW_PICK` probe's everything-along-the-ray reading) or just the front entity without.
///
/// Broad phase: a world-space slab test on each candidate's `Aabb` (entry distance), nearest entry
/// first, so the narrow walk stops as soon as a confirmed hit is closer than every remaining box. A
/// part with no bound (a `NoFrustumCulling` fx part) is narrow-tested unconditionally; a keyed
/// entity with a bound but no [`PickMesh`] (the model-less cube fallback, whose cuboid *is* its
/// box) takes its box entry as the hit. Triangles test **two-sided**, like the unit picker's narrow
/// phase — a generous pick beats a strict one at silhouette edges.
pub fn cast_pick_ray(
    ray: Ray3d,
    pickable: &HashSet<Entity>,
    parts: &PickParts,
    all_hits: bool,
) -> Vec<(Entity, RayHit)> {
    let (origin, dir) = (ray.origin, *ray.direction);
    let mut candidates: Vec<(f32, Entity)> = Vec::new();
    for &entity in pickable {
        let Ok((mesh, gt, vis, aabb)) = parts.get(entity) else {
            continue;
        };
        if !vis.get() {
            continue; // not drawn in any view this frame — the old cast's VisibleInView rule
        }
        let entry = match (aabb, mesh) {
            (Some(aabb), _) => {
                let (min, max) = world_aabb(aabb, gt);
                match ray_aabb(origin, dir, min, max) {
                    Some(t) => t,
                    None => continue,
                }
            }
            (None, Some(_)) => 0.0,
            (None, None) => continue,
        };
        candidates.push((entry, entity));
    }
    candidates.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    let mut hits: Vec<(Entity, RayHit)> = Vec::new();
    let mut best = f32::INFINITY;
    for (entry, entity) in candidates {
        if !all_hits && best < entry {
            break; // every remaining box starts beyond the confirmed nearest hit
        }
        let Ok((mesh, gt, _, _)) = parts.get(entity) else {
            continue;
        };
        let hit = match mesh {
            Some(m) => ray_pick_mesh(m, gt, origin, dir),
            // The cube fallback: its box entry is exactly its surface (the cuboid IS its Aabb).
            None => Some(RayHit {
                point: origin + dir * entry,
                normal: -dir,
                distance: entry,
            }),
        };
        if let Some(h) = hit {
            best = best.min(h.distance);
            hits.push((entity, h));
        }
    }
    hits.sort_unstable_by(|a, b| a.1.distance.total_cmp(&b.1.distance));
    if !all_hits {
        hits.truncate(1);
    }
    hits
}

/// The narrow phase for one part: ray-test every triangle of its resident geometry, in **model-local
/// space** — the world ray mapped through the part's inverse affine, under which the ray parameter is
/// unchanged, so a local `t` (against the world-normalized direction's image) *is* the world distance.
/// Vertices go through the render form's own bake (`build_submesh_mesh`): `wow_to_bevy` per vertex,
/// a billboard card centred at its pivot — so the pick tests exactly the surface the part draws
/// (a card's live camera-facing rotation rides its `GlobalTransform`, shared here too).
fn ray_pick_mesh(mesh: &PickMesh, gt: &GlobalTransform, origin: Vec3, dir: Vec3) -> Option<RayHit> {
    let geo = &*mesh.0;
    let inv = gt.affine().inverse();
    let local_origin = inv.transform_point3(origin);
    let local_dir = inv.transform_vector3(dir);
    let center = geo
        .billboard
        .as_ref()
        .map_or(Vec3::ZERO, |b| wow_to_bevy(b.pivot));
    let pos = |i: u32| Some(wow_to_bevy(*geo.positions.get(i as usize)?) - center);
    let mut nearest: Option<(f32, [Vec3; 3])> = None;
    for t in geo.indices.chunks_exact(3) {
        let (Some(a), Some(b), Some(c)) = (pos(t[0]), pos(t[1]), pos(t[2])) else {
            continue; // an out-of-range index — corrupt authoring; skip the triangle, not the model
        };
        let tri = [a, b, c];
        if let Some(d) = ray_triangle(local_origin, local_dir, &tri) {
            if nearest.is_none_or(|(nd, _)| d < nd) {
                nearest = Some((d, tri));
            }
        }
    }
    let (t, tri) = nearest?;
    let world = tri.map(|v| gt.transform_point(v));
    let normal = (world[1] - world[0])
        .cross(world[2] - world[0])
        .normalize_or_zero();
    Some(RayHit {
        point: origin + dir * t,
        normal,
        distance: t,
    })
}

/// Ray–triangle intersection (Möller–Trumbore), **two-sided**, returning `t ≥ 0` along `dir`
/// (unnormalized is fine — `t` is in `dir` lengths) or `None` on miss/parallel. Two-sided because a
/// posed mesh can present back faces at silhouette edges and a generous pick beats a strict one.
pub(crate) fn ray_triangle(origin: Vec3, dir: Vec3, tri: &[Vec3; 3]) -> Option<f32> {
    let (e1, e2) = (tri[1] - tri[0], tri[2] - tri[0]);
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-8 {
        return None;
    }
    let inv = 1.0 / det;
    let s = origin - tri[0];
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t >= 0.0).then_some(t)
}

/// A mesh's model-local [`Aabb`] transformed into a world-space axis-aligned box `(min, max)` — its 8
/// corners run through the entity's world transform, then min/max'd. (An AABB of the rotated box: a hair
/// larger than the true oriented box, which only makes the hover more forgiving.)
pub(crate) fn world_aabb(aabb: &Aabb, gt: &GlobalTransform) -> (Vec3, Vec3) {
    let center = Vec3::from(aabb.center);
    let he = Vec3::from(aabb.half_extents);
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for sx in [-1.0, 1.0] {
        for sy in [-1.0, 1.0] {
            for sz in [-1.0, 1.0] {
                let world = gt.transform_point(center + he * Vec3::new(sx, sy, sz));
                min = min.min(world);
                max = max.max(world);
            }
        }
    }
    (min, max)
}

/// Ray vs axis-aligned box (the slab test): the entry distance along `dir` if the ray hits, else `None`.
/// `dir` need not be normalized; a zero component is handled by the infinities `recip` produces.
pub(crate) fn ray_aabb(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = dir.recip();
    let t1 = (min - origin) * inv;
    let t2 = (max - origin) * inv;
    let tmin = t1.min(t2).max_element();
    let tmax = t1.max(t2).min_element();
    (tmax >= tmin.max(0.0)).then_some(tmin.max(0.0))
}

/// The **dev chord + `I`** (for *inspect*) arms/disarms the inspector — off the bare-letter plane the
/// game's own bindings own (0585), on whichever plane this OS leaves free (0867). Unmistakable as a
/// chord, so unlike the old bare `i` it needs no chat-bar/EditBox gate.
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

/// The inspector's GameObject collision readout (decision 0763): does a hull exist, is it disabled
/// right now, and what stored state does the passability gate see. Named so the bundled `stores`
/// param stays readable.
type GoCollisionReadout = (
    Has<avian3d::prelude::Collider>,
    Has<avian3d::prelude::ColliderDisabled>,
    Option<&'static crate::go_anim::GoAnim>,
);

/// The inspector's entity LIGHT readout (decision 0776): the lane this object's parts render
/// under, and which attach found the room — "two identical GameObjects a few yards apart, one lit
/// like the room and one like the street" is the report that made this a card line rather than a
/// rebuild with `WOW_INTERIOR_LOG`.
type EntityLightReadout = (
    &'static crate::interior::InteriorAnchor,
    Has<crate::interior::ContainmentAttach>,
);

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
    stores: (
        Query<&ObjectStore>,
        Query<&crate::net::NetEntity>,
        // The GameObject collision readout (decision 0763): whether a hull exists, whether it is
        // currently disabled, and the client's stored state the gate reads.
        Query<GoCollisionReadout>,
        Query<EntityLightReadout>,
    ),
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
                    ui.label(
                        egui::RichText::new(format!("inspect · {DEV_CHORD}+I to exit")).small(),
                    );
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
    let (stores, kinds, collision, lit) = (&stores.0, &stores.1, &stores.2, &stores.3);
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
    // The GameObject collision + state readout (decision 0763) — the line that closes the loop on
    // "this door is drawn open but I can't walk through it". It answers, for the object under the
    // cursor, the three facts the passability gate turns on: the client's stored `GAMEOBJECT_STATE`
    // (`go_anim::go_state`, i.e. what we believe open/closed is), whether a collision hull exists at
    // all, and whether it is currently disabled. A door reading `state 0 open · SOLID` is the bug
    // live on screen; `state 0 open · passable` says the gate ran and something else is blocking.
    let go_line = store
        .filter(|_| kind == Some(benilla_protocol::EntityKind::GameObject))
        .map(|s| {
            let (has_hull, disabled, anim) = net_entity
                .and_then(|p| collision.get(p).ok())
                .unwrap_or((false, false, None));
            let state = crate::go_anim::go_state(anim, s);
            let word = match state {
                0 => "open(ACTIVE)",
                1 => "closed(READY)",
                2 => "alt(DESTROYED)",
                _ => "?",
            };
            let solidity = match (has_hull, disabled) {
                (false, _) => "no hull",
                (true, true) => "passable",
                (true, false) => "SOLID",
            };
            let flags = s.0.gameobject_flags();
            let mut named = Vec::new();
            for (bit, name) in [
                (0x1, "IN_USE"),
                (0x2, "LOCKED"),
                (0x4, "INTERACT_COND"),
                (0x10, "NO_INTERACT"),
            ] {
                if flags & bit != 0 {
                    named.push(name);
                }
            }
            let flag_text = if named.is_empty() {
                String::new()
            } else {
                format!(" [{}]", named.join("|"))
            };
            format!(
                "go type {} · state {state} {word} · {solidity} · flags {flags:#x}{flag_text}",
                s.0.gameobject_type_id()
            )
        });
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
        // The base slot's live playback rate (decision 0903) — `speed / (moveSpeed · modelScale)`
        // on a locomotion clip, a flat 1× on everything else. Shown so a "its walk is too fast"
        // report is a hover away from a number instead of a hand-worked divisor; suppressed at
        // exactly 1× so the ordinary case doesn't carry noise.
        let rate = d.rate();
        let rate = if (rate - 1.0).abs() > 1e-3 {
            format!(" · rate {rate:.2}×")
        } else {
            String::new()
        };
        match overlay {
            Some(o) => format!("anim {base} + overlay {}{rate}", fmt(o)),
            None => format!("anim {base}{rate}"),
        }
    });
    // The light lane + the attach that found it (decision 0776). Absent until the classifier has
    // resolved the anchor once — a freshly streamed object shows no line rather than a wrong one.
    let light_line = net_entity
        .and_then(|p| lit.get(p).ok())
        .map(|(anchor, containment)| {
            format!(
                "light {} · {} attach",
                anchor.law_label(),
                if containment {
                    "containment"
                } else {
                    "down-ray"
                }
            )
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
    if let Some(line) = &go_line {
        lines.push(line.clone());
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
    if let Some(line) = &light_line {
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
                    if let Some(line) = &go_line {
                        ui.label(egui::RichText::new(line).color(OVERLAY_TEXT_DIM));
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
                    if let Some(line) = &light_line {
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
                (toggle_inspect, update_mouseover)
                    .chain()
                    .after(crate::ui_script::UiInput),
            )
            // Always recording (messages persist two frames — no ordering constraint needed).
            .add_systems(Update, journal::record_casts)
            .add_systems(EguiPrimaryContextPass, (inspect_ui, journal::journal_ui));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_formats::RenderSubmesh;

    const TRI: [Vec3; 3] = [
        Vec3::new(-1.0, 0.0, -1.0),
        Vec3::new(1.0, 0.0, -1.0),
        Vec3::new(0.0, 0.0, 1.0),
    ];

    #[test]
    fn ray_triangle_hits_at_the_right_distance() {
        let t = ray_triangle(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, &TRI).expect("hit");
        assert!((t - 5.0).abs() < 1e-5);
    }

    #[test]
    fn ray_triangle_is_two_sided() {
        // Same triangle hit from below (reversed winding relative to the ray) still connects.
        assert!(ray_triangle(Vec3::new(0.0, -5.0, 0.0), Vec3::Y, &TRI).is_some());
    }

    #[test]
    fn ray_triangle_misses_outside_and_behind() {
        // Outside the triangle's extent.
        assert!(ray_triangle(Vec3::new(3.0, 5.0, 0.0), Vec3::NEG_Y, &TRI).is_none());
        // Triangle behind the ray origin (t < 0).
        assert!(ray_triangle(Vec3::new(0.0, 5.0, 0.0), Vec3::Y, &TRI).is_none());
        // Parallel to the plane.
        assert!(ray_triangle(Vec3::new(0.0, 5.0, 0.0), Vec3::X, &TRI).is_none());
    }

    /// [`TRI`]'s pre-image under `wow_to_bevy` (`bevy = (−y, z, −x)` ⇒ `wow = (−bz, −bx, by)`):
    /// a submesh authored with THESE WoW-space positions must be picked exactly where the render
    /// form draws it, i.e. at [`TRI`] in model-local Bevy space.
    fn wow_tri() -> RenderSubmesh {
        RenderSubmesh {
            positions: vec![[1.0, 1.0, 0.0], [1.0, -1.0, 0.0], [-1.0, 0.0, 0.0]],
            indices: vec![0, 1, 2],
            ..Default::default()
        }
    }

    /// The picker ↔ render-form bake contract (decision 0857): [`ray_pick_mesh`] must test the
    /// resident WoW-axes geometry through the SAME `wow_to_bevy` bake `submesh_to_static_mesh`
    /// builds the drawn mesh with, under the part's world transform — the render mesh itself is
    /// `RENDER_WORLD`-only (0834), so this path is the only thing keeping static models pickable.
    #[test]
    fn resident_geometry_picks_where_the_render_form_draws() {
        let mesh = PickMesh(std::sync::Arc::new(wow_tri()));
        // A translated + uniformly scaled part: the triangle spans x∈[8,12], z∈[-2,2] at y=1.
        let gt =
            GlobalTransform::from(Transform::from_xyz(10.0, 1.0, 0.0).with_scale(Vec3::splat(2.0)));
        let hit = ray_pick_mesh(&mesh, &gt, Vec3::new(10.0, 6.0, 0.0), Vec3::NEG_Y)
            .expect("the baked triangle must be hit under its world transform");
        assert!((hit.distance - 5.0).abs() < 1e-4);
        assert!(hit.point.abs_diff_eq(Vec3::new(10.0, 1.0, 0.0), 1e-4));
        assert!(hit.normal.abs_diff_eq(Vec3::Y, 1e-4) || hit.normal.abs_diff_eq(-Vec3::Y, 1e-4));
        // …and a miss stays a miss.
        assert!(ray_pick_mesh(&mesh, &gt, Vec3::new(20.0, 6.0, 0.0), Vec3::NEG_Y).is_none());
    }

    /// A billboard card's render form is centred at its pivot (`build_submesh_mesh`); the pick must
    /// subtract the same pivot or it tests the card a whole pivot-offset away from where it draws.
    #[test]
    fn billboard_card_picks_pivot_centred() {
        let mut sub = wow_tri();
        // Pivot at WoW (0, 0, 3) → Bevy (0, 3, 0): the baked card shifts down 3 in model-local y.
        sub.billboard = Some(benilla_formats::Billboard {
            pivot: [0.0, 0.0, 3.0],
            bone: 0,
            kind: benilla_formats::BillboardKind::Spherical,
            scale_anim: None,
            seq_translations: Vec::new(),
        });
        let mesh = PickMesh(std::sync::Arc::new(sub));
        // The card entity sits AT the pivot's world spot (the spawn sites bake
        // `transform_point(pivot)` into its translation), so the authored triangle — 3 below the
        // pivot in model-local Bevy y — draws at world y = 0: a straight-down ray from y = 8 hits
        // at distance 8. Without the pivot subtraction it would (wrongly) hit at y = 3.
        let gt = GlobalTransform::from(Transform::from_xyz(0.0, 3.0, 0.0));
        let hit = ray_pick_mesh(&mesh, &gt, Vec3::new(0.0, 8.0, 0.0), Vec3::NEG_Y)
            .expect("the pivot-centred card must be hit where it draws");
        assert!((hit.distance - 8.0).abs() < 1e-4);
        assert!(hit.point.abs_diff_eq(Vec3::ZERO, 1e-4));
    }
}
