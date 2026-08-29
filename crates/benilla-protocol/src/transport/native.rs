//! [`Conn`] over a real socket — the native body of the transport seam.
//!
//! Every method is the `TcpStream` call it wraps, so nothing about the native wire changed when the
//! seam went in: the same `connect`, the same `TCP_NODELAY`, the same `SO_RCVTIMEO`, the same
//! `try_clone` split into a reader thread and a writer thread. The one new shape is that reads are
//! `async` — and here the future is ready the moment it is polled, because it *is* the blocking
//! `read_exact`, run (as before) on the network thread under
//! [`futures_lite::future::block_on`](https://docs.rs/futures-lite).

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use super::ReadExactAsync;

/// One connection, both directions — what the handshake holds before [`Conn::split`].
pub struct Conn {
    stream: TcpStream,
}

/// The read half after [`Conn::split`] — its own `try_clone`d descriptor onto the same socket.
pub struct ConnReader {
    stream: TcpStream,
}

/// The write half after [`Conn::split`]. `Send`, so it can be handed to the write thread.
pub struct ConnWriter {
    stream: TcpStream,
}

impl Conn {
    /// Dial `host:port`. `host` may be a name or a literal address — `(&str, u16)` resolves either.
    pub async fn connect(host: &str, port: u16) -> io::Result<Conn> {
        Ok(Conn {
            stream: TcpStream::connect((host, port))?,
        })
    }

    /// `TCP_NODELAY` (decision 0617 — Nagle is wrong for this protocol; see `world::send_packet`).
    pub fn set_nodelay(&self, on: bool) -> io::Result<()> {
        self.stream.set_nodelay(on)
    }

    /// Bound how long a read may wait; `None` blocks forever (decision 0065's handshake timeout).
    pub fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(t)
    }

    /// Split into independent halves so a reader can stream while another path writes.
    pub fn split(self) -> io::Result<(ConnReader, ConnWriter)> {
        let read = self.stream.try_clone()?;
        Ok((
            ConnReader { stream: read },
            ConnWriter {
                stream: self.stream,
            },
        ))
    }
}

impl ReadExactAsync for Conn {
    async fn read_exact_async(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.stream.read_exact(buf)
    }
}

impl ReadExactAsync for ConnReader {
    async fn read_exact_async(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.stream.read_exact(buf)
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl Write for ConnWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
