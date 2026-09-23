//! **The spell** — the reference's `Spell_C` owns the cast lifecycle, the cooldowns, the
//! channels, the spell-modifier tables and its 22 packet registrations in one translation unit;
//! benilla had no such owner (2265 §A7). This module is that owner: the in-flight slot and the
//! local cancel ([`inflight`]), the cooldown store ([`cooldowns`]), the talent modifier tables
//! ([`mods`]) and the packet handlers ([`net`]) — decisions 2324 and 2328.
//!
//! What it delegates, as the reference delegates it: the wire to `net`, the units to the object
//! layer, the spell table to `ui_action::Spells` (the DBC catalog every face reads), the error
//! text and the VM to the `ui_*` feeds, the visuals and animation to `creature_anim`. The one
//! cast path — the `TryCast` ladder — is still `ui_action`'s; it writes this module's state
//! through `crate::spell` like every other caller (2328 §Open).

use bevy::prelude::*;

use benilla_world::schedule::WorldStage;

use crate::char_select::ClientState;
use crate::ui_unit::UnitFeed;

pub(crate) mod cooldowns;
mod inflight;
mod mods;
pub(crate) mod net;

pub(crate) use cooldowns::Cooldowns;
pub(crate) use inflight::{
    inflight, ActiveChannel, AutoRepeatActive, ChainCasts, LocalMoveStart, PendingCast,
    QueuedMeleeSpell, SPELL_INTERRUPT_MOVEMENT,
};
pub(crate) use mods::{SpellModifiers, OP_COST};

/// The local cancel's slot in the frame: a feed that drains the edges it pushes (the cast bar)
/// orders itself `.after(LocalCancel)` so a move-edge's `Stop` reaches the VM the same frame it
/// happened — the `chain()` `ui_cast` held while both were its (2328).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct LocalCancel;

pub(crate) struct SpellPlugin;

impl Plugin for SpellPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PendingCast>()
            .init_resource::<QueuedMeleeSpell>()
            .init_resource::<ActiveChannel>()
            .init_resource::<LocalMoveStart>()
            .init_resource::<AutoRepeatActive>()
            .init_resource::<ChainCasts>()
            .init_resource::<Cooldowns>()
            .init_resource::<SpellModifiers>()
            .add_systems(
                Update,
                (
                    inflight::local_self_cancel
                        .in_set(UnitFeed)
                        .in_set(LocalCancel),
                    // After the net stage that merges the avatar's descriptor, and before the
                    // feeds that read a cost through it — so the family is this frame's, never
                    // last frame's, on the frame the avatar first resolves.
                    mods::track_class_family
                        .after(WorldStage::Net)
                        .before(UnitFeed),
                ),
            )
            .add_systems(OnEnter(ClientState::InWorld), mods::clear_on_world_enter);
        net::register(app);
    }
}
