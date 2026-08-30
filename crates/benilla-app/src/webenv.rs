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

/// wasm32: a page-provided env object first, then the page's `?key=value&…` query string —
/// both keyed by `name` lowercased with its `WOW_` prefix dropped.
///
/// The env object is `window.__wenilla_env` (`{user, pass, host, …}`), which a hosting page sets
/// before `init()` after fetching the values over its own authenticated session; it is
/// snapshotted on the first read so the page can `delete` the global right after boot. The
/// query string is the dev fast path (`?user=&pass=`), but for the three credential keys —
/// `user`, `pass`, `char` — it is consulted **only if** the env object carries
/// `dev_query_creds: "1"`: a production host never wants a password in a shareable URL, and a
/// page that sets no env object at all (upstream's plain `index.html`) opts in explicitly. Every
/// other key (`host`, and each CVar via `cvars.rs`) keeps the unconditional query fallback.
///
/// Values are percent-decoded through `js_sys::decode_uri_component` (the browser's own decoder,
/// so whatever encoded the URL round-trips) — a key or value that fails to decode (not valid
/// percent-encoding, or not valid UTF-8 once decoded) is treated as absent rather than panicking
/// on a malformed link.
#[cfg(target_arch = "wasm32")]
pub fn var(name: &str) -> Option<String> {
    let key = name.strip_prefix("WOW_").unwrap_or(name).to_lowercase();
    let env = page_env();
    let search = web_sys::window()
        .and_then(|w| w.location().search().ok())
        .unwrap_or_default();
    lookup(&env, &search, &key)
}

/// The keys the query string may only answer when the page opted in with `dev_query_creds`.
const CREDENTIAL_KEYS: [&str; 3] = ["user", "pass", "char"];
const DEV_QUERY_CREDS: &str = "dev_query_creds";

/// The pure lookup behind [`var`] — env object first, query string second, credentials gated —
/// kept free of `web_sys` so it is unit-tested natively.
#[allow(dead_code)]
fn lookup(env: &std::collections::HashMap<String, String>, search: &str, key: &str) -> Option<String> {
    if let Some(v) = env.get(key) {
        return Some(v.clone());
    }
    if CREDENTIAL_KEYS.contains(&key) && env.get(DEV_QUERY_CREDS).map(String::as_str) != Some("1") {
        return None;
    }
    let query = search.strip_prefix('?').unwrap_or(search);
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (decode(k)? == key).then(|| decode(v)).flatten()
    })
}

/// `window.__wenilla_env` as a string map, read once. Non-string values are ignored; a missing
/// or non-object global is an empty map (so upstream's page — which sets nothing — behaves as
/// "no env, credentials not allowed from the query").
#[cfg(target_arch = "wasm32")]
fn page_env() -> std::collections::HashMap<String, String> {
    thread_local! {
        static ENV: std::cell::OnceCell<std::collections::HashMap<String, String>> = const { std::cell::OnceCell::new() };
    }
    ENV.with(|cell| {
        cell.get_or_init(|| {
            let mut map = std::collections::HashMap::new();
            let Some(window) = web_sys::window() else { return map };
            let Ok(obj) = js_sys::Reflect::get(&window, &"__wenilla_env".into()) else { return map };
            if !obj.is_object() {
                return map;
            }
            for entry in js_sys::Object::entries(&js_sys::Object::from(obj)).iter() {
                let pair = js_sys::Array::from(&entry);
                if let (Some(k), Some(v)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
                    map.insert(k, v);
                }
            }
            map
        })
        .clone()
    })
}

/// `decodeURIComponent`, tolerant of failure — a query string a person hand-edited can contain a
/// bare `%` or an invalid UTF-8 sequence, and this is a login convenience, not a protocol parser.
#[cfg(target_arch = "wasm32")]
fn decode(s: &str) -> Option<String> {
    js_sys::decode_uri_component(s).ok().map(String::from)
}

/// Native stand-in for the browser decoder so [`lookup`] can be tested here: percent-decoding
/// only, which is what the tests exercise.
#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code)]
fn decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::lookup;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn env_object_wins_over_query() {
        let e = env(&[("user", "fromenv")]);
        assert_eq!(lookup(&e, "?user=fromquery", "user").as_deref(), Some("fromenv"));
    }

    #[test]
    fn credentials_from_query_need_the_dev_flag() {
        let e = env(&[]);
        assert_eq!(lookup(&e, "?user=alice&pass=pw&char=Al", "user"), None);
        assert_eq!(lookup(&e, "?user=alice&pass=pw&char=Al", "pass"), None);
        assert_eq!(lookup(&e, "?user=alice&pass=pw&char=Al", "char"), None);
        let e = env(&[("dev_query_creds", "1")]);
        assert_eq!(lookup(&e, "?user=alice&pass=pw", "user").as_deref(), Some("alice"));
        assert_eq!(lookup(&e, "?user=alice&pass=pw", "pass").as_deref(), Some("pw"));
    }

    #[test]
    fn non_credential_keys_always_read_the_query() {
        let e = env(&[]);
        assert_eq!(lookup(&e, "?host=realm.example&renderscale=0.5", "host").as_deref(), Some("realm.example"));
        assert_eq!(lookup(&e, "?renderscale=0.5", "renderscale").as_deref(), Some("0.5"));
        assert_eq!(lookup(&e, "", "host"), None);
    }

    #[test]
    fn query_values_are_percent_decoded_and_malformed_pairs_skipped() {
        let e = env(&[]);
        assert_eq!(lookup(&e, "?host=a%20b&junk&host2=%zz", "host").as_deref(), Some("a b"));
        assert_eq!(lookup(&e, "?host=%zz", "host"), None);
    }
}

/// `WOW_HOST`'s fallback when unset — `net/io.rs`'s `NetConfig::from_env` calls this instead of
/// hard-coding a default, so it stays a one-line swap there (`var("WOW_HOST").unwrap_or_else(…)`)
/// no matter what the right default is per platform.
///
/// Native: `localhost`, decision 0539's original default, unchanged. Web: the page's own
/// hostname — `wenilla-host`'s proxy always runs beside the game server it forwards to, so
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
