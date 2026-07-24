//! The shared debug-trace sink behind `WOW_MOVE_TRACE=<path>`: one file, one clock, written by
//! the player mover's frame lines ([`crate::player`]'s `move_trace`) and the anim driver's event
//! lines ([`crate::creature_anim`]), so the two layers interleave on a common timeline and a feel
//! report ("it snaps when I land") can be read across both. Costs one `OnceLock` read per call
//! when the env var is unset.

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
        let out = File::create(&path)
            .map_err(|e| eprintln!("dbg-trace: cannot create {path}: {e}"))
            .ok()?;
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
