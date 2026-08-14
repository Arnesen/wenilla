//! The reputation pane's wire: the pane's three client verbs, byte-exact against the vmangos
//! reader side, and the one server push (`SMSG_SET_FACTION_VISIBLE`) that carries no standing.
//!
//! The standing-bearing pair (`SMSG_INITIALIZE_FACTIONS` / `SMSG_SET_FACTION_STANDING`) already
//! has its goldens in `tests/simple_packets.rs`, where it landed with the unit-reaction decode.

mod common;

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages::{self, opcode};
use benilla_protocol::ServerPacket;
use common::hx;

/// `CMSG_SET_FACTION_ATWAR` (293): `u32 repListId`, `u8 flag` — the pane's crossed-swords box.
///
/// Byte-exact against vmangos's reader (`WorldPackets::Misc::SetFactionAtWar::ReadFromWorldPacket`,
/// `Server/Packets/Misc.cpp`, over the `uint32 repListId` / `uint8 flag` fields in `Misc.h`), which
/// is the side that has to parse what we send.
#[test]
fn set_faction_at_war_body_is_slot_then_flag() {
    assert_eq!(opcode::CMSG_SET_FACTION_ATWAR, 293);
    // Slot 21 (Booty Bay is reputationIndex 1; 21 is Darnassus's), war ON.
    assert_eq!(messages::set_faction_at_war(21, true), hx("1500000001"));
    // …and OFF. The flag is a whole byte, not a bit — `0`, never a cleared bit in a mask.
    assert_eq!(messages::set_faction_at_war(21, false), hx("1500000000"));
    // Slot 0 is a REAL faction (the Bloodsail Buccaneers hold reputationIndex 0), so a zero slot
    // is an ordinary request here — the "nothing" sentinel problem is the watched verb's alone.
    assert_eq!(messages::set_faction_at_war(0, true), hx("0000000001"));
}

/// `CMSG_SET_FACTION_INACTIVE` (791): `u32 repListId`, `u8 inactive` — the "move to inactive" box.
///
/// Byte-exact against `WorldPackets::Misc::SetFactionInactive::ReadFromWorldPacket`.
#[test]
fn set_faction_inactive_body_is_slot_then_flag() {
    assert_eq!(opcode::CMSG_SET_FACTION_INACTIVE, 791);
    assert_eq!(messages::set_faction_inactive(54, true), hx("3600000001"));
    assert_eq!(messages::set_faction_inactive(54, false), hx("3600000000"));
}

/// `CMSG_SET_WATCHED_FACTION` (792): one **signed** `i32` slot, `-1` for "watch nothing".
///
/// The signedness is the whole point and is why this has its own assert: vmangos writes the value
/// straight into `PLAYER_FIELD_WATCHED_FACTION_INDEX` with `SetInt32Value`
/// (`HandleSetWatchedFactionOpcode`), and slot `0` is a real faction — so a `0` here would watch the
/// Bloodsail Buccaneers rather than clear the bar. FrameXML's `SetWatchedFactionIndex(0)` means "no
/// display row"; translating that to [`messages::WATCHED_FACTION_NONE`] is the binding's job, above
/// this layer, and this test pins the two values apart so the translation cannot be skipped silently.
#[test]
fn set_watched_faction_body_is_a_signed_slot_and_none_is_minus_one() {
    assert_eq!(opcode::CMSG_SET_WATCHED_FACTION, 792);
    assert_eq!(messages::WATCHED_FACTION_NONE, -1);
    assert_eq!(messages::set_watched_faction(11), hx("0b000000"));
    assert_eq!(
        messages::set_watched_faction(messages::WATCHED_FACTION_NONE),
        hx("ffffffff")
    );
    // The trap, asserted: watching slot 0 and watching nothing are different bytes.
    assert_ne!(
        messages::set_watched_faction(0),
        messages::set_watched_faction(messages::WATCHED_FACTION_NONE)
    );
}

/// `SMSG_SET_FACTION_VISIBLE` (291): one `u32` reputation-list slot, no standing
/// (vmangos `ReputationMgr::SendVisible` / `SetFactionVisible::AppendBodyTo`).
///
/// The server pushes it the first time the player meets a faction. A client that drops it keeps a
/// correct standing for a row the pane will not list — which is exactly the failure this parse arm
/// exists to prevent, so the decode leg is asserted too.
#[test]
fn set_faction_visible_parses_and_decodes() {
    assert_eq!(opcode::SMSG_SET_FACTION_VISIBLE, 291);
    let p = messages::parse_server(opcode::SMSG_SET_FACTION_VISIBLE, &hx("0d000000")).unwrap();
    assert!(matches!(p, ServerPacket::SetFactionVisible { list_id: 13 }));
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::ReputationVisible { list_id: 13 }]
    ));
}
