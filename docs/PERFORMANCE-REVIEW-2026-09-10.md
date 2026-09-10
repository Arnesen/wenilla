# Performance review — 2026-09-10

Reviewed commit: `26d707b6b440025b6fd7bfe282a315340e18de86`. Updated clean local `main` from `c3a948ec` with `git pull --ff-only origin main`. Three review agents covered client rendering/UI, browser loading/audio, and realm/host services; the primary reviewer cross-checked findings and ran validation. No implementation changes were made during the review. The subsequent implementation is documented in [PERFORMANCE.md](PERFORMANCE.md).

The best near-term opportunities are overlapping WASM compilation with prefetch, reducing repeated server work during cold boots, and limiting bridge payload construction. Browser collider construction deserves early profiling because its asynchronous wrapper does not bound main-thread work. Existing retained rendering, visibility gates, lazy rigs, merged colliders, asynchronous audio decoding, manifest prefetch, and compression already address many obvious optimizations.

Priorities express investigation/implementation order, not measured speedup. Code paths below are confirmed; no live game FPS, GPU, memory, or production throughput measurements were collected.

## Findings

### 1. P1 — Collider construction can still monopolize the browser thread

Evidence: [collider.rs:321](C:/Users/alexa/code/wenilla/crates/benilla-world/src/terrain_stream/collider.rs:321), [terrain_stream.rs:998](C:/Users/alexa/code/wenilla/crates/benilla-world/src/terrain_stream.rs:998), [weld.rs:198](C:/Users/alexa/code/wenilla/crates/benilla-world/src/terrain_stream/weld.rs:198).

`build_collider_task` spawns an async body containing only synchronous `Collider::trimesh(verts, tris)`. In this project's single-threaded WASM build, moving that work into an async task does not make the computation preemptible or move it off the browser thread. The 2 ms budget in `finish_colliders` covers attaching completed shapes, not constructing them. Large terrain or welded meshes can therefore cause long tasks despite the attachment budget. Native builds do benefit from the compute pool.

Add separate build-duration instrumentation first. For WASM, pace the queue across actual browser frame yields and bound individual builds through geometry partitioning or a worker/preprocessing design. One yield before a large synchronous build cannot bound its duration. Preserve holes, impassable walls, collision readiness, and loading-screen gates. This is a larger change than the startup and bridge fixes.

Validate cold world entry, city entry, and tile crossings with long-task counts and frame p95/p99; verify collision equivalence. Historical timings in comments are not measurements from this review. The exact locked Bevy task-pool source was not retrieved; the platform conclusion follows the repository's explicit no-threads WASM constraint and the non-yielding construction body.

### 2. P1 — Argon2 runs directly on async request workers

Evidence: [local.rs:84](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/auth/local.rs:84), [local.rs:161](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/auth/local.rs:161), [local.rs:315](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/auth/local.rs:315).

Login verification, the unknown-user timing path, password creation, and password changes perform synchronous Argon2 work inside async functions. Concurrent authentication can occupy Tokio workers needed by asset requests and WebSocket relays, particularly on small VMs. Existing per-IP/user rate limits reduce attempts but do not isolate this work from the runtime.

Move hashing and verification to `spawn_blocking`, guarded by bounded admission so concurrent memory-heavy hashes cannot grow unchecked. Preserve hash parameters and the unknown-user verification behavior.

Validate concurrent logins alongside asset requests and a running relay; compare unrelated-request p95, relay delay, CPU, and peak memory. Include password-change and bootstrap paths.

### 3. P2 — WASM compilation waits for the entire data prefetch

Evidence: [boot.js:48](C:/Users/alexa/code/wenilla/web/boot.js:48), [boot.js:58](C:/Users/alexa/code/wenilla/web/boot.js:58), [boot.js:85](C:/Users/alexa/code/wenilla/web/boot.js:85).

`fetchWasm()` starts before the manifest wait, but its response has no compilation consumer until `init()` is invoked after `await prefetch`. The comment claiming streaming compilation overlaps the pool is incorrect. The counting stream also backpressures rather than continuously consuming the body during the wait.

The browser agent tested the actual module under Node with mocked DOM/fetch and a gated manifest response: while prefetch was pending, initialization had not been called and only two of 100 source chunks had been pulled. After release, initialization consumed all 102,400 bytes. This establishes sequencing/backpressure, not actual browser download or startup savings.

Start streaming compilation immediately and await both compilation and prefetch before instantiating the compiled module through wasm-bindgen. Keep game startup delayed: it performs synchronous catalog reads that depend on the warmed cache. Preserve failure/fallback handling and immediate WASI memory binding.

Validate both dev and realm pages on cold/warm caches with network and CPU throttling. Measure navigation-to-ready, compilation overlap, byte progress, and failed-download behavior.

### 4. P2 — Cold asset requests incur three sequential database reads

Evidence: [lib.rs:138](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/lib.rs:138), [session.rs:129](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/session.rs:129), [session.rs:141](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/session.rs:141), [db.rs:18](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/db.rs:18).

Game assets pass through a setup-completion query, session query, then user/credential query. Hundreds of cold-boot requests multiply this work against a four-connection SQLite pool. Rotation and last-seen updates can add writes; those are already time-throttled.

Join session/user/credential resolution into one query and cache the setup-complete state with explicit updates wherever setup state changes. Keep session revocation, disabled-account checks, and forced password-change enforcement immediate. Increasing pool size alone leaves the redundant work intact.

Validate SQL counts per asset, concurrent cold-boot completion time, pool waits, and all authorization tests.

### 5. P2 — The immutable asset index is rebuilt per cold request

Evidence: [data.rs:155](C:/Users/alexa/code/wenilla/crates/wenilla-host/src/data.rs:155), [chain.rs:207](C:/Users/alexa/code/wenilla/crates/benilla-formats/src/chain.rs:207).

Each uncached `/data/__index` request invokes `Chain::list()`, rereads archive listfiles, deduplicates names, resolves entries and sizes, then serializes names to JSON and dynamically compresses the result. The response discards the calculated sizes. Browser caching avoids repeat work for that browser, but not for other cold clients.

Cache the serialized index once per mounted immutable chain using shared initialization. Consider cached compression variants after measuring encoding cost. Retain the authenticated route and private cache semantics. If hot archive replacement is introduced, make chain identity the invalidation boundary.

Validate concurrent cold requests perform one index computation, contents are identical, memory is bounded, and a new chain obtains a new index. A bounded byte cache with coalesced in-flight reads for individual popular assets is a secondary opportunity; do not cache an entire installation indiscriminately.

### 6. P2 — Bridge limits bound output, not payload construction

Evidence: [snapshot.rs:173](C:/Users/alexa/code/wenilla/crates/benilla-app/src/webbridge/snapshot.rs:173), [snapshot.rs:201](C:/Users/alexa/code/wenilla/crates/benilla-app/src/webbridge/snapshot.rs:201), [mod.rs:116](C:/Users/alexa/code/wenilla/crates/benilla-app/src/webbridge/mod.rs:116).

For every unit within the radius, the bridge creates a full `PlainValue` payload, including strings and unit-state snapshots, then sorts and truncates. Defaults are 20 Hz, radius 60, and 64 units. In crowded scenes, rows discarded by the cap still incur full allocation and conversion costs; a zero-unit cap does too. Publication throttling and the absent-hook fast path already exist.

Collect lightweight distance/entity candidates, select the nearest K, sort selected candidates, and only then construct payloads. Preserve the separately reported target, including targets outside the radius, and tie ordering where observable.

Validate nearest-unit results for K=0/64, target edge cases, and crowded scenes. Measure allocations and bridge duration separately from rendering. Avoid introducing a spatial index before measuring whether candidate scanning itself matters.

### 7. P2 — UI event delivery repeatedly searches the listener list

Evidence: [tick.rs:111](C:/Users/alexa/code/wenilla/crates/benilla-ui/src/script/tick.rs:111), [model.rs:374](C:/Users/alexa/code/wenilla/crates/benilla-ui/src/script/model.rs:374).

For each recipient, `fire_event_into` searches the listener vector from the beginning with `position`. For N stable listeners this requires N(N+1)/2 handle comparisons per event, independent of Lua callback cost. Addon-heavy event traffic is the relevant workload.

Use a fast cursor when the registration generation is unchanged, falling back to lookup after mutations, or use stable listener nodes. A plain index walk or snapshot is not an equivalent replacement: the current dispatcher deliberately saves the next listener before callbacks and supports mutation during delivery. Preserve self-unregistration, saved-next removal, nested events, and applicable appended-registration behavior.

Benchmark no-op listener counts of 10/100/1,000 and realistic addon traffic; run mutation-order tests. This cross-platform upstream-file change should be proposed upstream and merged, or otherwise comply with this fork's carry restrictions.

### 8. P2 — Admin account listing performs sequential N+1 queries

Evidence: [admin.rs:183](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/web/admin.rs:183), [realmdb.rs:12](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/realmdb.rs:12), [realmdb.rs:30](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/realmdb.rs:30).

`user_rows` awaits a separate character query for every linked account. Latency scales with account count; an unavailable database can accumulate repeated five-second acquisition waits. Errors currently become empty character lists.

Fetch character rows for the displayed accounts in one bounded query or bounded batches and group them in memory. Consider pagination as the account list grows. Report a database outage once rather than treating it as each account having no characters.

Validate query count as account count increases, ordering, empty accounts, and outage response time. For a ten-player realm this is less urgent than client startup and authentication isolation.

### 9. P3 — Rig composition allocates two temporary arrays per refreshed rig

Evidence: [compose.rs:103](C:/Users/alexa/code/wenilla/crates/benilla-world/src/rig_anim/compose.rs:103), [compose.rs:205](C:/Users/alexa/code/wenilla/crates/benilla-world/src/rig_anim/compose.rs:205), [compose.rs:267](C:/Users/alexa/code/wenilla/crates/benilla-world/src/rig_anim/compose.rs:267).

`rig_worlds` allocates transform and touched-bone vectors on each call. Active pose-dirty rigs reach it each frame, so R nonempty refreshed rigs incur at least 2R temporary allocations. Existing animation contribution scratch reuse does not cover these composition arrays.

Reuse system-local scratch arrays in the sequential finalization pass. Preserve parent-before-child order, mounted cascades, rebasing, and billboard math. This is likely a modest optimization; measure allocator counts and `WOW_RIG_COST` before prioritizing it over larger work. Validate palette equivalence and existing composition tests. Follow upstream carry policy for any implementation.

## Related correctness and measurement issues

- **Session rotation race:** [session.rs:161](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/session.rs:161) unconditionally replaces a token after an earlier lookup. Concurrent requests using a token due for rotation can mint different replacements. Cookies are written after handlers finish at [session.rs:285](C:/Users/alexa/code/wenilla/crates/wenilla-realm/src/session.rs:285), so reversed completion order can leave the browser with an invalid token and subsequent asset 401s. Make rotation concurrency-safe, with overlapping requests converging on a usable token; compare-and-swap alone needs a defined losing-request/grace strategy. Add a concurrent test with reversed response completion order. This is code-reviewed, not reproduced in this run.
- **Browser FPS journal cannot append samples:** [journal.rs:327](C:/Users/alexa/code/wenilla/crates/benilla-app/src/perf/journal.rs:327) creates its header through `local_state::write_atomic`, whose WASM arm uses localStorage; [journal.rs:414](C:/Users/alexa/code/wenilla/crates/benilla-app/src/perf/journal.rs:414) then appends through `std::fs::OpenOptions`. That bypasses browser storage, and failures are silently ignored. Provide a WASM-specific bounded sample buffer and explicit CSV export; avoid rewriting an ever-growing localStorage value each second. Validate exported rows in an actual browser.
- **Windows CPU attribution is absent:** [clock.rs:20](C:/Users/alexa/code/wenilla/crates/benilla-app/src/perf/clock.rs:20) and [clock.rs:77](C:/Users/alexa/code/wenilla/crates/benilla-app/src/perf/clock.rs:77) return `None` outside Unix. Implement Windows process/thread CPU clocks if Windows is a profiling target. Keep unavailable fields distinct from zero; browser wall time is not interchangeable with process CPU time.

## Further experiments

- Compare current WASM release builds with `profile.ship` (fat LTO, one codegen unit), holding the wasm-opt pass constant. Measure CPU frame cost, startup/compile time, compressed size, and build time. Native improvement comments do not establish a WASM gain. `scripts/web-build.sh` currently uses `--release`; it already runs wasm-opt `-O3` when available and produces compressed siblings.
- Profile decoded SFX residency and pending loads during long same-continent travel. Prewarming starts distinct voice loads without a concurrency/PCM-byte budget; dropping pending state does not cancel fetch/decode. If memory or network contention grows materially, add priority, bounded concurrency, cancellation, and byte-budgeted eviction while preserving currently playing sounds.
- Do not begin with rendering rewrites: static batching, lazy rigs, visibility parking, still-scene gates, and coalesced uploads are already present. Use per-system attribution to justify further changes.

## Validation and implementation sequence

1. Capture a reproducible browser baseline: cold/warm boot, fixed dense-city viewpoint, a tile/zone-crossing route, and prolonged same-map travel. Record resolution, device, adapter, browser, build profile, network conditions, and frame p50/p95/p99 rather than FPS alone.
2. Land separate small changes for compile overlap, bridge selection, index caching, and database-query consolidation. Compare each against the same baseline; preserve both page entry points and the build script's shipped-file list.
3. Isolate hashing and address session-rotation concurrency with focused service tests.
4. Instrument collider construction, then choose a WASM-specific mitigation from measured long tasks. Keep new upstream-file carries cfg-gated or send general improvements upstream.
5. Pursue event dispatch, rig scratch reuse, LTO, and sound-memory changes only with relevant workload measurements.

Validation performed: the original realm test command built successfully but failed its Linux `/proc` assumption on Windows (`sysinfo::tests::reads_something_on_linux`, line 102). Rerunning with that one test excluded passed eight unit tests and six app integration tests. The live MariaDB test reports success after an internal early return when `TEST_MARIADB_URL` is absent, so it does not establish live database coverage here. Browser boot sequencing was checked with a mocked Node harness. Full native/WASM game builds and real game-data/GPU profiling were not run; no speedup percentage is claimed.


The host suite also passed: five unit tests, the static-site integration test, and the WebSocket relay integration test. Its two real-MPQ tests are internally gated on a game installation and returned without exercising an archive in this run.
