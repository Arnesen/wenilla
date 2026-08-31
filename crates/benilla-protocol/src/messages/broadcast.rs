//! The four **world broadcasts** — the packets the server sends to everybody, or to everybody in a
//! zone, rather than to one player about their own doing.
//!
//! They have no shared wire family (two carry a zone id, one a DBC index, one nothing at all), and
//! they are grouped here because they share a *destination*: all four end at a chat line or a UI
//! toast the player did not ask for. The client's own handlers are three adjacent functions plus
//! one arm of a shared dispatcher, and two of them are the same function twice over.
//!
//! | opcode | handler | what it becomes |
//! |---|---|---|
//! | `SMSG_ZONE_UNDER_ATTACK` `0x254` | `0x49dcc0` | `CHAT_MSG_CHANNEL` on the defense channels |
//! | `SMSG_DEFENSE_MESSAGE` `0x33B` | `0x49de30` | `CHAT_MSG_CHANNEL` on the defense channels |
//! | `SMSG_SERVER_MESSAGE` `0x291` | `0x49df80` | `CHAT_MSG_SYSTEM` |
//! | `SMSG_CHAT_RESTRICTED` `0x2FD` | `0x5e38c0` arm `0x5e4a09` | `DisplayError(451)` → `CHAT_MSG_SYSTEM` |
//!
//! **The two defense broadcasts do not go where a reimplementation guesses.** Neither is a system
//! line: the client walks its own **joined-channel list** (`[0xb4fe04]`, count `[0xb4fe00]`, stride
//! `0xa0` — the record wow-re's `chat-msg-event-args.md` §7 owns) and delivers the text on every
//! joined channel whose `ChatChannels.dbc` row carries the `DEFENSE` flag `0x10000`, as chat type
//! `0xE` = `CHAT_MSG_CHANNEL` with **no sender** and language `0`. A `ZONE_DEP` (`0x2`) channel —
//! LocalDefense — additionally requires the broadcast's zone to be the player's current one; a
//! GLOBAL one — WorldDefense — always passes. So a player who has joined neither channel sees
//! nothing at all, which is the faithful answer and not a dropped packet.
//!
//! The zone test is on the **parent** zone, not the packet's area: both handlers resolve the id
//! through `AreaTable.dbc` and replace it with the row's parent (`[rec+0x8]`) when that is nonzero.
//! `SMSG_ZONE_UNDER_ATTACK`'s *text* still names the packet's own area — the name is read off the
//! row before the remap — so "Sentinel Hill is under attack!" is matched against Westfall.
//!
//! The wire shapes below are VERIFIED against vmangos source with a file:line cite per field, and
//! confirmed against the client's own reads in the handlers above. The goldens
//! (`tests/broadcast.rs`) are built the way the server builds each body.

use std::io;

use crate::wire::{read_cstring, read_u32_le};

/// Read `SMSG_SERVER_MESSAGE` (VERIFIED vmangos `WorldPackets::Misc::ServerMessage::AppendBodyTo`,
/// `Server/Packets/Misc.cpp:341-345`): `u32 messageType` + one cstring.
///
/// `messageType` is a **`ServerMessages.dbc` row id**, not a free enum — the client indexes the
/// store at `[0xc0d990]` by it directly (`0x49dfbb`) and takes the row's localized text as a
/// **format string**, which the packet's own text fills. vmangos's producers are the five rows the
/// shipped table has (`World.h:62`, whose comment says "ServerMessages.dbc"):
///
/// | type | `ServerMessages.dbc` text (enUS) | vmangos sends |
/// |---|---|---|
/// | 1 | `[SERVER] Shutdown in %s` | `secsToTimeString(remaining)` |
/// | 2 | `[SERVER] Restart in %s` | `secsToTimeString(remaining)` |
/// | 3 | `%s` | the operator's own text (`SERVER_MSG_CUSTOM`) |
/// | 4 | `[SERVER] Shutdown cancelled` | `""` |
/// | 5 | `[SERVER] Restart cancelled` | `""` |
///
/// The text is carried whole rather than being resolved here: the DBC lives on the *client* side
/// of this crate's boundary (`benilla_formats::ServerMessagesCatalog`), and the row-missing
/// fallback the client itself uses — `"[%d]: %s"` at `0x844864` — needs the raw type to render.
pub(super) fn read_server_message(r: &mut &[u8]) -> io::Result<(u32, String)> {
    let message_type = read_u32_le(r)?;
    let text = read_cstring(r)?;
    Ok((message_type, text))
}

/// Read `SMSG_ZONE_UNDER_ATTACK` (VERIFIED vmangos
/// `WorldPackets::Misc::ZoneUnderAttack::AppendBodyTo`, `Server/Packets/Misc.cpp:451-454`): one
/// `u32`, the `AreaTable.dbc` id of the area under attack.
///
/// vmangos sends it from `Creature::SendZoneUnderAttackMessage`
/// (`src/game/Objects/Creature.cpp:2889`), whose sole caller is the creature-death path
/// (`Objects/Unit.cpp:1200`) when the victim `IsGuard()` or carries
/// `CREATURE_STATIC_FLAG_PVP_ENABLING` and the killer was a player. It goes **map-wide to the
/// opposing team**, throttled to one per area per 10 s — and the function's own comment reads
/// *"Send a message to LocalDefense channel for players opposition team in the zone"*, which is the
/// server independently naming the destination the client's handler picks.
pub(super) fn read_zone_under_attack(r: &mut &[u8]) -> io::Result<u32> {
    read_u32_le(r)
}

/// Read `SMSG_DEFENSE_MESSAGE` (VERIFIED vmangos `Map::SendDefenseMessage`,
/// `src/game/Maps/Map.cpp:1868-1884`): `u32 zoneId`, `u32 length`, then `length` bytes of text.
///
/// **Built raw, not through a packet class** — the whole body is three `ByteBuffer <<` writes
/// inside a `#if SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_11_2`, so this packet exists for 1.12 and
/// no earlier build. The length is written as `strlen(text) + 1` and the `char const*` write
/// appends the NUL, so it counts the terminator.
///
/// **The length is load-bearing here, unlike [`super::area_trigger::read_area_trigger_message`]'s.**
/// The client does not read a C string at all: `0x49de5c` calls the `CDataStore` **borrow-and-skip**
/// `0x419ac0`, which copies nothing, hands back a live pointer into the receive buffer and advances
/// the cursor by exactly `length` — and which **zeroes its out-parameter before any bounds check**,
/// so a `length` that overruns the packet reaches the chat composer as a NULL text and
/// `0x49a870`'s NULL guard at `0x49a889` drops the line entirely (wow-re
/// `system/net/scratch/cdatastore-get-primitives.md` §5, `system/ui/scratch/world-broadcast-opcodes.md`
/// §3 — a §5 cross-check that corrected two committed notes calling `0x419ac0` a copy).
///
/// So an overrunning length is a **refusal**, not a hint to ignore: it errors here, and the session
/// drops the packet, which is the same nothing-displayed the reference produces. Within the bounds
/// the text is the borrowed window up to its first NUL — the reference reads past `length` when
/// there is no NUL in it (an unmitigated OOB read, wow-re's words), which is the one place a
/// faithful reimplementation stops short on purpose.
///
/// The Eastern Plaguelands tower captures are its only vmangos sender.
pub(super) fn read_defense_message(r: &mut &[u8]) -> io::Result<(u32, String)> {
    let zone_id = read_u32_le(r)?;
    let length = read_u32_le(r)? as usize;
    if length > r.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "SMSG_DEFENSE_MESSAGE: length {length} overruns the {} body bytes left",
                r.len()
            ),
        ));
    }
    let (window, rest) = r.split_at(length);
    *r = rest;
    let text = window.split(|b| *b == 0).next().unwrap_or(window);
    Ok((zone_id, String::from_utf8_lossy(text).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_message_is_a_type_and_a_string() {
        let mut body = 2u32.to_le_bytes().to_vec();
        body.extend_from_slice(b"15 Minutes\0");
        assert_eq!(
            read_server_message(&mut &body[..]).unwrap(),
            (2, "15 Minutes".to_string())
        );
    }

    /// The cancel messages carry an EMPTY text, and the row's own string is the whole line — so an
    /// empty string here is a value, never a parse failure.
    #[test]
    fn a_cancel_message_carries_an_empty_text() {
        let mut body = 4u32.to_le_bytes().to_vec();
        body.push(0);
        assert_eq!(
            read_server_message(&mut &body[..]).unwrap(),
            (4, String::new())
        );
    }

    #[test]
    fn zone_under_attack_is_the_bare_area_id() {
        let body = 40u32.to_le_bytes();
        assert_eq!(read_zone_under_attack(&mut &body[..]).unwrap(), 40);
    }

    /// The length prefix is skipped and the text read whole — built the way `Map.cpp` builds it,
    /// terminator counted.
    #[test]
    fn a_defense_message_skips_its_length_and_reads_the_text() {
        let text = b"The Eastern Plaguelands tower has been taken!";
        let mut body = 139u32.to_le_bytes().to_vec();
        body.extend_from_slice(&(text.len() as u32 + 1).to_le_bytes());
        body.extend_from_slice(text);
        body.push(0);
        let (zone, out) = read_defense_message(&mut &body[..]).unwrap();
        assert_eq!(zone, 139);
        assert_eq!(out, String::from_utf8_lossy(text));
    }

    /// A truncated body is an error, not a silently empty line — and it is the *declared length*
    /// that decides, because that is what the reference bounds-checks. An overrun leaves its
    /// out-pointer NULL and the chat composer drops the line; erroring here reproduces the same
    /// nothing-displayed, with a log line instead of silence.
    #[test]
    fn a_length_that_overruns_the_body_is_refused() {
        let mut body = 139u32.to_le_bytes().to_vec();
        body.extend_from_slice(&8u32.to_le_bytes());
        assert!(read_defense_message(&mut &body[..]).is_err());

        // Even with bytes present: 8 declared, 4 supplied.
        let mut body = 139u32.to_le_bytes().to_vec();
        body.extend_from_slice(&8u32.to_le_bytes());
        body.extend_from_slice(b"abc\0");
        assert!(read_defense_message(&mut &body[..]).is_err());
    }

    /// A window with no NUL in it is the whole window — the reference would read on past `length`
    /// (an OOB read it mitigates nowhere); stopping at the bound is the deliberate divergence.
    #[test]
    fn a_window_without_a_terminator_is_taken_whole() {
        let mut body = 139u32.to_le_bytes().to_vec();
        body.extend_from_slice(&3u32.to_le_bytes());
        body.extend_from_slice(b"abcTRAILING");
        assert_eq!(
            read_defense_message(&mut &body[..]).unwrap(),
            (139, "abc".to_string())
        );
    }
}
