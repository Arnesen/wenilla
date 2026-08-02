//! The self-spline ride: when a server-authored spline drives our own player — an `SMSG_MONSTER_MOVE`
//! addressed to our guid — input yields, we ride it out, and we acknowledge `CMSG_MOVE_SPLINE_DONE`
//! so the server hands control back. Warrior **Charge** is the first case (decision 0260); knockback,
//! taxi flight, and fear reuse the same path.
//!
//! The mechanic, pinned live against vmangos: Charge is neither a teleport nor a knockback — the
//! server moves the *caster* with the same `MoveSpline` machinery as any creature (a ground spline at
//! run×4-capped-24 yd/s, facing the target), broadcasts it to the caster too, and for a player mover
//! waits on `CMSG_MOVE_SPLINE_DONE` (the `MovementInfo` at the endpoint + the `splineId`) before it
//! stops treating us as spline-controlled.
//!
//! Division of labour each frame while riding:
//! - [`crate::net`]'s `sample_splines` (Net stage) advances the [`Spline`] into the entity
//!   `Transform` — the sole position authority for the ride. A player owns its Z (no creature
//!   terrain-reground), and the spline's ground path already carries a walkable Z.
//! - [`drive_self_ride`] (Input stage, *before* `control`) owns the *whole* pose while the ride
//!   lasts: it mirrors that transform into [`Player`] (`pos`/`face_yaw`/`model_yaw`), drives a
//!   forward-run animation via [`MovementState`], unwinds the strafe counter-twist ([`BodyTwist`]),
//!   and — the frame the spline ends — emits the ack and clears the ride so `control` resumes from
//!   the endpoint at rest.
//! - `control`'s ride guard carries the follow-camera onto the moving avatar and skips input,
//!   physics, and the outbound movement stream.

use benilla_assets::coords::bevy_to_wow;
use bevy::prelude::*;

use crate::creature_anim::{move_flags, BodyTwist, MovementState};
use crate::net::{ClientCommand, NetCommands, SelfPlayer, Spline};

use super::Player;

/// Extract the Bevy Y-yaw of a facing quaternion. The net bridge and `sample_splines` both write
/// the self entity's rotation as `Quat::from_rotation_y(facing)` (a pure Y turn), and benilla's
/// Bevy yaw equals the WoW orientation (decision 0002), so this recovers both the controller's
/// `face_yaw` and the wire orientation. Shared with the take-control edge (`wire_in`), which adopts
/// the streamed spawn pose's facing the same way.
pub(super) fn yaw_of(rotation: Quat) -> f32 {
    rotation.to_euler(EulerRot::YXZ).0
}

/// Mirror an in-progress self-spline into [`Player`], and ack it when it ends. Runs in
/// [`crate::schedule::WorldStage::Input`] just before `control`, so the pose it publishes is what the
/// camera seats on and the animation reads this frame.
#[allow(clippy::type_complexity)] // a Bevy query's component tuple
pub(super) fn drive_self_ride(
    net: Res<NetCommands>,
    mut commands: Commands,
    mut player: ResMut<Player>,
    mut q: Query<
        (
            Entity,
            &Transform,
            Option<&Spline>,
            Option<&mut MovementState>,
            Option<&mut BodyTwist>,
        ),
        With<SelfPlayer>,
    >,
) {
    // Only while we hold control (post-login). A free-fly detach (`F`) abandons any ride rather than
    // yanking the parked camera — rare, and the spline still finalizes server-side.
    if !player.active || player.detached {
        player.server_riding = false;
        return;
    }
    let Ok((entity, transform, spline, motion, twist)) = q.single_mut() else {
        return;
    };
    // A teleport landed since last frame: the server relocated us, voiding any in-progress ride
    // (the taxi flight-end teleport beats our own spline end by ~latency — vmangos's spline-done
    // handler ignores acks while its teleport is pending, so the relocation IS the hand-back; no
    // `CMSG_MOVE_SPLINE_DONE` is owed, and the teleport ack + position report already went out).
    // Mirroring the still-present spline this frame would clobber the snap back to the stale
    // flight pose — the 4-yd hover whose settle probe then missed the ground for the full 6 s
    // timeout at every taxi landing (decision 0501).
    if std::mem::take(&mut player.ride_abort) {
        if spline.is_some() {
            commands.entity(entity).remove::<Spline>();
        }
        if player.server_riding {
            player.server_riding = false;
            player.move_flags = 0;
            player.airborne_since = None;
            player.vel_y = 0.0;
            player.horiz_vel = Vec3::ZERO;
        }
        return;
    }
    match spline {
        // Riding: the freshly-sampled transform is our pose this frame.
        Some(spline) => {
            if !player.server_riding {
                info!(
                    "charge/ride: server spline {} drives the avatar ({} pts, {:.0} yd/s over {} ms)",
                    spline.id,
                    spline.points.len(),
                    spline.speed(),
                    spline.duration.as_millis(),
                );
            }
            let yaw = yaw_of(transform.rotation);
            player.pos = transform.translation;
            player.face_yaw = yaw;
            player.model_yaw = yaw;
            player.server_riding = true;
            player.ride_spline_id = spline.id;
            // A forward run — the charge reads as a fast run (the gait selector keys on the FORWARD
            // flag + speed). It also gives a sane baseline for the resume and for observers.
            player.move_flags = move_flags::FORWARD;
            if let Some(mut motion) = motion {
                motion.speed = spline.speed();
                motion.vertical_speed = 0.0;
                motion.flags = move_flags::FORWARD;
                motion.stand_state = 0;
            }
            // The ride is a forward run, and the display-facing law's moving-forward case (the
            // `flags & 0x2003` snap — decisions 0101/0103) puts the body ON the aim: no gap, no
            // counter-twist — the same one-frame unwind as releasing a strafe key while running.
            // `control`, the normal gap owner, is parked behind the ride guard, so without this
            // write a charge engaged mid-strafe rode the whole spline with the spine/head frozen
            // ±90° off the run (the director's strafe-engage report).
            if let Some(mut twist) = twist {
                twist.yaw_gap = 0.0;
            }
        }
        // The ride just ended: `sample_splines` wrote the endpoint transform this frame and then
        // dropped the `Spline`. Sync to that exact endpoint, ack the server — it holds us as
        // spline-pending until this arrives, then relocates us and broadcasts a stop to observers —
        // and clear the ride so `control` resumes its own stream from rest.
        None if player.server_riding => {
            player.pos = transform.translation;
            player.face_yaw = yaw_of(transform.rotation);
            player.model_yaw = player.face_yaw;
            player.server_riding = false;
            player.move_flags = 0;
            player.airborne_since = None;
            // "Resumes from the endpoint at rest" (decision 0260) — including the velocities. The
            // mover re-derives them only when it reads grounded; a ride ending a hair above our
            // terrain (navmesh Z vs ours) would otherwise inherit the pre-ride momentum — e.g. a
            // strafe-engaged charge sliding sideways out of its landing.
            player.vel_y = 0.0;
            player.horiz_vel = Vec3::ZERO;
            let _ = net.0.send(ClientCommand::MoveSplineDone {
                flags: 0,
                pos: bevy_to_wow(player.pos),
                orientation: player.face_yaw,
                spline_id: player.ride_spline_id,
            });
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;
    use std::time::{Duration, Instant};

    use super::*;

    /// A one-system app riding a 2-point self-spline, `Player` mid-strafe as at charge engage.
    fn ride_app() -> (App, Entity, crossbeam_channel::Receiver<ClientCommand>) {
        let mut app = App::new();
        app.add_systems(Update, drive_self_ride);
        let (tx, rx) = crossbeam_channel::unbounded();
        app.insert_resource(NetCommands(tx));
        app.insert_resource(Player {
            active: true,
            // Stale pre-ride momentum: a strafe was held when the spline took over.
            horiz_vel: Vec3::new(5.0, 0.0, 0.0),
            vel_y: -2.0,
            ..Default::default()
        });
        // Mid-strafe counter-twist: the aim sits 90° off the rendered root.
        let mut twist = BodyTwist::new(None, None);
        twist.yaw_gap = -FRAC_PI_2;
        let entity = app
            .world_mut()
            .spawn((
                Transform::default(),
                Spline {
                    points: vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0]],
                    start: Instant::now(),
                    duration: Duration::from_secs(600), // far from ending during the test
                    id: 77,
                    grounded: true,
                },
                twist,
                SelfPlayer,
            ))
            .id();
        (app, entity, rx)
    }

    #[test]
    fn engaging_a_ride_mid_strafe_unwinds_the_counter_twist() {
        // The ride owns the pose: `control` (the normal gap owner) is parked behind the ride
        // guard, so the ride itself must zero the strafe counter-twist — frozen at ±90°, the
        // avatar charged the whole spline with its spine/head twisted off the run direction.
        let (mut app, entity, _rx) = ride_app();
        app.update();
        assert!(app.world().resource::<Player>().server_riding);
        let gap = app.world().get::<BodyTwist>(entity).unwrap().yaw_gap;
        assert_eq!(
            gap, 0.0,
            "riding forward, the body is on the aim — no counter-twist"
        );
    }

    /// The landing teleport voids the ride (decision 0501): the server relocates us at ITS
    /// flight end, before our own spline finishes — the mirror must not clobber the snap, the
    /// spline drops, and no `CMSG_MOVE_SPLINE_DONE` goes out (vmangos ignores it mid-teleport).
    #[test]
    fn a_teleport_aborts_the_ride_without_an_ack() {
        let (mut app, entity, rx) = ride_app();
        app.update(); // riding
        app.world_mut().resource_mut::<Player>().ride_abort = true;
        app.update(); // the abort frame: no mirror, spline dropped
        let player = app.world().resource::<Player>();
        assert!(!player.server_riding);
        assert!(
            app.world().get::<Spline>(entity).is_none(),
            "the spline is dropped with the ride"
        );
        app.update(); // and the ride-end arm must NOT fire afterwards (server_riding cleared)
        assert!(
            rx.try_recv().is_err(),
            "no MoveSplineDone — the teleport relocation superseded the ride"
        );
    }

    #[test]
    fn ride_end_acks_the_spline_and_resumes_at_rest() {
        let (mut app, entity, rx) = ride_app();
        app.update(); // riding
        app.world_mut().entity_mut(entity).remove::<Spline>();
        app.update(); // the ride-end edge
        let player = app.world().resource::<Player>();
        assert!(!player.server_riding);
        assert_eq!(
            (player.vel_y, player.horiz_vel),
            (0.0, Vec3::ZERO),
            "the controller resumes from the endpoint at rest (decision 0260) — \
             stale pre-ride momentum must not leak into the resume"
        );
        match rx.try_recv() {
            Ok(ClientCommand::MoveSplineDone { spline_id, .. }) => assert_eq!(spline_id, 77),
            Ok(_) => panic!("expected the MoveSplineDone ack, got another command"),
            Err(_) => panic!("expected the MoveSplineDone ack, got nothing"),
        }
    }
}
