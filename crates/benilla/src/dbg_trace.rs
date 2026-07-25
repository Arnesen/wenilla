//! The shared debug-trace sink behind `WOW_MOVE_TRACE=<path>`: one file, one clock, written by
//! the player mover's frame lines ([`crate::player`]'s `move_trace`), its outbound wire lines
//! (`snd`), the anim driver's event lines ([`crate::creature_anim`]), and the remote-replay lines
//! (`rly`/`run`, [`crate::net::motion`]) — so the layers interleave on a common timeline and a feel
//! report ("it snaps when I land") can be read across all of them. Each file opens with a
//! `# t0=<unix epoch>` header so **two clients' traces align with each other**, which is what a
//! sender-vs-observer question needs. Costs one `OnceLock` read per call when the env var is unset.

use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Sink {
    out: File,
    t0: Instant,
}

static SINK: OnceLock<Option<Mutex<Sink>>> = OnceLock::new();

fn sink() -> Option<&'static Mutex<Sink>> {
    SINK.get_or_init(|| {
        let path = std::env::var("WOW_MOVE_TRACE").ok()?;
        let mut out = File::create(&path)
            .map_err(|e| eprintln!("dbg-trace: cannot create {path}: {e}"))
            .ok()?;
        // The wall-clock epoch of this file's `t=0`, so **two traces can be read against each other**
        // — a sender's `snd` lines beside an observer's `rly`/`run` lines from a different process
        // (decision 0619). `t=` alone is per-process seconds and says nothing across clients; with
        // this header, wall time is `t0 + t`.
        let t0_wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let _ = writeln!(out, "# t0={t0_wall:.3} (unix epoch seconds at t=0)");
        Some(Mutex::new(Sink {
            out,
            t0: Instant::now(),
        }))
    })
    .as_ref()
}

/// Whether the trace is enabled — lets callers skip building their line when it's off.
pub(crate) fn enabled() -> bool {
    sink().is_some()
}

/// Append one tagged line, stamped with the sink's shared clock.
pub(crate) fn line(tag: &str, msg: &str) {
    let Some(sink) = sink() else { return };
    let Ok(mut s) = sink.lock() else { return };
    let t = s.t0.elapsed().as_secs_f32();
    let _ = writeln!(s.out, "t={t:9.3} {tag:4} {msg}");
}
