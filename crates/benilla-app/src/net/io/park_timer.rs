//! The character park's link-check tick on either target (wenilla). Upstream's park ticks a
//! `crossbeam_channel::tick`; the sequencer is a future here, so the tick is one too: natively an
//! `async-io` timer (the net thread's `block_on` is woken by its driver), in a page a `setTimeout`.

use std::time::Duration;

/// Resolves after `after`.
#[cfg(not(target_arch = "wasm32"))]
pub(super) async fn sleep(after: Duration) {
    async_io::Timer::after(after).await;
}

/// Resolves after `after`. A sleep dropped before it fires leaves a timer that resolves a promise
/// nobody awaits, which costs nothing.
#[cfg(target_arch = "wasm32")]
pub(super) async fn sleep(after: Duration) {
    let ms = after.as_millis().min(i32::MAX as u128) as i32;
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
