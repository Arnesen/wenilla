//! NPC-interaction arm bodies for [`super::apply_net_updates`]'s dispatch match — the gossip
//! window (decision 0081 phase 3), the vendor window (phase 4), the trainer window (decision
//! 0237), and the taxi-map session (decision 0484 phase 1). Each `pub(super)` fn here is exactly
//! one arm's body; the match at the call site stays the dispatcher, one call per arm.

use benilla_protocol::messages::{GossipOption, NpcTextBlock, TaxiMask, TrainerSpell, VendorItem};
use bevy::prelude::*;

use crate::ui_bank::{BankErrors, BankOpen};
use crate::ui_gossip::GossipState;
use crate::ui_merchant::{MerchantErrors, MerchantOpen, MerchantRefusal};
use crate::ui_quest::QuestGiver;
use crate::ui_taxi::TaxiState;
use crate::ui_trainer::{TrainerErrors, TrainerOpen};

use super::super::{ClientCommand, GuidIndex, NetCommands, ObjectStore};

/// A streamed unit's gender (`UNIT_FIELD_BYTES_0` byte 2) by guid — the gossip greeting's column
/// selector (wow-re `gossip-npctext-law.md`: tested `== 1` for female, so genderless `2` reads as
/// male). `0` when the guid isn't streamed in or carries no descriptor yet, which is the same
/// column the reference takes for a gossip target that isn't a unit at all.
fn npc_gender(guid: u64, index: &GuidIndex, stores: &Query<&mut ObjectStore>) -> u8 {
    index
        .0
        .get(&guid)
        .and_then(|&e| stores.get(e).ok())
        .and_then(|s| s.0.unit_gender())
        .unwrap_or(0)
}

/// A gossip menu opened (`SMSG_GOSSIP_MESSAGE`): fill the [`GossipState`] the gossip feed
/// (`crate::ui_gossip`) reads. On a menu opening, auto-send the ask-once `CMSG_NPC_TEXT_QUERY`
/// for its text record (served from the cache on a revisit); the record lands via [`npc_greeting`];
/// [`gossip_complete`] closes it.
///
/// The greeting is **drawn here**, not at the packet — this is the reference's own moment for it
/// (`0x4e2010`), and the draw needs both this NPC's gender ([`npc_gender`]) and a fresh roll.
#[allow(clippy::too_many_arguments)]
pub(super) fn gossip_menu(
    npc: u64,
    text_id: u32,
    options: Vec<GossipOption>,
    quests: Vec<(u32, u32, String)>,
    gossip: &mut GossipState,
    net_commands: &NetCommands,
    index: &GuidIndex,
    stores: &Query<&mut ObjectStore>,
) {
    let npc_gender = npc_gender(npc, index, stores);
    debug!(
        "net: gossip menu on {npc:#x} — {} options, {} quests",
        options.len(),
        quests.len()
    );
    gossip.npc = Some(npc);
    gossip.text_id = text_id;
    gossip.options = options;
    gossip.quests = quests;
    match gossip.cached_record(text_id).is_some() {
        true => gossip.greeting = gossip.draw_greeting(text_id, npc_gender),
        false => {
            gossip.greeting = None;
            let _ = net_commands
                .0
                .send(ClientCommand::NpcTextQuery { text_id, guid: npc });
        }
    }
}

/// The NPC-text answer (`SMSG_NPC_TEXT_UPDATE`) — seed the cache with the whole record, and draw
/// the greeting only for the OPEN menu (a late answer for a menu we already closed just seeds the
/// cache; the next open draws its own line).
///
/// The record answers a query we sent for the OPEN menu, so its NPC is the one whose gender picks
/// the column (decision 0081's ask-once flow).
pub(super) fn npc_greeting(
    text_id: u32,
    blocks: Vec<NpcTextBlock>,
    gossip: &mut GossipState,
    index: &GuidIndex,
    stores: &Query<&mut ObjectStore>,
) {
    let npc_gender = gossip.npc.map_or(0, |npc| npc_gender(npc, index, stores));
    gossip.remember_record(text_id, blocks);
    if gossip.npc.is_some() && gossip.text_id == text_id {
        gossip.greeting = gossip.draw_greeting(text_id, npc_gender);
    }
}

/// `SMSG_GOSSIP_COMPLETE` ends the whole interaction (e.g. right after a quest accept), so the
/// quest window closes with the gossip menu (decision 0088).
pub(super) fn gossip_complete(gossip: &mut GossipState, quest: &mut QuestGiver) {
    debug!("net: gossip complete — closing the menu");
    gossip.clear();
    quest.clear();
}

/// A vendor's stock (`SMSG_LIST_INVENTORY`): fill the [`MerchantOpen`] the merchant feed
/// (`crate::ui_merchant`) reads. A successful buy updates the stock display via
/// [`vendor_buy_result`] (the item itself lands via item-create); a successful sell is silent
/// (never a packet here — only the error path is).
pub(super) fn vendor_inventory(vendor: u64, items: Vec<VendorItem>, merchant: &mut MerchantOpen) {
    debug!("net: vendor {vendor:#x} listed {} items", items.len());
    merchant.open(vendor, items);
}

/// The bank opened (`SMSG_SHOW_BANK`): point the [`BankOpen`] session the bank feed
/// (`crate::ui_bank`) reads at the banker. Sent for our own `CMSG_BANKER_ACTIVATE` *and*
/// volunteered by the server for the gossip menu's bank option (`GOSSIP_OPTION_BANKER` →
/// `SendShowBank`, decision 0604) — so this never assumes we asked. The vault's contents are
/// descriptor fields already streamed; the window renders from local state.
///
/// Opening the bank ends any open gossip interaction (the [`gossip_complete`] clear): vmangos
/// sends no `SMSG_GOSSIP_COMPLETE` for the gossip menu's bank option (VERIFIED
/// `Player::OnGossipSelect` — BANKER only calls `SendShowBank`), and the panel slots alone can't
/// close the menu — the bank's `pushable = 6` sends it to the *center* slot beside a pushable-0
/// gossip instead of replacing it, so the menu would linger beside the vault
/// (director-observed). The real client ends the old NPC interaction when the new one starts
/// (INFERRED — the exact C++ `GOSSIP_CLOSED` fire isn't RE'd; the observable is vanilla's: the
/// menu is gone once the vault is up).
pub(super) fn show_bank(
    banker: u64,
    bank: &mut BankOpen,
    gossip: &mut GossipState,
    quest: &mut QuestGiver,
) {
    debug!("net: bank opened at {banker:#x}");
    if gossip.npc.is_some() {
        gossip_complete(gossip, quest);
    }
    bank.open(banker);
}

/// A bank-slot purchase refusal (`SMSG_BUY_BANK_SLOT_RESULT` — vmangos sends it only on failure;
/// success is the PLAYER_BYTES_2 delta): queue it for the bank feed's red error line.
pub(super) fn bank_buy_slot_result(result: u32, errors: &mut BankErrors) {
    debug!("net: bank slot purchase refused (code {result})");
    errors.0.push(result);
}

/// A trainer's service list (`SMSG_TRAINER_LIST`): fill the [`TrainerOpen`] the trainer feed
/// (`crate::ui_trainer`) reads.
pub(super) fn trainer_list(
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
pub(super) fn trainer_buy_succeeded(
    trainer: u64,
    spell_id: u32,
    trainer_open: &TrainerOpen,
    net_commands: &NetCommands,
) {
    debug!("net: trainer {trainer:#x} taught spell {spell_id} — re-listing");
    if trainer_open.trainer == Some(trainer) {
        let _ = net_commands.0.send(ClientCommand::TrainerList { trainer });
    }
}

/// A trainer refused a purchase — the trainer window's error line.
pub(super) fn trainer_buy_failed(error: u32, errors: &mut TrainerErrors) {
    debug!("net: trainer buy failed (code {error})");
    errors.0.push(error);
}

/// A purchase updated the vendor's stock (`SMSG_BUY_ITEM`). Only touch stock for the open
/// vendor (a late answer for a closed window is stale).
pub(super) fn vendor_buy_result(
    vendor: u64,
    slot: u32,
    new_count: u32,
    merchant: &mut MerchantOpen,
) {
    if merchant.vendor == Some(vendor) {
        merchant.update_stock(slot, new_count);
    }
}

/// A purchase was refused (`SMSG_BUY_FAILED`) — the merchant window's error line.
pub(super) fn vendor_buy_failed(reason: u8, errors: &mut MerchantErrors) {
    debug!("net: buy failed (reason {reason})");
    errors.0.push(MerchantRefusal::Buy(reason));
}

/// A sell was refused (`SMSG_SELL_ITEM`'s error path) — the merchant window's error line.
pub(super) fn vendor_sell_failed(reason: u8, errors: &mut MerchantErrors) {
    debug!("net: sell failed (reason {reason})");
    errors.0.push(MerchantRefusal::Sell(reason));
}

/// The taxi map (`SMSG_SHOWTAXINODES`): fill the [`TaxiState`] phase 2's feed will read — the
/// [`trainer_list`] shape.
pub(super) fn taxi_nodes_shown(
    flightmaster: u64,
    nearest_node: u32,
    known_mask: TaxiMask,
    taxi: &mut TaxiState,
) {
    debug!("net: taxi map on {flightmaster:#x} — nearest node {nearest_node}");
    taxi.open(flightmaster, nearest_node, known_mask);
}

/// A taxi node's known status (`SMSG_TAXINODE_STATUS`) — answers `CMSG_TAXINODE_STATUS_QUERY`
/// ([`crate::ui_taxi`]'s per-flight-master ask), and also rides the first-visit learn pair
/// alongside `SMSG_NEW_TAXI_PATH` ([`taxi_new_path`] carries that signal into `discovered`; the
/// paired `known = true` here is what clears the icon live on a learn). Upserts
/// [`crate::ui_taxi::FlightMasterStatus`] on the flight master: `known = false` shows the green
/// `TalkToMeGreen` overhead icon — the client's `0x5ecdd0` handler → `0x607480` marker swap
/// (resource table `0xc4d9d8` index 4; the 0497 §5). The client also gates the reply on the
/// unit's NPC_FLAGS bit 3 — moot here: only flight masters are ever queried or answered.
pub(super) fn taxi_node_status(
    guid: u64,
    known: bool,
    commands: &mut Commands,
    index: &crate::net::GuidIndex,
) {
    debug!("net: taxi node status — {guid:#x} known={known}");
    if let Some(&e) = index.0.get(&guid) {
        commands
            .entity(e)
            .insert(crate::ui_taxi::FlightMasterStatus { known });
    }
}

/// The activate verdict (`SMSG_ACTIVATETAXIREPLY`) — staged for phase 2's window to surface as
/// the red error line and clear (the trainer/merchant error-line pattern, folded into
/// `TaxiState.reply` since there's no other consumer yet).
pub(super) fn taxi_activate_reply(code: u32, taxi: &mut TaxiState) {
    debug!("net: activate taxi reply — code {code}");
    taxi.reply = Some(code);
}

/// A first-visit "learn" landed (`SMSG_NEW_TAXI_PATH`, empty body) — vmangos's only send site
/// (`SendLearnNewTaxiNode`) always pairs this with a `SMSG_TAXINODE_STATUS(known=true)` for the
/// same flight master, so the empty body alone is the discovery signal; phase 2's presentation
/// reads the flag.
pub(super) fn taxi_new_path(taxi: &mut TaxiState) {
    debug!("net: taxi — first-visit node learned (SMSG_NEW_TAXI_PATH)");
    taxi.discovered = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SMSG_SHOW_BANK` ends an open gossip interaction (the gossip menu's bank option arrives
    /// with no `SMSG_GOSSIP_COMPLETE` — `show_bank`'s doc): the gossip/quest sessions clear and
    /// the bank session opens. A direct right-click (no gossip open) just opens the bank.
    #[test]
    fn show_bank_ends_the_gossip_interaction() {
        let mut bank = BankOpen::default();
        let mut gossip = GossipState::default();
        let mut quest = QuestGiver::default();
        gossip.npc = Some(0x42);

        show_bank(0x42, &mut bank, &mut gossip, &mut quest);
        assert_eq!(bank.banker, Some(0x42));
        assert_eq!(gossip.npc, None, "the gossip session ended with the menu");

        // No gossip open: a plain open, nothing else touched.
        bank.clear();
        show_bank(0x43, &mut bank, &mut gossip, &mut quest);
        assert_eq!(bank.banker, Some(0x43));
    }
}
