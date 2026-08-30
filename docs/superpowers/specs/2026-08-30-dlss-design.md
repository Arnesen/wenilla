# DLSS (Super Resolution / DLAA) for benilla — experimental, Windows-native

## Context

The ask was "integrate the leaked DLSS 5 into the native Windows client". Research (2026-08-30) showed
DLSS 5 is **not an upscaler**: NVIDIA announced it at GTC (Mar 2026) as a neural *rendering* pass
(colour + motion vectors in → re-lit materials out, via Streamline, "Fall 2026"). What leaked from
NBA 2K27 is a separate, undocumented NGX feature DLL (`nvngx_dlssnr.dll` 310.8.0, 158 MB, Blackwell-only
CUDA blobs; ran on a 4090 only through a modder's binary patch; needs injector-style pipeline hooking).
There is no SDK, header, or feature ID for it, and it is an unauthorised proprietary binary.

**Decision (user-confirmed):** build the *real* DLSS stack — Bevy 0.18.1's first-class `dlss` feature
(`dlss_wgpu` 2.x, public NVIDIA DLSS SDK v310.4.0, redistributable, no NDA) — giving DLSS Super
Resolution + DLAA on Windows/Vulkan, testable on the RTX 4090. This is also the exact substrate
(NGX/Streamline + jittered render + motion vectors) DLSS 5 will plug into when NVIDIA ships its SDK;
newer *upscaler* DLLs drop in via DLSS Swapper / NVIDIA App override for free.

**Constraints (user-confirmed):**
- Windows build is produced **natively on the 4090 PC** (cargo-xwin cross-linking `nvsdk_ngx_d.lib` is
  untested; this Linux box has an AMD iGPU → compile-check only).
- Work lives in a **separate experimental worktree/branch**, off `main` until proven on the 4090.

## Facts the plan rests on (verified in vendored Bevy 0.18.1 + benilla source)

- `bevy/dlss` → `bevy_anti_alias/dlss` requires `bevy_render/raw_vulkan_init` = **Vulkan backend only**.
  `bevy/force_disable_dlss` compiles the whole `dlss` module **out** (`bevy_anti_alias/src/lib.rs:15`,
  `default_plugins.rs:35`) — so SDK-typed glue must be `cfg(all(feature="dlss", not(feature="dlss-mock")))`,
  and everything else must be written against core Bevy types (`DepthPrepass`, `MotionVectorPrepass`,
  `TemporalJitter`, `MainPassResolutionOverride`, `MipBias`) so it compiles and can be exercised here.
- Bevy DLSS API: `DlssProjectId(Uuid)` resource **before** `DefaultPlugins` (`DlssInitPlugin` is inside
  them, before `RenderPlugin`); `DlssPlugin` is normally added by `AntiAliasPlugin`, which benilla
  **disables** (`crates/benilla-world/src/boot.rs:125`) → add `DlssPlugin` by hand. Camera component
  `Dlss<DlssSuperResolutionFeature>{perf_quality_mode, reset}` with
  `#[require(TemporalJitter, MipBias, DepthPrepass, MotionVectorPrepass, Hdr)]`; `prepare_dlss` inserts
  `MainPassResolutionOverride` and rewrites jitter each frame; the node consumes
  `ViewTarget::post_process_write()` + prepass depth + motion vectors, so the world backdrop image stays
  full-size as DLSS's output. Edges: `EndMainPass → MotionBlur → DlssSuperResolution → … → Bloom`
  (`MotionBlur` node exists — `PostProcessPlugin` is kept). Msaa must be Off.
- **Bevy bug to work around:** `extract_cameras` re-inserts `MipBias` from the main world every frame
  (`bevy_render/src/camera.rs:573-577`) but `prepare_dlss` only writes it on context creation → compute
  the bias in the main world from a render→main readout (`world_backdrop::mip_bias()` formula).
- Main opaque pipelines keep depth write + `GreaterEqual` even with a prepass → bespoke prepass shaders
  must reproduce the main-pass clip math **bit-for-bit** or the main pass fails its own prepass depth.
- The prepass node copies `ViewDepthTexture → ViewPrepassTextures.depth` at the end of `run_prepass`;
  `StaticGxNode` (wired after `MainOpaquePass`, draws most of the static world) is invisible to DLSS
  unless it gets a prepass twin.
- benilla today: world `Camera3d` renders into an off-screen `Rgba16Float` backdrop composited by the UI
  camera at native res (`crates/benilla-app/src/world_backdrop.rs`); no prepass/motion vectors/jitter
  anywhere; no `WgpuSettings` (Windows → DX12 by wgpu default); `FfxGlowNode` sizes off
  `physical_viewport_size` (`ffx_glow.rs:331`) and is the frame's only gamma→linear decode (decision
  0161); Render Scale is the only AA control (SSAA, texture-size scale, `world_backdrop.rs:153`).

## Design decisions

| # | Decision | Why |
|---|---|---|
| D1 | Three layers: (a) always-compiled "upscaler contract" work in benilla-world/assets (prepass shaders, static_gx twin, glow sub-rect); (b) always-compiled CVar/UI knob; (c) SDK glue `crates/benilla-app/src/dlss.rs` under `cfg(dlss_real)` | Non-dlss builds stay behaviourally identical (no camera carries a prepass → nothing runs); (a)+(b) are testable on Linux via the fake-upscaler knob (T10) |
| D2 | Glow runs **before** DLSS on the render-res sub-rect; explicit edge `FfxGlow → DlssSuperResolution` | Keeps 0161 (glow combine = the single gamma decode; DLSS then sees linear HDR, which is what Bevy's `HighDynamicRange` flag promises the SDK). DLAA is bit-identical to today |
| D3 | `StaticGxNode` honours `MainPassResolutionOverride` (`set_camera_viewport`) and gains `StaticGxPrepassNode` wired `LatePrepass → StaticGxPrepass → EndPrepasses`, writing depth + MV then re-doing Bevy's depth copy | Without it the static world has no depth/MV for DLSS → dominant ghosting source |
| D4 | `wow_model` gets a bespoke prepass shader; terrain/wdl/sky use Bevy's stock prepass (their vertex = `get_world_from_local` + `position_world_to_clip`, identical math); transparents (liquid, clouds, stars, particles) are excluded by Bevy's prepass queue automatically | `wow_model.wgsl:499-526` uses camera-relative `view.clip_from_view * (view_rot * p_cam)`, a z-nudge and a merged-fade collapse that stock prepass can't reproduce |
| D5 | Skinned previous-frame position in two phases: phase 1 = previous rig origin + current palette (root/camera motion correct, limb motion 0); phase 2 (only if limb ghosting shows on the 4090) = previous-palette buffer copy | ~20 lines vs a layout change + 6 MB/frame copy; gate on evidence |
| D6 | CVar `gxDlss` int 0–5 (Off/DLAA/Quality/Balanced/Performance/Ultra), registered in all builds; `WOW_DLSS` env override session-only; deferred (Apply) row; no `Auto` | Matches `gxMultisample` conventions; config round-trips platform-independent |
| D7 | While DLSS is active the backdrop is built at scale 1.0 and `MipBias` comes from the DLSS readout; stored `renderScale` untouched; Render Scale row greyed when `gxDlss ≠ 0` | DLSS owns internal resolution; SSAA on top would double-upscale |
| D8 | DLSS implies `Msaa::Off` at spawn; live toggle with an MSAA camera is refused with a warn | NGX rejects multisampled prepass depth; MSAA is already latched |
| D9 | Vulkan forced only in `dlss` builds on Windows: `Backends::from_env().unwrap_or(VULKAN)` | Non-dlss builds keep DX12; `WGPU_BACKEND=dx12` A/B escape hatch |
| D10 | `Dlss.reset = true` on loading-screen reveal edge, `TeleportMessage`/`WorldportMessage`, `gxDlss` change, fly-cam detach | Camera cuts; Bevy clears the flag after one frame |

## Workspace (user requirement: experimental, separate)

Branch `experimental/dlss` from `main` in a new lane worktree, matching the existing convention
(`git worktree list` shows `benilla-wt/{app,assets,transport,webhost}` on `wt-*` branches):

```bash
cd /home/txcb/games/wowclassic/benilla && git worktree add -b experimental/dlss ../benilla-wt/dlss main
```
All work happens in `/home/txcb/games/wowclassic/benilla-wt/dlss`; nothing merges to `main` until the
4090 checklist passes. Docs/scripts that live in the parent repo (`/home/txcb/games/wowclassic/docs`,
`scripts/win/`) are the only files touched outside the worktree.

## Tasks (in order; each: files → change → verify)

**T1 Cargo features.** `crates/benilla/Cargo.toml`: `dlss = ["benilla-app/dlss"]`, `dlss-mock = ["benilla-app/dlss-mock"]`.
`crates/benilla-app/Cargo.toml`: `dlss = ["bevy/dlss", "benilla-world/dlss", "dep:uuid"]`,
`dlss-mock = ["dlss", "bevy/force_disable_dlss"]`, `uuid = { version = "1", optional = true }`.
`crates/benilla-world/Cargo.toml`: `dlss = ["bevy/dlss"]`. Add a `dlss_real` cfg alias (build.rs or a
small macro) = `all(feature="dlss", not(feature="dlss-mock"))`.
Verify: `cargo check -p benilla --no-default-features --features dlss-mock` builds (first run fetches
`dlss_wgpu`); `cargo tree -e features` for the plain build shows no `dlss`.

**T2 Backend + plugins.** `boot.rs::tuned_default_plugins`: under `cfg(all(feature="dlss", target_os="windows"))`
`.set(RenderPlugin{ render_creation: WgpuSettings{ backends: Some(Backends::from_env().unwrap_or(Backends::VULKAN)), ..}.into(), ..})`.
`crates/benilla-app/src/lib.rs:375`: `#[cfg(dlss_real)] app.insert_resource(DlssProjectId(uuid!("<fixed v4>")))`
before `tuned_default_plugins`, `.add_plugins(bevy::anti_alias::dlss::DlssPlugin)` after. Startup system logs
`dlss: super resolution supported|not supported` from `Option<Res<DlssSuperResolutionSupported>>`.

**T3 wow_model prepass shader.** New `crates/benilla-assets/src/shaders/wow_model_prepass.wgsl`; register via
`embedded_asset!` next to the others (`materials.rs:41-47`); `WowModelExt::prepass_vertex_shader/prepass_fragment_shader`
(`MaterialExtension::specialize` already reaches the prepass pipeline, so `WOW_RIG_SKIN`/`WOW_MERGED_FADE`/vertex
layout carry over). Vertex: verbatim copy of `wow_model.wgsl:456-531` clip math (frame/origin split, `p_cam`,
`view.clip_from_view * (view_rot * p_cam)`, z-nudge `1.1920929e-7`, merged-fade collapse) + under
`MOTION_VECTOR_PREPASS` the previous-frame twin from `get_previous_world_from_local` / `previous_view_uniforms`
(D5 phase 1). Fragment: `bevy_pbr::prepass_io::FragmentOutput`, alpha-mask discard matching the main fragment's
cutout test, `WOW_SKY_DEPTH` → far depth, `motion_vector = (clip − prev_clip) * vec2(0.5, −0.5)`.
Add a source-pin test in `materials.rs` (same style as `the_sky_lane_forces_the_far_depth`) asserting both files
contain the identical clip + nudge lines.

**T4 static_gx.** `crates/benilla-world/src/static_gx/render.rs`: main node adds
`Option<&MainPassResolutionOverride>` to `ViewQuery` and calls `pass.set_camera_viewport(...)` (mirror
`main_opaque_pass_3d_node.rs:95-98`). Four prepass pipeline variants keyed `(cutout, two_sided)`
(`prepass_vertex`/`prepass_fragment`, colour target `Rg16Float`, depth `CORE_3D_DEPTH_FORMAT` write+GreaterEqual,
sample_count 1). View bind group gains `PreviousViewUniforms` binding. `static_gx.wgsl` gets the prepass entry
points (same kill-bit/`p_cam`/nudge + previous twin; fragment = farclip + cutout discard + MV).
`StaticGxPrepassNode` (`ViewDepthTexture, ViewPrepassTextures, ViewUniformOffset, PreviousViewUniformOffset,
Option<MainPassResolutionOverride>, StaticGxView`): no-op when prepass textures are `None`; draws with the override
viewport; then `copy_texture_to_texture(view depth → prepass depth)` like `prepass/node.rs:241-249`. Wire
`(Node3d::LatePrepass, StaticGxPrepassLabel, Node3d::EndPrepasses)`.

**T5 FfxGlow sub-rect.** `ffx_glow.rs`: `prepare_textures` sizes from `override.map_or(vp, |o| o.0)`; uniform grows
to 32 B `(gain, death, haze, dither), (ratio.xy, half_texel.xy)`; downsample samples `clamp(uv*ratio, half_texel,
ratio−half_texel)`; combine `set_viewport(0,0,render)` and samples source at `uv*ratio`. Keep
`(StartMainPassPostProcessing, FfxGlow, Bloom)`; add `(FfxGlowLabel, Node3d::DlssSuperResolution)` under
`cfg(dlss_real)`. Unit test: sub-rect uniform math (ratio = (1,1) with no override → booths bit-identical).

**T6 CVar/knob.** New always-compiled `crates/benilla-app/src/dlss_setting.rs`: `DlssSetting{mode:u8}`,
`DLSS_RANGE 0..=5`, `Default` reads `WOW_DLSS` (ints or `off|dlaa|quality|balanced|performance|ultra`),
`label()`, `cfg(dlss_real) perf_quality() -> DlssPerfQualityMode`, plus a `DlssActive` resource.
`cvars.rs`: `("gxDlss","0")` in `REGISTERED` (~351), knob apply in `apply_to_knobs` (~663), env-override key,
session table entry. Extend the existing tests (`registered_defaults_mirror_the_code_truths`,
`apply_parses_clamps_and_reports_unknowns`, `the_toml_round_trips`) + a parse-table test.

**T7 SDK glue** — new `crates/benilla-app/src/dlss.rs` (`cfg(dlss_real)`): `apply_dlss_setting` (on change /
`Added<WorldCamera>`: insert `Dlss{perf_quality_mode, reset:true}` if supported ∧ mode≠0 ∧ `Msaa::Off`; on off:
`remove::<(Dlss, DepthPrepass, MotionVectorPrepass, TemporalJitter)>()`); `DlssReadout(Arc<Mutex<Option<(UVec2,UVec2)>>>)`
shared into the render app (pattern: `perf/gpu.rs` `GpuMsShared`), filled from `(&MainPassResolutionOverride,
&ExtractedCamera)`; main-world `sync_dlss_mip_bias` after `retarget_world_camera`; `reset_dlss_history` (D10).
`player/setup.rs:93-135` spawns `Msaa::Off` when DLSS requested+supported. `world_backdrop.rs`: `track_render_size`
uses scale 1.0 and `retarget_world_camera` skips its `MipBias` stamp while `DlssActive`.

**T8 Options UI.** `crates/benilla-app/assets/ui/OptionsFrame.xml`: `$parentRowDlss` dropdown (Off…Ultra
Performance = "0".."5", `deferred = 1`, hidden unless `IsDlssSupported()`), last in `OPTIONS_PAGE_ROWS.Graphics`;
Render Scale row disabled while `gxDlss ≠ "0"`; tooltip `BENILLA_TOOLTIP_DLSS` (~line 619).
`crates/benilla-ui/src/script/cvars.rs`: `set_dlss_supported` + Lua `IsDlssSupported()` (mirror
`set_multisample_formats`/`GetMultisampleFormats` at 157/270); pushed from `sync_cvars`. Tests in
`ui_script/options_tests.rs` (pattern at 1376): hidden when unsupported; entries/deferred/greying when supported.

**T9 Debug panel + probe.** `debug_panel/world.rs`: `dlss Quality · 2560x1440 → 3840x2160 · mip −0.58` from
`DlssReadout`; `capture/mod.rs:1144` appends `dlss_px=WxH`.

**T10 Linux fake-upscaler knob (dev only).** `WOW_FAKE_UPSCALE=<0.25..1>` inserts `DepthPrepass + MotionVectorPrepass +
TemporalJitter + MipBias` on the world camera, a render system inserting `MainPassResolutionOverride(vp*f)`, sets
`DlssActive`; optional `WOW_TAA=1` adds `TemporalAntiAliasPlugin` so MV mistakes show as smearing. This is the
gate for T3/T4/T5 before touching the 4090.

**T11 pipe_warm audit.** Confirm the menagerie warm covers the new `Opaque3dPrepass`/`AlphaMask3dPrepass`
variants and static_gx's 4 prepass pipelines; `watch_pipelines` tripwire stays green with `gxDlss ≠ 0`.

**T12 Windows recipe + docs.** New `/home/txcb/games/wowclassic/scripts/win/build-dlss.ps1` (prereqs: rustup
stable-msvc, VS Build Tools C++, Vulkan SDK → `VULKAN_SDK`, LLVM → `LIBCLANG_PATH`,
`git clone --branch v310.4.0 https://github.com/NVIDIA/DLSS` → `DLSS_SDK`; `cargo build --release -p benilla
--no-default-features --features dlss`; copy `$DLSS_SDK/lib/Windows_x86_64/rel/nvngx_dlss.dll` + licence blurb
beside `benilla.exe`). If the DLL isn't found at runtime (dlss_wgpu #10), add `SetDllDirectoryW(exe_dir)` in
`crates/benilla/src/main.rs` under `cfg(all(windows, feature="dlss"))`. New `docs/BENILLA-DLSS.md`: what DLSS 5
actually is and why this is the substrate, build/verify steps, DLSS Swapper note, licence obligations
(in-app redistribution OK, NVIDIA attribution, notify NVIDIA before any commercial release). One-line note in
`scripts/benilla.sh` `cmd_build_win` (71-80) that `--features dlss` is native-only.

## Verification

Automated (Linux, in the worktree):
- `cargo test -p benilla-app` (cvars, options rows, glow uniform math, shader source pins).
- `cargo check -p benilla --no-default-features --features dlss-mock`.
- `cargo build -p benilla --no-default-features`; `cargo tree` diff vs `main` shows no new deps.
- Manual iGPU: `WOW_FAKE_UPSCALE=0.5 WOW_TAA=1 scripts/benilla.sh run …` — no holes/z-fight on WMOs/models,
  glow confined to the sub-rect, UI crisp, picking unchanged.

Manual on the RTX 4090 (4K, `gxVSync 0`):
1. Boot log: Vulkan adapter + `dlss: super resolution supported`.
2. FPS native vs DLAA vs Quality vs Performance; Ctrl+Shift+D readout shows 3840×2160 / 2560×1440 / 1920×1080.
3. Ghosting: strafe past a WMO wall (static_gx MV), fast camera spin, particle-heavy cast (mild trail expected),
   swimming (water has no MV).
4. Glow: Stormwind night native vs Quality — similar radius, no hard edge at the sub-rect boundary.
5. Teleport/hearth/loading screen: no ghost of the old zone.
6. Options toggle off→on→off: Render Scale greys/un-greys; no stuck jitter after off.
7. Release build only for validation-noise checks (NGX validation-layer spam is a known DLSS bug).

## Risks

- Prepass/main-pass shader divergence (holes/sparkle) — verbatim math + source-pin tests + T10 gate.
- Skinned limb ghosting (D5 phase 1) → phase 2 if visible.
- No MV for transparents (water, particles, clouds, fade twins) — inherent; watch for smearing.
- Vulkan for the whole client on Windows in dlss builds — present-mode/vsync/window-mode regressions possible;
  `WGPU_BACKEND=dx12` A/B.
- DLL discovery (dlss_wgpu #10) — test a shortcut launch, not just a terminal launch.
- Live toggle compiles prepass variants async → a few frames of pop-in; acceptable, pipe_warm re-arm is a follow-up.
- Nothing here yields DLSS 5 neural rendering; that waits on NVIDIA's SDK (Fall 2026) and is a separate task.
