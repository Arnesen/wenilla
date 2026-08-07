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

/// Installs the [`WorldStage`] ordering + the boot-order correction below. Add before the subsystem
/// plugins (order doesn't matter — set configuration is resolved at schedule build), but *after*
/// `DefaultPlugins`, which is where `StatesPlugin` seeds the order this fixes.
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
        move_initial_state_transition_after_startup(app);
    }
}

/// **The initial state's `OnEnter` runs AFTER the app is built, not before it.**
///
/// `bevy_state` seeds the startup order with (`bevy_state-0.18.1/src/app.rs:308-309`):
///
/// ```ignore
/// schedule.insert_after(PreUpdate, StateTransition);
/// schedule.insert_startup_before(PreStartup, StateTransition);   // ← this one
/// ```
///
/// so the one-shot transition that enters the *initial* state runs before `PreStartup` — ahead of
/// every `Startup` system, i.e. ahead of the patch chain ([`crate::assets::AssetSet::Open`]), the
/// Lua VM, the mixer and every catalog. An `OnEnter` for the state the app boots into therefore
/// sees an empty world, which is not what "on entering this state" means anywhere else.
///
/// Three subsystems each hit this independently and each wrote the same local workaround — spawn
/// from a per-frame `Update` poll instead of the state edge:
/// [`crate::login::screen::materialize_screen`], [`crate::char_select::screen::enter_select`] and
/// `sound::glue::start_glue_music`. Their comments name it "the boot-order trap". It is one cause
/// with three patches, which is the shape §3 says to go after rather than add a fourth to.
///
/// The fix is to move that single startup entry to the end: the initial transition now runs after
/// `PostStartup`, so `OnEnter(<initial>)` observes a fully built app exactly like every later
/// transition does. Safe here because **no `Startup` system in the workspace writes `NextState`**
/// (every writer is an `Update` system), so nothing can be waiting on the transition to be applied
/// mid-startup; and `insert_state`/`init_state` insert the `State<S>` resource directly at plugin
/// build, so a `Startup` system reading `Res<State<..>>` is unaffected either way.
///
/// This does not retire the three workarounds — two of them also re-spawn when *async* client art
/// lands, which is a different clock. It removes the boot-order reason to write a fourth.
fn move_initial_state_transition_after_startup(app: &mut App) {
    use bevy::app::MainScheduleOrder;
    use bevy::state::state::StateTransition;

    let mut order = app.world_mut().resource_mut::<MainScheduleOrder>();
    let before = order.startup_labels.len();
    order.startup_labels.retain(|l| !(**l).eq(&StateTransition));
    // Only re-seat what was actually there — if bevy_state ever stops seeding it, do not invent it.
    if order.startup_labels.len() < before {
        order.insert_startup_after(PostStartup, StateTransition);
    }
}

#[cfg(test)]
mod tests {
    use bevy::prelude::*;
    use bevy::state::app::StatesPlugin;

    use super::SchedulePlugin;
    use crate::char_select::ClientState;

    #[derive(Resource)]
    struct BuiltAtStartup;

    /// What [`super::move_initial_state_transition_after_startup`] buys: an `OnEnter` for the state
    /// the app BOOTS INTO sees the resources `Startup` built. Without the re-seat this is false —
    /// the initial transition runs ahead of `PreStartup` — and that is the boot-order trap three
    /// subsystems each worked around by hand.
    #[test]
    fn the_initial_states_on_enter_sees_what_startup_built() {
        #[derive(Resource, Default)]
        struct Saw(Option<bool>);

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin))
            .add_plugins(SchedulePlugin)
            .insert_state(ClientState::InWorld)
            .init_resource::<Saw>()
            .add_systems(Startup, |mut c: Commands| c.insert_resource(BuiltAtStartup))
            .add_systems(
                OnEnter(ClientState::InWorld),
                |built: Option<Res<BuiltAtStartup>>, mut saw: ResMut<Saw>| {
                    saw.0 = Some(built.is_some());
                },
            );

        app.update();

        assert_eq!(
            app.world().resource::<Saw>().0,
            Some(true),
            "the initial state's OnEnter ran before Startup — the boot-order trap is back"
        );
    }
}
