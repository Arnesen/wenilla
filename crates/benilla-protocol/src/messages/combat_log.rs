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

/// One decoded `SMSG_PARTYKILLLOG` — the killing blow, sent to the killer's party (vmangos
/// `WorldPackets::Combat::PartyKillLog::AppendBodyTo`, `Server/Packets/Combat.cpp:52-56`).
///
/// It is the **only** source of the "You have slain %s!" / "%s is slain by %s!" pair; the plain
/// "%s dies." line is not a packet at all — it rides the client's own unit-death reflex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartyKillLog {
    pub killer: u64,
    pub victim: u64,
}

/// Read `SMSG_PARTYKILLLOG`: killer raw `u64` guid · victim raw `u64` guid.
pub(super) fn read_party_kill_log(r: &mut impl Read) -> io::Result<PartyKillLog> {
    Ok(PartyKillLog {
        killer: read_u64_le(r)?,
        victim: read_u64_le(r)?,
    })
}

/// One decoded `SMSG_SPELLINSTAKILLLOG` — an instant kill (vmangos `Spell::EffectInstaKill`,
/// `Spells/SpellEffects.cpp:274-279`; the packet exists only for `> 1.11.2` clients, which 5875 is).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellInstaKillLog {
    pub victim: u64,
    pub spell_id: u32,
}

/// Read `SMSG_SPELLINSTAKILLLOG`: victim raw `u64` guid · spellId u32.
pub(super) fn read_spell_insta_kill_log(r: &mut impl Read) -> io::Result<SpellInstaKillLog> {
    Ok(SpellInstaKillLog {
        victim: read_u64_le(r)?,
        spell_id: read_u32_le(r)?,
    })
}

/// One decoded `SMSG_PROCRESIST` **or** `SMSG_SPELLORDAMAGE_IMMUNE` — the two share a body byte for
/// byte (vmangos `WorldPackets::Spell::ProcResist` / `SpellOrDamageImmune`,
/// `Server/Packets/Spell.cpp:88-102`) and differ only in which sentence they word.
///
/// `log_format` is vmangos's `logFormat` (`0` default, `1` debug); the reference reads it as the
/// "is periodic" flag that decides whether `CombatLogPeriodicSpells` gates the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpellOutcomeLog {
    pub caster: u64,
    pub target: u64,
    pub spell_id: u32,
    pub log_format: u8,
}

/// Read `SMSG_PROCRESIST` / `SMSG_SPELLORDAMAGE_IMMUNE`: caster raw `u64` · target raw `u64` ·
/// spellId u32 · logFormat u8.
pub(super) fn read_spell_outcome_log(r: &mut impl Read) -> io::Result<SpellOutcomeLog> {
    Ok(SpellOutcomeLog {
        caster: read_u64_le(r)?,
        target: read_u64_le(r)?,
        spell_id: read_u32_le(r)?,
        log_format: read_u8(r)?,
    })
}

/// One decoded `SMSG_SPELLDISPELLOG` — auras a dispel actually removed (vmangos
/// `Spell::EffectDispel`, `Spells/SpellEffects.cpp:2524-2539`; the guid pair is **packed** on the
/// `>= 1.12.1` branch our build takes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpellDispelLog {
    /// The unit the auras were on.
    pub victim: u64,
    /// The dispeller. The sentence never names them — `AURADISPEL*` words the bearer and the aura.
    pub caster: u64,
    pub spell_ids: Vec<u32>,
}

/// Read `SMSG_SPELLDISPELLOG`: victim PackedGuid · caster PackedGuid · count u32 · `count` × u32.
pub(super) fn read_spell_dispel_log(r: &mut impl Read) -> io::Result<SpellDispelLog> {
    let victim = read_packed_guid(r)?;
    let caster = read_packed_guid(r)?;
    let count = read_u32_le(r)?;
    let mut spell_ids = Vec::with_capacity(count.min(64) as usize);
    for _ in 0..count {
        spell_ids.push(read_u32_le(r)?);
    }
    Ok(SpellDispelLog {
        victim,
        caster,
        spell_ids,
    })
}

/// One decoded `SMSG_DISPEL_FAILED` — auras a dispel tried and failed to remove (vmangos
/// `Spell::EffectDispel`, `Spells/SpellEffects.cpp:2549-2555`).
///
/// **The list runs to the end of the packet** — vmangos writes no count, so the body's own length
/// is the terminator. The reference reads it the same way (`0x628c20` loops to end of buffer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispelFailed {
    pub caster: u64,
    pub victim: u64,
    pub spell_ids: Vec<u32>,
}

/// Read `SMSG_DISPEL_FAILED`: caster raw `u64` · victim raw `u64` · u32 spell ids to end of body.
pub(super) fn read_dispel_failed(r: &mut impl Read) -> io::Result<DispelFailed> {
    let caster = read_u64_le(r)?;
    let victim = read_u64_le(r)?;
    let mut spell_ids = Vec::new();
    loop {
        let mut b = [0u8; 4];
        match r.read_exact(&mut b) {
            Ok(()) => spell_ids.push(u32::from_le_bytes(b)),
            // The body ended — the only terminator this packet has.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }
    Ok(DispelFailed {
        caster,
        victim,
        spell_ids,
    })
}

/// One decoded `SMSG_ENCHANTMENTLOG` — an enchant landing on, or fading from, an item (vmangos
/// `WorldPackets::Item::EnchantmentLog::AppendBodyTo`, `Server/Packets/Item.cpp:235-242`;
/// filled by `Player::SendEnchantmentLog`, `Objects/Player.cpp:12049-12072`).
///
/// **An empty `caster` means the enchant FADED**, not that the server forgot who cast it — that is
/// vmangos's own comment on the field, and it is the two-way the reference switches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnchantmentLog {
    pub caster: u64,
    pub owner: u64,
    pub item_entry: u32,
    pub spell_id: u32,
    /// vmangos `showAffiliation`: false on the copy sent to the item's owner, true on the broadcast
    /// to everyone else. The reference lets it pick the msg-id selector on the ADD leg.
    pub show_affiliation: bool,
}

/// Read `SMSG_ENCHANTMENTLOG`: caster raw `u64` · owner raw `u64` · itemEntry u32 · spellId u32 ·
/// showAffiliation u8.
pub(super) fn read_enchantment_log(r: &mut impl Read) -> io::Result<EnchantmentLog> {
    Ok(EnchantmentLog {
        caster: read_u64_le(r)?,
        owner: read_u64_le(r)?,
        item_entry: read_u32_le(r)?,
        spell_id: read_u32_le(r)?,
        show_affiliation: read_u8(r)? != 0,
    })
}

/// One `SMSG_SPELLLOGEXECUTE` entry — a single (effect, target) row. The payload shape is decided
/// by the **effect id**, exactly as vmangos's `Spell::SendLogExecute` switch writes it
/// (`Spells/Spell.cpp:4694-4771`) and as the reference's own per-effect jump table reads it
/// (`0x5e8074 jmp [edx*4+0x5e8430]`, byte remap `0x5e845c`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExecuteLog {
    /// Effect 8 `POWER_DRAIN`. **Field order VERIFIED at the client's bytes, not just the server's**:
    /// the arm at `0x5e807b` reads guid → u32 → u32 → f32 and hands them to `0x62dbf0` →
    /// `0x627930(…, spellId, powerType, amount, multiplier)` — where `0x62793c cmp edi,4` and
    /// `0x62795d call 0x6278f0` (GetPowerTypeNoun) identify the SECOND u32 as the power type. So
    /// the wire is (amount, power), which is what vmangos writes, and the two agree.
    PowerDrain {
        target: u64,
        amount: u32,
        power: u32,
        multiplier: f32,
    },
    /// Effects 10 `HEAL` / 62 `HEAL_MAX_HEALTH`.
    Heal {
        target: u64,
        amount: u32,
        critical: bool,
    },
    /// Effect 30 `ENERGIZE`.
    Energize {
        target: u64,
        amount: u32,
        power: u32,
    },
    /// Effect 19 `ADD_EXTRA_ATTACKS` — "You gain %d extra attacks through %s."
    ExtraAttacks { target: u64, count: u32 },
    /// Effect 24 `CREATE_ITEM` — the tradeskill line. **No target guid**: the item entry is the
    /// whole payload.
    CreateItem { item_entry: u32 },
    /// Effect 68 `INTERRUPT_CAST` — the interrupted target and the spell that was interrupted.
    InterruptCast { target: u64, spell_id: u32 },
    /// Effect 101 `FEED_PET` — like [`Self::CreateItem`], an item entry alone.
    FeedPet { item_entry: u32 },
    /// Effect 111 `DURABILITY_DAMAGE`. Both fields are **signed**: `-1`/`-1` is the "all items"
    /// form, which the reference words with its own `SPELLDURABILITYDAMAGEALL*` family. vmangos
    /// calls the second field `unk`; the reference calls it the item SLOT and tests it with the
    /// entry for exactly that `-1` pair.
    DurabilityDamage {
        target: u64,
        item_entry: i32,
        slot: i32,
    },
    /// Every other effect vmangos logs — a bare target guid. The effect id is kept because it is
    /// what the reference's formatter switch keys on (open lock, dismiss pet, summon, dispel …).
    Target { target: u64 },
}

/// One decoded `SMSG_SPELLLOGEXECUTE` — "this cast's effects did these things" (vmangos
/// `Spell::SendLogExecute`, `Spells/Spell.cpp:4662-4778`).
///
/// The packet is a *list of lists*: one group per spell effect that logged anything, each group a
/// run of per-target rows. Groups keep their effect id because the whole formatter choice hangs off
/// it — one packet can carry a tradeskill create, an interrupt and a power drain at once.
#[derive(Debug, Clone, PartialEq)]
pub struct SpellLogExecute {
    pub caster: u64,
    pub spell_id: u32,
    /// `(effect id, the rows that effect logged)`, in wire order.
    pub effects: Vec<(u32, Vec<ExecuteLog>)>,
}

// vmangos `SpellEffects.h` effect ids, named for the arms that read a payload wider than a guid.
const EFFECT_POWER_DRAIN: u32 = 8;
const EFFECT_HEAL: u32 = 10;
const EFFECT_ADD_EXTRA_ATTACKS: u32 = 19;
const EFFECT_CREATE_ITEM: u32 = 24;
const EFFECT_ENERGIZE: u32 = 30;
const EFFECT_HEAL_MAX_HEALTH: u32 = 62;
const EFFECT_INTERRUPT_CAST: u32 = 68;
const EFFECT_FEED_PET: u32 = 101;
const EFFECT_DURABILITY_DAMAGE: u32 = 111;

/// Read `SMSG_SPELLLOGEXECUTE`: caster PackedGuid · spellId u32 · effectCount u32 · per group
/// `{effect u32, count u32, count × payload}` — the payload per [`ExecuteLog`].
///
/// An effect id outside the set vmangos logs cannot be skipped without desyncing the body (the row
/// width is not on the wire), so it errors rather than guessing. vmangos never sends one: its own
/// switch `return`s before `SendMessageToSet` for anything it has no case for, so an unlisted
/// effect means no packet at all rather than a truncated one.
pub(super) fn read_spell_log_execute(r: &mut impl Read) -> io::Result<SpellLogExecute> {
    let caster = read_packed_guid(r)?;
    let spell_id = read_u32_le(r)?;
    let group_count = read_u32_le(r)?;
    let mut effects = Vec::with_capacity(group_count.min(8) as usize);
    for _ in 0..group_count {
        let effect = read_u32_le(r)?;
        let rows = read_u32_le(r)?;
        let mut out = Vec::with_capacity(rows.min(64) as usize);
        for _ in 0..rows {
            out.push(match effect {
                EFFECT_POWER_DRAIN => ExecuteLog::PowerDrain {
                    target: read_u64_le(r)?,
                    amount: read_u32_le(r)?,
                    power: read_u32_le(r)?,
                    multiplier: read_f32_le(r)?,
                },
                EFFECT_HEAL | EFFECT_HEAL_MAX_HEALTH => ExecuteLog::Heal {
                    target: read_u64_le(r)?,
                    amount: read_u32_le(r)?,
                    critical: read_u8(r)? != 0,
                },
                EFFECT_ENERGIZE => ExecuteLog::Energize {
                    target: read_u64_le(r)?,
                    amount: read_u32_le(r)?,
                    power: read_u32_le(r)?,
                },
                EFFECT_ADD_EXTRA_ATTACKS => ExecuteLog::ExtraAttacks {
                    target: read_u64_le(r)?,
                    count: read_u32_le(r)?,
                },
                EFFECT_CREATE_ITEM => ExecuteLog::CreateItem {
                    item_entry: read_u32_le(r)?,
                },
                EFFECT_INTERRUPT_CAST => ExecuteLog::InterruptCast {
                    target: read_u64_le(r)?,
                    spell_id: read_u32_le(r)?,
                },
                EFFECT_FEED_PET => ExecuteLog::FeedPet {
                    item_entry: read_u32_le(r)?,
                },
                EFFECT_DURABILITY_DAMAGE => ExecuteLog::DurabilityDamage {
                    target: read_u64_le(r)?,
                    item_entry: read_i32_le(r)?,
                    slot: read_i32_le(r)?,
                },
                // The long tail of vmangos's switch: instakill, dispel, threat, summon, open lock,
                // dismiss pet … all one guid wide.
                _ if rows_are_guid_only(effect) => ExecuteLog::Target {
                    target: read_u64_le(r)?,
                },
                other => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("SMSG_SPELLLOGEXECUTE: unknown effect {other}"),
                    ))
                }
            });
        }
        effects.push((effect, out));
    }
    Ok(SpellLogExecute {
        caster,
        spell_id,
        effects,
    })
}

/// The effects vmangos logs as a bare target guid — its `SendLogExecute` switch's long
/// fall-through case list (`Spells/Spell.cpp:4735-4770`), each name resolved against
/// `Spells/SpellDefines.h`'s `SpellEffects` enum rather than transcribed from memory.
fn rows_are_guid_only(effect: u32) -> bool {
    matches!(
        effect,
        1     // INSTAKILL
            | 18  // RESURRECT
            | 28  // SUMMON
            | 33  // OPEN_LOCK
            | 38  // DISPEL
            | 41  // SUMMON_WILD
            | 42  // SUMMON_GUARDIAN
            | 50  // TRANS_DOOR
            | 56  // SUMMON_PET
            | 59  // OPEN_LOCK_ITEM
            | 63  // THREAT
            | 69  // DISTRACT
            | 73  // SUMMON_POSSESSED
            | 74  // SUMMON_TOTEM
            | 76  // SUMMON_OBJECT_WILD
            | 79  // SANCTUARY
            | 87..=90 // SUMMON_TOTEM_SLOT1..4
            | 91  // THREAT_ALL
            | 97  // SUMMON_CRITTER
            | 102 // DISMISS_PET
            | 104..=107 // SUMMON_OBJECT_SLOT1..4
            | 108 // DISPEL_MECHANIC
            | 112 // SUMMON_DEMON
            | 113 // RESURRECT_NEW
            | 114 // ATTACK_ME
            | 116 // SKIN_PLAYER_CORPSE
            | 125 // MODIFY_THREAT_PERCENT
            | 126 // SPELL_EFFECT_126
    )
}
