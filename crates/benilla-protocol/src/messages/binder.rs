//! The innkeeper bind family — the confirm question, its answer, and the "you are bound" echo
//! (opcodes 747, 437, 344; decision 1331).
//!
//! Three packets and one law worth stating plainly, because getting it wrong is silent: **selecting
//! the innkeeper's gossip line does not bind anything.** vmangos's `GOSSIP_OPTION_INNKEEPER` arm
//! (`Player.cpp:12341`) closes the gossip menu and calls `Player::SetBindPoint`, which sends
//! `SMSG_BINDER_CONFIRM` and stops. The bind happens only when the client answers
//! `CMSG_BINDER_ACTIVATE` (`HandleBinderActivateOpcode`, `NPCHandler.cpp:479`), at which point the
//! innkeeper casts spell 3286 "Bind" on the player and `SPELL_EFFECT_BIND`
//! (`SpellEffects.cpp:5837`) writes the homebind and sends `SMSG_BINDPOINTUPDATE` +
//! `SMSG_PLAYERBOUND`. A client that shows the gossip line and never sends the answer therefore
//! looks exactly like a server that ignored the click — which is how B249 read.
//!
//! | opcode | direction | body |
//! |---|---|---|
//! | `SMSG_BINDER_CONFIRM` 0x2eb | in | `u64` binder guid |
//! | `CMSG_BINDER_ACTIVATE` 0x1b5 | out | `u64` binder guid |
//! | `SMSG_PLAYERBOUND` 0x158 | in | `u64` binder guid + `u32` area id |
//!
//! The server bodies are VERIFIED against vmangos `Server/Packets/Npc.{h,cpp}` and
//! `Server/Packets/Misc.{h,cpp}` (`BinderConfirm`, `BinderActivate`, `PlayerBound`). What the
//! *client* puts in `CMSG_BINDER_ACTIVATE` is the guid it stored from the confirm: vmangos reads
//! that guid and uses it (`GetNPCIfCanInteractWith(packet.npcGuid, UNIT_NPC_FLAG_INNKEEPER)`), so
//! unlike the duel echo this one is load-bearing — a zero here is an ignored bind.

use std::io;

use crate::wire::{read_u32_le, read_u64_le};

/// `SMSG_PLAYERBOUND` — the server's echo once the bind has actually taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerBound {
    /// The NPC that bound us (vmangos sends the *caster* of spell 3286, i.e. the innkeeper).
    pub binder: u64,
    /// The `AreaTable` id the player is now bound in — the same id `SMSG_BINDPOINTUPDATE` carries
    /// in the packet beside it, so the two can never disagree.
    pub area: u32,
}

/// Body of `CMSG_BINDER_ACTIVATE`: the binder's full 8-byte guid — the one the client stored from
/// [`read_binder_confirm`]. **Not decorative:** `HandleBinderActivateOpcode` resolves this guid to
/// a live `UNIT_NPC_FLAG_INNKEEPER` creature in interact range and drops the request if it can't,
/// so an empty or stale guid binds nothing and reports nothing.
pub fn binder_activate(binder_guid: u64) -> Vec<u8> {
    binder_guid.to_le_bytes().to_vec()
}

/// Read `SMSG_BINDER_CONFIRM` (vmangos `Npc.cpp`, `buffer << binderGuid`): one full 8-byte guid.
///
/// This is the *question*, and it arrives with `SMSG_GOSSIP_COMPLETE` right before it — the gossip
/// window is already closing when the dialog goes up, which is why the confirm is not part of the
/// gossip session's state.
pub(super) fn read_binder_confirm(r: &mut &[u8]) -> io::Result<u64> {
    read_u64_le(r)
}

/// Read `SMSG_PLAYERBOUND` (vmangos `Misc.cpp`, `buffer << binderGuid << areaId`).
pub(super) fn read_player_bound(r: &mut &[u8]) -> io::Result<PlayerBound> {
    Ok(PlayerBound {
        binder: read_u64_le(r)?,
        area: read_u32_le(r)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The activate body is the bare guid, little-endian — the shape vmangos's `BinderActivate`
    /// reads back.
    #[test]
    fn the_activate_body_is_one_little_endian_guid() {
        assert_eq!(
            binder_activate(0x0000_0000_0000_2a01),
            vec![0x01, 0x2a, 0, 0, 0, 0, 0, 0]
        );
    }

    /// Confirm in, bound out: both read the guid first, and `SMSG_PLAYERBOUND` carries the area
    /// id after it.
    #[test]
    fn the_two_inbound_bodies_round_trip() {
        let guid: u64 = 0xF130_0000_0001_2345;
        let mut bytes = guid.to_le_bytes().to_vec();
        assert_eq!(read_binder_confirm(&mut &bytes[..]).unwrap(), guid);

        bytes.extend_from_slice(&141u32.to_le_bytes());
        assert_eq!(
            read_player_bound(&mut &bytes[..]).unwrap(),
            PlayerBound {
                binder: guid,
                area: 141
            }
        );
    }
}
