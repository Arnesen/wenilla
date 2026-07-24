//! The group-loot-roll bindings (decision 0591) — the `GroupLootFrame` half of the loot surface,
//! the same two-way seam as [`super::loot`]: the app pushes a snapshot of the **open rolls**
//! ([`UiScript::set_loot_rolls`] — each already resolved to name/icon/quantity/quality/bind and a
//! live time-remaining) and the Lua `RollOnLoot` call queues an outbound **vote** the app drains
//! ([`UiScript::take_loot_roll_votes`]). The engine holds no roll knowledge — a roll is "an id, an
//! item's display fields, and how long is left".
//!
//! ## The Era API shape
//!
//! 1.12 drives four `GroupLootFrame`s (`NUM_GROUP_LOOT_FRAMES = 4`, `LootFrame.lua:2`) off a flat
//! set of globals (VERIFIED against the extracted `LootFrame.lua`/`LootFrame.xml`/`UIParent.lua`):
//!
//! - `START_LOOT_ROLL` fires with `arg1 = rollID, arg2 = rollTime`; `UIParent.lua:513-515` hands it
//!   to `GroupLootFrame_OpenNewFrame(id, rollTime)`, which claims the first non-visible frame,
//!   stores `frame.rollID`, and sets its `Timer` StatusBar's max to `rollTime` (`LootFrame.lua:246-258`).
//! - `GetLootRollItemInfo(rollID)` → `texture, name, count, quality, bindOnPickUp`
//!   (`LootFrame.lua:261`) — `bindOnPickUp` swaps the frame to the gold BoP backdrop.
//! - `GetLootRollTimeLeft(rollID)` → the milliseconds left, polled from the Timer's `OnUpdate`
//!   (`LootFrame.lua:287-296`).
//! - `RollOnLoot(rollID, rollType)` — `0` Pass, `1` Need, `2` Greed, wired to the frame's
//!   PassButton/RollButton/GreedButton `OnClick` (`LootFrame.xml:375`/`:398`/`:425`).
//! - `CANCEL_LOOT_ROLL` fires with `arg1 = rollID`; the matching frame hides (`LootFrame.lua:279-285`).
//!
//! `rollID` is **client-internal** — it never reaches the wire (`CMSG_LOOT_ROLL` addresses a roll by
//! `(lootedTarget, itemSlot)` instead), so the app allocates its own monotonic id per open roll.
//! An unknown `rollID` answers `nil` / `0`, exactly as an out-of-range loot slot does.

use mlua::Lua;

use super::Model;

/// `rollType` 0 — the one vote the bind-on-pickup gate never intercepts (passing binds nothing).
const PASS: u8 = 0;

/// One open group loot roll, resolved by the app (decision 0591). Plain data — the app rebuilds the
/// whole list each frame, so `time_left_ms` is simply re-derived rather than ticked here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LootRollEntry {
    /// The client-internal roll id the Lua side addresses this roll by — allocated by the app,
    /// never on the wire. Stable for the life of the roll.
    pub roll_id: u32,
    /// The item's name (`GetLootRollItemInfo`'s `name`). `None` while the ask-once item-template
    /// query is still in flight; the API reports `nil` and the frame shows its placeholder.
    pub name: Option<String>,
    /// Icon texture path (`Interface\Icons\…`). `None` only if the display catalog had no icon.
    pub texture: Option<String>,
    /// The stack size being rolled for (`GetLootRollItemInfo`'s `count`).
    pub quantity: u32,
    /// Item quality 0..6, for the quality-coloured name; `None` while the template is in flight.
    pub quality: Option<u32>,
    /// Whether the item binds when picked up — the gold-backdrop swap in `GroupLootFrame_OnShow`.
    /// `false` while the template is in flight (the plain backdrop is the safe default).
    pub bind_on_pickup: bool,
    /// Milliseconds left before the roll times out — re-derived by the app each frame from the
    /// roll's start and `SMSG_LOOT_START_ROLL`'s countdown. Saturates at `0`.
    pub time_left_ms: u32,
    /// The item id — the shared item-tooltip store's key (`BenillaGetItemStats`). A benilla
    /// extension riding as a TRAILING return of `GetLootRollItemInfo`, the same idiom
    /// [`super::loot::LootRow::item_id`] uses on `GetLootSlotInfo`.
    pub item_id: u32,
}

/// Every group loot roll currently open, in the order the app opened them. Pushed whole each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LootRollsState {
    pub rolls: Vec<LootRollEntry>,
}

impl super::UiScript {
    /// Push the open-rolls snapshot (an empty list means no roll is open).
    pub fn set_loot_rolls(&mut self, state: LootRollsState) {
        self.model_mut().loot_rolls = state;
    }

    /// Drain the `(roll_id, roll_type)` votes queued by `RollOnLoot` since the last call. The app
    /// maps each roll id back to its `(lootedTarget, itemSlot)` for `CMSG_LOOT_ROLL`.
    pub fn take_loot_roll_votes(&mut self) -> Vec<(u32, u8)> {
        std::mem::take(&mut self.model_mut().loot_roll_votes)
    }

    /// Drain the `(roll_id, roll_type)` **confirm requests** — a Need or Greed on a bind-on-pickup
    /// roll, which sends nothing and instead asks for the `CONFIRM_LOOT_ROLL` popup (see
    /// [`install`]'s `RollOnLoot`). The app fires the event; the popup's OnAccept calls
    /// `ConfirmLootRoll`, which queues the real vote.
    pub fn take_loot_roll_confirms(&mut self) -> Vec<(u32, u8)> {
        std::mem::take(&mut self.model_mut().loot_roll_confirms)
    }
}

/// Look one roll up by its client-internal id.
fn find(model: &Model, roll_id: u32) -> Option<&LootRollEntry> {
    model.loot_rolls.rolls.iter().find(|r| r.roll_id == roll_id)
}

/// Register the group-loot-roll globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetLootRollItemInfo(rollID) → texture, name, count, quality, bindOnPickUp, itemID.
    // An unknown id answers nil (the frame is mid-teardown, or an addon asked for a stale roll).
    g.set(
        "GetLootRollItemInfo",
        lua.create_function(|lua, roll_id: u32| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(r) = find(&model, roll_id) else {
                return Ok(mlua::MultiValue::new());
            };
            let vals = (
                r.texture.clone(),
                r.name.clone(),
                r.quantity,
                r.quality,
                r.bind_on_pickup,
                r.item_id,
            );
            lua.pack_multi(vals)
        })?,
    )?;

    // GetLootRollTimeLeft(rollID) → milliseconds remaining; 0 for an unknown id (the reference
    // OnUpdate clamps anything outside the bar's range to its minimum anyway, LootFrame.lua:290-293).
    g.set(
        "GetLootRollTimeLeft",
        lua.create_function(|lua, roll_id: u32| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(find(&model, roll_id).map_or(0, |r| r.time_left_ms))
        })?,
    )?;

    // RollOnLoot(rollID, rollType) — queue the vote. rollType is 0 Pass / 1 Need / 2 Greed; the
    // server hard-rejects anything >= 3 (MAX_ROLL_FROM_CLIENT), so out-of-range votes are dropped
    // here rather than sent. An unknown rollID is likewise dropped — the app couldn't map it back
    // to a (lootedTarget, itemSlot) anyway.
    //
    // THE BoP GATE (decision 0594, VERIFIED in the 5875 binary at `0x61bdf0`): a Need or Greed on
    // an item whose template binds on pickup sends **no packet at all** and leaves the dialog up —
    // it fires `CONFIRM_LOOT_ROLL` and returns (`0x61be8b`). Only `ConfirmLootRoll` (below), which
    // the popup's OnAccept calls, re-enters past the gate. Pass is never gated: passing binds
    // nothing. This gate lives here, in the seam, because that is where the real client puts it —
    // in the C function, not in the Lua — so a stock addon calling RollOnLoot on a BoP item gets
    // the confirm rather than silently binding the item.
    g.set(
        "RollOnLoot",
        lua.create_function(|lua, (roll_id, roll_type): (u32, u8)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let Some(entry) = find(&model, roll_id) else {
                return Ok(());
            };
            if roll_type > 2 {
                return Ok(());
            }
            if entry.bind_on_pickup && roll_type != PASS {
                model.loot_roll_confirms.push((roll_id, roll_type));
            } else {
                model.loot_roll_votes.push((roll_id, roll_type));
            }
            Ok(())
        })?,
    )?;

    // ConfirmLootRoll(rollID, rollType) — the BoP gate's bypass (`0x4c33e0`, which re-enters
    // `0x61bdf0` with the third argument `1`). Same validation as RollOnLoot minus the bind check.
    g.set(
        "ConfirmLootRoll",
        lua.create_function(|lua, (roll_id, roll_type): (u32, u8)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if roll_type <= 2 && find(&model, roll_id).is_some() {
                model.loot_roll_votes.push((roll_id, roll_type));
            }
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LootRollEntry, LootRollsState};
    use crate::script::UiScript;

    fn rolls() -> LootRollsState {
        LootRollsState {
            rolls: vec![
                // A resolved BoP item.
                LootRollEntry {
                    roll_id: 7,
                    name: Some("Staff of Jordan".into()),
                    texture: Some("Interface\\Icons\\INV_Staff_12".into()),
                    quantity: 1,
                    quality: Some(4),
                    bind_on_pickup: true,
                    time_left_ms: 42_000,
                    item_id: 17182,
                },
                // An in-flight item: the roll opened, the item-template answer hasn't landed.
                LootRollEntry {
                    roll_id: 8,
                    name: None,
                    texture: None,
                    quantity: 2,
                    quality: None,
                    bind_on_pickup: false,
                    time_left_ms: 60_000,
                    item_id: 4306,
                },
            ],
        }
    }

    #[test]
    fn roll_snapshot_reads() {
        let mut s = UiScript::new().unwrap();
        // No roll open: info nil, time left 0.
        assert!(s
            .eval::<bool>("return GetLootRollItemInfo(7) == nil")
            .unwrap());
        assert_eq!(s.eval::<i64>("return GetLootRollTimeLeft(7)").unwrap(), 0);

        s.set_loot_rolls(rolls());

        // The resolved roll: the full 1.12 five-tuple, in LootFrame.lua's order.
        let (texture, name, count, quality, bop) = s
            .eval::<(String, String, i64, i64, bool)>("return GetLootRollItemInfo(7)")
            .unwrap();
        assert_eq!(texture, "Interface\\Icons\\INV_Staff_12");
        assert_eq!(name, "Staff of Jordan");
        assert_eq!((count, quality, bop), (1, 4, true));
        assert_eq!(
            s.eval::<i64>("return GetLootRollTimeLeft(7)").unwrap(),
            42_000
        );
        // The benilla item-id extension rides as the trailing return.
        assert_eq!(
            s.eval::<i64>("local _, _, _, _, _, id = GetLootRollItemInfo(7)\nreturn id")
                .unwrap(),
            17182
        );

        // The in-flight roll: name/texture/quality nil, count + BoP default still readable.
        assert!(s
            .eval::<bool>(
                "local t, n, c, q, b = GetLootRollItemInfo(8)\n\
                 return t == nil and n == nil and q == nil and c == 2 and b == false",
            )
            .unwrap());

        // An unknown id → nil / 0, never an error.
        assert!(s
            .eval::<bool>("return GetLootRollItemInfo(99) == nil")
            .unwrap());
        assert_eq!(s.eval::<i64>("return GetLootRollTimeLeft(99)").unwrap(), 0);
    }

    #[test]
    fn roll_on_loot_queues_votes() {
        let mut s = UiScript::new().unwrap();
        s.set_loot_rolls(rolls());
        // Roll 8 is NOT bind-on-pickup, so every vote on it goes straight out.
        s.run("RollOnLoot(8, 1)").unwrap(); // Need
        s.run("RollOnLoot(8, 2)").unwrap(); // Greed
        s.run("RollOnLoot(8, 0)").unwrap(); // Pass
        assert_eq!(s.take_loot_roll_votes(), vec![(8, 1), (8, 2), (8, 0)]);
        assert!(s.take_loot_roll_votes().is_empty(), "drained");
        assert!(s.take_loot_roll_confirms().is_empty(), "nothing to confirm");
    }

    /// The BoP gate (decision 0594): Need/Greed on a bind-on-pickup roll must send NOTHING and ask
    /// for the confirm popup instead; Pass on the same roll goes straight out. This is the
    /// behaviour benilla shipped wrong in 0591 — it sent the vote immediately, binding the item
    /// with no prompt.
    #[test]
    fn need_or_greed_on_a_bop_roll_confirms_instead_of_voting() {
        let mut s = UiScript::new().unwrap();
        s.set_loot_rolls(rolls()); // roll 7 is bind_on_pickup
        s.run("RollOnLoot(7, 1)").unwrap(); // Need on BoP
        s.run("RollOnLoot(7, 2)").unwrap(); // Greed on BoP
        assert!(
            s.take_loot_roll_votes().is_empty(),
            "a BoP need/greed must not reach the wire before the popup is accepted"
        );
        assert_eq!(s.take_loot_roll_confirms(), vec![(7, 1), (7, 2)]);

        // Pass is never gated — passing binds nothing.
        s.run("RollOnLoot(7, 0)").unwrap();
        assert_eq!(s.take_loot_roll_votes(), vec![(7, 0)]);
        assert!(s.take_loot_roll_confirms().is_empty());

        // ConfirmLootRoll is the bypass: the popup's OnAccept lands the real vote.
        s.run("ConfirmLootRoll(7, 1)").unwrap();
        assert_eq!(s.take_loot_roll_votes(), vec![(7, 1)]);
        assert!(s.take_loot_roll_confirms().is_empty(), "no second prompt");
    }

    /// `ConfirmLootRoll` bypasses only the *bind* gate — it still validates id and range, or a
    /// stale popup could push a vote the app cannot address.
    #[test]
    fn confirm_still_validates() {
        let mut s = UiScript::new().unwrap();
        s.set_loot_rolls(rolls());
        s.run("ConfirmLootRoll(99, 1)").unwrap(); // no such roll
        s.run("ConfirmLootRoll(7, 3)").unwrap(); // server-only rollType
        assert!(s.take_loot_roll_votes().is_empty());
    }

    /// The two votes that must never reach the wire: a rollType the server would reject outright
    /// (`>= MAX_ROLL_FROM_CLIENT`), and an id no open roll owns (nothing to address it with).
    #[test]
    fn bad_votes_are_dropped() {
        let mut s = UiScript::new().unwrap();
        s.set_loot_rolls(rolls());
        s.run("RollOnLoot(7, 3)").unwrap(); // ROLL_NOT_EMITED_YET — server-only
        s.run("RollOnLoot(7, 200)").unwrap();
        s.run("RollOnLoot(99, 1)").unwrap(); // no such roll
        assert!(s.take_loot_roll_votes().is_empty());
    }

    #[test]
    fn clearing_the_rolls_empties_them() {
        let mut s = UiScript::new().unwrap();
        s.set_loot_rolls(rolls());
        s.set_loot_rolls(LootRollsState::default());
        assert!(s
            .eval::<bool>("return GetLootRollItemInfo(7) == nil")
            .unwrap());
        assert_eq!(s.eval::<i64>("return GetLootRollTimeLeft(7)").unwrap(), 0);
    }
}
