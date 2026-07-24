//! The rendered **body heading** + the animation's view of the flags — the client's display-facing
//! pose (wow-re `body-facing-pipeline.md` §3, the `0x607ed0` tail; supersedes 0051's
//! ease-toward-velocity), beside its concern like `mover`/`swim`/`arc`. [`super::control`] calls
//! [`drive_body_heading`] once per controlled frame, after this frame's move flags are final.

use crate::creature_anim::{ease_strafe_yaw, move_flags, strafe_body_offset, wrap_pi};

use super::{Player, STATIONARY_CHASE_RATE, TURN_RATE};

/// Advance `model_yaw` (the rendered body heading) one frame and return the **animation's** view of
/// the move flags (the wire keeps the real key flags — observers integrate the facing from them):
///
///  · **strafing** — the root turns to `face_yaw ± 90°` (pure) / `± 45°` (diagonal, mirrored
///    when backpedaling), eased a quarter of the remaining gap per frame **in aim-relative
///    offset space** (a left↔right flip is an exact 180° tie in absolute yaw — offset space
///    always swings it around the front); the SpineLow/Head counter-twist walks the upper
///    body back so the head keeps looking at the aim.
///  · **moving fwd/back or airborne** — snap to the aim (the client's `flags & 0x2003` snap;
///    a backpedal keeps facing forward and plays WalkBackwards).
///  · **standing** — the FROZEN chase (byte rule, wow-re `b947e5aa`): while the aim is being
///    steered (A/D turn keys or mouse-look) the catch-up is off and only the 90° ceiling
///    applies — the camera/aim and the head-twist lead while the body holds, then the body
///    follows the turn lagging exactly 90°; once the steering stops it sweeps back onto the
///    aim at 8× the turn rate (~63 ms for 90°). The lag mechanism is the freeze, not a slow
///    rate — the client stamps its chase clock every non-steering frame.
///  · **swimming** ignores the ground display-facing pose (no strafe body-offset, no standing
///    chase): the body SNAPs to the aim (the client's facing snap list — dead or swimming) and
///    the swim *pitch* tilts it in `control`'s transform write (TU-A's render law).
///
/// The turn-in-place foot-shuffle rides the BODY's actual rotation (the client's chase-step bits
/// `0x800/0x1000`, cleared each frame after the anim layer reads them) — NOT the turn keys: while
/// the frozen chase holds the body under a leading head, the feet hold too, and a stationary
/// MOUSE turn shuffles once the body steps (no key flag involved). A deck turning under the rider
/// carries `model_yaw` rigidly at the ride block, so it never registers as a body step here —
/// the shuffle (and its keyframed step sounds) only ever sees real turns (decision 0458/0466).
pub(super) fn drive_body_heading(
    player: &mut Player,
    move_flags_now: u32,
    dt: f32,
    swimming: bool,
    moving: bool,
    airborne: bool,
    steering: bool,
) -> u32 {
    let strafe_offset = if swimming {
        0.0
    } else {
        strafe_body_offset(move_flags_now)
    };
    let mut body_turn_step = 0.0_f32;
    if swimming {
        player.model_yaw = player.face_yaw;
    } else if strafe_offset != 0.0 {
        player.model_yaw = ease_strafe_yaw(player.model_yaw, player.face_yaw, strafe_offset, dt);
    } else if moving || airborne {
        player.model_yaw = player.face_yaw;
    } else {
        let delta = wrap_pi(player.face_yaw - player.model_yaw);
        let mut step = (delta.abs() - std::f32::consts::FRAC_PI_2).max(0.0); // the ceiling
        if !steering {
            step += dt * TURN_RATE * STATIONARY_CHASE_RATE; // the release sweep
        }
        body_turn_step = step.min(delta.abs()).copysign(delta);
        player.model_yaw = wrap_pi(player.model_yaw + body_turn_step);
    }

    let mut anim_flags = move_flags_now;
    if !swimming && !moving && !airborne {
        anim_flags &= !(move_flags::TURN_LEFT | move_flags::TURN_RIGHT);
        if body_turn_step > 1e-5 {
            anim_flags |= move_flags::TURN_LEFT; // +yaw = turning left
        } else if body_turn_step < -1e-5 {
            anim_flags |= move_flags::TURN_RIGHT;
        }
    }
    anim_flags
}
