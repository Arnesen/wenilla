//! **The spell** — the reference's `Spell_C` owns the cast lifecycle, the cooldowns, the
//! channels, the spell table and its 22 packet registrations in one translation unit; benilla
//! had no such owner (2265 §A7). This module is where that owner lives. Since decision 2324 it
//! holds the packet handlers ([`net`]); the lifecycle state they fold into is still spread over
//! `ui_action`, `ui_cast` and `cooldowns`, and moving it here is A7's remaining half.

use bevy::prelude::*;

pub(crate) mod net;

/// Registration only — the state is still the feeds' (see the module doc).
pub(crate) struct SpellPlugin;

impl Plugin for SpellPlugin {
    fn build(&self, app: &mut App) {
        net::register(app);
    }
}
