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
//! - [`WarmPass`] + `spawn_menagerie` — the warm pass: one tiny rig per reachable pipeline
//!   variant — the model lane with its shard-rung and far-side twins, and the sky/water lanes
//!   (celestial, stars, clouds, gradient dome, WMO skybox, liquid; decision 0945 widened 0837's
//!   model-only scope) — parented to the world camera, spawned a few frames AFTER the entry
//!   cover rises (so the cover is on the glass before the burst, not racing it — 0962); the
//!   loading screen's clear condition holds on [`WarmPass::satisfied`] until the pipeline cache
//!   drains (10 s backstop, 0737's rule), then the menagerie despawns (roots only — recursion
//!   takes the twin booth's children, 0962). Captures skip it.
//!   Booth twins ride a real booth's layer (samples=1, 0938) AND the pass's own twin booth
//!   ([`crate::portrait::spawn_warm_booth`] — the custom-projection view key real bakes install;
//!   decision 0958), and [`warm_effect_lane`] pushes the `wow_effect` lane's whole key cross
//!   through the production stream each warm frame (the ring's first-target stall, 0958).
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

use bevy::prelude::*;
use bevy::render::render_resource::{CachedPipelineState, PipelineCache, PipelineDescriptor};
use bevy::render::{Render, RenderApp, RenderSystems};

use crate::char_select::ClientState;
use crate::loading_screen::LoadingScreen;
use crate::model_render::MaterialCache;
use crate::particles::buffer::{
    begin_effect_frame, EffectBlend, EffectDrawSpec, EffectFog, EffectQuads, EffectVertex,
};
use crate::terrain::WowModelMaterial;

mod menagerie;
use menagerie::{spawn_menagerie, BoothCamQuery, WarmLanes};

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
    // The effect-lane warm writer rides the production stream, which is cleared at the top of
    // PostUpdate's effect set — so it writes after the clear, like every family writer. The
    // HUD-quad warm rides the UI append lane the same way (cleared at the top of its own set).
    app.add_systems(PostUpdate, warm_effect_lane.after(begin_effect_frame));
    app.add_systems(
        Update,
        warm_ui_quad_lane.in_set(crate::ui_pass::UiQuadAppend),
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

/// Marker on every menagerie entity.
#[derive(Component)]
struct WarmRig;

/// Marker on the menagerie's twin booth camera ([`crate::portrait::spawn_warm_booth`] — the
/// custom-projection view key space, decision 0958), so [`warm_effect_lane`] can address its
/// view. It also carries [`WarmRig`], which despawns it with the rest of the pass.
#[derive(Component)]
struct WarmBoothCam;

/// Main-world warm-pass state. The loading screen folds [`Self::satisfied`] into its clear
/// condition, so the cover holds while menagerie pipelines are still compiling.
#[derive(Resource, Default)]
pub(crate) struct WarmPass {
    /// `Time::elapsed_secs` when the menagerie spawned under the current cover; `None` = idle
    /// (no cover, or the pass already finished for this cover).
    spawned_at: Option<f32>,
    /// This cover's warm work is done (drained, timed out, or not applicable).
    done: bool,
    /// The 1×1 stand-in texture [`warm_effect_lane`]'s draws bind while the pass runs (a strong
    /// handle so the asset lives exactly as long as the pass; `None` = the lane isn't warming).
    effect_tex: Option<Handle<Image>>,
    /// Consecutive covered+in-world frames seen while idle (see [`WARM_COVER_PRESENT_FRAMES`]).
    covered_frames: u32,
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
/// Covered frames the pass waits before spawning the menagerie, so the compile burst lands
/// behind a cover that is ON THE GLASS. On world entry the raise frame still renders the glue
/// (the state flips a frame later), so the FIRST covered+in-world frame is also the first frame
/// the cover can draw — spawning then puts the whole synchronous burst in that same frame's
/// render, and what stays on screen for the burst is the previous present: the frozen character
/// screen (the director's report). Two extra frames ≈ 33 ms of cover; the burst is seconds cold.
const WARM_COVER_PRESENT_FRAMES: u32 = 3;

#[allow(clippy::too_many_arguments)] // a Bevy system: each param is one resource, the app's convention
fn run_warm_pass(
    mut commands: Commands,
    mut warm: ResMut<WarmPass>,
    watch: Res<PipeWatch>,
    loading: Res<LoadingScreen>,
    state: Res<State<ClientState>>,
    time: Res<Time>,
    camera: Query<Entity, With<crate::player::WorldCamera>>,
    rigs: Query<(Entity, Option<&ChildOf>), With<WarmRig>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<WowModelMaterial>>,
    mut lanes: WarmLanes,
    booth: BoothCamQuery,
    mut gizmos: Gizmos,
    mut cache: Local<MaterialCache>,
    shared_light: Option<Res<crate::lighting::SharedLightBuffer>>,
) {
    let covering = loading.covering() && *state.get() == ClientState::InWorld;
    if !covering {
        // No world cover → nothing to hold; a leftover menagerie (timeout, teleport race)
        // despawns. `done` stays true so the gate never blocks an uncovered frame.
        warm.done = true;
        warm.spawned_at = None;
        warm.effect_tex = None;
        warm.covered_frames = 0;
        despawn_rigs(&mut commands, &rigs);
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
        // until they do, the gate holds the cover, which is exactly right) — and once the cover
        // has had [`WARM_COVER_PRESENT_FRAMES`] frames to reach the glass, so the burst is
        // actually hidden (the char-screen freeze, 0962).
        warm.done = false;
        let Ok(cam) = camera.single() else { return };
        let Some(light) = shared_light.as_ref() else {
            return;
        };
        warm.covered_frames += 1;
        if warm.covered_frames < WARM_COVER_PRESENT_FRAMES {
            return;
        }
        warm.spawned_at = Some(now);
        // The twin booth (0958): the custom-projection view key space real bakes use — the real
        // booths warm the placeholder-Perspective class, this camera the NONSTANDARD one. It is
        // a WarmRig, so every despawn path below cleans it up with the rigs.
        let warm_booth = crate::portrait::spawn_warm_booth(&mut commands, &mut lanes.images);
        commands
            .entity(warm_booth.0)
            .insert((WarmRig, WarmBoothCam));
        // The effect lane's stand-in texture — held for the life of the pass.
        warm.effect_tex = Some(lanes.images.add(Image::default()));
        let count = spawn_menagerie(
            &mut commands,
            cam,
            booth.iter().next(),
            &warm_booth,
            &mut meshes,
            &mut materials,
            &mut lanes,
            &mut cache,
            &light.0,
        );
        info!("pipeline warm: menagerie up ({count} variants)");
        return;
    };
    if warm.done {
        return;
    }
    // The gizmo-line lane (0938): gizmos are immediate-mode, so the warm draw happens per frame
    // while the pass runs — one tiny line through the DEFAULT config group, exactly the config
    // the bowstring draws with, compiles the `LineGizmo` pipeline that otherwise waits for the
    // first bow-wielder in view.
    gizmos.line(
        Vec3::new(0.0, 0.0, -0.5),
        Vec3::new(0.001, 0.0, -0.5),
        Color::WHITE,
    );
    let pending = watch
        .0
        .created
        .load(Ordering::Relaxed)
        .saturating_sub(watch.0.settled.load(Ordering::Relaxed));
    if now - spawned >= WARM_SETTLE_SECS && pending == 0 {
        warm.done = true;
        warm.effect_tex = None;
        despawn_rigs(&mut commands, &rigs);
        info!("pipeline warm: drained in {:.2}s", now - spawned);
    } else if now - spawned >= WARM_TIMEOUT_SECS {
        warm.done = true;
        warm.effect_tex = None;
        despawn_rigs(&mut commands, &rigs);
        warn!("pipeline warm: TIMED OUT with {pending} pipelines pending — cover released");
    }
}

/// Tear the pass down by despawning only its ROOT entities. `despawn` is recursive, and the twin
/// booth's rigs are *children* of the twin booth camera — itself a `WarmRig` — so despawning
/// every query row queues the children twice (once explicitly, once via the parent's recursion):
/// a warn per child on the teardown frame (0962). Children of a live camera (the world camera,
/// a real booth) still get their explicit despawn.
fn despawn_rigs(commands: &mut Commands, rigs: &Query<(Entity, Option<&ChildOf>), With<WarmRig>>) {
    for (e, child_of) in rigs {
        if child_of.is_some_and(|c| rigs.contains(c.parent())) {
            continue;
        }
        commands.entity(e).despawn();
    }
}

/// The **effect-lane** warm writer (decision 0958 — the 07:45 log's [831], the selection ring's
/// first-target stall). `wow_effect` is a custom `SpecializedRenderPipeline` lane, not a
/// `MaterialPlugin` one, so no menagerie *entity* can reach it: its pipelines exist only when a
/// draw record sits in the shared stream at queue time. So while the pass runs, this pushes one
/// degenerate draw per reachable [`EffectPipelineKey`] — the full blend × raster-bias cross
/// ({Add, Alpha, Opaque, Multiply, Mod2x} × {0, ground-decal, blob-shadow}; the key's own doc
/// pins the closed bias set) — through the PRODUCTION stream (`EffectQuads` → extract → queue →
/// specialize), once per view class: the world camera (samples=N) and the twin booth (samples=1).
/// The queue path specializes per matching view regardless of coverage, so a 4-vertex sliver at
/// the origin compiles the whole space behind the cover; the stand-in texture keeps the prepare
/// half exercised too. Per-frame like the gizmo line: the stream clears every frame.
///
/// [`EffectPipelineKey`]: crate::particles::render::EffectPipelineKey
/// The HUD-substrate warm (0958's sweep, residual): `UiQuadMaterial` is exactly ONE pipeline,
/// and on a normal entry it compiles covered because the HUD's first quad batch lands under the
/// cover — but nothing structural holds that timing (a slow Interface load would land it after
/// the lift). One invisible overlay quad per warm frame pins the compile inside the cover
/// window; the append lane clears itself every frame, so nothing lingers.
fn warm_ui_quad_lane(warm: Res<WarmPass>, mut quads: ResMut<crate::ui_pass::UiQuads>) {
    if warm.spawned_at.is_none() || warm.done {
        return;
    }
    quads.overlays.push(crate::ui_pass::UiQuad {
        rect: Rect::new(0.0, 0.0, 1.0, 1.0),
        color: [0.0, 0.0, 0.0, 0.0],
        ..default()
    });
}

fn warm_effect_lane(
    warm: Res<WarmPass>,
    mut quads: ResMut<EffectQuads>,
    world_cam: Query<Entity, With<crate::player::WorldCamera>>,
    warm_booth: Query<Entity, With<WarmBoothCam>>,
) {
    if warm.spawned_at.is_none() || warm.done {
        return;
    }
    let Some(tex) = warm.effect_tex.as_ref() else {
        return;
    };
    for cam in world_cam.iter().chain(warm_booth.iter()) {
        for blend in [
            EffectBlend::Add,
            EffectBlend::Alpha,
            EffectBlend::Opaque,
            EffectBlend::Multiply,
            EffectBlend::Mod2x,
        ] {
            for raster_bias in [
                0,
                crate::ground_fx::GROUND_FX_DEPTH_BIAS as i32,
                crate::blob_shadow::SHADOW_RASTER_BIAS,
            ] {
                let start = quads.begin();
                for (u, v) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
                    quads.verts.push(EffectVertex {
                        pos: [u * 0.01, v * 0.01, 0.0],
                        uv: [u, v],
                        color: [1.0, 1.0, 1.0, 1.0],
                    });
                }
                quads.commit_quads(
                    start,
                    EffectDrawSpec {
                        cam,
                        texture: tex.id(),
                        blend,
                        fog: EffectFog::Off,
                        anchor: Vec3::ZERO,
                        bias: 0.0,
                        raster_bias,
                        main_entity: cam,
                        light: None,
                    },
                );
            }
        }
    }
}
