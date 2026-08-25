//! Melee auto-attack messages — the attack start/stop edges, the per-swing damage report, and the
//! creature-aggro notice. Split out of `messages/spells.rs` (decision 0640); mirrored by
//! `world::writer::attack`.
//!
//! `SMSG_ATTACKERSTATEUPDATE` is the melee twin of [`super::combat_log`]'s spell reports: a *swing*
//! reports itself here (fired exactly once per weapon-timer cycle, independently per hand — which is
//! what makes it the animation driver, decision 0073), a *spell* reports itself there.
//! `SMSG_AI_REACTION` rides along because it is the same moment from the creature's side: vmangos
//! broadcasts it from `Unit::Attack`, i.e. the instant a creature decides to swing (decision 0277).

use std::io::{self, Read};

use crate::wire::{read_f32_le, read_packed_guid, read_u32_le, read_u64_le, read_u8};

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
    /// The **first** sub-damage block's school (`SpellSchools`: 0 physical … 6 arcane; vmangos
    /// writes `GetFirstSchoolInMask(subDamage->damageSchoolMask)`), `0` when the swing carried no
    /// sub-damage at all.
    ///
    /// It exists for the combat log's wording: a non-physical swing takes the `…SCHOOL` template
    /// ("You hit X for 5 fire damage.") and physical takes the plain one. The floating text has
    /// never needed it, which is why the field was read and dropped until B297.
    pub school: u8,
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
    let mut school = 0u8;
    for i in 0..subs {
        // damage f32 + damage u32 are folded into TotalDamage above; absorb/resist are summed. The
        // school is kept from the FIRST block only — the wording takes one word, and vmangos's
        // extra blocks are the off-school splits of the same swing.
        let block_school = read_u32_le(r)?;
        if i == 0 {
            school = u8::try_from(block_school).unwrap_or(0);
        }
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
        school,
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

/// Body of `CMSG_ATTACKSWING` (vmangos `AttackSwing::ReadFromWorldPacket`): one full `u64` victim
/// guid. Starts melee auto-attack; the server answers `SMSG_ATTACKSTART` (or an attack-swing error
/// packet). `CMSG_ATTACKSTOP`'s body is empty — no builder needed.
pub fn attack_swing(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}
