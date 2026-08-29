//! The one connection the protocol talks through — a byte pipe with a native and a browser body.
//!
//! Everything above this module (auth, the world handshake, the reader/writer halves) used to name
//! [`std::net::TcpStream`] directly, which is the single thing a browser cannot give us: a page has
//! no sockets, only WebSockets, and a WebSocket delivers its bytes through the JS event loop rather
//! than out of a blocking `read`. So the socket is behind [`Conn`], with one body per target
//! ([`native`] over `TcpStream`, `web` over `web_sys::WebSocket`), and the shape of the seam is set
//! by what the browser can honour:
//!
//! - **Reads are `async`** ([`ReadExactAsync`]). On the web a read cannot block — the frame it is
//!   waiting for arrives on the very event loop a block would be sitting on. Natively the future
//!   resolves immediately around a blocking `read_exact`, which is correct because the native
//!   sequencer runs on its own thread under `block_on`: same syscall, same thread, same behaviour
//!   as before this seam existed.
//! - **Writes stay synchronous** ([`std::io::Write`]). `WebSocket.send` buffers and returns; there
//!   is nothing to await. That keeps every one of the writer's ~200 verbs a plain sync call.
//!
//! `Conn` is pre-split (both directions on one value) for the handshake, and [`Conn::split`] hands
//! out the two halves the streaming phase needs: natively two `try_clone`d sockets, on the web two
//! handles onto the one shared `WebSocket`.

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{Conn, ConnReader, ConnWriter};

#[cfg(target_arch = "wasm32")]
pub mod web;
#[cfg(target_arch = "wasm32")]
pub use web::{Conn, ConnReader, ConnWriter};

/// Read exactly `buf.len()` bytes, or fail — [`std::io::Read::read_exact`]'s contract, awaited.
///
/// `async fn` in a trait, so the browser body can suspend on the event loop. That makes the
/// returned futures un-nameable and un-`Send`, which the `async_fn_in_trait` lint warns about; both
/// are fine here and neither is negotiable on the web anyway: the only caller is the sequencer
/// future, which is polled on the thread (native) or the task (web) that owns the connection, and a
/// `web_sys::WebSocket` cannot cross threads to begin with.
#[allow(async_fn_in_trait)]
pub trait ReadExactAsync {
    /// Fill `buf` completely. On end of stream: [`std::io::ErrorKind::UnexpectedEof`]; on an
    /// expired read timeout: [`std::io::ErrorKind::TimedOut`] (or `WouldBlock`, which is what a
    /// native socket's own timeout raises).
    async fn read_exact_async(&mut self, buf: &mut [u8]) -> std::io::Result<()>;
}
