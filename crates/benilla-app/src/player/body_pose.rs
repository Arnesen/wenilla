//! **Writing the frame onto the body we drive** — the last thing the controller does to the
//! avatar itself, after the mover has moved it and [`super::gait`] has decided where it faces.
//!
//! The driven entity is a streamed unit like any other (decision 0041), so this is not a special
//! "player render" path: it writes the same transform and the same `MovementState` the entity
//! renderer would, which is exactly why a *possessed creature* needs nothing extra here
//! (decision 1277). It is also where the frame's landing is reported to the local hard-landing
//! predictor, and where the camera-pivot target for this frame is read off the body — the one
//! value that leaves this module, because the camera seat consumes it.

use bevy::prelude::*;

use crate::creature_anim::move_flags;

use super::{model_pivot_height, wrap_pi, BodyQuery, CameraPivot, Player};

/// Write this frame onto the driven body and return the camera-pivot **target** height it carries.
/// `anim_flags` is [`super::gait::drive_body_heading`]'s verdict; `move_flags_now` is the live
/// wire word, whose forward/back bits gate the swim body pitch.
#[allow(clippy::too_many_arguments)]
pub(super) fn drive(
    player: &Player,
    body: &mut BodyQuery,
    hard_landing: &mut MessageWriter<crate::creature_anim::HardLanding>,
    swimming: bool,
    swim_pitch: f32,
    move_flags_now: u32,
    anim_flags: u32,
    landed: bool,
    stand_now: u8,
) -> Option<f32> {
    // Drive the streamed self entity: its transform is the avatar's pose (feet position + body
    // heading, like every other streamed unit), and its `MovementState` is the live movement the
    // animation selector reads. Scale is left untouched (the renderer baked the display scale on).
    // `horiz_vel` is already the directional speed (runBack when backpedaling), so the backpedal
    // clip scales by it and no longer drags.
    // This frame's camera-pivot **target** — the model-local [`CameraPivot`] × the body's RAW
    // scale, clamped (see [`super::camera::head_height`] for why raw and not the rendered scale).
    // `None` while the body has no model yet: the channel holds rather than aiming at a
    // placeholder, which is what makes a display swap one glide instead of two.
    let mut cam_pivot_target = None;
    if let Ok((entity, mut t, motion, pivot, .., twist, _, net_entity)) = body.single_mut() {
        t.translation = player.pos;
        // The swim body pitch (TU-A, `0x60a110`→`0x710620`): while swimming AND moving fwd/back
        // the model root renders `Rz(yaw)·Ry(−pitch)` — in Bevy axes, the yaw then a nose-up
        // pitch about the body's local X. Strafe-only, idle, and grounded all render LEVEL (the
        // ground path) — exactly the gate the client's per-frame `+0x3c` sync branches on.
        // The pitch presented is this frame's `swim_pitch` — the raw aim, except leveled by
        // the 0499 surface redirect when the rest-line cap bites (the body swims flat along
        // the surface, not pitched against it); the wire tail streams the same value.
        t.rotation =
            if swimming && move_flags_now & (move_flags::FORWARD | move_flags::BACKWARD) != 0 {
                Quat::from_rotation_y(player.model_yaw) * Quat::from_rotation_x(swim_pitch)
            } else {
                Quat::from_rotation_y(player.model_yaw)
            };
        // Report every landing's fall height for the client-side landing predictor
        // (`0x602d00`, decision 0412): its consumers gate on the descent and, past the HARD
        // floor, play the wound grunt + a locally-predicted dust puff at THIS frame — the
        // server's 0x1FC echo arrives ~an RTT later (the reference double-fires the dust the
        // same way). `fall_start_y` still holds this arc's launch height here (it is only
        // re-seeded at the next take-off).
        if landed {
            hard_landing.write(crate::creature_anim::HardLanding {
                entity,
                descent: player.fall_start_y - player.pos.y,
            });
        }
        cam_pivot_target = pivot_target(pivot, net_entity);
        if let Some(mut motion) = motion {
            // A swimmer's stroke rate takes the flag-scalar directional speed (full rate at
            // any pitch — a vertical climb must not freeze the stroke); the ground gaits
            // scale by the achieved horizontal speed as before.
            motion.speed = if swimming {
                player.swim_stroke_speed
            } else {
                player.horiz_vel.length()
            };
            motion.vertical_speed = player.vel_y;
            motion.flags = anim_flags;
            motion.stand_state = stand_now;
        }
        // The counter-twist gap: how far the aim sits from the rendered body — the strafe
        // offset while it lasts, unwinding to zero as `model_yaw` closes on `face_yaw`.
        if let Some(mut twist) = twist {
            // `WOW_TWIST_GAP=<radians>` forces the gap — the counter-twist's A/B lever. The
            // pass is inert at `yaw_gap == 0` and a scripted probe cannot open a real gap
            // (`WOW_PROBE_CAM` turns the model with the camera, so the measured gap is float
            // noise, ~1e-6 rad), which means "removing the twist changed nothing" has never
            // yet been a measurement of the twist — only of a pass that never ran. This is
            // what lets it actually be exercised.
            twist.yaw_gap =
                twist_gap_override().unwrap_or_else(|| wrap_pi(player.face_yaw - player.model_yaw));
        }
    }
    cam_pivot_target
}

/// The camera pivot's **target** height for a driven body: its model-local [`CameraPivot`] × the
/// body's raw `OBJECT_FIELD_SCALE_X`, clamped — or `None` before its model has attached.
///
/// `None` is the load-bearing half. The reference recomputes the pivot preset only on a model event
/// and *skips the camera update entirely* while the model is unresolved (`0x50e907`), so a
/// display swap reads as a brief hold and then one glide. Aiming the channel at a placeholder height
/// during those frames instead would send the camera on a round trip nobody asked for.
///
/// The **raw** scale (not the transform's eased one) is the reference's own input — see
/// [`super::camera::head_height`] for the byte citation and why the distinction is visible.
pub(super) fn pivot_target(
    pivot: Option<&CameraPivot>,
    net: Option<&crate::net::NetEntity>,
) -> Option<f32> {
    pivot.map(|p| model_pivot_height(p, net.map_or(1.0, |n| n.scale)))
}

/// `WOW_TWIST_GAP=<radians>`: pin the body counter-twist's yaw gap instead of deriving it from
/// aim-minus-model. Zero-cost when unset: one env read, once.
fn twist_gap_override() -> Option<f32> {
    static G: std::sync::OnceLock<Option<f32>> = std::sync::OnceLock::new();
    *G.get_or_init(|| {
        std::env::var("WOW_TWIST_GAP")
            .ok()
            .and_then(|v| v.trim().parse::<f32>().ok())
    })
}
