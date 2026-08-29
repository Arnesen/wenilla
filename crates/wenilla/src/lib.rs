//! The browser entry point — benilla's third launcher shim, after `benilla` and
//! `benilla-worldview` (decision 1160), and like them ~20 lines whose only job is to carry the
//! build-id stamp so a commit dirties this crate and not `benilla-app` (decision 0993).
//!
//! `web/index.html` loads the `wasm-bindgen --target web` glue this crate compiles to
//! (`scripts/web-build.sh`), which calls `init()` then this module's exported [`start`] —
//! `#[wasm_bindgen(start)]` also runs it automatically the moment the module finishes
//! instantiating, so the two are equivalent; the page calls it explicitly only to control
//! sequencing with `wasi_stubs.js`'s `bind(memory)` (see that file's own header for why the wasm
//! Lua VM needs it).

use wasm_bindgen::prelude::*;

use benilla_app::BuildId;

/// Set the panic hook (so a Rust panic reaches `console.error` with a real message instead of
/// the browser's bare "unreachable executed"), then hand off to [`benilla_app::run`] exactly like
/// the native `benilla` shim does — same `BuildId`, same entry point, so behaviour that differs
/// between the two targets differs inside `benilla_app`, never at this seam.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    // `AppExit` is discarded deliberately: on wasm32, Bevy's winit runner hands the event loop to
    // the browser (`requestAnimationFrame`) and returns here immediately rather than blocking
    // until the app actually closes, so the value this call produces answers a question — "what
    // did the run exit with?" — that hasn't been decided yet. The native shims' `fn main() ->
    // AppExit` (where the return value IS the process exit code) has no wasm32 equivalent: a
    // browser tab has no exit code for a page to report.
    benilla_app::run(BuildId {
        sha: env!("BENILLA_GIT_SHA"),
        short: env!("BENILLA_GIT_SHORT"),
        date: env!("BENILLA_GIT_DATE"),
        profile: env!("BENILLA_PROFILE"),
    });
}
