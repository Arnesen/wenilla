//! Loot messages — the solo-loot wire family (opcodes 264, 349-355, 357-358; VERIFIED vmangos
//! `Server/Protocol/Opcodes_1_12_1.h:267,350-359`). Bodies from vmangos `Server/Packets/Loot.{h,cpp}`
//! (the light client-read / server-append shapes) + the hand-serialized `LootMgr.cpp`
//! (`operator<<(LootItem)`/`operator<<(LootView)`) and `Player.cpp` (`SendLoot`/`SendLootError`).
//! The group-roll family (opcodes 670-674 — `SMSG_LOOT_START_ROLL`/`SMSG_LOOT_ROLL`/
//! `SMSG_LOOT_ROLL_WON`/`SMSG_LOOT_ALL_PASSED` + our `CMSG_LOOT_ROLL`) rides here too, from
//! vmangos `Group/Group.cpp`'s four senders (decision 0591). Master loot (`CMSG_LOOT_MASTER_GIVE`
//! 675 / `SMSG_LOOT_MASTER_LIST` 676) remains out of scope.

use std::io;

use crate::wire::{read_u32_le, read_u64_le, read_u8};

/// One item row on the normal shape of `SMSG_LOOT_RESPONSE` (vmangos `LootMgr.cpp:837-845` the
/// `LootItem` write, wrapped by the slot index + trailing slot-type byte the caller appends around
/// it — `LootMgr.cpp:900-912`, `ALL_PERMISSION` branch, the solo-loot path). `slot` is the wire's
/// 0-based loot-window slot — what [`autostore_loot_item`] addresses back; a quest item rides the
/// same list with `slot = items.len() + i` (`LootMgr.cpp:970`, out of the solo drop-loot path but
/// parsed the same way if ever present). The wire's `randomSuffix` field is **not** a real per-item
/// value — the server always writes a literal `0` there (`LootMgr.cpp:841`) — so this parser reads
/// and discards it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootItem {
    pub slot: u8,
    pub item_id: u32,
    pub count: u32,
    /// `item_template.display_id` → icon via `ItemDisplayInfo.dbc`.
    pub display_info_id: u32,
    pub random_property_id: u32,
    /// A [`slot_type`] code. Solo paths (`ALL_PERMISSION`/`OWNER_PERMISSION`) always write
    /// `ALLOW_LOOT` (`LootMgr.cpp:918-926`); the others are group-loot only, out of scope for v1.
    pub slot_type: u8,
}

/// `LootSlotType` (VERIFIED vmangos `LootMgr.h:75-83`) — the `u8` riding after each [`LootItem`] on
/// `SMSG_LOOT_RESPONSE`.
pub mod slot_type {
    /// The player can loot the item.
    pub const ALLOW_LOOT: u8 = 0;
    /// A group roll is ongoing on this item — shown, not yet lootable.
    pub const ROLL_ONGOING: u8 = 1;
    /// Only the group's loot master can distribute this item.
    pub const MASTER: u8 = 2;
    /// Shown red — not lootable (a missing-requirement item in a group).
    pub const LOCKED: u8 = 3;
    /// Owner-permission solo looting, binding checks skipped.
    pub const OWNER: u8 = 4;
}

/// `LootType` (VERIFIED vmangos `LootMgr.h:49-61`) — the `u8` naming what's being looted on the
/// normal shape of `SMSG_LOOT_RESPONSE`. Only 4 values ever reach the wire: `Player::SendLoot`
/// (`Player.cpp:8117-8131`) remaps `SKINNING`(6)/`INSIGNIA`(22) to `PICKPOCKETING` and
/// `FISHING_HOLE`(20)/`FISHING_FAIL`(21) to `FISHING` before sending — the client has no separate
/// UI state for those. `CMSG_LOOT` (a corpse/creature loot request) always answers with `CORPSE`
/// (`LootHandler.cpp:340-354`, `HandleLootOpcode` → `SendLoot(guid, LOOT_CORPSE)` unconditionally);
/// the rest arrive only from other loot sources (pickpocket spell, fishing, disenchant), out of
/// scope for this slice but pinned for completeness.
pub mod loot_type {
    pub const CORPSE: u8 = 1;
    pub const PICKPOCKETING: u8 = 2;
    pub const FISHING: u8 = 3;
    pub const DISENCHANTING: u8 = 4;
}

/// `LootError` (VERIFIED vmangos `LootMgr.h:85-100`) — the `u8` on the error shape of
/// `SMSG_LOOT_RESPONSE` (`Player::SendLootError`, `Player.cpp:7744-7750`). A plain `CMSG_LOOT` on a
/// creature corpse can only surface a subset: `DIDNT_KILL` (guid isn't a lootable
/// creature/player/corpse, or the creature check fails — `LootHandler.cpp:342-346`,
/// `Player.cpp:7938-7943`), `PLAYER_NOT_FOUND` (dead/not-in-world — `LootHandler.cpp:349-353`),
/// `PLAY_TIME_EXCEEDED`, `NOTSTANDING`, `STUNNED` (`LootHandler.cpp:358-373`), and `TOO_FAR`
/// (range check in `Player.cpp:7938-7947`). The rest (`BAD_FACING`/`LOCKED`/the `MASTER_*`
/// trio/`ALREADY_PICKPOCKETED`/`NOT_WHILE_SHAPESHIFTED`) ride other loot sources or group/master
/// loot — out of scope for v1, pinned for completeness.
pub mod loot_error {
    pub const DIDNT_KILL: u8 = 0;
    pub const TOO_FAR: u8 = 4;
    pub const BAD_FACING: u8 = 5;
    pub const LOCKED: u8 = 6;
    pub const NOTSTANDING: u8 = 8;
    pub const STUNNED: u8 = 9;
    pub const PLAYER_NOT_FOUND: u8 = 10;
    pub const PLAY_TIME_EXCEEDED: u8 = 11;
    pub const MASTER_INV_FULL: u8 = 12;
    pub const MASTER_UNIQUE_ITEM: u8 = 13;
    pub const MASTER_OTHER: u8 = 14;
    pub const ALREADY_PICKPOCKETED: u8 = 15;
    pub const NOT_WHILE_SHAPESHIFTED: u8 = 16;
}

/// The two shapes `SMSG_LOOT_RESPONSE` can take, disambiguated by the `lootType` byte right after
/// the guid: `0` means "no window — this is an error", anything else is a real loot window.
#[derive(Debug, Clone, PartialEq)]
pub enum LootResponseBody {
    /// The normal shape (VERIFIED vmangos `Player.cpp:8135-8138` + `operator<<(LootView)`,
    /// `LootMgr.cpp:848-873`): `u32 gold, u8 itemCount`, then `itemCount` rows of [`LootItem`].
    Items {
        loot_type: u8,
        gold: u32,
        items: Vec<LootItem>,
    },
    /// The error shape (VERIFIED vmangos `Player::SendLootError`, `Player.cpp:7744-7750`): just
    /// the trailing `u8` error code, nothing else follows.
    Error { error: u8 },
}

/// Body of `CMSG_LOOT` (VERIFIED vmangos `Server/Packets/Loot.cpp:8-11`,
/// `LootUnit::ReadFromWorldPacket`): one full 8-byte guid — the corpse/creature/player being
/// opened. Always answered by `SMSG_LOOT_RESPONSE` (one shape or the other).
pub fn loot(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_AUTOSTORE_LOOT_ITEM` (VERIFIED vmangos `Server/Packets/Loot.cpp:3-6`,
/// `AutoStoreLootItem::ReadFromWorldPacket`): one `u8` — the 0-based wire loot slot (see
/// [`LootItem::slot`]). The server auto-places the item into the first free bag slot
/// (`LootHandler.cpp:183-206`, `CanStoreNewItem`/`StoreNewItem`); no specific-slot variant exists
/// for loot (unlike vendor buy).
pub fn autostore_loot_item(loot_slot: u8) -> Vec<u8> {
    vec![loot_slot]
}

/// Body of `CMSG_LOOT_MONEY` (VERIFIED vmangos `Server/Protocol/Opcodes_1_12_1.h:351` — the
/// handler `WorldSession::HandleLootMoneyOpcode` takes a `NullClientPacket`): empty.
pub fn loot_money() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_LOOT_RELEASE` (VERIFIED vmangos `Server/Packets/Loot.cpp:13-16`,
/// `LootRelease::ReadFromWorldPacket`): one full 8-byte guid, though the server ignores it and
/// releases whatever loot guid it has stored for us instead (`Server/Packets/Loot.h:31-36`: "not
/// used by server"; handler `HandleLootReleaseOpcode`, `LootHandler.cpp:383-389`).
pub fn loot_release(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// `RollVote` (VERIFIED vmangos `Group/Group.h:88-99`) — the `u8` vote a client may cast on
/// `CMSG_LOOT_ROLL`. The server hard-rejects anything `>= 3` (`MAX_ROLL_FROM_CLIENT`,
/// `GroupHandler.cpp:367-368`), so these three are the whole client-side vocabulary; `Group.h`'s
/// `ROLL_NOT_EMITED_YET`(3)/`ROLL_NOT_VALID`(4) are server bookkeeping that never reaches us.
///
/// These same values also ride *back* as `SMSG_LOOT_ROLL`/`SMSG_LOOT_ROLL_WON`'s `roll_type` —
/// but there they are **not** self-describing: see [`LootRoll::is_dice`] for the disambiguation.
pub mod roll_vote {
    pub const PASS: u8 = 0;
    pub const NEED: u8 = 1;
    pub const GREED: u8 = 2;
}

/// Body of `CMSG_LOOT_ROLL` (VERIFIED vmangos `Server/Packets/Loot.cpp:18-23`,
/// `LootRoll::ReadFromWorldPacket`): `u64 lootedTarget, u32 itemSlot, u8 rollType` — the corpse
/// guid + wire loot slot that identify *which* roll, and our [`roll_vote`]. The pair
/// `(lootedTarget, itemSlot)` is the roll's server-side identity; the client-side `rollID` the
/// FrameXML API passes around is ours alone and never reaches the wire.
pub fn loot_roll(looted_target: u64, item_slot: u32, roll_type: u8) -> Vec<u8> {
    let mut body = looted_target.to_le_bytes().to_vec();
    body.extend_from_slice(&item_slot.to_le_bytes());
    body.push(roll_type);
    body
}

/// `SMSG_LOOT_START_ROLL` (VERIFIED vmangos `Server/Packets/Loot.{h:78-90,cpp:43-51}`) — a group
/// roll opened on one drop; sent to every eligible roller. `countdown_ms` is the server's
/// `LOOT_ROLL_TIMEOUT`, a flat `1*MINUTE*IN_MILLISECONDS` = `60_000` at 5875
/// (`Group/Group.cpp:67`), but it is read from the wire rather than assumed.
///
/// The wire's `randomSuffix` field is a literal `0` the server never fills (`Group.cpp:764`), so
/// this parser reads and discards it — same treatment as [`LootItem`]'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootStartRoll {
    /// The corpse/container being looted — with `item_slot`, the roll's wire identity.
    pub looted_target: u64,
    /// The 0-based **wire** loot slot (the same vocabulary as [`LootItem::slot`], widened to `u32`
    /// here because this packet carries it as one).
    pub item_slot: u32,
    pub item_id: u32,
    pub random_property_id: u32,
    /// How long the roll stays open, in milliseconds.
    pub countdown_ms: u32,
}

/// `SMSG_LOOT_ROLL` (VERIFIED vmangos `Server/Packets/Loot.{h:92-106,cpp:53-63}`) — one
/// announcement about one roller, broadcast to every eligible roller.
///
/// **The `(roll_number, roll_type)` pair is overloaded** and this is the load-bearing subtlety of
/// the whole family. vmangos emits exactly four shapes (VERIFIED `Group/Group.cpp:970-990` for the
/// votes, `:1163`/`:1214` for the dice):
///
/// | `roll_number` | `roll_type` | meaning                                    |
/// |---------------|-------------|--------------------------------------------|
/// | `0`           | `0`         | that player **voted** Need                  |
/// | `128`         | `128`       | that player **voted** Pass                  |
/// | `128`         | `2`         | that player **voted** Greed                 |
/// | `1..=100`     | `1` or `2`  | that player's **dice result** (Need/Greed)  |
///
/// So `roll_type` alone cannot tell a Greed *vote* (`128, 2`) from a Greed *dice roll*
/// (`57, 2`) — `roll_number` is the discriminator. [`LootRoll::is_dice`] encodes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootRoll {
    pub looted_target: u64,
    pub item_slot: u32,
    /// The player this announcement is about — compare against our own guid for the "You …"
    /// phrasing.
    pub roller: u64,
    pub item_id: u32,
    pub random_property_id: u32,
    /// `1..=100` for a dice result; `0`/`128` for a vote announcement (see the type docs).
    pub roll_number: u8,
    /// A [`roll_vote`] value for a dice result; `0`/`2`/`128` for a vote (see the type docs).
    pub roll_type: u8,
}

impl LootRoll {
    /// Whether this is an actual **dice result** (`roll_number` in `1..=100`) rather than one of
    /// the three vote announcements (`roll_number` `0` or `128`). See the type docs for the table
    /// this encodes — vmangos only ever emits `urand(1, 100)` for real rolls (`Group.cpp:1162`,
    /// `:1213`), and only `0`/`128` for votes, so the ranges cannot collide.
    pub fn is_dice(&self) -> bool {
        (1..=100).contains(&self.roll_number)
    }

    /// The vote this announcement reports when it is *not* a dice result — a [`roll_vote`] value.
    /// `None` for a dice result (there `roll_type` is already a true [`roll_vote`]) or for a pair
    /// outside the four shapes vmangos emits.
    pub fn vote(&self) -> Option<u8> {
        match (self.roll_number, self.roll_type) {
            (0, 0) => Some(roll_vote::NEED),
            (128, 128) => Some(roll_vote::PASS),
            (128, roll_vote::GREED) => Some(roll_vote::GREED),
            _ => None,
        }
    }
}

/// `SMSG_LOOT_ROLL_WON` (VERIFIED vmangos `Server/Packets/Loot.{h:108-122,cpp:65-75}`) — the roll
/// resolved and `winner` took the item. Note the field order differs from [`LootRoll`]: the winner
/// guid lands *after* the item block, not before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootRollWon {
    pub looted_target: u64,
    pub item_slot: u32,
    pub item_id: u32,
    pub random_property_id: u32,
    pub winner: u64,
    /// The winning dice value (`1..=100`), or a literal `100` on the uncontested
    /// single-eligible-roller short-circuit (`Group.cpp:1110`, sent with `ROLL_NEED`).
    pub roll_number: u8,
    /// A [`roll_vote`] value — which kind of roll won.
    pub roll_type: u8,
}

/// `SMSG_LOOT_ALL_PASSED` (VERIFIED vmangos `Server/Packets/Loot.{h:124-134,cpp:77-84}`) — nobody
/// wanted it; the roll closes and the item returns to the corpse for ordinary looting.
///
/// **Field-order quirk:** unlike every other packet in this family, the append order here is
/// `itemRandomPropId` **then** `randomSuffixId` (`Loot.cpp:79-83`) — the two `u32`s are swapped
/// relative to [`LootStartRoll`]/[`LootRoll`]/[`LootRollWon`]. Both are effectively `0` in
/// practice (the server never fills `randomSuffixId`), so the swap is invisible on the wire today,
/// but the parser follows the source order rather than the family's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootAllPassed {
    pub looted_target: u64,
    pub item_slot: u32,
    pub item_id: u32,
    pub random_property_id: u32,
}

/// Read `SMSG_LOOT_START_ROLL` (see [`LootStartRoll`]).
pub(super) fn read_loot_start_roll(r: &mut &[u8]) -> io::Result<LootStartRoll> {
    let looted_target = read_u64_le(r)?;
    let item_slot = read_u32_le(r)?;
    let item_id = read_u32_le(r)?;
    let _random_suffix = read_u32_le(r)?; // literal 0 on the wire (Group.cpp:764)
    let random_property_id = read_u32_le(r)?;
    let countdown_ms = read_u32_le(r)?;
    Ok(LootStartRoll {
        looted_target,
        item_slot,
        item_id,
        random_property_id,
        countdown_ms,
    })
}

/// Read `SMSG_LOOT_ROLL` (see [`LootRoll`] — including the overloaded `(roll_number, roll_type)`).
pub(super) fn read_loot_roll(r: &mut &[u8]) -> io::Result<LootRoll> {
    let looted_target = read_u64_le(r)?;
    let item_slot = read_u32_le(r)?;
    let roller = read_u64_le(r)?;
    let item_id = read_u32_le(r)?;
    let _random_suffix = read_u32_le(r)?; // literal 0 on the wire (Group.cpp:794)
    let random_property_id = read_u32_le(r)?;
    let roll_number = read_u8(r)?;
    let roll_type = read_u8(r)?;
    Ok(LootRoll {
        looted_target,
        item_slot,
        roller,
        item_id,
        random_property_id,
        roll_number,
        roll_type,
    })
}

/// Read `SMSG_LOOT_ROLL_WON` (see [`LootRollWon`] — the winner guid follows the item block).
pub(super) fn read_loot_roll_won(r: &mut &[u8]) -> io::Result<LootRollWon> {
    let looted_target = read_u64_le(r)?;
    let item_slot = read_u32_le(r)?;
    let item_id = read_u32_le(r)?;
    let _random_suffix = read_u32_le(r)?; // literal 0 on the wire (Group.cpp:822)
    let random_property_id = read_u32_le(r)?;
    let winner = read_u64_le(r)?;
    let roll_number = read_u8(r)?;
    let roll_type = read_u8(r)?;
    Ok(LootRollWon {
        looted_target,
        item_slot,
        item_id,
        random_property_id,
        winner,
        roll_number,
        roll_type,
    })
}

/// Read `SMSG_LOOT_ALL_PASSED` (see [`LootAllPassed`] — note the swapped `u32` pair).
pub(super) fn read_loot_all_passed(r: &mut &[u8]) -> io::Result<LootAllPassed> {
    let looted_target = read_u64_le(r)?;
    let item_slot = read_u32_le(r)?;
    let item_id = read_u32_le(r)?;
    let random_property_id = read_u32_le(r)?;
    let _random_suffix = read_u32_le(r)?; // the swapped tail — a literal 0 (Group.cpp:849)
    Ok(LootAllPassed {
        looted_target,
        item_slot,
        item_id,
        random_property_id,
    })
}

/// Read `SMSG_LOOT_RESPONSE` — both shapes (see [`LootResponseBody`]). Wire order (VERIFIED
/// vmangos `Player.cpp:8135-8138`): `u64 guid, u8 lootType`, then either the error tail (`lootType
/// == 0`) or `u32 gold, u8 itemCount` + `itemCount` × [`LootItem`] rows (each `u8 slot, u32
/// itemid, u32 count, u32 displayInfoID, u32 0(randomSuffix, discarded), u32 randomPropertyId, u8
/// slotType` — `LootMgr.cpp:848-873,900-912`, the `ALL_PERMISSION` solo branch).
pub(super) fn read_loot_response(r: &mut &[u8]) -> io::Result<(u64, LootResponseBody)> {
    let guid = read_u64_le(r)?;
    let loot_type = read_u8(r)?;
    if loot_type == 0 {
        let error = read_u8(r)?;
        return Ok((guid, LootResponseBody::Error { error }));
    }
    let gold = read_u32_le(r)?;
    let count = read_u8(r)?;
    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let slot = read_u8(r)?;
        let item_id = read_u32_le(r)?;
        let item_count = read_u32_le(r)?;
        let display_info_id = read_u32_le(r)?;
        let _random_suffix = read_u32_le(r)?; // always a literal 0 on the wire (LootMgr.cpp:841)
        let random_property_id = read_u32_le(r)?;
        let slot_type = read_u8(r)?;
        items.push(LootItem {
            slot,
            item_id,
            count: item_count,
            display_info_id,
            random_property_id,
            slot_type,
        });
    }
    Ok((
        guid,
        LootResponseBody::Items {
            loot_type,
            gold,
            items,
        },
    ))
}

/// Read `SMSG_LOOT_RELEASE_RESPONSE` (VERIFIED vmangos `Server/Packets/Loot.h:137-145`,
/// `Player::SendLootRelease`, `Player.cpp:7736-7742`): `u64 guid, u8 result` — `result` is always
/// `1` (the struct's default; vmangos never sets it otherwise).
pub(super) fn read_loot_release_response(r: &mut &[u8]) -> io::Result<(u64, u8)> {
    Ok((read_u64_le(r)?, read_u8(r)?))
}

/// Read `SMSG_LOOT_REMOVED` (VERIFIED vmangos `Server/Packets/Loot.h:147-154`): one `u8` — the wire
/// loot slot that was just taken (by anyone; the row disappears from every current looter's
/// window, `Loot::NotifyItemRemoved`).
pub(super) fn read_loot_removed(r: &mut &[u8]) -> io::Result<u8> {
    read_u8(r)
}

/// Read `SMSG_LOOT_MONEY_NOTIFY` (VERIFIED vmangos `Server/Packets/Loot.h:69-76`): one `u32` — the
/// requester's share of the coin pile (an equal split among current looters; solo looting is the
/// degenerate 1-looter case, the requester's whole share).
pub(super) fn read_loot_money_notify(r: &mut &[u8]) -> io::Result<u32> {
    read_u32_le(r)
}

/// `SMSG_ITEM_PUSH_RESULT` (VERIFIED vmangos `Server/Packets/Item.h:324-345` +
/// `Item.cpp:211-224`; both `SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_10_2` conditionals evaluate
/// *included* for build 5875) — drives the "You receive loot: …" / "You receive item: …" chat
/// line. `item_slot` is `0xFFFF_FFFF` when the item stacked onto an existing slot instead of
/// landing in a fresh one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemPushResult {
    pub player_guid: u64,
    /// `false` = looted, `true` = received from an NPC (vendor purchase, quest reward).
    pub from_npc: bool,
    /// `true` = the item was CREATED (crafting's `EffectCreateItem`, a conjure aura — vmangos
    /// `SendNewItem`'s own comment "0=received, 1=created"; the only `created=true` senders are
    /// the create-item paths) → the client's "You create: …" line. NOT a stack-merge flag.
    pub created: bool,
    pub show_in_chat: bool,
    /// Which bag slot the item went to (`NULL_BAG`/`INVENTORY_SLOT_BAG_0`-style index — the same
    /// vocabulary as [`super::items::BAG_PLAYER_INVENTORY`]/[`super::items::SLOT_BAG_FIRST`]).
    pub bag_slot: u8,
    pub item_slot: u32,
    pub item_entry: u32,
    pub suffix_factor: u32,
    pub random_property_id: u32,
    pub count: u32,
}

/// Read `SMSG_ITEM_PUSH_RESULT` (see [`ItemPushResult`] for the field-inclusion verification).
pub(super) fn read_item_push_result(r: &mut &[u8]) -> io::Result<ItemPushResult> {
    let player_guid = read_u64_le(r)?;
    let received = read_u32_le(r)?;
    let created = read_u32_le(r)?;
    let show_in_chat = read_u32_le(r)?;
    let bag_slot = read_u8(r)?;
    let item_slot = read_u32_le(r)?;
    let item_entry = read_u32_le(r)?;
    let suffix_factor = read_u32_le(r)?;
    let random_property_id = read_u32_le(r)?;
    let count = read_u32_le(r)?;
    Ok(ItemPushResult {
        player_guid,
        from_npc: received != 0,
        created: created != 0,
        show_in_chat: show_in_chat != 0,
        bag_slot,
        item_slot,
        item_entry,
        suffix_factor,
        random_property_id,
        count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{opcode, parse_server, ServerPacket};

    fn hx(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn cmsg_bodies_golden() {
        // CMSG_LOOT (Server/Packets/Loot.cpp:8-11): a full guid.
        assert_eq!(
            loot(0x1234_5678_9abc_def0),
            hx("f0debc9a78563412"),
            "CMSG_LOOT body"
        );

        // CMSG_AUTOSTORE_LOOT_ITEM (Loot.cpp:3-6): one u8 wire slot.
        assert_eq!(
            autostore_loot_item(3),
            hx("03"),
            "CMSG_AUTOSTORE_LOOT_ITEM body"
        );

        // CMSG_LOOT_MONEY: empty.
        assert_eq!(loot_money(), Vec::<u8>::new(), "CMSG_LOOT_MONEY body");

        // CMSG_LOOT_RELEASE (Loot.cpp:13-16): a full guid (server ignores it).
        assert_eq!(
            loot_release(0x1234_5678_9abc_def0),
            hx("f0debc9a78563412"),
            "CMSG_LOOT_RELEASE body"
        );

        // CMSG_LOOT_ROLL (Loot.cpp:18-23): u64 lootedTarget, u32 itemSlot, u8 rollType.
        assert_eq!(
            loot_roll(0x1234_5678_9abc_def0, 2, roll_vote::GREED),
            hx("f0debc9a785634120200000002"),
            "CMSG_LOOT_ROLL body"
        );
    }

    /// The five group-roll opcode values (VERIFIED vmangos `Opcodes_1_12_1.h:671-675`).
    #[test]
    fn roll_opcode_values() {
        assert_eq!(opcode::SMSG_LOOT_ALL_PASSED, 670);
        assert_eq!(opcode::SMSG_LOOT_ROLL_WON, 671);
        assert_eq!(opcode::CMSG_LOOT_ROLL, 672);
        assert_eq!(opcode::SMSG_LOOT_START_ROLL, 673);
        assert_eq!(opcode::SMSG_LOOT_ROLL, 674);
    }

    #[test]
    fn loot_start_roll_decodes() {
        // Group.cpp:759-772 / Loot.cpp:43-51: guid, itemSlot, itemEntry, randomSuffix(0),
        // randomPropId, countdown.
        let mut body = 0xAAu64.to_le_bytes().to_vec();
        body.extend_from_slice(&1u32.to_le_bytes()); // itemSlot
        body.extend_from_slice(&17182u32.to_le_bytes()); // itemEntryId
        body.extend_from_slice(&0u32.to_le_bytes()); // randomSuffix — always 0
        body.extend_from_slice(&0u32.to_le_bytes()); // itemRandomPropId
        body.extend_from_slice(&60_000u32.to_le_bytes()); // LOOT_ROLL_TIMEOUT (Group.cpp:67)

        match parse_server(opcode::SMSG_LOOT_START_ROLL, &body).unwrap() {
            ServerPacket::LootStartRoll(p) => assert_eq!(
                p,
                LootStartRoll {
                    looted_target: 0xAA,
                    item_slot: 1,
                    item_id: 17182,
                    random_property_id: 0,
                    countdown_ms: 60_000,
                }
            ),
            other => panic!("expected LootStartRoll, got {}", other.name()),
        }
    }

    /// The four `(roll_number, roll_type)` shapes vmangos actually emits — the overloaded pair
    /// [`LootRoll::is_dice`]/[`LootRoll::vote`] disentangle. The Greed pair is the load-bearing
    /// case: a Greed *vote* is `(128, 2)` and a Greed *dice roll* is `(1..=100, 2)`, identical in
    /// `roll_type`.
    #[test]
    fn loot_roll_decodes_and_disambiguates() {
        let announce = |roll_number: u8, roll_type: u8| {
            let mut body = 0xAAu64.to_le_bytes().to_vec();
            body.extend_from_slice(&1u32.to_le_bytes()); // itemSlot
            body.extend_from_slice(&0xBBu64.to_le_bytes()); // rollerGuid
            body.extend_from_slice(&17182u32.to_le_bytes()); // itemEntryId
            body.extend_from_slice(&0u32.to_le_bytes()); // randomSuffix
            body.extend_from_slice(&0u32.to_le_bytes()); // itemRandomPropId
            body.push(roll_number);
            body.push(roll_type);
            match parse_server(opcode::SMSG_LOOT_ROLL, &body).unwrap() {
                ServerPacket::LootRoll(p) => p,
                other => panic!("expected LootRoll, got {}", other.name()),
            }
        };

        // Field placement, once, off the Need-vote shape.
        let need_vote = announce(0, 0);
        assert_eq!(
            need_vote,
            LootRoll {
                looted_target: 0xAA,
                item_slot: 1,
                roller: 0xBB,
                item_id: 17182,
                random_property_id: 0,
                roll_number: 0,
                roll_type: 0,
            }
        );

        // The three vote announcements (Group.cpp:970-990).
        assert!(!need_vote.is_dice());
        assert_eq!(need_vote.vote(), Some(roll_vote::NEED));

        let pass_vote = announce(128, 128);
        assert!(!pass_vote.is_dice());
        assert_eq!(pass_vote.vote(), Some(roll_vote::PASS));

        let greed_vote = announce(128, roll_vote::GREED);
        assert!(!greed_vote.is_dice());
        assert_eq!(greed_vote.vote(), Some(roll_vote::GREED));

        // The dice results (Group.cpp:1163 / :1214) — urand(1, 100), so both bounds are legal.
        for roll_number in [1u8, 57, 100] {
            for roll_type in [roll_vote::NEED, roll_vote::GREED] {
                let dice = announce(roll_number, roll_type);
                assert!(dice.is_dice(), "roll {roll_number} type {roll_type}");
                assert_eq!(dice.vote(), None, "roll {roll_number} type {roll_type}");
                assert_eq!(dice.roll_type, roll_type);
            }
        }
    }

    #[test]
    fn loot_roll_won_decodes() {
        // Loot.cpp:65-75 — note the winner guid lands AFTER the item block, unlike SMSG_LOOT_ROLL.
        let mut body = 0xAAu64.to_le_bytes().to_vec();
        body.extend_from_slice(&1u32.to_le_bytes()); // itemSlot
        body.extend_from_slice(&17182u32.to_le_bytes()); // itemEntryId
        body.extend_from_slice(&0u32.to_le_bytes()); // randomSuffix
        body.extend_from_slice(&0u32.to_le_bytes()); // itemRandomPropId
        body.extend_from_slice(&0xBBu64.to_le_bytes()); // winnerGuid
        body.push(84); // rollNumber
        body.push(roll_vote::NEED);

        match parse_server(opcode::SMSG_LOOT_ROLL_WON, &body).unwrap() {
            ServerPacket::LootRollWon(p) => assert_eq!(
                p,
                LootRollWon {
                    looted_target: 0xAA,
                    item_slot: 1,
                    item_id: 17182,
                    random_property_id: 0,
                    winner: 0xBB,
                    roll_number: 84,
                    roll_type: roll_vote::NEED,
                }
            ),
            other => panic!("expected LootRollWon, got {}", other.name()),
        }
    }

    #[test]
    fn loot_all_passed_decodes() {
        // Loot.cpp:77-84 — the SWAPPED tail: itemRandomPropId THEN randomSuffixId, the reverse of
        // every sibling in this family. A non-zero prop id proves the parser follows source order.
        let mut body = 0xAAu64.to_le_bytes().to_vec();
        body.extend_from_slice(&1u32.to_le_bytes()); // itemSlot
        body.extend_from_slice(&17182u32.to_le_bytes()); // itemEntryId
        body.extend_from_slice(&7u32.to_le_bytes()); // itemRandomPropId — read
        body.extend_from_slice(&0u32.to_le_bytes()); // randomSuffixId — discarded

        match parse_server(opcode::SMSG_LOOT_ALL_PASSED, &body).unwrap() {
            ServerPacket::LootAllPassed(p) => assert_eq!(
                p,
                LootAllPassed {
                    looted_target: 0xAA,
                    item_slot: 1,
                    item_id: 17182,
                    random_property_id: 7,
                }
            ),
            other => panic!("expected LootAllPassed, got {}", other.name()),
        }
    }

    #[test]
    fn loot_response_items_shape_decodes() {
        // SMSG_LOOT_RESPONSE, normal shape: guid, lootType=CORPSE, gold, itemCount, 2 rows.
        let mut body = 0xAAu64.to_le_bytes().to_vec();
        body.push(loot_type::CORPSE);
        body.extend_from_slice(&1234u32.to_le_bytes()); // gold
        body.push(2); // item count
                      // item 0: slot 0, entry 117, count 1, display 123, randomSuffix(0), randomPropertyId 0, ALLOW_LOOT
        body.push(0);
        body.extend_from_slice(&117u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&123u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.push(slot_type::ALLOW_LOOT);
        // item 1: slot 1, entry 6948 (Hearthstone), count 1, display 4500, randomSuffix(0), randomPropertyId 0, LOCKED
        body.push(1);
        body.extend_from_slice(&6948u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&4500u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.push(slot_type::LOCKED);

        match parse_server(opcode::SMSG_LOOT_RESPONSE, &body).unwrap() {
            ServerPacket::LootResponse {
                guid,
                loot_type,
                gold,
                items,
            } => {
                assert_eq!(guid, 0xAA);
                assert_eq!(loot_type, loot_type::CORPSE);
                assert_eq!(gold, 1234);
                assert_eq!(
                    items,
                    vec![
                        LootItem {
                            slot: 0,
                            item_id: 117,
                            count: 1,
                            display_info_id: 123,
                            random_property_id: 0,
                            slot_type: slot_type::ALLOW_LOOT,
                        },
                        LootItem {
                            slot: 1,
                            item_id: 6948,
                            count: 1,
                            display_info_id: 4500,
                            random_property_id: 0,
                            slot_type: slot_type::LOCKED,
                        },
                    ]
                );
            }
            other => panic!("expected LootResponse, got {}", other.name()),
        }
    }

    #[test]
    fn loot_response_error_shape_decodes() {
        // SMSG_LOOT_RESPONSE, error shape: guid, lootType=0, error code. Nothing else follows —
        // the parser must not try to read gold/itemCount off a slice this short.
        let mut body = 0xBBu64.to_le_bytes().to_vec();
        body.push(0); // lootType 0 ⇒ error shape
        body.push(loot_error::TOO_FAR);

        match parse_server(opcode::SMSG_LOOT_RESPONSE, &body).unwrap() {
            ServerPacket::LootError { guid, error } => {
                assert_eq!(guid, 0xBB);
                assert_eq!(error, loot_error::TOO_FAR);
            }
            other => panic!("expected LootError, got {}", other.name()),
        }
    }

    #[test]
    fn loot_release_response_wire() {
        let mut body = 0xCCu64.to_le_bytes().to_vec();
        body.push(1);
        match parse_server(opcode::SMSG_LOOT_RELEASE_RESPONSE, &body).unwrap() {
            ServerPacket::LootReleaseResponse { guid, result } => {
                assert_eq!(guid, 0xCC);
                assert_eq!(result, 1);
            }
            other => panic!("expected LootReleaseResponse, got {}", other.name()),
        }
    }

    #[test]
    fn loot_removed_wire() {
        let body = vec![5u8];
        match parse_server(opcode::SMSG_LOOT_REMOVED, &body).unwrap() {
            ServerPacket::LootRemoved { slot } => assert_eq!(slot, 5),
            other => panic!("expected LootRemoved, got {}", other.name()),
        }
    }

    #[test]
    fn loot_money_notify_wire() {
        let body = 777u32.to_le_bytes().to_vec();
        match parse_server(opcode::SMSG_LOOT_MONEY_NOTIFY, &body).unwrap() {
            ServerPacket::LootMoneyNotify { amount } => assert_eq!(amount, 777),
            other => panic!("expected LootMoneyNotify, got {}", other.name()),
        }
    }

    #[test]
    fn loot_clear_money_wire() {
        match parse_server(opcode::SMSG_LOOT_CLEAR_MONEY, &[]).unwrap() {
            ServerPacket::LootClearMoney => {}
            other => panic!("expected LootClearMoney, got {}", other.name()),
        }
    }

    #[test]
    fn item_push_result_wire() {
        // SMSG_ITEM_PUSH_RESULT (Item.cpp:211-224, both build-5875 conditionals included):
        // playerGuid, received, created, showInChat, bagSlot, itemSlot, itemEntry, suffixFactor,
        // randomPropertyId, count.
        let mut body = 0x42u64.to_le_bytes().to_vec();
        body.extend_from_slice(&0u32.to_le_bytes()); // received: looted
        body.extend_from_slice(&1u32.to_le_bytes()); // created: new item
        body.extend_from_slice(&1u32.to_le_bytes()); // showInChat
        body.push(255); // bagSlot: player backpack
        body.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // itemSlot: stacked
        body.extend_from_slice(&117u32.to_le_bytes()); // itemEntry
        body.extend_from_slice(&0u32.to_le_bytes()); // suffixFactor
        body.extend_from_slice(&0u32.to_le_bytes()); // randomPropertyId
        body.extend_from_slice(&5u32.to_le_bytes()); // count

        match parse_server(opcode::SMSG_ITEM_PUSH_RESULT, &body).unwrap() {
            ServerPacket::ItemPushResult(p) => {
                assert_eq!(
                    p,
                    ItemPushResult {
                        player_guid: 0x42,
                        from_npc: false,
                        created: true,
                        show_in_chat: true,
                        bag_slot: 255,
                        item_slot: 0xFFFF_FFFF,
                        item_entry: 117,
                        suffix_factor: 0,
                        random_property_id: 0,
                        count: 5,
                    }
                );
            }
            other => panic!("expected ItemPushResult, got {}", other.name()),
        }
    }
}
