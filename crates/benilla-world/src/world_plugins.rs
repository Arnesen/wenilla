//! **[`WorldPlugins`] — the engine, as one name.**
//!
//! Decision 1160 splits the world renderer out of `benilla-app`; 1164 counted what a second
//! program has to name to use it and found **31 of the 214 items were plugin registrations** — one
//! per engine module, every one named by the composition root and by nothing else. That is not an
//! API, it is an assembly manual, and it made the engine's doorway look five times wider than the
//! engine's actual vocabulary. Published as a group, the whole set is one item and the members go
//! back to being internals.
//!
//! **The order below is load-bearing and is the client's own**, so the two binaries stay diffable
//! (it was `worldview::engine`, "the cut line written down", before it was this). Two edges inside
//! it are documented dependencies rather than taste: `WmoSkyPlugin` registers after `SkyPlugin`,
//! whose dome it stands down, and `PerfPlugin` after the debug panel, whose egui context it
//! needs (both instruments, and both the client's to add in that order now). `ArtScopePlugin` before `AssetPlugin` is *not* one — its own comment says so — but it
//! costs nothing to keep.
//!
//! **What is deliberately NOT here.** `pipe_warm` looks engine-adjacent and is not: the viewer
//! disproved it by running, grinding 1051 ms main-thread hitches warming the *game's* pipeline set
//! (nameplates, blob shadows, the UI quad lane, the portrait booths) for a frame that would never
//! be drawn. It is a client instrument and stays with the client.
//!
//! `debug_panel`, `perf` and the particle census are **not** here either, and the reason they once
//! had to be is worth keeping: the panel was the model-`Visibility` authority (the WMO portal PVS
//! is applied through it, decisions 0025/0031), so the world did not draw without it. That
//! authority is `model_render`'s now and the toggles it reads are `dev_state`'s, so the panel is an
//! instrument again — which is what 1160 wanted, instruments at the top of the stack rather than
//! the bottom. `art_scope` stays: within-map art residency is engine behaviour, not a readout, and
//! a viewer that never evicts is a viewer that measures the wrong thing.
//!
//! This is what makes `benilla-worldview` mean something. With the instruments out, the viewer is
//! the engine and nothing else, and every gameplay resource it still needs shows up as a stub.

use avian3d::prelude::*;
use bevy::app::PluginGroupBuilder;
use bevy::prelude::*;

use benilla_assets::materials::{TerrainMaterial, WowModelMaterial};

/// Everything `benilla-world` will own, in one group. One `add_plugins(WorldPlugins)` installs the
/// engine; nothing in it may reach for a server, a player, or a UI.
pub struct WorldPlugins;

impl PluginGroup for WorldPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            // The engine's own WGSL, compiled into the binary (decision 1175). First in the group
            // because every material below specializes against a shader handle, and a shader that
            // was never registered fails silently — as a whole world drawn with nothing on it.
            .add(crate::shaders::plugin)
            .add(MaterialPlugin::<TerrainMaterial>::default())
            .add(MaterialPlugin::<WowModelMaterial>::default())
            // Physics (avian3d): collider storage + collider BVH + shape-casts for the character
            // controller (decision 0009). The streamed terrain/placement entities carry
            // `Collider`s; the player drives `MoveAndSlide` against them. No character controller
            // rides them in the viewer, but the ray-caster and the shape queries do.
            .add_group(PhysicsPlugins::default())
            // ...but NOT the contact pipeline. Nothing in this workspace reads a contact:
            // `CollisionStart`, `CollisionEnd`, `ContactPair` and `Collisions` have zero
            // consumers, because the player is a shape-cast controller and units carry no
            // colliders at all. The broad phase is the only thing that *creates* contact pairs,
            // so disabling it leaves the narrow phase and the solver iterating an empty
            // `ContactGraph` — every resource still exists (`BroadPhaseCorePlugin` owns
            // `ContactGraph`/`JointGraph`), and the collider BVH the shape-casts actually ride
            // is `ColliderTreePlugin`'s, untouched.
            //
            // This is not a micro-optimisation. Pairs were still generated for every
            // kinematic-or-standalone collider overlapping the static world, and each one cost a
            // parry `contact_manifolds_composite_shape_composite_shape` — trimesh vs trimesh,
            // against 65k-triangle terrain tiles — recomputed every physics tick, and Bevy's
            // fixed-timestep catch-up runs up to 16 ticks in one frame (`Time<Virtual>`'s stock
            // 250 ms `max_delta`). Four stall captures across three sessions caught the main
            // thread inside `update_narrow_phase` for 12–41% of their samples; the run that
            // prompted this stalled >600 ms three times in five minutes. Decision 1232.
            .disable::<BvhBroadPhasePlugin>()
            .add(WorldFoundation)
            // The per-frame world-transition ordering (Input → Stream → Present) the loading
            // screen relies on to cover a teleport the same frame it happens. See `schedule.rs`.
            .add(crate::schedule::SchedulePlugin)
            // World-interaction foundation: mouseover picking + object identity (the debug
            // inspector reads it now; hover tooltips / contextual cursor / targeting will later).
            .add(crate::interact::InteractPlugin)
            // M2 billboard cards (glow halos, chains) — faced to the camera each frame.
            .add(crate::billboard::BillboardPlugin)
            // The owned skin palette (decision 0720): every skinned rig's joint matrices, computed
            // by us and skinned in wow_model.wgsl — Bevy's SkinnedMesh lane is fully replaced.
            // The ground-fx decal lane (1164: 326 lines of pure render that had been
            // filed in the app). Its ordering rides the billboard place set, which is why the
            // lane registers itself rather than asking a caller to know that.
            .add(crate::ground_fx::plugin)
            .add(crate::rig_palette::plugin)
            // The rig machinery beside the palette it fills: the pose evaluator, the pose
            // post-pass window, the world/palette composition, and the global-sequence channels.
            // Split out of `creature_anim` (1163's re-check of `finalize_rig_worlds`) — the game
            // still decides which clip plays; posing and composing are the world's.
            .add(crate::rig_anim::plugin)
            // The per-instance body tint (decision 0812), on the same slot index as that palette:
            // the aura state kit's CharProc-1 colour, in its own region of the shared light buffer.
            .add(crate::instance_tint::plugin)
            // The M2 render lane's three own plugins (decision 1163, stage zero). All three used to
            // be registered by `EntitiesPlugin` — the entity streamer — which is why booting the
            // engine without the game left the model-`Visibility` authority reading a
            // `FarSideTwins` that nobody had created. Nothing in them is about streamed entities:
            // it is the render-alpha channel, the water-plane far-side twins, and the depth-prime
            // twins, all of which a terrain doodad needs exactly as much as an NPC does.
            .add(crate::model_fade::plugin)
            .add(crate::model_render::plugin)
            .add(crate::zfill::plugin)
            // Within-map art residency (decision 0793): the dedup caches expire by DISTANCE, so a
            // long flight inside one map stops ratcheting. Before `AssetPlugin` only so the census
            // resource exists for anything that reads it at startup; it needs no ordering.
            .add(crate::art_scope::ArtScopePlugin)
            // Foundation: opens the patch chain + inserts WorldAssets/RenderConfig
            // (`AssetSet::Open`), which every other subsystem's startup runs after.
            .add(crate::assets::AssetPlugin)
            // World-map state (Map.dbc catalog + CurrentMap), loaded right after the chain opens —
            // the terrain/WDL streamers, loading screen, and lighting all key off it.
            .add(crate::world_map::WorldMapPlugin)
            // Time-of-day lighting: sun + WoW shader colors, sky background, per-frame
            // update→apply.
            .add(crate::lighting::LightingPlugin)
            // Sky dome: the Light.dbc gradient backdrop (camera-centred), driven by that lighting.
            .add(crate::sky::SkyPlugin)
            // WMO skybox: the authored sky a building's `0x40000` group swaps in for that gradient
            // (Stratholme's burning city) — registered after SkyPlugin, whose dome it stands down.
            .add(crate::wmo_sky::WmoSkyPlugin)
            // Cloud coverage: the reference's procedural field — glare occlusion (occ1) + the
            // visible layer.
            .add(crate::clouds::CloudsPlugin)
            // Weather: the SMSG_WEATHER state machine driving the storm light-blend +
            // precipitation (decision 0310). Lighting reads its densities `.after(WeatherTick)`.
            .add(crate::weather::WeatherPlugin)
            // Sun disc + glow halo: the celestial sprites WoW draws at the sun (RE'd from
            // CSky::Render).
            .add(crate::sun::SunPlugin)
            // Interior lighting classifier: lights M2 entities (GameObjects/NPCs/other players)
            // standing inside a WMO room off the baked floor colour, day/night-independent (the
            // streamer fills its volume registry).
            .add(crate::interior::InteriorPlugin)
            .add(crate::entity_shade::EntityShadePlugin)
            // WMO portal visibility: per-frame, decides which of a building's groups are reachable
            // through portals from the camera's group, so the Stormwind cathedral culls from the
            // Trade District. Only computes the PVS; the `Visibility` authority
            // (`model_render::visibility`) applies it (decisions 0025/0031).
            .add(crate::wmo_portal::WmoPortalPlugin)
            // The exterior scene draws only through portal windows the flood left behind (decision
            // 0774): from inside a building, terrain and ADT doodads are gated on the deferred
            // window worklist.
            .add(crate::exterior_cull::ExteriorCullPlugin)
            // Doodad animation (decision 0130): placed M2s loop their first sequence + global
            // sequences, gated to drawn instances.
            .add(crate::doodad_anim::DoodadAnimPlugin)
            // Ground clutter: the GroundEffect catalog + the lazy per-chunk build lifecycle, owned
            // independently of the terrain streamer (so the streamer can be swapped). Whichever
            // streamer is active scatters into the ClutterChunks this builds.
            .add(crate::clutter::ClutterPlugin)
            // Distant low-detail terrain (WDL): the fogged horizon hills beyond the streamed tiles.
            .add(crate::wdl::WdlPlugin)
            // Liquid: animated lake/river/ocean water surfaces (MCLQ), spawned with their tile.
            .add(crate::liquid::LiquidPlugin)
            // Particle emitters: the additive flames/glows of campfires, torches, braziers
            // (decision 0014).
            .add(crate::particles::ParticlePlugin)
            // Water foam decals (CWater0Ripple wake/ring/step-in splash) — the record model,
            // rebuilt from the byte RE + two reference-trace reconstructions (decision 0264).
            .add(crate::water_fx::WaterFxPlugin)
            .add(crate::ffx_glow::FfxGlowPlugin)
            .add(crate::ribbons::RibbonPlugin)
            // Stuck-modifier reconciliation: macOS system shortcuts (⇧⌘5) swallow modifier
            // releases without a focus loss, wedging every bare-key binding (decision 0606).
            .add(crate::modkeys::ModKeysPlugin)
            // Terrain streaming: the benilla-assets `AdtTile` pipeline — streams tiles around the
            // viewer through the `AssetServer`, owning the terrain mesh/material/collision,
            // doodads/WMOs, liquid, clutter, and loading-screen residency. Last, as in both
            // binaries before this group existed.
            .add(crate::terrain_stream::TerrainPlugin)
    }
}

/// The engine's bare settings — the two avian tunings, the view distance, the dev state. A plugin only
/// because a [`PluginGroup`] holds plugins and these are `insert_resource` calls; it sits where the
/// client and the viewer both had them, immediately after `PhysicsPlugins`, because
/// [`SubstepCount`] overwrites avian's own default and must therefore land after it.
struct WorldFoundation;

impl Plugin for WorldFoundation {
    fn build(&self, app: &mut App) {
        // One solver substep, not avian's 6: the world has NO dynamic bodies (static terrain,
        // kinematic transports/attachments; the player is a shape-cast controller), so the substep
        // loop's contact/joint solving iterates over nothing — and kinematic motion integrates
        // exactly (constant velocity, no forces) at any substep count. Six substeps were pure
        // fixed-tick schedule overhead (~10 substep-schedule runs per frame on the idle-floor
        // ledger). Revisit if a dynamic body ever enters the world.
        app.insert_resource(SubstepCount(1))
            // WoW gravity (19.29 yd/s², binary-derived — now a feel knob, not a fidelity target)
            // replaces avian's 9.81 default.
            .insert_resource(Gravity(Vec3::NEG_Y * 19.291_105))
            // The faithful view distance (`farclip`) — one source of truth for the wall + the
            // per-object cull (and, post-split, the stream radius). See `view.rs`.
            .init_resource::<crate::view::ViewDistance>()
            // The viewer's body (wire (a)'s kinematics half) — defaulted by the engine so a
            // program with no avatar leaves it empty and every reader takes its no-body branch.
            .init_resource::<crate::view::Viewer>()
            // The dev state (decision 0026): the always-present config layer eight subsystems
            // read, whose defaults ARE the player behaviour. The debug panel is only its editor
            // and may not be installed at all.
            .init_resource::<crate::dev_state::DebugState>();
        // Avian's pre-step copy of Bevy's transform propagation is OFF by default (the 1370
        // bracket surfaced the lane; the 3-round SW split then measured the skip at −0.40
        // cpu_ms, negative in every round): `PhysicsTransformPlugin` re-registers
        // `mark_dirty_trees`/`propagate_parent_transforms`/`sync_simple_transforms` to run
        // before each physics step — a second whole-world transform sweep on top of
        // PostUpdate's own. Our world is static geometry plus kinematic movers
        // (`transport::tick_transports`) that mirror `Position`/`Rotation` directly — built
        // that way precisely because the sync's own timing already lagged — so the pre-step
        // sweep buys nothing. The one Transform-moved collider class left is an opening door
        // (a GameObject's model-local hull riding its pose): it reaches physics one frame late
        // via PostUpdate's own propagation — transient, bounded, imperceptible.
        // `WOW_PHYS_PREPROP=1` restores avian's upstream default (the A/B lever back).
        if std::env::var_os("WOW_PHYS_PREPROP").is_none() {
            app.insert_resource(avian3d::physics_transform::PhysicsTransformConfig {
                propagate_before_physics: false,
                ..Default::default()
            });
        }
        // `WOW_PHYS_HZ=<hz>` — run the fixed loop at a different rate. An EXPERIMENT lever
        // (1370 item 10): avian's per-tick stack is the fixed loop's ONLY occupant (nothing
        // first-party lives in any fixed schedule — zero-hit grep, re-verified at flip time),
        // so 8 vs the default 64 prices the whole-avian bracket in one leg. A measurement
        // lever, never a setting: at 8 Hz the spatial trees ingest streamed colliders up to
        // 125 ms late, which the player's shape-cast controller would feel at a tile seam.
        if let Some(hz) = std::env::var("WOW_PHYS_HZ")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
        {
            app.insert_resource(bevy::time::Time::<bevy::time::Fixed>::from_hz(hz));
        }
        // Direct draws on EVERY camera — the default, not a knob (1374/1376). Bevy's indirect
        // lane is a per-draw indirect encode loop on Metal (no MULTI_DRAW_INDIRECT) plus the GPU
        // preprocessing dispatches, and the 1374 bracket priced it ~4.3 cpu_ms at LBRS on 0.18 —
        // shipped unclaimed for the campaign's whole run because 1364's knob under-measured it
        // (the UI Camera2d leak, 1370). `WOW_INDIRECT=1` restores the upstream default for
        // re-pricing. 1370's note that the lever must cover every camera, not just the world's,
        // is why this is a sweep.
        if std::env::var_os("WOW_INDIRECT").is_none() {
            app.add_systems(bevy::app::Update, force_direct_draws);
        }
    }
}

/// Insert [`NoIndirectDrawing`] on any camera that lacks it — see the note at the registration
/// site. Runs every frame so late-spawned cameras (portrait booths, the glue booth) are covered;
/// the query is empty in steady state.
fn force_direct_draws(
    mut commands: Commands,
    cams: Query<
        Entity,
        (
            With<bevy::camera::Camera>,
            Without<bevy::render::view::NoIndirectDrawing>,
        ),
    >,
) {
    for e in &cams {
        commands
            .entity(e)
            .insert(bevy::render::view::NoIndirectDrawing);
    }
}
