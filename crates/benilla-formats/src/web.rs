//! Web-target chain plumbing: `wasm32-unknown-unknown` has no filesystem, so [`crate::Chain`]
//! answers every read/existence/listing question with an HTTP call against the Data URL scheme a
//! companion web host (`wenilla-host`, Lane H) serves — `GET {origin}/data/{encoded name}`,
//! `HEAD` for existence, `GET /data/__index` for the name list. This module is that HTTP shim.
//!
//! [`encode_name`] is plain string math with no browser dependency, so it is exercised natively
//! (`tests/web_names.rs`); [`data_base`], [`fetch_sync`], and [`exists_sync`] need `web-sys` and
//! only make sense — and only compile their bodies — on `wasm32`.

/// Percent-encode `name` exactly like JavaScript's `encodeURIComponent`: the unreserved set is
/// `A-Za-z0-9-_.!~*'()`; every other byte, including `\` (chain names are internally
/// backslash-separated), becomes `%XX` uppercase-hex. This is the client half of the Data URL
/// scheme's "the full name percent-encoded as one component" rule — the web host's decode must be
/// this function's exact inverse, so it is kept pure and unit-tested rather than left to whatever
/// a URL-building crate happens to escape.
pub fn encode_name(name: &str) -> String {
    const UNRESERVED: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'()";
    let mut out = String::with_capacity(name.len());
    for &byte in name.as_bytes() {
        if UNRESERVED.contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use wasm_bindgen::JsValue;
    use web_sys::XmlHttpRequest;

    /// The root every chain fetch is served from: `/data` under this page's own origin. The web
    /// host answers both the client bundle and the `/data/*` routes from one process (Lane H), so
    /// there is never a cross-origin question to configure — this is the whole answer.
    pub fn data_base() -> String {
        let origin = web_sys::window()
            .expect("benilla_formats::web only runs inside a browser tab")
            .location()
            .origin()
            .expect("window.location.origin");
        format!("{origin}/data")
    }

    /// A blocking `XMLHttpRequest` GET, returning the raw response bytes.
    ///
    /// Synchronous on purpose: [`crate::Chain::read`] is called from Bevy systems on the main
    /// thread today — `WorldAssets` and ~60 other call sites read the chain synchronously — and
    /// making the whole call chain async to reach it would ripple through every one of them. A
    /// *synchronous* `XMLHttpRequest` is the one browser primitive that can return bytes from a
    /// call that must return before the function does; the Bevy `AssetReader` path
    /// (`benilla-assets`) is already `async` end to end, so it uses `fetch` instead (no such
    /// constraint there).
    ///
    /// A sync XHR's `responseType` is stuck at the default `""` (text), so an `arraybuffer`
    /// response type — the normal way to get binary out of an XHR — isn't available here. The
    /// standard workaround, used below, is `override_mime_type("text/plain; charset=x-user-defined")`:
    /// it forces the browser to decode the response body as one code unit (0x00-0xFF) per byte
    /// instead of guessing UTF-8 and mangling anything non-ASCII into U+FFFD, so `response_text()`
    /// round-trips arbitrary binary losslessly through `& 0xFF`.
    pub fn fetch_sync(url: &str) -> std::io::Result<Vec<u8>> {
        let xhr = XmlHttpRequest::new().map_err(js_err)?;
        xhr.open_with_async("GET", url, false).map_err(js_err)?;
        xhr.override_mime_type("text/plain; charset=x-user-defined")
            .map_err(js_err)?;
        xhr.send().map_err(js_err)?;
        match xhr.status().map_err(js_err)? {
            200 => {
                let text = xhr.response_text().map_err(js_err)?.unwrap_or_default();
                Ok(text.chars().map(|c| (c as u32 & 0xff) as u8).collect())
            }
            404 => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                url.to_string(),
            )),
            status => Err(std::io::Error::other(format!("{url}: HTTP {status}"))),
        }
    }

    /// A blocking `HEAD` request: does the web host have this name? No body to decode, so no mime
    /// override is needed — just the status line.
    pub fn exists_sync(url: &str) -> bool {
        let Ok(xhr) = XmlHttpRequest::new() else {
            return false;
        };
        if xhr.open_with_async("HEAD", url, false).is_err() {
            return false;
        }
        if xhr.send().is_err() {
            return false;
        }
        xhr.status().map(|status| status == 200).unwrap_or(false)
    }

    fn js_err(e: JsValue) -> std::io::Error {
        std::io::Error::other(format!("{e:?}"))
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::{data_base, exists_sync, fetch_sync};
