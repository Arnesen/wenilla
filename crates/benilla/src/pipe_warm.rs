//! The pipeline warm pass + its instrument — decision 0837 (the B181 city-approach stall).
//!
//! **Why this exists:** on macOS, Bevy compiles every GPU pipeline **synchronously on the render
//! thread** — `bevy_render`'s `create_pipeline_task` has a `target_os = "macos"` carve-out (0.18
//! and 0.19 both) that `block_on`s the build regardless of `synchronous_pipeline_compilation`,
//! and the Metal half of that build runs out-of-process in `MTLCompilerService` (near-zero
//! process CPU while the frame is blocked). So any pipeline variant first drawn *live* is a
//! frame-long stall the app cannot pace; the only fix is compiling everything where a stall is
//! invisible — behind the loading cover, where 0540 put the warm-up. (The worst offender — the
//! per-batch-index depth bias that made every WMO batch its own pipeline, ~3000 variants at
//! Stormwind — left the pipeline key in this same decision: the nudge now rides `sun_scale.y`
//! into `wow_model.wgsl`'s vertex stage as uniform data.)
//!
//! The pieces:
//!
//! - [`WarmPass`] + `spawn_menagerie` — the warm pass: one tiny quad per reachable model-lane
//!   pipeline variant, parented to the world camera, spawned when the entry cover rises; the
//!   loading screen's clear condition holds on [`WarmPass::satisfied`] until the pipeline cache
//!   drains (10 s backstop, 0737's rule), then the menagerie despawns. Captures skip it.
//! - [`PipeWatch`] — an `Arc` shared by the main and render worlds: how many pipelines the cache
//!   has ever queued, how many have settled (Ok/Err), and whether a cover currently hides the
//!   frame (loading screen up, or not in world — the glue scene is its own cover, 0540).
//! - [`watch_pipelines`] (render world, after the cache's own process step): maintains the
//!   counters and — the permanent tripwire — logs a `warn!` for every pipeline compiled while
//!   **uncovered**. That line firing in a session log IS the regression signal: it means the
//!   menagerie has a coverage hole (extend its loops, don't guess).
//! - `WOW_PIPE_TRACE=<path>` — the inventory dump: one line per pipeline creation (covered or
//!   not) with the full variant identity (shaders, defs, depth bias, blend, write mask, vertex
//!   buffers, cull), the ground truth the menagerie was built from.
//! - Two stream-trace columns (`pipes_new`, `pipes_pending` — see `perf::trace_stream`) so a
//!   compile burst is attributable on the same row as the frame that paid for it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use benilla_formats::{FogPolicy, ModelBlend, RenderSubmesh};
use bevy::camera::primitives::MeshAabb;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy::render::render_resource::{
    Buffer, CachedPipelineState, PipelineCache, PipelineDescriptor,
};
use bevy::render::{Render, RenderApp, RenderSystems};

use crate::char_select::ClientState;
use crate::loading_screen::LoadingScreen;
use crate::model_render::{model_material, zfill_material, MaterialCache, ShadeSel};
use crate::terrain::WowModelMaterial;

/// The cross-world channel: cloned into the render app at plugin build. Frame alignment between
/// the two worlds is ±1 frame under pipelined rendering — fine for counters and a tripwire.
#[derive(Resource, Clone)]
pub(crate) struct PipeWatch(pub(crate) Arc<PipeShared>);

pub(crate) struct PipeShared {
    /// Pipelines the cache has ever queued (its vec only grows; ids are indices).
    pub(crate) created: AtomicUsize,
    /// Of those, how many have settled — `Ok` or a non-retryable `Err`. A retryable error
    /// (shader not loaded yet) flips back to `Queued` and correctly reads as pending.
    pub(crate) settled: AtomicUsize,
    /// Main-world truth: an opaque cover hides the frame (loading screen, or not `InWorld`).
    pub(crate) covered: AtomicBool,
}

pub(crate) fn plugin(app: &mut App) {
    let shared = Arc::new(PipeShared {
        created: AtomicUsize::new(0),
        settled: AtomicUsize::new(0),
        covered: AtomicBool::new(true),
    });
    app.insert_resource(PipeWatch(shared.clone()));
    app.init_resource::<WarmPass>();
    app.add_systems(Last, publish_cover);
    // Before the Present stage so the loading screen reads this frame's gate, not last frame's.
    app.add_systems(
        Update,
        run_warm_pass.before(crate::schedule::WorldStage::Present),
    );
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    render_app.insert_resource(PipeWatch(shared));
    render_app.add_systems(Render, watch_pipelines.in_set(RenderSystems::Cleanup));
}

/// Main world → render world: is the frame covered right now?
fn publish_cover(
    watch: Res<PipeWatch>,
    loading: Res<LoadingScreen>,
    state: Res<State<ClientState>>,
) {
    let covered = loading.covering() || *state.get() != ClientState::InWorld;
    watch.0.covered.store(covered, Ordering::Relaxed);
}

/// Render world, after `PipelineCache::process_pipeline_queue_system` has merged this frame's new
/// pipelines and started (= on macOS: finished) their builds. `seen` is how many cache entries the
/// previous frame had — everything past it is new this frame.
fn watch_pipelines(cache: Res<PipelineCache>, watch: Res<PipeWatch>, mut seen: Local<usize>) {
    let covered = watch.0.covered.load(Ordering::Relaxed);
    let mut total = 0usize;
    let mut settled = 0usize;
    for (id, pipe) in cache.pipelines().enumerate() {
        total += 1;
        if matches!(
            pipe.state,
            CachedPipelineState::Ok(_) | CachedPipelineState::Err(_)
        ) {
            settled += 1;
        }
        if id >= *seen {
            let line = describe(&pipe.descriptor);
            if covered {
                debug!("pipeline compiled (covered) [{id}] {line}");
            } else {
                // THE TRIPWIRE: after 0837, a live compile is a stall the director can feel —
                // this line in a session log means the warm pass has a coverage hole.
                warn!("pipeline compiled LIVE [{id}] {line}");
            }
            if let Ok(path) = std::env::var("WOW_PIPE_TRACE") {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    let cov = if covered { "covered" } else { "LIVE" };
                    let _ = writeln!(f, "[{id}] {cov} {line}");
                }
            }
        }
    }
    *seen = total;
    watch.0.created.store(total, Ordering::Relaxed);
    watch.0.settled.store(settled, Ordering::Relaxed);
}

/// One line of variant identity: everything that distinguishes this pipeline from its neighbours
/// (label + shaders + defs + the raster/depth/blend states), compact enough to grep and diff.
fn describe(desc: &PipelineDescriptor) -> String {
    fn defs(d: &[bevy::shader::ShaderDefVal]) -> String {
        let mut v: Vec<String> = d
            .iter()
            .map(|d| match d {
                bevy::shader::ShaderDefVal::Bool(k, true) => k.clone(),
                bevy::shader::ShaderDefVal::Bool(k, false) => format!("!{k}"),
                bevy::shader::ShaderDefVal::Int(k, i) => format!("{k}={i}"),
                bevy::shader::ShaderDefVal::UInt(k, u) => format!("{k}={u}"),
            })
            .collect();
        v.sort();
        v.join("+")
    }
    match desc {
        PipelineDescriptor::RenderPipelineDescriptor(d) => {
            let label = d.label.as_deref().unwrap_or("?");
            let vs = d
                .vertex
                .shader
                .path()
                .map_or_else(|| format!("{:?}", d.vertex.shader.id()), |p| p.to_string());
            let vbufs: Vec<String> = d
                .vertex
                .buffers
                .iter()
                .map(|b| {
                    let locs: Vec<String> = b
                        .attributes
                        .iter()
                        .map(|a| a.shader_location.to_string())
                        .collect();
                    format!("stride{}@[{}]", b.array_stride, locs.join(","))
                })
                .collect();
            let (bias, dw, cmp) = d.depth_stencil.as_ref().map_or_else(
                || (0, false, String::from("none")),
                |ds| {
                    (
                        ds.bias.constant,
                        ds.depth_write_enabled,
                        format!("{:?}", ds.depth_compare),
                    )
                },
            );
            let frag = d.fragment.as_ref().map_or_else(
                || String::from("frag=none"),
                |f| {
                    let fs = f
                        .shader
                        .path()
                        .map_or_else(|| format!("{:?}", f.shader.id()), |p| p.to_string());
                    let tgt = f.targets.iter().flatten().next().map_or_else(
                        || String::from("none"),
                        |t| format!("blend={:?} mask={:?}", t.blend, t.write_mask),
                    );
                    format!("fs={fs} fs_defs=[{}] {tgt}", defs(&f.shader_defs))
                },
            );
            format!(
                "label={label} vs={vs} vs_defs=[{}] bufs=[{}] cull={:?} bias={bias} depth_write={dw} cmp={cmp} {frag} samples={}",
                defs(&d.vertex.shader_defs),
                vbufs.join(";"),
                d.primitive.cull_mode,
                d.multisample.count,
            )
        }
        PipelineDescriptor::ComputePipelineDescriptor(d) => {
            let label = d.label.as_deref().unwrap_or("?");
            let cs = d
                .shader
                .path()
                .map_or_else(|| format!("{:?}", d.shader.id()), |p| p.to_string());
            format!("label={label} compute={cs} defs=[{}]", defs(&d.shader_defs))
        }
    }
}

// --------------------------------------------------------------------------------------------
// The warm pass — the fix half of 0837.
//
// The 0837 inventory (pipes1.log) showed the model lane's REACHABLE pipeline space is small once
// the batch-order axis left the key: 4 vertex layouts × the blend/depth-flag families, ~28
// observed in a wilderness-to-Stormwind leg. The menagerie below compiles that whole space
// behind the entry loading cover — one 1 cm quad per variant, parented to the world camera (the
// camera renders under the cover, 0540, so every quad's draw queues its pipeline through the
// production specialize path), the cover held (via `WarmPass::satisfied` in the loading screen's
// clear condition) until the cache drains, then the quads despawn. A variant this misses shows
// up as the tripwire's "compiled LIVE" warn — extend the loops, don't guess.

/// Marker on every menagerie entity.
#[derive(Component)]
struct WarmRig;

/// Main-world warm-pass state. The loading screen folds [`Self::satisfied`] into its clear
/// condition, so the cover holds while menagerie pipelines are still compiling.
#[derive(Resource, Default)]
pub(crate) struct WarmPass {
    /// `Time::elapsed_secs` when the menagerie spawned under the current cover; `None` = idle
    /// (no cover, or the pass already finished for this cover).
    spawned_at: Option<f32>,
    /// This cover's warm work is done (drained, timed out, or not applicable).
    done: bool,
}

impl WarmPass {
    /// Cover-lift gate: false while the menagerie still has pipelines in flight.
    pub(crate) fn satisfied(&self) -> bool {
        self.done
    }
}

/// The menagerie must have been extracted + drawn + its pipelines queued before `pending == 0`
/// means anything — under a second even on the entry frame's load.
const WARM_SETTLE_SECS: f32 = 0.25;
/// 0737's rule: never hold a cover unbounded. A timeout fires the tripwire-adjacent warn and
/// releases; the remaining compiles land live (the pre-0837 world, once, with a named cause).
const WARM_TIMEOUT_SECS: f32 = 10.0;

#[allow(clippy::too_many_arguments)] // a Bevy system: each param is one resource, the app's convention
fn run_warm_pass(
    mut commands: Commands,
    mut warm: ResMut<WarmPass>,
    watch: Res<PipeWatch>,
    loading: Res<LoadingScreen>,
    state: Res<State<ClientState>>,
    time: Res<Time>,
    camera: Query<Entity, With<crate::player::WorldCamera>>,
    rigs: Query<Entity, With<WarmRig>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<WowModelMaterial>>,
    mut cache: Local<MaterialCache>,
    shared_light: Option<Res<crate::lighting::SharedLightBuffer>>,
) {
    let covering = loading.covering() && *state.get() == ClientState::InWorld;
    if !covering {
        // No world cover → nothing to hold; a leftover menagerie (timeout, teleport race)
        // despawns. `done` stays true so the gate never blocks an uncovered frame.
        warm.done = true;
        warm.spawned_at = None;
        for e in &rigs {
            commands.entity(e).despawn();
        }
        return;
    }
    // Captures boot straight in-world, deterministic by construction — no menagerie in a shot.
    if crate::capture::scenario_active() {
        warm.done = true;
        return;
    }
    let now = time.elapsed_secs();
    let Some(spawned) = warm.spawned_at else {
        // The cover just rose (or the world just became live under one): raise the gate and
        // spawn the menagerie once the camera + shared light exist (both are entry-frame-early;
        // until they do, the gate holds the cover, which is exactly right).
        warm.done = false;
        let Ok(cam) = camera.single() else { return };
        let Some(light) = shared_light.as_ref() else {
            return;
        };
        warm.spawned_at = Some(now);
        let count = spawn_menagerie(
            &mut commands,
            cam,
            &mut meshes,
            &mut materials,
            &mut cache,
            &light.0,
        );
        info!("pipeline warm: menagerie up ({count} variants)");
        return;
    };
    if warm.done {
        return;
    }
    let pending = watch
        .0
        .created
        .load(Ordering::Relaxed)
        .saturating_sub(watch.0.settled.load(Ordering::Relaxed));
    if now - spawned >= WARM_SETTLE_SECS && pending == 0 {
        warm.done = true;
        for e in &rigs {
            commands.entity(e).despawn();
        }
        info!("pipeline warm: drained in {:.2}s", now - spawned);
    } else if now - spawned >= WARM_TIMEOUT_SECS {
        warm.done = true;
        for e in &rigs {
            commands.entity(e).despawn();
        }
        warn!("pipeline warm: TIMED OUT with {pending} pipelines pending — cover released");
    }
}

/// Spawn one tiny quad per reachable model-lane pipeline variant, parented to the world camera.
/// Materials come from the PRODUCTION builders (`model_material` / `zfill_material`) so the
/// variant encoding can never drift from the real spawn paths; meshes from the production
/// submesh builders so the vertex layouts can't either. Returns the entity count.
fn spawn_menagerie(
    commands: &mut Commands,
    cam: Entity,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<WowModelMaterial>,
    cache: &mut MaterialCache,
    light: &Buffer,
) -> usize {
    // The four vertex layouts the model lane ships (0837 dump: strides 32/48/56/72): static ×
    // {plain, vertex-colours} and their skinned twins. Statics are RENDER_WORLD-only, so their
    // Aabb is computed here and inserted explicitly (0832's rule); skinned twins keep main-world
    // data and `calculate_bounds` covers them.
    let mut layouts: Vec<(Handle<Mesh>, Option<bevy::camera::primitives::Aabb>, bool)> = Vec::new();
    for colors in [false, true] {
        let stat = benilla_assets::submesh_to_static_mesh(&warm_quad(colors, false));
        let aabb = stat.compute_aabb();
        layouts.push((meshes.add(stat), aabb, false));
        let skin = benilla_assets::submesh_to_skinned_mesh(&warm_quad(colors, true));
        layouts.push((meshes.add(skin), None, true));
    }

    // The material families. The full cross is deliberate: every branch here is authorable in an
    // M2/WMO (blends, the 0x10/0x08 depth flags, sidedness), and an over-warmed variant costs
    // milliseconds behind a loading bar once per run, while a missed one is a director-felt live
    // stall. The observed set (28) is the floor, not the target.
    let mut mats: Vec<Handle<WowModelMaterial>> = Vec::new();
    for two_sided in [false, true] {
        for blend in [
            ModelBlend::Opaque,
            ModelBlend::AlphaTest,
            ModelBlend::Blend,
            ModelBlend::Mod,
            ModelBlend::Mod2x,
        ] {
            for no_depth_write in [false, true] {
                for no_depth_test in [false, true] {
                    mats.push(model_material(
                        cache,
                        materials,
                        None,
                        blend,
                        two_sided,
                        false,
                        false,
                        false,
                        false,
                        false,
                        no_depth_write,
                        no_depth_test,
                        FogPolicy::Scene,
                        ShadeSel::Lit,
                        0,
                        None,
                        None,
                        None,
                        None,
                        false,
                        light,
                    ));
                }
            }
        }
        // The additive glow-card blend state (specialize's pure ONE/ONE add), both depth-write
        // flavours, and the doodad/entity distance-fade blend twin (depth-write forced on).
        for no_depth_write in [false, true] {
            mats.push(model_material(
                cache,
                materials,
                None,
                ModelBlend::Blend,
                two_sided,
                false,
                false,
                false,
                true,
                false,
                no_depth_write,
                false,
                FogPolicy::Scene,
                ShadeSel::Lit,
                0,
                None,
                None,
                None,
                None,
                false,
                light,
            ));
        }
        mats.push(model_material(
            cache,
            materials,
            None,
            ModelBlend::Blend,
            two_sided,
            false,
            false,
            false,
            false,
            true,
            false,
            false,
            FogPolicy::Scene,
            ShadeSel::Lit,
            0,
            None,
            None,
            None,
            None,
            false,
            light,
        ));
        // The depth-prime twin (colour writes masked off), plain and cutout.
        for cutout in [false, true] {
            mats.push(zfill_material(
                cache, materials, None, two_sided, cutout, light,
            ));
        }
    }
    // The ground-clutter lane (Mask + specialize's over-blend), both sidednesses — the first
    // verification leg caught the two-sided one compiling live. Its material is built by
    // `WorldAssets::model_material` (image machinery this pass doesn't need) — the pipeline only
    // sees the KEY bits, so arm `clutter_fade` on a COPY of the plain Mask material (a fresh
    // asset; the dedup cache's own entry stays untouched).
    for two_sided in [false, true] {
        let mask = model_material(
            cache,
            materials,
            None,
            ModelBlend::AlphaTest,
            two_sided,
            false,
            false,
            false,
            false,
            false,
            false,
            false,
            FogPolicy::Scene,
            ShadeSel::Lit,
            0,
            None,
            None,
            None,
            None,
            false,
            light,
        );
        if let Some(m) = materials.get(&mask) {
            let mut m = m.clone();
            m.extension.clutter_fade = Vec4::new(52.5, 70.0, 0.0, 1.0);
            let clutter = materials.add(m);
            mats.push(clutter);
        }
    }

    let mut count = 0;
    for (mesh, aabb, skinned) in &layouts {
        for mat in &mats {
            let tag = if *skinned {
                MeshTag(crate::mesh_tag::rig_bits(0) | crate::mesh_tag::alpha_bits(1.0))
            } else {
                MeshTag(crate::mesh_tag::alpha_bits(1.0))
            };
            let mut e = commands.spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(mat.clone()),
                Transform::from_xyz(0.0, 0.0, -0.5).with_scale(Vec3::splat(0.01)),
                tag,
                WarmRig,
                ChildOf(cam),
            ));
            if let Some(aabb) = aabb {
                e.insert(*aabb);
            }
            count += 1;
        }
    }
    count
}

/// A unit quad in each attribute combination the model lane ships. Every `RenderSubmesh` field
/// is spelled out on purpose: a new field breaks THIS build, which is the drift alarm that keeps
/// the menagerie honest against the format.
fn warm_quad(colors: bool, skinned: bool) -> RenderSubmesh {
    let n = 4usize;
    RenderSubmesh {
        positions: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        normals: vec![[0.0, 0.0, 1.0]; n],
        uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        indices: vec![0, 1, 2, 0, 2, 3],
        texture: None,
        skin_slot: None,
        geoset_id: 0,
        char_slot: None,
        blend: ModelBlend::Opaque,
        wrap_x: true,
        wrap_y: true,
        two_sided: false,
        joints: if skinned { vec![[0; 4]; n] } else { Vec::new() },
        weights: if skinned {
            vec![[1.0, 0.0, 0.0, 0.0]; n]
        } else {
            Vec::new()
        },
        vertex_colors: if colors {
            vec![[1.0, 1.0, 1.0, 1.0]; n]
        } else {
            Vec::new()
        },
        interior: false,
        emissive: false,
        sidn: None,
        window: false,
        additive: false,
        no_depth_write: false,
        no_depth_test: false,
        fog_policy: FogPolicy::Scene,
        billboard: None,
        alpha_anim: None,
        uv_anim: None,
        rgb_anim: None,
        wmo_batch: None,
    }
}
