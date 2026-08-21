//! Action-bar messages — the 120-slot bar's packing, the login snapshot, and the one-slot write.
//! Split out of `messages/spells.rs` (decision 0640); mirrored by `world::writer::action_bar`.
//!
//! The bar is **client-authoritative** (decisions 0216 §7 / 0218 §4): the server stores 120 packed
//! `u32`s and hands them back at login, and the client sends one `CMSG_SET_ACTION_BUTTON` per local
//! slot mutation. There is no server-side edit in normal play, so `SMSG_ACTION_BUTTONS` is a
//! login-only packet in practice — which is why the whole family is three items.
//!
//! The **visibility** of the four extra bars is the opposite arrangement and shares this file only
//! because it shares the subject: it is one server-owned byte the client can post but never write
//! locally. See [`set_actionbar_toggles`].

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

/// Body of `CMSG_SET_ACTIONBAR_TOGGLES` (opcode [`super::opcode::CMSG_SET_ACTIONBAR_TOGGLES`]):
/// **one `u8`**, and nothing else — VERIFIED at the bytes, wow-re
/// `system/ui/scratch/action-bar-toggles.md` §3. `SetActionBarToggles 0x4e76e0` builds a stack
/// `CDataStore`, appends the opcode with `PutUInt32 0x418190` and the byte with `PutUInt8
/// 0x418070`, and sends; those two appends are the only ones between construction and send, and
/// `NetClient::Send 0x5379b3` computes the payload length as `size − read` = **5** — the
/// independent confirmation that 4 + 1 is the whole frame.
///
/// **The byte is server-owned; this is a post, not a write.** No instruction in the real client
/// stores to `PLAYER_FIELD_BYTES` byte 2 (§4.1: displacement `0x102a` occurs exactly once
/// image-wide and it is the *read* inside `GetActionBarToggles`) — the cell only ever moves through
/// the generic `SMSG_UPDATE_OBJECT` value-apply. So the sender's own field copy still holds the old
/// value until the server echoes, and nothing is notified when it does (§4.2: all 49 field-change
/// registrations at `0x468070` were enumerated and none sits at an offset ≥ `0x1000`). Read it back
/// with [`super::ObjectFields::player_action_bar_toggles`].
///
/// **Only the low nibble is reachable.** The real binding accumulates from zero and ORs at most
/// bits 0..3 (§2), so a value it produces is always `0x00..=0x0f` and a `Set` *destroys* whatever
/// the server held in the high nibble. This encoder does not mask: what the caller packed is what
/// goes on the wire, and the four-bit law belongs where the packing happens (the Lua binding).
///
/// The bit→bar meaning is **not** the binary's — it is bar-agnostic (§7), and naming the bars here
/// would put a FrameXML convention in the protocol layer.
pub fn set_actionbar_toggles(toggles: u8) -> Vec<u8> {
    vec![toggles]
}
