//! Water foam — the client's **`CWater0Ripple`** wade wake / standing ring / step-in splash,
//! rebuilt as the reference's actual **record model** (decision 0264; supersedes the ribbon of 0240
//! and the stamps of 0234).
//!
//! Ground truth, twice over: the byte-exact RE (wow-5875-re `system/terrain/scratch/`
//! `water-ripple-decal.md` + the `waterdecal-*.c` decomps, §5-verified) **and** two live GL traces
//! of the reference reconstructed frame-by-frame (Northshire wade 2026-07-08 + standing-ring
//! capture 2026-07-10; per-record texgen fits residual ≈ 0). The verified model:
//!
//! - **A decal is a pool record, not a mesh idiom.** Per wading unit the client emits records into
//!   a 128-slot pool (32 reserved for the active mover, 96 for everyone else — eviction only).
//!   Each record: feet-anchored center (Z = the **liquid surface** height), heading, `size0`,
//!   growth rate, lifetime, peak alpha, birth time.
//! - **Geometry is built ONCE, at emission**, from the wet liquid-lattice cells overlapping the
//!   record's *final* box (`size0 + growth·lifetime`) — which clips foam at banks. **Growth is
//!   pure texgen**: each frame the texture maps the box `[center ± size(t)]` to UV `[0,1]`,
//!   rotated to the heading; outside the box the stencil clamps to its transparent border (both
//!   foam textures have alpha-0 edges; sampler CLAMP — measured; the 0240 "GL_REPEAT tiling" was
//!   a misread). The shape *stretches* over a static patch as `size(t)` grows.
//! - **Alpha** rises to the peak over `0.4·L`, decays to 0 over `0.6·L`; the record dies at `L`.
//!   Vertex colour is flat white × that alpha; the **stencil's own near-black RGB is the
//!   intensity** (the reference fragment is a full MODULATE — measured in its FFP GLSL:
//!   `tex × diffuse` on RGB *and* A). Recolouring the stencil to white — 0234/0240's move — made
//!   the foam ~20× too bright; that was the blown-out white wedge.
//! - **Selection** (driver `0x5fa760`): translating (`MOVEMENTFLAGS & 0xf` — the avatar's real
//!   flags; a velocity proxy for streamed units) → WAKE, V-apex **along** the movement vector;
//!   turning in place (`& 0x30`) → full-size RING; standing → reduced RING (size ×0.6, growth
//!   ×0.25, alpha ×0.8); plus a one-shot full RING on the wade-depth crossing (`0x6030c0`).
//!   Emission gates on `depth < max(2·collisionHeight, 1.0)` — ≈4.06 yd for a human (decision
//!   0489: surface SWIMMING sits well inside the gate at its ~1.52-yd rest depth, so a floating
//!   or stroking swimmer keeps its ring/wake, matching the reference; only a real dive past
//!   ~2 body heights silences it) — with a linear attenuation past half. One shared per-unit
//!   cooldown cell paces both
//!   kinds: rings every 400–450 ms, wakes one per ~0.625 yd of travel (the byte cadence laws —
//!   the 0264 INTERIM constants are resolved, decision 0265).
//!
//! The formulas + lifecycle math live in [`params`]; this half is the ECS: the emitter over the
//! avatar + streamed units, the record pool, and the per-(chunk, category) additive meshes on the
//! shared effect gamma lane (fog OFF — the reference's verified foam render state).

mod params;
mod view;

pub(crate) use view::{WaterFxView, WfxMode};

use bevy::asset::RenderAssetUsages;
use bevy::ecs::entity::EntityHashMap;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::ExtendedMaterial;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_protocol::EntityKind;

use crate::assets::{AssetSet, WorldAssets};
use crate::creature_anim::move_flags;
use crate::lighting::SharedLightBuffer;
use crate::liquid::{FoamPatch, WaterChunkInfo};
use crate::net::{NetEntity, SelfPlayer};
use crate::particles::{WowParticleExt, WowParticleMaterial};
use crate::player::{Player, CAPSULE_HEIGHT};
use crate::schedule::WorldStage;

use params::{foam_params, foam_uv, rand01, record_alpha, record_size, WadeState};
use params::{wake_cooldown, RING_INTERVAL};

/// The two foam stencils (render categories: 0 = ring, 1 = wake). Loaded RAW — the alpha carries
/// the shape, the near-black RGB carries the intensity (never recolour; see module doc).
const RING_TEXTURE: &str = "xtextures/splash/splash.blp";
const WAKE_TEXTURE: &str = "xtextures/splash/wake.blp";

/// Pool geometry (VERIFIED `0x68f8b0`/`0x68f9f0`): 128 records; the active mover allocates from
/// `[0, 32)`, everyone else from `[32, 128)` — an anti-eviction partition, nothing more.
const POOL_SIZE: usize = 128;
const SELF_SLOTS: usize = 32;

/// Step-in/out one-shot depth threshold, as a fraction of collision height (VERIFIED `0x6030c0`:
/// feet-Z vs `0.4 × collision height` from CMovement+0xb4, crossing latched either direction).
/// Human collision height ≈ 2.03 yd → fires ~0.81 yd deep.
const ONESHOT_DEPTH: f32 = 0.4 * CAPSULE_HEIGHT;

/// The emission depth gate at scale 1: **2 × collision height** (≈4.06 yd for a human; scaled by
/// the unit's scale, floored at 1.0 like the reference's `max(…, 1.0)`). The gate field
/// `[unit+0x297]` is the dword-indexed `+0xa5c` = CMovement+0xb4 = **collision height** —
/// Q5(c) of wow-re `water-ripple-decal.md` cross-verifies the field identity; the note's
/// depth-gate paragraph mislabels the same field "boundingRadius", and transcribing that label
/// (`2 × UNIT_FIELD_BOUNDINGRADIUS` ≈ 0.78, clamped to a 1-yd gate) is what killed all foam the
/// moment swim latched: the ~1.52-yd swim rest depth sat past the misread gate (decision 0489 —
/// the director's ref-check shows surface swimmers foaming, which the true 4-yd gate allows).
const GATE_DEPTH: f32 = 2.0 * CAPSULE_HEIGHT;

/// Horizontal speed (yd/s) above which a streamed unit counts as translating — the velocity proxy
/// for the reference's `MOVEMENTFLAGS & 0xf` bit-test (the avatar uses its real flags). A small
/// guard over extrapolation jitter.
const MOVE_EPSILON: f32 = 0.5;
/// Yaw rate (rad/s) above which a non-translating streamed unit counts as turning in place
/// (the `& 0x30` proxy).
const TURN_EPSILON: f32 = 0.35;

/// Geometric lift (yd) above the water surface — the anti-coplanarity measure standing in for the
/// reference's polygon offset (−2048 GL units) where foam meets the *terrain* at the shoreline.
const SURFACE_LIFT: f32 = 0.03;

/// One live foam decal — a `CWater0Ripple` pool record. Geometry is static (built at emission);
/// per-frame size/alpha are derived from `born` + the params (`params.rs`).
struct FoamRecord {
    /// Feet position at emission, WoW XY.
    center: [f32; 2],
    /// Texgen heading (WoW radians): the movement direction (wake) or random (ring).
    heading: f32,
    size0: f32,
    /// yd/s.
    growth: f32,
    /// s; death at `born + lifetime`.
    lifetime: f32,
    /// Peak vertex alpha.
    peak: f32,
    born: f32,
    /// Render category: ring (`splash.blp`) vs wake (`wake.blp`).
    ring: bool,
    /// Static patch triangles (Bevy space, surface-lifted), from the wet liquid cells overlapping
    /// the final box. Empty patches never allocate a record.
    verts: Vec<Vec3>,
    /// The liquid chunk that hosted the emission — foam meshes group per chunk so the transparent
    /// pass sorts them with their water (then `depth_bias` wins the coplanar tie).
    chunk: Entity,
}

/// Per-unit emitter state: the shared cooldown cell (`unit+0xc78` — ONE cell for both kinds: a
/// ring pulse delays a following wake and vice versa, faithful), the step-in latch, and motion
/// history for the streamed-unit velocity/yaw proxies.
struct UnitFoam {
    last_pos: Option<Vec3>,
    last_yaw: Option<f32>,
    /// Absolute time the next emission is allowed (the shared tick cooldown).
    ready: f32,
    /// Step-in latch: currently deeper than the one-shot threshold.
    wading: bool,
    rng: u32,
    /// Fed this frame (drives retiring state for despawned units).
    active: bool,
}

impl UnitFoam {
    fn new(seed: u32) -> Self {
        UnitFoam {
            last_pos: None,
            last_yaw: None,
            ready: 0.0,
            wading: false,
            rng: seed | 1,
            active: false,
        }
    }
}

/// All foam state: the 128-slot record pool with its two allocation cursors, and the per-unit
/// emitter states (the avatar keys [`Entity::PLACEHOLDER`] — it has no streamed entity of its own).
#[derive(Resource)]
struct WaterFoam {
    pool: Vec<Option<FoamRecord>>,
    self_cursor: usize,
    other_cursor: usize,
    units: EntityHashMap<UnitFoam>,
}

impl Default for WaterFoam {
    fn default() -> Self {
        WaterFoam {
            pool: (0..POOL_SIZE).map(|_| None).collect(),
            self_cursor: 0,
            other_cursor: 0,
            units: EntityHashMap::default(),
        }
    }
}

/// Advance a partition cursor, returning the absolute pool slot to (over)write — the reference's
/// oldest-in-partition eviction (VERIFIED: counters `0xc7f3b0` self / `0xc81d48` others).
fn alloc_slot(cursor: &mut usize, base: usize, len: usize) -> usize {
    let i = base + *cursor;
    *cursor = (*cursor + 1) % len;
    i
}

/// The two shared foam materials (ring/wake stencils on the effect gamma lane, fog off).
#[derive(Resource)]
struct FoamMaterials {
    ring: Handle<WowParticleMaterial>,
    wake: Handle<WowParticleMaterial>,
}

/// One pooled mesh entity per (liquid chunk, is-ring) with live records.
#[derive(Resource, Default)]
struct FoamMeshes(HashMap<(Entity, bool), Entity>);

/// Marks a foam mesh entity.
#[derive(Component)]
struct FoamMesh;

/// Load the stencils raw (CLAMP, no mips — the reference's measured sampler state), build the two
/// materials on the shared effect lane with fog disabled (VERIFIED render state: FOG off).
fn setup_water_fx(
    mut commands: Commands,
    world_assets: Option<ResMut<WorldAssets>>,
    light: Res<SharedLightBuffer>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<WowParticleMaterial>>,
) {
    commands.init_resource::<WaterFoam>();
    commands.init_resource::<FoamMeshes>();
    let Some(mut world_assets) = world_assets else {
        return;
    };
    let Some(ring) = foam_image(&mut world_assets, RING_TEXTURE, &mut images) else {
        return;
    };
    let Some(wake) = foam_image(&mut world_assets, WAKE_TEXTURE, &mut images) else {
        return;
    };
    let mut material = |texture: Handle<Image>| {
        materials.add(ExtendedMaterial {
            base: StandardMaterial {
                base_color_texture: Some(texture),
                unlit: true,
                alpha_mode: AlphaMode::Add,
                cull_mode: None,
                double_sided: true,
                // Wins the coplanar transparent-sort tie against its own water chunk (the
                // reference draws foam in the water group right after the liquid surface).
                depth_bias: 1.0,
                ..default()
            },
            extension: WowParticleExt {
                light_buf: light.0.clone(),
                // x = fog enable: the reference's foam render sets FOG off (VERIFIED `0x68fcc1`).
                params: Vec4::ZERO,
                mod2x: false,
            },
        })
    };
    let wake = material(wake);
    let ring = material(ring);
    commands.insert_resource(FoamMaterials { ring, wake });
}

/// Decode a foam stencil, keeping its authored RGBA verbatim (raw gamma bytes, the house
/// invariant — the near-black RGB is the intensity). `None` (with a warning) if missing.
fn foam_image(
    world_assets: &mut WorldAssets,
    path: &str,
    images: &mut Assets<Image>,
) -> Option<Handle<Image>> {
    let Some((w, h, rgba)) = world_assets.decode_rgba(path) else {
        warn!("water foam: {path} unavailable — no water foam");
        return None;
    };
    let mut image = Image::new(
        Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    Some(images.add(image))
}

/// Build a record's static patch: every wet liquid cell whose XY bounds overlap the final box, cut
/// into the same two triangles the liquid surface draws, surface-lifted, in Bevy space; plus the
/// hosting chunk (the one containing the center). `None` when the center has no hosting water or no
/// wet cell overlaps (off the edge).
fn build_patch(
    center: [f32; 2],
    final_size: f32,
    chunks: &[(Entity, &WaterChunkInfo, &FoamPatch)],
) -> Option<(Vec<Vec3>, Entity)> {
    let (lo_x, hi_x) = (center[0] - final_size, center[0] + final_size);
    let (lo_y, hi_y) = (center[1] - final_size, center[1] + final_size);
    let mut verts = Vec::new();
    let mut host: Option<Entity> = None;
    for (entity, info, _foam) in chunks {
        if !info.overlaps(lo_x, hi_x, lo_y, hi_y) {
            continue;
        }
        if host.is_none() && info.contains(center[0], center[1]) {
            host = Some(*entity);
        }
        info.for_each_wet_cell(|[tl, tr, bl, br]| {
            let x = [tl[0], tr[0], bl[0], br[0]];
            let y = [tl[1], tr[1], bl[1], br[1]];
            let (cx0, cx1) = (
                x.iter().copied().fold(f32::MAX, f32::min),
                x.iter().copied().fold(f32::MIN, f32::max),
            );
            let (cy0, cy1) = (
                y.iter().copied().fold(f32::MAX, f32::min),
                y.iter().copied().fold(f32::MIN, f32::max),
            );
            if cx1 < lo_x || cx0 > hi_x || cy1 < lo_y || cy0 > hi_y {
                return;
            }
            // The liquid mesh's own winding: [tl, bl, br] then [tl, br, tr].
            for v in [tl, bl, br, tl, br, tr] {
                verts.push(wow_to_bevy([v[0], v[1], v[2] + SURFACE_LIFT]));
            }
        });
    }
    let host = host?;
    if verts.is_empty() {
        None
    } else {
        Some((verts, host))
    }
}

/// Classify + gate + emit for one unit this frame (the driver `0x5fa760`, once per unit per
/// frame; emission paced by the unit's shared cooldown cell).
#[allow(clippy::too_many_arguments)]
fn drive_unit(
    foam_state: &mut UnitFoam,
    alloc: &mut dyn FnMut(FoamRecord),
    pos: Vec3,
    state: WadeState,
    scale: f32,
    gate: f32,
    chunks: &[(Entity, &WaterChunkInfo, &FoamPatch)],
    now: f32,
) {
    foam_state.active = true;
    let wow = bevy_to_wow(pos);
    // The surface height under the unit, from the wet cell it stands on — not the chunk's box and
    // not the chunk's highest vertex, which on a sloped river put the wade depth ~2 yd out
    // (decision 0642).
    let Some(surface) = chunks
        .iter()
        .find_map(|(_, info, _)| info.surface_z_at(wow[0], wow[1]))
    else {
        foam_state.wading = false;
        return;
    };
    let depth = surface - wow[2];

    // Step-in/out one-shot: fires once on crossing the wade depth in EITHER direction
    // (`0x6030c0`, latched at `[+0x269]` — VERIFIED; the driver's mode-0xC9 call also clears the
    // shared cooldown).
    let wading_now = depth > ONESHOT_DEPTH * scale;
    let oneshot = wading_now != foam_state.wading;
    foam_state.wading = wading_now;

    if !oneshot && now < foam_state.ready {
        return;
    }
    let Some(p) = foam_params(state, oneshot, scale, gate, depth, &mut foam_state.rng) else {
        return;
    };
    let heading = match (p.ring, state) {
        (false, WadeState::Translating { heading, .. }) => heading,
        _ => rand01(&mut foam_state.rng) * std::f32::consts::TAU,
    };
    let final_size = p.size0 + p.growth * p.lifetime;
    let center = [wow[0], wow[1]];
    if let Some((verts, chunk)) = build_patch(center, final_size, chunks) {
        alloc(FoamRecord {
            center,
            heading,
            size0: p.size0,
            growth: p.growth,
            lifetime: p.lifetime,
            peak: p.peak,
            born: now,
            ring: p.ring,
            verts,
            chunk,
        });
    }
    // The shared cooldown cell: the one-shot resets it; a ring pulses on the 400+U[0,50) ms law;
    // a wake re-arms on the distance law (one decal per ~0.625 yd of travel).
    let mut uni = |a: f32, b: f32| a + (b - a) * rand01(&mut foam_state.rng);
    foam_state.ready = if oneshot {
        now
    } else if p.ring {
        now + uni(RING_INTERVAL.0, RING_INTERVAL.1)
    } else {
        let speed = match state {
            WadeState::Translating { speed, .. } => speed,
            _ => 0.0,
        };
        now + wake_cooldown(speed, &mut foam_state.rng)
    };
}

/// Per frame: run the driver for the avatar (its real movement flags — the reference's own
/// selection bits) and for every streamed unit (the velocity/yaw proxy), emitting pool records.
fn emit_water_foam(
    time: Res<Time>,
    materials: Option<Res<FoamMaterials>>,
    mut foam: ResMut<WaterFoam>,
    player: Res<Player>,
    self_store: Query<&NetEntity, With<SelfPlayer>>,
    units: Query<(Entity, &Transform, &NetEntity), Without<SelfPlayer>>,
    water: Query<(Entity, &WaterChunkInfo, &FoamPatch)>,
) {
    if materials.is_none() {
        return;
    }
    let now = time.elapsed_secs();
    let dt = time.delta_secs().max(1.0e-4);
    let chunks: Vec<_> = water.iter().collect();

    for uf in foam.units.values_mut() {
        uf.active = false;
    }

    if !chunks.is_empty() {
        // The avatar: byte-faithful selection off our own streamed movement flags.
        if player.active && !player.detached {
            let uf = foam
                .units
                .entry(Entity::PLACEHOLDER)
                .or_insert_with(|| UnitFoam::new(0x5EED_F0A5));
            let prev = uf.last_pos.replace(player.pos);
            let vel = prev.map_or(Vec3::ZERO, |p| (player.pos - p) / dt);
            let w = bevy_to_wow(vel);
            let speed = (w[0] * w[0] + w[1] * w[1]).sqrt();
            let flags = player.move_flags();
            let state = if flags & 0xf != 0 {
                WadeState::Translating {
                    speed,
                    heading: w[1].atan2(w[0]),
                }
            } else if flags & (move_flags::TURN_LEFT | move_flags::TURN_RIGHT) != 0 {
                WadeState::Turning
            } else {
                WadeState::Standing
            };
            let scale = self_store.single().map(|net| net.scale).unwrap_or(1.0);
            let gate = (GATE_DEPTH * scale).max(1.0);
            let WaterFoam {
                pool,
                self_cursor,
                units,
                ..
            } = &mut *foam;
            let uf = units.get_mut(&Entity::PLACEHOLDER).expect("inserted above");
            let mut alloc = |rec: FoamRecord| {
                pool[alloc_slot(self_cursor, 0, SELF_SLOTS)] = Some(rec);
            };
            drive_unit(uf, &mut alloc, player.pos, state, scale, gate, &chunks, now);
        }

        // Streamed units (the avatar's own wire ghost is excluded via `Without<SelfPlayer>`).
        for (entity, transform, net) in &units {
            if !matches!(net.kind, EntityKind::Unit | EntityKind::Player) {
                continue;
            }
            let pos = transform.translation;
            let seed = (entity.to_bits() as u32) ^ 0xA11C_E5ED;
            let uf = foam
                .units
                .entry(entity)
                .or_insert_with(|| UnitFoam::new(seed));
            let prev_pos = uf.last_pos.replace(pos);
            let yaw = transform.rotation.to_euler(EulerRot::YXZ).0;
            let prev_yaw = uf.last_yaw.replace(yaw);
            let vel = prev_pos.map_or(Vec3::ZERO, |p| (pos - p) / dt);
            let w = bevy_to_wow(vel);
            let speed = (w[0] * w[0] + w[1] * w[1]).sqrt();
            let yaw_rate = prev_yaw.map_or(0.0, |p| {
                let mut d = yaw - p;
                while d > std::f32::consts::PI {
                    d -= std::f32::consts::TAU;
                }
                while d < -std::f32::consts::PI {
                    d += std::f32::consts::TAU;
                }
                (d / dt).abs()
            });
            let state = if speed > MOVE_EPSILON {
                WadeState::Translating {
                    speed,
                    heading: w[1].atan2(w[0]),
                }
            } else if yaw_rate > TURN_EPSILON {
                WadeState::Turning
            } else {
                WadeState::Standing
            };
            let gate = (GATE_DEPTH * net.scale).max(1.0);
            let WaterFoam {
                pool,
                other_cursor,
                units,
                ..
            } = &mut *foam;
            let uf = units.get_mut(&entity).expect("inserted above");
            let mut alloc = |rec: FoamRecord| {
                pool[alloc_slot(other_cursor, SELF_SLOTS, POOL_SIZE - SELF_SLOTS)] = Some(rec);
            };
            drive_unit(uf, &mut alloc, pos, state, net.scale, gate, &chunks, now);
        }
    }

    // Retire emitter state not fed this frame (unit despawned); its records age out on their own.
    foam.units.retain(|_, uf| uf.active);
}

/// Per frame: age the pool, drop dead records, and rebuild one mesh per (chunk, category) with
/// live records — positions static, UV = the texgen at `size(t)`, colour = white × the alpha ramp.
#[allow(clippy::too_many_arguments)] // one Bevy system's full input set
fn render_water_foam(
    time: Res<Time>,
    materials: Option<Res<FoamMaterials>>,
    mut foam: ResMut<WaterFoam>,
    mut foam_meshes: ResMut<FoamMeshes>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
    mesh_entities: Query<&Mesh3d, With<FoamMesh>>,
    chunks_alive: Query<(), With<WaterChunkInfo>>,
) {
    let Some(materials) = materials else {
        return;
    };
    let now = time.elapsed_secs();

    for slot in &mut foam.pool {
        if slot.as_ref().is_some_and(|r| now - r.born >= r.lifetime) {
            *slot = None;
        }
    }

    let mut groups: HashMap<(Entity, bool), MeshBuilder> = HashMap::default();
    for rec in foam.pool.iter().flatten() {
        if chunks_alive.get(rec.chunk).is_err() {
            continue; // chunk streamed out; the record ages out silently
        }
        let b = groups.entry((rec.chunk, rec.ring)).or_default();
        let size = record_size(rec.size0, rec.growth, rec.born, now);
        let alpha = record_alpha(rec.peak, rec.lifetime, rec.born, now);
        for v in &rec.verts {
            let wow = bevy_to_wow(*v);
            let uv = foam_uv(rec.center, rec.heading, size, [wow[0], wow[1]]);
            b.push(*v, uv, alpha);
        }
    }

    // Clear meshes whose group emptied; drop mesh entities whose chunk despawned.
    foam_meshes.0.retain(|key, entity| {
        if groups.contains_key(key) {
            return true;
        }
        if chunks_alive.get(key.0).is_ok() {
            if let Ok(Mesh3d(h)) = mesh_entities.get(*entity) {
                if let Some(m) = meshes.get_mut(h) {
                    MeshBuilder::default().write(m);
                }
            }
            true
        } else {
            commands.entity(*entity).despawn();
            false
        }
    });
    for (key, builder) in groups {
        if let Some(&entity) = foam_meshes.0.get(&key) {
            if let Ok(Mesh3d(h)) = mesh_entities.get(entity) {
                if let Some(m) = meshes.get_mut(h) {
                    builder.write(m);
                }
            }
        } else {
            let material = if key.1 {
                materials.ring.clone()
            } else {
                materials.wake.clone()
            };
            let mut mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            );
            builder.write(&mut mesh);
            let entity = commands
                .spawn((
                    FoamMesh,
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(material),
                    Transform::IDENTITY,
                ))
                .id();
            foam_meshes.0.insert(key, entity);
        }
    }
}

/// A tiny growable triangle-list accumulator: white verts with per-vertex alpha, up normal, UV.
/// Foam triangles are unshared (identity indices), like the reference's per-record fans.
#[derive(Default)]
struct MeshBuilder {
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    colors: Vec<[f32; 4]>,
}

impl MeshBuilder {
    fn push(&mut self, pos: Vec3, uv: [f32; 2], alpha: f32) {
        self.positions.push(pos.to_array());
        self.uvs.push(uv);
        self.colors.push([1.0, 1.0, 1.0, alpha]);
    }

    fn write(self, mesh: &mut Mesh) {
        let n = self.positions.len();
        let indices = (0..n as u32).collect::<Vec<_>>();
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; n]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, self.uvs);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.colors);
        mesh.insert_indices(Indices::U32(indices));
    }
}

/// The water foam plugin.
pub(crate) struct WaterFxPlugin;

impl Plugin for WaterFxPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_water_fx.after(AssetSet::Open))
            .add_systems(
                Update,
                (
                    view::waterfx_spawn,
                    view::waterfx_drive,
                    emit_water_foam,
                    render_water_foam,
                )
                    .chain()
                    .in_set(WorldStage::Present),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pool partitioning: the self cursor wraps within [0, 32), others within [32, 128) —
    /// eviction replaces the oldest of the same partition, never crosses it.
    #[test]
    fn pool_partitions_and_evicts() {
        let mut foam = WaterFoam::default();
        for _ in 0..SELF_SLOTS + 3 {
            let i = alloc_slot(&mut foam.self_cursor, 0, SELF_SLOTS);
            assert!(i < SELF_SLOTS);
        }
        assert_eq!(foam.self_cursor, 3);
        for _ in 0..(POOL_SIZE - SELF_SLOTS) + 5 {
            let i = alloc_slot(&mut foam.other_cursor, SELF_SLOTS, POOL_SIZE - SELF_SLOTS);
            assert!((SELF_SLOTS..POOL_SIZE).contains(&i));
        }
        assert_eq!(foam.other_cursor, 5);
    }

    /// The patch builder: wet cells overlapping the final box make it in (surface-lifted), ones
    /// outside stay out, and a dry position (no hosting chunk) yields no record.
    #[test]
    fn patch_clips_to_wet_cells() {
        // A 3×3-vertex grid over `[0,10]²` — 4 cells of 5 yd. Only the near (0,0) cell is wet, so
        // the far half of the box is dry ground the patch must not cover. The grid rides the info
        // now (the swim query needs the same cells); `FoamPatch` is just the marker.
        let mut positions = Vec::new();
        for j in 0..3 {
            for i in 0..3 {
                positions.push([i as f32 * 5.0, j as f32 * 5.0, 5.0]);
            }
        }
        let info = WaterChunkInfo::new(
            crate::liquid::LiquidSource::AdtChunk,
            benilla_formats::LiquidKind::Still,
            [3, 3],
            positions,
            vec![true, false, false, false],
        );
        let patch = FoamPatch;
        let chunks = vec![(Entity::PLACEHOLDER, &info, &patch)];
        let (verts, _) = build_patch([2.0, 2.0], 1.5, &chunks).unwrap();
        assert_eq!(verts.len(), 6, "only the one wet cell overlaps");
        let wow = bevy_to_wow(verts[0]);
        assert!(
            (wow[2] - (5.0 + SURFACE_LIFT)).abs() < 1e-4,
            "lifted to the surface"
        );
        assert!(
            build_patch([50.0, 50.0], 1.5, &chunks).is_none(),
            "dry ⇒ none"
        );
    }
}
