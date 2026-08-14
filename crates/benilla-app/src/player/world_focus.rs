//! **The game's half of 1160's wire (a) and the settle release** — what the world is told, and
//! what the game does with what the world publishes back.
//!
//! The terrain streamer used to read `player::Player` directly (where the avatar is, whether it is
//! settled) and write it back (lifting the post-snap hold when the destination's colliders
//! arrived). Both directions are the engine reaching across the line: a world renderer cannot
//! depend on a game's avatar type, and `benilla-worldview` proved the point by having to stub one.
//!
//! Inverted, it is two systems and no shared type:
//!
//! - [`publish_view_focus`] answers the world's one question — *where should I stream from* —
//!   ahead of the stream stage. The viewer with no avatar (a capture run, the world viewer)
//!   answers `ViewFocus::camera()` and needs nothing else.
//! - [`release_post_snap_hold`] reads the residency the world publishes and decides when the
//!   mover's hold ends. The *decision* is the game's; the *fact* is the world's. That split is why
//!   decision 0737's law survives the move intact — the hold still ends on residency and never on
//!   ground contact, and every mover mode still releases the same way, because there is still
//!   exactly one place that does it.

use bevy::prelude::*;

use super::{Player, SETTLE_TIMEOUT};
use benilla_world::terrain_stream::{ViewFocus, WorldLoadProgress};
use benilla_world::view::Viewer;

/// Tell the world where the avatar's body is, in the one shape the three lanes that ask actually
/// want (see [`Viewer`]). Same frame position and same gate as the focus below.
pub(super) fn publish_viewer(
    mut viewer: ResMut<Viewer>,
    player: Option<Res<Player>>,
    rig: Option<Res<super::CameraControl>>,
    screen: Option<Res<crate::loading_screen::LoadingScreen>>,
    store: Query<&crate::net::ObjectStore, With<crate::net::SelfPlayer>>,
) {
    // The viewer's *condition*, off **our own character's** descriptor block: both are whole-screen
    // effects, which is why they ride here rather than on any body in the scene — and why they read
    // `SelfPlayer` rather than the body we drive. Being drunk is a fact about you; possessing a boar
    // does not sober you up, and a boar has no drunk byte to read (decision 1277).
    let (drunk, ghost) = match store.single() {
        Ok(s) => (
            s.0.player_drunk_byte()
                .map_or(0.0, |b| f32::from(b.min(100)) / 100.0),
            s.0.player_is_ghost(),
        ),
        Err(_) => (0.0, false),
    };
    let body = match player.as_deref() {
        Some(p) if p.active && !p.detached => Viewer {
            at: Some(p.pos),
            move_flags: p.move_flags(),
            planar_speed: p.planar_speed(),
            height: p.collision_height.0,
            ..Viewer::default()
        },
        _ => Viewer::default(),
    };
    *viewer = Viewer {
        drunk,
        ghost,
        // The bare zoom feather. The mesh-side writer folds the aura factor separately, so this
        // must stay the zoom alone or the self body double-applies it.
        self_fade: rig.as_deref().map_or(1.0, super::CameraControl::self_fade),
        world_covered: screen.as_deref().is_some_and(|s| s.covering()),
        ..body
    };
}

/// Tell the world where to stream from, once per frame, before the stream stage reads it.
pub(super) fn publish_view_focus(
    mut focus: ResMut<ViewFocus>,
    player: Option<Res<Player>>,
    roster: Option<Res<crate::char_select::Roster>>,
) {
    let entry = roster
        .as_deref()
        .and_then(crate::char_select::Roster::pending_entry);
    // The pacing bit: spawn caps apply only to a live avatar standing in a settled world. Through
    // entry, a teleport and a world swap the loading cover is absorbing the burst, and a cap there
    // would only lengthen the reveal.
    *focus = match player.as_deref() {
        Some(p) if p.active => {
            let wow = benilla_assets::coords::bevy_to_wow(p.pos);
            let paced = !p.settling && !p.world_stale;
            if p.detached {
                ViewFocus::detached(wow, paced)
            } else {
                ViewFocus::body(wow, paced)
            }
        }
        // No avatar: the picked character's row for the entry window (decision 0777), else
        // whatever the camera can see.
        _ => match entry {
            Some((map, pos)) => ViewFocus::entry(map, pos),
            None => ViewFocus::camera(),
        },
    };
}

/// End the post-snap hold when the destination's world has arrived — decision 0737, reading the
/// streamer's published residency instead of being written by it.
///
/// Runs after the stream stage so `colliders_pending` is this frame's count, which is the same
/// freshness `finish_colliders` heading the streaming chain was always there to give it.
pub(super) fn release_post_snap_hold(
    mut player: ResMut<Player>,
    progress: Option<Res<WorldLoadProgress>>,
    time: Res<Time>,
) {
    let Some(p) = progress else { return };
    // The streamer is the only authority on *which map* the colliders under the avatar belong to.
    // Residency here means this map's own tile (or, on a WMO-only map, its one building) is
    // spawned — reachable only after a swap has drained every tile of the map we left.
    if p.focus_resident && p.total > 0 {
        player.world_stale = false;
    }
    if !player.settling {
        return;
    }
    // The release ends on scene AND colliders, never on ground contact — feet-on-ground dragged
    // the whole mover-mode matrix into a loading decision, and a flyer or swimmer never touched
    // it. While the resident colliders still belong to the map we just left the deadline is
    // pushed (0710's fail-closed law: a world that never arrives keeps the hold and the screen).
    let now = time.elapsed_secs();
    if player.world_stale {
        player.settle_deadline = now + SETTLE_TIMEOUT;
    } else if p.scene_ready && p.colliders_pending == 0 {
        player.end_settle(true, now);
    } else if now >= player.settle_deadline {
        player.end_settle(false, now);
    }
}
