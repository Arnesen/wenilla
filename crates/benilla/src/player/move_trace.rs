//! The mover's side of the `WOW_MOVE_TRACE` debug trace ([`crate::dbg_trace`]): one line per
//! *interesting* frame of the player mover — a step-down snap, a grounded flip, an airborne
//! frame, or any sizeable vertical delta — so a movement-feel report ("it pops when I step off
//! the fence") can be read back as per-frame numbers instead of re-guessed from watching the
//! screen. The anim driver writes its own `anim` lines into the same file, on the same clock.

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
