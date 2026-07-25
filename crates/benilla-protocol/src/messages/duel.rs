//! The duel family — challenge, countdown, bounds, completion, and the winner broadcast
//! (opcodes 359-365 + 695; decision 0633).
//!
//! Duels are the one family where **both ends are pinned at the bytes**. The server side is
//! vmangos `Server/Packets/Duel.{h,cpp}` (every `ReadFromWorldPacket` / `AppendBodyTo`),
//! `Handlers/DuelHandler.cpp`, `Objects/Player.cpp` (`DuelComplete`, `CheckDuelDistance`,
//! `UpdateDuelFlag`, `SendDuelCountdown`) and `Spells/SpellEffects.cpp` (`EffectDuel`). The
//! client side is WoW.exe's own duel TU — `E:\build\buildWoW\WoW\Source\Ui\DuelInfo.cpp`
//! (`__FILE__` @`0x84a020`), handlers registered at `0x4d4710`:
//!
//! | opcode | handler | what it reads |
//! |---|---|---|
//! | `SMSG_DUEL_REQUESTED` 0x167 | `0x4d49d0` | two full guids: arbiter, then challenger |
//! | `SMSG_DUEL_OUTOFBOUNDS` 0x168 | `0x4d4aa0` | nothing |
//! | `SMSG_DUEL_INBOUNDS` 0x169 | `0x4d4ac0` | nothing |
//! | `SMSG_DUEL_COMPLETE` 0x16a | `0x4d4b20` | one `u8` |
//! | `SMSG_DUEL_WINNER` 0x16b | `0x4d4ba0` | `u8` + two cstrings |
//! | `SMSG_DUEL_COUNTDOWN` 0x2b7 | `0x4d4ae0` | one `u32`, **milliseconds** |
//!
//! and the two sends, `AcceptDuel` `0x4d4830` / `CancelDuel` `0x4d48b0`, which each build the
//! opcode and push the **stored arbiter guid** (`[0xb73240]`, saved by the request handler) — the
//! body vmangos ignores, so only the binary could settle it.

use std::io::{self, Read};

use crate::wire::{read_cstring, read_u32_le, read_u64_le, read_u8};

/// `SMSG_DUEL_REQUESTED` — a duel challenge, sent to **both** parties (VERIFIED vmangos
/// `SpellEffects.cpp` `EffectDuel`: `data << pGameObj->GetObjectGuid(); data <<
/// caster->GetObjectGuid();` then `SendPacket` to caster *and* target).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuelRequested {
    /// The **duel-flag GameObject** the effect spawned midway between the two players (`Duel
    /// Flag`, entry 21680, type 16 `DUEL_ARBITER`). It is both the out-of-bounds anchor and the
    /// identity of the duel: it lands in each duellist's `PLAYER_DUEL_ARBITER` descriptor field,
    /// and it is the guid the client echoes back on accept/cancel.
    pub arbiter: u64,
    /// Who cast the challenge. The client compares this against its own guid: equal means *we*
    /// challenged (show "You have requested a duel." and auto-accept), otherwise we are the one
    /// being challenged (`0x4d49d0`).
    pub challenger: u64,
}

/// Read `SMSG_DUEL_REQUESTED`: two full 8-byte guids, arbiter first.
pub fn read_duel_requested(r: &mut impl Read) -> io::Result<DuelRequested> {
    Ok(DuelRequested {
        arbiter: read_u64_le(r)?,
        challenger: read_u64_le(r)?,
    })
}

/// Read `SMSG_DUEL_COMPLETE`'s single `u8` and return it as vmangos's `started` flag (VERIFIED
/// `Player::DuelComplete`: `packet->started = (type != DUEL_INTERRUPTED)`).
///
/// `false` — the duel ended **before it began** (declined, cancelled during the countdown, one
/// side logged out or was interrupted). That is exactly the case the client answers with the
/// `ERR_DUEL_CANCELLED` line (`0x4d4b20`: `test al,al; jne` past `DisplayError(0x136)`).
pub fn read_duel_complete(r: &mut impl Read) -> io::Result<bool> {
    Ok(read_u8(r)? != 0)
}

/// `SMSG_DUEL_WINNER` — the outcome line, broadcast to everyone around the loser (VERIFIED
/// `Player::DuelComplete`'s `SendObjectMessageToSet`, so bystanders read it too).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuelWinner {
    /// `false` = knocked out (`DUEL_WINNER_KNOCKOUT`, "%1$s has defeated %2$s in a duel"),
    /// `true` = the loser fled or forfeited (`DUEL_WINNER_RETREAT`, "%2$s has fled from %1$s in
    /// a duel"). vmangos writes `fled = (type != DUEL_WON)`; the client picks the template on
    /// `test al,al` at `0x4d4bd9`.
    pub fled: bool,
    /// Winner's name (`%1$s`). The client reads it into a 0x30-byte buffer.
    pub winner: String,
    /// Loser's name (`%2$s`), same buffer size.
    pub loser: String,
}

/// Read `SMSG_DUEL_WINNER`: `u8` flag, then winner and loser cstrings in that order.
pub fn read_duel_winner(r: &mut impl Read) -> io::Result<DuelWinner> {
    Ok(DuelWinner {
        fled: read_u8(r)? != 0,
        winner: read_cstring(r)?,
        loser: read_cstring(r)?,
    })
}

/// Read `SMSG_DUEL_COUNTDOWN` and return **seconds**.
///
/// The wire carries milliseconds (vmangos `HandleDuelAcceptedOpcode` sends a literal `3000`), and
/// the client divides by 1000 before using it as a tick count — `0x4d4aef: mov eax,0x10624dd3;
/// mul [countdown]; shr edx,0x6` is the compiler's reciprocal for `/1000`. The truncation is the
/// client's, so it is ours: a hypothetical 3500 ms counts down from 3, not 4.
pub fn read_duel_countdown(r: &mut impl Read) -> io::Result<u32> {
    Ok(read_u32_le(r)? / 1000)
}

/// Body of `CMSG_DUEL_ACCEPTED` — the duel-arbiter guid (VERIFIED WoW.exe `AcceptDuel 0x4d4830`:
/// builds opcode `0x16c`, then `PutInt64([0xb73240])`, the guid the request handler stored).
///
/// vmangos reads the guid into `DuelAccepted::playerGuid` and never looks at it
/// (`HandleDuelAcceptedOpcode` keys off `m_duel` alone) — the binary is the only authority for
/// what actually goes on the wire, which is why we read it there rather than guessing zero.
pub fn duel_accepted(arbiter: u64) -> Vec<u8> {
    arbiter.to_le_bytes().to_vec()
}

/// Body of `CMSG_DUEL_CANCELLED` — the same arbiter guid (VERIFIED WoW.exe `CancelDuel 0x4d48b0`,
/// identical to [`duel_accepted`] but for opcode `0x16d`).
///
/// One opcode covers three player intents, disambiguated server-side by duel state
/// (`HandleDuelCancelledOpcode`): declining the popup, cancelling during the countdown
/// (`DUEL_INTERRUPTED`), and `/forfeit` **after** the duel has started — which makes the sender
/// beg (spell 7267) and hands the win to the opponent.
pub fn duel_cancelled(arbiter: u64) -> Vec<u8> {
    arbiter.to_le_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SMSG_DUEL_REQUESTED`: arbiter then challenger, both full 8-byte little-endian guids.
    #[test]
    fn duel_requested_is_arbiter_then_challenger() {
        let body = [
            0xEF, 0xBE, 0xAD, 0xDE, 0x00, 0x00, 0x00, 0xF1, // arbiter
            0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // challenger
        ];
        assert_eq!(
            read_duel_requested(&mut &body[..]).unwrap(),
            DuelRequested {
                arbiter: 0xF100_0000_DEAD_BEEF,
                challenger: 7,
            }
        );
    }

    /// `SMSG_DUEL_COMPLETE`'s `started` byte: 0 = interrupted before the duel began.
    #[test]
    fn duel_complete_started_byte() {
        assert!(!read_duel_complete(&mut &[0u8][..]).unwrap());
        assert!(read_duel_complete(&mut &[1u8][..]).unwrap());
    }

    /// `SMSG_DUEL_WINNER`: flag, winner cstring, loser cstring.
    #[test]
    fn duel_winner_flag_then_two_names() {
        let mut body = vec![1u8];
        body.extend_from_slice(b"Onerogue\0");
        body.extend_from_slice(b"Twomage\0");
        assert_eq!(
            read_duel_winner(&mut &body[..]).unwrap(),
            DuelWinner {
                fled: true,
                winner: "Onerogue".into(),
                loser: "Twomage".into(),
            }
        );
    }

    /// `SMSG_DUEL_COUNTDOWN` is milliseconds on the wire; the client's `/1000` truncates.
    #[test]
    fn duel_countdown_is_milliseconds_truncated_to_seconds() {
        assert_eq!(
            read_duel_countdown(&mut &3000u32.to_le_bytes()[..]).unwrap(),
            3
        );
        assert_eq!(
            read_duel_countdown(&mut &3999u32.to_le_bytes()[..]).unwrap(),
            3
        );
        assert_eq!(
            read_duel_countdown(&mut &0u32.to_le_bytes()[..]).unwrap(),
            0
        );
    }

    /// Both client sends are the bare arbiter guid — byte-for-byte what `0x4d4830`/`0x4d48b0`
    /// push.
    #[test]
    fn accept_and_cancel_carry_the_arbiter_guid() {
        let guid = 0x0000_00F1_DEAD_BEEFu64;
        assert_eq!(duel_accepted(guid), guid.to_le_bytes());
        assert_eq!(duel_cancelled(guid), guid.to_le_bytes());
    }
}
