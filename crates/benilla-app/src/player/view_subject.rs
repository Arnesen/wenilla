//! **What the camera orbits** — the body, or a far-sight object standing somewhere else entirely.
//!
//! Decision 0092 settled that this client has two answers to "where am I?": the **camera eye**
//! (what it draws, culls and clicks) and the **active-player character** (what it is in and hears).
//! Far sight adds no third answer — it moves the *eye*, and leaves the character exactly where it
//! was. That is the whole of the feature, and it is why this is a published fact rather than a
//! branch in the controller: Mind Vision must keep your body walking, your zone text, your
//! minimap, your ambience and your movement stream all yours, while the picture comes from
//! somewhere else. Free-fly ([`Player::detached`]) looks superficially similar and is the opposite
//! shape — it swaps the *whole* control branch and parks the mover — so far sight must never be
//! built as its peer.
//!
//! **The server names the object and nothing else announces it** (VERIFIED vmangos): the only
//! signal is `PLAYER_FARSIGHT` (player field 712, a guid pair) arriving in a values block for our
//! own object. Non-zero means look through it, zero means come home. Three details of how it
//! arrives are load-bearing here:
//!
//! - **The set runs ~400 ms late** (`Player::ScheduleCameraUpdate` defers it by `BATCHING_INTERVAL`)
//!   while the clear is immediate. So the object is always created, and its surroundings always
//!   streamed, well before we are told to look through it — we never have to wait for the subject.
//! - **The clear is broadcast to every client in range**, not just ours: it goes out through
//!   `DirectSendPublicValueUpdate`, which bypasses the field's PRIVATE flag entirely. Reading only
//!   our own [`SelfPlayer`] store is what keeps a neighbour cancelling Mind Vision from moving our
//!   camera.
//! - **`SMSG_CLEAR_FAR_SIGHT_IMMEDIATE` never arrives.** vmangos has zero send sites for it, so
//!   there is nothing to handle and nothing to wait for; every teardown is the field going to zero.
//!
//! Mind Control rides the same field (the server sets it to the victim alongside the control
//! handoff), so possession's camera half falls out of this for free and needs no arm of its own.

use bevy::prelude::*;

use super::{head_height, CameraPivot};
use crate::net::{ClientCommand, GuidIndex, NetCommands, ObjectStore, SelfPlayer};

/// Where the camera should sit its rig this frame, when that is *not* our own body.
///
/// `None` — the overwhelmingly common case — means "the body", and the controller takes its
/// normal path. This deliberately carries a resolved **pose** rather than an `Entity`: the
/// controller cannot query an arbitrary unit's `Transform` (it already holds the self entity's
/// mutably, and the two queries would conflict), and resolving here also lets one place answer
/// "far sight is set but the object is not streamed" without the camera ever seeing a half-state.
#[derive(Resource, Default)]
pub(crate) struct ViewSubject {
    pub(crate) remote: Option<RemoteView>,
}

/// A far-sight subject's pose, in the two forms the camera rig asks for.
#[derive(Clone, Copy)]
pub(crate) struct RemoteView {
    /// The subject's feet, world space — what the rig orbits.
    pub(crate) feet: Vec3,
    /// Its framing pivot height above those feet (attachment-17 derived, scaled), the same
    /// quantity [`head_height`] answers for our own avatar.
    pub(crate) pivot_height: f32,
}

impl RemoteView {
    /// The origin the camera-collision boom sweeps *from*.
    ///
    /// For our own body the controller roots this at the capsule's top hemisphere centre, which it
    /// can do because it owns the avatar's capsule constants. A far-sight subject has no capsule of
    /// ours to read, so the framing pivot stands in — it is head-height by construction, which is
    /// what the sweep wants.
    ///
    /// **Rooting the sweep at the subject rather than at our body is not a detail.** The boom is
    /// cast from this point out to the seat; left at our own head it would be cast across the whole
    /// world to a subject that may be hundreds of yards away, and jam against the first wall in
    /// between — parking the camera at our feet with the view pointing at nothing.
    pub(crate) fn sweep_origin(&self) -> Vec3 {
        self.feet + Vec3::Y * self.pivot_height
    }
}

/// Resolve `PLAYER_FARSIGHT` on our own descriptor into a pose the rig can use.
///
/// Runs before the controller so the substitution is a plain read there. Deliberately silent about
/// a far sight whose object we have not streamed: the camera stays on the body, which is the right
/// picture while we wait, and the server's ordering makes the gap vanishingly rare.
pub(super) fn publish_view_subject(
    mut subject: ResMut<ViewSubject>,
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    index: Res<GuidIndex>,
    // The subject's pose, its model-derived pivot, and its **raw** scale — the pivot height takes
    // the raw `OBJECT_FIELD_SCALE_X`, not the eased render scale ([`head_height`]).
    poses: Query<(
        &Transform,
        Option<&CameraPivot>,
        Option<&crate::net::NetEntity>,
    )>,
    net: Res<NetCommands>,
    mut engaged: Local<Option<u64>>,
) {
    let anchor = self_store
        .iter()
        .next()
        .and_then(|store| store.0.player_farsight());
    subject.remote = anchor
        .and_then(|guid| index.0.get(&guid).copied())
        .and_then(|entity| poses.get(entity).ok())
        .map(|(t, pivot, net)| RemoteView {
            feet: t.translation,
            pivot_height: head_height(pivot, net.map_or(1.0, |n| n.scale)),
        });

    // The toggle vote, on the edge — the reference sends it from exactly two sites, `1` as the view
    // attaches and `0` as it releases (`0x5ee290`). Keyed off the FIELD, not off whether we managed
    // to resolve the object: the server is asking about the view it already chose, and answering
    // "released" merely because the subject has not streamed yet would tear down its object stream
    // around the very object we are waiting for.
    if *engaged != anchor {
        if anchor.is_some() {
            let _ = net.0.send(ClientCommand::FarSight { engage: true });
        } else {
            let _ = net.0.send(ClientCommand::FarSight { engage: false });
        }
        *engaged = anchor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three states the resolver has to tell apart, and the two that must both read "body".
    ///
    /// The middle one is the case the ordering makes rare but not impossible — the field names an
    /// object we have not streamed. Answering `None` there is what keeps the camera on the body
    /// instead of at the world origin, which is what a naive `unwrap_or_default` would give.
    #[test]
    fn an_unset_or_unstreamed_far_sight_both_resolve_to_the_body() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.init_resource::<ViewSubject>()
            .init_resource::<GuidIndex>()
            .insert_resource(NetCommands(tx))
            .add_systems(Update, publish_view_subject);
        // The toggle vote, drained as engage/release booleans.
        let votes = |rx: &crossbeam_channel::Receiver<ClientCommand>| -> Vec<bool> {
            rx.try_iter()
                .filter_map(|c| match c {
                    ClientCommand::FarSight { engage } => Some(engage),
                    _ => None,
                })
                .collect()
        };

        // A subject standing 100 yards away, at scale 1.
        let subject = app
            .world_mut()
            .spawn((
                Transform::from_xyz(100.0, 0.0, 0.0),
                CameraPivot { height_local: 2.0 },
            ))
            .id();

        // No far sight: the body.
        let me = app
            .world_mut()
            .spawn((
                SelfPlayer,
                Transform::default(),
                ObjectStore(benilla_protocol::ObjectFields::from_pairs(&[])),
            ))
            .id();
        app.update();
        assert!(
            app.world().resource::<ViewSubject>().remote.is_none(),
            "no far sight set → the camera stays on the body"
        );
        assert!(votes(&rx).is_empty(), "no field, no vote");

        // Far sight set, but the object never streamed — still the body, not the origin.
        const FARSIGHT: u16 = 712;
        let set = |app: &mut App, lo: u32| {
            *app.world_mut().get_mut::<ObjectStore>(me).unwrap() =
                ObjectStore(benilla_protocol::ObjectFields::from_pairs(&[
                    (FARSIGHT, lo),
                    (FARSIGHT + 1, 0),
                ]));
        };
        set(&mut app, 0x1234);
        app.update();
        assert!(
            app.world().resource::<ViewSubject>().remote.is_none(),
            "far sight naming an unstreamed object → the body, never the world origin"
        );
        assert_eq!(
            votes(&rx),
            [true],
            "the vote is keyed off the FIELD, not off resolving the object — voting 'released' \
             here would tear down the server's stream around the object we are waiting for"
        );
        app.update();
        assert!(
            votes(&rx).is_empty(),
            "and it fires on the edge, not per frame"
        );

        // Streamed: the subject's feet, and its pivot as the sweep origin's height.
        app.world_mut()
            .resource_mut::<GuidIndex>()
            .0
            .insert(0x1234, subject);
        app.update();
        let remote = app
            .world()
            .resource::<ViewSubject>()
            .remote
            .expect("a streamed far-sight object resolves");
        assert_eq!(remote.feet, Vec3::new(100.0, 0.0, 0.0));
        assert_eq!(
            remote.sweep_origin(),
            Vec3::new(100.0, 2.0, 0.0),
            "the boom sweeps from the SUBJECT's head, not ours — otherwise it is cast across the \
             world and stops at the first wall in between"
        );

        // Cleared back to zero: home, by the same path a real teardown takes.
        set(&mut app, 0);
        app.update();
        assert!(
            app.world().resource::<ViewSubject>().remote.is_none(),
            "a zeroed field is the ONLY teardown signal there is — no CLEAR_FAR_SIGHT_IMMEDIATE"
        );
        assert_eq!(votes(&rx), [false], "and the release votes 0, once");
    }
}
