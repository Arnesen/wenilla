//! The GM trouble-ticket family — the five request/answer pairs behind the Help window
//! (opcodes 0x205-0x21B; decision 1673).
//!
//! | opcode | direction | body |
//! |---|---|---|
//! | `CMSG_GMTICKET_CREATE` 0x205 | out | `u8` category, `u32` map, `f32` x/y/z, cstring text, cstring reserved |
//! | `SMSG_GMTICKET_CREATE` 0x206 | in | `u32` response |
//! | `CMSG_GMTICKET_UPDATETEXT` 0x207 | out | `u8` category, cstring text |
//! | `SMSG_GMTICKET_UPDATETEXT` 0x208 | in | `u32` response |
//! | `CMSG_GMTICKET_GETTICKET` 0x211 | out | *empty* |
//! | `SMSG_GMTICKET_GETTICKET` 0x212 | in | `u32` status, then the ticket iff status == 6 |
//! | `CMSG_GMTICKET_DELETETICKET` 0x217 | out | *empty* |
//! | `SMSG_GMTICKET_DELETETICKET` 0x218 | in | `u32` response |
//! | `CMSG_GMTICKET_SYSTEMSTATUS` 0x21A | out | *empty* |
//! | `SMSG_GMTICKET_SYSTEMSTATUS` 0x21B | in | `u32` queue status |
//!
//! ## The two outbound bodies are byte-verified against the real client, not just the server
//!
//! Both are places where reading only the emulator would have got it wrong, so both were taken
//! from the 5875 binary's own senders:
//!
//! - **`CMSG_GMTICKET_CREATE`'s category is a `u8`, not a `u32`** (sender `0x5ef740`, reached from
//!   the Lua glue `NewGMTicket` `0x48c9d0`): `PutInt8` at `0x5ef851`, then `PutInt32(mapId)`, three
//!   `PutFloat`s, `PutCString(text)`, and a second cstring — the literal
//!   [`RESERVED_FOR_FUTURE_USE`] at VA `0x860708`. vmangos reads exactly that head
//!   (`Server/Packets/GmTicket.cpp:5-17`).
//! - **`CMSG_GMTICKET_UPDATETEXT` carries the category byte too** (sender `0x5efac0`: `PutInt8` at
//!   `0x5efb2d`, `PutCString` at `0x5efb3c`). This one is a real fork in the emulator world —
//!   vmangos reads `u8 type; cstring text` (`GmTicket.cpp:19-23`) and matches the client, while
//!   cmangos-classic reads a bare cstring (`GMTicketHandler.cpp:160-161`) and therefore swallows
//!   our category byte as the first character of the ticket. We are faithful to the client; a
//!   cmangos server would store a stray control byte at the head of the text. Recorded rather than
//!   worked around: the client is the authority, and bending the wire to one emulator's bug would
//!   break the other.
//!
//! **The `category == 2` chat-log tail is deliberately not sent.** The real client appends a
//! zlib-compressed transcript of the last (up to 60) chat lines to a Behavior/Harassment ticket
//! (`0x5ef936` onward). Neither emulator uses it: vmangos does not read it at all (and logs a
//! parse-length warning for the leftover bytes), cmangos reads two `u32`s under the wrong names
//! and discards the rest. Sending it would buy nothing and cost a log line per ticket, so
//! [`gm_ticket_create`] stops after the reserved string — see [`crate::messages::gm_ticket`]'s
//! test for the byte-exact shape we do send.
//!
//! ## The inbound ticket reads TEXT BEFORE CATEGORY
//!
//! [`GmTicket`]'s field order is the *wire* order, and it is not the order the Lua `UPDATE_TICKET`
//! event uses (`HelpFrameOpenTicket_OnEvent` reads arg1 = category, arg2 = description). vmangos
//! and cmangos agree byte-for-byte on the wire order — `cstring text` then `u8 category`
//! (vmangos `Server/Packets/GmTicket.cpp:66-79` filled at `GMTicketMgr.cpp:120-151`; cmangos
//! `GMTickets/GMTicketHandler.cpp:35-51`) — so the reorder happens inside the client's own handler
//! and belongs to the app's event feed, not here. Decoding it any other way would desynchronise
//! the whole struct after the first string.

use std::io;

use crate::wire::{read_cstring, read_f32_le, read_u32_le, read_u8};

/// The second cstring of `CMSG_GMTICKET_CREATE`, verbatim from the client's own constant at VA
/// `0x860708`. It is exactly what it says: a slot nothing has ever read. We send the real string
/// rather than an empty one because that is what the client puts on the wire, and a server that
/// ever grows a use for it should see what retail sent.
pub const RESERVED_FOR_FUTURE_USE: &str = "Reserved for future use";

/// `SMSG_GMTICKET_GETTICKET`'s status when a ticket follows (`GMTICKET_STATUS_HASTEXT`).
pub const GMTICKET_STATUS_HASTEXT: u32 = 0x06;

/// `SMSG_GMTICKET_GETTICKET`'s status when there is no ticket (`GMTICKET_STATUS_DEFAULT`) — a
/// 4-byte body and nothing after it. **This is the ordinary answer, not an error**: it is what
/// retail answered the client's own post-login ask with in the 1.12.1 sniff, and it is what the
/// Help window's "you have no open ticket" state is drawn from.
pub const GMTICKET_STATUS_DEFAULT: u32 = 0x0A;

/// The queue is accepting tickets (`GMTICKET_QUEUE_STATUS_ENABLED`).
pub const GMTICKET_QUEUE_ENABLED: i32 = 1;

/// The player's open ticket, as `SMSG_GMTICKET_GETTICKET` describes it.
///
/// The three `f32`s are **days**, not seconds, and two of them can legitimately be negative:
/// cmangos sends `-1.0` for `oldest_ticket_age`/`update_time` when it has never computed them
/// (`GMTicketMgr.h:318`, `GMTicketMgr.cpp:1340-1343`), and the shipped FrameXML tests exactly that
/// (`arg4 < 0 or arg5 < 0` → "Wait time currently unavailable"). So this is a plain `f32` with no
/// clamping: the negative *is* the signal.
#[derive(Debug, Clone, PartialEq)]
pub struct GmTicket {
    /// What the player typed. On vmangos a completed ticket has the GM's answer **appended into
    /// this same string** (`GMTicketMgr.cpp:124-136`) — there is no response channel in 1.12, so
    /// the reply arrives as part of the description.
    pub text: String,
    /// The `GMTicketCategory.dbc` id the ticket was filed under (1..10).
    pub category: u8,
    /// Days since the ticket was last modified — the FrameXML's `arg3`.
    pub ticket_age: f32,
    /// Days since the oldest open ticket on the realm was last modified; `0.0` when there is none,
    /// negative when unknown. The FrameXML subtracts `ticket_age` from this to estimate the wait.
    pub oldest_ticket_age: f32,
    /// How stale the oldest-ticket figure is, in days; negative or `> 0.042` (≈ 1 hour) means the
    /// data is no good and the window says so instead of guessing.
    pub update_time: f32,
    /// 0 = unassigned, 1 = assigned to a GM, 2 = in the escalation queue. vmangos clamps its
    /// internal 3 ("escalated and assigned") down to 2 before sending (`GMTicketMgr.cpp:147`), so
    /// nothing above 2 can arrive.
    pub assigned_to_gm: u8,
    /// 1 once a GM has actually opened the ticket, 0 before that.
    pub opened_by_gm: u8,
}

/// Body of `CMSG_GMTICKET_CREATE` — the Submit that files a new ticket.
///
/// `map`/`x`/`y`/`z` are where the player is standing when they file it; vmangos stores them so a
/// GM can `.ticket go` to the spot. The text is sent as typed: the client's own 1999-char cap
/// (`SStrCopy` bound `0x7d0` at `0x5ef7fd`) sits far above the Help window's `letters="500"`
/// EditBox, so nothing here can truncate a ticket the UI allowed.
pub fn gm_ticket_create(category: u8, map: u32, pos: [f32; 3], text: &str) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(1 + 4 + 12 + text.len() + 1 + RESERVED_FOR_FUTURE_USE.len() + 1);
    out.push(category);
    out.extend_from_slice(&map.to_le_bytes());
    for c in pos {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out.extend_from_slice(text.as_bytes());
    out.push(0);
    out.extend_from_slice(RESERVED_FOR_FUTURE_USE.as_bytes());
    out.push(0);
    out
}

/// Body of `CMSG_GMTICKET_UPDATETEXT` — the "Save Changes" that edits an open ticket.
///
/// Carries the category byte as well as the text (module doc: byte-verified at `0x5efb2d`), and
/// vmangos **stores** it: `HandleGMTicketUpdateTextOpcode` does `SetMessage(packet.ticketText)`
/// then `SetTicketType(TicketType(packet.type))` before saving (`GMTicketHandler.cpp:59-61`). So an
/// edit re-files the ticket under whatever category the byte carries, and getting this field wrong
/// silently moves a GM's ticket to a different queue rather than merely being ignored.
///
/// benilla's own Help window has no category picker (decision 1687), so it passes back the
/// category the server last reported for the ticket — precisely so an edit does not overwrite a
/// re-filing a GM made. That is the window's discipline, not the server's: the opcode itself will
/// happily move a ticket to whatever byte arrives.
pub fn gm_ticket_updatetext(category: u8, text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + text.len() + 1);
    out.push(category);
    out.extend_from_slice(text.as_bytes());
    out.push(0);
    out
}

/// Read the shared `u32` answer of `SMSG_GMTICKET_CREATE` / `UPDATETEXT` / `DELETETICKET`.
///
/// One reader for three opcodes because all three bodies are the same 4 bytes — the *meaning* of
/// the number is the opcode's, and that split lives in [`crate::events`]. The value set (vmangos
/// `GMTicketMgr.h:49-54`): 1 already-exists, 2 create-ok, 3 create-error, 4 update-ok,
/// 5 update-error, 9 deleted.
pub(super) fn read_gm_ticket_response(r: &mut &[u8]) -> io::Result<u32> {
    read_u32_le(r)
}

/// Read `SMSG_GMTICKETSYSTEMSTATUS`: 1 = the queue is taking tickets, 0 = it is not.
///
/// **Signed, and that is byte-verified rather than a preference.** The reference copies the field
/// off the wire verbatim (`0x418e95`, no extension), pushes it unmodified (`0x5e467b`) and hands it
/// to Lua through `0x704fa6 fild dword` — a signed load, with no `cmp`, `test`, clamp or mapping
/// anywhere in between (wow-re §5). `HelpFrame`'s `arg1 == -1` arm — "the queue is down, say so" —
/// is therefore reachable exactly when a server sends `0xFFFFFFFF`, and reading this unsigned would
/// turn that into 4294967295 and silently cost the dialog. vmangos only ever sends 0 or 1, so
/// nothing here would have caught the difference in a live run.
pub(super) fn read_gm_ticket_system_status(r: &mut &[u8]) -> io::Result<i32> {
    Ok(read_u32_le(r)? as i32)
}

/// Read `SMSG_GMTICKET_GETTICKET`: a status, and the ticket itself only when that status is
/// [`GMTICKET_STATUS_HASTEXT`].
///
/// Returns `Ok(None)` for every other status — which in practice is the 4-byte
/// [`GMTICKET_STATUS_DEFAULT`] body meaning "you have no open ticket". Deliberately **not** an
/// error: that answer is the common case, it is what the client's own post-login ask gets, and
/// treating it as a parse failure would put a warn line in the log on every login.
pub(super) fn read_gm_ticket(r: &mut &[u8]) -> io::Result<Option<GmTicket>> {
    if read_u32_le(r)? != GMTICKET_STATUS_HASTEXT {
        return Ok(None);
    }
    Ok(Some(GmTicket {
        text: read_cstring(r)?,
        category: read_u8(r)?,
        ticket_age: read_f32_le(r)?,
        oldest_ticket_age: read_f32_le(r)?,
        update_time: read_f32_le(r)?,
        assigned_to_gm: read_u8(r)?,
        opened_by_gm: read_u8(r)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The create body, byte for byte — the client's own field order and widths (`0x5ef740`), with
    /// the category as ONE byte. Getting this wrong by widening the category to `u32` would shift
    /// the map id and the whole position by three bytes and file every ticket at a garbage spot.
    #[test]
    fn the_create_body_is_a_category_byte_then_map_position_text_and_the_reserved_string() {
        let body = gm_ticket_create(4, 1, [-8949.95, -132.493, 83.5312], "My sword vanished.");
        let mut want = vec![0x04];
        want.extend_from_slice(&1u32.to_le_bytes());
        want.extend_from_slice(&(-8949.95f32).to_le_bytes());
        want.extend_from_slice(&(-132.493f32).to_le_bytes());
        want.extend_from_slice(&(83.5312f32).to_le_bytes());
        want.extend_from_slice(b"My sword vanished.\0");
        want.extend_from_slice(b"Reserved for future use\0");
        assert_eq!(body, want);
        // The tail the client sends for a Behavior/Harassment ticket is deliberately absent: the
        // body ends at the reserved string for every category, including 2.
        assert_eq!(
            gm_ticket_create(2, 0, [0.0; 3], "x").len(),
            1 + 4 + 12 + 2 + 24,
            "no chat-log tail, even for category 2"
        );
    }

    /// The updatetext body leads with the category byte — the divergence the module doc records.
    /// A bare cstring here is what cmangos expects and what the client never sends.
    #[test]
    fn the_updatetext_body_leads_with_the_category_byte() {
        assert_eq!(
            gm_ticket_updatetext(7, "Still stuck."),
            b"\x07Still stuck.\0"
        );
    }

    /// The ticket round-trips in WIRE order — text first, category second. Pinning it here is the
    /// point: the Lua event's order is the reverse, and a "fix" that swapped these two to match
    /// the event would desynchronise every field after the string.
    #[test]
    fn a_held_ticket_decodes_in_wire_order_text_before_category() {
        let mut bytes = GMTICKET_STATUS_HASTEXT.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"Stuck in a rock.\0");
        bytes.push(1);
        bytes.extend_from_slice(&0.25f32.to_le_bytes());
        bytes.extend_from_slice(&2.5f32.to_le_bytes());
        bytes.extend_from_slice(&0.01f32.to_le_bytes());
        bytes.push(2);
        bytes.push(1);

        assert_eq!(
            read_gm_ticket(&mut &bytes[..]).unwrap(),
            Some(GmTicket {
                text: "Stuck in a rock.".to_string(),
                category: 1,
                ticket_age: 0.25,
                oldest_ticket_age: 2.5,
                update_time: 0.01,
                assigned_to_gm: 2,
                opened_by_gm: 1,
            })
        );
    }

    /// The queue status is read SIGNED: `0xFFFFFFFF` must reach Lua as `-1`, which is the value the
    /// shipped window branches on to raise "GM Help Tickets are currently unavailable." Read it
    /// unsigned and that arm can never fire.
    #[test]
    fn the_queue_status_is_signed_so_all_ones_reads_as_minus_one() {
        assert_eq!(
            read_gm_ticket_system_status(&mut &0xFFFF_FFFFu32.to_le_bytes()[..]).unwrap(),
            -1
        );
        assert_eq!(
            read_gm_ticket_system_status(&mut &1u32.to_le_bytes()[..]).unwrap(),
            GMTICKET_QUEUE_ENABLED
        );
    }

    /// "You have no ticket" is a 4-byte body and an `Ok(None)`, never an error — it is the answer
    /// retail gave the client's own post-login ask, so an error here would warn on every login.
    #[test]
    fn the_no_ticket_answer_is_four_bytes_and_is_not_an_error() {
        let bytes = GMTICKET_STATUS_DEFAULT.to_le_bytes();
        assert_eq!(read_gm_ticket(&mut &bytes[..]).unwrap(), None);
        // Any other status is likewise "nothing to show" rather than a parse failure.
        let bytes = 0u32.to_le_bytes();
        assert_eq!(read_gm_ticket(&mut &bytes[..]).unwrap(), None);
    }
}
