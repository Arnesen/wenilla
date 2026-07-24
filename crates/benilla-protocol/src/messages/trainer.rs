//! Trainer messages — the class/profession trainer window's wire (opcodes 432-436, vmangos
//! `Opcodes_1_12_1.h:433-437`, VERIFIED). Bodies from vmangos `NPCHandler.cpp` (`SendTrainerList` +
//! its per-service helper `SendTrainerSpellHelper`) and the hand-serialized `Server/Packets/Npc.cpp`
//! (the `TrainerList`/`TrainerBuySpell`/`TrainerBuySucceeded`/`TrainerBuyFailed` structs). One frame
//! (`ClassTrainerFrame`) serves both class and tradeskill trainers; the window is reached through the
//! gossip trainer option (`GOSSIP_OPTION_TRAINER`, [`super::gossip`]), not a trainer-specific open
//! verb — so this arc is the receive side plus the two send verbs (decision 0237).

use std::io;

use crate::wire::{read_cstring, read_u32_le, read_u64_le, read_u8};

/// One trainer service (`SMSG_TRAINER_LIST`, vmangos `SendTrainerSpellHelper`,
/// `NPCHandler.cpp:97-139`) — a spell or tradeskill step the trainer can teach. The 38-byte wire
/// record; the list is **pre-filtered server-side** to services fitting the player's class/race, so
/// what arrives is already the player's own menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrainerSpell {
    /// The service spell id — what `CMSG_TRAINER_BUY_SPELL` names to purchase it (see
    /// [`trainer_buy_spell`]).
    pub spell: u32,
    /// A [`trainer_spell_state`] byte: the green/red/gray colouring the client renders (it does not
    /// recompute it). `GREEN_DISABLED` (server-internal, 10) never rides the wire — it is sent as
    /// `GREEN`.
    pub state: u8,
    /// Cost in copper, already reputation-discounted server-side.
    pub cost: u32,
    /// Wire field 4 (`primary_prof_first_rank && can_learn_primary_prof`): the client enables the
    /// "Learn" button only when this equals [`is_primary_prof_first_rank`] — so a primary-profession
    /// first rank you *can't* take (already holding two professions) arrives as `false` here with
    /// `is_primary_prof_first_rank == true`, greying the button.
    pub can_learn_primary_prof: bool,
    /// Wire field 5 (`primary_prof_first_rank`): this service is a first-rank primary profession, so
    /// taking it spends a profession slot (drives the client's confirmation dialog).
    pub is_primary_prof_first_rank: bool,
    /// Character level required to learn (`spellLevel`).
    pub req_level: u8,
    /// Required `SkillLine.dbc` id (0 = none) — e.g. the tradeskill this step belongs to.
    pub req_skill: u32,
    /// Required skill points in [`req_skill`] (0 = none).
    pub req_skill_value: u32,
    /// Up to three prerequisite spell ids (`SpellChainNode` req/prev + a trailing slot). The trailing
    /// slot is structurally always 0 on 5875; kept as an array to match the client's own
    /// `GetTrainerServiceAbilityReq(i)` model.
    pub req_spells: [u32; 3],
}

/// `TrainerSpellState` (vmangos `Player.h:119-122`) — the service `state` byte. `GREEN_DISABLED` (10)
/// is server-internal and is sent on the wire as `GREEN`, so only these three ever arrive.
pub mod trainer_spell_state {
    /// Learnable now.
    pub const GREEN: u8 = 0;
    /// Requirements unmet (level / skill / prerequisite).
    pub const RED: u8 = 1;
    /// Already known.
    pub const GRAY: u8 = 2;
}

/// The `errorCode` on `SMSG_TRAINER_BUY_FAILED` (vmangos `SharedDefines.h:1120-1122`,
/// `TrainerServiceType`-adjacent `TRAIN_FAIL_*`). A `u32` on the wire.
pub mod train_fail {
    /// Trainer service unavailable (not a trainer of yours, out of LOS, service not in the list).
    pub const UNAVAILABLE: u32 = 0;
    /// Not enough money.
    pub const NOT_ENOUGH_MONEY: u32 = 1;
    /// Not enough skill points.
    pub const NOT_ENOUGH_SKILL: u32 = 2;
}

/// Body of `CMSG_TRAINER_LIST` (vmangos `Npc.cpp`, `TrainerList::ReadFromWorldPacket`): one full
/// 8-byte trainer guid. Requests (and, after a purchase, re-requests to repaint green→gray) the
/// service list — the *refresh* verb, not the open verb: the window first opens off the gossip
/// trainer option's `SMSG_TRAINER_LIST` (decision 0237).
pub fn trainer_list(trainer_guid: u64) -> Vec<u8> {
    trainer_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_TRAINER_BUY_SPELL` (vmangos `Npc.cpp`, `TrainerBuySpell::ReadFromWorldPacket`):
/// `u64 trainerGuid, u32 spellId` (the [`TrainerSpell::spell`] of the chosen service). The server
/// answers `SMSG_TRAINER_BUY_SUCCEEDED` (+ the learned spell via `SMSG_LEARNED_SPELL`) or
/// `SMSG_TRAINER_BUY_FAILED`.
pub fn trainer_buy_spell(trainer_guid: u64, spell_id: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&trainer_guid.to_le_bytes());
    body.extend_from_slice(&spell_id.to_le_bytes());
    body
}

/// Read `SMSG_TRAINER_LIST` (vmangos `WorldSession::SendTrainerList`, `NPCHandler.cpp:141-241`):
/// `u64 trainerGuid, u32 trainerType, u32 count, count ×` [`TrainerSpell`] (38 bytes each)`, cstr
/// title`. `trainerType` is the window-framing kind (0 class · 1 mount · 2 tradeskill · 3 pet); the
/// title is the greeting line. Returns `(trainer, trainer_type, services, title)`.
pub(super) fn read_trainer_list(
    r: &mut &[u8],
) -> io::Result<(u64, u32, Vec<TrainerSpell>, String)> {
    let trainer = read_u64_le(r)?;
    let trainer_type = read_u32_le(r)?;
    let count = read_u32_le(r)?;
    let mut services = Vec::with_capacity(count as usize);
    for _ in 0..count {
        // Struct-literal fields evaluate top-to-bottom, so this reads in wire order.
        services.push(TrainerSpell {
            spell: read_u32_le(r)?,
            state: read_u8(r)?,
            cost: read_u32_le(r)?,
            can_learn_primary_prof: read_u32_le(r)? != 0,
            is_primary_prof_first_rank: read_u32_le(r)? != 0,
            req_level: read_u8(r)?,
            req_skill: read_u32_le(r)?,
            req_skill_value: read_u32_le(r)?,
            req_spells: [read_u32_le(r)?, read_u32_le(r)?, read_u32_le(r)?],
        });
    }
    let title = read_cstring(r)?;
    Ok((trainer, trainer_type, services, title))
}

/// Read `SMSG_TRAINER_BUY_SUCCEEDED` (vmangos `TrainerBuySucceeded::AppendBodyTo`): `u64
/// trainerGuid, u32 spellId`. Confirmation + sound only — the learned spell itself lands via
/// `SMSG_LEARNED_SPELL`, and the green→gray repaint needs a `CMSG_TRAINER_LIST` re-request.
pub(super) fn read_trainer_buy_succeeded(r: &mut &[u8]) -> io::Result<(u64, u32)> {
    Ok((read_u64_le(r)?, read_u32_le(r)?))
}

/// Read `SMSG_TRAINER_BUY_FAILED` (vmangos `TrainerBuyFailed::AppendBodyTo`): `u64 trainerGuid, u32
/// serviceId (the spell), u32 errorCode` (a [`train_fail`] code).
pub(super) fn read_trainer_buy_failed(r: &mut &[u8]) -> io::Result<(u64, u32, u32)> {
    Ok((read_u64_le(r)?, read_u32_le(r)?, read_u32_le(r)?))
}
