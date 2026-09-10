//! `GET`/`HEAD /data/{*name}` and `GET /data/__index` — the Data URL scheme Lane A's wasm
//! `Chain` fetches against verbatim (see the plan's "Shared interfaces"): the browser build has
//! no filesystem, so every asset load the client makes becomes one of these requests, answered
//! straight from the same [`Chain`] the native client reads off disk.
//!
//! The name in the URL is percent-decoded here, not left to axum's own path-segment decoding —
//! `Chain` names use `\` as their separator (`Interface\Glues\...`), and the wildcard capture
//! would otherwise hand us a single decoded segment with no way to tell an encoded `/` (a literal
//! path separator in some other scheme) apart from an encoded `\`. Decoding the raw tail
//! ourselves and mapping `/` -> `\` afterward matches exactly what the client's `encode_name`
//! produces on the way out.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use benilla_formats::Chain;
use tokio::sync::OnceCell;
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};
use tower_http::compression::CompressionLayer;
use tower_http::set_header::SetResponseHeaderLayer;

/// Router state: the opened patch chain. `Chain::read`/`list` are `&self` and lock-free (see the
/// chain module doc). The serialized index is shared across requests for this mounted chain.
#[derive(Clone)]
pub struct DataState {
    pub chain: Arc<Chain>,
    index: Arc<IndexCache>,
}

/// Single-flight initialization; transient list errors are retried on the next request.
#[derive(Default)]
struct IndexCache(Arc<OnceCell<Bytes>>);

impl IndexCache {
    async fn get(
        &self,
        build: impl FnOnce() -> anyhow::Result<Vec<u8>> + Send + 'static,
    ) -> anyhow::Result<Bytes> {
        if let Some(bytes) = self.0.get() {
            return Ok(bytes.clone());
        }
        let cell = Arc::clone(&self.0);
        // The initializer owns the cell independently of this request. Dropping a request
        // cannot cancel spawn_blocking, so keep its result and single-flight guard alive too.
        tokio::spawn(async move {
            cell.get_or_try_init(|| async move {
                tokio::task::spawn_blocking(build).await?.map(Bytes::from)
            })
            .await
            .cloned()
        })
        .await?
    }
}

/// Build the `/data/*` router. Kept separate from `static_site`'s and `ws`'s so `main.rs` can
/// merge them with `Router::merge` and each test file can stand its half up alone.
pub fn router(chain: Arc<Chain>) -> Router {
    Router::new()
        .route("/data/__index", get(index))
        // axum's matchit picks the more specific literal route above over this wildcard on its
        // own — registration order here doesn't matter.
        .route("/data/{*name}", get(file).head(file))
        // On the fly, not precompressed like `static_site`'s: that route serves the handful of
        // files `web-build.sh` writes and can pay brotli once at build time, while this one
        // serves an arbitrary slice of a 5 GB install nobody can enumerate ahead of time.
        // What keeps the cost sane is the predicate, not the level — see [`content_type`].
        .layer(CompressionLayer::new().compress_when(compressible()))
        // COEP `require-corp` on the document (see `static_site`) makes every subresource prove
        // it consents to being embedded. Same-origin responses pass that check without a header,
        // and today `/data` is always same-origin with the page — this is the belt to that
        // braces, so an operator who ever fronts the two from different origins gets a
        // recognisable failure instead of a world that loads with holes in it.
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        .with_state(DataState {
            chain,
            index: Arc::default(),
        })
}

/// Which bodies are worth compressing, as a value rather than inline in [`router`] so the tests
/// can put the real predicate behind a stub handler — otherwise it could only be exercised
/// against a chain, and every assertion about it would skip on a box with no game install.
///
/// `DefaultPredicate` already contributes the size floor and skips `image/*`; naming
/// `NotForContentType::IMAGES` again keeps the BLP half of [`content_type`]'s contract visible
/// where the decision is made, and `audio/mpeg` is the one family it does not cover.
fn compressible() -> impl Predicate {
    DefaultPredicate::new()
        .and(NotForContentType::IMAGES)
        .and(NotForContentType::const_new("audio/mpeg"))
}

/// The `Content-Type` for a chain name — chosen for what it tells the compression predicate,
/// not for the browser, which never looks (the wasm `Chain` reads bytes and parses them by
/// signature).
///
/// The chain hands us *inflated* bytes: `Chain::read` has already undone the MPQ's own zlib, so
/// a DBC or an ADT arrives here as the raw structured form and compresses like one. Two families
/// do not, and they are most of the volume:
///
/// - **BLP** — the texels are DXT blocks (or a palette + indices). Already compressed; brotli
///   spends CPU to add bytes. `image/x-blp` puts them in the family `DefaultPredicate` skips.
/// - **MP3** — likewise, and audio is *not* in that default skip set, so it needs naming.
///
/// Anything else falls through to `application/octet-stream` and gets compressed. That is the
/// right default: the unlisted formats are DBC, ADT, WDT, WDL, M2, WMO and the FrameXML `.lua`
/// / `.xml` / `.toc` text, and every one of them is structured and redundant. A `.wav` lands
/// here too, deliberately — vanilla's are uncompressed PCM.
fn content_type(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or_default();
    if ext.eq_ignore_ascii_case("blp") {
        "image/x-blp"
    } else if ext.eq_ignore_ascii_case("mp3") {
        "audio/mpeg"
    } else {
        "application/octet-stream"
    }
}

/// A chain-read failure that means "this path doesn't exist in the composite" — the two shapes
/// `Chain::read` produces for a missing file (never mounted at all, or tombstoned by a patch) —
/// as opposed to a real I/O fault reading a corrupt archive, which is a 500, not a 404.
fn is_missing(err: &anyhow::Error) -> bool {
    let msg = err.to_string();
    msg.contains("not in patch chain") || msg.contains("deleted from patch chain")
}

async fn file(method: Method, uri: Uri, State(state): State<DataState>) -> Response {
    let raw = uri.path().strip_prefix("/data/").unwrap_or("");
    let decoded = percent_encoding::percent_decode_str(raw).decode_utf8_lossy();
    let name = decoded.replace('/', "\\");
    // Before the move into `spawn_blocking` — the classification is pure string math on the
    // name and yields a `&'static str`, so it costs nothing to take it out of the task's way.
    let content_type = content_type(&name);

    let chain = Arc::clone(&state.chain);
    // `Chain::read` does synchronous file I/O (through benilla-mpq's blocking reads); running it
    // on the async runtime thread would stall every other in-flight request behind one disk seek.
    let read = tokio::task::spawn_blocking(move || chain.read(&name)).await;
    match read {
        Ok(Ok(bytes)) => {
            let body = if method == Method::HEAD {
                Vec::new()
            } else {
                bytes
            };
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, content_type),
                    // `private`: the bytes are the operator's game files — cacheable by the player's
                    // browser (they never change under one name), never by a shared cache.
                    (
                        header::CACHE_CONTROL,
                        "private, max-age=31536000, immutable",
                    ),
                ],
                body,
            )
                .into_response()
        }
        Ok(Err(e)) if is_missing(&e) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "chain read failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(join_err) => {
            tracing::error!(error = %join_err, "chain read task panicked");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `GET /data/__index` — every name [`Chain::list`] can enumerate, for the wasm `Chain::list`
/// (Lane A) to mirror the native directory walk it can't do in a browser.
async fn index(State(state): State<DataState>) -> Response {
    let chain = Arc::clone(&state.chain);
    let listed = state
        .index
        .get(move || {
            let entries = chain.list()?;
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            Ok(serde_json::to_vec(&names)?)
        })
        .await;
    match listed {
        Ok(bytes) => (
            [
                (header::CACHE_CONTROL, "private, max-age=86400"),
                (header::CONTENT_TYPE, "application/json"),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "chain index initialization failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{compressible, content_type};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn index_cache_coalesces_requests_and_is_scoped_to_mount() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let cache = Arc::new(super::IndexCache::default());
        let builds = Arc::new(AtomicUsize::new(0));
        let mut requests = Vec::new();
        for _ in 0..32 {
            let cache = cache.clone();
            let builds = builds.clone();
            requests.push(tokio::spawn(async move {
                cache
                    .get(move || {
                        builds.fetch_add(1, Ordering::SeqCst);
                        Ok(br#"["Interface\\FrameXML\\UI.lua"]"#.to_vec())
                    })
                    .await
                    .unwrap()
            }));
        }
        let mut first: Option<super::Bytes> = None;
        for request in requests {
            let bytes = request.await.unwrap();
            if let Some(ref prior) = first {
                assert_eq!(&bytes, prior);
                assert_eq!(bytes.as_ptr(), prior.as_ptr());
            } else {
                first = Some(bytes);
            }
        }
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        let new_mount = super::IndexCache::default();
        let bytes = new_mount.get(|| Ok(b"[]".to_vec())).await.unwrap();
        assert_eq!(&bytes[..], b"[]");
    }

    #[tokio::test]
    async fn index_cache_retains_initialization_when_first_request_is_canceled() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let cache = Arc::new(super::IndexCache::default());
        let builds = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_cache = cache.clone();
        let first_builds = builds.clone();
        let first = tokio::spawn(async move {
            first_cache
                .get(move || {
                    first_builds.fetch_add(1, Ordering::SeqCst);
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(br#"["first"]"#.to_vec())
                })
                .await
        });
        // Cancel only after the blocking work has definitely started and cannot be canceled.
        started_rx.await.unwrap();
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        let later_builds = builds.clone();
        let later_cache = cache.clone();
        let later = tokio::spawn(async move {
            later_cache
                .get(move || {
                    later_builds.fetch_add(1, Ordering::SeqCst);
                    Ok(br#"["duplicate"]"#.to_vec())
                })
                .await
                .unwrap()
        });
        release_tx.send(()).unwrap();
        let bytes = later.await.unwrap();
        assert_eq!(&bytes[..], br#"["first"]"#);
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        let warm = cache
            .get(|| panic!("warm cache must not rebuild"))
            .await
            .unwrap();
        assert_eq!(bytes.as_ptr(), warm.as_ptr());
    }

    #[tokio::test]
    async fn index_cache_retries_failed_initialization() {
        let cache = super::IndexCache::default();
        assert!(cache
            .get(|| anyhow::bail!("temporary read failure"))
            .await
            .is_err());
        assert_eq!(&cache.get(|| Ok(b"[]".to_vec())).await.unwrap()[..], b"[]");
    }

    /// Run one GET through the real predicate behind a handler that answers with `content_type`
    /// and `len` bytes, and report what `Content-Encoding` came back. `aaaa...` is maximally
    /// compressible on purpose: the question under test is whether the layer *tried*, not how
    /// well brotli did.
    async fn encoding_for(content_type: &'static str, len: usize) -> Option<String> {
        let app = axum::Router::new()
            .route(
                "/x",
                axum::routing::get(move || async move {
                    (
                        [(super::header::CONTENT_TYPE, content_type)],
                        vec![b'a'; len],
                    )
                }),
            )
            .layer(super::CompressionLayer::new().compress_when(compressible()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/x")
                    .header("accept-encoding", "br")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("router response");
        response
            .headers()
            .get(super::header::CONTENT_ENCODING)
            .map(|v| v.to_str().expect("ascii encoding").to_owned())
    }

    /// The pairing that makes the whole scheme work: a DBC/ADT/Lua body compresses, and the two
    /// families [`content_type`] diverts do not. Asserted through the real predicate, so a change
    /// to either half has to keep this true.
    #[tokio::test]
    async fn the_predicate_compresses_only_what_is_worth_compressing() {
        assert_eq!(
            encoding_for("application/octet-stream", 4096)
                .await
                .as_deref(),
            Some("br")
        );
        assert_eq!(encoding_for("image/x-blp", 4096).await, None);
        assert_eq!(encoding_for("audio/mpeg", 4096).await, None);
    }

    /// `DefaultPredicate`'s size floor still applies. Worth pinning: it is why a `HEAD`, whose
    /// body this route deliberately empties, never comes back claiming an encoding.
    #[tokio::test]
    async fn tiny_bodies_are_left_alone() {
        assert_eq!(encoding_for("application/octet-stream", 0).await, None);
    }

    /// The two families that must *not* be compressed, and the fall-through that must be. These
    /// are assertions about the compression predicate as much as about the strings: `image/x-blp`
    /// is only correct because `DefaultPredicate` skips `image/*`, and `audio/mpeg` is only
    /// correct because the layer in [`super::router`] names it.
    #[test]
    fn already_compressed_families_get_a_skipped_content_type() {
        assert_eq!(
            content_type("Interface\\Glues\\Common\\Glue-Panel-Button-Up.blp"),
            "image/x-blp"
        );
        assert_eq!(
            content_type("Sound\\Music\\CityMusic\\Stormwind\\1.mp3"),
            "audio/mpeg"
        );
    }

    /// Chain names arrive in whatever case the archive stored them in — the client's own reads
    /// mix `Interface\...` and `interface\...` in one session (see `web/world-manifest.json`),
    /// so a case-sensitive match would silently compress half the textures in the world.
    #[test]
    fn the_extension_match_is_case_insensitive() {
        assert_eq!(content_type("World\\Textures\\FOO.BLP"), "image/x-blp");
        assert_eq!(content_type("Sound\\ambience\\Forest.Mp3"), "audio/mpeg");
    }

    /// Everything else compresses, including the two edge shapes a `rsplit('.')` classifier can
    /// trip on: a name with no extension at all, and one whose only dot is in a directory.
    #[test]
    fn everything_else_is_compressible() {
        assert_eq!(
            content_type("DBFilesClient\\AreaTable.dbc"),
            "application/octet-stream"
        );
        assert_eq!(
            content_type("Interface\\FrameXML\\ContainerFrame.lua"),
            "application/octet-stream"
        );
        assert_eq!(
            content_type("World\\Maps\\Azeroth\\Azeroth_32_48.adt"),
            "application/octet-stream"
        );
        assert_eq!(content_type("Readme"), "application/octet-stream");
        assert_eq!(content_type("some.dir\\file"), "application/octet-stream");
    }
}
