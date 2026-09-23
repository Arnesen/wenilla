//! The two **client-volunteered pose** bodies — sheath state and stand state. Split out of
//! `messages/spells.rs` (`set_sheathed`) and `messages/client.rs` (`stand_state_change`) by
//! decision 0640, which is also what mirrors `world::writer::pose`.
//!
//! They were apart for no reason anyone recorded, and they are plainly one thing: the client
//! decides, the server stores whatever we send with no validation of its own, and the echo into our
//! `UNIT_FIELD_BYTES_2` / `UNIT_FIELD_BYTES_1` is what every observer's body reads (decisions
//! 0080 / 0080c). The server has no independent way to know a weapon is drawn or that we sat down —
//! so the only consequence of getting one wrong is that other players see the wrong body.
//!
//! `CMSG_MOUNTSPECIAL_ANIM`, the third member of that family, has an empty body and so needs no
//! builder here (see `world::writer::pose`).
//!
//! The stand state also has an **inbound** twin, [`read_stand_state_update`] (decision 2339): the
//! server's own `SMSG_STANDSTATE_UPDATE`, one byte, which the reference applies to the local player
//! through the same setter the volunteered change goes through.

use std::io;

use crate::wire::read_u8;

/// Body of `CMSG_SETSHEATHED` (vmangos `SetSheathed::ReadFromWorldPacket`: `recv_data >> sheathed`):
/// one `u32` sheath state (0 unarmed/stowed, 1 melee drawn, 2 ranged drawn). Purely
/// client-volunteered — `HandleSetSheathedOpcode` (`CombatHandler.cpp:80-87`) just stores whatever
/// we send via `Unit::SetSheath`, which lands in our own `UNIT_FIELD_BYTES_2` and relays to nearby
/// observers on the next values update; the server has no independent way to know a weapon is drawn.
pub fn set_sheathed(state: u32) -> Vec<u8> {
    state.to_le_bytes().to_vec()
}

/// Body of `CMSG_STANDSTATECHANGE` (vmangos `StandStateChange::ReadFromWorldPacket`:
/// `recv_data >> animState`): one `u32` stand state. The server accepts only
/// {0 STAND, 1 SIT, 3 SLEEP, 8 KNEEL} (`HandleStandStateChangeOpcode`,
/// `MiscHandler.cpp:437`) and applies it via `Unit::SetStandState` → `UNIT_FIELD_BYTES_1`
/// byte 0, which relays to every observer — the same client-volunteers → server-echoes →
/// fields-drive-everyone pattern as sheath (decision 0080c).
pub fn stand_state_change(state: u32) -> Vec<u8> {
    state.to_le_bytes().to_vec()
}

/// Read `SMSG_STANDSTATE_UPDATE` → the stand state. VERIFIED vmangos `Unit::SetStandState`
/// (`Objects/Unit.cpp:9541`) → `WorldPackets::Misc::StandStateUpdate::AppendBodyTo`, one `u8`
/// (`UnitStandStateType`: 0 STAND · 1 SIT · 3 SLEEP · 8 KNEEL …). No guid: the reference's
/// `0x603e50` applies it to the local player whatever unit the server meant (2339).
pub(super) fn read_stand_state_update(r: &mut &[u8]) -> io::Result<u8> {
    read_u8(r)
}

#[cfg(test)]
mod inbound_tests {
    use crate::messages::{opcode, parse_server, ServerPacket};

    #[test]
    fn stand_state_update_decodes() {
        // One byte (VERIFIED vmangos `Misc::StandStateUpdate::AppendBodyTo`); a drink's sit is 1.
        match parse_server(opcode::SMSG_STANDSTATE_UPDATE, &[1]).unwrap() {
            ServerPacket::StandStateUpdate { state } => assert_eq!(state, 1),
            other => panic!("expected StandStateUpdate, got {}", other.name()),
        }
        assert!(
            parse_server(opcode::SMSG_STANDSTATE_UPDATE, &[]).is_err(),
            "a short body is an error"
        );
    }
}
