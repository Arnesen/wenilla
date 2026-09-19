//! The inventory refusal's packet handler (in the net handler table since 2319, moved out of the
//! drain's loot arm file) — `SMSG_INVENTORY_CHANGE_FAILURE` onto the equip-error line, the pending
//! item locks, and the loot latch's sixth clear.

use benilla_protocol::{SessionEvent, SessionEventKind};
use bevy::prelude::*;

use super::{EquipError, EquipErrors};
use crate::net::NetHandlerApp;
use crate::pending_item_ops::{LockTransitions, PendingItemOps};
use crate::ui_loot::LootLatch;

/// Register the handler — called from [`super::UiItemsPlugin`].
pub(super) fn register(app: &mut App) {
    app.net_handler(SessionEventKind::InventoryFailure, on_inventory_failure);
}

fn on_inventory_failure(
    In(ev): In<SessionEvent>,
    mut equip_errors: ResMut<EquipErrors>,
    mut pending: ResMut<PendingItemOps>,
    mut lock_cleared: ResMut<LockTransitions>,
    mut latch: ResMut<LootLatch>,
) {
    if let SessionEvent::InventoryFailure {
        reason,
        required_level,
        item_guid,
        bag_slot,
    } = ev
    {
        inventory_failure(
            reason,
            required_level,
            item_guid,
            bag_slot,
            &mut equip_errors,
            &mut pending,
            &mut lock_cleared,
            &mut latch,
        );
    }
}

/// The server refused an inventory operation (`SMSG_INVENTORY_CHANGE_FAILURE` — equip level,
/// proficiency, bag full, …): the UI error line's inventory vocabulary, the equip twin of the
/// cast-result failure path. Also the pending-lock's failure-driven clear (decision 0216 §4 /
/// 0218 §3): every arrival here already has reason ≠ 0 (reason 0 is filtered before this event
/// exists at all — `benilla_protocol::events`'s `if reason != 0` guard), so it always tries a
/// [`PendingItemOps::clear_by_failure`]. This site has no `UiScript` to fire `ITEM_LOCK_CHANGED`
/// through, so the transitioned slots queue in [`LockTransitions`] for the container feed
/// ([`super::feed::feed_containers`]) to drain and fire next time it runs.
fn inventory_failure(
    reason: u8,
    required_level: Option<u32>,
    item_guid: u64,
    bag_slot: u8,
    equip_errors: &mut EquipErrors,
    pending: &mut PendingItemOps,
    lock_cleared: &mut LockTransitions,
    latch: &mut LootLatch,
) {
    debug!("net: inventory failure {reason:#04x} (item {item_guid:#x}, bag slot {bag_slot})");
    equip_errors.0.push(EquipError {
        reason,
        required_level,
        bag_slot,
    });
    lock_cleared.0.extend(pending.clear_by_failure(item_guid));
    // The sixth loot-latch clear (`0x5e3a84`, wow-re `loot-anim-leg.md` §5; decision 1477): when
    // the packet's **first item guid** is the object we are looting, the session ends here. It is
    // how an item-container loot (a lockbox) closes when the move out of it fails — the one clear
    // the 1471 census was missing. Guid-matched, as the bytes are.
    if item_guid != 0 {
        latch.clear_for(item_guid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sixth clear (`0x5e3a84`): an inventory-move failure whose first item guid IS the loot
    /// target ends that session. Guid-matched, so an unrelated bag failure leaves it alone.
    #[test]
    fn an_inventory_failure_on_the_looted_object_clears_the_latch() {
        const LOCKBOX: u64 = 0x4000_0000_0000_0007;
        let mut errs = EquipErrors::default();
        let mut pending = PendingItemOps::default();
        let mut cleared = LockTransitions::default();
        let mut latch = LootLatch(Some(LOCKBOX));

        // An unrelated item's failure must not end the session.
        inventory_failure(
            2,
            None,
            0x1234,
            0,
            &mut errs,
            &mut pending,
            &mut cleared,
            &mut latch,
        );
        assert_eq!(latch.0, Some(LOCKBOX));

        inventory_failure(
            2,
            None,
            LOCKBOX,
            0,
            &mut errs,
            &mut pending,
            &mut cleared,
            &mut latch,
        );
        assert_eq!(latch.0, None);
    }
}
