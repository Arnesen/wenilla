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
//!   `i` (1..24) → live `39 + i` (wire 39..62 + 1), bag button `j` (1..6) → live `63 + j`
//!   (wire 63..68 + 1) — the same "live id − 1 = wire slot" law as the doll
//!   (`crate::script::container`'s `EQUIPMENT_BAG` space).

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
    /// The bag buttons' icons: bank bag slot `i`'s held bag, resolved by the app to its icon path
    /// (`None` = the slot is empty or the template answer is in flight — the XML shows the empty
    /// slot texture). The reference reads these through the inventory-item API; benilla's doll
    /// array stops at the equipped bags, so the snapshot carries them instead
    /// (`BenillaGetBankBagTexture` below — benilla-named, like the merchant's stat feed).
    pub bag_textures: [Option<String>; 6],
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
    g.set(
        "BenillaGetBankBagTexture",
        lua.create_function(|lua, i: usize| {
            let texture = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model.bank.as_ref().and_then(|b| {
                    i.checked_sub(1)
                        .and_then(|n| b.bag_textures.get(n))
                        .and_then(|t| t.clone())
                })
            };
            Ok(match texture {
                Some(t) => Value::String(lua.create_string(&t)?),
                None => Value::Nil,
            })
        })?,
    )?;

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

    // BankButtonIDToInvSlotID(id, isBag) — the pure button→live-slot map (module doc): item
    // button 1..24 → 40..63, bag button 1..6 → 64..69. Out-of-range answers 0 (the reference
    // binding is total over its buttons; our XML never asks outside them — 0 is the visible
    // "wired wrong" tell rather than a silent misroute).
    g.set(
        "BankButtonIDToInvSlotID",
        lua.create_function(|_, (id, is_bag): (u32, Option<Value>)| {
            let is_bag = is_bag.is_some_and(|v| v.as_boolean().unwrap_or(true));
            let live = match (is_bag, id) {
                (false, 1..=24) => 39 + id,
                (true, 1..=6) => 63 + id,
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
            ..Default::default()
        }));
        assert!(s
            .eval::<bool>("local n, full = GetNumBankSlots()\nreturn n == 2 and full == nil")
            .unwrap());
        assert_eq!(s.eval::<i64>("return GetBankSlotCost(2)").unwrap(), 100_000);

        // Six purchased: full = 1; the cost read still answers (the DBC's own sentinel row).
        s.set_bank(Some(BankState {
            num_purchased: 6,
            next_cost: 999_999_999,
            ..Default::default()
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

    /// The bag-button icon feed: slot 1 resolved, slot 2 empty/in-flight (nil), out of range nil.
    #[test]
    fn bank_bag_texture_reads() {
        let mut s = UiScript::new().unwrap();
        assert!(s
            .eval::<bool>("return BenillaGetBankBagTexture(1) == nil")
            .unwrap());
        let mut state = BankState::default();
        state.bag_textures[0] = Some("Interface\\Icons\\INV_Misc_Bag_08".into());
        s.set_bank(Some(state));
        assert_eq!(
            s.eval::<String>("return BenillaGetBankBagTexture(1)")
                .unwrap(),
            "Interface\\Icons\\INV_Misc_Bag_08"
        );
        assert!(s
            .eval::<bool>("return BenillaGetBankBagTexture(2) == nil")
            .unwrap());
        assert!(s
            .eval::<bool>("return BenillaGetBankBagTexture(7) == nil")
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

    /// The button→live-slot map: items 1..24 → 40..63, bags 1..6 → 64..69 (live id − 1 = the wire
    /// slot: bank items 39..62, bank bags 63..68). The reference calls it with `this.isBag` = 1 or
    /// nil, so nil/false both mean "item button".
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
            s.eval::<i64>("return BankButtonIDToInvSlotID(1, 1)")
                .unwrap(),
            64
        );
        assert_eq!(
            s.eval::<i64>("return BankButtonIDToInvSlotID(6, 1)")
                .unwrap(),
            69
        );
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
            s.eval::<i64>("return BankButtonIDToInvSlotID(7, 1)")
                .unwrap(),
            0
        );
    }
}
