//! The tutorial system's wire (decision 1976; wow-re `system/ui/scratch/tutorial-flags.md` §7):
//! one inbound bank of bits and the three sends the client's tutorial code makes.
//!
//! The client keeps two bit banks filled from the same `SMSG_TUTORIAL_FLAGS` — the fire-once bank
//! its C++ trigger reads and sets, and the acknowledged bank Lua's `FlagTutorial` and six C++
//! auto-flag sites write, which is the only path that sends. The handler takes **whatever bytes
//! are left in the packet** and sizes both banks as `bytes × 8` bits; vmangos sends 32 bytes.

use std::io::{self, Read};

/// `SMSG_TUTORIAL_FLAGS` (VERIFIED at the bytes, handler `0x4b5700`): the raw bank, every
/// remaining byte of the packet — bit `id` is `bytes[id >> 3] & (1 << (id & 7))`, the same
/// `word = id >> 5, bit = 1 << (id & 31)` addressing on little-endian dwords.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TutorialFlags {
    pub bytes: Vec<u8>,
}

/// Parse it: the rest of the body, whatever its length.
pub(super) fn read_tutorial_flags(r: &mut impl Read) -> io::Result<TutorialFlags> {
    let mut bytes = Vec::new();
    r.read_to_end(&mut bytes)?;
    Ok(TutorialFlags { bytes })
}

/// Body of `CMSG_TUTORIAL_FLAG` (VERIFIED, `0x4b54c0`): one `u32`, the **0-based** id.
pub fn tutorial_flag(id: u32) -> Vec<u8> {
    id.to_le_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bank_is_every_remaining_byte_and_the_flag_is_one_dword() {
        let body = [0x01u8, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF];
        let f = read_tutorial_flags(&mut body.as_slice()).unwrap();
        assert_eq!(f.bytes, body.to_vec());
        let f = read_tutorial_flags(&mut [].as_slice()).unwrap();
        assert!(f.bytes.is_empty(), "an empty body is an empty bank");
        assert_eq!(tutorial_flag(41), vec![41, 0, 0, 0]);
    }
}
