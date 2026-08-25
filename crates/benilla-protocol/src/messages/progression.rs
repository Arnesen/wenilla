//! Character-progression messages — XP awarded, the level-up summary, and the talent spend. Split
//! out of `messages/spells.rs` (decision 0640).
//!
//! One decision produced all three (**0304**), and they are one arc on the wire: kill or complete
//! something → `SMSG_LOG_XPGAIN` → (on a ding) `SMSG_LEVELUP_INFO` → the points it granted go back
//! out as `CMSG_LEARN_TALENT`. Layouts VERIFIED against vmangos (cited per item).

use std::io::{self, Read};

use crate::wire::{read_f32_le, read_u32_le, read_u64_le, read_u8};

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

/// One decoded `SMSG_EXPLORATION_EXPERIENCE` — a first visit to an area: the newly explored
/// `AreaTable.dbc` row **id** (not the explore-flag bit) and the XP the discovery granted
/// (vmangos `WorldPackets::Misc::ExplorationExperience`, `Server/Packets/Misc.h:838-846` +
/// `Misc.cpp:552-556`; filled by `Player::CheckAreaExploreAndOutdoor`, Player.cpp:6228-6341,
/// which sends it **even when xp is 0** — max level, or an area with no level). The XP itself
/// also rides a separate non-kill [`XpGain`]; this packet is what names the area. Decision 0828.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExplorationXp {
    /// The `AreaTable.dbc` row id of the area just discovered.
    pub area_id: u32,
    /// The exploration XP awarded (0 at max level / unleveled areas).
    pub xp: u32,
}

/// Read `SMSG_EXPLORATION_EXPERIENCE`: areaId u32 · xp u32 (8 bytes, `Misc.cpp:552-556`).
pub(super) fn read_exploration_xp(r: &mut impl Read) -> io::Result<ExplorationXp> {
    let area_id = read_u32_le(r)?;
    let xp = read_u32_le(r)?;
    Ok(ExplorationXp { area_id, xp })
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

/// One decoded **inbound** `MSG_TALENT_WIPE_CONFIRM` — a class trainer asking whether to unlearn
/// every talent (vmangos `WorldPackets::Skill::TalentWipeConfirmResponse`,
/// `Server/Packets/Skill.h:39-52` + `Skill.cpp:19-23`, filled by `Player::SendTalentWipeConfirm`,
/// `Player.cpp:8338`). Decision 1580.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TalentWipeConfirm {
    /// The trainer that asked — echoed back in the outbound answer, where it is load-bearing
    /// (`HandleTalentWipeConfirmOpcode` resolves it to a live `UNIT_NPC_FLAG_TRAINER` in range and
    /// drops the request otherwise), exactly like the innkeeper guid in [`super::binder`].
    ///
    /// **Zero means "you have no talents to reset"** — vmangos's own comment on the field, and the
    /// value its refusal path sends (`SkillHandler.cpp:52`, after a failed `ResetTalents`). There
    /// is no trainer to ask about, so there is nothing to put on screen.
    pub trainer: u64,
    /// What the reset costs, in copper — `Player::GetResetTalentsCost()`, which climbs with every
    /// reset (the dialog's money frame, and the line "The cost will increase each time you do it").
    pub cost: u32,
}

/// Read the inbound `MSG_TALENT_WIPE_CONFIRM`: trainer guid u64 · cost u32 (`Skill.cpp:19-23`).
pub(super) fn read_talent_wipe_confirm(r: &mut impl Read) -> io::Result<TalentWipeConfirm> {
    Ok(TalentWipeConfirm {
        trainer: read_u64_le(r)?,
        cost: read_u32_le(r)?,
    })
}

/// Body of the **outbound** `MSG_TALENT_WIPE_CONFIRM` — the `CONFIRM_TALENT_WIPE` dialog's Accept
/// (vmangos `WorldPackets::Skill::TalentWipeConfirm`, `Skill.cpp:14-17`: one guid, no cost). The
/// guid is the one the question arrived with; the real client sends its own latched copy
/// (`0xc4d7a0`, wow-re `talent-api.md`), which is the same number. Decision 1580.
pub fn talent_wipe_confirm(trainer_guid: u64) -> Vec<u8> {
    trainer_guid.to_le_bytes().to_vec()
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

    /// The respec pair, both directions: the question carries guid + cost, the answer carries the
    /// guid alone. Asymmetric bodies on ONE opcode is the whole reason this is an `MSG_`.
    #[test]
    fn the_talent_wipe_pair_round_trips() {
        let trainer: u64 = 0xF130_0000_0000_2A01;
        let mut body = trainer.to_le_bytes().to_vec();
        body.extend_from_slice(&15_000u32.to_le_bytes());
        assert_eq!(
            read_talent_wipe_confirm(&mut &body[..]).unwrap(),
            TalentWipeConfirm {
                trainer,
                cost: 15_000
            }
        );
        assert_eq!(talent_wipe_confirm(trainer), trainer.to_le_bytes().to_vec());
    }

    /// vmangos's "you have no talents to reset" reply — guid 0, and a cost that is still filled in.
    /// It parses; what to DO with it is the app's call (decision 1580: there is no trainer to ask
    /// about, so nothing goes on screen).
    #[test]
    fn a_zero_trainer_is_a_parseable_refusal() {
        let mut body = 0u64.to_le_bytes().to_vec();
        body.extend_from_slice(&0u32.to_le_bytes());
        let ask = read_talent_wipe_confirm(&mut &body[..]).unwrap();
        assert_eq!(ask.trainer, 0);
    }
}
