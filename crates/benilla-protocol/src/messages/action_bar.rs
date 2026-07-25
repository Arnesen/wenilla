//! Action-bar messages — the 120-slot bar's packing, the login snapshot, and the one-slot write.
//! Split out of `messages/spells.rs` (decision 0640); mirrored by `world::writer::action_bar`.
//!
//! The bar is **client-authoritative** (decisions 0216 §7 / 0218 §4): the server stores 120 packed
//! `u32`s and hands them back at login, and the client sends one `CMSG_SET_ACTION_BUTTON` per local
//! slot mutation. There is no server-side edit in normal play, so `SMSG_ACTION_BUTTONS` is a
//! login-only packet in practice — which is why the whole family is three items.

use std::io::{self};

use crate::wire::read_u32_le;

/// Action-button kind byte (bits 24–31 of the packed slot word — vmangos `Player.h`
/// `ActionButtonType`): a spell id, a macro id, or an item id in the low 24 bits.
pub const ACTION_KIND_SPELL: u8 = 0x00;
pub const ACTION_KIND_MACRO: u8 = 0x40;
pub const ACTION_KIND_ITEM: u8 = 0x80;

/// One *occupied* action-bar slot from `SMSG_ACTION_BUTTONS`. The wire is 120 packed `u32`s
/// (`MAX_ACTION_BUTTONS`, vmangos `MasterPlayer::SendInitialActionButtons`) — `action` in bits
/// 0–23, `kind` in bits 24–31 (`ACTION_BUTTON_ACTION/TYPE`, `Player.h`); a zero word is an empty
/// slot and is not surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionButton {
    /// The bar slot index (0..119). Slots 0–11 are the main bar's buttons 1–12.
    pub slot: u8,
    /// The spell/macro/item id (bits 0–23).
    pub action: u32,
    /// The kind byte (bits 24–31): [`ACTION_KIND_SPELL`]/[`ACTION_KIND_MACRO`]/[`ACTION_KIND_ITEM`]
    /// (0x01 "click?" exists in the enum, carried raw if it ever appears).
    pub kind: u8,
}

/// Read `SMSG_ACTION_BUTTONS`: packed `u32` per slot to end-of-body (the server sends exactly 120;
/// reading to the boundary keeps us robust to a different count). Zero words (empty slots) are
/// dropped; occupied slots surface as [`ActionButton`]s.
pub(super) fn read_action_buttons(r: &mut &[u8]) -> io::Result<Vec<ActionButton>> {
    let mut buttons = Vec::new();
    let mut slot: u32 = 0;
    while !r.is_empty() {
        let packed = read_u32_le(r)?;
        if packed != 0 {
            buttons.push(ActionButton {
                slot: slot.min(u8::MAX as u32) as u8,
                action: packed & 0x00FF_FFFF,
                kind: (packed >> 24) as u8,
            });
        }
        slot += 1;
    }
    Ok(buttons)
}

/// Body of `CMSG_SET_ACTION_BUTTON` (VERIFIED vmangos `WorldPackets::Misc::SetActionButton::
/// ReadFromWorldPacket`, `Server/Packets/Misc.cpp:87-90`; opcode 296 `Opcodes_1_12_1.h:299`):
/// `button u8` + `packetData u32` (`action | kind<<24`, [`ActionButton`]'s own packing) — 5
/// bytes. `packed == 0` clears the slot (`HandleSetActionButtonOpcode`'s `!packet.packetData`
/// branch calls `removeActionButton`, never sent back over the wire — decision 0216 §7/0218 §4:
/// the client sends ONE of these per local slot mutation, a drag-swap is two sends, never atomic).
pub fn set_action_button(button: u8, packed: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(5);
    body.push(button);
    body.extend_from_slice(&packed.to_le_bytes());
    body
}
