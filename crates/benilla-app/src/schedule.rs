//! The cross-subsystem **frame ordering contract** for the world-transition pipeline.
//!
//! A teleport must not flash the half-loaded world: the moment the player snaps, the loading screen
//! has to cover the swap *that same frame*. That requires four subsystems — owned by separate
//! plugins — to run in a fixed order within `Update`:
//!
//! 0. [`WorldStage::Net`] — `net::apply_net_updates` drains the world stream into ECS entities and
//!    surfaces the teleport/worldport as Bevy events, *before* the player reads them.
//! 1. [`WorldStage::Input`] — `player::control` applies input + the teleport/worldport snap.
//! 2. [`WorldStage::Stream`] — `terrain_stream::stream_terrain` reacts: swaps/streams tiles around the
//!    NEW position and publishes residency ([`crate::loading_screen::WorldLoadProgress`]).
//! 3. [`WorldStage::Present`] — `loading_screen::drive_loading_screen` reads that residency and shows/
//!    hides the cover. Visibility set here propagates in `PostUpdate` and renders the same frame.
//!
//! Expressing this as a [`SystemSet`] (rather than `.after(some_fn)`) keeps each system + its private
//! params encapsulated in its own module — plugins opt in with `.in_set(..)` and never reference one
//! another's functions. Add future world-transition work to the matching stage.

use bevy::prelude::*;

use crate::char_select::ClientState;

/// **Is there a world?** — the run condition every world-*owning* subsystem gates on.
///
/// The world exists only while a character is in it. That reads as a truism and was, for most of
/// this project, false: benilla began as a terrain viewer, so the streamers ran from `Startup` and
/// the client loaded and simulated Elwynn Forest behind the login screen — 5095 tiles and tens of
/// thousands of entities, anchored on a hardcoded [`crate::SPAWN_XY`], for a character who might
/// be on another continent. 0540 gated the *camera* (the world was at least not drawn); 0772
/// measured what was left — 5.5 cpu ms/frame of streaming, visibility sweeps, palette uploads and
/// render-world prepare under a static background image, and a 638 ms worst frame — and left the
/// load itself open as a director's call. The call: **don't load the world until we have to.**
///
/// Consumers gate on this, not on `in_state(InWorld)` written out by hand, so "what counts as
/// having a world" has one definition to change. The camera's own gate
/// ([`crate::player::setup::gate_world_camera`]) is deliberately *wider* — it also renders under
/// an opaque loading screen, which is what compiles the world's pipelines before the first visible
/// frame.
pub(crate) fn world_is_live(state: Res<State<ClientState>>) -> bool {
    *state.get() == ClientState::InWorld
}

/// Ordered stages of the per-frame world-transition pipeline (configured `.chain()`ed in `Update`).
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WorldStage {
    /// Drain the world stream into ECS entities + surface teleport/worldport messages.
    Net,
    /// Player input + teleport/worldport snap.
    Input,
    /// Terrain residency: tile swap/stream around the (possibly just-snapped) position.
    Stream,
    /// Loading-screen present: cover the world while the surrounding tiles aren't resident.
    Present,
}

/// Installs the [`WorldStage`] ordering. Add before the subsystem plugins (order doesn't matter — set
/// configuration is resolved at schedule build).
pub(crate) struct SchedulePlugin;

impl Plugin for SchedulePlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            Update,
            (
                WorldStage::Net,
                WorldStage::Input,
                WorldStage::Stream,
                WorldStage::Present,
            )
                .chain(),
        );
    }
}
