//! Loot-window + item-template/inventory-failure arm bodies for [`super::apply_net_updates`]'s
//! dispatch match — one of the largest arm families, split out on its own (decision 0084's loot
//! feed, plus the item-template ask-once cache and the equip-refusal UI line). Each `pub(super)` fn
//! here is exactly one arm's body; the match at the call site stays the dispatcher, one call per
//! arm.

use benilla_protocol::messages::{
    ItemPushResult, LootAllPassed, LootItem, LootRoll, LootRollWon, LootStartRoll,
};
use benilla_protocol::ItemInfo;
use bevy::prelude::*;

use super::super::SelfGuid;
use crate::items::Items;
use crate::pending_item_ops::{LockClearedByFailure, PendingItemOps};
use crate::ui_action::{UiError, UiErrorKeys};
use crate::ui_items::{EquipError, EquipErrors};
use crate::ui_loot::{LootErrors, LootLatch, LootState};
use crate::ui_loot_roll::LootRolls;

/// A loot window opened (`SMSG_LOOT_RESPONSE`'s normal shape), answering our `CMSG_LOOT`.
pub(super) fn loot_response(
    guid: u64,
    loot_type: u8,
    gold: u32,
    items: Vec<LootItem>,
    loot: &mut LootState,
) {
    // The loot window opens (decision 0084): fill LootState from the wire; the feed
    // ([`crate::ui_loot`]) resolves rows + fires LOOT_OPENED next frame. `loot_type` rides along
    // for `IsFishingLoot()` (decision 1086).
    debug!(
        "net: loot response {guid:#x} type {loot_type} gold {gold} {} item(s)",
        items.len()
    );
    loot.open(guid, loot_type, gold, items);
}

/// A fishing verdict with no loot window (`SMSG_FISH_ESCAPED` / `SMSG_FISH_NOT_HOOKED`, both
/// empty-bodied; decision 1086): the **yellow** toast by GlobalStrings key — `ERR_FISH_ESCAPED`
/// ("Your fish got away!") when the skill roll failed on the click, `ERR_FISH_NOT_HOOKED`
/// ("No fish are hooked.") when the bobber expired or was clicked before the splash. Yellow, not
/// red: the reference handlers (`0x5e3fc5`/`0x5e3fe2` → `DisplayError` ids `0x13e`/`0x13f`) are
/// **type-1** registry entries, which fire `UI_INFO_MESSAGE` — byte-verified in wow-re
/// `fish-msg-handlers.md`, correcting 1086's shipped guess (the fold-back record).
pub(super) fn fish_verdict(escaped: bool, errors: &mut UiErrorKeys) {
    let key = if escaped {
        "ERR_FISH_ESCAPED"
    } else {
        "ERR_FISH_NOT_HOOKED"
    };
    debug!("net: fishing verdict {key}");
    errors.0.push(UiError::info_key(key));
}

/// The server refused to open the loot window (`SMSG_LOOT_RESPONSE`'s error shape — didn't kill
/// it, too far, not standing, …). The refusal also ends the client-predicted kneel: the latch
/// armed at the `CMSG_LOOT` send drops (guid-matched), or the character would kneel forever at a
/// corpse whose window never opened (decision 0515).
pub(super) fn loot_error(
    guid: u64,
    error: u8,
    loot_errors: &mut LootErrors,
    latch: &mut LootLatch,
) {
    // The server refused to open the window → the red UI error line (equip-error path).
    debug!("net: loot error {error} on {guid:#x}");
    loot_errors.0.push(error);
    latch.clear_for(guid);
}

/// One loot-window row was taken, by anyone (`SMSG_LOOT_REMOVED`) — the UI clears that row.
pub(super) fn loot_removed(slot: u8, loot: &mut LootState) {
    // A row was taken (by anyone): drop it; the feed repaints via LOOT_UPDATE.
    debug!("net: loot slot {slot} removed");
    loot.remove_slot(slot);
}

/// Our share of the loot's coin pile (`SMSG_LOOT_MONEY_NOTIFY`), answering our `CMSG_LOOT_MONEY`.
pub(super) fn loot_money_notify(amount: u32) {
    // Our share of the coin pile — informational; the coin row drops on CLEAR_MONEY and
    // the purse rides the ordinary COINAGE flush. Solo looting rarely sends this.
    debug!("net: loot money {amount}");
}

/// The coin line disappears for every current looter (`SMSG_LOOT_CLEAR_MONEY`).
pub(super) fn loot_clear_money(loot: &mut LootState) {
    // The coin line disappears for everyone → drop the coin row.
    debug!("net: loot coin line cleared");
    loot.clear_money();
}

/// The loot window closes (`SMSG_LOOT_RELEASE_RESPONSE`), answering our `CMSG_LOOT_RELEASE`.
/// Idempotent — a client-side close already cleared. The latch clear is **guid-matched**: under
/// the corpse-switch race (loot B requested while A was open) the old window's release response
/// must not drop the latch the new request just armed (decision 0515).
pub(super) fn loot_release_response(guid: u64, loot: &mut LootState, latch: &mut LootLatch) {
    debug!("net: loot released {guid:#x}");
    loot.clear();
    latch.clear_for(guid);
}

/// A group roll opened on one drop (`SMSG_LOOT_START_ROLL`) — a `GroupLootFrame` goes up with
/// Need/Greed/Pass and the countdown bar (decision 0591).
pub(super) fn loot_start_roll(p: LootStartRoll, rolls: &mut LootRolls) {
    debug!(
        "net: loot roll opened on item {} ({:#x} slot {}), {} ms",
        p.item_id, p.looted_target, p.item_slot, p.countdown_ms
    );
    rolls.start(p);
}

/// One roller's vote or dice result (`SMSG_LOOT_ROLL`) — the chat announcement line. The
/// `(roll_number, roll_type)` pair is overloaded; `LootRoll::is_dice`/`vote` disentangle it.
pub(super) fn loot_roll(p: LootRoll, rolls: &mut LootRolls) {
    debug!(
        "net: loot roll announce — roller {:#x} number {} type {}",
        p.roller, p.roll_number, p.roll_type
    );
    rolls.announce(p);
}

/// A group roll resolved (`SMSG_LOOT_ROLL_WON`) — the "won" line, and that roll's frame closes.
pub(super) fn loot_roll_won(p: LootRollWon, rolls: &mut LootRolls) {
    debug!(
        "net: loot roll won by {:#x} with {} (type {})",
        p.winner, p.roll_number, p.roll_type
    );
    rolls.won(p);
}

/// Everyone passed (`SMSG_LOOT_ALL_PASSED`) — the frame closes and the item returns to the corpse
/// as an ordinary lootable row.
pub(super) fn loot_all_passed(p: LootAllPassed, rolls: &mut LootRolls) {
    debug!("net: loot roll — everyone passed on item {}", p.item_id);
    rolls.all_passed(p);
}

/// Is this push **ours**? The outermost gate in `CGGameUI::OnItemPush` (1.12.1 `WoW.exe` 5875,
/// `0x491a60`): `0x491b56`/`0x491b61` compare the packet guid against the active player's and fork,
/// and the self arm is the only one that prints "You receive …" *or* animates a bag button. A group
/// member's push takes the `0x491d9f` arm and prints `LOOT_ITEM` ("%s receives loot: %s.") with
/// *their* name — a line we do not build yet, so a foreign push is dropped rather than mislabelled
/// ours.
///
/// The wire's **`showInChat`** used to be folded in here, which read correctly while the chat line
/// was this packet's only output. It is a *later*, narrower gate — `0x491bf3` (self) / `0x491db1`
/// (other) skip the chat formatter alone, after the `ITEM_PUSH` fire at `0x491be8` — so it now
/// rides into [`LootState::push_receive`] as `PendingReceive::in_chat` and silences the line
/// without touching the animation (decision 0887). vmangos always sends 1, so it is inert against
/// our server either way; it is the client's own gate, kept where the client keeps it.
fn is_our_push(p: &ItemPushResult, self_guid: &SelfGuid) -> bool {
    self_guid.0 == Some(p.player_guid)
}

/// An item landed in our bags — looted or received from an NPC (`SMSG_ITEM_PUSH_RESULT`); drives
/// the "You receive loot: …" chat line (gated by [`prints_receive_line`]) **and** the bag-bar drop
/// animation (decision 0887), which is NOT so gated — hence the whole packet going through, and the
/// self check being the only thing that can stop a push here. The reference's
/// `CGGameUI::OnItemPush 0x491a60` emits both from this one packet: it returns early only on a guid
/// mismatch, and tests `showInChat` further down, after the `ITEM_PUSH` fire.
pub(super) fn item_push_result(p: ItemPushResult, self_guid: &SelfGuid, loot: &mut LootState) {
    // Solo loot never gets a MONEY_NOTIFY from this server (vmangos comments it out;
    // the purse rides the ordinary COINAGE field flush) — so this push line is the
    // one reliable "it landed" signal, surfaced as the "You receive loot/item" line.
    debug!(
        "net: item push {} x{} → bag {} slot {:#x}",
        p.item_entry, p.count, p.bag_slot, p.item_slot
    );
    if !is_our_push(&p, self_guid) {
        return;
    }
    loot.push_receive(&p);
}

/// An item template's display head (`SMSG_ITEM_QUERY_SINGLE_RESPONSE`, answering our
/// `CMSG_ITEM_QUERY_SINGLE`).
pub(super) fn item_template(entry: u32, info: Option<ItemInfo>, items: &mut Items) {
    // Fill the ask-once template cache (decisions 0068/0072 — one cache serves held-
    // item resolution and the container layer); a server miss records `None` so the
    // entry is never re-asked. Consumers re-read it next frame.
    debug!("net: item template {entry} → {info:?}");
    items.insert_template(entry, info);
}

/// The server refused an inventory operation (`SMSG_INVENTORY_CHANGE_FAILURE` — equip level,
/// proficiency, bag full, …): the UI error line's inventory vocabulary, the equip twin of the
/// cast-result failure path. Also the pending-lock's failure-driven clear (decision 0216 §4 /
/// 0218 §3): every arrival here already has reason ≠ 0 (reason 0 is filtered before this event
/// exists at all — `benilla_protocol::events`'s `if reason != 0` guard), so it always tries a
/// [`PendingItemOps::clear_by_failure`]. This site has no `UiScript` to fire `ITEM_LOCK_CHANGED`
/// through, so the transitioned slots queue in [`LockClearedByFailure`] for the container feed
/// (`ui_items::feed::feed_containers`) to drain and fire next time it runs.
pub(super) fn inventory_failure(
    reason: u8,
    required_level: Option<u32>,
    item_guid: u64,
    bag_slot: u8,
    equip_errors: &mut EquipErrors,
    pending: &mut PendingItemOps,
    lock_cleared: &mut LockClearedByFailure,
) {
    debug!("net: inventory failure {reason:#04x} (item {item_guid:#x}, bag slot {bag_slot})");
    equip_errors.0.push(EquipError {
        reason,
        required_level,
        bag_slot,
    });
    lock_cleared.0.extend(pending.clear_by_failure(item_guid));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both fish-verdict keys queue as **yellow** (type-1 / `UI_INFO_MESSAGE`) entries — the
    /// byte-verified arm, wow-re `fish-msg-handlers.md` — and both resolve to the exact 1.12
    /// strings in the shipped `GlobalStrings.lua` (the equip-error test's runtime pattern —
    /// a typo'd key would silently swallow the toast). Skips without client data.
    #[test]
    fn fish_verdict_keys_resolve_in_the_real_global_strings() {
        let mut errors = UiErrorKeys::default();
        fish_verdict(false, &mut errors);
        fish_verdict(true, &mut errors);
        assert_eq!(
            errors.0,
            vec![
                UiError::info_key("ERR_FISH_NOT_HOOKED"),
                UiError::info_key("ERR_FISH_ESCAPED"),
            ]
        );

        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let src = chain
            .read_file("Interface\\FrameXML\\GlobalStrings.lua")
            .expect("GlobalStrings.lua in the chain");
        let s = benilla_ui::script::UiScript::new().expect("VM");
        s.run(&String::from_utf8_lossy(&src)).expect("runs clean");
        let g = |key: &str| s.lua().globals().get::<String>(key).expect(key);
        assert_eq!(g("ERR_FISH_NOT_HOOKED"), "No fish are hooked.");
        assert_eq!(g("ERR_FISH_ESCAPED"), "Your fish got away!");
    }

    const ME: u64 = 0x0000_0000_0000_002A;
    const THEM: u64 = 0x0000_0000_0000_00FF;

    fn push(player_guid: u64, show_in_chat: bool) -> ItemPushResult {
        ItemPushResult {
            player_guid,
            from_npc: false,
            created: false,
            show_in_chat,
            bag_slot: 0xFF,
            item_slot: 0,
            item_entry: 2589,
            suffix_factor: 0,
            random_property_id: 0,
            count: 1,
        }
    }

    /// `OnItemPush`'s outermost gate: the push must be **ours** to produce anything at all. A party
    /// member's drop must NOT print as "You receive loot" — the real client takes its other-player
    /// arm and names them instead.
    #[test]
    fn only_our_own_pushes_reach_the_receive_queue() {
        let me = SelfGuid(Some(ME));
        assert!(is_our_push(&push(ME, true), &me));
        assert!(!is_our_push(&push(THEM, true), &me));
        // Before login lands a guid there is no active player to match against.
        assert!(!is_our_push(&push(ME, true), &SelfGuid(None)));
        // `showInChat` is NOT this gate (decision 0887): a silent push still queues, and still
        // animates — it only loses its chat line, downstream in `drain_receives`.
        assert!(is_our_push(&push(ME, false), &me));
        let mut loot = LootState::default();
        item_push_result(push(ME, false), &me, &mut loot);
        assert_eq!(loot.pending_receive_count(), 1);
    }
}
