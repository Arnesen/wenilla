//! Spell-book, action-bar, cast, and melee-attack messages — the wire under decision 0068's
//! slice-1 action bar. Every layout here is VERIFIED against vmangos source (cited per item);
//! the opcodes are in [`super::opcode`] (verified `Opcodes_1_12_1.h`).

use std::io::{self, Read};

use crate::wire::{
    read_cstring, read_f32_le, read_i32_le, read_packed_guid, read_u16_le, read_u32_le,
    read_u64_le, read_u8, Vector3d,
};

/// Action-button kind byte (bits 24–31 of the packed slot word — vmangos `Player.h`
/// `ActionButtonType`): a spell id, a macro id, or an item id in the low 24 bits.
pub const ACTION_KIND_SPELL: u8 = 0x00;
pub const ACTION_KIND_MACRO: u8 = 0x40;
pub const ACTION_KIND_ITEM: u8 = 0x80;

/// One *occupied* action-bar slot from `SMSG_ACTION_BUTTONS`. The wire is 120 packed `u32`s
/// (`MAX_ACTION_BUTTONS`, vmangos `MasterPlayer::SendInitialActionButtons`) — `action` in bits
/// 0–23, `kind` in bits 24–31 (`ACTION_BUTTON_ACTION/TYPE`, `Player.h`); a zero word is an empty
/// slot and is not surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionButton {
    /// The bar slot index (0..119). Slots 0–11 are the main bar's buttons 1–12.
    pub slot: u8,
    /// The spell/macro/item id (bits 0–23).
    pub action: u32,
    /// The kind byte (bits 24–31): [`ACTION_KIND_SPELL`]/[`ACTION_KIND_MACRO`]/[`ACTION_KIND_ITEM`]
    /// (0x01 "click?" exists in the enum, carried raw if it ever appears).
    pub kind: u8,
}

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

/// `SMSG_CAST_RESULT`'s verdict: `u32 spellId, u8 status` — status `0` (`SPELL_RESULT_STATUS_OKAY`)
/// ends the packet, status `2` (`SPELL_RESULT_STATUS_FAIL`) appends a `u8` failure reason
/// (vmangos `CastResult::AppendBodyTo`; some reasons append extra arg words we skip).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CastOutcome {
    Ok,
    Failed { reason: u8 },
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

/// Read `SMSG_SUPERCEDED_SPELL` (vmangos `SupercededSpell::AppendBodyTo`, `Spell.cpp:169-173`): `u16
/// oldSpellId, u16 newSpellId` — a rank-up replaces the old spell with the new one in both the book
/// and the action bar (decision 0237).
pub(super) fn read_superceded_spell(r: &mut impl Read) -> io::Result<(u16, u16)> {
    Ok((read_u16_le(r)?, read_u16_le(r)?))
}

/// Read `SMSG_ACTION_BUTTONS`: packed `u32` per slot to end-of-body (the server sends exactly 120;
/// reading to the boundary keeps us robust to a different count). Zero words (empty slots) are
/// dropped; occupied slots surface as [`ActionButton`]s.
pub(super) fn read_action_buttons(r: &mut &[u8]) -> io::Result<Vec<ActionButton>> {
    let mut buttons = Vec::new();
    let mut slot: u32 = 0;
    while !r.is_empty() {
        let packed = read_u32_le(r)?;
        if packed != 0 {
            buttons.push(ActionButton {
                slot: slot.min(u8::MAX as u32) as u8,
                action: packed & 0x00FF_FFFF,
                kind: (packed >> 24) as u8,
            });
        }
        slot += 1;
    }
    Ok(buttons)
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

/// Read `SMSG_ATTACKSTART` (vmangos `AttackStart::AppendBodyTo`): two full `u64` guids.
pub(super) fn read_attack_start(r: &mut impl Read) -> io::Result<(u64, u64)> {
    Ok((read_u64_le(r)?, read_u64_le(r)?))
}

/// One decoded `SMSG_ATTACKERSTATEUPDATE` — a completed melee swing (vmangos
/// `Unit::SendAttackStateUpdate`, `Unit.cpp:4572-4605`; fired **exactly once per weapon-timer
/// cycle**, independently per hand — the real client plays one attacker swing animation per packet,
/// wow-re `combat-swing-anim.md`, decision 0073). The per-school sub-damage split collapses to the
/// packet's own `TotalDamage` plus the summed `absorb` (decision 0137 phase 2's floating combat
/// text feed); `hit_info` bit `0x4` marks an **offhand** swing (the anim selector keys on it), bit
/// `0x10000` suppresses the swing anim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttackerState {
    pub attacker: u64,
    pub victim: u64,
    pub hit_info: u32,
    /// `TotalDamage` — the swing's damage before the sub-damage split.
    pub damage: u32,
    /// `TargetState` (vmangos `VictimState`): 1 hit · 2 dodge · 3 parry · 4 interrupt · 5 blocks ….
    /// A defended outcome (dodge/parry/block/deflect) plays a dedicated victim defense clip at
    /// the swing's `$CPP` keyframe (decision 0279, correcting 0073's "never a body animation");
    /// landed hits flinch/bleed; the rest is sound + floating text.
    pub victim_state: u32,
    /// Sum of the per-sub-damage `absorb` fields (vmangos writes exactly one sub-damage block in
    /// practice; summed faithfully in case more than one ever rides the wire).
    pub absorb: u32,
    /// Sum of the per-sub-damage `resist` fields — the partial-resist trailer's amount
    /// (decision 0580's center-text fold-back).
    pub resist: i32,
    /// `BlockedAmount` — the trailing blocked-damage word.
    pub blocked: u32,
}

/// Read `SMSG_ATTACKERSTATEUPDATE` (byte-verified order, attacker **PackGUID first** — settled by
/// the wow-re §5 against the handler's downstream use, decision 0073): HitInfo · attacker PackGUID ·
/// victim PackGUID · TotalDamage · SubDamageCount + per-sub `{school u32, damage f32, damage u32,
/// absorb u32, resist i32}` · TargetState · two u32s (zero + "spell id, seen with heroic strike") ·
/// BlockedAmount.
pub(super) fn read_attacker_state(r: &mut impl Read) -> io::Result<AttackerState> {
    let hit_info = read_u32_le(r)?;
    let attacker = read_packed_guid(r)?;
    let victim = read_packed_guid(r)?;
    let damage = read_u32_le(r)?;
    let subs = read_u8(r)?;
    let mut absorb = 0u32;
    let mut resist = 0i32;
    for _ in 0..subs {
        // school, damage f32, damage u32 — folded into TotalDamage above; absorb/resist summed.
        let _school = read_u32_le(r)?;
        let _damage_f = read_f32_le(r)?;
        let _damage = read_u32_le(r)?;
        absorb += read_u32_le(r)?;
        resist += read_u32_le(r)? as i32;
    }
    let victim_state = read_u32_le(r)?;
    let _zero = read_u32_le(r)?;
    let _spell_id = read_u32_le(r)?;
    let blocked = read_u32_le(r)?;
    Ok(AttackerState {
        attacker,
        victim,
        hit_info,
        damage,
        victim_state,
        absorb,
        resist,
        blocked,
    })
}

/// Read `SMSG_ATTACKSTOP` (vmangos `AttackStop::AppendBodyTo`): two **packed** guids + a `u32`
/// "victim is dead" word (dropped — death arrives through the descriptor seam).
pub(super) fn read_attack_stop(r: &mut impl Read) -> io::Result<(u64, u64)> {
    let attacker = read_packed_guid(r)?;
    let victim = read_packed_guid(r)?;
    let _is_dead = read_u32_le(r)?;
    Ok((attacker, victim))
}

/// Read `SMSG_AI_REACTION` → `(unit, reaction)` (vmangos `Creature::SendAIReaction`,
/// `Objects/Creature.cpp:2490-2498` → `WorldPackets::Misc::AiReaction::AppendBodyTo`,
/// `Server/Packets/Misc.cpp:445-449`): a raw (unpacked) `u64` guid + a `u32` reaction. Broadcast
/// with reaction 2 (HOSTILE) on every creature melee-attack start (`Unit::Attack`) and 0 (ALERT)
/// on stealth pre-aggro detection (`CreatureAI::TriggerAlertDirect`); 1/4 exist server-side but
/// are never sent (decision 0277).
pub(super) fn read_ai_reaction(r: &mut impl Read) -> io::Result<(u64, u32)> {
    let unit = read_u64_le(r)?;
    let reaction = read_u32_le(r)?;
    Ok((unit, reaction))
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

/// Body of `CMSG_ATTACKSWING` (vmangos `AttackSwing::ReadFromWorldPacket`): one full `u64` victim
/// guid. Starts melee auto-attack; the server answers `SMSG_ATTACKSTART` (or an attack-swing error
/// packet). `CMSG_ATTACKSTOP`'s body is empty — no builder needed.
pub fn attack_swing(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_SET_ACTION_BUTTON` (VERIFIED vmangos `WorldPackets::Misc::SetActionButton::
/// ReadFromWorldPacket`, `Server/Packets/Misc.cpp:87-90`; opcode 296 `Opcodes_1_12_1.h:299`):
/// `button u8` + `packetData u32` (`action | kind<<24`, [`ActionButton`]'s own packing) — 5
/// bytes. `packed == 0` clears the slot (`HandleSetActionButtonOpcode`'s `!packet.packetData`
/// branch calls `removeActionButton`, never sent back over the wire — decision 0216 §7/0218 §4:
/// the client sends ONE of these per local slot mutation, a drag-swap is two sends, never atomic).
pub fn set_action_button(button: u8, packed: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(5);
    body.push(button);
    body.extend_from_slice(&packed.to_le_bytes());
    body
}

/// Body of `CMSG_SETSHEATHED` (vmangos `SetSheathed::ReadFromWorldPacket`: `recv_data >> sheathed`):
/// one `u32` sheath state (0 unarmed/stowed, 1 melee drawn, 2 ranged drawn). Purely
/// client-volunteered — `HandleSetSheathedOpcode` (`CombatHandler.cpp:80-87`) just stores whatever
/// we send via `Unit::SetSheath`, which lands in our own `UNIT_FIELD_BYTES_2` and relays to nearby
/// observers on the next values update; the server has no independent way to know a weapon is drawn.
pub fn set_sheathed(state: u32) -> Vec<u8> {
    state.to_le_bytes().to_vec()
}

/// Body of `CMSG_CANCEL_AURA` (vmangos `WorldPackets::Spell::CancelAura`, `Server/Packets/Spell.h:55-62`):
/// one `u32` spell id. The server cancels **by spell, not by slot** — `HandleCancelAuraOpcode`
/// (`SpellHandler.cpp:333-405`) looks the spell up, refuses passives, `SPELL_ATTR_NO_AURA_CANCEL`
/// spells and debuffs, then calls `RemoveAurasDueToSpellByCancel`. The wire's own
/// `AURA_FLAG_CANCELABLE` nibble bit is the matching client-side gate (decision 0255).
pub fn cancel_aura(spell_id: u32) -> Vec<u8> {
    spell_id.to_le_bytes().to_vec()
}

// --- the combat-log wire (decision 0137 phase 2's floating-combat-text data feed) --------------------

/// One decoded `SMSG_SPELLNONMELEEDAMAGELOG` — non-melee (spell) damage dealt (vmangos
/// `WorldPackets::Spell::SpellNonMeleeDamageLog::AppendBodyTo`, `Server/Packets/Spell.cpp:124-140` +
/// `Spell.h:178-198`). `hit_info` bit `0x2` is `SPELL_HIT_TYPE_CRIT` (vmangos `SpellDefines.h:179`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellDamageLog {
    pub target: u64,
    pub attacker: u64,
    pub spell_id: u32,
    pub damage: u32,
    pub school: u8,
    pub absorb: u32,
    pub resist: i32,
    pub periodic: bool,
    pub blocked: u32,
    pub hit_info: u32,
}

/// Read `SMSG_SPELLNONMELEEDAMAGELOG`: target PackedGuid · attacker PackedGuid · spellId u32 ·
/// damage u32 · school u8 · absorbed u32 · resist i32 · periodicLog u8 (bool) · unused u8 · blocked
/// u32 · hitInfo u32 · extendedData u8 (always 0 — read and dropped).
pub(super) fn read_spell_damage_log(r: &mut impl Read) -> io::Result<SpellDamageLog> {
    let target = read_packed_guid(r)?;
    let attacker = read_packed_guid(r)?;
    let spell_id = read_u32_le(r)?;
    let damage = read_u32_le(r)?;
    let school = read_u8(r)?;
    let absorb = read_u32_le(r)?;
    let resist = read_i32_le(r)?;
    let periodic = read_u8(r)? != 0;
    let _unused = read_u8(r)?;
    let blocked = read_u32_le(r)?;
    let hit_info = read_u32_le(r)?;
    let _extended_data = read_u8(r)?;
    Ok(SpellDamageLog {
        target,
        attacker,
        spell_id,
        damage,
        school,
        absorb,
        resist,
        periodic,
        blocked,
        hit_info,
    })
}

/// One tick of `SMSG_PERIODICAURALOG` — the payload shape depends on the tick's `AuraType` (vmangos
/// `SpellAuraDefines.h`): `PERIODIC_DAMAGE` (3) / `PERIODIC_DAMAGE_PERCENT` (89) carry a damage
/// breakdown; `PERIODIC_HEAL` (8) / `OBS_MOD_HEALTH` (20) a plain heal amount; `OBS_MOD_MANA` (21) /
/// `PERIODIC_ENERGIZE` (24) a power+amount pair; `PERIODIC_MANA_LEECH` (64) a power+amount+multiplier
/// triple.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PeriodicTick {
    Damage {
        amount: u32,
        school: u32,
        absorb: u32,
        resist: i32,
    },
    Heal {
        amount: u32,
    },
    Energize {
        power: u32,
        amount: u32,
    },
    ManaLeech {
        power: u32,
        amount: u32,
        multiplier: f32,
    },
}

/// One decoded `SMSG_PERIODICAURALOG` — periodic (DoT/HoT/regen) aura ticks (vmangos
/// `Unit::SendPeriodicAuraLog`, `Unit.cpp:4395-4443`). vmangos always writes `count == 1`; the loop
/// is decoded faithfully regardless.
#[derive(Debug, Clone, PartialEq)]
pub struct PeriodicAuraLog {
    pub target: u64,
    pub caster: u64,
    pub spell_id: u32,
    pub ticks: Vec<PeriodicTick>,
}

const AURA_PERIODIC_DAMAGE: u32 = 3;
const AURA_PERIODIC_HEAL: u32 = 8;
const AURA_OBS_MOD_HEALTH: u32 = 20;
const AURA_OBS_MOD_MANA: u32 = 21;
const AURA_PERIODIC_ENERGIZE: u32 = 24;
const AURA_PERIODIC_MANA_LEECH: u32 = 64;
const AURA_PERIODIC_DAMAGE_PERCENT: u32 = 89;

/// Read `SMSG_PERIODICAURALOG`: target PackedGuid · caster PackedGuid · spellId u32 · count u32 ·
/// `count` entries of `{auraType u32, payload}` — see [`PeriodicTick`] for the payload shapes. An
/// aura type outside that set cannot be skipped without desyncing the stream, so it errors instead.
pub(super) fn read_periodic_aura_log(r: &mut impl Read) -> io::Result<PeriodicAuraLog> {
    let target = read_packed_guid(r)?;
    let caster = read_packed_guid(r)?;
    let spell_id = read_u32_le(r)?;
    let count = read_u32_le(r)?;
    let mut ticks = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let aura_type = read_u32_le(r)?;
        let tick = match aura_type {
            AURA_PERIODIC_DAMAGE | AURA_PERIODIC_DAMAGE_PERCENT => PeriodicTick::Damage {
                amount: read_u32_le(r)?,
                school: read_u32_le(r)?,
                absorb: read_u32_le(r)?,
                resist: read_i32_le(r)?,
            },
            AURA_PERIODIC_HEAL | AURA_OBS_MOD_HEALTH => PeriodicTick::Heal {
                amount: read_u32_le(r)?,
            },
            AURA_OBS_MOD_MANA | AURA_PERIODIC_ENERGIZE => PeriodicTick::Energize {
                power: read_u32_le(r)?,
                amount: read_u32_le(r)?,
            },
            AURA_PERIODIC_MANA_LEECH => PeriodicTick::ManaLeech {
                power: read_u32_le(r)?,
                amount: read_u32_le(r)?,
                multiplier: read_f32_le(r)?,
            },
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("SMSG_PERIODICAURALOG: unknown aura type {other}"),
                ))
            }
        };
        ticks.push(tick);
    }
    Ok(PeriodicAuraLog {
        target,
        caster,
        spell_id,
        ticks,
    })
}

/// One decoded `SMSG_SPELLHEALLOG` — a direct heal landing (vmangos
/// `WorldPackets::Spell::SpellHealLog::AppendBodyTo`, `Server/Packets/Spell.cpp:105-112` +
/// `Spell.h:151-163`) — the center combat text's HEAL/HEAL_CRIT feed (decision 0578).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellHealLog {
    pub target: u64,
    pub healer: u64,
    pub spell_id: u32,
    pub amount: u32,
    pub critical: bool,
}

/// Read `SMSG_SPELLHEALLOG`: target PackedGuid · healer PackedGuid · spellId u32 · amount u32 ·
/// critical u8 (bool).
pub(super) fn read_spell_heal_log(r: &mut impl Read) -> io::Result<SpellHealLog> {
    Ok(SpellHealLog {
        target: read_packed_guid(r)?,
        healer: read_packed_guid(r)?,
        spell_id: read_u32_le(r)?,
        amount: read_u32_le(r)?,
        critical: read_u8(r)? != 0,
    })
}

/// One decoded `SMSG_SPELLENERGIZELOG` — an instant power gain (vmangos
/// `WorldPackets::Spell::SpellEnergizeLog::AppendBodyTo`, `Server/Packets/Spell.cpp:114-121` +
/// `Spell.h:165-176`). `power` is the vmangos `Powers` enum (0 mana · 1 rage · 2 focus ·
/// 3 energy · 4 happiness) — the center combat text's MANA/RAGE/FOCUS/ENERGY feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellEnergizeLog {
    pub target: u64,
    pub caster: u64,
    pub spell_id: u32,
    pub power: u32,
    pub amount: u32,
}

/// Read `SMSG_SPELLENERGIZELOG`: target PackedGuid · caster PackedGuid · spellId u32 ·
/// powerType u32 · amount u32.
pub(super) fn read_spell_energize_log(r: &mut impl Read) -> io::Result<SpellEnergizeLog> {
    Ok(SpellEnergizeLog {
        target: read_packed_guid(r)?,
        caster: read_packed_guid(r)?,
        spell_id: read_u32_le(r)?,
        power: read_u32_le(r)?,
        amount: read_u32_le(r)?,
    })
}

/// One decoded `SMSG_SPELLDAMAGESHIELD` — a damage-shield (Thorns-style) return hit (vmangos
/// `WorldPackets::Combat::SpellDamageShield::AppendBodyTo`, `Server/Packets/Combat.cpp:73-79` +
/// `Combat.h:124-134`). `victim` is the shield's bearer; `attacker` is the unit that struck them and
/// now **receives** this damage back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageShield {
    pub victim: u64,
    pub attacker: u64,
    pub damage: u32,
    pub school: u32,
}

/// Read `SMSG_SPELLDAMAGESHIELD`: victim raw `u64` guid · attacker raw `u64` guid · damage u32 ·
/// school u32.
pub(super) fn read_damage_shield(r: &mut impl Read) -> io::Result<DamageShield> {
    Ok(DamageShield {
        victim: read_u64_le(r)?,
        attacker: read_u64_le(r)?,
        damage: read_u32_le(r)?,
        school: read_u32_le(r)?,
    })
}

/// One decoded `SMSG_ENVIRONMENTALDAMAGELOG` — environmental damage taken: fall, drowning,
/// fatigue, lava, slime, fire (vmangos `Unit::SendEnvironmentalDamageLog`, `Objects/Unit.cpp:5392`
/// → `WorldPackets::Combat::EnvironmentalDamageLog::AppendBodyTo`, `Server/Packets/Combat.cpp:58-67`;
/// the absorb/resist tail is the `> 1.6.1` layout our 5875 wire carries). `damage_type` is
/// vmangos `EnvironmentalDamageType` (`Objects/Player.h:590`): 0 exhausted · 1 drowning ·
/// 2 **fall** · 3 lava · 4 slime · 5 fire — the index into the client's `EnvironmentalDamage.dbc`
/// 6-slot damage-type → SpellVisualKit table (its fall row is the landing dust puff).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentalDamageLog {
    pub victim: u64,
    pub damage_type: u8,
    pub damage: u32,
    pub absorb: u32,
    pub resist: i32,
}

/// Read `SMSG_ENVIRONMENTALDAMAGELOG`: victim raw `u64` guid (vmangos `ObjectGuid.cpp:174`
/// streams the raw value; the client reads it with its plain 8-byte guid reader `0x4190b0`) ·
/// damageType u8 · damage u32 · absorbed u32 · resist i32.
pub(super) fn read_environmental_damage_log(
    r: &mut impl Read,
) -> io::Result<EnvironmentalDamageLog> {
    Ok(EnvironmentalDamageLog {
        victim: read_u64_le(r)?,
        damage_type: read_u8(r)?,
        damage: read_u32_le(r)?,
        absorb: read_u32_le(r)?,
        resist: read_i32_le(r)?,
    })
}

/// One decoded `SMSG_SPELLLOGMISS` — a spell cast's per-target miss list (vmangos
/// `WorldPackets::Spell::SpellLogMiss::AppendBodyTo`, `Server/Packets/Spell.cpp:68-86` +
/// `Spell.h:109-124`). Each entry's `u8` is a `SpellMissInfo` (vmangos `SpellDefines.h:160-174`,
/// same vocabulary as [`SPELL_MISS_REFLECT`]'s family): 1 MISS · 2 RESIST · 3 DODGE · 4 PARRY ·
/// 5 BLOCK · 6 EVADE · 7/8 IMMUNE · 9 DEFLECT · 10 ABSORB · 11 REFLECT.
#[derive(Debug, Clone, PartialEq)]
pub struct SpellLogMiss {
    pub spell_id: u32,
    pub caster: u64,
    pub misses: Vec<(u64, u8)>,
}

/// Read `SMSG_SPELLLOGMISS`: spellId u32 · caster raw `u64` · useExtended u8 (vmangos always 0) ·
/// count u32 · `count` entries of `{target raw u64, missInfo u8}`. When `useExtended != 0`, each
/// entry additionally carries a trailing `2×f32` — read and dropped to keep the cursor aligned; no
/// consumer needs it.
pub(super) fn read_spell_log_miss(r: &mut impl Read) -> io::Result<SpellLogMiss> {
    let spell_id = read_u32_le(r)?;
    let caster = read_u64_le(r)?;
    let use_extended = read_u8(r)?;
    let count = read_u32_le(r)?;
    let mut misses = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let target = read_u64_le(r)?;
        let miss_info = read_u8(r)?;
        if use_extended != 0 {
            let _arg1 = read_f32_le(r)?;
            let _arg2 = read_f32_le(r)?;
        }
        misses.push((target, miss_info));
    }
    Ok(SpellLogMiss {
        spell_id,
        caster,
        misses,
    })
}

/// One decoded `SMSG_LOG_XPGAIN` — an XP award, kill or non-kill (vmangos
/// `WorldPackets::Misc::LogXpGain::AppendBodyTo`, `Server/Packets/Misc.cpp:512-522` +
/// `Misc.h:779-790`). `victim` is `0` for non-kill xp (quest/exploration/GM command).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XpGain {
    pub victim: u64,
    pub total: u32,
    /// The award before the rested bonus — only a kill carries it on the wire (rested applies
    /// to kill XP only, so a non-kill packet's `base` is `= total` here). `total − base` is the
    /// chat line's "(+N exp Rested bonus)" amount (decision 0304).
    pub base: u32,
    pub kill: bool,
}

/// Read `SMSG_LOG_XPGAIN`: victim raw `u64` (0 for non-kill) · totalXp u32 · xpType u8 (0 = kill,
/// 1 = non-kill) · iff `xpType == 0`: baseXp u32 · groupBonus f32 (read and dropped — composing
/// the "+N group bonus" line form waits for parties to exist; decision 0304 scope call).
pub(super) fn read_xp_gain(r: &mut impl Read) -> io::Result<XpGain> {
    let victim = read_u64_le(r)?;
    let total = read_u32_le(r)?;
    let xp_type = read_u8(r)?;
    let kill = xp_type == 0;
    let mut base = total;
    if kill {
        base = read_u32_le(r)?;
        let _group_bonus = read_f32_le(r)?;
    }
    Ok(XpGain {
        victim,
        total,
        base,
        kill,
    })
}

/// Body of `CMSG_LEARN_TALENT` (vmangos `Server/Packets/Skill.h:10-19` +
/// `Player::LearnTalent`, Player.cpp:20807): `u32 talentId` (a `Talent.dbc` row id) +
/// `u32 requestedRank`, **0-based** — requesting rank k learns *up to* k (the server spends
/// `k − current + 1` points), so the click sends the current rank count itself (the next
/// 0-based rank). Decision 0304.
pub fn learn_talent(talent_id: u32, requested_rank: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&talent_id.to_le_bytes());
    body.extend_from_slice(&requested_rank.to_le_bytes());
    body
}

/// One decoded `SMSG_LEVELUP_INFO` — our own ding, self-addressed only (vmangos
/// `WorldPackets::Misc::LevelUpInfo`, `Misc.h:793-802` + `Misc.cpp:524-532`; filled by
/// `Player::GiveLevel`, Player.cpp:3210-3218, and sent straight to the leveling session — no
/// guid). Decision 0304.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelUpInfo {
    /// The level just reached.
    pub level: u32,
    /// Hit points gained.
    pub health: u32,
    /// Power gains indexed mana, rage, focus, energy, happiness. The 1.12 server fills mana (or
    /// zero for non-mana classes) and leaves the rest zero; `powers[0]` is the reference chat
    /// line's mana arg.
    pub powers: [u32; 5],
    /// Stat gains indexed str, agi, stam, int, spirit — the reference chat's `SPELL_STAT0..4`
    /// order, one line per positive entry.
    pub stats: [u32; 5],
}

/// Read `SMSG_LEVELUP_INFO`: level u32 · healthGain u32 · powerGains u32×5 · statGains u32×5
/// (48 bytes, `Misc.cpp:524-532`).
pub(super) fn read_level_up_info(r: &mut impl Read) -> io::Result<LevelUpInfo> {
    let level = read_u32_le(r)?;
    let health = read_u32_le(r)?;
    let mut powers = [0u32; 5];
    for p in &mut powers {
        *p = read_u32_le(r)?;
    }
    let mut stats = [0u32; 5];
    for s in &mut stats {
        *s = read_u32_le(r)?;
    }
    Ok(LevelUpInfo {
        level,
        health,
        powers,
        stats,
    })
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
