//! Spell-book and cooldown messages — what the character knows, and when they may use it again.
//! Split out of `messages/spells.rs` (decision 0640).
//!
//! Book and cooldowns share a file because they share a **packet**: `SMSG_INITIAL_SPELLS` delivers
//! the known-spell list and the active-cooldown list in one body, so [`read_initial_spells`] cannot
//! be separated from [`SpellCooldown`] at the wire. The book then grows through
//! `SMSG_LEARNED_SPELL` / `SMSG_SUPERCEDED_SPELL` (a trainer purchase, a quest reward, a level-up
//! rank gain — decision 0237), and the four cooldown packets adjust it from the server side.
//!
//! Worth knowing before reaching for these: a **normal cast's cooldown is CLIENT-tracked**. The
//! server only sends cooldowns for school lockouts, pets, item procs and GM resets — so these
//! packets are the exceptions, not the mechanism. Layouts VERIFIED against vmangos and, for the four
//! cooldown packets, against the real client's own handlers (wow-re `wave-handlers.md`).
//!
//! This family is inbound only; the outbound half of "what I know" is
//! `world::writer::{skills, progression}`.

use std::io::{self, Read};

use crate::wire::{read_u16_le, read_u32_le, read_u64_le, read_u8};

/// One active cooldown from `SMSG_INITIAL_SPELLS`' second list (vmangos `SendInitialSpells`):
/// `u16 spell, u16 castItem, u16 category, u32 spellCdMs, u32 categoryCdMs`. A *permanent*
/// cooldown (a one-per-fight ability the server re-arms) is `spell_cd_ms == 1` with the category
/// word's top bit set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellCooldown {
    pub spell_id: u16,
    pub item_id: u16,
    pub category: u16,
    pub spell_cd_ms: u32,
    pub category_cd_ms: u32,
}

/// Read `SMSG_INITIAL_SPELLS` (vmangos `Player::SendInitialSpells`): `u8 0; u16 n; n×(u16 spellId,
/// u16 0); u16 m; m×`[`SpellCooldown`]. The per-spell second word is "not slot id" (vmangos's own
/// note) and always 0 — skipped.
pub(super) fn read_initial_spells(r: &mut impl Read) -> io::Result<(Vec<u16>, Vec<SpellCooldown>)> {
    let _ = read_u8(r)?;
    let n = read_u16_le(r)?;
    let mut spells = Vec::with_capacity(n as usize);
    for _ in 0..n {
        spells.push(read_u16_le(r)?);
        let _ = read_u16_le(r)?;
    }
    let m = read_u16_le(r)?;
    let mut cooldowns = Vec::with_capacity(m as usize);
    for _ in 0..m {
        cooldowns.push(SpellCooldown {
            spell_id: read_u16_le(r)?,
            item_id: read_u16_le(r)?,
            category: read_u16_le(r)?,
            spell_cd_ms: read_u32_le(r)?,
            category_cd_ms: read_u32_le(r)?,
        });
    }
    Ok((spells, cooldowns))
}

/// Read `SMSG_LEARNED_SPELL` (vmangos `WorldPackets::Spell::LearnedSpell::AppendBodyTo`,
/// `Server/Packets/Spell.cpp:175-179`): `u16 spellId, u16 actionBarSlot`. The slot is "not used on
/// client" (vmangos's own note) and dropped. This is the one wire that grows the spell book *after*
/// login — a trainer purchase, a quest reward, a level-up rank gain (decision 0237); benilla's book
/// was otherwise login-only ([`read_initial_spells`]).
pub(super) fn read_learned_spell(r: &mut impl Read) -> io::Result<u16> {
    let spell_id = read_u16_le(r)?;
    let _action_bar_slot = read_u16_le(r)?;
    Ok(spell_id)
}

/// Read `SMSG_REMOVED_SPELL` (vmangos `RemovedSpell::AppendBodyTo`, `Server/Packets/Spell.cpp:181`):
/// a bare `u16 spellId`, and **no action-bar slot** — unlike its `SMSG_LEARNED_SPELL` sibling above,
/// which pads one on. The spell is gone from the book; what happens to a bar button still pointing
/// at it is a separate law (decision 1584).
pub(super) fn read_removed_spell(r: &mut impl Read) -> io::Result<u16> {
    read_u16_le(r)
}

/// Read `SMSG_SUPERCEDED_SPELL` (vmangos `SupercededSpell::AppendBodyTo`, `Spell.cpp:169-173`): `u16
/// oldSpellId, u16 newSpellId` — a rank-up replaces the old spell with the new one in both the book
/// and the action bar (decision 0237).
pub(super) fn read_superceded_spell(r: &mut impl Read) -> io::Result<(u16, u16)> {
    Ok((read_u16_le(r)?, read_u16_le(r)?))
}

/// Read `SMSG_SPELL_COOLDOWN` → `(caster, Vec<(spell_id, cooldown_ms)>)` (VERIFIED both sides:
/// vmangos `WorldPackets::Spell::SpellCooldown::AppendBodyTo`, `Server/Packets/Spell.cpp:142-151` —
/// a raw `u64` guid then pairs to end-of-body, NO flags byte in 1.12 (vmangos's own commented-out
/// `uint8`); the client handler `0x6e9460` reads exactly guid + `(GetInt32, GetUInt32)*` until the
/// stream runs dry, wow-re `wave-handlers.md`). `cooldown_ms == 0` means "use the spell's own
/// `Spell.dbc` RecoveryTime/CategoryRecoveryTime" (the handler's `cooldownMs!=0` fork); nonzero is
/// a server-set duration (the school-lockout path sends these). vmangos sends this for lockouts
/// (`Player::LockOutSpells`) and pet cooldowns — a normal cast's cooldown is CLIENT-tracked.
pub(super) fn read_spell_cooldown(r: &mut &[u8]) -> io::Result<(u64, Vec<(u32, u32)>)> {
    let caster = read_u64_le(r)?;
    let mut cooldowns = Vec::new();
    while !r.is_empty() {
        let spell_id = read_u32_le(r)?;
        let cooldown_ms = read_u32_le(r)?;
        cooldowns.push((spell_id, cooldown_ms));
    }
    Ok((caster, cooldowns))
}

/// Read `SMSG_ITEM_COOLDOWN` → `(item_guid, spell_id)` (VERIFIED both sides: vmangos
/// `WorldPackets::Item::ItemCooldown::AppendBodyTo`, `Server/Packets/Item.cpp:229-233` — raw `u64`
/// item guid + `u32` spell id; the client handler `0x6e95d0` resolves the item object and inserts a
/// **fixed 30 000 ms** cooldown on it, wow-re `wave-handlers.md` — the 30 s is the client's
/// hardcode, nothing more rides the wire). Sent when a proc puts an equipped on-use item on its
/// shared 30 s use-cooldown (vmangos `Player.cpp:19370-19383`).
pub(super) fn read_item_cooldown(r: &mut impl Read) -> io::Result<(u64, u32)> {
    Ok((read_u64_le(r)?, read_u32_le(r)?))
}

/// Read `SMSG_COOLDOWN_EVENT` / `SMSG_CLEAR_COOLDOWN` → `(spell_id, caster)` — the two share one
/// body shape (VERIFIED both sides: vmangos `CooldownEvent`/`ClearCooldown::AppendBodyTo`,
/// `Server/Packets/Spell.cpp:152-167` — `u32` spell id THEN raw `u64` guid; the client handler
/// `0x6e9670` reads GetInt32 then GetGuid, wow-re `wave-handlers.md`). EVENT **starts** an on-hold
/// (`SPELL_ATTR_COOLDOWN_ON_EVENT`) record's parked timers now; CLEAR removes the record outright.
pub(super) fn read_cooldown_event(r: &mut impl Read) -> io::Result<(u32, u64)> {
    Ok((read_u32_le(r)?, read_u64_le(r)?))
}

/// Read `SMSG_COOLDOWN_CHEAT` → the target guid (VERIFIED both sides: vmangos
/// `CooldownCheat::AppendBodyTo` — one raw `u64`; the client handler `0x6e9730` wipes the whole
/// self/pet cooldown list when the guid matches, wow-re `wave-handlers.md`). The GM `.cooldown`
/// reset.
pub(super) fn read_cooldown_cheat(r: &mut impl Read) -> io::Result<u64> {
    read_u64_le(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three incremental book packets, side by side, because their bodies differ in exactly the
    /// way that bites: LEARNED pads an unused action-bar slot after the id, REMOVED does not pad
    /// anything, and SUPERCEDED carries a pair. Reading REMOVED with LEARNED's reader would work by
    /// accident on a 4-byte body and starve on the real 2-byte one.
    #[test]
    fn the_three_book_deltas_have_three_different_bodies() {
        // SMSG_LEARNED_SPELL: u16 spell + u16 slot (dropped).
        let body: Vec<u8> = [1752u16.to_le_bytes(), 0u16.to_le_bytes()].concat();
        assert_eq!(read_learned_spell(&mut &body[..]).unwrap(), 1752);

        // SMSG_REMOVED_SPELL: the bare u16, and nothing after it.
        let body = 1752u16.to_le_bytes().to_vec();
        let mut r = &body[..];
        assert_eq!(read_removed_spell(&mut r).unwrap(), 1752);
        assert!(r.is_empty(), "the removal body is two bytes, full stop");

        // SMSG_SUPERCEDED_SPELL: the pair.
        let body: Vec<u8> = [1752u16.to_le_bytes(), 1757u16.to_le_bytes()].concat();
        assert_eq!(read_superceded_spell(&mut &body[..]).unwrap(), (1752, 1757));
    }

    #[test]
    fn cooldown_bodies_golden() {
        // SMSG_SPELL_COOLDOWN: raw u64 guid, then (u32 spell, u32 ms) pairs to end-of-body — NO
        // flags byte in 1.12 (vmangos's own commented-out uint8; the client handler reads the
        // pairs straight after the guid). Guid 0x10, then {133, 0} ("use Spell.dbc") and
        // {5384, 30000}.
        let body: Vec<u8> = [
            0x10u64.to_le_bytes().to_vec(),
            133u32.to_le_bytes().to_vec(),
            0u32.to_le_bytes().to_vec(),
            5384u32.to_le_bytes().to_vec(),
            30_000u32.to_le_bytes().to_vec(),
        ]
        .concat();
        let mut r = &body[..];
        assert_eq!(
            read_spell_cooldown(&mut r).unwrap(),
            (0x10, vec![(133, 0), (5384, 30_000)])
        );
        assert!(r.is_empty());

        // SMSG_ITEM_COOLDOWN: raw u64 item guid + u32 spell id — nothing else (the 30 s is the
        // client's hardcode).
        let body: Vec<u8> = [
            0x40u64.to_le_bytes().to_vec(),
            439u32.to_le_bytes().to_vec(),
        ]
        .concat();
        let mut r = &body[..];
        assert_eq!(read_item_cooldown(&mut r).unwrap(), (0x40, 439));

        // SMSG_COOLDOWN_EVENT / SMSG_CLEAR_COOLDOWN: u32 spell id FIRST, then the raw u64 guid
        // (vmangos `CooldownEvent::AppendBodyTo`; the client reads GetInt32 then GetGuid).
        let body: Vec<u8> = [
            1784u32.to_le_bytes().to_vec(),
            0x22u64.to_le_bytes().to_vec(),
        ]
        .concat();
        let mut r = &body[..];
        assert_eq!(read_cooldown_event(&mut r).unwrap(), (1784, 0x22));

        // SMSG_COOLDOWN_CHEAT: the lone raw u64.
        let body = 0x33u64.to_le_bytes();
        let mut r = &body[..];
        assert_eq!(read_cooldown_cheat(&mut r).unwrap(), 0x33);
    }
}
