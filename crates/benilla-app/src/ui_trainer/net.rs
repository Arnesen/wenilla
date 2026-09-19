//! The trainer window's packet handlers (decision 0237; in the net handler table since 2318,
//! moved out of the drain's npc arm file) — the [`TrainerOpen`] session and the
//! [`TrainerErrors`] line queue the trainer feed ([`super`]) reads.

use benilla_protocol::messages::TrainerSpell;
use benilla_protocol::{SessionEvent, SessionEventKind};
use bevy::prelude::*;

use super::{TrainerErrors, TrainerOpen};
use crate::net::{ClientCommand, NetCommands, NetHandlerApp};

/// Register the trainer handlers — called from [`super::UiTrainerPlugin`]. One per kind, plus
/// the session-end listener.
pub(super) fn register(app: &mut App) {
    use SessionEventKind as K;
    app.net_handler(K::TrainerList, on_list)
        .net_handler(K::TrainerBuySucceeded, on_buy_succeeded)
        .net_handler(K::TrainerBuyFailed, on_buy_failed)
        .net_handler(K::Disconnected, on_session_end);
}

fn on_list(In(ev): In<SessionEvent>, mut trainer_open: ResMut<TrainerOpen>) {
    if let SessionEvent::TrainerList {
        trainer,
        trainer_type,
        services,
        greeting,
    } = ev
    {
        trainer_list(trainer, trainer_type, services, greeting, &mut trainer_open);
    }
}

fn on_buy_succeeded(
    In(ev): In<SessionEvent>,
    mut trainer_open: ResMut<TrainerOpen>,
    commands: Res<NetCommands>,
) {
    if let SessionEvent::TrainerBuySucceeded { trainer, spell_id } = ev {
        trainer_buy_succeeded(trainer, spell_id, &mut trainer_open, &commands);
    }
}

fn on_buy_failed(In(ev): In<SessionEvent>, mut errors: ResMut<TrainerErrors>) {
    if let SessionEvent::TrainerBuyFailed { error, .. } = ev {
        trainer_buy_failed(error, &mut errors);
    }
}

/// An open trainer window dies with the socket. A listener on the session end
/// ([`crate::net::handlers::BROADCAST`]).
fn on_session_end(In(_): In<SessionEvent>, mut trainer_open: ResMut<TrainerOpen>) {
    trainer_open.clear_session();
}

/// A trainer's service list (`SMSG_TRAINER_LIST`): fill the [`TrainerOpen`] the trainer feed
/// ([`super`]) reads.
fn trainer_list(
    trainer: u64,
    trainer_type: u32,
    services: Vec<TrainerSpell>,
    greeting: String,
    trainer_open: &mut TrainerOpen,
) {
    debug!(
        "net: trainer {trainer:#x} (type {trainer_type}) listed {} services",
        services.len()
    );
    trainer_open.open(trainer, trainer_type, services, greeting);
}

/// A trainer taught a service — confirmation only; the spell already landed via
/// `SMSG_LEARNED_SPELL`. Re-request the list to repaint the bought row green→gray (and unlock
/// any next rank); the server never auto-resends on a buy (VERIFIED vmangos
/// `HandleTrainerBuySpellOpcode`). Guard on the window still being open for this trainer so a
/// late reply for a closed/switched window doesn't re-open it.
///
/// This re-request is **benilla's**, not the reference's — the reference repaints a purchase by
/// re-deriving every service's state client-side (`0x4d7d40`, decision 1128 §4.2) and receives no
/// packet at all. So the answering list is marked as a *refresh* ([`TrainerOpen::refresh_pending`]):
/// it repaints the open window, and must not carry the reference's per-packet filter/collapse reset,
/// which is what dropped the player's "Available only" choice the moment they learned a spell
/// (B256).
fn trainer_buy_succeeded(
    trainer: u64,
    spell_id: u32,
    trainer_open: &mut TrainerOpen,
    net_commands: &NetCommands,
) {
    debug!("net: trainer {trainer:#x} taught spell {spell_id} — re-listing");
    if trainer_open.trainer == Some(trainer) {
        trainer_open.refresh_pending = true;
        let _ = net_commands.0.send(ClientCommand::TrainerList { trainer });
    }
}

/// A trainer refused a purchase — the trainer window's error line.
fn trainer_buy_failed(error: u32, errors: &mut TrainerErrors) {
    debug!("net: trainer buy failed (code {error})");
    errors.0.push(error);
}
