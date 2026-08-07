//! The **page-text** query pair (decision 1105) — the ask-once cache behind every readable *book*.
//!
//! Distinct from the mail letter's `CMSG_ITEM_TEXT_QUERY` ([`super::mail`]), which fetches one
//! whole player-written body by an *instance* text id. A `PageText.wdb` id names a single **page**,
//! and pages **chain**: each answer carries the next page's id (`0` = last). Two things read from
//! this cache, and both reach it the same way — the client opens its reader on an object guid and
//! asks the object for its page id:
//!
//! - a readable **item template** (`ItemPrototype::PageText`) — a book in your bags;
//! - a **`GAMEOBJECT_TYPE_TEXT`** world object (its template `data[0]`) — a book, plaque or sign
//!   lying in the world.
//!
//! The client's request carries the asking object's guid after the page id (VERIFIED: the cache's
//! request writer `0x564730` appends the 8-byte guid at `0x56485d` under the per-cache "requests
//! carry a guid" flag `[cache+0x38]` — the very address vmangos cites in
//! `QueryPageText::ReadFromWorldPacket` when it reads the tail as optional). vmangos resolves the
//! page from the id alone and skips the guid.

use std::io;

use crate::wire::{read_cstring, read_u32_le};

/// Body of `CMSG_PAGE_TEXT_QUERY` (opcode `0x005A`/90 — VERIFIED vmangos
/// `Server/Protocol/Opcodes_1_12_1.h`): `u32 pageId` then the asking object's `u64 guid` (VERIFIED
/// `QueryPageText::ReadFromWorldPacket`, which reads the guid only `if (rpos() < size())` and
/// discards it; the real client writes it — see the module doc).
pub fn page_text_query(page_id: u32, guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&page_id.to_le_bytes());
    body.extend_from_slice(&guid.to_le_bytes());
    body
}

/// Read `SMSG_PAGE_TEXT_QUERY_RESPONSE` (VERIFIED vmangos `Server/Packets/Query.cpp`,
/// `PageTextQueryResponse::AppendBodyTo`): `u32 pageId, cstr text, u32 nextPageId`.
///
/// vmangos answers **one query with the whole chain** — `HandlePageTextQueryOpcode` loops on
/// `next_page`, sending one of these per page — so asking for page 1 lands every page of the book
/// unsolicited. A page the server doesn't know answers `"Item page missing."` with `nextPageId = 0`,
/// which terminates the chain rather than erroring.
pub(super) fn read_page_text_query_response(r: &mut &[u8]) -> io::Result<(u32, String, u32)> {
    Ok((read_u32_le(r)?, read_cstring(r)?, read_u32_le(r)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{opcode, parse_server, ServerPacket};

    #[test]
    fn query_body_is_page_id_then_guid() {
        assert_eq!(
            page_text_query(333, 0xF110_0000_0000_0042),
            [
                0x4D, 0x01, 0x00, 0x00, // pageId 333
                0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0xF1, // guid
            ]
        );
    }

    #[test]
    fn response_reads_the_chain_link() {
        let mut body = 333u32.to_le_bytes().to_vec();
        body.extend_from_slice(b"Page one.\0");
        body.extend_from_slice(&334u32.to_le_bytes());
        match parse_server(opcode::SMSG_PAGE_TEXT_QUERY_RESPONSE, &body).unwrap() {
            ServerPacket::PageTextQueryResponse {
                page_id,
                text,
                next_page_id,
            } => {
                assert_eq!(page_id, 333);
                assert_eq!(text, "Page one.");
                assert_eq!(next_page_id, 334);
            }
            other => panic!("expected PageTextQueryResponse, got {}", other.name()),
        }
    }

    #[test]
    fn last_page_terminates_the_chain() {
        let mut body = 9u32.to_le_bytes().to_vec();
        body.extend_from_slice(b"The end.\0");
        body.extend_from_slice(&0u32.to_le_bytes());
        match parse_server(opcode::SMSG_PAGE_TEXT_QUERY_RESPONSE, &body).unwrap() {
            ServerPacket::PageTextQueryResponse { next_page_id, .. } => {
                assert_eq!(next_page_id, 0)
            }
            other => panic!("expected PageTextQueryResponse, got {}", other.name()),
        }
    }
}
