//! The three **client-volunteered** pose `WorldWriter` sends — sheath, stand state, and the
//! mounted flourish. Bodies in [`crate::messages`]'s `set_sheathed`/`stand_state_change` builders
//! (the flourish is bodyless). Split out of `writer/mod.rs` (decision 0636).
//!
//! What makes these one family, and unlike every other send here: **the client decides, the server
//! only relays.** Nothing validates them and nothing answers them — the sheath byte and the stand
//! byte land in our own `UNIT_FIELD_BYTES_2`/`BYTES_1` and every observer reads the pose from
//! there (decisions 0080/0080c), and the flourish is a pure broadcast the sender plays locally at
//! send time. So the *only* consequence of getting one wrong is that other players see the wrong
//! body.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Tell the server our sheath state (`CMSG_SETSHEATHED`): `state` 0 unarmed/stowed, 1 melee
    /// drawn, 2 ranged drawn. Purely client-volunteered (body in [`messages::set_sheathed`]) — it
    /// becomes our own `UNIT_FIELD_BYTES_2` sheath byte, which other clients read. Sent by the
    /// manual toggle, the attack-start auto-draw, and every reconcile force (byte-verified: the
    /// setter `0x611cf0` sends it whenever `bFireEvent` — wow-re `sheath-policy.md`,
    /// decision 0080).
    pub fn set_sheathed(&mut self, state: u32) -> Result<()> {
        self.send(opcode::CMSG_SETSHEATHED, &messages::set_sheathed(state))
    }

    /// Tell the server our stand state (`CMSG_STANDSTATECHANGE`): 0 stand · 1 sit · 3 sleep ·
    /// 8 kneel (the only values vmangos accepts). Client-volunteered like sheath — the echo into
    /// `UNIT_FIELD_BYTES_1` byte 0 drives every observer's sit/stand pose (decision 0080c).
    pub fn stand_state_change(&mut self, state: u32) -> Result<()> {
        self.send(
            opcode::CMSG_STANDSTATECHANGE,
            &messages::stand_state_change(state),
        )
    }

    /// The mounted space-bar flourish (`CMSG_MOUNTSPECIAL_ANIM`, EMPTY body — VERIFIED vmangos
    /// `HandleMountSpecialAnimOpcode`). The sender plays its own MountSpecial(94) locally at
    /// send time and self-suppresses the broadcast echo (decision 0441 P2 — whether the echo
    /// arrives is a server-config detail).
    pub fn mount_special(&mut self) -> Result<()> {
        self.send(opcode::CMSG_MOUNTSPECIAL_ANIM, &[])
    }
}
