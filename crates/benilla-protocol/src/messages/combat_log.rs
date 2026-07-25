//! The **combat log** wire — the inbound "what just happened to whom" packets: spell damage,
//! periodic aura ticks, heals, power gains, damage shields, environmental damage, and a cast's
//! per-target miss list. Split out of `messages/spells.rs` (decision 0640), where these already sat
//! behind a hand-drawn `--- the combat-log wire ---` banner; the banner was the concern boundary the
//! file couldn't express.
//!
//! Every layout here is VERIFIED against vmangos source (cited per item). This family is **inbound
//! only** — nothing in it has an outbound counterpart, so unlike its siblings it has no
//! `world::writer` twin. Its consumer is the floating/center combat text (decisions 0137 phase 2,
//! 0578, 0580) plus the fall-damage dust puff (`EnvironmentalDamageLog`).
//!
//! The melee half of the same story is [`super::attack`]'s `SMSG_ATTACKERSTATEUPDATE`: a *swing*
//! reports itself there, a *spell* reports itself here.

use std::io::{self, Read};

use crate::wire::{read_f32_le, read_i32_le, read_packed_guid, read_u32_le, read_u64_le, read_u8};

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
/// the same vocabulary [`SpellGo`](crate::messages::SpellGo)'s own miss list carries): 1 MISS ·
/// 2 RESIST · 3 DODGE · 4 PARRY ·
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
