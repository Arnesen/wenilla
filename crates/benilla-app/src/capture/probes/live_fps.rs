//! The live frame-time sample ([`LiveFpsPlugin`]) and everything printed from the same sample
//! window: the `FPS_PROBE` line itself, the visible-submesh census (`VisCensus` —
//! `VIS_CENSUS`/`VIS_ESCAPED`/`VIS_DUMP`), the asset-churn ratchet (`ChurnCensus` —
//! `MAT_CHURN`) and the residency meter (`ResidencyMeter` — `ASSET_DUMP`). One file because
//! one system, `drive_live_fps`, builds all four from the same frame's queries at the same
//! instant — they are one measurement with four line families, not four instruments.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::capture::PROBE_WARMUP_FRAMES;

/// The LIVE FPS probe (`WOW_LIVE_FPS=<frames>`, delay via `WOW_LIVE_FPS_AT` seconds, default 25;
/// `WOW_LIVE_FPS_MOVE=1` holds W through warmup + sampling, so the probe measures RUNNING through
/// the scene — streaming, spawns, re-classification — not a parked camera; the 0366 hunt's
/// "running around SW" gap):
/// the [`crate::capture::CapturePlugin`] probe's numbers on a NORMAL connected run — streamed units, net
/// apply, quest markers, everything the server-less harness deliberately excludes. Built for the
/// 0362 residual: the serverless stormwind probe pinned 60 while the director's live session read
/// 20, so the gap IS the live world — this instrument measures it. Waits for in-world + the delay
/// (park the character first with [`super::ProbeChatPlugin`]), uncaps vsync, warms
/// [`PROBE_WARMUP_FRAMES`], samples, prints the same machine-greppable `FPS_PROBE` line
/// (scenario=`live`), and exits.
pub(crate) struct LiveFpsPlugin;

impl Plugin for LiveFpsPlugin {
    fn build(&self, app: &mut App) {
        let frames = std::env::var("WOW_LIVE_FPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);
        let at = std::env::var("WOW_LIVE_FPS_AT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(25.0);
        app.insert_resource(LiveFps {
            frames,
            at,
            run: std::env::var("WOW_LIVE_FPS_MOVE").as_deref() == Ok("1"),
            phase: LiveFpsPhase::Waiting,
            samples: Vec::new(),
            cpu_at_start: None,
            sys_at_start: None,
            occluded_now: false,
            occluded_frames: 0,
        })
        .init_resource::<ChurnCensus>()
        .add_systems(Update, drive_live_fps)
        .add_systems(
            First,
            (
                (
                    churn_counter::<bevy::image::Image>("image"),
                    churn_counter::<Mesh>("mesh"),
                    churn_counter::<StandardMaterial>("std"),
                    churn_counter::<crate::terrain::TerrainMaterial>("terrain"),
                    churn_counter::<crate::terrain::WowModelMaterial>("model"),
                    churn_counter::<crate::terrain::WdlMaterial>("wdl"),
                    churn_counter::<crate::terrain::LiquidMaterial>("liquid"),
                ),
                (
                    churn_counter::<crate::sky::SkyMaterial>("sky"),
                    churn_counter::<crate::sun::CelestialMaterial>("celestial"),
                    churn_counter::<crate::sun::StarMaterial>("star"),
                    churn_counter::<crate::clouds::CloudMaterial>("cloud"),
                    churn_counter::<crate::ui_pass::UiQuadMaterial>("uiquad"),
                ),
            ),
        );
    }
}

#[derive(Clone, Copy, PartialEq)]
enum LiveFpsPhase {
    Waiting,
    Warmup(u32),
    Sampling,
    Done,
}

/// [`LiveFpsPlugin`] state.
#[derive(Resource)]
struct LiveFps {
    frames: usize,
    at: f32,
    /// Hold W while measuring (`WOW_LIVE_FPS_MOVE=1`) — the moving-workload probe.
    run: bool,
    phase: LiveFpsPhase,
    samples: Vec<f32>,
    /// Process CPU seconds at the first sampled frame ([`crate::perf::process_cpu_secs`]) — the
    /// baseline for the window's `cpu_ms`/`cpu_pct`.
    cpu_at_start: Option<f64>,
    /// Machine-wide CPU ticks at the first sampled frame ([`crate::perf::system_cpu_ticks`]) — the
    /// baseline for `sys_busy_pct`, which says whether anyone ELSE was competing for the cores
    /// while this leg ran. `cpu_ms` alone cannot: see that function's header for the 25.93-vs-18.8
    /// leg that motivated it (1157).
    sys_at_start: Option<(u64, u64)>,
    /// The window's occlusion state, maintained from `WindowOccluded` transitions. macOS
    /// throttles a FULLY covered window to ~1 fps (any covering window, not just the lock
    /// screen — the director's correction to 0729/0730's lock-screen reading): a probe launched
    /// detached spawns unfocused and can land completely behind other windows, and its leg then
    /// measures the throttle, not the client.
    occluded_now: bool,
    /// Sampled frames taken while occluded — `occluded_frames=` on the probe line, so a
    /// throttled leg names itself instead of being inferred from a ~1 s hitch signature.
    occluded_frames: usize,
}

/// Asset residency — the leak meter (the #bugs teleport leak: caches hold strong handles, so maps
/// visited earlier keep their materials/meshes/images resident, and every uv/tint registry survivor
/// is re-uploaded per frame). A tour probe reading the same counts as a fresh control is what
/// "torn down" means, machine-checked. One struct because `drive_live_fps` is at Bevy's
/// system-param arity limit.
#[derive(bevy::ecs::system::SystemParam)]
struct ResidencyMeter<'w> {
    mats: Res<'w, Assets<crate::terrain::WowModelMaterial>>,
    meshes: Res<'w, Assets<Mesh>>,
    images: Res<'w, Assets<bevy::image::Image>>,
    uv_reg: Res<'w, crate::doodad_anim::UvAnimMaterials>,
    tint_reg: Res<'w, crate::doodad_anim::TintAnimMaterials>,
    models: Res<'w, Assets<benilla_assets::M2Model>>,
    server: Res<'w, AssetServer>,
    churn: ResMut<'w, ChurnCensus>,
}

/// The asset-churn census (the probe's ratchet meter): `AssetEvent::Modified` counts per asset
/// type across the sample window, printed as one `MAT_CHURN` line beside `FPS_PROBE`. A modified
/// material re-creates its uniform buffers + bind group that frame (the Metal non-bindless path),
/// and a modified image/mesh re-uploads — the teleport leak's CPU engine was exactly a per-frame
/// ratchet of this shape, so the floor hunt names them instead of guessing suspects one at a
/// time. Counters register only under `WOW_LIVE_FPS` ([`LiveFpsPlugin`]) — a normal run carries
/// none of this.
#[derive(Resource, Default)]
struct ChurnCensus(std::collections::BTreeMap<&'static str, usize>);

/// One census counter for asset type `A`, folding this frame's `Modified` events in under
/// `label` (a short stable name — the `type_name` of an `ExtendedMaterial` alias is unreadable).
fn churn_counter<A: bevy::asset::Asset>(
    label: &'static str,
) -> impl FnMut(MessageReader<bevy::asset::AssetEvent<A>>, ResMut<ChurnCensus>) {
    move |mut reader, mut census| {
        let n = reader
            .read()
            .filter(|e| matches!(e, bevy::asset::AssetEvent::Modified { .. }))
            .count();
        if n > 0 {
            *census.0.entry(label).or_default() += n;
        }
    }
}

impl ResidencyMeter<'_> {
    /// `WOW_ASSET_DUMP=1`: one `ASSET_DUMP` line per resident image/mesh at sample time, path
    /// via the asset server (runtime-built assets have no path and print as `<unpathed>` counts).
    /// Diffing a tour probe's dump against a fresh control's names exactly which files a
    /// teardown left behind — the leak meter's magnifying glass.
    fn dump(&self) {
        let mut lines: Vec<String> = Vec::new();
        let mut unpathed = [0usize; 3];
        for (kind, ids) in [
            (
                "image",
                self.images.ids().map(|i| i.untyped()).collect::<Vec<_>>(),
            ),
            (
                "mesh",
                self.meshes.ids().map(|i| i.untyped()).collect::<Vec<_>>(),
            ),
            (
                "model",
                self.models.ids().map(|i| i.untyped()).collect::<Vec<_>>(),
            ),
        ] {
            let slot = match kind {
                "image" => 0,
                "mesh" => 1,
                _ => 2,
            };
            for id in ids {
                match self.server.get_path(id) {
                    Some(p) => lines.push(format!("ASSET_DUMP {kind} {p}")),
                    None => unpathed[slot] += 1,
                }
            }
        }
        lines.sort();
        for l in &lines {
            println!("{l}");
        }
        println!(
            "ASSET_DUMP <unpathed> images={} meshes={} models={} materials={}",
            unpathed[0],
            unpathed[1],
            unpathed[2],
            self.mats.len()
        );
    }
}

/// Where — and in what scene state — the sample was taken. Bundled because [`drive_live_fps`] sits
/// at Bevy's 16-param ceiling, and because these four are read together or not at all.
///
/// The `room`/`windows` half is the exterior-scene gate's two terms (decision 0774), the same pair
/// the debug panel's World section shows. Without them a `drawn=` reading taken indoors cannot be
/// read at all: a big number means either "we claimed a room and the cull let everything through" or
/// "we never claimed a room, so nothing was gated" — opposite bugs with identical numbers, and the
/// difference cost a measurement (0780).
#[derive(SystemParam)]
struct SamplePin<'w> {
    /// The map the sample landed on (0705's prove-the-run law): a probe number is evidence only once
    /// the body is known to be at the pin, and `WOW_PROBE_CHAT`'s `.go` can silently fail (a bad map
    /// id, a refused command) leaving the run measuring the login spot.
    map: Option<Res<'w, crate::world_map::CurrentMap>>,
    body: Option<Res<'w, crate::player::Player>>,
    room: Option<Res<'w, crate::wmo_portal::CameraInteriorClaim>>,
    windows: Option<Res<'w, crate::wmo_portal::ExteriorWindows>>,
    /// What the cull DID, not just what it was told — see [`crate::exterior_cull::ExteriorCullVerdict`].
    verdict: Option<Res<'w, crate::exterior_cull::ExteriorCullVerdict>>,
}

/// Wait for in-world + the delay, uncap, warm, sample, print, exit — the live twin of the
/// harness probe's `Phase::ProbeWarmup`/`Probing` arms.
#[allow(clippy::too_many_arguments)]
fn drive_live_fps(
    mut probe: ResMut<LiveFps>,
    time: Res<Time<bevy::time::Real>>,
    self_player: Query<(), With<crate::net::SelfPlayer>>,
    mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
    particles: Query<&crate::particles::ParticleEmitter>,
    // Every spawned model submesh, with the two facts that make a visible one accountable: which
    // subsystem it belongs to, and whether it is gated by the exterior-scene cull. See
    // [`VisCensus`] for why "visible" alone was not enough to diagnose anything.
    parts: Query<CensusData>,
    streamed: Query<(), With<crate::net::NetEntity>>,
    // The animation-LOD gate's effect, machine-readable per probe (decision 0448): how many
    // streamed rigs sat parked at sample end.
    parked: Query<(), With<crate::creature_anim::AnimParked>>,
    entities: Query<()>,
    pin: SamplePin,
    // The owned skin-palette occupancy (decision 0720) — `rigs=live/peak bones=live/peak` on the
    // probe line proves the palette lane is actually populated (an all-zero table renders
    // origin-collapsed rigs, which no other probe number would catch).
    palettes: Option<Res<crate::rig_palette::RigPalettes>>,
    mut residency: ResidencyMeter,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut key_events: MessageWriter<bevy::input::keyboard::KeyboardInput>,
    mut exit: MessageWriter<AppExit>,
    mut occlusions: MessageReader<bevy::window::WindowOccluded>,
) {
    // Drain every frame so the state is current whichever phase we're in — the window can be
    // occluded before sampling ever starts (a detached launch spawns behind whatever is open).
    for o in occlusions.read() {
        probe.occluded_now = o.occluded;
    }
    match probe.phase {
        LiveFpsPhase::Done => {}
        LiveFpsPhase::Waiting => {
            // (The un-occludable window that keeps the SETTLE phase from streaming the world at
            // ~1 fps is [`ProbeFocusPlugin`]'s now — every probe needs it, not just this one.)
            if time.elapsed_secs() < probe.at || self_player.is_empty() {
                return;
            }
            if let Ok(mut w) = windows.single_mut() {
                w.present_mode = crate::capture::probe_uncap_mode();
            }
            info!(
                "live-fps: in-world + settled; vsync off, warming {PROBE_WARMUP_FRAMES} frames{}",
                if probe.run { ", holding W" } else { "" }
            );
            if probe.run {
                // A synthetic held key: `ButtonInput` persists a press until its release, and the
                // winit feed only releases keys it saw go down, so this holds across frames. The
                // raw KeyboardInput message rides along for the binding dispatch's press edge
                // (0997 — MOVEFORWARD latches off the event, holds off the state).
                keys.press(KeyCode::KeyW);
                key_events.write(bevy::input::keyboard::KeyboardInput {
                    key_code: KeyCode::KeyW,
                    logical_key: bevy::input::keyboard::Key::Unidentified(
                        bevy::input::keyboard::NativeKey::Unidentified,
                    ),
                    state: bevy::input::ButtonState::Pressed,
                    text: None,
                    repeat: false,
                    window: Entity::PLACEHOLDER,
                });
            }
            probe.phase = LiveFpsPhase::Warmup(0);
        }
        LiveFpsPhase::Warmup(n) => {
            probe.phase = if n + 1 >= PROBE_WARMUP_FRAMES {
                LiveFpsPhase::Sampling
            } else {
                LiveFpsPhase::Warmup(n + 1)
            };
        }
        LiveFpsPhase::Sampling => {
            if probe.samples.is_empty() {
                probe.cpu_at_start = crate::perf::process_cpu_secs();
                probe.sys_at_start = crate::perf::system_cpu_ticks();
                // The churn census restarts with the window — warmup noise (streaming, shader
                // warms) would otherwise read as steady-state ratchets.
                residency.churn.0.clear();
                probe.occluded_frames = 0;
            }
            if probe.occluded_now {
                probe.occluded_frames += 1;
            }
            let ms = time.delta_secs() * 1000.0;
            probe.samples.push(ms);
            if probe.samples.len() < probe.frames {
                return;
            }
            let mut v = probe.samples.clone();
            v.sort_by(f32::total_cmp);
            let at = |q: f32| v[(((v.len() - 1) as f32) * q).round() as usize];
            let mean = v.iter().sum::<f32>() / v.len() as f32;
            let (emitters, active, live) = particles
                .iter()
                .fold((0usize, 0usize, 0usize), |(e, a, l), p| {
                    (e + 1, a + usize::from(p.live() > 0), l + p.live())
                });
            let mut census = VisCensus {
                own_instance: pin.room.as_ref().and_then(|r| r.0).map(|c| c.room.instance),
                ..Default::default()
            };
            let (submeshes, drawn) = parts.iter().fold((0usize, 0usize), |(n, d), row| {
                census.add(row);
                (n + 1, d + usize::from(row.0.get()))
            });
            let px = windows
                .single()
                .map(|w| (w.physical_width(), w.physical_height()))
                .unwrap_or((0, 0));
            // The present mode actually measured under — an uncap that silently rails (0362) is
            // only diagnosable if the line says what was asked for.
            let present = windows
                .single()
                .map(|w| format!(" present={:?}", w.present_mode))
                .unwrap_or_default();
            // CPU cost per frame across every thread — the load-robust half of the measurement
            // (`perf::process_cpu_secs`), and directly comparable with a reporter's CPU %.
            let cpu = match (probe.cpu_at_start, crate::perf::process_cpu_secs()) {
                (Some(t0), Some(t1)) => {
                    let per_frame_ms = (t1 - t0) * 1000.0 / v.len() as f64;
                    format!(
                        " cpu_ms={per_frame_ms:.2} cpu_pct={:.0}",
                        per_frame_ms / mean as f64 * 100.0
                    )
                }
                _ => String::new(),
            };
            // How busy the WHOLE machine was across the same window — every core, every process.
            // `cpu_ms` is load-ROBUST, not load-immune (1157: the same pin leg read 25.93 with two
            // other slots compiling and 18.80-20.12 quiet), so a leg that does not say this cannot
            // be compared with one taken at a different time. A stamp, never a gate: legs at
            // similar `sys_busy_pct` are comparable, legs far apart are not.
            let sys = match (probe.sys_at_start, crate::perf::system_cpu_ticks()) {
                (Some((b0, t0)), Some((b1, t1))) if t1 > t0 => {
                    format!(
                        " sys_busy_pct={:.0}",
                        (b1 - b0) as f64 / (t1 - t0) as f64 * 100.0
                    )
                }
                _ => String::new(),
            };
            // The pin the number belongs to, in the `.go xyz` order, so a probe line can be
            // matched against the report's coordinates without a second instrument — followed by
            // the exterior-scene gate's state, which is what makes `drawn=` legible indoors.
            let at_pin = match (pin.map.as_ref(), pin.body.as_ref().filter(|b| b.active)) {
                (Some(m), Some(b)) => {
                    let [x, y, z] = benilla_assets::coords::bevy_to_wow(b.pos);
                    format!(" map={} pos={x:.1},{y:.1},{z:.1}", m.0)
                }
                _ => String::new(),
            };
            let gate = {
                let room = match pin.room.as_ref().and_then(|r| r.0) {
                    Some(claim) => format!("g{:02}", claim.room.group),
                    None => "none".to_string(),
                };
                match pin.windows.as_deref() {
                    Some(crate::wmo_portal::ExteriorWindows::Windows(rects)) => {
                        format!(" room={room} windows={}", rects.len())
                    }
                    Some(crate::wmo_portal::ExteriorWindows::Unrestricted) => {
                        format!(" room={room} windows=unrestricted")
                    }
                    None => String::new(),
                }
            };
            let culled = match pin.verdict.as_deref() {
                Some(v) => format!(
                    " cull_windows={} cull_frusta={} cull_tested={} cull_hidden={} cull_unbounded={}",
                    v.windows
                        .map_or("unrestricted".to_string(), |n| n.to_string()),
                    v.frusta,
                    v.tested,
                    v.hidden,
                    v.unbounded
                ),
                None => String::new(),
            };
            let rigs = palettes
                .map(|p| {
                    let (s, b, ps, pb) = p.occupancy();
                    format!(
                        " rigs={s}/{ps} rig_bones={b}/{pb} rig_computed={}",
                        p.computed_rigs()
                    )
                })
                .unwrap_or_default();
            let residency_line = format!(
                " mats={} meshes={} images={} uv={} tint={}",
                residency.mats.len(),
                residency.meshes.len(),
                residency.images.len(),
                residency.uv_reg.0.len(),
                residency.tint_reg.0.len(),
            );
            println!(
                "FPS_PROBE scenario=live frames={} mean_ms={mean:.2} p50_ms={:.2} p95_ms={:.2} p99_ms={:.2} max_ms={:.2} fps={:.1} emitters={emitters} active={active} particles={live} submeshes={submeshes} drawn={drawn} streamed={} parked={} entities={}{rigs}{residency_line} px={}x{}{cpu}{sys}{present} occluded_frames={}{at_pin}{gate}{culled}",
                v.len(),
                at(0.50),
                at(0.95),
                at(0.99),
                v[v.len() - 1],
                1000.0 / mean,
                streamed.iter().len(),
                parked.iter().len(),
                entities.iter().len(),
                px.0,
                px.1,
                probe.occluded_frames,
            );
            census.print();
            // The window's Modified-event totals per asset type — a type at ~1×/frame here is a
            // per-frame re-upload ratchet (see [`ChurnCensus`]); absent means quiet.
            if !residency.churn.0.is_empty() {
                let churn = residency
                    .churn
                    .0
                    .iter()
                    .map(|(k, n)| format!("{k}={n}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("MAT_CHURN frames={} {churn}", v.len());
            }
            if std::env::var_os("WOW_ASSET_DUMP").is_some() {
                residency.dump();
            }
            if probe.run {
                keys.release(KeyCode::KeyW);
            }
            probe.phase = LiveFpsPhase::Done;
            exit.write(AppExit::Success);
        }
    }
}

/// **What is on the screen, and who is accountable for it** — the `VIS_CENSUS` line beside
/// `FPS_PROBE`, plus a per-model breakdown under `WOW_VIS_DUMP=1`.
///
/// `drawn=` alone cannot answer "why can I still see that from in here?". It is one number over
/// every model submesh, and the interesting split is not visible-vs-not: it is which *subsystem* the
/// visible thing belongs to and whether anything is gating it at all. A tree that draws through a
/// wall is a completely different defect depending on whether it carries
/// [`crate::exterior_cull::ExteriorScene`] (tagged but admitted — the cull or the bound is wrong) or
/// does not (nothing is gating it — the wrong lane spawned it). Naming which took a screenshot, an
/// asset dig and a wrong guess; this line answers it in one run.
///
/// `WOW_VIS_DUMP=1` then names the models: one `VIS_DUMP` line per distinct visible label, ungated
/// first, most-drawn first — which is the "so WHICH trees are they?" question.
#[derive(Default)]
struct VisCensus {
    /// Per [`ModelKind`] index: `(visible, visible-and-gated)`.
    kinds: [(usize, usize); 4],
    /// `label -> (visible count, gated)`, for the dump.
    labels: std::collections::HashMap<(String, bool), usize>,
    /// Of every `ExteriorScene`-tagged submesh: how many the cull actually wrote `Hidden` on, and
    /// how many carry **no `Aabb`** — the cull's fail-open arm, which admits them unconditionally.
    /// A tagged-but-drawn object is one of those two, and they are opposite bugs.
    gated_total: usize,
    gated_hidden: usize,
    gated_no_aabb: usize,
    /// Tagged, bounded, NOT exempt, and yet NOT `Hidden` — the escapees, by `(label, is a
    /// billboard card)`. **Exempt** means the piece belongs to the placement the camera is standing
    /// in, which is not exterior scene to itself (decision 0784) and is *supposed* to draw; without
    /// that subtraction this list is all room-you-are-in furniture and says nothing.
    escaped: std::collections::HashMap<(String, bool), usize>,
    /// The placement the camera is inside, for that subtraction.
    own_instance: Option<Entity>,
    /// How many tagged pieces were exempt — the number that explains the `tagged` vs `hidden` gap.
    exempt: usize,
}

/// What [`VisCensus`] reads off every model submesh — the query shape and its fetched row.
type CensusData = (
    &'static ViewVisibility,
    &'static crate::debug_panel::ModelPart,
    Has<crate::exterior_cull::ExteriorScene>,
    Option<&'static crate::interact::WorldObject>,
    &'static Visibility,
    Option<&'static bevy::camera::primitives::Aabb>,
    Has<crate::billboard::BillboardCard>,
    Option<&'static crate::wmo_portal::WmoGroupVis>,
);

type CensusRow<'a> = (
    &'a ViewVisibility,
    &'a crate::debug_panel::ModelPart,
    bool,
    Option<&'a crate::interact::WorldObject>,
    &'a Visibility,
    Option<&'a bevy::camera::primitives::Aabb>,
    bool,
    Option<&'a crate::wmo_portal::WmoGroupVis>,
);

impl VisCensus {
    fn add(&mut self, (vis, part, gated, object, want, aabb, card, group): CensusRow) {
        if gated {
            let exempt = group.is_some_and(|g| Some(g.instance) == self.own_instance);
            self.gated_total += 1;
            self.gated_hidden += usize::from(*want == Visibility::Hidden);
            self.gated_no_aabb += usize::from(aabb.is_none());
            self.exempt += usize::from(exempt);
            if *want != Visibility::Hidden && aabb.is_some() && !exempt {
                let label = object.map_or("<unlabelled>", |o| o.label.as_str());
                *self.escaped.entry((label.to_string(), card)).or_default() += 1;
            }
        }
        if !vis.get() {
            return;
        }
        let slot = &mut self.kinds[kind_index(part.kind)];
        slot.0 += 1;
        slot.1 += usize::from(gated);
        if let Some(o) = object {
            *self.labels.entry((o.label.clone(), gated)).or_default() += 1;
        }
    }

    fn print(&self) {
        let line = ["doodad", "wmo", "creature", "gameobject"]
            .iter()
            .zip(&self.kinds)
            .map(|(name, (vis, gated))| format!("{name}={vis}/gated{gated}"))
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "VIS_CENSUS visible-submeshes {line} | tagged={} hidden={} exempt={} no_aabb={}",
            self.gated_total, self.gated_hidden, self.exempt, self.gated_no_aabb
        );
        // The escapees always print: a tagged, bounded object the cull left un-hidden is a defect
        // by construction, and burying it behind a flag is how it stays unnoticed.
        let mut escapees: Vec<_> = self.escaped.iter().collect();
        escapees.sort_by(|a, b| b.1.cmp(a.1));
        for ((label, card), n) in escapees {
            let c = if *card { " BILLBOARD-CARD" } else { "" };
            println!("VIS_ESCAPED {n}{c} {label}");
        }
        if std::env::var_os("WOW_VIS_DUMP").is_none() {
            return;
        }
        let mut rows: Vec<_> = self.labels.iter().collect();
        // Ungated first (the leak candidates), then most-drawn first.
        rows.sort_by(|a, b| a.0 .1.cmp(&b.0 .1).then(b.1.cmp(a.1)));
        for ((label, gated), n) in rows {
            let g = if *gated { "gated" } else { "UNGATED" };
            println!("VIS_DUMP {g} {n} {label}");
        }
    }
}

/// [`ModelKind`] has a private `index`; the census needs its own (and pins the column order).
fn kind_index(kind: crate::debug_panel::ModelKind) -> usize {
    match kind {
        crate::debug_panel::ModelKind::Doodad => 0,
        crate::debug_panel::ModelKind::Wmo => 1,
        crate::debug_panel::ModelKind::Creature => 2,
        crate::debug_panel::ModelKind::GameObject => 3,
    }
}
