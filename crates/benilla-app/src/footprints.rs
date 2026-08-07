//! **Footprint decals** — the prints a walking unit leaves on snow and sand (B212, decisions
//! 1006/1012): the fourth client of the shared surface-decal projector ([`crate::decal`]), drawn
//! on the shared effect stream like the blob shadow, but **spawn-once**: a print is projected the
//! frame its foot plants and the cached triangles replay every frame until the fade retires it —
//! exactly the reference's own shape (baked once at spawn over collector-gathered ground
//! triangles; the per-frame draw `0x69a3e0` only re-copies with the current alpha).
//!
//! **The mechanism — wow-re §5 cross-checked** (`footprint-decals.md`, folded back in 1012):
//! - **Surface gate**: `TerrainType.Flags & 1` (`0x699eb6`) — set on exactly **Snow** and
//!   **Sand** — resolved under the unit through the same ground-effect → TerrainType chain the
//!   footstep sounds ride ([`benilla_formats::FootstepCatalog::leaves_footprints`]).
//! - **Trigger**: each per-foot animation event tag (`$xL*`/`$xR*`; the same [`AnimSoundEvent`]
//!   stream the sounds read). **`$FSD` is sound-only** — a quadruped whose run authors only
//!   `$FSD` leaves no prints, faithfully. Position = the event record's authored offset through
//!   the live bone matrix ([`BoneAttach::markers`], `0x7196df`); yaw = the unit's facing.
//! - **Ink + size**: `CreatureModelData.FootprintTextureID` → `FootprintTextures.dbc` →
//!   `textures\Footsteps\*` (32×32 pure-black-RGB under soft alpha), sized Length/Width ×(1/36)
//!   inches→yards (`0x5fc310`/`0x607a00`) × the unit's wire `OBJECT_FIELD_SCALE_X` **only**
//!   (`0x469f10` — no display scale multiplies in). `0xFFFFFFFF` = printless (133 of 430
//!   models). A mounted composite prints the MOUNT's ink/dims (the mount model's own event
//!   stream) at the RIDER's scale. **The texture is authored as the LEFT foot** — right-foot
//!   prints mirror (`0x5fc07f`, scale(−1,1,1) before the yaw).
//! - **Fade**: lifetime **6000 ms**, `t = 1 − age/6000`, `alpha = min(127, ⌊255·t⌋)` — ≈3.0 s
//!   hold at ~50 % opacity then a ≈3.0 s linear fade, no fade-in ([`fade`], byte-diffed
//!   `PRIMITIVE:footprint_alpha` @`0x69a3e0`).
//! - **Caps**: ring pools of **64 local-player + 512 everyone-else** slots, unconditional
//!   rotation (ring-select = GUID == local player, `0xca05f0`).
//! - **Suppressions**: hover (`MOVEFLAG 0x4000_0000`) · stealth (`BYTES_1` byte 3 bit 0x2 —
//!   NOT death) · player ghost (`PLAYER_FLAGS & 0x10`) · farther than **50 yd** from the local
//!   player · the terrain flag / printless id / no ground triangles. Water does NOT suppress
//!   the decal (only the spray branch wades). The reference's `showfootprints` cvar is the
//!   master toggle (default on) — we are always-on until a settings page wires the knob (the
//!   cvar-policy line, like the blob shadow's `shadowLOD`).
//!
//! Draw state per the RE: src-alpha blend, depth-write off, unlit, white vertex RGB with the
//! fade in vertex alpha — the ink darkness lives in the texture. Still open there (none
//! load-bearing, note §13): the forward-axis sign convention inside the shared UV basis, the
//! uv1 64×8 edge-fade ramp's combine mode (not modeled here — prints are small; the blob
//! shadow's vertical trapezoid is that ramp's other consumer), and the frame order vs the blob
//! shadow (our rung 2048 under its 4096 is a deterministic stand-in).

use std::collections::VecDeque;

use avian3d::prelude::Collider;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;

use benilla_assets::AdtTile;

use crate::assets::{AssetSet, LockRecover, WorldAssets};
use crate::blob_shadow::SHADOW_RASTER_BIAS;
use crate::collision::GroundDecalSurface;
use crate::creature_anim::{footfall_side, move_flags, AnimSoundEvent, MovementState};
use crate::decal::{project_decal, DecalFrame};
use crate::entities::{BoneAttach, Creatures};
use crate::net::{NetEntity, ObjectStore, SelfPlayer};
use crate::particles::buffer::{
    begin_effect_frame, EffectBlend, EffectDrawSpec, EffectFog, EffectQuads, EffectVertex,
};
use crate::player::WorldCamera;
use crate::schedule::WorldStage;
use crate::sound::footsteps::Footsteps;
use crate::terrain_stream::{ground_effect_under, TerrainStreamer};

/// Print lifetime, spawn to gone — the reference's 6000 ms (byte-verified, wow-re
/// `footprint-decals.md`: `t = 1 − age/6000`, die at `t < 0`).
const LIFETIME: f32 = 6.0;
/// The local player's own ring pool: 64 slots (the reserved head of the reference's 576-slot
/// table @`0xca05f0`), unconditional rotation.
const OWN_CAP: usize = 64;
/// Everyone else shares 512 slots (the table's tail), same rotation.
const SHARED_CAP: usize = 512;
/// Prints farther than this from the local player never spawn (the reference's 2500 yd² gate).
const MAX_DISTANCE_SQ: f32 = 2500.0;
/// The print's sort-ladder rung: below the blob shadow's 4096 — where both decals stack the
/// shadow draws later, deterministically (the reference's frame order is the RE's open §13 item).
const PRINT_SORT_BIAS: f32 = 2048.0;
/// Vertical reach of the projection slab about the planted foot: enough to catch the ground
/// through a slightly-lifted foot bone and drape a step edge, small enough not to paint a
/// terrace below (the blob shadow's slab is the model box; a print has no box to read).
const SLAB_HALF_HEIGHT: f32 = 1.0;

/// One live print: the projected world-space triangles (vertex alpha 1.0 — the fade multiplies
/// at push time), its spawn stamp, and its ink texture.
struct Print {
    verts: Vec<EffectVertex>,
    spawned: f32,
    texture: AssetId<Image>,
    /// The draw's sort anchor (the planted-foot spot).
    anchor: Vec3,
}

/// The live prints — the reference's two ring pools ([`OWN_CAP`] local player /
/// [`SHARED_CAP`] everyone else), each spawn-ordered (front = oldest; spawn order is expiry
/// order — one shared lifetime). Resources, not per-print entities: prints are inert after
/// spawn — no identity, queries, or despawn wiring needed.
#[derive(Resource, Default)]
struct Footprints {
    own: VecDeque<Print>,
    shared: VecDeque<Print>,
}

/// The lane's one identity entity — the phase probe's `main_entity` for every print draw (prints
/// have no entity of their own; a probe line still needs a producer to name).
#[derive(Resource)]
struct FootprintLane(Entity);

/// `FootprintTextures.dbc` id → the loaded ink texture. Loaded once at startup (six rows).
#[derive(Resource, Default)]
struct FootprintInk(HashMap<u32, Handle<Image>>);

pub(crate) struct FootprintsPlugin;

impl Plugin for FootprintsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Footprints>()
            .add_systems(Startup, load_ink.after(AssetSet::Open))
            // Present, like the footstep sounds: the same event stream, after the frame's
            // animation drive + transforms have settled the foot bones.
            .add_systems(Update, spawn_footprints.in_set(WorldStage::Present))
            .add_systems(PostUpdate, push_footprints.after(begin_effect_frame));
    }
}

/// Read `FootprintTextures.dbc` and start the six ink textures loading.
fn load_ink(
    mut commands: Commands,
    assets: Option<Res<WorldAssets>>,
    asset_server: Res<AssetServer>,
) {
    let lane = commands.spawn(Name::new("footprint-lane")).id();
    commands.insert_resource(FootprintLane(lane));
    let Some(assets) = assets else { return };
    let table = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_footprint_textures(&mut chain)
    };
    match table {
        Ok(table) => {
            let ink = table
                .into_iter()
                .map(|(id, path)| {
                    // DBC paths are extensionless (`textures\Footsteps\BaseFootprint`); the
                    // `mpq://` source wants forward slashes + the `.blp`.
                    let url = format!("mpq://{}.blp", path.replace('\\', "/"));
                    (id, asset_server.load::<Image>(url))
                })
                .collect();
            commands.insert_resource(FootprintInk(ink));
        }
        Err(e) => warn!("footprints: FootprintTextures.dbc failed to load: {e:#}"),
    }
}

/// The spawner's ROOT-unit reads (see [`spawn_footprints`]'s `roots` param).
type RootState = (
    Has<SelfPlayer>,
    Option<&'static NetEntity>,
    Option<&'static ObjectStore>,
    Option<&'static MovementState>,
);

/// Spawn a print for each per-foot plant on a footprint surface: the state gates (hover /
/// stealth / ghost / distance), the terrain-flags gate, the unit's ink + params, the event
/// marker's live bone position, yaw to the facing, project once, cache the triangles.
#[allow(clippy::too_many_arguments)]
fn spawn_footprints(
    mut events: MessageReader<AnimSoundEvent>,
    time: Res<Time>,
    // GlobalTransform for the same reason as the footstep sounds (0441): a mounted unit's steps
    // are the MOUNT child's tags, whose local Transform is the seat-relative ~origin.
    units: Query<(&NetEntity, &GlobalTransform, Option<&BoneAttach>)>,
    // The spawner's ROOT (the rider for a mount child): the pool select, the print scale (the
    // rider's SCALE_X — RE-corrected), and the state gates all read the root.
    parents: Query<&ChildOf>,
    roots: Query<RootState>,
    self_pos: Query<&Transform, With<SelfPlayer>>,
    joints: Query<&GlobalTransform>,
    footsteps: Option<Res<Footsteps>>,
    creatures: Option<Res<Creatures>>,
    ink: Option<Res<FootprintInk>>,
    streamer: Res<TerrainStreamer>,
    adt_tiles: Res<Assets<AdtTile>>,
    surfaces: Query<&Collider, With<GroundDecalSurface>>,
    mut prints: ResMut<Footprints>,
) {
    if events.is_empty() {
        return;
    }
    let (Some(footsteps), Some(creatures), Some(ink)) = (footsteps, creatures, ink) else {
        return;
    };
    let now = time.elapsed_secs();
    for ev in events.read() {
        // The VISUAL footfall channel: the per-foot side tags only (`$FSD` is the sound handler's,
        // decision 1080). **Right** prints get the mirrored texture — the shipped art is the left
        // foot (the `0x5fc07f` `side==0` mirror, corrected from 1006's guess in 1012).
        let Some(side) = footfall_side(&ev.ident) else {
            continue;
        };
        let Ok((net, transform, attach)) = units.get(ev.entity) else {
            continue;
        };
        // The ink + dims come from the EVENT's model (the mount for a mounted composite); the
        // scale and every state gate from the ROOT unit (the rider) — the RE's split.
        let Some(params) = net.display_id.and_then(|d| creatures.footprint(d)) else {
            continue;
        };
        let Some(texture) = ink.0.get(&params.texture_id) else {
            continue;
        };
        let mut root = ev.entity;
        while let Ok(child_of) = parents.get(root) {
            root = child_of.parent();
        }
        let (is_self, root_net, store, movement) = match roots.get(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        // The state gates (each byte-verified, module docs): hover · stealth · player ghost.
        if movement.is_some_and(|m| m.flags & move_flags::HOVER != 0) {
            continue;
        }
        if let Some(store) = store {
            if store.0.unit_is_stealthed() || store.0.player_is_ghost() {
                continue;
            }
        }
        // The planted foot: the event's own marker (bone + offset) through the live joint —
        // exactly the shape the missile launch points use. No marker/joint = the unit origin.
        let foot = attach
            .and_then(|a| {
                let (bone, offset) = a.markers.get(&ev.ident).copied()?;
                let joint = a.anchor(bone)?;
                Some(joints.get(joint).ok()?.transform_point(offset))
            })
            .unwrap_or_else(|| transform.translation());
        // The reference's 50-yd radius about the local player (2500 yd², self always passes).
        if !is_self
            && self_pos
                .single()
                .is_ok_and(|p| p.translation.distance_squared(foot) > MAX_DISTANCE_SQ)
        {
            continue;
        }
        // The surface gate, read under the foot itself (a beach edge prints per-foot).
        if !footsteps
            .0
            .leaves_footprints(ground_effect_under(&streamer, &adt_tiles, foot))
        {
            continue;
        }
        // Print frame: length along the unit's facing, width across, yawed to the facing.
        // With `(sin, cos) = (sin θ, cos θ)` the facing maps to the frame's −z′ axis, so v = 0
        // is the toe end (the absolute sign convention is the RE's open §13 item — a
        // backwards-pointing print flips one sign here).
        let yaw = transform
            .to_scale_rotation_translation()
            .1
            .to_euler(EulerRot::YXZ)
            .0;
        // The rider's wire SCALE_X for a mounted composite, the unit's own otherwise (RE: the
        // print scale is SCALE_X alone — display scales never multiply in).
        let scale = root_net.unwrap_or(net).scale.max(0.0);
        let (half_len, half_wid) = (params.length * scale * 0.5, params.width * scale * 0.5);
        if half_len <= 0.0 || half_wid <= 0.0 {
            continue;
        }
        let frame = DecalFrame {
            center: foot,
            sin: yaw.sin(),
            cos: yaw.cos(),
            min_x: -half_wid,
            max_x: half_wid,
            min_z: -half_len,
            max_z: half_len,
            min_y: -SLAB_HALF_HEIGHT,
            max_y: SLAB_HALF_HEIGHT,
        };
        let mirror = side == b'R';
        let mut verts = Vec::new();
        let projected = project_decal(
            &mut verts,
            &surfaces,
            &frame,
            |_| 1.0,
            |x, z| {
                let [u, v] = frame.rect_uv(x, z);
                [if mirror { 1.0 - u } else { u }, v]
            },
        );
        if !projected {
            continue; // no receiving surface under the foot (the projector's no-ground gate)
        }
        let n_verts = verts.len();
        let (pool, cap) = if is_self {
            (&mut prints.own, OWN_CAP)
        } else {
            (&mut prints.shared, SHARED_CAP)
        };
        if pool.len() >= cap {
            pool.pop_front(); // the reference's unconditional ring rotation
        }
        pool.push_back(Print {
            verts,
            spawned: now,
            texture: texture.id(),
            anchor: foot,
        });
        // The lane's census line (`RUST_LOG=benilla_app::footprints=debug`): every spawn names
        // its tag, ink, spot and triangle count — the first question of any "no prints under X"
        // report, answerable from a log (the blob shadow census pattern).
        debug!(
            "footprint: {} ink {} at ({:.2}, {:.2}, {:.2}), {} verts ({} own + {} shared live)",
            ev.ident.map(char::from).iter().collect::<String>(),
            params.texture_id,
            foot.x,
            foot.y,
            foot.z,
            n_verts,
            prints.own.len(),
            prints.shared.len(),
        );
    }
}

/// The reference's fade at age seconds (byte-diffed `PRIMITIVE:footprint_alpha` @`0x69a3e0`):
/// `t = 1 − age/6 s`, `alpha_byte = min(127, ⌊255·t⌋)` — ≈3.0 s hold at 127/255 (~50 %
/// opacity), then linear to 0; no fade-in; dead past [`LIFETIME`].
fn fade(age: f32) -> f32 {
    let t = 1.0 - age / LIFETIME;
    if t <= 0.0 {
        return 0.0;
    }
    (255.0 * t).floor().min(127.0) / 255.0
}

/// Retire expired prints, then replay every live print's cached triangles onto the effect
/// stream — src-alpha blend of the black-RGB ink, the fade multiplied into the vertex alpha at
/// push time (the cache stays untouched).
fn push_footprints(
    time: Res<Time>,
    cam: Query<Entity, With<WorldCamera>>,
    lane: Option<Res<FootprintLane>>,
    mut quads: ResMut<EffectQuads>,
    mut prints: ResMut<Footprints>,
) {
    let now = time.elapsed_secs();
    let prints = &mut *prints;
    for pool in [&mut prints.own, &mut prints.shared] {
        while pool.front().is_some_and(|p| now - p.spawned >= LIFETIME) {
            pool.pop_front();
        }
    }
    if prints.own.is_empty() && prints.shared.is_empty() {
        return;
    }
    let Ok(cam) = cam.single() else { return };
    let Some(lane) = lane else { return };
    for print in prints.own.iter().chain(prints.shared.iter()) {
        let alpha = fade(now - print.spawned);
        let start = quads.begin();
        quads.verts.extend(print.verts.iter().map(|v| EffectVertex {
            color: [v.color[0], v.color[1], v.color[2], v.color[3] * alpha],
            ..*v
        }));
        quads.commit_tris(
            start,
            EffectDrawSpec {
                cam,
                texture: print.texture,
                blend: EffectBlend::Alpha,
                fog: EffectFog::Off,
                lit: false,
                anchor: print.anchor,
                bias: PRINT_SORT_BIAS,
                raster_bias: SHADOW_RASTER_BIAS,
                main_entity: lane.0,
                light: None,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte-verified ramp: no fade-in — 127/255 from age 0, held while `255·t ≥ 127`
    /// (through age = 6·(1 − 127/255) ≈ 3.012 s), then the raw `⌊255·t⌋/255` line to zero at
    /// 6 s, dead after.
    #[test]
    fn fade_matches_the_reference_ramp() {
        assert_eq!(fade(0.0), 127.0 / 255.0);
        assert_eq!(fade(1.0), 127.0 / 255.0);
        assert_eq!(fade(3.0), 127.0 / 255.0);
        // Past the hold knee: the raw line. age 4.5 → t = 0.25 → ⌊63.75⌋ = 63.
        assert_eq!(fade(4.5), 63.0 / 255.0);
        // age 5.988 → t = 0.002 → ⌊0.51⌋ = 0 — the line reaches the floor before death.
        assert_eq!(fade(5.988), 0.0);
        assert_eq!(fade(6.0), 0.0);
        assert_eq!(fade(7.0), 0.0);
    }
}
