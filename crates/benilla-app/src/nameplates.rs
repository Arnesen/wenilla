//! **Overhead unit names** — the 1.12.1 overhead-name system, three §5 verdicts deep (wow-re
//! `object-layer/scratch/overhead-name.md` + `playername/scratch/name-render-geometry-law.md`,
//! incl. the `ee2ea7af` color/occlusion corrections the director's reference A/B forced):
//!
//! - **World-pass geometry, depth-tested.** The name batch is created depth-test + depth-write ON
//!   (`0x6c7470 → 0x5c1d60(1,1)`; flush `0x5c8b70` → `GxRsSet(GL_DEPTH_TEST, 1)`) and drawn inside
//!   the world/model 3-D pass — **walls occlude names**. (The floating combat text batch is
//!   created `(0,0)`: an overlay, through walls — our combat text stays as-is.) So a name here is
//!   a real **world billboard mesh**: glyph quads in name-local pitch units, camera-facing,
//!   world-scaled — occlusion, the distance shrink, and exact centering all fall out of the world
//!   pass. Named divergence: `AlphaMode::Blend` depth-tests but doesn't depth-write; name-vs-name
//!   overlap resolves by transparent-pass sorting instead — visually equivalent.
//! - **Reaction-colored** (the A/B falsified the earlier "constant white"): the render `0x6c6e90`
//!   fetches `unit->vtable[0x2c]` = `CGUnit::GetSelectionCircleColor 0x605960` — the SAME selector
//!   as the ground selection ring. NPC: dead gray, else reaction red/orange/yellow/green; player:
//!   hostile red, else the `¬X∧¬Y` split — PvP-flagged green (party pale-green), unflagged the
//!   soft blue (party pale-blue). benilla does not re-derive any of that: it calls the ring's own
//!   [`ring_variant`] and paints what it answers, because "the SAME selector" above is the literal
//!   claim (decision 0659 — a hand-copied mirror had drifted and a flagged player wore a green
//!   ring under a blue name). The selector's **first-priority branch** — the combat-flash
//!   red↔orange pulse — is live too: [`CombatFlash`] (recomputed per frame from the selection +
//!   our server-echoed attack bracket, exactly the client's per-frame `[unit+0xc58]` bit 0x10)
//!   overrides the whole palette while we melee this unit.
//! - **Anchor**: the posed PlayerName attachment ([`overhead_anchor`]), re-read **every frame in
//!   lockstep with the mesh pose, no smoothing** (byte-verified: the client memoizes the pose per
//!   model generation; our per-frame joint read matches).
//! - **Scale**: `d = anchor.z − feet.z`, `scale = d > 4 ? (d/4)·1.5·0.2 : 0.2` — unit-HEIGHT
//!   scaling (taller model, bigger name; humanoids all at the 0.2 floor). Pitch : size is 1:1 at
//!   the desc level (verified).
//! - **Seat** (the world-mode vertical law, §5-verified after the director's "names too low"
//!   falsified the first reading): `drawPos.z = anchor.z + lineCount·scale` is the **TOP line's
//!   BASELINE** and the block **hangs DOWN** from it — line `i`'s baseline at
//!   `anchor.z + (lineCount − i)·scale` (`string_layout_lines 0x5cdc20` rotated branch: line 0
//!   seeded at local-Y 0, one pitch subtracted per line; sign pinned by the raid-marker
//!   cross-check). So the whole block sits ABOVE the attachment — never "bottom at the anchor",
//!   which sat every name one line-height too low. Each line's baseline lands on that grid via
//!   the face's own ascent fraction ([`UiFontAtlas::ascent_ratio`], the `[0x17c]` load_param); the
//!   sub-pitch ascent/descent extents inside the top/bottom lines remain the gx-boundary
//!   INFERRED residual (a director A/B pins the exact pixels).
//! - **Show gate** (`ShouldShowName 0x6070a0`): own unit → `UnitNameOwn` (binary default OFF,
//!   checked before the rescue; benilla defaults **ON** — the director's directive — plus a
//!   benilla-side fade gate: the own name hides with the fully-faded first-person avatar, our
//!   reading of the gate's INFERRED `vtable+0x58` can-show leg); the **current TARGET shows
//!   regardless of cvars** — the rescue global `[0xb4e2d8]` is the *selection*, pinned by the
//!   combat-flash RE; the earlier "mouseover" reading of the same global is CORRECTED here; a
//!   **dead creature shows only via the target rescue** (director-verified on the reference; the
//!   byte leg is a flagged pin); players → `UnitNamePlayer` (ON); NPCs → `UnitNameNPC` (binary
//!   default OFF; benilla defaults **ON** — the director's directive; the reference install's
//!   effective default is a flagged open).
//! - **Lines** (`0x608f50`): NPC = name + `<Subname>` (an empty wire subname is no line); player =
//!   [flag prefixes +] name (guild/PVP-title lines wait on data that doesn't stream yet). The
//!   prefixes are the a1–a3 vtable-slot decorations of the line stack (`+0x7c/+0x80/+0x84`),
//!   glued straight onto the name by the `%s%s…` assembly — `<AFK>`/`<DND>`/`<GM>` in that slot
//!   order from `PLAYER_FLAGS` (see [`flag_prefix`]; byte-verified, wow-re Q4a). The trio is
//!   unique to the overhead name binary-wide (`CHAT_FLAG_GM`'s only xref is the a3 slot).

use std::collections::HashMap;

use bevy::prelude::*;

use benilla_protocol::EntityKind;
use benilla_ui::script::{JustifyH, JustifyV, Outline};

use crate::entities::{overhead_anchor, BoneAttach, OverheadFallback};
use crate::names::NameCache;
use crate::net::{Guid, NetCommands, NetEntity, ObjectStore, Reputations, SelfPlayer};
use crate::target::{ring_reaction, ring_variant, CombatFlash, Factions, RingVariant, Selection};
use crate::ui_text::{layout_text_quads, FontSpec, Justify, UiFontAtlas};
use benilla_world::view::WorldCamera;

/// The height-scale law (`0x6c6e90`): `d > KNEE ? d/KNEE · RATE · FLOOR : FLOOR`, `d` the anchor's
/// height above the feet in world units.
const SCALE_FLOOR: f32 = 0.2; // [0x80679c]
const SCALE_KNEE: f32 = 4.0; // [0x8112a8]
const SCALE_RATE: f32 = 1.5; // [0x8112ac]

/// benilla's nameplate config (the client's `0xce8720` cvar mask, reduced to what streams today),
/// player-settable since 0992: 1.12's own `UnitNamePlayer`/`UnitNameNPC`/`UnitNameOwn` CVars over
/// these gates (the Options window's Nameplates page, through the 0954 store — the arms live in
/// [`crate::cvars`]). Defaults: `npc` and `own` are ON by the director's directives ("figure out
/// text name over NPCs"; "we should show our name", 2026-07-12) — the byte-verified binary
/// defaults are both "0"; `player` ON is the byte-verified default.
#[derive(Resource, Clone, Copy)]
pub(crate) struct NameConfig {
    pub(crate) player: bool,
    pub(crate) npc: bool,
    pub(crate) own: bool,
}

impl Default for NameConfig {
    fn default() -> Self {
        Self {
            player: true,
            npc: true,
            own: true,
        }
    }
}

/// The player name-line flag decorations — the a1–a3 prefix slots of the line-stack law
/// (`0x608f50`), byte-VERIFIED (wow-re `overhead-name.md` Q4a, commit `4a75d9be`): the CGPlayer
/// vtable overrides at `+0x7c/+0x80/+0x84` (`0x5ec9e0/0x5eca40/0x5eca80`) each test one
/// `PLAYER_FLAGS` bit and resolve one GlobalStrings key — slot order **AFK (0x2) → DND (0x4) →
/// GM (0x8)**, all set flags stack, bare `%s%s…` concatenation before the name (`<GM>One`, no
/// space). The base CGUnit slots are `ret 8` stubs — players only, NPCs never decorate.
/// `PLAYER_FLAGS` is field 190, `UF_FLAG_PUBLIC` (streams for every player in range);
/// vmangos sets the bits at `/afk`, `/dnd`, `.gm on` (`Player.h:316–318`). The tag strings are
/// the reference install's own `CHAT_FLAG_AFK/DND/GM` values (patch-2 `GlobalStrings.lua:534–536`
/// — the exe holds only the keys). Not built: the local player's client-side `/afk` echo
/// (`[0xb6e5cc]`, role INFERRED) — the wire flag round-trips fast enough.
const FLAG_PREFIXES: [(u32, &str); 3] = [(0x2, "<AFK>"), (0x4, "<DND>"), (0x8, "<GM>")];

/// The concatenated name-line prefix for a player's `PLAYER_FLAGS` — empty for the common
/// unflagged case (no allocation beyond the empty `String`).
fn flag_prefix(player_flags: u32) -> String {
    FLAG_PREFIXES
        .iter()
        .filter(|(bit, _)| player_flags & bit != 0)
        .map(|(_, s)| *s)
        .collect()
}

/// The name's world scale for a unit whose overhead anchor sits `d` world units above its feet.
/// Shared with the raid-target marker ([`crate::raid_marks`]) — §6 of the same geometry law.
pub(crate) fn height_scale(d: f32) -> f32 {
    if d > SCALE_KNEE {
        (d / SCALE_KNEE) * SCALE_RATE * SCALE_FLOOR
    } else {
        SCALE_FLOOR
    }
}

/// What a name line is painted with. The classification is **not** ours: the per-frame name render
/// fetches `unit->vtable[0x2c]` = `CGUnit::GetSelectionCircleColor 0x605960` — literally the same
/// selector as the ground ring (decision 0156) — so [`ring_variant`] IS the law and this only adds
/// the flash seat that lives outside the palette.
///
/// It used to be a hand-copied mirror of that selector, and the copy went stale: decision 0453
/// taught the ring the player path's `¬X∧¬Y` legs (PvP-flagged → green, party → pale) and the
/// mirror kept answering plain blue, so a flagged player drew a green ring under a blue name
/// (decision 0659). Deleting the mirror is the fix; there is one selector now.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum NamePaint {
    /// The selector's own answer — the whole palette.
    Variant(RingVariant),
    /// The combat-flash pulse — the selector's first-priority branch, which outranks the palette.
    /// Its material tint is rewritten each frame from [`CombatFlash::color`]; this is the seed.
    Flash,
}

impl NamePaint {
    // GAMMA LANE (0161): `linear_rgb` passes the authored byte values RAW into the gamma
    // framebuffer (stock-PBR unlit writes base_color as-is; the frame decodes once at FFXGlow).
    fn color(self) -> Color {
        match self {
            Self::Variant(v) => v.color(),
            Self::Flash => Color::linear_rgb(1.0, 0.0, 0.0), // seed: the wave's G=0 endpoint
        }
    }
}

/// The nameplate render caches: one unlit depth-tested material per palette color (built once the
/// atlas exists), one mesh per distinct line stack (every "Young Wolf" shares one mesh), and the
/// live per-unit plate entities.
#[derive(Resource, Default)]
pub(crate) struct Nameplates {
    materials: HashMap<NamePaint, Handle<StandardMaterial>>,
    meshes: HashMap<Vec<String>, Handle<Mesh>>,
    /// unit → (plate entity, the (lines, color) it was built with).
    live: bevy::ecs::entity::EntityHashMap<(Entity, Vec<String>, NamePaint)>,
}

impl Nameplates {
    /// Whether `unit` currently shows an overhead name — benilla's equivalent of the client's
    /// per-unit name object (`unit+0xc7c`) being live. The questgiver marker keys its raised
    /// (anim 190) bob off this (wow-re `questgiver-marker.md` Q4: `0x6076c0` checks `0x6c7950`).
    pub(crate) fn shows(&self, unit: Entity) -> bool {
        self.live.contains_key(&unit)
    }

    /// The line count of `unit`'s live overhead name (`None` = no name this frame) — the raid
    /// marker's seat law reads it (§6: the marker sits one pitch above the block when the name
    /// shows, at the bare anchor otherwise).
    pub(crate) fn line_count(&self, unit: Entity) -> Option<usize> {
        self.live.get(&unit).map(|(_, lines, _)| lines.len())
    }
}

/// Marker on a plate entity (a world-pass billboard mesh, root-level, following its unit).
#[derive(Component)]
struct NamePlate;

/// Mounted name-rock damping — a **named modernization** (director taste, ref-A/B'd: the
/// reference name rides the gallop too, so the raw per-frame seat is the faithful behavior;
/// ours read "a bit more intense", so the mounted plate shows only this fraction of the seat's
/// oscillation about its slow-tracked mean). `1.0` = the raw faithful seat. On-foot plates are
/// untouched.
const ROCK_KEEP: f32 = 0.7;
/// The mean tracker's exponential rate (1/s) — slow against the ~1 Hz gallop (the oscillation
/// stays in the damped residual, not the mean), fast enough to follow a stance drift.
const ROCK_MEAN_RATE: f32 = 1.5;
/// A residual this large (yd) is a stance change (mount swap, teleport), not gait — snap the
/// mean to it instead of easing through seconds of misplaced name.
const ROCK_SNAP: f32 = 1.0;

/// The shaped-glyph source size (px) for the mesh bake — a baked atlas size (crisp cells); only
/// the cell UVs and the relative geometry survive the normalization to pitch units.
const BAKE_PX: f32 = 36.0;

/// Build one name-block mesh in **name-local pitch units**: x centered on 0 (exact centering for
/// any name length — by construction), y up, one line = 1.0 of pitch, line 0 (the name) on TOP,
/// each line's **BASELINE at local y = lineCount − i** — so a transform at the anchor with
/// scale = the world `scale` puts the top baseline exactly at the byte law's
/// `anchor + lineCount × scale` and the block hangs below it (the §5 world-mode seat; the old
/// bottom-at-anchor bake sat everything one line too low). Glyphs come from the real layout at
/// [`BAKE_PX`], normalized by the pitch (= the font size, the verified 1:1); the baseline sits
/// [`UiFontAtlas::ascent_ratio`] into the layout's cell, the same `[0x17c]` load_param seat the
/// 2-D path uses.
fn build_name_mesh(atlas: &mut UiFontAtlas, lines: &[String]) -> Mesh {
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::{Indices, PrimitiveTopology};

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let n = lines.len() as f32;
    // Where the baseline sits inside the layout's one-pitch cell (the same `round(size ·
    // asc/(asc+|desc|))` load_param the 2-D path seats with) — needed to hang each line FROM its
    // baseline. Invariant to the ratio's value: `line_top` shifts with it but the baseline stays
    // pinned to the byte grid, so this only has to AGREE with `layout_text_quads`, not be exact.
    let baseline_frac = ((f64::from(BAKE_PX) * f64::from(atlas.ascent_ratio(None)) + 0.5).floor()
        / f64::from(BAKE_PX)) as f32;
    for (i, line) in lines.iter().enumerate() {
        let glyphs = layout_text_quads(
            atlas,
            line,
            Rect::from_center_size(Vec2::ZERO, Vec2::ZERO),
            [1.0, 1.0, 1.0, 1.0], // the material tints; glyph alpha rides the texture
            Justify {
                h: JustifyH::Center,
                v: JustifyV::Middle,
            },
            0,
            FontSpec {
                path: None, // UNIT_NAME_FONT = Friz Quadrata, the atlas default
                height: Some(BAKE_PX),
                outline: Outline::None,
                paint_halo: true,
                alpha_gradient: None,
            },
        );
        // Recenter the ink box on x = 0, then normalize px → pitch units, flipping y-down px into
        // y-up locals within this line's band.
        let Some(bounds) = glyphs.iter().map(|q| q.rect).reduce(|a, b| a.union(b)) else {
            continue;
        };
        let cx = (bounds.min.x + bounds.max.x) * 0.5;
        // The seat: line `i`'s BASELINE must land at local y = n − i (the byte grid). The layout
        // (degenerate point rect → top placement at px y = 0) puts the baseline `baseline_frac`
        // of a cell below the cell top, so the cell top maps to n − i + baseline_frac.
        let line_top = n - i as f32 + baseline_frac;
        for q in &glyphs {
            let x0 = (q.rect.min.x - cx) / BAKE_PX;
            let x1 = (q.rect.max.x - cx) / BAKE_PX;
            let y0 = line_top - q.rect.min.y / BAKE_PX; // px top → local (higher)
            let y1 = line_top - q.rect.max.y / BAKE_PX; // px bottom → local (lower)
            let base = positions.len() as u32;
            positions.extend([[x0, y0, 0.0], [x1, y0, 0.0], [x1, y1, 0.0], [x0, y1, 0.0]]);
            let [tl, tr, br, bl] = q.uv.corners;
            uvs.extend([tl, tr, br, bl]);
            normals.extend([[0.0, 0.0, 1.0]; 4]);
            indices.extend([base, base + 2, base + 1, base, base + 3, base + 2]);
        }
    }
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Drive the plates: gate → resolve lines + color → (re)build the plate entity on change. Runs in
/// Update after the camera controller ([`WorldStage::Input`]); all entity/mesh churn stays in
/// Update (the schedule the mesh pipelines support). The per-frame *placement* is
/// [`place_nameplates`] (PostUpdate, off this frame's propagated pose) — a fresh plate spawned
/// here gets its first seat there, same frame (Update commands flush before PostUpdate).
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // one Bevy system's full input set
pub(crate) fn drive_nameplates(
    mut commands: Commands,
    units: Query<(
        Entity,
        &NetEntity,
        &Guid,
        &Transform,
        Option<&ObjectStore>,
        Has<SelfPlayer>,
        // Whether the body is drawn at all — the ShouldShowName gate's unwritten first term
        // (decision 1277). A body the exterior-scene election sent to pass 2 never enters the
        // reference's scene, so there is nothing over which to float a name.
        Option<&InheritedVisibility>,
    )>,
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    // The show-gate inputs (one tuple param — Bevy's 16-param ceiling).
    gates: (
        Res<Selection>,
        Res<CombatFlash>,
        Option<Res<crate::player::CameraControl>>,
        Res<crate::vplates::VPlates>,
        Res<crate::chat_bubble::BubblesActive>,
        // The selector's party roster (`0xbc6f48`) — the `¬X∧¬Y` leg's pale variants.
        Res<crate::ui_party::GroupState>,
        // The UnitName* cvar mask (0992) — the kind gates below read it.
        Res<NameConfig>,
    ),
    mut names: ResMut<NameCache>,
    net_commands: Res<NetCommands>,
    factions: Option<Res<Factions>>,
    reputations: Res<Reputations>,
    camera: Query<&Transform, With<WorldCamera>>,
    mut atlas: Option<ResMut<UiFontAtlas>>,
    mut plates: ResMut<Nameplates>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    // The overhead-anchor inputs ([`overhead_anchor`]) — the spawn-frame seat only.
    anchor_q: (
        Query<&BoneAttach>,
        Query<&OverheadFallback>,
        Query<&GlobalTransform>,
        Query<(), With<crate::entities::mount::MountChild>>,
    ),
) {
    let (selection, flash, rig, vplates, bubbles, group, name_cfg) = gates;
    let (Ok(cam_tf), Some(atlas)) = (camera.single(), atlas.as_mut()) else {
        return;
    };
    // The palette materials, once (they need the atlas image).
    if plates.materials.is_empty() {
        let image = atlas.image();
        for kind in RingVariant::ALL
            .map(NamePaint::Variant)
            .into_iter()
            .chain([NamePaint::Flash])
        {
            plates.materials.insert(
                kind,
                materials.add(StandardMaterial {
                    base_color: kind.color(),
                    base_color_texture: Some(image.clone()),
                    unlit: true,
                    // Depth-TESTED (the verified world-pass state — walls occlude); Blend skips
                    // the depth WRITE (the named divergence: name-vs-name resolves by sorting).
                    alpha_mode: AlphaMode::Blend,
                    cull_mode: None,
                    // Sort names AFTER every ordinary transparent (Bevy's Transparent3d key is
                    // `view-z of the mesh center + depth_bias`, sorted ASCENDING = far first, so
                    // a POSITIVE bias draws last = on top — the sign law is in `sky_order`).
                    // Without it a WATER chunk whose center sat nearer the camera than the plate
                    // sorted in front and tinted the name — the "name looks underwater from some
                    // angles" artifact (director-reported, 2026-07-18; decision 0519). With the
                    // sign inverted, as 0519 shipped it, deep water (alpha 1.0) erased the
                    // glyphs outright — the canal report of 2026-07-25 (decision 0639). The ref
                    // draws its world text late in the frame, after the liquid — this reproduces
                    // that order; walls still occlude via the depth test, and plate-vs-plate
                    // ordering is unchanged (uniform bias).
                    depth_bias: benilla_world::sky_order::Rung::NAMEPLATE,
                    ..default()
                }),
            );
        }
    }
    // The flash material rides this frame's wave sample — mutated only while a flash is live, so
    // the asset isn't dirtied (re-uploaded) every idle frame.
    if flash.unit.is_some() {
        if let Some(mat) = plates
            .materials
            .get(&NamePaint::Flash)
            .and_then(|h| materials.get_mut(h))
        {
            mat.base_color = flash.color;
        }
    }
    // The billboard facing: screen-aligned (the camera's own rotation), this frame's camera.
    let facing = cam_tf.rotation;
    let mut seen: Vec<Entity> = Vec::new();
    for (entity, net, guid, tf, store, is_self, drawn) in &units {
        // **A name needs a body to float over.** The director, inside Caverns of Time: the Tanaris
        // mobs 222 yd overhead had correctly stopped drawing, and their names went on hanging in
        // the rock — this lane walks every net entity with a `Guid` and never asked whether the
        // unit was in the scene. Ahead of the whole ShouldShowName ladder below, because it is not
        // one of that gate's terms: it is the precondition for asking at all.
        if !drawn.is_none_or(|v| v.get()) {
            continue;
        }
        // The ShouldShowName gate, in the client's own order (own-unit answers its cvar BEFORE
        // the rescue; everyone else: the current-TARGET bypass — `[0xb4e2d8]` is the selection,
        // not the mouseover — then the kind cvar). A unit carrying a V-key nameplate never also
        // draws its floating name (the plate/name mutual exclusion, wow-re `nameplate-vkey.md`),
        // and neither does one carrying a live chat bubble (`+0xe64` — the same gate's other
        // handle read, wow-re `chat-bubble.md` §0).
        // A DEAD creature's name shows ONLY through the target rescue — director-verified on the
        // reference (a corpse field isn't a name field; targeting the corpse still names it);
        // the exact ShouldShowName leg is on the pin list — flagged interim.
        let show = if vplates.0.contains(&entity) || bubbles.0.contains(&entity) {
            false
        } else if store.is_some_and(|s| s.0.unit_is_ghost_visual()) {
            // The bytes_1 ghost vis-flag's ONE render effect (byte-VERIFIED, wow-re
            // ghost-death-visuals.md): overhead-name/plate suppression — the create-gate and the
            // per-tick gate both test `byte3 & 3` (ghost|creep; the creep leg is the stealth
            // arc's). A released ghost carries no floating name, target rescue included.
            false
        } else if is_self {
            // The own-unit cvar leg, before the rescue (faithful order: self-targeting still
            // obeys it) — plus the fade gate: a fully-faded first-person avatar carries no name
            // (our reading of `ShouldShowName`'s INFERRED `vtable+0x58` can-show leg; a name
            // floating over an invisible body is the alternative).
            name_cfg.own
                && rig
                    .as_deref()
                    .map_or(1.0, crate::player::CameraControl::self_fade)
                    > 0.0
        } else if selection.target == Some(entity) {
            true
        } else if net.kind == EntityKind::Unit && store.is_some_and(|s| s.0.unit_is_dead()) {
            false
        } else {
            match net.kind {
                EntityKind::Player => name_cfg.player,
                EntityKind::Unit => name_cfg.npc,
                _ => false,
            }
        };
        if !show {
            continue;
        }
        let Some(name) = names.resolve(guid.0, &net_commands).map(str::to_owned) else {
            continue;
        };
        // The player flag decorations (a1–a3): glued straight onto the name line, no space.
        let name = if net.kind == EntityKind::Player {
            let prefix = flag_prefix(store.map_or(0, |s| s.0.player_flags()));
            if prefix.is_empty() {
                name
            } else {
                format!("{prefix}{name}")
            }
        } else {
            name
        };
        let mut lines = vec![name];
        if net.kind == EntityKind::Unit {
            if let Some(sub) = benilla_protocol::guid::entry(guid.0)
                .and_then(|e| names.creature_subname(e))
                // vmangos ships "" (present-but-empty) for most templates — an empty wire
                // subname is NO subname, never an empty `<>` line.
                .filter(|s| !s.trim().is_empty())
            {
                lines.push(format!("<{sub}>"));
            }
        }
        // The color: the ring's byte-verified rank + the shared palette selector.
        let rank = ring_reaction(
            factions.as_deref(),
            &reputations,
            store,
            self_store.single().ok(),
        );
        let is_dead = store.is_some_and(|s| s.0.unit_is_dead());
        // The player path's `¬X∧¬Y` split inputs, exactly as the ring reads them: the unit's own
        // PvP flag (`UNIT_FIELD_FLAGS` 0x1000) and roster membership by guid — here keyed on THIS
        // unit's guid, where the ring (which only ever draws the selection) uses the selected one.
        let pvp = store.is_some_and(|s| s.0.unit_flags() & 0x1000 != 0);
        let in_party = group.members.iter().any(|m| m.guid == guid.0);
        // The selector's first-priority branch (shared with the ring): the combat flash
        // overrides the whole palette while we melee this unit.
        let color = if flash.unit == Some(entity) {
            NamePaint::Flash
        } else {
            NamePaint::Variant(ring_variant(
                rank,
                net.kind == EntityKind::Player,
                is_dead,
                pvp,
                in_party,
            ))
        };

        // (Re)build the plate entity when its content changed. The seat below is only the
        // spawn-frame placement — [`place_nameplates`] re-seats every plate each frame from the
        // propagated (same-frame) pose.
        let anchor = overhead_anchor(
            entity,
            tf,
            &anchor_q.0,
            &anchor_q.1,
            &anchor_q.2,
            &anchor_q.3,
        );
        let scale = height_scale(anchor.y - tf.translation.y);
        let place = Transform {
            translation: anchor,
            rotation: facing,
            scale: Vec3::splat(scale),
        };
        seen.push(entity);
        match plates.live.get(&entity) {
            Some((_, l, c)) if *l == lines && *c == color => {}
            stale => {
                if let Some((old, _, _)) = stale {
                    let old = *old;
                    if let Ok(mut e) = commands.get_entity(old) {
                        e.despawn();
                    }
                }
                debug!("nameplates: rebuild {entity} -> {lines:?} ({color:?})");
                let mesh = plates
                    .meshes
                    .entry(lines.clone())
                    .or_insert_with(|| meshes.add(build_name_mesh(atlas, &lines)))
                    .clone();
                let material = plates.materials[&color].clone();
                let plate = commands
                    .spawn((Mesh3d(mesh), MeshMaterial3d(material), place, NamePlate))
                    .id();
                plates.live.insert(entity, (plate, lines, color));
            }
        }
    }
    // Plates whose unit despawned or gated off this frame.
    plates.live.retain(|unit, (plate, _, _)| {
        if seen.contains(unit) {
            true
        } else {
            if let Ok(mut e) = commands.get_entity(*plate) {
                e.despawn();
            }
            false
        }
    });
}

/// Seat every live plate from THIS frame's propagated pose: anchor off the posed joints (fresh
/// after `TransformSystems::Propagate`), camera-facing rotation, height scale — written to both
/// `Transform` and `GlobalTransform` (plates are root entities, so the direct global write is
/// exact; propagation already ran this frame). Running in Update read last-frame joint globals,
/// so a moving player's name trailed a frame behind and snapped forward on stop — the director's
/// "lags behind and snaps back". Ordered before `CheckVisibility` so culling sees the fresh seat.
#[allow(clippy::type_complexity)] // one Bevy system's full input set
fn place_nameplates(
    plates: Res<Nameplates>,
    camera: Query<&Transform, (With<WorldCamera>, Without<NamePlate>)>,
    units: Query<&Transform, (Without<NamePlate>, Without<WorldCamera>)>,
    mut plate_tfs: Query<(&mut Transform, &mut GlobalTransform), With<NamePlate>>,
    anchor_q: (
        Query<&BoneAttach>,
        Query<&OverheadFallback>,
        Query<&GlobalTransform, Without<NamePlate>>, // disjoint from the plate global write
        Query<(), With<crate::entities::mount::MountChild>>,
    ),
    trace: (
        Res<NameAnchorTrace>,
        Res<Time>,
        Query<(), With<crate::net::SelfPlayer>>,
    ),
    // unit → the slow mean of (anchor − root) while mounted — the rock-damping state.
    mut rock: Local<bevy::ecs::entity::EntityHashMap<Vec3>>,
) {
    let Ok(cam_tf) = camera.single() else {
        return;
    };
    let facing = cam_tf.rotation;
    let blend = 1.0 - (-ROCK_MEAN_RATE * trace.1.delta_secs()).exp();
    for (&unit, (plate, ..)) in plates.live.iter() {
        let (Ok(tf), Ok((mut ptf, mut pglobal))) = (units.get(unit), plate_tfs.get_mut(*plate))
        else {
            continue; // spawned this frame and not yet flushed, or unit despawning — next frame
        };
        let raw = overhead_anchor(unit, tf, &anchor_q.0, &anchor_q.1, &anchor_q.2, &anchor_q.3);
        // The mounted rock damp: seat the plate at mean + ROCK_KEEP·residual instead of the raw
        // gallop-riding anchor. Everything is in root-relative offsets, so the damped plate still
        // tracks a 7 yd/s run with zero world-space lag (the on-foot trailing lesson).
        let anchor = if anchor_q.3.contains(unit) {
            let off = raw - tf.translation;
            let mean = rock.entry(unit).or_insert(off);
            if (off - *mean).length_squared() > ROCK_SNAP * ROCK_SNAP {
                *mean = off;
            } else {
                *mean += (off - *mean) * blend;
            }
            tf.translation + *mean + (off - *mean) * ROCK_KEEP
        } else {
            raw
        };
        if trace.0 .0 && trace.2.contains(unit) {
            // The numeric feed for an anchor smoothness question (`NAME_TRACE`): the same-frame
            // root, the SEATED (post-damp) anchor, and the camera — per frame, machine-greppable;
            // `raw` is the undamped joint read for depth.
            let (r, a, c) = (tf.translation, anchor, cam_tf.translation);
            info!(
                "NAME_TRACE t={:.4} root=({:.4},{:.4},{:.4}) anchor=({:.4},{:.4},{:.4}) cam=({:.3},{:.3},{:.3}) raw=({:.4},{:.4},{:.4})",
                trace.1.elapsed_secs(), r.x, r.y, r.z, a.x, a.y, a.z, c.x, c.y, c.z, raw.x, raw.y, raw.z
            );
        }
        let scale = height_scale(anchor.y - tf.translation.y);
        let place = Transform {
            translation: anchor,
            rotation: facing,
            scale: Vec3::splat(scale),
        };
        *ptf = place;
        *pglobal = GlobalTransform::from(place);
    }
    // Dismounted or despawned units drop their damping state (a re-mount starts fresh).
    rock.retain(|e, _| anchor_q.3.contains(*e));
}

/// Registers the plate driver (Update — the entity/mesh churn stays in the schedule the mesh
/// pipelines support) and the per-frame placer (PostUpdate, after transform propagation — the
/// same-frame pose the render draws, before visibility culling).
pub(crate) struct NameplatesPlugin;

/// `WOW_PROBE_NAME_TRACE=1`: per-frame `NAME_TRACE` lines for the self player's plate seat —
/// the numeric instrument for "the name moves weirdly" reports (a smoothness/lag question is
/// measured, never eyeballed — decision 0404).
#[derive(Resource)]
struct NameAnchorTrace(bool);

impl Plugin for NameplatesPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(NameAnchorTrace(
            std::env::var_os("WOW_PROBE_NAME_TRACE").is_some(),
        ))
        .init_resource::<Nameplates>()
        .init_resource::<NameConfig>()
        // After the whole targeting chain (the gate + colour read THIS frame's selection and
        // flash verdicts; the chain is itself after `WorldStage::Input`) and after the V-key
        // plate + chat-bubble drives (the plate/name and bubble/name mutual exclusions read
        // their verdicts).
        .add_systems(
            Update,
            drive_nameplates
                .after(crate::target::TargetUpdate)
                .after(crate::vplates::VPlateSet)
                .after(crate::chat_bubble::BubbleSet),
        )
        .add_systems(Update, evict_name_meshes)
        .add_systems(
            PostUpdate,
            place_nameplates
                .after(bevy::transform::TransformSystems::Propagate)
                .before(bevy::camera::visibility::VisibilitySystems::CheckVisibility),
        );
    }
}

/// Drop the per-line-stack mesh dedup on a cross-map transition (`world_map::MapChange` — see its
/// doc): it grows with every distinct name ever seen and the old map's population never comes
/// back. The palette `materials` stay — a small fixed set, rebuilt only if the atlas changes.
fn evict_name_meshes(
    mut changes: MessageReader<benilla_world::world_map::MapChange>,
    mut plates: ResMut<Nameplates>,
) {
    if changes.is_empty() {
        return;
    }
    changes.clear();
    plates.meshes.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The height-scale law at its pinned points: the 0.2 floor through 4 units, `>` not `>=` at
    /// the knee, `0.075·d` beyond (the byte behavior — the jump at the knee is real).
    #[test]
    fn height_scale_matches_the_byte_law() {
        assert_eq!(height_scale(1.8), 0.2, "human: the floor");
        assert_eq!(height_scale(4.0), 0.2, "knee is > not >=");
        let big = height_scale(8.0);
        assert!((big - 0.6).abs() < 1e-6, "8/4 · 1.5 · 0.2 = 0.6, got {big}");
        assert!(height_scale(4.1) > 0.2, "past the knee: the jump is real");
    }

    /// The flag decorations against the byte-verified slot law (wow-re Q4a): bare concatenation
    /// (no space — the `%s%s…` line-stack law), stacking in slot order AFK → DND → GM, empty for
    /// the common unflagged case, unrelated bits (ghost 0x10, resting 0x20) ignored.
    #[test]
    fn flag_prefix_matches_the_slot_law() {
        assert_eq!(flag_prefix(0), "");
        assert_eq!(flag_prefix(0x8), "<GM>");
        assert_eq!(flag_prefix(0x2), "<AFK>");
        assert_eq!(flag_prefix(0x4), "<DND>");
        assert_eq!(flag_prefix(0x8 | 0x2), "<AFK><GM>", "slot order, a1 first");
        assert_eq!(
            flag_prefix(0x2 | 0x4 | 0x8),
            "<AFK><DND><GM>",
            "all three stack — the client enforces no exclusivity"
        );
        assert_eq!(
            flag_prefix(0x10 | 0x20 | 0x1),
            "",
            "ghost/resting/leader don't decorate"
        );
    }

    /// The name paints itself from the ring's own selector — one law, not a copy of one (decision
    /// 0659). The regression this locks: a **PvP-flagged** friendly player is GREEN, the same
    /// green the ring under their feet draws. The name used to keep a private mirror of the
    /// selector that predated the `¬X∧¬Y` legs (0453), so a flagged player wore a green ring under
    /// a blue name — which is exactly what the director saw.
    #[test]
    fn name_color_is_the_ring_selector_itself() {
        let paint = |rank, is_player, is_dead, pvp, in_party| {
            NamePaint::Variant(ring_variant(rank, is_player, is_dead, pvp, in_party))
        };
        // The regression: flagged ⇒ green, unflagged ⇒ the soft blue, and the two really differ.
        let flagged = paint(6, true, false, true, false);
        let unflagged = paint(6, true, false, false, false);
        assert_eq!(flagged, NamePaint::Variant(RingVariant::Friendly));
        assert_eq!(unflagged, NamePaint::Variant(RingVariant::Player));
        assert_ne!(flagged.color(), unflagged.color());
        assert_eq!(
            flagged.color(),
            Color::linear_rgb(0.0, 1.0, 0.0),
            "0xFF00FF00 — the ring's own green, byte for byte"
        );
        // The party legs come along for free now.
        assert_eq!(
            paint(6, true, false, true, true),
            NamePaint::Variant(RingVariant::PartyPvp)
        );
        assert_eq!(
            paint(6, true, false, false, true),
            NamePaint::Variant(RingVariant::Party)
        );
        // And the rest of the law still holds through the shared selector.
        assert_eq!(paint(0, false, false, false, false).color(), RED);
        assert_eq!(
            paint(6, true, true, false, false),
            NamePaint::Variant(RingVariant::Player),
            "dead player never grays"
        );
        assert_eq!(paint(1, true, false, true, false).color(), RED, "hostile");
        assert_eq!(
            paint(6, false, true, false, false),
            NamePaint::Variant(RingVariant::Dead)
        );
    }

    const RED: Color = Color::linear_rgb(1.0, 0.0, 0.0);
}
