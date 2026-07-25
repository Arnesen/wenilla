//! The two **client-volunteered pose** bodies — sheath state and stand state. Split out of
//! `messages/spells.rs` (`set_sheathed`) and `messages/client.rs` (`stand_state_change`) by
//! decision 0640, which is also what mirrors `world::writer::pose`.
//!
//! They were apart for no reason anyone recorded, and they are plainly one thing: the client
//! decides, the server stores whatever we send with no validation of its own, and the echo into our
//! `UNIT_FIELD_BYTES_2` / `UNIT_FIELD_BYTES_1` is what every observer's body reads (decisions
//! 0080 / 0080c). The server has no independent way to know a weapon is drawn or that we sat down —
//! so the only consequence of getting one wrong is that other players see the wrong body.
//!
//! `CMSG_MOUNTSPECIAL_ANIM`, the third member of that family, has an empty body and so needs no
//! builder here (see `world::writer::pose`).

/// Body of `CMSG_SETSHEATHED` (vmangos `SetSheathed::ReadFromWorldPacket`: `recv_data >> sheathed`):
/// one `u32` sheath state (0 unarmed/stowed, 1 melee drawn, 2 ranged drawn). Purely
/// client-volunteered — `HandleSetSheathedOpcode` (`CombatHandler.cpp:80-87`) just stores whatever
/// we send via `Unit::SetSheath`, which lands in our own `UNIT_FIELD_BYTES_2` and relays to nearby
/// observers on the next values update; the server has no independent way to know a weapon is drawn.
pub fn set_sheathed(state: u32) -> Vec<u8> {
    state.to_le_bytes().to_vec()
}

/// Body of `CMSG_STANDSTATECHANGE` (vmangos `StandStateChange::ReadFromWorldPacket`:
/// `recv_data >> animState`): one `u32` stand state. The server accepts only
/// {0 STAND, 1 SIT, 3 SLEEP, 8 KNEEL} (`HandleStandStateChangeOpcode`,
/// `MiscHandler.cpp:437`) and applies it via `Unit::SetStandState` → `UNIT_FIELD_BYTES_1`
/// byte 0, which relays to every observer — the same client-volunteers → server-echoes →
/// fields-drive-everyone pattern as sheath (decision 0080c).
pub fn stand_state_change(state: u32) -> Vec<u8> {
    state.to_le_bytes().to_vec()
}
