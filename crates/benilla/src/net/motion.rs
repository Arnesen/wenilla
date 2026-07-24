//! Per-frame motion model for streamed units: server splines + remote-player dead-reckoning.
//!
//! Two motion sources drive an entity's [`Transform`] between the sparse server packets (decisions 0052
//! + 0053):
//! - **[`Spline`]** — a server-authored path (`SMSG_MONSTER_MOVE`), sampled at constant speed by
//!   [`spline::sample_splines`]. Creatures ride these.
//! - **[`RemoteMotion`]** — another player's flag-driven locomotion, integrated by
//!   [`remote::extrapolate_remote_units`]: the client's own dead-reckoning in miniature (extrapolate
//!   from the last reported state, snap to the truth on the next packet). A jump is a **ballistic
//!   event** — the launch played out locally under gravity — not flag-driven walking (decision 0053).
//!
//! The parent [`super`] module owns the wire→ECS bridge that seeds + corrects these from the packet
//! stream; this module owns the integration, split by concern: [`spline`] (the path walk + its
//! terrain re-ground), [`remote`] (dead-reckoning + jump ballistics), [`facing`] (wire facing
//! resolution + the client-local idle re-face and its [`FacingStep`] shuffle latch). This face keeps
//! the shared pose glue. Poses live canonically in raw WoW space on the components; the `Transform`
//! is derived (translation + facing only — scale, baked by the renderer, is preserved).

use benilla_assets::coords::wow_to_bevy;
use bevy::prelude::*;

mod facing;
mod remote;
mod spline;
#[cfg(test)]
mod tests;

pub(crate) use facing::FacingStep;
pub(super) use facing::{face_target, resolve_facing};
pub(in crate::net) use remote::{apply_move, arrival_snap};
pub(super) use remote::{drain_pending_moves, extrapolate_remote_units};
pub(crate) use remote::{PendingMove, RelayClock, RemoteMotion};
pub(super) use spline::{
    ground_clamp_creatures, mark_swimming_creatures, monster_move_spline, sample_splines,
};
pub(crate) use spline::{CreatureSwimming, Spline};

/// A full [`Transform`] for a freshly-spawned entity from a raw-WoW pose. WoW orientation (yaw about
/// +Z) maps straight to a Bevy +Y yaw under `wow_to_bevy` — no 180° offset (unlike doodad/WMO
/// *placement* rotation).
pub(super) fn pose_transform(position: [f32; 3], orientation: f32) -> Transform {
    Transform {
        translation: wow_to_bevy(position),
        rotation: Quat::from_rotation_y(orientation),
        ..default()
    }
}

/// Update an existing entity's translation + rotation from a raw-WoW pose, **preserving its scale**
/// (the renderer bakes per-display model scale into the transform; a move must not reset it). The
/// entity may not be query-visible the very frame it spawned — then the move is simply skipped (the
/// spawn pose already placed it, and the next move lands once commands flush).
pub(super) fn write_pose(
    transforms: &mut Query<&mut Transform>,
    e: Entity,
    position: [f32; 3],
    orientation: f32,
) {
    if let Ok(mut t) = transforms.get_mut(e) {
        t.translation = wow_to_bevy(position);
        t.rotation = Quat::from_rotation_y(orientation);
    }
}

/// Recover the WoW yaw from a unit [`Transform`] rotation (always built as [`Quat::from_rotation_y`]).
fn yaw_of(rotation: Quat) -> f32 {
    rotation.to_euler(EulerRot::YXZ).0
}
