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
| Build experiments | `WEB_PROFILE=ship` opt-in; release remains default; wasm-opt errors stop builds | Owned build script |

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
- `WEB_PROFILE=ship scripts/web-build.sh` enables the existing fat-LTO profile for comparison. It is not the default because native performance results do not establish a browser benefit. Measure frame cost, download size, compilation/startup time, and build time before choosing it for releases.

## Validation

The WebGPU WASM and native client compile checks pass. The realm suite, host suite, Node startup/journal tests, UI event-order/mutation tests, and rig differential/allocation tests pass. Browser construction-budget policy tests pass natively. Additional standalone tests exercise the bridge selection, PCM-cache accounting/Arc ownership, and Windows CPU clocks. Real-MPQ and live-MariaDB tests are environment-gated; their early returns are not live coverage.

For actual gains, compare the same build/device/resolution on cold and warm boot, a dense city viewpoint, zone/tile crossings, and long same-continent travel. Record frame p50/p95/p99 and long tasks, bridge duration, memory, and navigation-to-ready. For the realm, compare simultaneous logins/cold boots and unrelated-request/relay latency. No game data or live realm was used for performance measurements here.
