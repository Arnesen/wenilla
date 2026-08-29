//! Config that would be an environment variable on native and can't be one in a browser tab —
//! wasm32 has no process environment (`std::env::var` always answers `NotPresent` there), so the
//! web build reads the page's own query string instead. Every `WOW_*` read this crate ships to
//! wasm goes through [`var`] (`net/io.rs`'s `NetConfig::from_env`, `char_select`'s
//! create-if-empty pick, `login`'s env fast path) so a native `WOW_HOST=… cargo run` and a web
//! `?host=…` URL are the exact same fast path from the caller's point of view — see the plan's
//! shared interface, `benilla_app::webenv::var`.
//!
//! Key mapping is mechanical: `WOW_<NAME>` reads back as the query key `<name>` lowercased with
//! the prefix dropped — `WOW_HOST` → `?host=`, `WOW_USER` → `?user=`, `WOW_PASS` → `?pass=`,
//! `WOW_CHAR` → `?char=`. `WOW_WIN` (the windowed-vs-fullscreen dev switch) is native-only and
//! never routed through here — there is no window chrome to resize in a browser tab.

/// Native: exactly `std::env::var(name).ok()` — every existing caller already treats a missing
/// var this way, so routing through here changes nothing on the platform that already worked.
#[cfg(not(target_arch = "wasm32"))]
pub fn var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// wasm32: the page's `?key=value&…` query string, keyed by `name` lowercased with its `WOW_`
/// prefix dropped. Percent-decoded through `js_sys::decode_uri_component` (the browser's own
/// decoder, so whatever encoded the URL round-trips) — a key or value that fails to decode (not
/// valid percent-encoding, or not valid UTF-8 once decoded) is treated as absent rather than
/// panicking on a malformed link.
#[cfg(target_arch = "wasm32")]
pub fn var(name: &str) -> Option<String> {
    let key = name.strip_prefix("WOW_").unwrap_or(name).to_lowercase();
    let search = web_sys::window()?.location().search().ok()?;
    let query = search.strip_prefix('?').unwrap_or(&search);
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (decode(k)? == key).then(|| decode(v)).flatten()
    })
}

/// `decodeURIComponent`, tolerant of failure — a query string a person hand-edited can contain a
/// bare `%` or an invalid UTF-8 sequence, and this is a login convenience, not a protocol parser.
#[cfg(target_arch = "wasm32")]
fn decode(s: &str) -> Option<String> {
    js_sys::decode_uri_component(s).ok().map(String::from)
}

/// `WOW_HOST`'s fallback when unset — `net/io.rs`'s `NetConfig::from_env` calls this instead of
/// hard-coding a default, so it stays a one-line swap there (`var("WOW_HOST").unwrap_or_else(…)`)
/// no matter what the right default is per platform.
///
/// Native: `localhost`, decision 0539's original default, unchanged. Web: the page's own
/// hostname — `benilla-webhost`'s proxy always runs beside the game server it forwards to, so
/// whatever host served this page is already the right one to open `/ws/{port}` against; a
/// browser tab has no `localhost`-as-loopback concept worth defaulting to instead (the page did
/// not load from the player's own machine).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn default_wow_host() -> String {
    "localhost".into()
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn default_wow_host() -> String {
    web_sys::window()
        .and_then(|w| w.location().hostname().ok())
        .unwrap_or_else(|| "localhost".into())
}
