//! The loot bindings (decision 0084) — the Era-shaped loot-window surface, the same two-way seam as
//! [`super::merchant`]/[`super::container`]/[`super::gossip`]: the app pushes a **loot snapshot**
//! ([`UiScript::set_loot`] — the rows already resolved from the wire to name/icon/quantity/quality by
//! the app's item stores, with the synthesized coin row first) and the Lua `LootSlot`/`CloseLoot`
//! calls queue outbound **intents** the app drains ([`UiScript::take_loot_picks`] /
//! [`UiScript::take_loot_close`]). The engine holds no loot knowledge — a row is "a name, an icon
//! path, a quantity, a quality, and whether it's the coin pile".
//!
//! ## The Era API shape
//!
//! 1.12's loot API is a flat set of globals the FrameXML `LootFrame.lua` drives (VERIFIED against the
//! extracted `LootFrame.lua`, scratchpad): `GetNumLootItems()`, `GetLootSlotInfo(slot)` →
//! `texture, item, quantity, quality` (`LootFrame.lua:81`), `LootSlotIsItem(slot)` /
//! `LootSlotIsCoin(slot)` (`LootFrame.lua:80`), `GetLootSlotLink(slot)` → the row's item link (what
//! the row click's ctrl/shift arms read, `LootFrame.lua:149`/`:152` — decision 1059),
//! `LootSlot(slot)` (the C `LootButton` behaviour, the action a row click performs —
//! `LootFrame.lua:94`), and `CloseLoot()` (fired from `OnHide`, `LootFrame.lua:143-145`).
//! `slot` is **1-based**; an out-of-range slot answers `nil`.
//!
//! The **coin pile is a synthesized client-side row** (first in the list when the loot carries gold):
//! `LootSlotIsCoin` is true for it, its `item` text is the formatted money amount, and `LootSlot(1)`
//! on it queues the money intent. The app maps a clicked 1-based row to either the money intent or the
//! wire loot slot the item lives at (`CMSG_AUTOSTORE_LOOT_ITEM` addresses the **wire** slot, which is
//! *not* the 1-based display position once a coin row is prepended or a row is removed) — the Lua side
//! never sees the wire slot, exactly as the merchant's Lua side never sees the item entry.
//!
//! ## Master loot (decision 1675)
//!
//! Two more globals ride the same seam when the group's loot method is master loot:
//! `GetMasterLootCandidate(index)` reads [`LootState::master_candidates`] (names, 1-based, dense)
//! and `GiveMasterLoot(slot, candidateIndex)` queues an assignment the app resolves to a wire slot
//! and a recipient guid. The *decision* to open the dropdown is not Lua's: the app fires
//! `OPEN_MASTER_LOOT_LIST` when a picked row's wire `slot_type` is `MASTER`, which is where the
//! real client puts it too — its take dispatcher branches on the same byte before any Lua runs.

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// One loot-window row, resolved by the app (decision 0084). Plain data — its 1-based order in the
/// window is its position in [`LootState::rows`]; the coin row (when present) is always position 1.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LootRow {
    /// The row text (`GetLootSlotInfo`'s `item` return): an item's name, or the coin row's formatted
    /// money amount. `None` while the ask-once item-template query is still in flight (items only —
    /// the coin row's text is always set); the API reports `nil` and the XML shows a placeholder.
    pub name: Option<String>,
    /// Icon texture path (`Interface\Icons\…` — from the wire `display_info_id`, or the coin
    /// texture). `None` only if the display catalog had no icon for the id.
    pub texture: Option<String>,
    /// The stack size looted (`GetLootSlotInfo`'s `quantity`); the coin row is always `1`.
    pub quantity: u32,
    /// Item quality 0..6, for the quality-coloured row text; `None` while the template is in flight
    /// (the XML falls back to common/white). The coin row carries a fixed quality.
    pub quality: Option<u32>,
    /// Whether this is the synthesized coin pile (`LootSlotIsCoin` true, `LootSlotIsItem` false).
    pub is_coin: bool,
    /// The item id — the shared item-tooltip store's key (`BenillaGetItemStats`); `0` for the coin
    /// row. A benilla extension riding as a TRAILING return of `GetLootSlotInfo` (the era 4-tuple
    /// never carried it; tooltip content was C++'s alone), same idiom as the quest item getters.
    pub item_id: u32,
    /// The row's full escaped `|cff…|Hitem:…|h[Name]|h|r` link (`GetLootSlotLink`, decision 1059).
    /// `None` for the synthesized coin row (there is no item to link) and while the ask-once
    /// template answer is in flight — the link embeds the name, so it cannot exist before `name`
    /// does; the same `Option` shape [`super::char_stats::InvSlotView::link`] carries. Both arms of
    /// the row click take a nil in stride: `DressUpItemLink` returns on one (its own guard,
    /// `DressUpFrame.lua:10-16`) and the shift arm goes through `BenillaChatEdit_InsertLink`, which
    /// drops it — our `EditBox:Insert` binding is typed `String` and would raise.
    pub link: Option<String>,
    /// The wire's `randomPropertyId` — the drop's **random-suffix roll**, the id the tooltip
    /// resolves against [`super::Model::random_properties`] for its enchant lines (decision 1547).
    /// `0` = unrolled.
    ///
    /// The client keeps exactly this, at `+0x14` of its own 0x1c-byte loot record, and
    /// `SetLootItem 0x533470` copies it into the tooltip's `+0x424` — a loot slot is **not** an
    /// item object (that leg passes an all-zero item guid), so the roll is the only enchant source
    /// a loot hover can have (wow-re `loot-slot-record.md`, `tooltip-content-law.md` §E6-LOOT).
    /// [`Self::name`] already carries the suffix the same id joins on.
    pub random_property_id: u32,
}

/// One open loot window: its rows (coin first when present). Pushed whole by the app; `None` means no
/// loot is open (the window is closed).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LootState {
    /// The window's **fixed** slot list: positions never shift while the window is open. A looted
    /// slot becomes `None` — still counted by `GetNumLootItems` (the reference's slot array keeps
    /// cleared slots; `LootFrame.numLootItems` is captured once at `OnShow`, l.132) but neither
    /// `LootSlotIsItem` nor `LootSlotIsCoin`, which is exactly how the reference's
    /// `LootFrame_Update` leaves a cleared slot's button hidden in place (`LootFrame.lua:80`)
    /// instead of collapsing the rows below it upward.
    pub rows: Vec<Option<LootRow>>,
    /// Whether this loot came from fishing (`IsFishingLoot()` — the wire `SMSG_LOOT_RESPONSE`
    /// `loot_type == 3`, decision 1086). `LootFrame_OnShow` keys the "FISHING REEL IN" sound and
    /// the FishingLoot portrait overlay on it (`LootFrame.lua:137-140`).
    pub fishing: bool,
    /// The master-loot candidate slots — what `GetMasterLootCandidate(i)` answers for a 1-based
    /// `i` (decision 1675). Empty unless the group's loot method is master loot and the server
    /// sent `SMSG_LOOT_MASTER_LIST` for this window.
    ///
    /// **A `None` is a real slot that answers nil**, not padding, and the list is deliberately
    /// not packed: in a raid the client files each candidate into its own subgroup's five-slot
    /// block, so index `i` carries which raid group the candidate is in. That is what lets
    /// `GroupLootDropDown_Initialize` walk `1..40` in blocks of five and label each block
    /// "Group N" (`LootFrame.lua:186-212`). A `None` also covers a candidate whose name has not
    /// resolved yet — the binding pushes nil for that too, and `UPDATE_MASTER_LOOT_LIST` exists
    /// to repaint the menu when it lands.
    ///
    /// Names, not guids: the seam's standing division is that Lua speaks names and 1-based
    /// indices while the app owns guids and wire slots, and `GroupLootDropDown_Initialize` puts
    /// this string straight into `info.text` (`LootFrame.lua:181`/`:229`).
    pub master_candidates: Vec<Option<String>>,
}

impl super::UiScript {
    /// Push (or clear, with `None`) the open loot's row snapshot.
    pub fn set_loot(&mut self, state: Option<LootState>) {
        self.model_mut().loot = state;
    }

    /// Drain the 1-based row indices queued by `LootSlot` since the last call. The app maps each to
    /// either the coin (money) intent or the item's wire loot slot.
    pub fn take_loot_picks(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().loot_picks)
    }

    /// Whether `CloseLoot` was called since the last drain (and clear the flag). The app maps this to
    /// `CMSG_LOOT_RELEASE` when a loot is open.
    pub fn take_loot_close(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().loot_close)
    }

    /// Drain the `GiveMasterLoot(slot, candidateIndex)` assignments queued since the last call —
    /// both 1-based display numbers. The app resolves the row to its wire slot and the candidate
    /// to a guid, then sends `CMSG_LOOT_MASTER_GIVE` (decision 1675).
    pub fn take_loot_master_gives(&mut self) -> Vec<(u32, u32)> {
        std::mem::take(&mut self.model_mut().loot_master_gives)
    }
}

/// Register the loot globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetNumLootItems() → the number of slots the open loot has, cleared ones included (0 when none
    // is open). Constant while a window is open — the reference reads it once at OnShow (l.132) and
    // never again; ours is invariant by construction, so the live read is the same value.
    g.set(
        "GetNumLootItems",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.loot.as_ref().map_or(0, |l| l.rows.len()) as i64)
        })?,
    )?;

    // GetLootSlotInfo(slot) → texture, item, quantity, quality (the Era flat-tuple shape,
    // `LootFrame.lua:81`). `slot` is 1-based; out of range → nil, and so is a cleared (already
    // looted) slot. `item`/`quality` are nil while the item-template query is in flight; `texture`
    // rides the wire display id, so it's there at once.
    g.set(
        "GetLootSlotInfo",
        lua.create_function(|lua, slot: usize| {
            let row = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model
                    .loot
                    .as_ref()
                    .and_then(|l| slot.checked_sub(1).and_then(|n| l.rows.get(n)))
                    .and_then(|r| r.clone())
            };
            let Some(row) = row else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let texture = match &row.texture {
                Some(t) => Value::String(lua.create_string(t)?),
                None => Value::Nil,
            };
            let item = match &row.name {
                Some(n) => Value::String(lua.create_string(n)?),
                None => Value::Nil,
            };
            let quality = match row.quality {
                Some(q) => Value::Integer(i64::from(q)),
                None => Value::Nil,
            };
            Ok(MultiValue::from_vec(vec![
                texture,
                item,
                Value::Integer(i64::from(row.quantity)),
                quality,
                // Benilla extension (5th): the item id, the shared tooltip store's key.
                Value::Integer(i64::from(row.item_id)),
            ]))
        })?,
    )?;

    // GetLootSlotLink(slot) → the row's full escaped `|cff…|Hitem:…|h[Name]|h|r` link | nil. 1-based
    // like GetLootSlotInfo beside it; nil out of range, nil for the coin row, and nil while the
    // item-template query is in flight (the link embeds the name). The reference's row click reads
    // it for BOTH modifier arms — `DressUpItemLink(GetLootSlotLink(this.slot))` (`LootFrame.lua:149`)
    // and `ChatFrameEditBox:Insert(GetLootSlotLink(this.slot))` (`:152`); ours routes the second
    // through `BenillaChatEdit_InsertLink`, whose whole job is the nil this getter can answer.
    // Decision 1059.
    g.set(
        "GetLootSlotLink",
        lua.create_function(|lua, slot: usize| {
            let link = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model
                    .loot
                    .as_ref()
                    .and_then(|l| slot.checked_sub(1).and_then(|n| l.rows.get(n)))
                    .and_then(|r| r.as_ref()?.link.clone())
            };
            match link {
                Some(link) => Ok(Value::String(lua.create_string(&link)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // LootSlotIsItem(slot) → true for a real item row (in range, not the coin pile, not cleared).
    g.set(
        "LootSlotIsItem",
        lua.create_function(|lua, slot: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(slot
                .checked_sub(1)
                .and_then(|n| model.loot.as_ref()?.rows.get(n)?.as_ref())
                .is_some_and(|r| !r.is_coin))
        })?,
    )?;

    // LootSlotIsCoin(slot) → true for the synthesized coin pile row (false once it's been looted —
    // the cleared slot stays in the count but answers neither predicate, `LootFrame.lua:80`).
    g.set(
        "LootSlotIsCoin",
        lua.create_function(|lua, slot: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(slot
                .checked_sub(1)
                .and_then(|n| model.loot.as_ref()?.rows.get(n)?.as_ref())
                .is_some_and(|r| r.is_coin))
        })?,
    )?;

    // IsFishingLoot() → whether the open loot came from fishing (false when none is open).
    // `LootFrame_OnShow` keys the reel-in sound + the fishing portrait overlay on it
    // (`LootFrame.lua:137-140`; decision 1086).
    g.set(
        "IsFishingLoot",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.loot.as_ref().is_some_and(|l| l.fishing))
        })?,
    )?;

    // LootSlot(slot) — queue the 1-based row pick; the app maps it to the coin or the item wire slot.
    g.set(
        "LootSlot",
        lua.create_function(|lua, slot: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.loot_picks.push(slot);
            Ok(())
        })?,
    )?;

    // CloseLoot([failed]) — flag the release intent. The optional arg (the client's "couldn't open
    // the UI" signal, `LootFrame.lua:18`) is accepted and ignored; the app decides whether to send a
    // release (only when a loot is actually open).
    g.set(
        "CloseLoot",
        lua.create_function(|lua, _args: MultiValue| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.loot_close = true;
            Ok(())
        })?,
    )?;

    // GetMasterLootCandidate(index) → the 1-based candidate's NAME, or nil past the end.
    //
    // `GroupLootDropDown_Initialize` probes this sparsely and relies entirely on the nil:
    // the party arm walks `1..MAX_PARTY_MEMBERS+1` and the raid arm walks `1..40` in blocks of
    // five, keeping a "Group N" submenu only where the block's first probe answered non-nil
    // (`LootFrame.lua:169-232`). So an out-of-range index must answer nil rather than raise, and
    // the list must be DENSE — a hole would silently drop a candidate from the menu.
    g.set(
        "GetMasterLootCandidate",
        lua.create_function(|lua, index: u32| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let name = index.checked_sub(1).and_then(|i| {
                model
                    .loot
                    .as_ref()?
                    .master_candidates
                    .get(i as usize)?
                    .clone()
            });
            Ok(name)
        })?,
    )?;

    // GiveMasterLoot(slot, candidateIndex) — queue the assignment; the app owns the wire slot and
    // the recipient guid. Called from the dropdown's own handler for a below-threshold item and
    // from the CONFIRM_LOOT_DISTRIBUTION popup's OnAccept above it (`LootFrame.lua:236-244`,
    // `StaticPopup.lua:85-94`).
    g.set(
        "GiveMasterLoot",
        lua.create_function(|lua, (slot, candidate): (u32, u32)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.loot_master_gives.push((slot, candidate));
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LootRow, LootState};
    use crate::script::UiScript;

    fn loot() -> LootState {
        LootState {
            master_candidates: Vec::new(),
            rows: vec![
                // The coin pile, always first. No link: there is no item to link (decision 1059).
                Some(LootRow {
                    item_id: 0,
                    name: Some("1g 23s 45c".into()),
                    texture: Some("Interface\\Icons\\INV_Misc_Coin_01".into()),
                    quantity: 1,
                    quality: Some(1),
                    is_coin: true,
                    link: None,
                    random_property_id: 0,
                }),
                // A resolved item — name, quality AND link all landed together (one template answer).
                Some(LootRow {
                    item_id: 0,
                    name: Some("Wool Cloth".into()),
                    texture: Some("Interface\\Icons\\INV_Fabric_Wool_01".into()),
                    quantity: 3,
                    quality: Some(1),
                    is_coin: false,
                    link: Some("|cffffffff|Hitem:2589:0:0:0|h[Wool Cloth]|h|r".into()),
                    random_property_id: 0,
                }),
                // An in-flight item: the loot arrived, the item-template answer hasn't.
                Some(LootRow {
                    item_id: 0,
                    name: None,
                    texture: Some("Interface\\Icons\\INV_Misc_QuestionMark".into()),
                    quantity: 1,
                    quality: None,
                    is_coin: false,
                    link: None,
                    random_property_id: 0,
                }),
            ],
            fishing: false,
        }
    }

    #[test]
    fn loot_snapshot_reads() {
        let mut s = UiScript::new().unwrap();
        // No loot open: count 0, info nil.
        assert_eq!(s.eval::<i64>("return GetNumLootItems()").unwrap(), 0);
        assert!(s.eval::<bool>("return GetLootSlotInfo(1) == nil").unwrap());

        s.set_loot(Some(loot()));
        assert_eq!(s.eval::<i64>("return GetNumLootItems()").unwrap(), 3);

        // Row 1: the coin pile — IsCoin true, IsItem false, its text the money amount.
        assert!(s.eval::<bool>("return LootSlotIsCoin(1)").unwrap());
        assert!(s.eval::<bool>("return not LootSlotIsItem(1)").unwrap());
        let (texture, item, quantity, quality) = s
            .eval::<(String, String, i64, i64)>("return GetLootSlotInfo(1)")
            .unwrap();
        assert_eq!(texture, "Interface\\Icons\\INV_Misc_Coin_01");
        assert_eq!(item, "1g 23s 45c");
        assert_eq!((quantity, quality), (1, 1));

        // Row 2: a resolved item — IsItem true, IsCoin false, quantity + quality present.
        assert!(s.eval::<bool>("return LootSlotIsItem(2)").unwrap());
        assert!(s.eval::<bool>("return not LootSlotIsCoin(2)").unwrap());
        let (name, qty) = s
            .eval::<(String, i64)>("local _, i, q = GetLootSlotInfo(2)\nreturn i, q")
            .unwrap();
        assert_eq!((name.as_str(), qty), ("Wool Cloth", 3));

        // Row 3: in flight — item name + quality nil, texture + quantity still present.
        assert!(s
            .eval::<bool>(
                "local t, i, q, ql = GetLootSlotInfo(3)\n\
                 return i == nil and ql == nil and t ~= nil and q == 1",
            )
            .unwrap());

        // GetLootSlotLink: the resolved item's link, nil for the coin row and for the in-flight one
        // (both arms of the reference's row click hand this straight on — decision 1059).
        assert_eq!(
            s.eval::<String>("return GetLootSlotLink(2)").unwrap(),
            "|cffffffff|Hitem:2589:0:0:0|h[Wool Cloth]|h|r"
        );
        assert!(s.eval::<bool>("return GetLootSlotLink(1) == nil").unwrap());
        assert!(s.eval::<bool>("return GetLootSlotLink(3) == nil").unwrap());

        // IsFishingLoot: false on an ordinary loot, true when the snapshot says fishing, false
        // again with no loot open (decision 1086).
        assert!(s.eval::<bool>("return not IsFishingLoot()").unwrap());
        let mut fished = loot();
        fished.fishing = true;
        s.set_loot(Some(fished));
        assert!(s.eval::<bool>("return IsFishingLoot()").unwrap());
        s.set_loot(Some(loot()));

        // Out of range → nil.
        assert!(s.eval::<bool>("return GetLootSlotInfo(9) == nil").unwrap());
        assert!(s.eval::<bool>("return GetLootSlotLink(9) == nil").unwrap());
        assert!(s.eval::<bool>("return not LootSlotIsItem(9)").unwrap());
        assert!(s.eval::<bool>("return not LootSlotIsCoin(9)").unwrap());

        // A CLEARED slot (looted): still counted — the slot list is fixed while the window is open,
        // the reference's own shape (`LootFrame.numLootItems` is read once at OnShow) — but it
        // answers neither predicate and nil info/link, so the frame hides that button in place
        // instead of collapsing the rows below it.
        let mut cleared = loot();
        cleared.rows[0] = None;
        s.set_loot(Some(cleared));
        assert_eq!(s.eval::<i64>("return GetNumLootItems()").unwrap(), 3);
        assert!(s.eval::<bool>("return GetLootSlotInfo(1) == nil").unwrap());
        assert!(s.eval::<bool>("return GetLootSlotLink(1) == nil").unwrap());
        assert!(s.eval::<bool>("return not LootSlotIsItem(1)").unwrap());
        assert!(s.eval::<bool>("return not LootSlotIsCoin(1)").unwrap());
        // …and the rows below keep their own slots.
        assert!(s.eval::<bool>("return LootSlotIsItem(2)").unwrap());
    }

    /// The master-loot half of the seam (decision 1675). `GetMasterLootCandidate` must answer nil
    /// past the end rather than raise — `GroupLootDropDown_Initialize` probes it sparsely (the
    /// raid arm walks 1..40 in blocks of five) and reads the nil as "nobody here".
    #[test]
    fn master_loot_candidates_read_1_based_and_nil_past_the_end() {
        let mut s = UiScript::new().unwrap();

        // No loot open at all: nil, not an error.
        assert!(s
            .eval::<bool>("return GetMasterLootCandidate(1) == nil")
            .unwrap());

        // A window with no candidate list (any loot method but master): still nil everywhere.
        s.set_loot(Some(loot()));
        assert!(s
            .eval::<bool>("return GetMasterLootCandidate(1) == nil")
            .unwrap());

        let mut ml = loot();
        ml.master_candidates = vec![Some("Thrall".into()), Some("Cairne".into())];
        s.set_loot(Some(ml));
        assert_eq!(
            s.eval::<String>("return GetMasterLootCandidate(1)")
                .unwrap(),
            "Thrall"
        );
        assert_eq!(
            s.eval::<String>("return GetMasterLootCandidate(2)")
                .unwrap(),
            "Cairne"
        );
        assert!(
            s.eval::<bool>("return GetMasterLootCandidate(3) == nil")
                .unwrap(),
            "past the end is nil"
        );
        assert!(
            s.eval::<bool>("return GetMasterLootCandidate(0) == nil")
                .unwrap(),
            "0 is not a Lua index"
        );
        // The raid arm's real sweep shape: 40 probes must all answer without raising.
        assert_eq!(
            s.eval::<i64>(
                "local n = 0
                 for i = 1, 40 do if GetMasterLootCandidate(i) then n = n + 1 end end
                 return n",
            )
            .unwrap(),
            2
        );

        // A HOLE in the middle answers nil without ending the list — the raid layout, where a
        // candidate's index carries which subgroup they are in, so slot 6 can be occupied while
        // slots 2-5 are empty. A list that stopped at the first nil would lose them.
        let mut raid = loot();
        raid.master_candidates = vec![
            Some("Thrall".into()),
            None,
            None,
            None,
            None,
            Some("Cairne".into()),
        ];
        s.set_loot(Some(raid));
        assert_eq!(
            s.eval::<String>("return GetMasterLootCandidate(1)")
                .unwrap(),
            "Thrall"
        );
        for hole in 2..=5 {
            assert!(
                s.eval::<bool>(&format!("return GetMasterLootCandidate({hole}) == nil"))
                    .unwrap(),
                "slot {hole} is a hole"
            );
        }
        assert_eq!(
            s.eval::<String>("return GetMasterLootCandidate(6)")
                .unwrap(),
            "Cairne",
            "an occupant past the holes is still reachable"
        );
    }

    #[test]
    fn give_master_loot_queues_the_assignment() {
        let mut s = UiScript::new().unwrap();
        let mut ml = loot();
        ml.master_candidates = vec![Some("Thrall".into())];
        s.set_loot(Some(ml));
        assert!(s.take_loot_master_gives().is_empty());
        s.run("GiveMasterLoot(2, 1)").unwrap();
        assert_eq!(s.take_loot_master_gives(), vec![(2, 1)]);
        assert!(s.take_loot_master_gives().is_empty(), "drained");
        // It is NOT a loot pick — the two intents must not cross wires.
        assert!(s.take_loot_picks().is_empty());
    }

    #[test]
    fn loot_slot_queues_picks() {
        let mut s = UiScript::new().unwrap();
        s.set_loot(Some(loot()));
        s.run("LootSlot(1)").unwrap(); // coin
        s.run("LootSlot(2)").unwrap(); // item
        assert_eq!(s.take_loot_picks(), vec![1, 2]);
        assert!(s.take_loot_picks().is_empty(), "drained");
    }

    #[test]
    fn close_loot_flags_the_intent() {
        let mut s = UiScript::new().unwrap();
        s.set_loot(Some(loot()));
        assert!(!s.take_loot_close());
        s.run("CloseLoot()").unwrap();
        assert!(s.take_loot_close());
        assert!(!s.take_loot_close(), "drained");
        // The client's failed-open form (CloseLoot(1)) is accepted and flags the same intent.
        s.run("CloseLoot(1)").unwrap();
        assert!(s.take_loot_close());
    }

    #[test]
    fn clearing_the_loot_empties_it() {
        let mut s = UiScript::new().unwrap();
        s.set_loot(Some(loot()));
        s.set_loot(None);
        assert_eq!(s.eval::<i64>("return GetNumLootItems()").unwrap(), 0);
        assert!(s.eval::<bool>("return GetLootSlotInfo(1) == nil").unwrap());
    }
}
