//! The guild tabard designer's wire (decision 1977; wow-re `system/ui/scratch/tabard-designer.md`
//! §4, VERIFIED at the bytes): two `MSG_*` opcodes used in both directions, and the battlemaster
//! greeting the same TU ships beside them.
//!
//! - `MSG_TABARDVENDOR_ACTIVATE 0x1F2` — out: `u64 npcGuid` (the NPC-click ladder's
//!   TABARDDESIGNER arm, `0x5e00e0`); in: `u64 vendorGuid`, the only thing that opens the frame
//!   (`0x5e70c0` → `0x4f5840`).
//! - `MSG_SAVE_GUILD_EMBLEM 0x1F1` — out: `u64 vendorGuid` then the five `u32` design values in
//!   the order *emblemStyle, emblemColor, borderStyle, borderColor, backgroundColor* (`0x5e03f0`);
//!   in: one `u32` result (`0x5e70f0`), indexed into a six-row message table.
//! - `CMSG_BATTLEMASTER_HELLO 0x2D7` — out: `u64 npcGuid`, no gate (`0x5e01a0`).

use std::io::{self, Read};

use crate::wire::{read_u32_le, read_u64_le};

/// Body of `MSG_SAVE_GUILD_EMBLEM` outbound: the vendor guid raw little-endian (not packed), then
/// the five design values as `u32`s in the designer's slot order.
pub fn save_guild_emblem(vendor: u64, design: [u32; 5]) -> Vec<u8> {
    let mut body = vendor.to_le_bytes().to_vec();
    for v in design {
        body.extend_from_slice(&v.to_le_bytes());
    }
    body
}

/// Body of `MSG_TABARDVENDOR_ACTIVATE` outbound: the NPC guid and nothing else.
pub fn tabard_vendor_activate(npc: u64) -> Vec<u8> {
    npc.to_le_bytes().to_vec()
}

/// Body of `CMSG_BATTLEMASTER_HELLO`: the NPC guid and nothing else.
pub fn battlemaster_hello(npc: u64) -> Vec<u8> {
    npc.to_le_bytes().to_vec()
}

/// `MSG_SAVE_GUILD_EMBLEM` inbound: one `u32` result.
pub(super) fn read_save_guild_emblem_result(r: &mut impl Read) -> io::Result<u32> {
    read_u32_le(r)
}

/// `MSG_TABARDVENDOR_ACTIVATE` inbound: the vendor guid.
pub(super) fn read_tabard_vendor_activate(r: &mut impl Read) -> io::Result<u64> {
    read_u64_le(r)
}

/// The reply handler's result table (`0x85fe88`, six rows — the consumer's own `0 ≤ result < 6`
/// bound): the message-catalog key each result shows, `None` for row 5, the `0x1d1` sentinel that
/// shows nothing. A result past the table is ignored outright.
pub const GUILD_EMBLEM_RESULT_MESSAGES: [Option<&str>; 6] = [
    Some("ERR_GUILDEMBLEM_SUCCESS"),
    Some("ERR_GUILDEMBLEM_INVALID_TABARD_COLORS"),
    Some("ERR_GUILDEMBLEM_NOGUILD"),
    Some("ERR_GUILDEMBLEM_NOTGUILDMASTER"),
    Some("ERR_GUILDEMBLEM_NOTENOUGHMONEY"),
    None,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_save_body_is_the_guid_then_the_five_values_in_slot_order() {
        let body = save_guild_emblem(0x0102_0304_0506_0708, [170, 1, 2, 3, 50]);
        assert_eq!(body.len(), 28);
        assert_eq!(&body[..8], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(&body[8..12], &170u32.to_le_bytes());
        assert_eq!(&body[24..28], &50u32.to_le_bytes());
        assert_eq!(tabard_vendor_activate(7), 7u64.to_le_bytes());
        assert_eq!(battlemaster_hello(9), 9u64.to_le_bytes());
    }

    #[test]
    fn the_replies_read_their_one_field() {
        assert_eq!(
            read_save_guild_emblem_result(&mut &3u32.to_le_bytes()[..]).unwrap(),
            3
        );
        assert_eq!(
            read_tabard_vendor_activate(&mut &0xF130_0000_0000_0042u64.to_le_bytes()[..]).unwrap(),
            0xF130_0000_0000_0042
        );
        assert_eq!(GUILD_EMBLEM_RESULT_MESSAGES[5], None);
    }
}
