//! The bank bindings (decision 0604 phase 4) — the Era-shaped bank surface, the same two-way seam
//! as [`super::merchant`]: the app pushes a **bank snapshot** ([`UiScript::set_bank`] — the
//! purchased-slot count off the descriptor's `PLAYER_BYTES_2` byte 2 plus the next slot's
//! `BankBagSlotPrices.dbc` cost), and the Lua `PurchaseSlot`/`CloseBankFrame` calls queue outbound
//! **intents** the app drains ([`UiScript::take_bank_purchase`] / [`UiScript::take_bank_close`]).
//!
//! The bank's *contents* never pass through here: bank slots are player-array slots the container
//! seam already carries — the app feeds them as container `-1` (`BANK_CONTAINER`, the 24 generic
//! slots) and containers `5..=10` (the six bank bags), the reference client's own id space
//! (`BankFrame.lua:1-4`), so the container verbs, the cursor drag-drop, and the stack split all
//! work on bank slots with no bank-specific surface.
//!
//! Nor do the six bank BAGS — the bag items themselves, as opposed to what is inside them. Those
//! are inventory slots at live ids 64..=69, fed beside the paper doll's
//! ([`super::char_stats::BankBagSlots`]) and read through the ordinary `GetInventoryItem*` /
//! `PickupBagFromSlot` surface, which is exactly how the reference's own bank reads them
//! (`BankFrame.lua:28`, `ButtonInventorySlot`). They stream in the player descriptor whether or
//! not a banker is open, so they are not part of the window's snapshot.
//!
//! ## The 5875 API shape (the reference `BankFrame.lua`, read as behavior spec this session)
//!
//! - `GetNumBankSlots()` → `numSlots, full` — purchased count 0..6, `full` as `1`/`nil`
//!   (`UpdateBagSlotStatus` destructures exactly this pair; `full` hides the purchase frame).
//! - `GetBankSlotCost(numSlots)` → the NEXT slot's cost in copper. The real binding reads
//!   `BankBagSlotPrices.dbc` — whose rows 7+ hold a 999999999 sentinel, so the call answers even
//!   when the bank is full (the purchase frame is already hidden then). The argument is ignored
//!   here as it is there: the cost of "the next slot" is a fact of the pushed state.
//! - `PurchaseSlot()` — the confirm popup's accept (`StaticPopup.lua` `CONFIRM_BUY_BANK_SLOT`):
//!   queue the buy intent; the app sends `CMSG_BUY_BANK_SLOT`. No packet on success — the
//!   descriptor's byte-2 delta is the confirmation (`PLAYERBANKBAGSLOTS_CHANGED`).
//! - `CloseBankFrame()` — client-side close, **no packet exists** for it (decision 0604): flag the
//!   app to clear its session, the merchant/gossip pattern.
//! - `BankButtonIDToInvSlotID(id, isBag)` — the pure button→live-inventory-slot map: item button
//!   `i` (1..24) → live `39 + i` (wire 39..62 + 1), bag button — whose id is the **container id**
//!   5..10, not a bag number — → live `59 + id` (wire 63..68 + 1); the same "live id − 1 = wire
//!   slot" law as the doll (`crate::script::container`'s `EQUIPMENT_BAG` space). See the binding
//!   for the four places the reference's own file pins the bag arm's numbering.

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// The open bank window's snapshot: what the purchase row and the six bag buttons need. Pushed
/// whole by the app while the bank session is open; `None` = no bank open (the window is closed).
/// The bank's item contents ride the container seam (module doc), not this.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BankState {
    /// Purchased bank-bag slots (0..=6) — the descriptor's `PLAYER_BYTES_2` byte 2.
    pub num_purchased: u32,
    /// The NEXT slot's price in copper (`BankBagSlotPrices.dbc` row `num_purchased + 1`; the DBC's
    /// own 999999999 sentinel past slot 6). 0 only if the DBC row is genuinely absent.
    pub next_cost: u32,
}

impl super::UiScript {
    /// Push (or clear, with `None`) the open bank's snapshot.
    pub fn set_bank(&mut self, state: Option<BankState>) {
        self.model_mut().bank = state;
    }

    /// Whether `PurchaseSlot()` was called since the last drain (and clear the flag). The app
    /// sends `CMSG_BUY_BANK_SLOT` to the open session's banker.
    pub fn take_bank_purchase(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().bank_purchase)
    }

    /// Whether `CloseBankFrame()` was called since the last drain (and clear the flag). No packet
    /// — the app just clears its local bank session (the merchant pattern).
    pub fn take_bank_close(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().bank_close)
    }
}

/// Register the bank globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetNumBankSlots() → numSlots, full (1/nil — the client's boolean shape; the reference
    // destructures `local numSlots, full = GetNumBankSlots()`). 0, nil with no bank open.
    g.set(
        "GetNumBankSlots",
        lua.create_function(|lua, ()| {
            let n = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model.bank.as_ref().map_or(0, |b| b.num_purchased)
            };
            let full = if n >= 6 {
                Value::Integer(1)
            } else {
                Value::Nil
            };
            Ok(MultiValue::from_vec(vec![
                Value::Integer(i64::from(n)),
                full,
            ]))
        })?,
    )?;

    // GetBankSlotCost(numSlots) → the next slot's cost in copper (module doc: the argument is
    // decorative — the pushed state already names the next slot). 0 with no bank open.
    g.set(
        "GetBankSlotCost",
        lua.create_function(|lua, _n: Option<u32>| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(model.bank.as_ref().map_or(0, |b| b.next_cost)))
        })?,
    )?;

    // BenillaGetBankBagTexture(i) → the bank bag slot's held-bag icon path | nil (empty slot, or
    // no bank open) — benilla-named (module doc: the snapshot carries what the reference read
    // through the inventory-item API).
    // PurchaseSlot() — queue the bank-slot buy intent (the CONFIRM_BUY_BANK_SLOT popup's accept).
    g.set(
        "PurchaseSlot",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.bank_purchase = true;
            Ok(())
        })?,
    )?;

    // CloseBankFrame() — client-side close (no packet exists, decision 0604): flag the app.
    g.set(
        "CloseBankFrame",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.bank_close = true;
            Ok(())
        })?,
    )?;

    // BankButtonIDToInvSlotID(id, isBag) — the pure button→live-slot map (module doc).
    //
    // **The bag arm takes the CONTAINER id 5..=10, not a bag number 1..=6**, and getting that
    // wrong reads six slots off the end of the band. The reference's own file is the oracle
    // (0675) and says so four times over: `BankFrame.xml` gives `BankFrameBag1..BankFrameBag6`
    // the ids **5..10** (the generic buttons `BankFrameItem1..24` take 1..24);
    // `ButtonInventorySlot` hands `this:GetID()` straight in; and both
    // `UpdateBagButtonHighlight(id)` (`"BankFrameBag"..(id - NUM_BAG_SLOTS)`) and
    // `BankFrameItemButton_UpdateLock` (`(this:GetID() - 4) > GetNumBankSlots()`) subtract 4 from
    // that same id to get back to 1..6. So the bag arm is `id + 59` — byte-identical to
    // `ContainerIDToInventoryID`'s carved second arm (`t = id - 1; t >= 4 → t + 60`,
    // `crate::script::container`), which is the same map under the other name.
    //
    // Out-of-range answers 0 (the reference binding is total over its buttons; our XML never asks
    // outside them — 0 is the visible "wired wrong" tell rather than a silent misroute).
    g.set(
        "BankButtonIDToInvSlotID",
        lua.create_function(|_, (id, is_bag): (u32, Option<Value>)| {
            let is_bag = is_bag.is_some_and(|v| v.as_boolean().unwrap_or(true));
            let live = match (is_bag, id) {
                (false, 1..=24) => 39 + id,
                (true, 5..=10) => 59 + id,
                _ => 0,
            };
            Ok(i64::from(live))
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::BankState;
    use crate::script::UiScript;

    /// The purchase-row reads: closed → (0, nil)/0; open → the pushed count + next cost; six
    /// purchased → `full` = 1 (the reference hides the purchase frame on it).
    #[test]
    fn bank_snapshot_reads() {
        let mut s = UiScript::new().unwrap();
        assert!(s
            .eval::<bool>("local n, full = GetNumBankSlots()\nreturn n == 0 and full == nil")
            .unwrap());
        assert_eq!(s.eval::<i64>("return GetBankSlotCost(0)").unwrap(), 0);

        s.set_bank(Some(BankState {
            num_purchased: 2,
            next_cost: 100_000,
        }));
        assert!(s
            .eval::<bool>("local n, full = GetNumBankSlots()\nreturn n == 2 and full == nil")
            .unwrap());
        assert_eq!(s.eval::<i64>("return GetBankSlotCost(2)").unwrap(), 100_000);

        // Six purchased: full = 1; the cost read still answers (the DBC's own sentinel row).
        s.set_bank(Some(BankState {
            num_purchased: 6,
            next_cost: 999_999_999,
        }));
        assert!(s
            .eval::<bool>("local n, full = GetNumBankSlots()\nreturn n == 6 and full == 1")
            .unwrap());

        // Clearing empties it.
        s.set_bank(None);
        assert!(s
            .eval::<bool>("local n, full = GetNumBankSlots()\nreturn n == 0 and full == nil")
            .unwrap());
    }

    /// The six bank BAGS are inventory slots at live ids 64..=69, read through the ordinary
    /// inventory API — the band the reference's own bank uses (`ButtonInventorySlot` →
    /// `BankButtonIDToInvSlotID(id, this.isBag)`, BankFrame.lua:28). Empty slots answer nil, and
    /// the band does not bleed into the equipment array below it.
    #[test]
    fn bank_bag_slots_read_through_the_inventory_api() {
        let mut s = UiScript::new().unwrap();
        assert!(s
            .eval::<bool>("return GetInventoryItemTexture(\"player\", 64) == nil")
            .unwrap());

        let mut bags: crate::script::BankBagSlots = Default::default();
        bags[0] = Some(crate::script::InvSlotView {
            item_id: 4500,
            icon: Some("Interface\\Icons\\INV_Misc_Bag_08".into()),
            count: 1,
            link: Some("|cffffffff|Hitem:4500:0:0:0|h[Traveler\'s Backpack]|h|r".into()),
            equip_slots: vec![20, 21, 22, 23],
            ..Default::default()
        });
        s.set_bank_bag_slots(bags);

        assert_eq!(
            s.eval::<i64>("return BankButtonIDToInvSlotID(5, 1)")
                .unwrap(),
            64,
            "BankFrameBag1's own id is 5 — the container id, not a bag number"
        );
        assert_eq!(
            s.eval::<String>("return GetInventoryItemTexture(\"player\", 64)")
                .unwrap(),
            "Interface\\Icons\\INV_Misc_Bag_08"
        );
        assert_eq!(
            s.eval::<i64>("return GetInventoryItemID(\"player\", 64)")
                .unwrap(),
            4500
        );
        // Bag slot 2 is empty, and slot 70 is past the band — neither falls through to the doll.
        assert!(s
            .eval::<bool>("return GetInventoryItemTexture(\"player\", 65) == nil")
            .unwrap());
        assert!(s
            .eval::<bool>("return GetInventoryItemTexture(\"player\", 70) == nil")
            .unwrap());
    }

    #[test]
    fn purchase_and_close_flag_the_intents() {
        let mut s = UiScript::new().unwrap();
        assert!(!s.take_bank_purchase());
        s.run("PurchaseSlot()").unwrap();
        assert!(s.take_bank_purchase());
        assert!(!s.take_bank_purchase(), "drained");

        assert!(!s.take_bank_close());
        s.run("CloseBankFrame()").unwrap();
        assert!(s.take_bank_close());
        assert!(!s.take_bank_close(), "drained");
    }

    /// **The bank's inventory BAND answers from the container feed** — the map decision 1751's
    /// bank swap turns on. The reference's bank paints every slot through the inventory API
    /// (`BankFrameItemButton_OnUpdate` → `GetInventoryItemTexture("player", BankButtonIDToInv
    /// SlotID(id))`, BankFrame.lua:35) while benilla feeds the same items as container `-1`. If
    /// those two ever disagree the bank draws empty, which is exactly the failure this pins.
    ///
    /// Asserted through the live API rather than the model, because the API is what the
    /// reference's file calls, and the tooltip must agree with the icon under it.
    #[test]
    fn the_bank_band_reads_the_vault_through_the_inventory_api() {
        let mut s = UiScript::new().unwrap();
        let mut slots = std::collections::HashMap::new();
        slots.insert(
            3,
            super::super::ContainerSlot {
                item_id: 4496,
                count: 7,
                texture: Some("Interface\\Icons\\INV_Misc_Bag_08".into()),
                quality: Some(2),
                link: Some("|cff1eff00|Hitem:4496:0:0:0|h[Small Brown Pouch]|h|r".into()),
                ..Default::default()
            },
        );
        s.set_container(
            -1,
            Some(super::super::ContainerState {
                name: Some("Bank".into()),
                num_slots: 24,
                slots,
            }),
        );

        // Vault slot 3 is live-API inventory id 42 (BankButtonIDToInvSlotID(3)).
        assert_eq!(
            s.eval::<i64>("return BankButtonIDToInvSlotID(3)").unwrap(),
            42
        );
        assert_eq!(
            s.eval::<String>("return GetInventoryItemTexture(\"player\", 42)")
                .unwrap(),
            "Interface\\Icons\\INV_Misc_Bag_08"
        );
        assert_eq!(
            s.eval::<i64>("return GetInventoryItemCount(\"player\", 42)")
                .unwrap(),
            7
        );
        assert_eq!(
            s.eval::<i64>("return GetInventoryItemID(\"player\", 42)")
                .unwrap(),
            4496
        );
        // …and an empty vault slot answers nil, not the equipment slot that shares no numbering
        // with it — the band must not fall through to `inventory_slots`.
        assert!(s
            .eval::<bool>("return GetInventoryItemTexture(\"player\", 43) == nil")
            .unwrap());
        // The doll is untouched either side of the band.
        assert!(s
            .eval::<bool>("return GetInventoryItemTexture(\"player\", 16) == nil")
            .unwrap());
    }

    /// The button→live-slot map: item buttons 1..24 → 40..63; bag buttons — **whose ids are the
    /// container ids 5..10, not bag numbers** — → 64..69 (live id − 1 = the wire slot: bank items
    /// 39..62, bank bags 63..68). The reference calls it with `this.isBag` = 1 or nil, so
    /// nil/false both mean "item button".
    ///
    /// The bag arm's numbering is the reference file's, checked here because getting it wrong
    /// reads six slots off the end of the band and draws an empty bag row: `BankFrame.xml` gives
    /// `BankFrameBag1` `id="5"`, and `ButtonInventorySlot` hands that id straight in.
    #[test]
    fn bank_button_to_inv_slot() {
        let s = UiScript::new().unwrap();
        assert_eq!(
            s.eval::<i64>("return BankButtonIDToInvSlotID(1)").unwrap(),
            40
        );
        assert_eq!(
            s.eval::<i64>("return BankButtonIDToInvSlotID(24)").unwrap(),
            63
        );
        assert_eq!(
            s.eval::<i64>("return BankButtonIDToInvSlotID(5, 1)")
                .unwrap(),
            64,
            "BankFrameBag1 carries id 5"
        );
        assert_eq!(
            s.eval::<i64>("return BankButtonIDToInvSlotID(10, 1)")
                .unwrap(),
            69,
            "BankFrameBag6 carries id 10"
        );
        // It is the SAME map `ContainerIDToInventoryID` computes for a bank bag — one arithmetic
        // under two names, and a drift between them would put the bag row and the bag windows on
        // different slots.
        for id in 5..=10 {
            assert_eq!(
                s.eval::<i64>(&format!("return BankButtonIDToInvSlotID({id}, 1)"))
                    .unwrap(),
                s.eval::<i64>(&format!("return ContainerIDToInventoryID({id})"))
                    .unwrap()
            );
        }
        // false is "not a bag" (nil-or-false truthiness), out of range answers 0.
        assert_eq!(
            s.eval::<i64>("return BankButtonIDToInvSlotID(2, false)")
                .unwrap(),
            41
        );
        assert_eq!(
            s.eval::<i64>("return BankButtonIDToInvSlotID(25)").unwrap(),
            0
        );
        assert_eq!(
            s.eval::<i64>("return BankButtonIDToInvSlotID(4, 1)")
                .unwrap(),
            0,
            "4 is an EQUIPPED bag, not a bank one"
        );
    }
}
