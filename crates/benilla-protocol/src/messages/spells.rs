//! The **cast pipeline** — a cast's whole life on the wire: the four outbound `CMSG_CAST_SPELL`
//! shapes, the server's verdict, the start/launch pair every observer sees, the interrupt and
//! pushback notices, the channel timer, and the aura a cast leaves behind. Every layout here is
//! VERIFIED against vmangos source (cited per item); the opcodes are in [`super::opcode`] (verified
//! `Opcodes_1_12_1.h`).
//!
//! This file was once the whole spell/combat/bar/progression wire in one 1061-line block; decision
//! 0640 peeled off [`super::spellbook`], [`super::action_bar`], [`super::attack`],
//! [`super::combat_log`], [`super::progression`] and [`super::pose`], leaving the cast itself.
//! Mirrored by `world::writer::spells`.
//!
//! Two things about the shape that are easy to get wrong and are load-bearing here:
//!
//! - [`SpellCastTargets`]'s decode must follow vmangos' **write**-side branch order bit for bit, not
//!   the symmetric-looking read side — the writer's if/else-if chain emits exactly one packed guid
//!   when several target bits are set, and guessing differently desyncs the stream.
//! - Nothing about missile travel rides `SMSG_SPELL_GO`. It is sent at **launch**; the server
//!   schedules impact itself off `Spell.dbc` Speed (decision 0099).
//!
//! The aura pair lives here rather than in a family of its own because an aura is what a spell
//! leaves behind, and the wire says so: `CMSG_CANCEL_AURA` is addressed **by spell id, not by aura
//! slot**. Aura *state* is descriptor data (`UNIT_FIELD_AURA`), not a packet.

use std::io::{self, Read};

use crate::wire::{
    read_cstring, read_packed_guid, read_u16_le, read_u32_le, read_u64_le, read_u8, Vector3d,
};

/// `SMSG_CAST_RESULT`'s verdict: `u32 spellId, u8 status` — status `0` (`SPELL_RESULT_STATUS_OKAY`)
/// ends the packet, status `2` (`SPELL_RESULT_STATUS_FAIL`) appends a `u8` failure reason
/// (vmangos `CastResult::AppendBodyTo`; some reasons append extra arg words we skip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastOutcome {
    Ok,
    Failed { reason: u8 },
}

/// Read `SMSG_CAST_RESULT` → `(spell_id, outcome)`. Reason-specific trailing args (a required
/// spell-focus id, an equip class) are left unread — the slice ends with the packet, nothing
/// follows in the stream.
pub(super) fn read_cast_result(r: &mut impl Read) -> io::Result<(u32, CastOutcome)> {
    let spell_id = read_u32_le(r)?;
    let status = read_u8(r)?;
    let outcome = if status == 2 {
        CastOutcome::Failed {
            reason: read_u8(r)?,
        }
    } else {
        CastOutcome::Ok
    };
    Ok((spell_id, outcome))
}

// --- the spell-visual pipeline wire (decision 0099 phase 1) -----------------------------------------
//
// `SpellCastTargets` bit flags (vmangos `SpellDefines.h:96-113`, `SpellCastTargetFlags`) needed to
// decode the `write()` shape (`SpellCastTargetsInfo.cpp:180-234`) SMSG_SPELL_START/GO carry.
const TARGET_FLAG_UNIT: u16 = 0x0002;
const TARGET_FLAG_ITEM: u16 = 0x0010;
const TARGET_FLAG_SOURCE_LOCATION: u16 = 0x0020;
const TARGET_FLAG_DEST_LOCATION: u16 = 0x0040;
const TARGET_FLAG_CORPSE_ENEMY: u16 = 0x0200;
const TARGET_FLAG_GAMEOBJECT: u16 = 0x0800;
const TARGET_FLAG_TRADE_ITEM: u16 = 0x1000;
const TARGET_FLAG_STRING: u16 = 0x2000;
const TARGET_FLAG_CORPSE_ALLY: u16 = 0x8000;

/// A decoded `SpellCastTargets` (vmangos `SpellCastTargetsInfo.cpp:180-234`, the **write** side —
/// asymmetric from the client→server `read()`: when more than one of UNIT/GAMEOBJECT/CORPSE_* is set,
/// the writer's if/else-if chain emits exactly **one** packed guid, by priority UNIT > GAMEOBJECT >
/// CORPSE_ALLY|CORPSE_ENEMY — never happens for a real spell, but the decode must follow the same
/// branch order bit for bit or it desyncs the stream). Surfaces what a consumer needs now
/// (`unit_target`, `go_target`, `dest`); item/corpse/source/string targets are read to keep the cursor
/// aligned and dropped — no consumer for them yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpellCastTargets {
    pub mask: u16,
    pub unit_target: Option<u64>,
    /// The GameObject this cast targets (`TARGET_FLAG_GAMEOBJECT`) — an open-lock cast on a chest / locked
    /// door rides here. Surfaced for the GO lid/door open animation (decision 0250); a unit spell leaves
    /// it `None`. Mutually exclusive with `unit_target` in practice (the writer emits one guid).
    pub go_target: Option<u64>,
    pub dest: Option<Vector3d>,
}

fn read_spell_cast_targets(r: &mut impl Read) -> io::Result<SpellCastTargets> {
    let mask = read_u16_le(r)?;
    let mut unit_target = None;
    let mut go_target = None;
    if mask
        & (TARGET_FLAG_UNIT
            | TARGET_FLAG_GAMEOBJECT
            | TARGET_FLAG_CORPSE_ENEMY
            | TARGET_FLAG_CORPSE_ALLY)
        != 0
    {
        // Exactly one packed guid rides here (see the struct doc); the writer's priority is UNIT >
        // GAMEOBJECT, so mirror that branch order (UNIT wins if both bits are set, never in practice).
        let guid = read_packed_guid(r)?;
        if mask & TARGET_FLAG_UNIT != 0 {
            unit_target = Some(guid);
        } else if mask & TARGET_FLAG_GAMEOBJECT != 0 {
            go_target = Some(guid);
        }
    }
    if mask & (TARGET_FLAG_ITEM | TARGET_FLAG_TRADE_ITEM) != 0 {
        let _item_guid = read_packed_guid(r)?;
    }
    if mask & TARGET_FLAG_SOURCE_LOCATION != 0 {
        let _src = Vector3d::read(r)?;
    }
    let dest = if mask & TARGET_FLAG_DEST_LOCATION != 0 {
        Some(Vector3d::read(r)?)
    } else {
        None
    };
    if mask & TARGET_FLAG_STRING != 0 {
        let _string_target = read_cstring(r)?;
    }
    Ok(SpellCastTargets {
        mask,
        unit_target,
        go_target,
        dest,
    })
}

/// `CAST_FLAG_AMMO` (vmangos `Spell.h:53`) — the projectile-visual bit, set for every ranged spell on
/// both `SMSG_SPELL_START` and `SMSG_SPELL_GO`; its presence gates the trailing ammo block.
const CAST_FLAG_AMMO: u16 = 0x0020;

/// Read the ammo block (`Spell::WriteAmmoToPacket`, `Spell.cpp:4540-4606`): `u32 displayId, u32
/// inventoryType`. Only `displayId` has a consumer today (the projectile model, phase 4/5) —
/// `inventoryType` is read to stay aligned and dropped.
fn read_ammo(r: &mut impl Read) -> io::Result<u32> {
    let display_id = read_u32_le(r)?;
    let _inventory_type = read_u32_le(r)?;
    Ok(display_id)
}

/// One decoded `SMSG_SPELL_START` — a non-triggered cast began, instants included (`cast_time_ms ==
/// 0` — the precast trigger, decision 0099 phase 1). VERIFIED vmangos `Spell::SendSpellStart`
/// (`Spell.cpp:4468-4503`): `item_or_caster` pguid (the cast item's guid when one is in play, else
/// the caster's own — `WriteGuidHelper`, `Spell.cpp:4453-4466`) · `caster` pguid (`m_casterUnit`,
/// always the casting Unit) · `u32 spellId` · `u16 castFlags` (always `CAST_FLAG_UNKNOWN2` 0x2, +
/// `CAST_FLAG_AMMO` 0x20 for a ranged spell) · `u32` remaining cast-time ms (`m_timer`) ·
/// [`SpellCastTargets`] · the ammo block iff `castFlags & 0x20`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpellStart {
    pub item_or_caster: u64,
    pub caster: u64,
    pub spell_id: u32,
    pub cast_flags: u16,
    pub cast_time_ms: u32,
    pub targets: SpellCastTargets,
    pub ammo_display_id: Option<u32>,
}

pub(super) fn read_spell_start(r: &mut impl Read) -> io::Result<SpellStart> {
    let item_or_caster = read_packed_guid(r)?;
    let caster = read_packed_guid(r)?;
    let spell_id = read_u32_le(r)?;
    let cast_flags = read_u16_le(r)?;
    let cast_time_ms = read_u32_le(r)?;
    let targets = read_spell_cast_targets(r)?;
    let ammo_display_id = if cast_flags & CAST_FLAG_AMMO != 0 {
        Some(read_ammo(r)?)
    } else {
        None
    };
    Ok(SpellStart {
        item_or_caster,
        caster,
        spell_id,
        cast_flags,
        cast_time_ms,
        targets,
        ammo_display_id,
    })
}

/// `SPELL_MISS_REFLECT` (vmangos `SpellDefines.h:173`) — the one `SpellMissInfo` that carries a
/// trailing byte (the reflected spell's own outcome against its new target).
const SPELL_MISS_REFLECT: u8 = 11;

/// One decoded `SMSG_SPELL_GO` — the cast launched. VERIFIED vmangos `Spell::SendSpellGo`
/// (`Spell.cpp:4505-4538`) + its target-list writer `WriteSpellGoTargets` (`Spell.cpp:4608-4659`):
/// the same guid pair + spellId as [`SpellStart`] · `u16 castFlags` (always `CAST_FLAG_UNKNOWN9`
/// 0x100, + `CAST_FLAG_AMMO` 0x20 for a ranged spell) · `u8` hit count + that many **raw** (unpacked)
/// `u64` hit guids · `u8` miss count + that many `{u64 guid, u8 SpellMissInfo, u8 reflectResult iff
/// the reason is `SPELL_MISS_REFLECT`}` · [`SpellCastTargets`] · the ammo block iff `castFlags &
/// 0x20`. Sent at **launch** — the server schedules impact itself off `Spell.dbc` Speed, so nothing
/// about missile travel rides this packet (decision 0099).
#[derive(Debug, Clone, PartialEq)]
pub struct SpellGo {
    pub item_or_caster: u64,
    pub caster: u64,
    pub spell_id: u32,
    pub cast_flags: u16,
    pub hits: Vec<u64>,
    /// `(guid, SpellMissInfo reason)` — the reflect-outcome byte (present only when `reason ==
    /// SPELL_MISS_REFLECT`) is read to stay aligned and dropped; no consumer yet.
    pub misses: Vec<(u64, u8)>,
    pub targets: SpellCastTargets,
    pub ammo_display_id: Option<u32>,
}

pub(super) fn read_spell_go(r: &mut impl Read) -> io::Result<SpellGo> {
    let item_or_caster = read_packed_guid(r)?;
    let caster = read_packed_guid(r)?;
    let spell_id = read_u32_le(r)?;
    let cast_flags = read_u16_le(r)?;

    let hit_count = read_u8(r)?;
    let mut hits = Vec::with_capacity(hit_count as usize);
    for _ in 0..hit_count {
        hits.push(read_u64_le(r)?); // raw guid (Spell.cpp:4627,4635) — the hit list is never packed
    }
    let miss_count = read_u8(r)?;
    let mut misses = Vec::with_capacity(miss_count as usize);
    for _ in 0..miss_count {
        let guid = read_u64_le(r)?;
        let reason = read_u8(r)?;
        if reason == SPELL_MISS_REFLECT {
            let _reflect_result = read_u8(r)?;
        }
        misses.push((guid, reason));
    }

    let targets = read_spell_cast_targets(r)?;
    let ammo_display_id = if cast_flags & CAST_FLAG_AMMO != 0 {
        Some(read_ammo(r)?)
    } else {
        None
    };
    Ok(SpellGo {
        item_or_caster,
        caster,
        spell_id,
        cast_flags,
        hits,
        misses,
        targets,
        ammo_display_id,
    })
}

/// Read `SMSG_SPELL_FAILED_OTHER` → `(caster, spell_id)` (vmangos `Spell::SendInterrupted`,
/// `Spell.cpp:4780-4789`): the broadcast cast-cancel notice for **observers** — a raw (unpacked)
/// `u64` guid + `u32` spellId. Our own cast's failure rides `SMSG_CAST_RESULT` instead;
/// `SMSG_SPELL_FAILURE` is never constructed server-side (decision 0099).
pub(super) fn read_spell_failed_other(r: &mut impl Read) -> io::Result<(u64, u32)> {
    let caster = read_u64_le(r)?;
    let spell_id = read_u32_le(r)?;
    Ok((caster, spell_id))
}

/// Read `SMSG_SPELL_DELAYED` → `(caster, delay_ms)` (vmangos `Spell::Delayed`, `Spell.cpp:7472`):
/// a raw (unpacked) `u64` caster guid + `u32` pushback time in ms, sent **to the caster** when a
/// pushback-eligible cast takes damage (Fireball carries `DAMAGE_PUSHBACK`; the server extends its
/// own cast timer by `delay_ms`). The cast bar shifts its window out by the same (decision 0256).
pub(super) fn read_spell_delayed(r: &mut impl Read) -> io::Result<(u64, u32)> {
    let caster = read_u64_le(r)?;
    let delay_ms = read_u32_le(r)?;
    Ok((caster, delay_ms))
}

/// Read `MSG_CHANNEL_START` → `(spell_id, duration_ms)` (vmangos `Spell::SendChannelStart`,
/// `Spell.cpp:4951-4954`): `u32 spellId` + `u32 duration` — **self-only** (SendDirectMessage to
/// the casting player; no guid on the wire). The cast bar's channel-open edge (decision 0137).
pub(super) fn read_channel_start(r: &mut impl Read) -> io::Result<(u32, u32)> {
    let spell_id = read_u32_le(r)?;
    let duration_ms = read_u32_le(r)?;
    Ok((spell_id, duration_ms))
}

/// Read `MSG_CHANNEL_UPDATE` → `remaining_ms` (vmangos `Player::SendChannelUpdate`,
/// `Player.cpp:21106-21110`): a single `u32` time-left — **self-only**, `0` = the channel is over
/// (sent on natural end and on interrupt alike). The cast bar's channel tick/close (decision 0137).
pub(super) fn read_channel_update(r: &mut impl Read) -> io::Result<u32> {
    read_u32_le(r)
}

/// Read `SMSG_UPDATE_AURA_DURATION` → `(slot, remaining_ms)` (vmangos
/// `SpellAuraHolder::UpdateAuraDuration`, `SpellAuras.cpp:7511-7523`): a `u8` `UNIT_FIELD_AURA` slot
/// index and a `u32` of milliseconds left. **Self-only** — it goes to the aura's target, never to
/// the caster or an onlooker — and never sent for a permanent aura, so an occupied slot that has no
/// duration is the reference's "until cancelled" (decision 0255).
pub(super) fn read_update_aura_duration(r: &mut impl Read) -> io::Result<(u8, u32)> {
    let slot = read_u8(r)?;
    let remaining_ms = read_u32_le(r)?;
    Ok((slot, remaining_ms))
}

/// Read `SMSG_PLAY_SPELL_VISUAL` → `(unit, kit_id)` (vmangos
/// `WorldPackets::Spell::PlaySpellVisual::AppendBodyTo`, `Server/Packets/Spell.cpp:54-58`): a raw
/// (unpacked) `u64` guid + `u32` kit id, bounds-checked against `SpellVisualKit.dbc` and played
/// at the client's hardcoded stage 0 (`0x6e98d0` — the eat/drink cadence; decision 0280).
pub(super) fn read_play_spell_visual(r: &mut impl Read) -> io::Result<(u64, u32)> {
    let unit = read_u64_le(r)?;
    let kit_id = read_u32_le(r)?;
    Ok((unit, kit_id))
}

/// Body of `CMSG_CAST_SPELL` (vmangos `CastSpell::ReadFromWorldPacket` → `SpellCastTargets::read`):
/// `u32 spellId` + the target block. `None` = a self/implicit cast — mask `TARGET_FLAG_SELF (0)`,
/// nothing follows (the server fills the target from the spell's implicit targeting).
/// `Some(guid)` = an explicit unit target — mask `TARGET_FLAG_UNIT (0x0002)` + the guid **packed**
/// (`ReadAsPackedClientBuildAware` is the packed reader for builds > 1.8.4).
pub fn cast_spell(spell_id: u32, target: Option<u64>) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&spell_id.to_le_bytes());
    match target {
        None => body.extend_from_slice(&0u16.to_le_bytes()),
        Some(guid) => {
            body.extend_from_slice(&2u16.to_le_bytes());
            crate::wire::write_packed_guid(guid, &mut body).expect("vec write");
        }
    }
    body
}

/// `TARGET_FLAG_LOCKED` — set alongside `GAMEOBJECT` when the client casts an OPEN_LOCK spell at a
/// lockable object (decision 0239). It carries **no** wire data of its own (VERIFIED vmangos
/// `SpellCastTargets` read/write — `LOCKED` is in no read branch); the real client sends the bit, so
/// benilla mirrors it.
const TARGET_FLAG_LOCKED: u16 = 0x4000;

/// Body of `CMSG_CAST_SPELL` aimed at a **GameObject** (decision 0239): the OPEN_LOCK cast a
/// right-click on a locked chest / mining vein / herb node sends instead of `CMSG_GAMEOBJ_USE`.
/// `spell_id`, then the target mask `GAMEOBJECT | LOCKED` (0x4800), then the GameObject's **packed**
/// guid — the only field the mask reads (the GameObject branch reads one packed guid; `LOCKED` adds
/// nothing). Distinct from [`cast_spell`]'s unit/self target shape (flag `0x2` + packed guid).
pub fn cast_spell_gameobject(spell_id: u32, go_guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&spell_id.to_le_bytes());
    body.extend_from_slice(&(TARGET_FLAG_GAMEOBJECT | TARGET_FLAG_LOCKED).to_le_bytes());
    crate::wire::write_packed_guid(go_guid, &mut body).expect("vec write");
    body
}

/// Body of `CMSG_CAST_SPELL` aimed at an **ITEM** (decision 0437 phase 3): the enchant/poison
/// cast the CraftFrame's item pick completes. `spell_id`, mask `TARGET_FLAG_ITEM (0x0010)`, then
/// the item's **packed** guid — the one field the mask reads (vmangos `SpellCastTargets::read`,
/// `SpellCastTargetsInfo.cpp:159-160`: `ITEM | TRADE_ITEM → one packed guid`; the trade-window
/// sentinel form (`TRADE_ITEM 0x1000`) is the player-trade arc's, not built here).
pub fn cast_spell_item(spell_id: u32, item_guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&spell_id.to_le_bytes());
    body.extend_from_slice(&TARGET_FLAG_ITEM.to_le_bytes());
    crate::wire::write_packed_guid(item_guid, &mut body).expect("vec write");
    body
}

/// Body of `CMSG_CAST_SPELL` aimed at a **ground point** (decision 0792): the targeting-cursor
/// commit for a `Targets & 0x40` spell (Blizzard, Flamestrike, Rain of Fire…). `spell_id`, mask
/// `DEST_LOCATION (0x0040)`, then the destination as three `f32` **WoW world coords** — the one
/// field the mask reads (vmangos `SpellCastTargets::read`, `SpellCastTargetsInfo.cpp:169-174`:
/// `DEST_LOCATION → x,y,z`, `IsValidMapCoord`-gated). The real client ships the same shape from
/// `SPELLCAST+0x3c..0x44` (`BindLocation 0x6e60f0` → `SendCast 0x6e54f0`, wow-re `wave-cast.md`).
pub fn cast_spell_at_dest(spell_id: u32, dest: [f32; 3]) -> Vec<u8> {
    let mut body = Vec::with_capacity(18);
    body.extend_from_slice(&spell_id.to_le_bytes());
    body.extend_from_slice(&TARGET_FLAG_DEST_LOCATION.to_le_bytes());
    for c in dest {
        body.extend_from_slice(&c.to_le_bytes());
    }
    body
}

/// Body of `CMSG_CANCEL_AURA` (vmangos `WorldPackets::Spell::CancelAura`, `Server/Packets/Spell.h:55-62`):
/// one `u32` spell id. The server cancels **by spell, not by slot** — `HandleCancelAuraOpcode`
/// (`SpellHandler.cpp:333-405`) looks the spell up, refuses passives, `SPELL_ATTR_NO_AURA_CANCEL`
/// spells and debuffs, then calls `RemoveAurasDueToSpellByCancel`. The wire's own
/// `AURA_FLAG_CANCELABLE` nibble bit is the matching client-side gate (decision 0255).
pub fn cancel_aura(spell_id: u32) -> Vec<u8> {
    spell_id.to_le_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_spell_gameobject_body_golden() {
        // spell_id (LE) + target mask GAMEOBJECT|LOCKED = 0x4800 (LE `00 48`) + packed guid 0x1234
        // (mask 0x03, bytes 34 12). VERIFIED vmangos `SpellCastTargets`: LOCKED reads no bytes; the
        // GameObject branch reads one packed guid.
        assert_eq!(
            cast_spell_gameobject(1, 0x1234),
            [0x01, 0x00, 0x00, 0x00, 0x00, 0x48, 0x03, 0x34, 0x12],
            "CMSG_CAST_SPELL (GameObject/OPEN_LOCK) body"
        );
        // The unit-target twin still carries flag 0x2, not 0x4800 — the two shapes stay distinct.
        assert_eq!(cast_spell(1, None), [0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn cast_spell_at_dest_body_golden() {
        // spell_id 10 (Blizzard, LE) + mask DEST_LOCATION 0x0040 (LE `40 00`) + the dest Vec3 as
        // three f32 LE: 1.0 = 00 00 80 3F, -2.5 = 00 00 20 C0, 3.0 = 00 00 40 40. VERIFIED against
        // vmangos `SpellCastTargets::read` (mask → DEST branch reads exactly x, y, z).
        assert_eq!(
            cast_spell_at_dest(10, [1.0, -2.5, 3.0]),
            [
                0x0A, 0x00, 0x00, 0x00, // spell id
                0x40, 0x00, // TARGET_FLAG_DEST_LOCATION
                0x00, 0x00, 0x80, 0x3F, // x = 1.0
                0x00, 0x00, 0x20, 0xC0, // y = -2.5
                0x00, 0x00, 0x40, 0x40, // z = 3.0
            ],
            "CMSG_CAST_SPELL (ground dest) body"
        );
    }

    #[test]
    fn aura_bodies_golden() {
        // CMSG_CANCEL_AURA: a lone u32 spell id, LE. 1126 (Mark of the Wild) = 0x0000_0466. The
        // server cancels by spell — there is no slot byte to get wrong.
        assert_eq!(cancel_aura(1126), [0x66, 0x04, 0x00, 0x00]);

        // SMSG_UPDATE_AURA_DURATION: u8 slot, then u32 ms LE. Slot 3, 12_000 ms = 0x0000_2EE0.
        let body = [0x03, 0xE0, 0x2E, 0x00, 0x00];
        let mut r = &body[..];
        assert_eq!(read_update_aura_duration(&mut r).unwrap(), (3, 12_000));
        assert!(
            r.is_empty(),
            "the body is exactly 5 bytes — slot is a byte, not a dword"
        );
    }
}
