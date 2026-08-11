//! The mover's side of the `WOW_MOVE_TRACE` debug trace ([`benilla_assets::trace`]), three tags:
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

use benilla_assets::trace;

/// What the mover did this frame, filled at the end of the physics step in [`super`].
pub(super) struct Frame {
    /// Feet height entering the step (yd).
    pub y_in: f32,
    /// Feet height leaving the step (yd) — after the slide *and* the step-down snap.
    pub y_out: f32,
    /// Horizontal distance covered this frame (yd). Paired with the vertical it gives the frame's
    /// **descent slope**, which is the whole question behind "we dive over the edge instead of
    /// stepping down it": the reference bounds a step-down to the foot cone's own
    /// [`super::STEP_SLOPE_RATIO`] per unit of horizontal travel (wow-re `step-off-recourse.md`), so
    /// a walk-off that reads far steeper — or worse, flat-then-plummet across two frames — is the
    /// mover leaving the surface instead of following it down.
    pub dx: f32,
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
    /// The root's anchor ran this frame (decision 0880) — no gravity, no slide, no snap. Without it
    /// on the line, a rooted hang and a genuinely stuck mover are the same column of `dy=+0.000`.
    pub anchored: bool,
}

/// One `snd` line per outbound movement packet: the opcode we chose, the live move-flags it carries,
/// and the facing. Read against the sniff's client stream (opcode + flags + orientation per line) this
/// answers "does our wire look like the reference's?" without anyone squinting at a second window.
pub(super) fn sent(kind: crate::net::MoveKind, flags: u32, facing: f32, pos: [f32; 3]) {
    if !trace::enabled() {
        return;
    }
    trace::line(
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
    if !trace::enabled() {
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
    trace::line(
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
    if !trace::enabled() {
        return;
    }
    trace::line(
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
    if !trace::enabled() {
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
    // The descent slope, printed only while actually going down and moving: `>cone` marks a frame
    // steeper than the foot cone allows, which is the signature of a dive rather than a step-down.
    let slope = if dy < -1.0e-4 && f.dx > 1.0e-4 {
        let s = -dy / f.dx;
        format!(
            " dx={:.3} slope={s:.2}{}",
            f.dx,
            if s > super::STEP_SLOPE_RATIO {
                " >cone"
            } else {
                ""
            }
        )
    } else {
        format!(" dx={:.3}", f.dx)
    };
    // The dive's real signature, and it is not steepness — it is the *absence* of descent. A body
    // that leaves the surface travels forward with `dy≈0` while the probe reports a steep face right
    // beneath it; everything after that is a ballistic arc, and by the time the fall looks steep the
    // frame that caused it is long gone. `>cone` was the first guess and the director's capture
    // showed it never fires: the arc starts at slope 0.05 and only reaches 0.83. This is the marker
    // that would have named it on the first read.
    let left = f
        .snap
        .and_then(|(_, hit)| hit)
        // **A face the body is touching is not a face it left.** The steep hit must be at a
        // *distance* — `d = 0.000` is the body resting flush against a bank, sliding along it, which
        // is the ordinary rub the mover is built to do and which reads identically on every other
        // term here (`dy = 0`, forward travel, a steep normal below). Eighteen of those in one
        // hillside capture buried the one real signature this marker exists to name.
        .is_some_and(|(dist, ny)| ny < super::GROUND_COS && dist > 1.0e-3)
        // **Flat is the signature — not "anything that isn't descending".** A dive frame is one that
        // travels forward and goes *nowhere* vertically while a steep surface sits below it. Written
        // as `dy > -eps` this also swallows every frame that is climbing, where not descending is
        // exactly right: first the atomic step-ups, then — once those were excluded — the ordinary
        // walk up a hill, which rises through the slope ride and carries no `climb` at all. Three
        // false-positive classes on three consecutive captures, the second of which buried nine real
        // takeoffs under fifty-five phantoms. A window around zero admits none of them.
        && dy.abs() < 1.0e-3
        && f.dx > 1.0e-4;
    let left = if left { " LEFT-SURFACE" } else { "" };
    let climb = f.climb.map_or(String::new(), |t| format!(" climb={t:+.3}"));
    let anchored = if f.anchored { " ROOTED" } else { "" };
    trace::line(
        "move",
        &format!(
            "y {:9.3} -> {:9.3} dy={:+.3}{} grounded={} walk={} vy={:+7.2} {}{}{}{}",
            f.y_in,
            f.y_out,
            dy,
            slope,
            f.grounded as u8,
            f.on_walkable as u8,
            f.vel_y,
            snap,
            left,
            climb,
            anchored
        ),
    );
}
