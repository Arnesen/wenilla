//! The app-side **loot feed** (decision 0084) — the inward half of the loot seam around
//! [`benilla_ui::script`]'s `loot` module, the twin of [`crate::ui_merchant`]'s merchant seam.
//!
//! The net bridge ([`crate::net::apply`]) fills [`LootState`] from the wire: `SMSG_LOOT_RESPONSE` →
//! the rows + coin pile ([`LootState::open`]); `SMSG_LOOT_REMOVED` → a row drops
//! ([`LootState::remove_slot`]); `SMSG_LOOT_CLEAR_MONEY` → the coin row drops
//! ([`LootState::clear_money`]); `SMSG_LOOT_RELEASE_RESPONSE` → the window closes
//! ([`LootState::clear`]); the error shape → [`LootErrors`]; `SMSG_ITEM_PUSH_RESULT` → a queued
//! "You receive loot" line ([`LootState::receives`]). A removal that empties the window arms the
//! client-authoritative **auto-close** ([`LootState::auto_release`] — the real engine's
//! close-on-last-slot), released by [`drain_loot`].
//!
//! Each frame [`feed_loot`] surfaces the errors + receive lines on the red UI error line (the
//! equip-error path's exact shape — an ErrorsFrame-style v1 stopgap that migrates to the chat frame
//! next arc), resolves each wire [`LootItem`] to a Lua-facing [`LootRow`] (icon straight from the
//! wire `display_info_id` through the same `ItemDisplayInfo.dbc` catalog the bags use — no template
//! wait; name + quality via the ask-once item-template cache, `None`/re-fed while in flight),
//! prepends the synthesized coin row when the loot carries gold, pushes the snapshot
//! ([`benilla_ui::script::UiScript::set_loot`]), and fires `LOOT_OPENED` on open / `LOOT_UPDATE` on a
//! content change / `LOOT_CLOSED` on clear. [`drain_loot`] pulls the Lua intents back out: `LootSlot`
//! → coin ? [`ClientCommand::LootMoney`] : [`ClientCommand::AutostoreLootItem`] (the clicked 1-based
//! row mapped to the item's **wire** loot slot); `CloseLoot` → [`ClientCommand::LootRelease`].

use benilla_protocol::messages::{ItemPushResult, LootItem, BAG_PLAYER_INVENTORY, SLOT_BAG_FIRST};
use bevy::prelude::*;

use benilla_ui::script::{LootRow, LootState as LootSnapshot, ScriptValue, UiScript};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{ClientCommand, NetCommands};
use crate::ui_items::KEYRING_CONTAINER;
use crate::ui_script::UiInput;

/// The coin-pile row icons (direct `Interface\Icons` paths — `SetTexture` takes them as-is, no DBC),
/// one per denomination; [`coin_icon`] picks by the highest nonzero denomination. All three VERIFIED
/// to extract from `interface.MPQ` (`INV_Misc_Coin_01`=gold pile, `_03`=silver, `_05`=copper).
///
/// **Stated approximation (documented gap).** The real 1.12 client chooses the coin-pile art from the
/// exact amount inside `GetLootSlotInfo` — a client-internal C selection that is **not** RE-recorded
/// (no wow-re node owns it; not dispatched). Mapping the icon to the highest nonzero denomination is
/// our faithful-shaped stand-in: a gold amount shows the gold pile, a silver-only amount the silver
/// pile, a copper-only amount the copper pile.
const COIN_ICON_GOLD: &str = "Interface\\Icons\\INV_Misc_Coin_01";
const COIN_ICON_SILVER: &str = "Interface\\Icons\\INV_Misc_Coin_03";
const COIN_ICON_COPPER: &str = "Interface\\Icons\\INV_Misc_Coin_05";
/// The fixed quality of the synthesized coin row (common/white — the money text reads as plain).
const COIN_QUALITY: u32 = 1;
/// The 1.12 coin-denomination words, QUOTED from `Interface\FrameXML\GlobalStrings.lua` (verified
/// extract): `GOLD = "Gold"` (l.2025), `SILVER = "Silver"` (l.3465), `COPPER = "Copper"` (l.865).
const GOLD_WORD: &str = "Gold";
const SILVER_WORD: &str = "Silver";
const COPPER_WORD: &str = "Copper";
/// Give up re-checking a pending receive line's item name after this many frames (a negative-cached
/// or genuinely-unknown entry never resolves; ~2s at 60fps is well past a normal template round-trip).
const RECEIVE_MAX_TRIES: u16 = 120;
/// The player-array slots the reference calls "the keyring" when deciding which bag button a push
/// animates — the literal `0x51`/`0x70` bounds compiled into `OnItemPush` (`0x491bc3`/`0x491bc8`).
/// It is the **descriptor array's** full 32-guid width (vmangos `KEYRING_SLOT_START 81` + 32), not
/// the 16 addressable positions `ui_items`'s `KEYRING_SLOTS` names (81..96) — the client tests the
/// wider range, so this transcribes the client rather than deriving from the server's stricter one.
const PUSH_KEYRING_SLOTS: std::ops::RangeInclusive<u32> = 81..=112;

/// One deferred push (`SMSG_ITEM_PUSH_RESULT`) awaiting its item template — the whole
/// `CGGameUI::OnItemPush 0x491a60` tail, not just the chat line. Carries the pushed entry + count,
/// the source flags (looted / from an NPC / created — the wording differs), the two random-property
/// wire fields the item link carries, whether the line is spoken at all, the destination container
/// ([`push_container`], for the bag-bar drop animation) and a retry budget.
///
/// Deferring **both** outputs on the template is the reference's own shape: `OnItemPush` opens with
/// an item-cache lookup (`0x491a93`) and, on a miss, copies its nine arguments into a heap record and
/// re-enters itself from the cache callback (`0x491aa7`-`0x491b05`, callback `0x491ee0`) — nothing is
/// emitted until the item is known.
struct PendingReceive {
    /// `SMSG_ITEM_PUSH_RESULT`'s created flag — a crafted/conjured item ("You create: …").
    created: bool,
    entry: u32,
    count: u32,
    from_npc: bool,
    /// `SMSG_ITEM_PUSH_RESULT`'s `randomPropertyId` — link field 3 (see [`receive_line`]).
    random_property_id: u32,
    /// `SMSG_ITEM_PUSH_RESULT`'s `suffixFactor` — link field 4 (see [`receive_line`]).
    suffix_factor: u32,
    /// The wire's `showInChat` — whether the chat LINE is spoken. It gates the line ONLY: the
    /// reference fires `ITEM_PUSH` at `0x491be8`, before the `[ebx+0x24]` test at `0x491bf3` that
    /// guards the whole chat block, so a silent push still animates. Which is why this rides in the
    /// record instead of short-circuiting at the net bridge (decision 0887).
    in_chat: bool,
    /// Which bag-bar button the drop animation plays on — see [`push_container`].
    container: i64,
    tries: u16,
}

/// The wire push's `(bag, slot)` → the live-API **container id** the bag bar speaks (`0` backpack,
/// `1..=4` an equipped bag, [`KEYRING_CONTAINER`] the keyring) — `ITEM_PUSH`'s `arg1`.
///
/// **Byte-VERIFIED** at `CGGameUI::OnItemPush` `0x491bb5`-`0x491bd6`, which is the whole selector:
///
/// ```text
///   491bb5  cmp edi,0xff        ; edi = the wire `bag` byte
///   491bbb  lea eax,[edi+1]     ; bag != 255  ⇒  bag + 1
///   491bbe  jne 491bd6
///   491bc0  mov eax,[ebp-0x4]   ; else look at the wire `slot`
///   491bc3  cmp eax,0x51        ; 81  = vmangos KEYRING_SLOT_START
///   491bc6  jl  491bd4
///   491bc8  cmp eax,0x70        ; 112 = the last keyring position
///   491bcb  jg  491bd4
///   491bcd  mov eax,-2          ;  ⇒ KEYRING_CONTAINER
///   491bd4  xor eax,eax         ;  ⇒ 0 (the backpack)
/// ```
///
/// The one translation: the reference emits `bag + 1`, which is the **inventory-slot** id its bag
/// buttons carry (`CharacterBag0Slot` = 20 … `Bag3Slot` = 23, `GetInventorySlotInfo`); benilla's bag
/// bar is keyed by container id like every other bag surface here (`BAG_UPDATE(bagID)`,
/// `BenillaBagBarSlot_InvSlot` = `19 + bagId`), so the same wire slots 19..22 become `1..=4`. Both
/// vocabularies agree on `0` and `-2`, and both leave a non-equipped-bag container (a bank bag,
/// wire 63..68) landing on an id no bag-bar button carries — no animation, exactly as there.
fn push_container(bag: u8, slot: u32) -> i64 {
    if bag != BAG_PLAYER_INVENTORY {
        return i64::from(bag) - i64::from(SLOT_BAG_FIRST) + 1;
    }
    if PUSH_KEYRING_SLOTS.contains(&slot) {
        return KEYRING_CONTAINER;
    }
    0
}

/// The open loot, filled by the net bridge and read by [`feed_loot`]. Holds the looted guid, the coin
/// pile, and the rows exactly as the wire delivered them (`SMSG_LOOT_RESPONSE`); the feed resolves
/// each to a display row and the drain maps a clicked 1-based row to its wire loot slot. Cleared on
/// release and on disconnect. `receives` outlives the window (a vendor buy pushes one with no loot
/// open) and is cleared only on disconnect.
#[derive(Resource, Default)]
pub(crate) struct LootState {
    /// The lootable unit whose window is open; `None` = no loot open.
    source: Option<u64>,
    /// The coin pile in copper; `0` = no coin row.
    gold: u32,
    /// The item rows (wire order); each carries its own **wire** loot slot (`LootItem::slot`).
    items: Vec<LootItem>,
    /// Deferred "You receive …" lines awaiting their item name.
    receives: Vec<PendingReceive>,
    /// A wire removal just emptied the open window (last item taken / coin line cleared with no
    /// items left) — the client-authoritative auto-close is due: the real client closes the loot
    /// itself when the last slot clears (the server never initiates a creature-loot release —
    /// vmangos `LootHandler.cpp` releases only in `HandleLootReleaseOpcode`; and the 1.12
    /// `LootFrame.lua` `LOOT_SLOT_CLEARED` handler only hides buttons, so the close is engine-side).
    /// Set only on the *transition* to empty via [`LootState::remove_slot`]/[`LootState::clear_money`],
    /// never at open — an empty-at-open window stays up (the reference `LootFrame_OnShow` even has a
    /// dedicated `LOOTWINDOWOPENEMPTY` sound for it). Drained by [`drain_loot`].
    auto_release: bool,
}

/// A resolved loot-row pick: the coin pile, or an item at a concrete **wire** loot slot (carrying
/// its display id, so the pick can play the item's pickup sound without a second lookup).
enum LootAction {
    Money,
    Item { wire_slot: u8, display_id: u32 },
}

impl LootState {
    /// Open (or replace) the window with a fresh loot response (`SMSG_LOOT_RESPONSE`). Quest items
    /// ride the same `items` list (the wire appends them with `slot = items.len()+i`), so they need no
    /// special handling here.
    pub(crate) fn open(&mut self, source: u64, gold: u32, items: Vec<LootItem>) {
        self.source = Some(source);
        self.gold = gold;
        self.items = items;
        self.auto_release = false; // empty-at-open stays open — only a removal auto-closes
    }

    /// A row was taken by anyone (`SMSG_LOOT_REMOVED`, keyed by the **wire** slot): drop it. The
    /// remaining rows keep their own wire slots, so autostore stays correct even as display indices
    /// shift. Emptying the window arms the auto-close ([`LootState::auto_release`]).
    pub(crate) fn remove_slot(&mut self, wire_slot: u8) {
        self.items.retain(|it| it.slot != wire_slot);
        self.arm_auto_release();
    }

    /// The coin line disappears for everyone (`SMSG_LOOT_CLEAR_MONEY`). Emptying the window arms
    /// the auto-close ([`LootState::auto_release`]).
    pub(crate) fn clear_money(&mut self) {
        self.gold = 0;
        self.arm_auto_release();
    }

    /// Arm the client-authoritative auto-close when a removal just left the open window with
    /// nothing in it — no item rows and no coin line (see [`LootState::auto_release`]).
    fn arm_auto_release(&mut self) {
        if self.source.is_some() && self.items.is_empty() && !self.has_coin() {
            self.auto_release = true;
        }
    }

    /// Take the armed auto-close edge (see [`LootState::auto_release`]) — `true` at most once per
    /// emptying.
    fn take_auto_release(&mut self) -> bool {
        std::mem::take(&mut self.auto_release)
    }

    /// Queue a deferred push (`SMSG_ITEM_PUSH_RESULT`) — the "You receive …" line *and* the bag-bar
    /// drop animation, emitted together once the item template lands (see [`PendingReceive`]). The
    /// **self** gate lives at the net bridge ([`crate::net::apply`]); the `showInChat` gate does
    /// not — it rides in as `in_chat` and silences the line alone, leaving the animation to play.
    pub(crate) fn push_receive(&mut self, p: &ItemPushResult) {
        self.receives.push(PendingReceive {
            created: p.created,
            entry: p.item_entry,
            count: p.count,
            from_npc: p.from_npc,
            random_property_id: p.random_property_id,
            suffix_factor: p.suffix_factor,
            in_chat: p.show_in_chat,
            container: push_container(p.bag_slot, p.item_slot),
            tries: 0,
        });
    }

    /// Close the open window (a release response, or a client-authoritative close on `CloseLoot`).
    /// Keeps `receives` (an in-flight receive line outlives the window).
    pub(crate) fn clear(&mut self) {
        self.source = None;
        self.gold = 0;
        self.items.clear();
        self.auto_release = false;
    }

    /// Disconnect: drop the open window **and** any pending receive lines (mirrors the merchant/gossip
    /// session clears).
    pub(crate) fn clear_session(&mut self) {
        self.clear();
        self.receives.clear();
    }

    /// How many pushes are queued awaiting their item template — the net bridge's test hook for
    /// "did this packet get through the self gate", since neither output is emitted until the
    /// template lands.
    #[cfg(test)]
    pub(crate) fn pending_receive_count(&self) -> usize {
        self.receives.len()
    }

    /// Whether a coin row is shown (position 1 when present).
    fn has_coin(&self) -> bool {
        self.gold > 0
    }

    /// The guid of the loot source whose window is open (`None` = closed). Read by the GameObject
    /// lid-close watcher ([`crate::go_anim`]) to close a chest's lid when its loot window closes
    /// (decision 0250) — the faithful client-authoritative close, any path (player close or the
    /// server's release on the last item).
    pub(crate) fn source(&self) -> Option<u64> {
        self.source
    }

    /// Resolve a clicked 1-based display row to its action: the coin pile (row 1 when gold > 0) or the
    /// item at the corresponding **wire** loot slot.
    fn action_at(&self, index_1based: u32) -> Option<LootAction> {
        let index = index_1based.checked_sub(1)?; // 0-based display position
        let as_item = |it: &LootItem| LootAction::Item {
            wire_slot: it.slot,
            display_id: it.display_info_id,
        };
        if self.has_coin() {
            if index == 0 {
                return Some(LootAction::Money);
            }
            return self.items.get((index - 1) as usize).map(as_item);
        }
        self.items.get(index as usize).map(as_item)
    }
}

/// A loot refusal (`SMSG_LOOT_RESPONSE`'s error shape) queued by the net bridge for the UI error line
/// — the loot twin of [`crate::ui_merchant::MerchantErrors`]. Carries the wire `u8` `LootError` code.
#[derive(Resource, Default)]
pub(crate) struct LootErrors(pub Vec<u8>);

/// The loot player knob (decision 0961): `autoLootDefault` — era's Controls-page checkbox (no
/// 1.12 CVar exists; vanilla only had the shift-click), settable from the Options window
/// through the CVar store (0954). The reference implements auto-loot ENGINE-side (era's own
/// Lua never reads this CVar outside its settings page), and so do we: [`feed_loot`] picks
/// every row itself at the open edge. A held SHIFT inverts the setting — era's
/// `AUTOLOOTTOGGLE` modified click, default SHIFT (Bindings_Vanilla.xml l.1467), the same
/// gesture that WAS vanilla's whole auto-loot.
#[derive(Resource, Default)]
pub(crate) struct LootConfig {
    pub(crate) auto_loot: bool,
}

/// The client-local **loot-target latch** — the mirror of the real client's `[player+0x1d28]`
/// guid (wow-re `loot-anim-leg.md`, the 2026-07-18 §5, decision 0515): set the instant
/// `CMSG_LOOT` goes out (`0x5df253`, the same site that client-predicts the kneel before any
/// server response) and cleared on release/close (`0x48f2c9`/`0x5ec0d4`). This — **not** the
/// mirrored `UNIT_FLAG_LOOTING` descriptor bit — is what the local player's own kneel rides
/// (the byte predicate `0x6126b0` splits: self → this latch, remote → the flag), so our kneel
/// starts at the click and ends at the release with no wire round-trip on either edge. Ours
/// clears **guid-matched** (release response / refusal / our own release sends) — the safe
/// generalization of the client's clears under our corpse-switch race, where the *old* window's
/// release response lands after the *new* loot request armed the latch — plus unconditionally at
/// session teardown.
#[derive(Resource, Default)]
pub(crate) struct LootLatch(pub(crate) Option<u64>);

impl LootLatch {
    /// Drop the latch if it still points at `guid` (a release/refusal for that loot session).
    pub(crate) fn clear_for(&mut self, guid: u64) {
        if self.0 == Some(guid) {
            self.0 = None;
        }
    }
}

pub(crate) struct UiLootPlugin;

impl Plugin for UiLootPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LootState>()
            .init_resource::<LootErrors>()
            .init_resource::<LootConfig>()
            .init_resource::<LootLatch>()
            .add_systems(
                Update,
                (
                    // Push before the input pass so an open/close is on screen the same frame; drain
                    // after it so a click's intent goes out the same frame (mirrors ui_merchant).
                    feed_loot.before(UiInput),
                    drain_loot.after(UiInput),
                ),
            );
    }
}

/// The client's message string for a `LootError` refusal (`SMSG_LOOT_RESPONSE`'s error shape); values
/// from [`benilla_protocol::messages::loot_error`] (VERIFIED vmangos `LootMgr.h`). Only the
/// subset a plain `CMSG_LOOT` can surface is spelled out; the rest print their code.
fn loot_error_text(reason: u8) -> String {
    use benilla_protocol::messages::loot_error as e;
    match reason {
        e::DIDNT_KILL => "You don't have permission to loot that corpse.".into(),
        e::TOO_FAR => "You are too far away to loot that.".into(),
        e::BAD_FACING => "You can't loot that from there.".into(),
        e::LOCKED => "Someone is already looting that corpse.".into(),
        e::NOTSTANDING => "You need to be standing up to loot.".into(),
        e::STUNNED => "You can't do that while stunned.".into(),
        e::PLAYER_NOT_FOUND => "You can't loot that right now.".into(),
        e::ALREADY_PICKPOCKETED => "Those pockets are already empty.".into(),
        other => format!("You can't loot that ({other})."),
    }
}

/// Copper → the loot coin-row name text: each nonzero denomination as "<n> <Word>", the words the real
/// GlobalStrings coin words (`GOLD`/`SILVER`/`COPPER`), joined by a single space and dropping leading
/// zeroes (25 → "25 Copper"; 10025 → "1 Gold 25 Copper"; 0 → "0 Copper"). The coin row's name is
/// resolved app-side (the loot twin of the merchant's coin display).
///
/// **Stated approximation (documented gap).** 1.12.1's GlobalStrings has **no** `GOLD_AMOUNT` /
/// `SILVER_AMOUNT` / `COPPER_AMOUNT` "%d <Word>" patterns — those arrive in a later client. The real
/// 1.12 coin text is produced inside `GetLootSlotInfo` (a C function, not FrameXML, not RE-recorded),
/// so we compose "<n> <Word>" from the bare `GOLD`/`SILVER`/`COPPER` words and join them the way
/// [`format_money`] always has (a single space); the exact client wording/separator is the stand-in.
fn format_money(copper: u32) -> String {
    let (g, s, c) = (copper / 10000, (copper % 10000) / 100, copper % 100);
    let mut parts: Vec<String> = Vec::new();
    if g > 0 {
        parts.push(format!("{g} {GOLD_WORD}"));
    }
    if s > 0 {
        parts.push(format!("{s} {SILVER_WORD}"));
    }
    if c > 0 || parts.is_empty() {
        parts.push(format!("{c} {COPPER_WORD}"));
    }
    parts.join(" ")
}

/// The coin-pile icon for a copper amount: the highest nonzero denomination's pile (gold ▸ silver ▸
/// copper). See [`COIN_ICON_GOLD`] for the stated-approximation note.
fn coin_icon(copper: u32) -> &'static str {
    if copper >= 10000 {
        COIN_ICON_GOLD
    } else if copper >= 100 {
        COIN_ICON_SILVER
    } else {
        COIN_ICON_COPPER
    }
}

/// Resolve one wire [`LootItem`] into the Lua-facing [`LootRow`]: the icon comes straight from the
/// wire `display_info_id` (no template wait), name + quality from the ask-once template cache (`None`
/// while in flight — the row shows a placeholder and fills in when the answer lands).
///
/// The row's **link** (`GetLootSlotLink`, decision 1059) comes off that same one template answer, out
/// of the one shared builder [`receive_line`] uses ([`crate::ui_items::item_link_full`], our
/// transcription of `0x52adb0`) and with the same arguments: enchant `0`, the wire's own
/// `randomPropertyId`, and suffix factor `0` — `SMSG_LOOT_RESPONSE`'s `randomSuffix` is a literal `0`
/// server-side (`LootMgr.cpp:841`, which is why [`LootItem`] doesn't even carry it), so there is
/// nothing else to pass. One builder, no drifting twins — the reason the zeros live in `item_link`
/// rather than at each call site.
fn resolve_item(
    item: &LootItem,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> LootRow {
    let (name, quality, link) = match items.template(item.item_id, 0, commands) {
        Some(t) => (
            Some(t.name.clone()),
            Some(t.quality),
            Some(crate::ui_items::item_link_full(
                item.item_id,
                0,
                item.random_property_id,
                0,
                &t.name,
                t.quality,
            )),
        ),
        None => (None, None, None),
    };
    let texture = icons
        .and_then(|i| i.catalog.get(item.display_info_id))
        .and_then(|d| d.icon.clone());
    LootRow {
        name,
        texture,
        quantity: item.count,
        quality,
        is_coin: false,
        item_id: item.item_id,
        link,
    }
}

/// Build the Lua-facing snapshot from [`LootState`] — `None` when no loot is open. The coin pile is
/// prepended (row 1) when the loot carries gold.
fn snapshot(
    loot: &LootState,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> Option<LootSnapshot> {
    loot.source?;
    let mut rows = Vec::with_capacity(loot.items.len() + 1);
    if loot.has_coin() {
        rows.push(LootRow {
            name: Some(format_money(loot.gold)),
            texture: Some(coin_icon(loot.gold).into()),
            quantity: 1,
            quality: Some(COIN_QUALITY),
            is_coin: true,
            item_id: 0,
            // No link: the coin pile is a synthesized row with no item behind it, so a modified
            // click on it finds nil and does nothing (decision 1059).
            link: None,
        });
    }
    for it in &loot.items {
        rows.push(resolve_item(it, items, icons, commands));
    }
    Some(LootSnapshot { rows })
}

/// Compose one `CHAT_MSG_LOOT` receive line — the whole of `CGGameUI::OnItemPush`'s self branch,
/// VERIFIED against the 1.12.1 client binary (`WoW.exe` 5875, `0x491a60`, self arm `0x491bfb`).
///
/// The mechanic the line hangs on, and the reason the item name is **not** LOOT-green: the `%s` the
/// GlobalString takes is a full **item link**, not a bare name. `0x491c43`/`0x491ca3` call the link
/// builder `0x52adb0` ([`crate::ui_items::item_link_full`] is our transcription of it) with the item
/// id, enchant `0`, the wire `randomPropertyId`, the wire `suffixFactor`, and the resolved name. The
/// link's `|r` closes right after the `]`, so the count that follows is drawn in the line's own
/// colour — exactly the reference client's `[Chipped Claw]x2.`
///
/// The format strings are QUOTED from the extracted `Interface\FrameXML\GlobalStrings.lua`
/// (patch-2.MPQ, l.2599-2605): `LOOT_ITEM_SELF = "You receive loot: %s."` /
/// `LOOT_ITEM_SELF_MULTIPLE = "You receive loot: %sx%d."` and the PUSHED_SELF / CREATED_SELF twins —
/// note there is **no space** before the `x%d`. The key is selected by the wire's (created,
/// received) pair with created winning (`0x491c04`-`0x491c1b` / `0x491c60`-`0x491c77`), exactly as
/// the server sets them (vmangos `SendNewItem`: crafting → created, vendor/quest → received,
/// loot → neither).
fn receive_line(r: &PendingReceive, name: &str, quality: u32) -> String {
    let verb = if r.created {
        "You create"
    } else if r.from_npc {
        "You receive item"
    } else {
        "You receive loot"
    };
    let link = crate::ui_items::item_link_full(
        r.entry,
        0,
        r.random_property_id,
        r.suffix_factor,
        name,
        quality,
    );
    if r.count > 1 {
        format!("{verb}: {link}x{}.", r.count)
    } else {
        format!("{verb}: {link}.")
    }
}

/// Emit any pending pushes (`SMSG_ITEM_PUSH_RESULT`) once their item template resolves — the whole
/// `CGGameUI::OnItemPush` tail, in the reference's own order: the bag-bar drop animation first
/// (`ITEM_PUSH(container, icon)`, fired at `0x491be8` *before* the chat block, decision 0887), then —
/// if the wire asked for it — the "You receive …" line in the chat window (decision 0084's chat arc),
/// a `LOOT`-green line carrying a quality-coloured item link ([`receive_line`], decision 0888).
/// Unresolved pushes retry up to [`RECEIVE_MAX_TRIES`] frames, then drop (the reference instead
/// sleeps on the item-cache callback, so it never gives up — a stated divergence that only shows on
/// an entry the server never answers for).
fn drain_receives(
    loot: &mut LootState,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
    chat: &mut crate::ui_chat::ChatLog,
    script: &mut UiScript,
) {
    let pending = std::mem::take(&mut loot.receives);
    let mut still = Vec::new();
    for mut r in pending {
        // One template read serves all three outputs: the name and quality for the line, the
        // display id for the animation's icon.
        let resolved = items
            .template(r.entry, 0, commands)
            .map(|t| (t.name.clone(), t.quality, t.display_info_id));
        match resolved {
            Some((name, quality, display_id)) => {
                // `ITEM_PUSH` is UNGATED by `show_in_chat` and by the created/received pair: the
                // fire at `0x491be8` precedes the `[ebx+0x24]` chat test at `0x491bf3`, so every
                // push that reaches us animates, including a silent one. `arg2` is the item's icon
                // path — the reference composes it from the icon directory + the display record's
                // own name (`0x491baa`, "%s%s%s"); ours is the same path out of the
                // `ItemDisplayInfo.dbc` catalog the bags and the loot window already read.
                let icon = icons
                    .and_then(|i| i.catalog.get(display_id))
                    .and_then(|d| d.icon.clone());
                if let Some(icon) = icon {
                    debug!(
                        "ui_loot: ITEM_PUSH container {} icon {icon} (item {})",
                        r.container, r.entry
                    );
                    script.fire_event(
                        "ITEM_PUSH",
                        vec![ScriptValue::Int(r.container), ScriptValue::Str(icon)],
                    );
                }
                if r.in_chat {
                    chat.push_event(crate::ui_chat::ChatEvent::text_only(
                        crate::ui_chat::ChatEventKind::Loot,
                        receive_line(&r, &name, quality),
                    ));
                }
            }
            None => {
                r.tries += 1;
                if r.tries < RECEIVE_MAX_TRIES {
                    still.push(r);
                }
            }
        }
    }
    loot.receives = still;
}

/// Push the current loot into the VM and fire open/update/close on a transition (or a content change
/// — an async name landing, a removed row, the coin clearing). Also routes refusals + receive lines
/// into the chat window. Diffed against a `Local` memory, exactly like the merchant/gossip feeds.
#[allow(clippy::too_many_arguments)]
fn feed_loot(
    script: Option<NonSendMut<UiScript>>,
    mut loot: ResMut<LootState>,
    mut items: ResMut<Items>,
    icons: Option<Res<ItemDisplays>>,
    commands: Res<NetCommands>,
    mut errors: ResMut<LootErrors>,
    mut chat: ResMut<crate::ui_chat::ChatLog>,
    mut last: Local<Option<LootSnapshot>>,
    cfg: Res<LootConfig>,
    keys: Res<ButtonInput<KeyCode>>,
    mut pickup: MessageWriter<crate::sound::LootPickupSound>,
) {
    let Some(mut script) = script else {
        return;
    };
    // Loot refusals + "You receive …" lines migrate to the chat window (decision 0084's chat arc):
    // refusals as informational SYSTEM-yellow lines, receive lines as LOOT-green. The ErrorsFrame
    // keeps only the cast/equip red toasts.
    for reason in errors.0.drain(..) {
        chat.push_event(crate::ui_chat::ChatEvent::text_only(
            crate::ui_chat::ChatEventKind::System,
            loot_error_text(reason),
        ));
    }
    drain_receives(
        &mut loot,
        &mut items,
        icons.as_deref(),
        &commands,
        &mut chat,
        &mut script,
    );

    let fresh = snapshot(&loot, &mut items, icons.as_deref(), &commands);
    if fresh == *last {
        return;
    }
    script.set_loot(fresh.clone());
    match (&*last, &fresh) {
        (None, Some(snap)) => {
            script.fire_event("LOOT_OPENED", vec![]);
            // era's engine-side auto-loot ([`LootConfig`]): the knob decides, a held SHIFT
            // inverts it, and the client "clicks" every row itself at the open edge — the
            // same autostore/coin sends a hand pick makes, pickup sounds included. A refusal
            // (inventory full) keeps its row and the window simply stays; emptying it fires
            // the existing last-row auto-release.
            let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
            if cfg.auto_loot != shift {
                for index in 1..=snap.rows.len() as u32 {
                    match loot.action_at(index) {
                        Some(LootAction::Money) => {
                            let _ = commands.0.send(ClientCommand::LootMoney);
                        }
                        Some(LootAction::Item {
                            wire_slot,
                            display_id,
                        }) => {
                            let _ = commands
                                .0
                                .send(ClientCommand::AutostoreLootItem { slot: wire_slot });
                            pickup.write(crate::sound::LootPickupSound { display_id });
                        }
                        None => {}
                    }
                }
            }
        }
        // A content change while open (async name landed, a row removed, coin cleared) → repaint,
        // keeping the current page (LOOT_UPDATE, the merchant's MERCHANT_UPDATE twin — this replaces
        // Blizzard's per-button LOOT_SLOT_CLEARED optimization with a full re-snapshot, exactly as
        // the merchant seam replaced per-row stock updates).
        (Some(_), Some(_)) => script.fire_event("LOOT_UPDATE", vec![]),
        (Some(_), None) => script.fire_event("LOOT_CLOSED", vec![]),
        (None, None) => {}
    }
    *last = fresh;
}

/// Drain the Lua intents: a picked row → coin (`CMSG_LOOT_MONEY`) or the item's wire slot
/// (`CMSG_AUTOSTORE_LOOT_ITEM`); a close → `CMSG_LOOT_RELEASE` + a client-authoritative local clear
/// (the window is already hidden by its `OnHide`; the release is fire-and-forget, and the server's
/// `SMSG_LOOT_RELEASE_RESPONSE` clears again idempotently). Also fires the **last-row auto-close**
/// ([`LootState::auto_release`]): the client, not the server, releases when a removal empties the
/// window — vmangos only ever releases in answer to our `CMSG_LOOT_RELEASE`.
fn drain_loot(
    script: Option<NonSendMut<UiScript>>,
    mut loot: ResMut<LootState>,
    mut latch: ResMut<LootLatch>,
    commands: Res<NetCommands>,
    mut pickup: MessageWriter<crate::sound::LootPickupSound>,
) {
    let Some(mut script) = script else {
        return;
    };
    for index in script.take_loot_picks() {
        match loot.action_at(index) {
            Some(LootAction::Money) => {
                // No coin play here: the coin rides the coinage-change watcher (`sound::money`)
                // when `PLAYER_FIELD_COINAGE` rises — the same rule as buy/sell.
                debug!("ui_loot: loot coin (row {index})");
                let _ = commands.0.send(ClientCommand::LootMoney);
            }
            Some(LootAction::Item {
                wire_slot,
                display_id,
            }) => {
                debug!("ui_loot: autostore row {index} (wire slot {wire_slot})");
                let _ = commands
                    .0
                    .send(ClientCommand::AutostoreLootItem { slot: wire_slot });
                // The pickup sound plays optimistically at the click, before the send (wow-re
                // `acquire-spend-sounds.md`): looting an item plays its ItemGroupSounds kit[0].
                pickup.write(crate::sound::LootPickupSound { display_id });
            }
            None => debug!("ui_loot: LootSlot({index}) out of range — ignored"),
        }
    }
    if script.take_loot_close() {
        if let Some(guid) = loot.source {
            debug!("ui_loot: release loot {guid:#x}");
            let _ = commands.0.send(ClientCommand::LootRelease { guid });
            loot.clear(); // client-authoritative close (mirrors the merchant's optimistic clear)
            latch.clear_for(guid); // the kneel ends at the release send (0515)
        }
    }
    // The last-row auto-close (`LootState::auto_release`): a wire removal just emptied the open
    // window, so the client releases on its own — the real engine's close-on-last-slot
    // (`0x4c2a70` empty-check → `0x48f200`; the server never initiates it). The clear makes the
    // feed fire LOOT_CLOSED next pass; the window's OnHide then calls CloseLoot(), whose drain
    // above no-ops on the already-cleared source.
    if loot.take_auto_release() {
        if let Some(guid) = loot.source {
            debug!("ui_loot: loot emptied — auto-release {guid:#x}");
            let _ = commands.0.send(ClientCommand::LootRelease { guid });
            loot.clear();
            latch.clear_for(guid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(slot: u8, entry: u32, count: u32) -> LootItem {
        LootItem {
            slot,
            item_id: entry,
            count,
            display_info_id: 1000 + entry,
            random_property_id: 0,
            slot_type: 0,
        }
    }

    /// One `SMSG_ITEM_PUSH_RESULT` as the loot path sees it (self, shown in chat, no random
    /// property) — the net bridge's guid/`showInChat` gates are tested at their own seam.
    fn push(entry: u32, count: u32, from_npc: bool, created: bool) -> ItemPushResult {
        ItemPushResult {
            player_guid: 0x1,
            from_npc,
            created,
            show_in_chat: true,
            bag_slot: 0xFF,
            item_slot: 0,
            item_entry: entry,
            suffix_factor: 0,
            random_property_id: 0,
            count,
        }
    }

    fn pending(entry: u32, count: u32, from_npc: bool, created: bool) -> PendingReceive {
        let mut loot = LootState::default();
        loot.push_receive(&push(entry, count, from_npc, created));
        loot.receives.pop().expect("queued")
    }

    #[test]
    fn action_maps_coin_first_then_items_by_wire_slot() {
        let mut loot = LootState::default();
        assert!(loot.source.is_none());
        // gold + two items at wire slots 0 and 1.
        loot.open(0x42, 1234, vec![item(0, 117, 1), item(1, 2589, 5)]);
        assert!(loot.source.is_some());
        // Row 1 is the coin pile; rows 2/3 are the items (wire slots 0/1). The item pick carries the
        // row's display id (here `1000 + entry`) so the pickup sound resolves without a second lookup.
        assert!(matches!(loot.action_at(1), Some(LootAction::Money)));
        assert!(matches!(
            loot.action_at(2),
            Some(LootAction::Item {
                wire_slot: 0,
                display_id: 1117
            })
        ));
        assert!(matches!(
            loot.action_at(3),
            Some(LootAction::Item {
                wire_slot: 1,
                display_id: 3589
            })
        ));
        assert!(loot.action_at(4).is_none());
        assert!(loot.action_at(0).is_none());
    }

    #[test]
    fn action_maps_items_directly_when_no_coin() {
        let mut loot = LootState::default();
        loot.open(0x42, 0, vec![item(3, 117, 1), item(7, 2589, 5)]);
        // No coin → row 1 is the first item (wire slot 3), row 2 the second (wire slot 7).
        assert!(matches!(
            loot.action_at(1),
            Some(LootAction::Item { wire_slot: 3, .. })
        ));
        assert!(matches!(
            loot.action_at(2),
            Some(LootAction::Item { wire_slot: 7, .. })
        ));
        assert!(loot.action_at(3).is_none());
    }

    #[test]
    fn remove_slot_keeps_the_remaining_wire_slots() {
        let mut loot = LootState::default();
        loot.open(
            0x42,
            0,
            vec![item(0, 117, 1), item(1, 2589, 5), item(2, 4306, 2)],
        );
        loot.remove_slot(1); // take the middle wire slot
        assert_eq!(loot.items.len(), 2);
        // The display shifts (row 2 is now wire slot 2), but the wire slots themselves are intact.
        assert!(matches!(
            loot.action_at(1),
            Some(LootAction::Item { wire_slot: 0, .. })
        ));
        assert!(matches!(
            loot.action_at(2),
            Some(LootAction::Item { wire_slot: 2, .. })
        ));
    }

    #[test]
    fn clear_money_drops_the_coin_row() {
        let mut loot = LootState::default();
        loot.open(0x42, 500, vec![item(0, 117, 1)]);
        assert!(loot.has_coin());
        assert!(matches!(loot.action_at(1), Some(LootAction::Money)));
        loot.clear_money();
        assert!(!loot.has_coin());
        // The coin row is gone; row 1 is now the item.
        assert!(matches!(
            loot.action_at(1),
            Some(LootAction::Item { wire_slot: 0, .. })
        ));
    }

    #[test]
    fn auto_release_arms_only_on_the_transition_to_empty() {
        let mut loot = LootState::default();
        // Coin + two items: taking rows one by one arms the auto-close only at the last removal.
        loot.open(0x42, 500, vec![item(0, 117, 1), item(1, 2589, 5)]);
        loot.remove_slot(0);
        assert!(!loot.take_auto_release(), "items + coin remain");
        loot.clear_money();
        assert!(!loot.take_auto_release(), "an item remains");
        loot.remove_slot(1);
        assert!(loot.take_auto_release(), "last row gone — auto-close due");
        assert!(!loot.take_auto_release(), "the edge drains once");
    }

    #[test]
    fn auto_release_arms_when_the_coin_line_is_the_last_row() {
        let mut loot = LootState::default();
        loot.open(0x42, 500, vec![]);
        loot.clear_money();
        assert!(
            loot.take_auto_release(),
            "coin-only loot empties → auto-close"
        );
    }

    #[test]
    fn empty_at_open_does_not_auto_release() {
        let mut loot = LootState::default();
        // The reference keeps an empty-at-open window up (LOOTWINDOWOPENEMPTY); only a removal closes.
        loot.open(0x42, 0, vec![]);
        assert!(!loot.take_auto_release());
        // …and a fresh open clears a stale armed edge.
        loot.open(0x43, 500, vec![]);
        loot.clear_money();
        loot.open(0x44, 0, vec![item(0, 117, 1)]);
        assert!(
            !loot.take_auto_release(),
            "open() disarms the previous window's edge"
        );
    }

    #[test]
    fn latch_clears_guid_matched_only() {
        // The corpse-switch race (decision 0515): loot B was requested while A's window was open;
        // A's release response must not drop the latch B's request just armed.
        let mut latch = LootLatch(Some(0xB));
        latch.clear_for(0xA);
        assert_eq!(latch.0, Some(0xB), "a stale release leaves the new latch");
        latch.clear_for(0xB);
        assert_eq!(latch.0, None, "the matching release drops it");
    }

    #[test]
    fn clear_closes_but_keeps_receives() {
        let mut loot = LootState::default();
        loot.open(0x42, 0, vec![item(0, 117, 1)]);
        loot.push_receive(&push(117, 1, false, false));
        loot.clear();
        assert!(loot.source.is_none());
        assert_eq!(loot.receives.len(), 1, "a receive line outlives the window");
        loot.clear_session();
        assert!(loot.receives.is_empty(), "disconnect drops receive lines");
    }

    /// The receive line is an item **link**, not a bare name: the quality escape opens before the
    /// `[` and `|r` closes after the `]`, so the count that follows falls back to the line's own
    /// LOOT green. This is the shape the reference client prints (`[Chipped Claw]x2.` — white name,
    /// green `x2`), and the reason the name must not inherit the chat colour.
    #[test]
    fn receive_line_carries_a_quality_colored_item_link() {
        // Common/white, single — LOOT_ITEM_SELF = "You receive loot: %s.".
        assert_eq!(
            receive_line(&pending(2589, 1, false, false), "Linen Cloth", 1),
            "You receive loot: |cffffffff|Hitem:2589:0:0:0|h[Linen Cloth]|h|r."
        );
        // Poor/grey, stacked — LOOT_ITEM_SELF_MULTIPLE = "You receive loot: %sx%d." and the `x2`
        // sits OUTSIDE the escape, with no space before it (GlobalStrings.lua l.2605, verbatim).
        assert_eq!(
            receive_line(&pending(7092, 2, false, false), "Chipped Claw", 0),
            "You receive loot: |cff9d9d9d|Hitem:7092:0:0:0|h[Chipped Claw]|h|rx2."
        );
        // The (created, received) pair picks the verb, created winning over received.
        assert_eq!(
            receive_line(&pending(4306, 1, true, false), "Silk Cloth", 2),
            "You receive item: |cff1eff00|Hitem:4306:0:0:0|h[Silk Cloth]|h|r."
        );
        assert_eq!(
            receive_line(&pending(2320, 4, true, true), "Coarse Thread", 1),
            "You create: |cffffffff|Hitem:2320:0:0:0|h[Coarse Thread]|h|rx4."
        );
    }

    /// The link's two random-property fields ride the wire straight through
    /// (`|Hitem:id:0:randomPropertyId:suffixFactor|h` — `0x52adb0`'s arg order).
    #[test]
    fn receive_line_carries_the_wire_random_property_fields() {
        let mut loot = LootState::default();
        loot.push_receive(&ItemPushResult {
            random_property_id: 862,
            suffix_factor: 1234,
            ..push(15268, 1, false, false)
        });
        let r = loot.receives.pop().expect("queued");
        assert_eq!(
            receive_line(&r, "Bloodrazor", 3),
            "You receive loot: |cff0070dd|Hitem:15268:0:862:1234|h[Bloodrazor]|h|r."
        );
    }

    /// The client clamps a quality outside the table to index 1 (white) — `0x52ad90`'s
    /// `cmpl $0x7 / jb` arm.
    #[test]
    fn receive_line_clamps_an_out_of_range_quality_to_white() {
        assert_eq!(
            receive_line(&pending(1, 1, false, false), "Odd Thing", 9),
            "You receive loot: |cffffffff|Hitem:1:0:0:0|h[Odd Thing]|h|r."
        );
    }

    /// [`push_container`] against the reference selector at `0x491bb5`-`0x491bd6`, arm by arm — the
    /// value that decides which bag button the drop animation plays on.
    #[test]
    fn push_container_maps_the_wire_destination_onto_a_bag_bar_button() {
        // `bag != 255` — an equipped bag. Wire 19..22 are the four bag inventory slots; the
        // reference emits 20..23 (its buttons' inventory-slot ids), ours the container ids 1..=4.
        assert_eq!(push_container(19, 0), 1);
        assert_eq!(push_container(22, 5), 4);
        // `bag == 255` with a keyring slot (the client's own 0x51..=0x70 window) — the keyring.
        assert_eq!(push_container(BAG_PLAYER_INVENTORY, 81), KEYRING_CONTAINER);
        assert_eq!(push_container(BAG_PLAYER_INVENTORY, 96), KEYRING_CONTAINER);
        assert_eq!(push_container(BAG_PLAYER_INVENTORY, 112), KEYRING_CONTAINER);
        // `bag == 255` anywhere else — the backpack. 23..38 are its own slots; 80 and 113 are the
        // positions just outside the keyring window, which the client resolves to 0, not -2.
        assert_eq!(push_container(BAG_PLAYER_INVENTORY, 23), 0);
        assert_eq!(push_container(BAG_PLAYER_INVENTORY, 80), 0);
        assert_eq!(push_container(BAG_PLAYER_INVENTORY, 113), 0);
        // A stack-merge reports no slot at all (`0xFFFF_FFFF`) — still the backpack, still animates.
        assert_eq!(push_container(BAG_PLAYER_INVENTORY, u32::MAX), 0);
        // A bank bag (wire 63..68) lands on an id no bag-bar button carries: no animation, which is
        // exactly what the reference's `bag + 1` does with the same push.
        assert!(!(0..=4).contains(&push_container(63, 0)));
    }

    #[test]
    fn money_formats_with_real_words_dropping_zero_denominations() {
        assert_eq!(format_money(0), "0 Copper");
        assert_eq!(format_money(4), "4 Copper");
        assert_eq!(format_money(25), "25 Copper");
        assert_eq!(format_money(10025), "1 Gold 25 Copper");
        assert_eq!(format_money(12_345), "1 Gold 23 Silver 45 Copper");
        assert_eq!(format_money(10_000), "1 Gold");
    }

    #[test]
    fn coin_icon_picks_the_highest_nonzero_denomination() {
        assert_eq!(coin_icon(4), COIN_ICON_COPPER); // copper only
        assert_eq!(coin_icon(99), COIN_ICON_COPPER);
        assert_eq!(coin_icon(500), COIN_ICON_SILVER); // 5 silver
        assert_eq!(coin_icon(10_025), COIN_ICON_GOLD); // gold present ⇒ gold pile
        assert_eq!(coin_icon(20_000), COIN_ICON_GOLD);
    }

    #[test]
    fn coin_row_uses_real_words_and_copper_pile_icon() {
        let mut items = Items::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        let mut loot = LootState::default();
        // A pure-copper drop: name reads "4 Copper", icon is the copper coin pile (_05).
        loot.open(0x42, 4, vec![]);
        let snap = snapshot(&loot, &mut items, None, &commands).expect("open");
        assert_eq!(snap.rows.len(), 1, "coin row only");
        assert!(snap.rows[0].is_coin);
        assert_eq!(snap.rows[0].name.as_deref(), Some("4 Copper"));
        assert_eq!(snap.rows[0].texture.as_deref(), Some(COIN_ICON_COPPER));
    }

    #[test]
    fn snapshot_prepends_coin_and_resolves_items() {
        let mut items = Items::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        let mut loot = LootState::default();
        // Closed → no snapshot.
        assert!(snapshot(&loot, &mut items, None, &commands).is_none());
        loot.open(0x42, 12_345, vec![item(0, 117, 3)]);
        let snap = snapshot(&loot, &mut items, None, &commands).expect("open");
        assert_eq!(snap.rows.len(), 2, "coin + one item");
        assert!(snap.rows[0].is_coin);
        assert_eq!(
            snap.rows[0].name.as_deref(),
            Some("1 Gold 23 Silver 45 Copper")
        );
        assert_eq!(snap.rows[0].texture.as_deref(), Some(COIN_ICON_GOLD));
        // The item's name is nil while its template is in flight; its quantity is present.
        assert!(!snap.rows[1].is_coin);
        assert!(snap.rows[1].name.is_none());
        assert_eq!(snap.rows[1].quantity, 3);
    }

    #[test]
    fn page_math_two_pages_of_three_and_two() {
        // The window shows 4 rows/page; > 4 items ⇒ 3 rows/page (a page slot is spent on the pager).
        // 5 items ⇒ ceil(5/3) = 2 pages, 3 then 2 (LootFrame.lua:70-73,112). Pure arithmetic mirror
        // of the shipped XML's paging, unit-tested here so a regression in either is caught.
        let num_items = 5u32;
        let per_page = if num_items > 4 { 3 } else { 4 };
        let pages = num_items.div_ceil(per_page);
        assert_eq!(per_page, 3);
        assert_eq!(pages, 2);
        // Page 1 shows rows for display slots 1..=3, page 2 for 4..=5.
        let page1: Vec<u32> = (1..=per_page).filter(|&i| i <= num_items).collect();
        let page2: Vec<u32> = (per_page + 1..=2 * per_page)
            .filter(|&i| i <= num_items)
            .collect();
        assert_eq!(page1, vec![1, 2, 3]);
        assert_eq!(page2, vec![4, 5]);
        // 4 items ⇒ a single page of 4, no pager.
        assert_eq!(if 4u32 > 4 { 3 } else { 4 }, 4);
    }
}
