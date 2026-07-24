//! The player-to-player trade arc's wire layer (decision 0592 phase P0): the request verbs the
//! trade window sends and the two status packets the server pushes back. Layout VERIFIED against
//! vmangos `Handlers/TradeHandler.cpp`, `Server/Packets/Trade.{h,cpp}`, `Objects/TradeData.{h,cpp}`
//! and `SharedDefines.h` (read directly at decision time). Trade renders nothing derived from
//! WoW.exe, so vmangos is the whole wire authority: its `ReadFromWorldPacket` pins every CMSG body
//! below, its `AppendBodyTo`/`SendUpdateTrade` pin both SMSG parses — no wow-re dispatch is
//! load-bearing (decision 0592, "No wow-re dispatch").

use std::io;

use crate::wire::{read_i32_le, read_u32_le, read_u64_le, read_u8};

/// `TRADE_SLOT_COUNT` (`TradeData.h`): seven slots per side — six tradeable (0..6) plus the
/// seventh **non-traded / enchant** slot ([`TRADE_SLOT_NONTRADED`]), whose item is not exchanged
/// but is the target an enchant/lockpick spell is applied to through the window.
pub const TRADE_SLOT_COUNT: usize = 7;
/// `TRADE_SLOT_TRADED_COUNT` — the six slots (0..6) whose items actually change hands.
pub const TRADE_SLOT_TRADED_COUNT: usize = 6;
/// `TRADE_SLOT_NONTRADED` — the 7th slot (index 6, UI id 7): an item parked here stays with its
/// owner; it is the enchant/spell target, not part of the exchange.
pub const TRADE_SLOT_NONTRADED: usize = 6;

/// The trade state machine's status codes (`SharedDefines.h` `enum TradeStatus`, 0..=22). The
/// tail-carrying members hold their `SMSG_TRADE_STATUS` payload inline (VERIFIED vmangos
/// `WorldPackets::Trade::TradeStatus::AppendBodyTo`): only `BEGIN_TRADE`, `CLOSE_WINDOW` and
/// `ONLY_CONJURED` ride a tail; every other status is the bare `u32`. [`TradeStatus::Unknown`]
/// keeps an out-of-range code (never emitted by vmangos on 5875) parseable rather than fatal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradeStatus {
    /// `0` — the target is already trading / busy (also the initiator's own "already trading").
    Busy,
    /// `1` — sent to the *target* of a fresh `CMSG_INITIATE_TRADE`; `partner` is the initiator's
    /// guid. The client answers `CMSG_BEGIN_TRADE`, which makes the server emit `OpenWindow` to
    /// both sides.
    BeginTrade { partner: u64 },
    /// `2` — open the trade window (fires the client's `TRADE_SHOW`). Sent to both sides once the
    /// target's `CMSG_BEGIN_TRADE` arrives.
    OpenWindow,
    /// `3` — the trade was cancelled; close both windows.
    Canceled,
    /// `4` — the *partner* pressed Trade (drives the accept-glow on their column).
    Accept,
    /// `5` — a second "busy" code, handled as busy.
    Busy2,
    /// `6` — no such target for `CMSG_INITIATE_TRADE`.
    NoTarget,
    /// `7` — a change happened after an accept (or the 200 ms scam-delay bounced an accept):
    /// drop the accept and go back to editing.
    BackToTrade,
    /// `8` — both sides accepted and the swap completed; close both windows.
    Complete,
    /// `9` — the trade was rejected.
    Rejected,
    /// `10` — the partner is out of `TRADE_DISTANCE` (also the flying/no-map initiate refusal).
    TargetTooFar,
    /// `11` — cross-faction trade refused (unless the server allows two-side interaction).
    WrongFaction,
    /// `12` — close the window; `result`/`item_limit_category` are the vanilla tail (usually 0;
    /// the middle `u8` vmangos writes is consumed but carries nothing for player trade).
    CloseWindow {
        result: u32,
        item_limit_category: u32,
    },
    /// `13` — vmangos comments it is "handled with TRADE_STATUS_TRADE_CANCELED"; kept distinct so
    /// the wire round-trips.
    Unknown13,
    /// `14` — the target has you on ignore.
    IgnoreYou,
    /// `15` — you are stunned.
    YouStunned,
    /// `16` — the target is stunned.
    TargetStunned,
    /// `17` — you are dead.
    YouDead,
    /// `18` — the target is dead.
    TargetDead,
    /// `19` — you are logging out.
    YouLogout,
    /// `20` — the target is logging out.
    TargetLogout,
    /// `21` — a trial account restriction.
    TrialAccount,
    /// `22` — "you can only trade conjured items…"; `slot` names the offending trade slot.
    OnlyConjured { slot: u8 },
    /// A code outside 0..=22 (vmangos never sends one on 5875) — kept parseable, no tail read.
    Unknown(u32),
}

impl TradeStatus {
    /// The raw `u32` code this status serializes as (the enum discriminant on the wire).
    pub fn code(self) -> u32 {
        match self {
            TradeStatus::Busy => 0,
            TradeStatus::BeginTrade { .. } => 1,
            TradeStatus::OpenWindow => 2,
            TradeStatus::Canceled => 3,
            TradeStatus::Accept => 4,
            TradeStatus::Busy2 => 5,
            TradeStatus::NoTarget => 6,
            TradeStatus::BackToTrade => 7,
            TradeStatus::Complete => 8,
            TradeStatus::Rejected => 9,
            TradeStatus::TargetTooFar => 10,
            TradeStatus::WrongFaction => 11,
            TradeStatus::CloseWindow { .. } => 12,
            TradeStatus::Unknown13 => 13,
            TradeStatus::IgnoreYou => 14,
            TradeStatus::YouStunned => 15,
            TradeStatus::TargetStunned => 16,
            TradeStatus::YouDead => 17,
            TradeStatus::TargetDead => 18,
            TradeStatus::YouLogout => 19,
            TradeStatus::TargetLogout => 20,
            TradeStatus::TrialAccount => 21,
            TradeStatus::OnlyConjured { .. } => 22,
            TradeStatus::Unknown(code) => code,
        }
    }
}

/// One item shown in a trade slot, as it rides `SMSG_TRADE_STATUS_EXTENDED`'s fixed 60-byte
/// (15×u32) per-slot block (VERIFIED vmangos `WorldSession::SendUpdateTrade`). An empty slot is
/// all-zero on the wire and folds to `None` rather than a zeroed `Some` (the `entry == 0` signal,
/// same convention as [`super::MailAttachment`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeItem {
    /// `Item.dbc` entry (`OBJECT_FIELD_ENTRY`) — the key to name/icon via `ITEM_QUERY_SINGLE`.
    pub entry: u32,
    pub display_id: u32,
    pub count: u32,
    /// A wrapped gift: the client hides stats and shows the gift-creator name.
    pub wrapped: bool,
    pub gift_creator: u64,
    pub perm_enchant: u32,
    pub creator: u64,
    /// Spell charges — signed (a negative count means "N uses left" on some items).
    pub charges: i32,
    pub suffix_factor: u32,
    pub random_prop_id: u32,
    pub lock_id: u32,
    pub max_durability: u32,
    pub durability: u32,
}

/// A decoded `SMSG_TRADE_STATUS_EXTENDED` — the full item/gold snapshot for **one** window side.
/// The server pushes one of these per side whenever that side's offer changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradeStatusExtended {
    /// `true` = the *partner's* (recipient / right) column, `false` = our own (left) column
    /// (vmangos's `trader_state`: `1` for the trader's data, `0` for ours).
    pub their_window: bool,
    /// Gold offered on this side, in copper.
    pub gold: u32,
    /// The spell applied to this side's non-traded slot item (`0` = none) — the enchant slot.
    pub enchant_spell_id: u32,
    /// The seven slots (0..[`TRADE_SLOT_COUNT`]); `None` = empty.
    pub slots: [Option<TradeItem>; TRADE_SLOT_COUNT],
}

// ── Client → server bodies (what vmangos `ReadFromWorldPacket` consumes) ─────────────────────

/// Body of `CMSG_INITIATE_TRADE` (vmangos `InitiateTrade::ReadFromWorldPacket`): one full 8-byte
/// target-player guid. The server answers the initiator on any refusal (`SMSG_TRADE_STATUS`) and,
/// on success, sends the *target* `BeginTrade`.
pub fn initiate_trade(target: u64) -> Vec<u8> {
    target.to_le_bytes().to_vec()
}

/// Body of `CMSG_ACCEPT_TRADE` (vmangos `AcceptTrade::ReadFromWorldPacket`): one `u32` the server
/// **read-skips**. The real client sends `1` once it has seen `OPEN_WINDOW`; we send `1` too (the
/// value is discarded either way).
pub fn accept_trade() -> Vec<u8> {
    1u32.to_le_bytes().to_vec()
}

/// Body of `CMSG_SET_TRADE_ITEM` (vmangos `SetTradeItem::ReadFromWorldPacket`): `u8 tradeSlot,
/// u8 bag, u8 slot` — put the item at inventory (`bag`, `slot`) into trade slot `tradeSlot`.
pub fn set_trade_item(trade_slot: u8, bag: u8, slot: u8) -> Vec<u8> {
    vec![trade_slot, bag, slot]
}

/// Body of `CMSG_CLEAR_TRADE_ITEM` (vmangos `ClearTradeItem::ReadFromWorldPacket`): `u8 tradeSlot`.
pub fn clear_trade_item(trade_slot: u8) -> Vec<u8> {
    vec![trade_slot]
}

/// Body of `CMSG_SET_TRADE_GOLD` (vmangos `SetTradeGold::ReadFromWorldPacket`): `u32 copper`.
pub fn set_trade_gold(copper: u32) -> Vec<u8> {
    copper.to_le_bytes().to_vec()
}

// `CMSG_BEGIN_TRADE`, `CMSG_BUSY_TRADE`, `CMSG_IGNORE_TRADE`, `CMSG_UNACCEPT_TRADE` and
// `CMSG_CANCEL_TRADE` carry **empty** bodies (vmangos reads them as `NullClientPacket`); the
// writer sends `&[]` directly, so they need no builder here.

// ── Server → client parses (what vmangos `AppendBodyTo` / `SendUpdateTrade` emit) ─────────────

/// Read `SMSG_TRADE_STATUS` (VERIFIED vmangos `TradeStatus::AppendBodyTo`): `u32 status`, then a
/// per-status tail — `BEGIN_TRADE → u64 partnerGuid`; `CLOSE_WINDOW → u32 result, u8 unk, u32
/// itemLimitCategory`; `ONLY_CONJURED → u8 slot`; every other status has none.
pub(super) fn read_trade_status(r: &mut &[u8]) -> io::Result<TradeStatus> {
    let code = read_u32_le(r)?;
    Ok(match code {
        0 => TradeStatus::Busy,
        1 => TradeStatus::BeginTrade {
            partner: read_u64_le(r)?,
        },
        2 => TradeStatus::OpenWindow,
        3 => TradeStatus::Canceled,
        4 => TradeStatus::Accept,
        5 => TradeStatus::Busy2,
        6 => TradeStatus::NoTarget,
        7 => TradeStatus::BackToTrade,
        8 => TradeStatus::Complete,
        9 => TradeStatus::Rejected,
        10 => TradeStatus::TargetTooFar,
        11 => TradeStatus::WrongFaction,
        12 => {
            let result = read_u32_le(r)?;
            let _unk = read_u8(r)?; // vmangos writes it; carries nothing for player trade
            let item_limit_category = read_u32_le(r)?;
            TradeStatus::CloseWindow {
                result,
                item_limit_category,
            }
        }
        13 => TradeStatus::Unknown13,
        14 => TradeStatus::IgnoreYou,
        15 => TradeStatus::YouStunned,
        16 => TradeStatus::TargetStunned,
        17 => TradeStatus::YouDead,
        18 => TradeStatus::TargetDead,
        19 => TradeStatus::YouLogout,
        20 => TradeStatus::TargetLogout,
        21 => TradeStatus::TrialAccount,
        22 => TradeStatus::OnlyConjured { slot: read_u8(r)? },
        other => TradeStatus::Unknown(other),
    })
}

/// Read `SMSG_TRADE_STATUS_EXTENDED` (VERIFIED vmangos `WorldSession::SendUpdateTrade`):
/// `u8 which`, `u32 slotCount`, `u32 slotCount` (again — the two counts match), `u32 gold`,
/// `u32 enchantSpellId`, then [`TRADE_SLOT_COUNT`] records of `u8 slotIndex` + a fixed 60-byte
/// (15×u32) item block (all-zero → the slot is empty). The `slotIndex` byte places the block, so
/// a block is stored at `slots[slotIndex]` (defensive against reordering; vmangos writes 0..7 in
/// order).
pub(super) fn read_trade_status_extended(r: &mut &[u8]) -> io::Result<TradeStatusExtended> {
    let which = read_u8(r)?;
    let _slot_count_a = read_u32_le(r)?;
    let _slot_count_b = read_u32_le(r)?;
    let gold = read_u32_le(r)?;
    let enchant_spell_id = read_u32_le(r)?;

    let mut slots: [Option<TradeItem>; TRADE_SLOT_COUNT] = [None; TRADE_SLOT_COUNT];
    for _ in 0..TRADE_SLOT_COUNT {
        let index = read_u8(r)? as usize;
        let entry = read_u32_le(r)?;
        let display_id = read_u32_le(r)?;
        let count = read_u32_le(r)?;
        let wrapped = read_u32_le(r)? != 0;
        let gift_creator = read_u64_le(r)?;
        let perm_enchant = read_u32_le(r)?;
        let creator = read_u64_le(r)?;
        let charges = read_i32_le(r)?;
        let suffix_factor = read_u32_le(r)?;
        let random_prop_id = read_u32_le(r)?;
        let lock_id = read_u32_le(r)?;
        let max_durability = read_u32_le(r)?;
        let durability = read_u32_le(r)?;
        if entry != 0 && index < TRADE_SLOT_COUNT {
            slots[index] = Some(TradeItem {
                entry,
                display_id,
                count,
                wrapped,
                gift_creator,
                perm_enchant,
                creator,
                charges,
                suffix_factor,
                random_prop_id,
                lock_id,
                max_durability,
                durability,
            });
        }
    }
    Ok(TradeStatusExtended {
        their_window: which == 1,
        gold,
        enchant_spell_id,
        slots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CMSG body goldens (byte-exact against vmangos's ReadFromWorldPacket layout) ──

    #[test]
    fn initiate_trade_is_the_target_guid_le() {
        assert_eq!(
            initiate_trade(0x1122_3344_5566_7788),
            vec![0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]
        );
    }

    #[test]
    fn accept_trade_is_u32_one() {
        assert_eq!(accept_trade(), vec![1, 0, 0, 0]);
    }

    #[test]
    fn set_trade_item_is_three_u8s() {
        // Put bag 0 / slot 23 into trade slot 2.
        assert_eq!(set_trade_item(2, 0, 23), vec![2, 0, 23]);
    }

    #[test]
    fn clear_trade_item_is_one_u8() {
        assert_eq!(clear_trade_item(5), vec![5]);
    }

    #[test]
    fn set_trade_gold_is_u32_le() {
        assert_eq!(set_trade_gold(0x0001_E240), vec![0x40, 0xE2, 0x01, 0x00]);
    }

    // ── SMSG_TRADE_STATUS parse goldens ──

    #[test]
    fn trade_status_bare_code_has_no_tail() {
        let buf = 2u32.to_le_bytes(); // OPEN_WINDOW
        let mut r = &buf[..];
        assert_eq!(read_trade_status(&mut r).unwrap(), TradeStatus::OpenWindow);
        assert!(r.is_empty(), "no tail should be consumed for a bare status");
    }

    #[test]
    fn trade_status_begin_trade_reads_the_partner_guid() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u32.to_le_bytes()); // BEGIN_TRADE
        buf.extend_from_slice(&0xDEAD_BEEF_0000_0001u64.to_le_bytes());
        let mut r = &buf[..];
        assert_eq!(
            read_trade_status(&mut r).unwrap(),
            TradeStatus::BeginTrade {
                partner: 0xDEAD_BEEF_0000_0001
            }
        );
        assert!(r.is_empty());
    }

    #[test]
    fn trade_status_close_window_consumes_u32_u8_u32() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&12u32.to_le_bytes()); // CLOSE_WINDOW
        buf.extend_from_slice(&7u32.to_le_bytes()); // result
        buf.push(0); // unk (dropped)
        buf.extend_from_slice(&3u32.to_le_bytes()); // itemLimitCategory
        let mut r = &buf[..];
        assert_eq!(
            read_trade_status(&mut r).unwrap(),
            TradeStatus::CloseWindow {
                result: 7,
                item_limit_category: 3
            }
        );
        assert!(r.is_empty(), "the full u32+u8+u32 tail must be consumed");
    }

    #[test]
    fn trade_status_only_conjured_reads_the_slot_byte() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&22u32.to_le_bytes()); // ONLY_CONJURED
        buf.push(4); // slot
        let mut r = &buf[..];
        assert_eq!(
            read_trade_status(&mut r).unwrap(),
            TradeStatus::OnlyConjured { slot: 4 }
        );
        assert!(r.is_empty());
    }

    #[test]
    fn trade_status_out_of_range_is_unknown_no_tail() {
        let buf = 99u32.to_le_bytes();
        let mut r = &buf[..];
        assert_eq!(read_trade_status(&mut r).unwrap(), TradeStatus::Unknown(99));
        assert!(r.is_empty());
    }

    // ── SMSG_TRADE_STATUS_EXTENDED parse golden ──

    /// Build a full 444-byte snapshot the way vmangos does: a 17-byte header, then 7 slot records
    /// (each a `u8 index` + 60-byte block). `items[i] = Some(entry)` fills slot `i`; `None` zeroes it.
    fn extended_wire(
        which: u8,
        gold: u32,
        spell: u32,
        entries: [Option<u32>; TRADE_SLOT_COUNT],
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.push(which);
        b.extend_from_slice(&(TRADE_SLOT_COUNT as u32).to_le_bytes());
        b.extend_from_slice(&(TRADE_SLOT_COUNT as u32).to_le_bytes());
        b.extend_from_slice(&gold.to_le_bytes());
        b.extend_from_slice(&spell.to_le_bytes());
        for (i, entry) in entries.iter().enumerate() {
            b.push(i as u8);
            match entry {
                Some(e) => {
                    b.extend_from_slice(&e.to_le_bytes()); // entry
                    b.extend_from_slice(&11u32.to_le_bytes()); // display_id
                    b.extend_from_slice(&5u32.to_le_bytes()); // count
                    b.extend_from_slice(&0u32.to_le_bytes()); // wrapped
                    b.extend_from_slice(&0u64.to_le_bytes()); // gift_creator
                    b.extend_from_slice(&0u32.to_le_bytes()); // perm_enchant
                    b.extend_from_slice(&0u64.to_le_bytes()); // creator
                    b.extend_from_slice(&(-3i32).to_le_bytes()); // charges (signed)
                    b.extend_from_slice(&0u32.to_le_bytes()); // suffix_factor
                    b.extend_from_slice(&0u32.to_le_bytes()); // random_prop_id
                    b.extend_from_slice(&0u32.to_le_bytes()); // lock_id
                    b.extend_from_slice(&100u32.to_le_bytes()); // max_durability
                    b.extend_from_slice(&80u32.to_le_bytes()); // durability
                }
                None => b.extend_from_slice(&[0u8; 60]),
            }
        }
        b
    }

    #[test]
    fn extended_snapshot_is_always_444_bytes() {
        // 17-byte header (u8 which + 4×u32) + 7 slot records of (u8 index + 60-byte block).
        let wire = extended_wire(0, 0, 0, [None; TRADE_SLOT_COUNT]);
        assert_eq!(wire.len(), 17 + TRADE_SLOT_COUNT * 61);
        assert_eq!(wire.len(), 444);
    }

    #[test]
    fn extended_parses_header_slots_and_signed_charges() {
        let mut entries = [None; TRADE_SLOT_COUNT];
        entries[0] = Some(0xABCD);
        entries[TRADE_SLOT_NONTRADED] = Some(0x1111); // the enchant slot carries an item too
        let wire = extended_wire(1, 12_345, 777, entries);
        let mut r = &wire[..];
        let ext = read_trade_status_extended(&mut r).unwrap();
        assert!(r.is_empty(), "the whole snapshot must be consumed");

        assert!(ext.their_window);
        assert_eq!(ext.gold, 12_345);
        assert_eq!(ext.enchant_spell_id, 777);

        let slot0 = ext.slots[0].expect("slot 0 filled");
        assert_eq!(slot0.entry, 0xABCD);
        assert_eq!(slot0.display_id, 11);
        assert_eq!(slot0.count, 5);
        assert_eq!(slot0.charges, -3);
        assert_eq!(slot0.max_durability, 100);
        assert_eq!(slot0.durability, 80);

        assert!(
            ext.slots[TRADE_SLOT_NONTRADED].is_some(),
            "enchant slot parsed"
        );
        assert!(ext.slots[1].is_none(), "empty slot folds to None");
        assert!(ext.slots[5].is_none());
    }

    #[test]
    fn extended_own_window_flag() {
        let wire = extended_wire(0, 0, 0, [None; TRADE_SLOT_COUNT]);
        let mut r = &wire[..];
        let ext = read_trade_status_extended(&mut r).unwrap();
        assert!(!ext.their_window, "which == 0 is our own column");
    }
}
