//! The mover's side of the `WOW_MOVE_TRACE` debug trace ([`crate::dbg_trace`]), three tags:
//!
//! - **`move`** — one line per *interesting* frame of the player mover ([`frame`]): a step-down snap,
//!   a grounded flip, an airborne frame, or any sizeable vertical delta — so a movement-feel report
//!   ("it pops when I step off the fence") can be read back as per-frame numbers instead of
//!   re-guessed from watching the screen.
//! - **`swim`** — one line per frame *over liquid* ([`swim`]): the waterline, the feet, the depth
//!   against the two latch thresholds, and which physics regime ran. A water-feel report ("it
//!   jitters at the surface") is a question about the depth signal frame by frame, and no other
//!   tag carries it: `move` is emitted by the walk arm only, so the swim frames of a flapping
//!   latch are exactly the ones it drops (decision 0644).
//! - **`snd`** — one line per outbound `MSG_MOVE_*` ([`sent`]), the send-side twin of `net::motion`'s
//!   `rly`: it makes **our own wire cadence measurable** (decision 0617), which is the only way to
//!   compare it against the reference's — the 1.12.1 sniff's client stream is a list of exactly these
//!   fields, so `grep snd` on a run of mouse-turning and strafing is directly diffable against it.
//!
//! The anim driver writes its own `anim` lines into the same file, on the same clock.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::dbg_trace;

/// What the mover did this frame, filled at the end of the physics step in [`super`].
pub(super) struct Frame {
    /// Feet height entering the step (yd).
    pub y_in: f32,
    /// Feet height leaving the step (yd) — after the slide *and* the step-down snap.
    pub y_out: f32,
    pub grounded: bool,
    pub on_walkable: bool,
    pub vel_y: f32,
    /// The step-down snap, when the walk-mode block ran: `(probe reach, what the probe found)`;
    /// the inner pair is `(hit distance, hit normal.y)` — a steep hit is recorded too, so a lip
    /// contact that killed the snap shows up in the trace.
    pub snap: Option<(f32, Option<(f32, f32)>)>,
    /// The atomic step-up's committed height gain this frame (yd), when the maneuver ran
    /// (decision 0209).
    pub climb: Option<f32>,
}

/// One `snd` line per outbound movement packet: the opcode we chose, the live move-flags it carries,
/// and the facing. Read against the sniff's client stream (opcode + flags + orientation per line) this
/// answers "does our wire look like the reference's?" without anyone squinting at a second window.
pub(super) fn sent(kind: crate::net::MoveKind, flags: u32, facing: f32, pos: [f32; 3]) {
    if !dbg_trace::enabled() {
        return;
    }
    dbg_trace::line(
        "snd",
        &format!(
            "{kind:?} flags={flags:#x} o={facing:.4} pos=[{:.2},{:.2},{:.2}]",
            pos[0], pos[1], pos[2]
        ),
    );
}

/// One `swim` line per frame the avatar is over liquid: the waterline, the feet, the submersion
/// depth and the two latch thresholds it is being compared against, plus the regime that ran. The
/// `<` / `>` markers flag the frames where the depth crossed a threshold, so a latch that is
/// flapping reads as a column of them rather than something to be inferred from a Z column.
///
/// `h` is the avatar's own collision height, and the thresholds are printed **derived from it**
/// rather than from a constant (decision 0645) — a trace that quoted a human's 1.52 while a gnome
/// latched at 0.86 would read as a bug in the latch instead of the height, which is precisely the
/// misdirection that let the constant survive this long. It is echoed on the line so a capture says
/// which body it was taken on.
pub(super) fn swim(feet_y: f32, surface_y: f32, swimming: bool, h: f32) {
    if !dbg_trace::enabled() {
        return;
    }
    let (enter, exit) = (
        super::swim::swim_enter_depth(h),
        super::swim::swim_exit_depth(h),
    );
    let depth = surface_y - feet_y;
    let band = if depth > enter {
        '>'
    } else if depth < exit {
        '<'
    } else {
        '='
    };
    dbg_trace::line(
        "swim",
        &format!(
            "y {feet_y:9.3} surf {surface_y:9.3} depth {depth:6.3} {band} [{exit:.3}..{enter:.3}] h {h:.3} mode={}",
            if swimming { "swim" } else { "walk" }
        ),
    );
}

/// How the post-teleport settle hold **ended** — the whole diagnosis of a fall-through report.
///
/// `resident` means the destination's world arrived (scene spawned + collider queue quiet —
/// decision 0737's release) and the hold released onto it; `!resident` is the
/// [`super::SETTLE_TIMEOUT`] backstop firing, which switches gravity on with the world never
/// having become resident. The distinction is invisible from inside the game (both look like "the
/// loading screen went away") and it is the difference between a world that streamed in time and
/// one that did not, so the timeout end is also a `warn!` on the ordinary log — a reporter's paste
/// can then name it without owning a trace.
pub(super) fn settle(resident: bool, waited: f32, pos: bevy::prelude::Vec3) {
    if !resident {
        bevy::log::warn!(
            "settle: TIMED OUT after {waited:.2}s with the world never resident at \
             ({:.1},{:.1},{:.1}) — releasing anyway. If a building stands here, its collider \
             had not finished streaming and the body is about to fall through it.",
            pos.x,
            pos.y,
            pos.z,
        );
    }
    if !dbg_trace::enabled() {
        return;
    }
    dbg_trace::line(
        "sett",
        &format!(
            "{} after {waited:6.2}s at ({:8.2},{:7.2},{:8.2})",
            if resident {
                "world resident"
            } else {
                "TIMED OUT     "
            },
            pos.x,
            pos.y,
            pos.z,
        ),
    );
}

static PREV_GROUNDED: AtomicBool = AtomicBool::new(true);

pub(super) fn frame(f: Frame) {
    if !dbg_trace::enabled() {
        return;
    }
    let dy = f.y_out - f.y_in;
    let snap_dist = f
        .snap
        .and_then(|(_, hit)| hit)
        .map_or(0.0, |(dist, _)| dist);
    let flipped = f.grounded != PREV_GROUNDED.swap(f.grounded, Ordering::Relaxed);
    if !(flipped || !f.grounded || dy.abs() > 0.05 || snap_dist > 0.05 || f.climb.is_some()) {
        return;
    }
    let snap = match f.snap {
        None => "snap -".to_string(),
        Some((reach, None)) => format!("snap miss (reach {reach:.2})"),
        Some((reach, Some((dist, ny)))) => format!(
            "snap d={dist:.3} ny={ny:.3} (reach {reach:.2}){}",
            if ny >= super::GROUND_COS {
                ""
            } else {
                " STEEP"
            }
        ),
    };
    let climb = f.climb.map_or(String::new(), |t| format!(" climb={t:+.3}"));
    dbg_trace::line(
        "move",
        &format!(
            "y {:9.3} -> {:9.3} dy={:+.3} grounded={} walk={} vy={:+7.2} {}{}",
            f.y_in, f.y_out, dy, f.grounded as u8, f.on_walkable as u8, f.vel_y, snap, climb
        ),
    );
}
