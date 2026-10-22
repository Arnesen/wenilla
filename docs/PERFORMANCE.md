# Performance carries

These changes address the September 2026 review while preserving the upstream merge path. They live on `codex/performance-improvements`; they have not been deployed. No live game FPS improvement is claimed without a controlled gameplay benchmark.

## What changed

| Area | Implementation | Boundary |
|---|---|---|
| Startup | Compile WASM while manifest data warms; instantiate only after both complete | Owned `web/boot.js`, shared by both pages |
| Asset index | Serialize once per mounted chain, share bytes, coalesce initialization, retain results after caller cancellation | Owned host |
| Realm authentication | Two blocking password workers; one joined session lookup; cached setup gate | Owned realm |
| Session rotation | Conditional token replacement, one cookie winner, 60-second old-token grace, logout revokes either accepted token | Owned realm; migration `0003_session_rotation.sql` |
| Admin users | Batch character queries in groups of 500 accounts | Owned realm |
| Bridge | Select nearest candidates before allocating unit payloads; stable ties and independent target | Owned `webbridge/` |
| Event delivery | Check the expected listener position, search only if mutation moved it | Small non-macOS hook; helper in `wenilla_listener.rs` |
| Rig composition | Retain world-transform and touched-bone buffers between rigs/frames | Small non-macOS hook; `compose_scratch.rs` |
| Collider construction | Spend up to 2 ms on queued builds before waiting for another Stream frame | Small WASM hook; `collider/web_budget.rs` |
| Browser audio | At most 16 pending SFX loads, speculative prewarming limited to four occupied slots; cancellation; 64 MiB PCM cache with LRU eviction | Existing WASM-owned audio files and gated cache type |
| Browser diagnostics | Latest 3,600 FPS-journal samples in tab memory, CSV export | WASM sink; native filesystem journal unchanged |
| Windows diagnostics | Process and calling-thread CPU clocks through Win32 | Windows-only helper |
| Build experiments | `WEB_PROFILE=ship` opt-in (the default since October 2026, below); wasm-opt errors stop builds | Owned build script |

## Upstream updates

Keep upstream as a Git merge through a PR, as described in `UPSTREAM.md`. No upstream histories or dependencies were replaced by these changes.

Most changes belong to wenilla's own files. The shared client changes use cfg-gated hooks: macOS retains the original event and rig paths, native collider construction retains its compute-pool path, and browser audio/diagnostics remain WASM-specific. General client optimizations can eventually be proposed upstream to retire carries.

`compose_scratch.rs` deliberately mirrors the original composition math. Its differential tests compare results to upstream's `rig_worlds`, including changed camera inputs, special bones, and different rig sizes. If an upstream merge changes that math, update the helper until the comparison passes. CI now runs these tests, listener mutation tests, browser startup/journal tests, and host/service tests in addition to the WASM compile check.

## Limits and behavior to verify in gameplay

- The collider budget bounds a burst, not the duration of a single `Collider::trimesh` call. An individual build over 8 ms emits its triangle count and elapsed time. Movement/backface handling and decals explicitly downcast to triangle meshes, so replacing them with compound chunks is not equivalent. Eliminating every large single-build hitch still requires a worker/preprocessing design or a carefully integrated partitioning change and live collision validation. The original geometry and loading/readiness gates are preserved.
- Audio overload favors bounded work. Above 16 pending SFX loads, additional uncached one-shots are skipped and can load on their next event. Prewarming skips speculative work once four slots are occupied. Deferred shots older than 200 ms are discarded, as before; at most 64 shots wait per load. Cache eviction releases cache ownership only, so currently playing sounds retain their shared PCM. The 64 MiB cap does not include active playback, in-flight browser buffers, or music.
- Setup-state caching has a five-second TTL to observe an external CLI reset. Setup completion updates it immediately. Session/account authorization is still queried per request; disabling an account or revoking sessions is not cached.
- Session rotation accepts the previous token for 60 seconds to let concurrent requests finish. Only the rotation winner emits a replacement cookie. The migration is applied by the existing startup migration path; no production database was touched during development.
- The browser journal is session-only. Reloading the tab clears it. `/console fpsJournal 1` or `?fps_journal=1` starts recording and shows a download button. `window.__wenilla_fps_journal.download()` also exports CSV. Disabling/re-enabling recording retains existing rows within that tab.
- `WEB_PROFILE=ship` was opt-in here; the October 2026 round below measured it in the browser and made it the default.

## Validation

The WebGPU WASM and native client compile checks pass. The realm suite, host suite, Node startup/journal tests, UI event-order/mutation tests, and rig differential/allocation tests pass. Browser construction-budget policy tests pass natively. Additional standalone tests exercise the bridge selection, PCM-cache accounting/Arc ownership, and Windows CPU clocks. Real-MPQ and live-MariaDB tests are environment-gated; their early returns are not live coverage.

For actual gains, compare the same build/device/resolution on cold and warm boot, a dense city viewpoint, zone/tile crossings, and long same-continent travel. Record frame p50/p95/p99 and long tasks, bridge duration, memory, and navigation-to-ready. For the realm, compare simultaneous logins/cold boots and unrelated-request/relay latency. No game data or live realm was used for performance measurements here.

## October 2026: browser frame cost and audio stutter

A second round, aimed at the browser build only, under the same rule: upstream merges come first, so every change is in the fork's own files or a small `cfg(target_arch = "wasm32")` hook.

| Change | Where | Upstream lines |
|---|---|---|
| No tracing spans in the browser | `benilla-world/src/boot/web_spans.rs` (new), hook in `boot.rs` | 5 (cfg'd `mod` + `custom_layer` arm) |
| Own Web Audio scheduler with an adaptive lead | `benilla-app/src/sound/web_backend.rs`, wasm `web-sys` features | 0 (both fork-only) |
| `RenderDiagnosticsPlugin` only when the FPS journal is armed | `benilla-app/src/perf/journal.rs` | ~10, cfg'd, beside the file's existing wasm arms |
| wasm SIMD (`+simd128`), fat LTO (`ship`) by default | `scripts/web-build.sh`, `check.yml` | 0 |
| binaryen on Windows (Git Bash/MSYS) | `scripts/web-setup.sh` | 0 |

### What was measured

Login screen, headless Chrome 152 on an RTX 4090 (WebGPU), `requestAnimationFrame` callback time (the whole Bevy update — main and render schedules run inline on wasm), ~2,600 frames per run, variants alternated, three rounds each. The login screen is light on world work; these are clean A/Bs of per-frame overhead, not in-world numbers.

- **Spans:** `tracing-wasm` (Bevy's wasm log layer) calls `performance.mark` on every span enter and `performance.measure` on every exit. Bevy and the client open INFO spans every frame, so the page wrote ~15,000 User Timing entries a second (500,000 in 33 s) and the browser keeps them for the life of the tab. Vetoing span callsites: mean frame CPU **2.21 → 1.81 ms (−18 %)**; p99 3.7–4.3 → 3.2–3.5 ms. Log events still reach the console and the crash ring.
- **Render diagnostics:** gating the plugin: **−5 to −6 %** in six paired rounds. `?fps_journal=1` / `?fpsjournal=1` keeps it (and the GPU columns); arming the journal later with `/console fpsJournal 1` gets no GPU columns.
- **Fat LTO:** ~**−3 %** CPU against the closest comparable `release` build, and the module shrinks **80 → 70 MB** (less to download and compile). Cold build ~18 min vs ~7.5.
- **SIMD:** neutral on the login screen (within noise). On glam 0.30's kernels under V8: transform composition 1.24×, frustum-vs-AABB 1.27×, rig slerp + `Mat4` 0.97×. Kept for in-world transform/visibility work; **its in-world effect is unmeasured**.
- **talc allocator:** −2 %, within noise, and it adds a second `allocator-api2` to the lockfile (rewriting hashbrown's lines). Not adopted.

### Audio

cpal 0.17's `webaudio` host — which `web_backend.rs` used — mixes on the main thread from `ended` events and keeps two 2048-frame buffers in flight: measured 28–45 ms of lead on an idle page. Any main-thread stall longer than that underruns, and cpal never resyncs its cursor, so the late buffers start "in the past" and overlap the next ones (garble until it catches up; after the first resume on the login screen it played 51 buffers on top of each other). The mix itself is cheap (~0.35 ms per 2048 frames on the login screen).

`web_backend.rs` now schedules 1024-frame chunks itself: refilled from each chunk's `ended` (an audio event, so it keeps running in a background tab), an adaptive lead (80 ms floor; ×1.5 per underrun up to 350 ms; back down one chunk per 8 quiet seconds), a resync on underrun (one gap, never overlap), and pooled `AudioBuffer`s. Forced main-thread stalls on the login screen:

| Stall | cpal (before) | Scheduler (after) |
|---|---|---|
| 60 ms every 300 ms | 11 of 215 buffers late | 1 underrun, then none (lead 120 ms) |
| 120 ms every 500 ms | 44 of 216 late | 1 underrun, then none |
| 250 ms every 1 s | — | 2 underruns, then none (lead 350 ms) |

The cost is latency while the page is janky: a sound starts up to the current lead after it was triggered (80 ms when steady). `?audiolead=<ms>` sets the floor; `window.__wenilla_audio` shows `{ underruns, lead_ms }`.

### Multithreading: what was ruled out

- **Bevy 0.18 cannot run its schedules or render world in parallel on wasm32**, whatever the build flags: `bevy_tasks` compiles its single-threaded pool on wasm32, `bevy_ecs` defaults every schedule to `SingleThreaded` there, and pipelined rendering is compiled out. A `+atomics` build also needs nightly `-Zbuild-std`. It would buy shared memory for our own code only, with COOP/COEP on the realm page and a second toolchain. Not worth it until Bevy has a web executor (bevy#4078).
- **What a worker could still take** (message passing, no shared memory): terrain/WMO trimesh collider builds (the seam is already ours in `collider/web_budget.rs`; parry's `TriMesh` serializes under `serde-serialize`). That removes stream-in spikes, not steady-state cost, and needs a second small wasm module. The audio mix could move to an AudioWorklet only by running all of kira there, which is a rewrite.
- `fetch_sync` (`benilla-formats/src/web.rs`) is mostly per-request XHR overhead, not decoding: 40 real files cost 37 ms of XHR and ~2 ms of decode. A byte cache would help only if reads repeat; the login screen does one sync read.

### Still to measure in-world

The world needs a realm login, which this round did not use. Before trusting the numbers above for gameplay: the FPS journal at a fixed city viewpoint and on a zone-crossing route (SIMD and LTO most of all), `window.__wenilla_audio` underruns and lead over a session of travel, and long-task counts on tile crossings.
