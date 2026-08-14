//! **What the client embodies** — which single unit it simulates, animates from input, and streams.
//!
//! Decision 0092 gave this client two answers to "where am I?": the **camera eye** and the
//! **active-player character**. Possession forces a third, and the reference has had it all along
//! as its own global: the **active mover** (`ds:0xc4da98`, written only by `SetActiveMover
//! 0x6006e0`), which the input applier `0x514640` resolves at the top of every tick and *skips the
//! whole tick* when it does not resolve. The three are genuinely independent stores there — the
//! camera never consults the mover to pick its anchor, and neither of them touches "the active
//! player" (`ds:0xb41414`), which is invariant under both far sight and possession (VERIFIED,
//! wow-re `object-layer/scratch/farsight-and-client-control.md` §9).
//!
//! So benilla splits the *marker*, not the identity. [`SelfPlayer`] keeps meaning **my character**
//! — bags, auras, quest log, paper doll, the name over the head — and never moves.
//! [`ActiveMover`] means **the body in my hands**, and moves. Of ~150 `SelfPlayer` query sites the
//! large majority are the first kind and are untouched by possession; the ones that changed are
//! exactly those that simulate, animate from input, render the body the camera is inside, or
//! stream it.
//!
//! Three properties of the placement carry the weight:
//!
//! - **Forbidden is nobody too.** A body we are told we may not move — ours under a Mind Control,
//!   or a possessed creature the moment it is feared — leaves the marker unplaced, mirroring the
//!   reference's own zeroing of the mover global. That is what lets the *server* drive it: the
//!   ordinary relayed-motion path skips whatever carries this marker, so a body that keeps it while
//!   the controller refuses to drive it is driven by nobody at all, and stands frozen while the
//!   server runs it around in front of everybody else.
//! - **Unresolvable is nobody, never a fallback.** A claimed grant whose object has not streamed in
//!   leaves the marker unplaced, because the alternative — quietly leaving it on our own body —
//!   drives our body under the creature's mover, and outbound `MSG_MOVE_*` carry no guid: the
//!   server writes our pose onto the creature. That is the sharpest trap in this family (decision
//!   1269 §3), and here it is structurally impossible rather than guarded against.
//! - **The reins change hands by GUID, not by entity.** A cross-map worldport despawns and
//!   re-streams our own body under a fresh entity while we never stop driving it, so keying the
//!   handover on the entity would re-seize (and re-settle) on every zone transfer.

use bevy::prelude::*;

use super::follow::FollowState;
use super::state::Player;
use crate::creature_anim::MovementState;
use crate::net::{ActiveMover, GuidIndex, RemoteMotion, SelfGuid, SelfPlayer};

/// Keep [`ActiveMover`] on the one body we drive: the possessed unit while we hold its reins, our
/// own otherwise, and nobody at all while neither is streamed.
///
/// Runs before the controller, which reads the marker to decide what — if anything — it is driving.
pub(super) fn maintain_active_mover(
    mut commands: Commands,
    mut player: ResMut<Player>,
    mut follow: ResMut<FollowState>,
    guids: (Res<SelfGuid>, Res<GuidIndex>),
    self_body: Query<Entity, With<SelfPlayer>>,
    tagged: Query<Entity, With<ActiveMover>>,
    mut driving: Local<Option<u64>>,
) {
    let (self_guid, index) = (&guids.0, &guids.1);
    // Who we mean to drive, and which entity that is. Three answers, not two:
    //
    // - **Forbidden to move it** ([`Player::control_lost`]) — nobody. This is the reference's own
    //   rule rather than an optimisation: `0x5fa600` *zeroes the mover global* for the named unit,
    //   and the consequence is the whole point. A mind-controlled player's body stops being locally
    //   simulated and falls back to the ordinary relayed-motion path, so it visibly walks where its
    //   captor drives it instead of standing frozen while the server runs it around. The same for a
    //   possessed creature that has just been feared: the server's flee path drives it, and we watch
    //   through a camera that is still on it.
    // - **A claimed foreign mover** answers by itself: either its object is streamed and it is the
    //   mover, or nothing is.
    // - Otherwise our own body.
    let want_guid = if player.control_lost {
        None
    } else {
        player.foreign_mover.or(self_guid.0)
    };
    let want = match (player.control_lost, player.foreign_mover) {
        (true, _) => None,
        (false, Some(guid)) => index.0.get(&guid).copied(),
        (false, None) => self_body.iter().next(),
    };

    if *driving != want_guid {
        *driving = want_guid;
        if want_guid.is_some() {
            // Whatever pose and momentum `Player` holds describe the body we just let go of, so the
            // controller has to adopt the new one's before it drives anything. It also drives
            // nothing at all until it has: see [`Player::reseat`].
            player.reseat = true;
            // The reference tears the same thing down on the outgoing mover — `SetActiveMover
            // 0x6006e0` calls `0x6103a0` → `0x60fb60(0, 1)`, cancelling click-to-move and follow.
            // A follow that survived would steer the creature toward whoever our *character* was
            // walking behind.
            follow.stop();
        }
    }

    let held = tagged.iter().next();
    if held == want {
        return;
    }
    for e in &tagged {
        commands.entity(e).remove::<ActiveMover>();
        // Hand the unit back to the ordinary remote path. [`MovementState`] is `unify`'s
        // *top-precedence* leg, so one left behind would freeze the creature's animation on the
        // last view we drove it with and shadow the relayed `MSG_MOVE_*` stream forever. Our own
        // body keeps its own — `net::apply::tag_self_player` owns that one and never re-inserts it.
        if !self_body.contains(e) {
            commands.entity(e).remove::<MovementState>();
        }
    }
    if let Some(e) = want {
        // Drop whatever server-replay state the unit carried at the instant we took it. While we
        // drive it the server sends us no relays for it (vmangos excludes the mover's own session
        // from the broadcast), so nothing accumulates — but a move already queued for a future
        // apply would otherwise fire after we let go and yank the unit back to a pose it left.
        commands
            .entity(e)
            .insert(ActiveMover)
            .remove::<RemoteMotion>();
        // A creature carries no controller-fed movement view — only our own avatar is given one,
        // beside its `SelfPlayer` tag. Without it the possessed unit would walk with its idle
        // animation playing: `control` writes this component, and `unify` reads it first.
        if !self_body.contains(e) {
            commands.entity(e).insert(MovementState::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::RemoteMotion;

    fn harness() -> (App, Entity) {
        let mut app = App::new();
        app.init_resource::<Player>()
            .init_resource::<FollowState>()
            .init_resource::<SelfGuid>()
            .init_resource::<GuidIndex>()
            .add_systems(Update, maintain_active_mover);
        let me = app.world_mut().spawn(SelfPlayer).id();
        app.world_mut().resource_mut::<SelfGuid>().0 = Some(0xAA);
        app.world_mut()
            .resource_mut::<GuidIndex>()
            .0
            .insert(0xAA, me);
        (app, me)
    }

    fn mover(app: &mut App) -> Option<Entity> {
        let mut q = app
            .world_mut()
            .query_filtered::<Entity, With<ActiveMover>>();
        q.iter(app.world()).next()
    }

    /// A remote unit's replay state, minimally populated — only its presence is under test.
    fn replaying() -> RemoteMotion {
        RemoteMotion {
            wow_pos: [0.0; 3],
            orientation: 0.0,
            flags: 0,
            pitch: 0.0,
            speed: 0.0,
            vertical_velocity: 0.0,
            jump_xy_vel: [0.0; 2],
            fall_start_z: None,
            pending: std::collections::VecDeque::new(),
            relay: Default::default(),
            last_apply_ms: 0.0,
            last_apply_pos: [0.0; 3],
        }
    }

    /// The whole handover, and the two states a casual version gets wrong: a claim we cannot yet
    /// resolve must leave the marker on **nobody**, and coming home must hand the creature back to
    /// the remote path intact.
    #[test]
    fn a_claim_we_cannot_resolve_yet_moves_the_marker_to_nobody_not_to_our_own_body() {
        let (mut app, me) = harness();
        app.update();
        assert_eq!(mover(&mut app), Some(me), "no claim → our own body");

        // Claimed, but the creature has not streamed in.
        app.world_mut().resource_mut::<Player>().foreign_mover = Some(0xBB);
        app.update();
        assert_eq!(
            mover(&mut app),
            None,
            "an unresolvable claim is NOBODY — leaving it on our body drives us under the \
             creature's mover, and outbound moves carry no guid of their own"
        );
        assert!(
            app.world().resource::<Player>().reseat,
            "and the controller is told to drive nothing until it has a pose to adopt"
        );

        // It streams in.
        let boar = app.world_mut().spawn(replaying()).id();
        app.world_mut()
            .resource_mut::<GuidIndex>()
            .0
            .insert(0xBB, boar);
        app.update();
        assert_eq!(mover(&mut app), Some(boar));
        assert!(
            app.world().entity(boar).contains::<MovementState>(),
            "a creature carries no controller-fed movement view of its own — without one it walks \
             with its idle animation playing"
        );
        assert!(
            !app.world().entity(boar).contains::<RemoteMotion>(),
            "and its queued server replay is dropped, or it snaps back the moment we let go"
        );

        // Released.
        app.world_mut().resource_mut::<Player>().foreign_mover = None;
        app.update();
        assert_eq!(mover(&mut app), Some(me), "the reins come home");
        assert!(
            !app.world().entity(boar).contains::<MovementState>(),
            "`unify` reads this leg FIRST, so one left behind freezes the creature's animation on \
             the last view we drove it with, forever"
        );
    }

    /// The one component this system must never take away. `tag_self_player` inserts our body's
    /// [`MovementState`] beside the `SelfPlayer` tag and never inserts it again, so stripping it
    /// here — as the creature leg does — would leave our own avatar animation-dead for the rest of
    /// the session, and only after a possession.
    #[test]
    fn our_own_body_never_loses_its_movement_view_to_a_possession() {
        let (mut app, me) = harness();
        app.world_mut()
            .entity_mut(me)
            .insert(MovementState::default());
        app.update();

        app.world_mut().resource_mut::<Player>().foreign_mover = Some(0xBB);
        let boar = app.world_mut().spawn(replaying()).id();
        app.world_mut()
            .resource_mut::<GuidIndex>()
            .0
            .insert(0xBB, boar);
        app.update();
        app.world_mut().resource_mut::<Player>().foreign_mover = None;
        app.update();

        assert!(
            app.world().entity(me).contains::<MovementState>(),
            "our own body's movement view is `tag_self_player`'s to own, never this system's"
        );
    }

    /// A cross-map worldport despawns and re-streams our own body under a fresh entity while we
    /// never stop driving it. Keying the handover on the entity would re-seize — and re-settle —
    /// on every zone transfer.
    #[test]
    fn re_streaming_our_own_body_moves_the_marker_without_calling_it_a_handover() {
        let (mut app, me) = harness();
        app.update();
        app.world_mut().resource_mut::<Player>().reseat = false;

        app.world_mut().entity_mut(me).despawn();
        let reborn = app.world_mut().spawn(SelfPlayer).id();
        app.world_mut()
            .resource_mut::<GuidIndex>()
            .0
            .insert(0xAA, reborn);
        app.update();

        assert_eq!(
            mover(&mut app),
            Some(reborn),
            "the marker follows the entity"
        );
        assert!(
            !app.world().resource::<Player>().reseat,
            "but the mover GUID never changed, so this is not a handover: re-seizing here would \
             discard the worldport's own snap and re-run the settle"
        );
    }
    /// Being forbidden to move a body hands it back to the *server's* motion, and that is visible:
    /// a mind-controlled player must be seen walking where their captor drives them. The marker
    /// staying on our own body is what would freeze it — `control` refuses to drive it, and the
    /// remote-replay path skips whatever carries the marker, so it would be driven by nobody at all.
    #[test]
    fn a_body_we_may_not_move_is_driven_by_nobody_so_the_server_can_drive_it() {
        let (mut app, me) = harness();
        app.update();
        assert_eq!(mover(&mut app), Some(me));

        // Mind-controlled: the server named us with allowMove = 0.
        app.world_mut().resource_mut::<Player>().control_lost = true;
        app.update();
        assert_eq!(
            mover(&mut app),
            None,
            "the reins are nobody's — our body rejoins the relayed-motion path and walks where it \
             is driven, instead of standing frozen while the server runs it around"
        );

        // And a possessed creature we have just been forbidden to move behaves the same way.
        app.world_mut().resource_mut::<Player>().foreign_mover = Some(0xBB);
        let boar = app.world_mut().spawn(replaying()).id();
        app.world_mut()
            .resource_mut::<GuidIndex>()
            .0
            .insert(0xBB, boar);
        app.update();
        assert_eq!(mover(&mut app), None, "held, but not ours to move");

        // Control back: the marker returns, and the body under us is re-adopted — it moved while
        // the server had it.
        app.world_mut().resource_mut::<Player>().reseat = false;
        app.world_mut().resource_mut::<Player>().control_lost = false;
        app.update();
        assert_eq!(mover(&mut app), Some(boar));
        assert!(
            app.world().resource::<Player>().reseat,
            "the creature ran under the server's fear path; carrying our stale pose onto it would \
             snap it back to wherever we last drove it"
        );
    }
}
