//! **The Bevy render glue** — the animated liquid surfaces themselves.
//!
//! One shared material per (kind, fog block) over a `texture_2d_array` of the kind's frames, the two
//! spawn paths (an ADT chunk's MCLQ and a WMO group's MLIQ), the flat mesh build, and the 24 fps
//! frame cycler. The faithful shading model is `super`'s header and `liquid.wgsl`'s; the *position*
//! side — the grid each spawned surface publishes and the queries against it — is [`super::query`].
//!
//! The frame-flip is the client's first render animation — a deliberate **one-off** (a frame-index
//! uniform off Bevy real `Time`), NOT a general animation system. Two clocks: animation =
//! wall-clock; day/night = server game-time.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;

use super::query::{wet_footprint, FoamPatch, LiquidSource, WmoPool};
use crate::lighting::WATER_SHININESS;
use benilla_assets::coords::wow_to_bevy;
use benilla_assets::materials::{LiquidExt, LiquidMaterial};
use benilla_assets::LockRecover;
use benilla_assets::{liquid_frame_array, RenderConfig, WorldAssets};
use benilla_formats::{read_texture_mip_chain, BlpMipChain, LiquidKind, LiquidMesh};

/// Frame-flip rate — 30 frames over 1.25 s (VERIFIED `FUN_0068aac0`), i.e. 24 fps, real wall-clock.
const ANIM_FPS: f32 = 24.0;

/// The shared liquid materials, keyed by [`LiquidKey`], plus each one's animated frame count (for
/// the modulo in `animate_liquid`). Read by the terrain streamer (via [`spawn_liquids`] /
/// [`spawn_wmo_liquids`]) to material the per-chunk water meshes. Absent when the client has no data
/// (no `WorldAssets`).
#[derive(Resource, Default)]
pub(crate) struct LiquidAssets {
    materials: HashMap<LiquidKey, LiquidEntry>,
}

/// Which shared material a surface takes: its kind, and **which fog block it draws under**.
///
/// The second lane exists because the reference decides fog per *pass*, not per liquid type. A WMO
/// group's own pool is drawn by the WMO liquid pass, which re-submits the smoothed interior fog block
/// (`0x6b6323`–`0x6b6342`) under the same `[0xca7f00]` gate as the WMO *geometry* pass — so the pool
/// and the room's walls always fog alike. ADT liquid submits no fog and draws under the once-a-frame
/// scene block. (VERIFIED wow-re `fog-env-state` §5's complete 6-site submit census +
/// `liquid-render-state-sided` §5; decision 0691's open lane, now closed.)
///
/// The third lane is the **lava/slime UV scroll** — see [`scrolls`]. It is a per-*nibble*, per-*path*
/// property that [`LiquidKind`] deliberately collapses (2 and 6 are both `Magma`), so it cannot be
/// derived from the kind and has to key the material.
///
/// The variants share one decoded frame array — only the tiny uniform differs — so the extra
/// materials cost a handful of bytes, not a second copy of 30 × 256² textures.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct LiquidKey {
    kind: LiquidKind,
    /// This surface belongs to a WMO **interior** group (`MOGI & 0x48 == 0` — the same test
    /// `wow_model.wgsl` fogs its interior lanes by, which is what keeps the two in step).
    interior: bool,
    /// This surface takes the animated stage-0 texture matrix ([`scrolls`]).
    scroll: bool,
}

/// **Texture repeats per second** the reference slides a scrolling liquid sheet along **v** —
/// `[0x801620]` = 0.1 (VERIFIED wow-re `liquid-uv-scroll-law.md`, six-agent §5).
const SCROLL_REPEATS_PER_SEC: f32 = 0.1;

/// The scroll's wrap period in seconds — the `fmod` divisor `[0x80e5a0]` = 10.0. With
/// [`SCROLL_REPEATS_PER_SEC`] that makes the offset a **sawtooth over exactly one repeat**, so the
/// wrap is invisible on a `REPEAT`-sampled texture and the phase never grows without bound.
const SCROLL_PERIOD_SECS: f32 = 10.0;

/// Does this surface take the reference's animated stage-0 texture matrix — the lava/slime scroll?
///
/// **Only WMO MLIQ surfaces whose type nibble is 6 or 7** (VERIFIED wow-re `liquid-uv-scroll-law.md`:
/// the WMO magma/slime kernel `0x6b68f0` gates on `and esi,0xc; cmp esi,4` over the `0x6ba970`
/// nibble, and builds a matrix that is identity except element 13 = the v-translate). Three
/// consequences that are easy to get wrong, all of them load-bearing:
///
/// * **Nibbles 2 and 3 do NOT scroll**, though they reach the same kernel and draw the same
///   `lava.blp`/`slime.blp`. In the shipped data that is the difference between Blackrock's lava
///   (nibble 6, scrolls) and the **Ironforge Great Forge's** (nibble 2, static) — the same texture,
///   two behaviours, and the reason "lava scrolls" is not a statement you can make about lava.
/// * **ADT liquid never scrolls, magma included** — the open-world Searing Gorge / Burning Steppes
///   lava is nibble 6 too, but its queue is a *different* kernel (`0x68dca0`) which pushes a
///   byte-exact identity with element 13 hard-zeroed at `0x68dda1`. So the gate is the nibble AND
///   the path; a nibble-only test would slide every lava pool in the open world.
/// * The phase is driven off the reference's uptime clock, so only its **rate and period** are
///   reproducible — never its absolute value. We drive ours off the same wall clock the frame
///   cycler uses.
fn scrolls(nibble: u8) -> bool {
    matches!(nibble, 6 | 7)
}

struct LiquidEntry {
    material: Handle<LiquidMaterial>,
    frame_count: u32,
    /// Mirrors [`LiquidKey::scroll`], so the cycler can drive the v-offset without re-deriving the
    /// key it is already iterating the values of.
    scroll: bool,
}

impl LiquidAssets {
    /// The shared material for a liquid kind on a given fog block, scrolling or not, if its frames
    /// loaded. `scroll` is [`scrolls`]'s verdict; a `true` for a kind that has no scrolling variant
    /// simply misses, which is the same "no material" path a failed frame load takes.
    pub(crate) fn material(
        &self,
        kind: LiquidKind,
        interior: bool,
        scroll: bool,
    ) -> Option<Handle<LiquidMaterial>> {
        self.materials
            .get(&LiquidKey {
                kind,
                interior,
                scroll,
            })
            .map(|e| e.material.clone())
    }
}

/// Marks a spawned water surface (one per liquid MCNK chunk), so it can be queried/culled as a group.
#[derive(Component)]
pub(crate) struct LiquidSurface;

/// Attribution kill-switch: `$WOW_NO_LIQUID=1` hides every liquid **surface**, and only the surface —
/// the swim grid, the submersion verdict, the foam and the ambient loops all stay live, because those
/// ride sibling components on the same entity ([`WaterChunkInfo`], [`FoamPatch`],
/// [`LiquidSoundSource`]) rather than its `Visibility`.
///
/// It exists because "is that thing occluding the NPC a liquid surface?" was otherwise unanswerable
/// from inside the game. A liquid surface is opaque where it is deep (`WATER_DEEP_ALPHA` = 1.0) and
/// fully opaque for the fullbright kinds, and it carries no silhouette of its own that reads as
/// *water* at a glancing angle — so a mis-placed one is indistinguishable by eye from a mis-placed
/// wall, and both look like a hard straight seam across the scene. One A/B now separates them; the
/// alternative was a screenshot argument. Same shape as `$WOW_NO_PARTICLES` / `$WOW_NO_FFX`.
///
/// Runs on `Added` so it costs one filtered query per frame and catches streamed-in surfaces too
/// (both spawn paths insert [`LiquidSurface`], MCLQ and WMO alike).
pub(super) fn hide_liquid_surfaces(mut surfaces: Query<&mut Visibility, Added<LiquidSurface>>) {
    for mut vis in &mut surfaces {
        *vis = Visibility::Hidden;
    }
}

/// What the **above-water ambient-loop system** needs beyond the surface's geometry (wow-re
/// `liquid-ambience-loop.md`, decision 0506): the sound-class nibble the driver resolves through
/// `SoundWaterType.dbc`. Attached to **every** liquid surface, the fullbright kinds included (the
/// Ironforge lava rumble, Undercity slime).
///
/// It used to carry its own copy of the footprint — bounds + a surface height — because when 0506
/// wrote it, magma and slime carried no [`WaterChunkInfo`] to read them from. 0634 gave every kind
/// one (that is what made lava swimmable), so the copy became a third set of numbers describing the
/// same surface, and its height stayed the chunk maximum after the grid sample landed. The driver
/// queries `(&LiquidSoundSource, &WaterChunkInfo)` instead — the pairing both spawn paths already
/// guarantee — and reads the geometry from the one component that owns it.
#[derive(Component)]
pub(crate) struct LiquidSoundSource {
    /// The surface's sound-class nibble (`class = n & 3`, `FluidSpeed = n & 0xc`).
    pub(crate) nibble: u8,
}

/// Spawn a set of water surfaces — one flat mesh per [`LiquidMesh`], on its [`LiquidKind`]'s shared
/// animated material. Used by the `AdtTile` pipeline (`terrain_stream`). No-op when the client has no
/// data (`liquid_assets` absent) or a kind's frames didn't load. Spawned entities are pushed onto
/// `entities` so they despawn with their tile.
pub(crate) fn spawn_liquids<'a>(
    commands: &mut Commands,
    liquids: impl Iterator<Item = &'a LiquidMesh>,
    liquid_assets: Option<&LiquidAssets>,
    meshes: &mut Assets<Mesh>,
    entities: &mut Vec<Entity>,
) {
    let Some(liquid) = liquid_assets else {
        return;
    };
    for lq in liquids {
        // ADT liquid always takes the SCENE fog: the ADT liquid passes submit no fog block of their
        // own, so they draw under the once-a-frame scene submit (wow-re `fog-env-state` §5).
        //
        // And it NEVER scrolls — `scroll: false` here is not a default, it is the finding. Shipped
        // ADT magma is nibble 6 throughout, the very nibble that scrolls on the WMO path, but the
        // ADT queue is a different kernel that hard-zeroes the v-translate ([`scrolls`]). Deriving
        // the flag from `lq.sound_nibble` here — the obvious-looking thing — would slide every
        // Searing Gorge and Burning Steppes lava pool in the game.
        let Some(material) = liquid.material(lq.kind, false, false) else {
            continue; // this kind's frames failed to load (warned at setup)
        };
        // The world-space liquid grid (MCLQ positions are already absolute WoW, so the IDENTITY
        // transform is a no-op round-trip).
        let info = wet_footprint(lq, &Transform::IDENTITY, LiquidSource::AdtChunk);
        let foam = !lq.kind.is_fullbright(); // white surf is a water thing
        entities.push(
            commands
                .spawn((
                    Mesh3d(meshes.add(liquid_bevy_mesh(lq))),
                    MeshMaterial3d(material),
                    Transform::IDENTITY,
                    LiquidSurface,
                    info,
                    LiquidSoundSource {
                        nibble: lq.sound_nibble,
                    },
                ))
                .id(),
        );
        // Foam is water-only; the cells it clips against ride `info`, not the marker.
        if foam {
            commands
                .entity(*entities.last().expect("just pushed"))
                .insert(FoamPatch);
        }
    }
}

/// Build the Bevy render mesh for one [`LiquidMesh`]: positions mapped WoW→Bevy (`lq.positions` are
/// raw WoW coords — absolute for MCLQ, WMO-model-local for WMO liquid), a flat up normal, the tiling
/// UVs, and the per-vertex swatch `V` packed into UV1.x for the shader's colour/opacity ramp. The
/// caller decides the surface's world placement via the spawned entity's `Transform` (`IDENTITY` for
/// absolute MCLQ water; the WMO placement transform for WMO liquid).
fn liquid_bevy_mesh(lq: &LiquidMesh) -> Mesh {
    let positions: Vec<[f32; 3]> = lq
        .positions
        .iter()
        .map(|p| wow_to_bevy(*p).to_array())
        .collect();
    let n = positions.len();
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    // Flat surface: WoW up (0,0,1) → Bevy up (0,1,0). The shader lights against this (rotated into
    // world by the entity transform) + the sun.
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; n]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, lq.uvs.clone());
    // UV1.x carries the per-vertex swatch depth (0..1) for the shader's opacity ramp.
    let uv1: Vec<[f32; 2]> = lq.depths.iter().map(|&d| [d, 0.0]).collect();
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv1);
    mesh.insert_indices(Indices::U32(lq.indices.clone()));
    mesh
}

/// Spawn a WMO group's embedded liquid surfaces (Stormwind's canals + fountains, the Ironforge lava,
/// dungeon pools) at the building's placement `transform`, on the shared per-kind liquid material —
/// the same animated water render as MCLQ, but its geometry is WMO-model-local (built by
/// `benilla_formats::wmo_group_liquid_mesh`) so the placement transform lifts it into the world.
///
/// No-op when the client has no data (`liquid_assets` absent) or a kind's frames didn't load. Each
/// WATER surface also carries a world-space [`WaterChunkInfo`] + [`FoamPatch`] (both built by baking the
/// placement transform into the raw liquid coords, [`world_wow`]) so the whole water-interaction stack
/// sees WMO liquid exactly like MCLQ: swimming ([`crate::player::swim`]), the underwater murk
/// ([`detect_submersion`]), the wading splash/footstep sounds, AND the `CWater0Ripple` wade wake /
/// standing ring ([`crate::water_fx`], which builds each foam decal from the wet-cell lattice). The
/// foam's world-axis texgen + per-triangle overlap consume the transformed cells fine, so a rotated
/// canal's ring is still correctly world-oriented. Spawned entities are pushed onto `entities` so they
/// despawn with the placement.
///
/// `interior` is the owning group's `MOGI & 0x48 == 0` class, and it selects the **fog block** the
/// surface draws under — the Great Forge's lava hazes with the forge's own fog, Stormwind's open
/// canals with the sky's (see [`LiquidKey`]).
///
/// `pool` is the surface's **scope** (see [`WmoPool`]): the room it belongs to, and that room's own
/// floor. A liquid footprint has no floor of its own, so an unscoped pool claims every position
/// under its XY forever — the Uldaman entrance read as submerged under a mushroom cave's water
/// 186 yd overhead (0696), and Undercity's upper slime submerged the rooms 115 yd below it (0701).
#[allow(clippy::too_many_arguments)] // one param per concern: assets, placement, fog block, scope
pub(crate) fn spawn_wmo_liquids<'a>(
    commands: &mut Commands,
    liquids: impl Iterator<Item = &'a LiquidMesh>,
    liquid_assets: Option<&LiquidAssets>,
    meshes: &mut Assets<Mesh>,
    transform: Transform,
    interior: bool,
    pool: WmoPool,
    entities: &mut Vec<Entity>,
) {
    let Some(liquid) = liquid_assets else {
        return;
    };
    for lq in liquids {
        // `interior` picks the fog block, not the look: an interior group's pool is drawn by the WMO
        // liquid pass, which re-submits the smoothed interior fog under the same `[0xca7f00]` gate as
        // the WMO geometry pass — so the pool fogs exactly like the walls around it (see [`LiquidKey`]).
        // This is the ONE path that can scroll, and the nibble — not the kind — is what decides it:
        // Blackrock's lava (6) creeps, the Ironforge Great Forge's (2) does not, and both are
        // `LiquidKind::Magma` drawing the same sheet ([`scrolls`]).
        let scroll = scrolls(lq.sound_nibble);
        let Some(material) = liquid.material(lq.kind, interior, scroll) else {
            continue; // this kind's frames failed to load (warned at setup)
        };
        if scroll {
            // Rare (only nibble 6/7 pools reach it) and the only outside view of a decision that is
            // otherwise invisible until you stand over the pool and watch it for ten seconds.
            debug!(
                "liquid: {:?} nibble {} takes the scroll lane",
                lq.kind, lq.sound_nibble
            );
        }
        let surface = commands
            .spawn((
                Mesh3d(meshes.add(liquid_bevy_mesh(lq))),
                MeshMaterial3d(material),
                transform,
                LiquidSurface,
                // The ambient-loop source rides EVERY kind — the fullbright lava/slime hum too
                // (0506). It reads its geometry off the `WaterChunkInfo` inserted below.
                LiquidSoundSource {
                    nibble: lq.sound_nibble,
                },
            ))
            .id();
        // The swim/submersion grid rides EVERY kind, magma and slime included — that is what
        // makes Blackrock's lava and Undercity's slime swimmable instead of something you fall
        // through (decision 0634, bugs B24/B25). It used to be gated on `!is_fullbright()` because
        // `WaterChunkInfo` carried no kind, so tagging lava would have swum the player under a teal
        // *water* murk with white foam. The component carries [`LiquidKind`] now and the
        // water-flavoured consumers filter on it (`water_surface_at`, `detect_submersion`), so the
        // exclusion is no longer what keeps lava from looking like a lake.
        //
        // Lava/slime **damage** is still not modelled — a named gap, not a reason to keep the
        // geometry non-solid.
        commands.entity(surface).insert(wet_footprint(
            lq,
            &transform,
            LiquidSource::WmoGroup(pool),
        ));
        // Foam stays water-only: it is white surf, and there is no such thing on magma.
        if !lq.kind.is_fullbright() {
            commands.entity(surface).insert(FoamPatch);
        }
        entities.push(surface);
    }
}

/// Each kind's animated frame set: `(kind, XTextures subdir, file stem, frame count on disk)`.
/// Frames are `XTextures\<dir>\<stem>.<1..=count>.blp` (256² RGBA, RGB dark + alpha ripple).
const FRAME_SETS: &[(LiquidKind, &str, &str, u32)] = &[
    (LiquidKind::Still, "river", "lake_a", 30),
    (LiquidKind::Rapids, "river", "fast_a", 16),
    (LiquidKind::Ocean, "ocean", "ocean_h", 30),
    // The fullbright kinds: opaque + unlit + fogged, the animated texture IS the body colour, there
    // being no vertex colour or depth LUT to modulate it by (VERIFIED wow-re
    // `liquid-render-state-sided` §5). **Magma reaches here from BOTH liquid systems** — the WMO
    // MLIQ pools *and* the ADT MCLQ magma queue (B21: the 128 open-world lava chunks, Burning
    // Steppes/Searing Gorge/Un'Goro); only slime is WMO-only, the reference having no ADT queue for
    // it at all. See [`benilla_formats::LiquidKind`].
    (LiquidKind::Magma, "lava", "lava", 30),
    (LiquidKind::Slime, "slime", "slime", 30),
];

pub(super) fn setup_liquid(
    mut commands: Commands,
    config: Option<Res<RenderConfig>>,
    world_assets: Option<ResMut<WorldAssets>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<LiquidMaterial>>,
) {
    let (Some(_config), Some(mut world_assets)) = (config, world_assets) else {
        return; // no client data → no terrain, so no water either
    };
    // No light seed and no per-frame push: light, fog and both water swatches come off the shared
    // global-light buffer (`lighting::global_light`), which `build_light_data` has already packed by
    // the time anything draws — the same path terrain and the models take.
    let mut assets = LiquidAssets::default();
    for &(kind, dir, stem, count) in FRAME_SETS {
        let Some((frames, frame_count)) =
            load_frame_array(&mut world_assets, &mut images, dir, stem, count)
        else {
            warn!("liquid: no frames for {stem} — {kind:?} water will not render");
            continue;
        };
        // Blend state is per KIND, and this is where it is decided (VERIFIED wow-re
        // `liquid-render-state-sided` — the device render-state *defaults* at `0x593bf0` are the baseline
        // a liquid batch draws under, because every setter in the reference is Push/Pop-scoped):
        //
        //   * water / ocean — the reference sets EGxBlend 2 (SRC_ALPHA / INV_SRC_ALPHA) and depth-write
        //     OFF, which is exactly Bevy's `AlphaMode::Blend` transparent pass. Unchanged. (Both are
        //     gated on the fancy-water cvar `[0xc9a324]`, which we do not model — a separate lane.)
        //   * magma / slime — blend state stays at the default **0**, i.e. `glDisable(GL_BLEND)`, with
        //     the baseline depth test *and* depth write both ON (ids `0x10`/`0x12` = 1). The ADT lava
        //     pass says so explicitly (`0x6855e2 (0x07, 0)`) and the WMO magma/slime arm never touches
        //     blend at all. So they are genuinely opaque and belong in the OPAQUE pass: riding the
        //     transparent pass with depth-write off is how a lava sheet fails to occlude what is behind
        //     it. Foam already carries `depth_bias: 1.0` for the coplanar tie, so it still wins.
        //
        // Two-sided is universal — all four liquid passes force GL_CULL_FACE off at entry against a
        // cull-ON baseline, and `glFrontFace` is never imported, so winding is moot (§6).
        let alpha_mode = if kind.is_fullbright() {
            AlphaMode::Opaque
        } else {
            AlphaMode::Blend
        };
        // One material per (FOG BLOCK × SCROLL) combination the kind can actually take (see
        // [`LiquidKey`]), all sharing the one decoded frame array (`frames` is a handle; the clone is
        // a refcount, not a second 30 × 256² decode). Every other input is identical, so this is the
        // whole cost of letting an interior room's pool fog like the room instead of like the sky
        // outside it — and of letting Blackrock's lava creep while Ironforge's sits still.
        //
        // The scroll variant exists only for the fullbright kinds: [`scrolls`] can only ever answer
        // true for nibbles 6/7, which map to `Magma`/`Slime` and nothing else, so building one for
        // water would be a material nothing can ever look up.
        let scroll_lanes: &[bool] = if kind.is_fullbright() {
            &[false, true]
        } else {
            &[false]
        };
        for (interior, &scroll) in [false, true]
            .into_iter()
            .flat_map(|i| scroll_lanes.iter().map(move |s| (i, s)))
        {
            let material = materials.add(ExtendedMaterial {
                base: StandardMaterial {
                    // We do our own (WoW) lighting in the shader.
                    unlit: true,
                    alpha_mode,
                    cull_mode: None,
                    double_sided: true,
                    // The transparent water kinds take the fixed water-pass slot: below every
                    // unclassified world transparent, above the far-side effects (the reference's
                    // 0x483460 interleave — `sky_order::WATER_BIAS`, where the byte story lives).
                    // Opaque kinds (magma/slime) draw in the opaque pass and take no rung.
                    depth_bias: if kind.is_fullbright() {
                        0.0
                    } else {
                        crate::sky_order::WATER_BIAS
                    },
                    ..default()
                },
                extension: LiquidExt {
                    frames: frames.clone(),
                    // x = fullbright (magma/slime: the animated texture is the opaque body, skipping
                    // the swatch and N·L — but NOT the fog, which every liquid kind takes); y = read
                    // the ocean swatch rows rather than the river/lake ones; z = fog with the WMO
                    // INTERIOR block; w = water's own sun-sheen exponent.
                    kind: Vec4::new(
                        if kind.is_fullbright() { 1.0 } else { 0.0 },
                        if kind == LiquidKind::Ocean { 1.0 } else { 0.0 },
                        if interior { 1.0 } else { 0.0 },
                        WATER_SHININESS,
                    ),
                    // x = frame 0 (index driven by `animate_liquid`); y = frame count; z = the
                    // v-scroll offset in texture repeats (also `animate_liquid`'s, and left at 0
                    // forever on a non-scrolling material); w unused.
                    anim: Vec4::new(0.0, frame_count as f32, 0.0, 0.0),
                    light_buf: world_assets.shared_light.clone(),
                },
            });
            assets.materials.insert(
                LiquidKey {
                    kind,
                    interior,
                    scroll,
                },
                LiquidEntry {
                    material,
                    frame_count,
                    scroll,
                },
            );
        }
    }
    // Frame SETS, not materials — a set backs every fog-block/scroll variant of its kind.
    info!(
        "liquid: loaded {} water frame set(s)",
        FRAME_SETS
            .iter()
            .filter(|(k, ..)| assets.material(*k, false, false).is_some())
            .count()
    );
    commands.insert_resource(assets);
}

/// Decode frames `1..=count` for a kind — each with its BLP **authored mip chain** — into one
/// repeating, mipmapped + anisotropic `texture_2d_array` (`assets::liquid_frame_array`; mips are what
/// stop the ripple aliasing into sparkle at distance). Stops at the first missing/non-square/
/// size-mismatched frame (the on-disk sets are contiguous 256² runs). Returns the image handle + the
/// number of frames actually loaded, or `None` if none decoded.
fn load_frame_array(
    world_assets: &mut WorldAssets,
    images: &mut Assets<Image>,
    dir: &str,
    stem: &str,
    count: u32,
) -> Option<(Handle<Image>, u32)> {
    let mut frames: Vec<BlpMipChain> = Vec::new();
    let mut size = 0u32;
    for i in 1..=count {
        let path = format!("XTextures\\{dir}\\{stem}.{i}.blp");
        let Ok(chain) = read_texture_mip_chain(&mut world_assets.chain.lock_recover(), &path)
        else {
            break;
        };
        if chain.width != chain.height {
            break; // water frames are square; bail rather than build a ragged array
        }
        if size == 0 {
            size = chain.width;
        } else if chain.width != size {
            break; // a frame at a different resolution can't share the array
        }
        frames.push(chain);
    }
    if frames.is_empty() {
        return None;
    }
    let loaded = frames.len() as u32;
    Some((images.add(liquid_frame_array(frames)), loaded))
}

/// Advance every liquid material's frame index at [`ANIM_FPS`] off Bevy **real** `Time` (wall-clock,
/// mirroring the reference's `GetTickCount`-driven cycler — NOT the day/night game clock), and the
/// **v-scroll offset** on the materials that take one ([`scrolls`]). Writes only on the
/// [`ANIM_FPS`] tick edge: `Assets::get_mut` alone marks the asset Modified and feeds the
/// respecialization pipeline (the mark-changed scan + `Changed<Mesh3d>` sweeps) every frame — the
/// 0353 demand-price law; between ticks the frame index cannot have changed.
///
/// The scroll rides the same 24 Hz edge rather than getting its own per-frame write. It is a
/// continuous quantity, so that quantises it — but to 1/24 s, i.e. **1/240th of a repeat** per step
/// on a 10 s sawtooth, which is far below what a 256² sheet can show, and it keeps the material
/// upload budget exactly where 0353 put it. (The reference is quantised too, just differently: it
/// rebuilds the matrix per draw off a millisecond clock.)
pub(super) fn animate_liquid(
    time: Res<Time>,
    liquid: Option<Res<LiquidAssets>>,
    mut materials: ResMut<Assets<LiquidMaterial>>,
    mut last_ticks: Local<Option<u32>>,
    surfaces: Query<(), With<MeshMaterial3d<LiquidMaterial>>>,
) {
    let Some(liquid) = liquid else {
        return;
    };
    // No liquid mesh in the world → nothing samples these materials; skip the whole cycle (the
    // startup-built kind set otherwise re-uploads ~10 materials at every 24 Hz edge on maps with
    // no water at all). The cycler is wall-clock ([`ANIM_FPS`] × elapsed), so when the first
    // surface streams in the index resumes at the current wall frame — exactly the reference's
    // `GetTickCount`-driven phase.
    if surfaces.is_empty() {
        return;
    }
    // Captures pin the cycler to frame 0: the wall-clock at screenshot time varies with load
    // times, so any framing with open water diffs differently run to run — the flake substrate's
    // baseline redesign caught (MAE 3.97 → 0.009 pinned; decision 0600). One clause, one frame.
    let ticks = if crate::dev_state::deterministic_run() {
        0
    } else {
        (time.elapsed_secs() * ANIM_FPS) as u32
    };
    if *last_ticks == Some(ticks) {
        return;
    }
    *last_ticks = Some(ticks);
    // The scroll's sawtooth, off the SAME quantised clock as the frame index so the two can never
    // disagree about what time it is: `fmod(t, 10) · 0.1` ∈ [0, 1) repeats. Pinned to 0 in a
    // deterministic run for exactly the reason the frame index is (0600).
    let scroll = if crate::dev_state::deterministic_run() {
        0.0
    } else {
        (ticks as f32 / ANIM_FPS) % SCROLL_PERIOD_SECS * SCROLL_REPEATS_PER_SEC
    };
    for entry in liquid.materials.values() {
        // Gated per entry: a single-frame liquid (`frame_count` 1) never changes index, and the
        // shared cache holds every kind ever built — re-marking them all Modified on each tick
        // edge re-uploaded ~10 materials/frame on maps with no water at all.
        let frame = (ticks % entry.frame_count.max(1)) as f32;
        let offset = if entry.scroll { scroll } else { 0.0 };
        benilla_assets::write_gated(
            &mut materials,
            &entry.material,
            |m| m.extension.anim.x != frame || m.extension.anim.z != offset,
            |m| {
                m.extension.anim.x = frame;
                m.extension.anim.z = offset;
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scroll gate, nibble by nibble — every trap in [`scrolls`] as an assertion.
    ///
    /// The failure this guards is not "lava doesn't move"; it is lava moving in the **wrong
    /// places**, which is far harder to see and impossible to unsee once shipped.
    #[test]
    fn only_nibbles_six_and_seven_scroll() {
        // The two that do: Blackrock's lava and Stratholme's slime.
        assert!(scrolls(6), "magma variant 6 takes the animated matrix");
        assert!(scrolls(7), "slime variant 7 takes the animated matrix");

        // The two that reach the SAME kernel and draw the SAME sheet, and still do not scroll —
        // the Ironforge Great Forge (2) and the Stratholme raid / Ahn'Qiraj slime (3).
        assert!(!scrolls(2), "the Great Forge's lava is static");
        assert!(!scrolls(3), "nibble-3 slime is static");

        // The near-miss that a `nibble & 0xc == 4` test would wrongly admit: 4 satisfies it, but it
        // is a *river* variant that never reaches the magma/slime kernel at all.
        assert!(!scrolls(4), "nibble 4 is lake_a, not a hazard liquid");

        // Everything else in the table, including the hole nibble.
        for n in [0u8, 1, 5, 8, 0xf] {
            assert!(!scrolls(n), "nibble {n} must not scroll");
        }
    }

    /// A scrolling material exists for the fullbright kinds and **only** for them — a scroll lane
    /// built for water would be a material no lookup can ever reach ([`setup_liquid`]).
    #[test]
    fn only_fullbright_kinds_can_take_a_scroll_lane() {
        for &(kind, ..) in FRAME_SETS {
            let wants_lane = kind.is_fullbright();
            assert_eq!(
                wants_lane,
                [2u8, 3, 6, 7]
                    .iter()
                    .any(|&n| scrolls(n) && LiquidKind::from_nibble(n) == Some(kind)),
                "{kind:?}: scroll lane and the nibbles that can ask for one must agree"
            );
        }
    }
}

/// Which fog block a WMO pool takes, against the **real client data**. Skips when the 1.12.1 client
/// isn't present (the repo never carries Blizzard data).
#[cfg(test)]
mod real_data {
    use super::LiquidKind;
    use benilla_formats::{parse_wmo_root, wmo_group_liquid_mesh};
    use std::collections::HashMap;

    /// **Which of the shipped hazard pools actually scroll** — [`super::scrolls`] against the real
    /// data, so the gate is pinned to content rather than to a reading of the binary.
    ///
    /// The whole reason this needs a test: "does lava scroll?" has no answer, only a per-pool one.
    /// The nibble is authored per WMO group, `LiquidKind` collapses it (2 and 6 are both `Magma`,
    /// drawing the same `lava.blp`), and the two behaviours sit **inside one building** — so a gate
    /// that quietly regressed to "all magma" or "no magma" would look plausible everywhere and be
    /// wrong nearly everywhere. These counts are the shipped 1.12.1 numbers, measured:
    ///
    /// * **Ironforge is the two-sided case, and it is why the director's report needed a location.**
    ///   Eleven magma groups: **9 at nibble 2 (static)** and **2 at nibble 6 (scrolling)**, plus 2
    ///   still-water groups. Lava that moves and lava that does not, in the same forge.
    /// * **Blackrock Mountain — the pin behind `blackrock_lava_is_below_the_feet_not_above_it`** —
    ///   is nibble 6 throughout, in the outer mountain and both instance WMOs. All of it creeps.
    /// * **Slime splits the same way**: Undercity's 38 canals are nibble 3 and stand still;
    ///   Stratholme's are nibble 7 and flow.
    #[test]
    fn only_the_nibble_six_and_seven_pools_scroll_in_the_shipped_data() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        // Every liquid-bearing group of a WMO, as `(nibble, kind)` → count.
        let mut census = |root_path: &str| -> HashMap<(u8, LiquidKind), usize> {
            let bytes = chain.read_file(root_path).expect("root readable");
            let root = parse_wmo_root(&bytes).expect("parse root");
            let stem = root_path
                .strip_suffix(".wmo")
                .unwrap_or(root_path)
                .to_string();
            let mut tally = HashMap::new();
            for gi in 0..root.group_count() as usize {
                let Ok(gb) = chain.read_file(&format!("{stem}_{gi:03}.wmo")) else {
                    continue;
                };
                if let Some(m) = wmo_group_liquid_mesh(&gb) {
                    *tally.entry((m.sound_nibble, m.kind)).or_default() += 1;
                }
            }
            tally
        };

        let ironforge = census("world\\wmo\\khazmodan\\cities\\ironforge\\ironforge.wmo");
        assert_eq!(
            ironforge,
            HashMap::from([
                ((2, LiquidKind::Magma), 9),
                ((4, LiquidKind::Still), 2),
                ((6, LiquidKind::Magma), 2),
            ]),
            "Ironforge's liquid census moved",
        );

        let blackrock = census("world\\wmo\\dungeon\\az_blackrock\\blackrock.wmo");
        assert_eq!(
            blackrock,
            HashMap::from([((6, LiquidKind::Magma), 2)]),
            "Blackrock Mountain's lava — the `.go xyz -7531.21 -1123.64 172.58` pin — is all nibble 6",
        );

        let undercity = census("world\\wmo\\lorderon\\undercity\\undercity.wmo");
        assert_eq!(
            undercity,
            HashMap::from([((3, LiquidKind::Slime), 38)]),
            "Undercity's slime is nibble 3 throughout",
        );

        let stratholme = census("world\\wmo\\dungeon\\ld_stratholme\\stratholme.wmo");
        assert_eq!(
            stratholme,
            HashMap::from([((7, LiquidKind::Slime), 3)]),
            "Stratholme's slime is nibble 7 throughout",
        );

        // And the gate's verdict over all of it — the same sheet, two behaviours.
        assert_eq!(
            ironforge
                .iter()
                .filter(|((n, _), _)| super::scrolls(*n))
                .map(|(_, c)| c)
                .sum::<usize>(),
            2,
            "exactly 2 of Ironforge's 11 magma pools scroll",
        );
        for (label, tally) in [("blackrock", &blackrock), ("stratholme", &stratholme)] {
            assert!(
                tally.keys().all(|(n, _)| super::scrolls(*n)),
                "{label}: every pool scrolls",
            );
        }
        assert!(
            undercity.keys().all(|(n, _)| !super::scrolls(*n)),
            "Undercity's slime stands still",
        );
    }

    /// **Which fog block each liquid-bearing WMO group takes**, against the real client files.
    ///
    /// This is decision 0691's follow-on lane. A WMO group's own pool is drawn by the pass that
    /// re-submits the smoothed INTERIOR fog block (`0x6b6323`-`0x6b6342`), gated on the same
    /// `[0xca7f00]` as the WMO *geometry* pass — so an indoor pool hazes with its room and an open-air
    /// one must not. `spawn_wmo_liquids` decides that from the group's `MOGI & 0x48 == 0`, and an
    /// inverted flag is invisible in any screenshot taken where the two blocks happen to agree (in dry
    /// Undercity both clamp to 1 within ~58 yd). So pin it on the two buildings that straddle the line,
    /// with the real numbers:
    ///
    /// * **Undercity** — 38 liquid groups, and all but ONE are interior. Its slime canals are the
    ///   director's original report; the lone exterior one (group 7) is the granularity working, not
    ///   noise, and is what makes this a real two-sided test rather than a constant.
    /// * **Stormwind** — 22 liquid groups (canals + fountains), every one EXTERIOR. Reading these as
    ///   interior would haze the open city's water with an indoor triple under an open sky.
    ///
    /// Skips when the 1.12.1 client isn't present (the repo never carries Blizzard data).
    #[test]
    fn a_wmo_pools_fog_block_follows_its_groups_interior_class() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        // Which groups of a WMO carry liquid, and whether each is an interior group.
        let liquid_groups = |chain: &mut benilla_formats::Chain, root_path: &str| {
            let bytes = chain.read_file(root_path).expect("root readable");
            let root = parse_wmo_root(&bytes).expect("parse root");
            let stem = root_path
                .strip_suffix(".wmo")
                .unwrap_or(root_path)
                .to_string();
            (0..root.group_count() as usize)
                .filter_map(|gi| {
                    let gb = chain.read_file(&format!("{stem}_{gi:03}.wmo")).ok()?;
                    wmo_group_liquid_mesh(&gb)?;
                    Some((gi, root.group_infos().get(gi).is_some_and(|g| g.interior)))
                })
                .collect::<Vec<_>>()
        };

        let uc = liquid_groups(&mut chain, "world\\wmo\\lorderon\\undercity\\undercity.wmo");
        let exterior: Vec<usize> = uc.iter().filter(|(_, i)| !i).map(|(g, _)| *g).collect();
        assert_eq!(uc.len(), 38, "Undercity's liquid group count moved: {uc:?}");
        assert_eq!(
            exterior,
            vec![7],
            "exactly one Undercity liquid group is exterior (the flag is per group, not per building)",
        );

        let sw = liquid_groups(
            &mut chain,
            "world\\wmo\\azeroth\\buildings\\stormwind\\stormwind.wmo",
        );
        assert_eq!(sw.len(), 22, "Stormwind's liquid group count moved: {sw:?}");
        assert!(
            sw.iter().all(|(_, interior)| !interior),
            "Stormwind's canals and fountains are open to the sky — none takes the interior fog: {sw:?}",
        );
    }
}
