//! Skill-line messages — the skills pane's one send verb, `CMSG_UNLEARN_SKILL` (0x202, vmangos
//! `Opcodes.cpp`'s `DEFINE_HANDLER` → `HandleUnlearnSkillOpcode`). Body from vmangos
//! `Server/Packets/Skill.h` (`UnlearnSkill { uint32 skillId }`); semantics from
//! `Handlers/SkillHandler.cpp`: the server gates on the requested line's `SkillRaceClassInfo.flags
//! & SKILL_FLAG_UNLEARNABLE (0x20)` — a request without it is DROPPED and anticheat-flagged, so
//! the client must only ever offer the unlearn where that bit is set (the skills pane's
//! `abandonable` predicate, `benilla-formats::skill_lines`) — and answers `SetSkill(id, 0, 0)`:
//! the removal comes back as a `PLAYER_SKILL_INFO` field update, never an ack packet.

/// Body of `CMSG_UNLEARN_SKILL`: one `u32` skill line id.
pub fn unlearn_skill(skill_id: u32) -> Vec<u8> {
    skill_id.to_le_bytes().to_vec()
}
