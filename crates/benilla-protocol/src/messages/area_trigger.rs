//! The area-trigger pair: the client's "I walked into trigger N" report, and the server's refusal
//! text that sometimes comes back.
//!
//! An `AreaTrigger.dbc` volume means nothing to the client — it owns the geometry and nothing else.
//! Walking into one sends [`area_trigger`] and the **server** decides what happens: a teleport
//! (every instance entrance, the Darnassus/Rut'theran portals), a quest's explore objective, the
//! inn's rested state, a battleground's entrance list. A teleport comes back as the ordinary
//! `SMSG_TRANSFER_PENDING` + `SMSG_NEW_WORLD` pair (decision 0455) or a same-map
//! `MSG_MOVE_TELEPORT_ACK`; a refusal comes back as [`read_area_trigger_message`]'s text.

use std::io;

use crate::wire::{read_cstring, read_u32_le};

/// Body of `CMSG_AREATRIGGER` (opcode `0xB4`/180 — VERIFIED vmangos `Opcodes_1_12_1.h:183`, and the
/// reference builds exactly this: `0x5e2110` writes `0xb4` then the record's first field, its id).
/// One `u32`: the `AreaTrigger.dbc` id.
///
/// The server re-checks the claim before acting — `HandleAreaTriggerOpcode`
/// (`Handlers/MiscHandler.cpp:622`) drops it unless the id is a real DBC row and the player is
/// inside the volume with 5 yd of slop — so a stale or invented id costs nothing but is never
/// obeyed. It is also dropped outright while taxi-flying.
pub fn area_trigger(trigger_id: u32) -> Vec<u8> {
    trigger_id.to_le_bytes().to_vec()
}

/// Read `SMSG_AREA_TRIGGER_MESSAGE` (VERIFIED vmangos `WorldSession::SendAreaTriggerMessage`,
/// `Server/WorldSession.cpp:882-898`: `u32 length` then the cstring): server-composed text
/// explaining why a trigger did *not* fire — "You must be at least level 58 to enter…", "You
/// cannot enter %s while in ghost form.", a battleground's level/faction refusal.
///
/// The length prefix counts the cstring's bytes **including** its terminator; it is redundant with
/// the string itself, and is read and discarded (the reference does the same — its `0x2b8` arm
/// reads a u32, then a string, then displays the string).
pub(super) fn read_area_trigger_message(r: &mut &[u8]) -> io::Result<String> {
    let _length = read_u32_le(r)?;
    read_cstring(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_is_the_bare_id() {
        assert_eq!(area_trigger(542), 542u32.to_le_bytes().to_vec());
    }

    /// The length prefix is skipped and the text read whole — built the way the server builds it.
    #[test]
    fn message_reads_past_its_length_prefix() {
        let text = "You must be at least level 58 to enter.";
        let mut body = Vec::new();
        body.extend_from_slice(&(text.len() as u32 + 1).to_le_bytes());
        body.extend_from_slice(text.as_bytes());
        body.push(0);
        let mut r = body.as_slice();
        assert_eq!(read_area_trigger_message(&mut r).unwrap(), text);
        assert!(r.is_empty(), "the whole body is consumed");
    }
}
