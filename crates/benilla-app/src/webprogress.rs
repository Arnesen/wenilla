//! The boot-progress bridge to the hosting page — the wasm side of `web/boot.js`.
//!
//! The pre-login boot cannot show its own progress: every stage that matters (the Startup
//! catalog loads, the glue art, the first screen spawn) happens before — or *is* — the first
//! thing this app could draw, so the page's DOM overlay owns that UI (`web/boot.js`, decision
//! recorded in the boot-overlay PR). What the page cannot know is when the client is genuinely
//! *ready*: `init()` resolving only means the module instantiated, and the first frames after it
//! are exactly the heavy ones. So the app calls back.
//!
//! The contract mirrors [`crate::webenv`]'s in reverse: where webenv *reads* a global the page
//! set (`window.__wenilla_env`), this *calls* one the page may define —
//! `window.__wenilla_progress(stage)`. A page without the hook (an old copy, upstream's plain
//! index) costs one failed `Reflect::get` per signal and nothing else; every error is swallowed,
//! because a progress overlay must never be able to take the client down. Stages are plain
//! strings the page switch-cases on:
//!
//! - `"startup"` — the end of the first update's Startup pile (the single long task the
//!   catalogs run in). Sent from a `PostStartup` system; the page repaints only after the task
//!   yields, which is precisely the moment this lands.
//! - `"ready"`  — the first glue screen (login *or* character select: the env fast path skips
//!   Login entirely) has spawned. The page fades its overlay here. Sent once, ever —
//!   [`signal_ready_once`] — because both screens' `materialize_screen` run every frame.
//!
//! Native builds: every function is a no-op; there is no page.

/// Call `window.__wenilla_progress(stage)` if the page defined it; silently do nothing
/// otherwise. See the module header for the stage vocabulary.
#[cfg(target_arch = "wasm32")]
pub(crate) fn signal(stage: &str) {
    use wasm_bindgen::JsCast;
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(hook) = js_sys::Reflect::get(&window, &"__wenilla_progress".into()) else {
        return;
    };
    let Ok(hook) = hook.dyn_into::<js_sys::Function>() else {
        return; // absent (undefined) or not a function — a page that doesn't care
    };
    let _ = hook.call1(&wasm_bindgen::JsValue::NULL, &stage.into());
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn signal(_stage: &str) {}

/// `signal("ready")`, at most once per process — the callers run per-frame and "ready" is an
/// edge, not a state. Relaxed is enough: wasm is single-threaded and native is a no-op anyway.
pub(crate) fn signal_ready_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SENT: AtomicBool = AtomicBool::new(false);
    if !SENT.swap(true, Ordering::Relaxed) {
        signal("ready");
    }
}

/// The `PostStartup` system behind the `"startup"` stage — registered in `lib.rs` beside the
/// app's other one-line wiring.
pub(crate) fn signal_startup() {
    signal("startup");
}
