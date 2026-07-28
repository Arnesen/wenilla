//! World-feed arm bodies for [`super::apply_net_updates`]'s dispatch match — what the server
//! pushes about the *world* rather than about an entity's own state: the three ambient audio
//! triggers, the weather change, and the world-state table the UI's `$<n>w` tokens read. Each
//! `pub(super)` fn here is exactly one arm's body; the match at the call site stays the dispatcher,
//! one call per arm.

use bevy::prelude::*;

use crate::world_state::WorldStates;

use super::super::{GuidIndex, ServerSoundKind, ServerSoundMessage, WeatherMessage};

/// `SMSG_PLAY_SOUND` — a 2D (non-positional) one-shot.
pub(super) fn play_sound(sound_id: u32, out: &mut MessageWriter<ServerSoundMessage>) {
    out.write(ServerSoundMessage {
        kind: ServerSoundKind::Sound2d,
        sound_id,
        source: None,
    });
}

/// `SMSG_PLAY_MUSIC` — the zone/event music track.
pub(super) fn play_music(music_id: u32, out: &mut MessageWriter<ServerSoundMessage>) {
    out.write(ServerSoundMessage {
        kind: ServerSoundKind::Music,
        sound_id: music_id,
        source: None,
    });
}

/// `SMSG_PLAY_OBJECT_SOUND` — a one-shot anchored to a streamed object (silent while its guid
/// isn't streamed in: the mixer has nowhere to place it).
pub(super) fn play_object_sound(
    sound_id: u32,
    guid: u64,
    index: &GuidIndex,
    out: &mut MessageWriter<ServerSoundMessage>,
) {
    out.write(ServerSoundMessage {
        kind: ServerSoundKind::ObjectSound,
        sound_id,
        source: index.0.get(&guid).copied(),
    });
}

/// `SMSG_WEATHER` — the zone's weather change (`instant` on zone entry, a ramp otherwise).
pub(super) fn weather(
    weather_type: u32,
    grade: f32,
    sound_id: u32,
    instant: bool,
    out: &mut MessageWriter<WeatherMessage>,
) {
    out.write(WeatherMessage {
        weather_type,
        grade,
        sound_id,
        instant,
    });
}

/// `SMSG_INIT_WORLD_STATES` / `SMSG_UPDATE_WORLD_STATE` — both wires funnel into the one setter,
/// as the reference's own handler does. An init does NOT clear first: what its `(map, zone)` dwords
/// drive is unrecorded, so we log the scope rather than act on it (rationale on
/// [`crate::world_state`]).
pub(super) fn world_states(
    scope: Option<(u32, u32)>,
    states: Vec<(u32, u32)>,
    world_states: &mut WorldStates,
) {
    if let Some((map, zone)) = scope {
        debug!(
            "world states: map {map} zone {zone}, {} entries",
            states.len()
        );
    }
    world_states.write(&states);
}
