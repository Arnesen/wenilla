//! The two honor-system wire messages (decision 1512): the inspect-honor request/reply pair and
//! the honor-gain credit. Every shape below is VERIFIED against vmangos source (file:line cites
//! per item); the goldens are hand-built against the server's own `AppendBodyTo` order.
//!
//! The *durable* honor state is not here — it rides the player descriptor, eleven PRIVATE fields
//! plus the PUBLIC current-rank byte ([`crate::messages::ObjectFields::player_pvp_rank`] and
//! friends). These two packets cover exactly what the descriptor cannot: **somebody else's**
//! honor stats (which never stream to us), and the *moment* a kill pays out.
//!
//! Not modelled here (known siblings, VERIFIED present in vmangos `Opcodes_1_12_1.h`): the
//! battleground family — `MSG_PVP_LOG_DATA` (736), `CMSG`/`SMSG_BATTLEFIELD_LIST` (572/573) and
//! `…_STATUS` (723/724) — which 0208/0631 defer wholesale, and `SMSG_ZONE_UNDER_ATTACK` (596), a
//! world broadcast rather than an honor message.

use std::io;

use crate::wire::{read_i32_le, read_u16_le, read_u32_le, read_u64_le, read_u8};

/// Another player's honor stats — the `MSG_INSPECT_HONOR_STATS` **reply** (opcode `0x2D6`/726,
/// VERIFIED vmangos `Opcodes_1_12_1.h`; already named in `opcode_names`). A 50-byte body, all
/// little-endian with no padding, in the exact order
/// `WorldPackets::Misc::InspectHonorStatsResponse::AppendBodyTo` writes it
/// (`Server/Packets/Misc.cpp:301-325`). Every `#if SUPPORTED_CLIENT_BUILD >= CLIENT_BUILD_1_6_1`
/// block in that function is live for build 5875, so all three conditional members are present.
///
/// This exists because the honor descriptor fields are PRIVATE: nothing about a *foreign* player's
/// kills, honor or progress bar is on the wire until we ask. The one exception is their current
/// rank, which streams publicly in `PLAYER_BYTES_3` and so is **not** in this packet —
/// [`crate::messages::ObjectFields::player_pvp_rank`] answers it off the target's own descriptor.
///
/// The server builds every value straight out of the target's descriptor
/// (`Handlers/MiscHandler.cpp:962-1010`), so these fields are the inspected player's own
/// `PLAYER_FIELD_*` honor block, re-serialized — with two shape differences worth knowing:
/// `sessionKills` arrives as the raw packed dword rather than a split pair, and the three weekly
/// HK counts arrive as bare `u16`s with a separate (dead) dishonorable slot beside each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InspectHonorStats {
    /// Whose stats these are — echoed from our request (`MiscHandler.cpp:977`). The reply carries
    /// no error shape at all: the server simply does not answer when the target is missing, out of
    /// `INSPECT_DISTANCE` (10 yd), or a valid attack target, so a silent non-reply is the refusal.
    pub player_guid: u64,
    /// The target's **highest lifetime** honor rank (`GetHighestRank().rank`,
    /// `MiscHandler.cpp:985`) — the *internal* rank 0..18, the same domain as
    /// [`crate::messages::ObjectFields::player_honor_rank`], NOT the visual number the UI draws.
    /// Their *current* rank is the PUBLIC descriptor byte, not this.
    pub highest_rank: u8,
    /// Today's kills as the raw packed `PLAYER_FIELD_SESSION_KILLS` dword — low `u16` honorable,
    /// high `u16` dishonorable (`MiscHandler.cpp:988` reads the whole field with
    /// `GetUInt32Value`). Split with [`InspectHonorStats::session_kills`].
    pub session_kills: u32,
    /// Yesterday's honorable kills (`GetUInt16Value(PLAYER_FIELD_YESTERDAY_KILLS, 0)`).
    pub yesterday_hk: u16,
    /// Written `0` by vmangos (`MiscHandler.cpp:994`, comment *"Unknown (deprecated, yesterday
    /// dishonourable?)"*). Decoded and carried rather than skipped: whether the real client reads
    /// these three slots at all is a question currently out with an RE orchestrator in
    /// `wow-5875-re`, and a field we decode and ignore is honest where a silently-skipped one
    /// would be a landmine if the verdict comes back "it reads them".
    pub unknown_old1: u16,
    /// Last week's honorable kills.
    pub last_week_hk: u16,
    /// Written `0` by vmangos — see [`InspectHonorStats::unknown_old1`].
    pub unknown_old2: u16,
    /// This week's honorable kills (a 1.6.0 addition; live for 5875).
    pub this_week_hk: u16,
    /// Written `0` by vmangos — see [`InspectHonorStats::unknown_old1`].
    pub unknown_old3: u16,
    /// Lifetime honorable kills (`PLAYER_FIELD_LIFETIME_HONORBALE_KILLS`, vmangos's spelling).
    pub lifetime_hk: u32,
    /// Lifetime dishonorable kills.
    pub lifetime_dhk: u32,
    /// Yesterday's honor points (`PLAYER_FIELD_YESTERDAY_CONTRIBUTION`).
    pub yesterday_honor: u32,
    /// Last week's honor points.
    pub last_week_honor: u32,
    /// This week's honor points (a 1.6.0 addition; live for 5875).
    pub this_week_honor: u32,
    /// Last week's **standing** — the numeric ladder position, not a rank
    /// (`PLAYER_FIELD_LAST_WEEK_RANK`; `0` = unranked that week).
    pub last_week_rank: u32,
    /// The rank progress bar, `0..255` **within the target's current rank** (a 1.6.0 addition;
    /// live for 5875). See [`crate::messages::ObjectFields::player_honor_rank_bar`] for the
    /// server's computation and the negative-rank wrap.
    pub rank_bar: u8,
}

impl InspectHonorStats {
    /// [`Self::session_kills`] split into `(honorable, dishonorable)` — the `TWO_SHORT` packing
    /// the descriptor field uses (low half, then high half). The wire carries the dword whole
    /// here, so unlike the descriptor accessor this split is ours to do.
    pub fn session_kills(&self) -> (u16, u16) {
        (self.session_kills as u16, (self.session_kills >> 16) as u16)
    }
}

/// Read the `MSG_INSPECT_HONOR_STATS` **reply** (see [`InspectHonorStats`]): 50 bytes in
/// `AppendBodyTo` order. A short body errors (`UnexpectedEof`) rather than yielding a
/// half-populated struct.
///
/// The opcode is an **`MSG_`**: the same number 0x2D6 carries our request *and* the server's
/// reply, and the two bodies are different shapes (8 bytes vs 50). Nothing has to disambiguate
/// them, because direction does it: [`super::parse_server`] is fed only by the world *reader*, so
/// every 0x2D6 body it ever sees is a reply. Our own request never passes through here — it is
/// built by [`inspect_honor_stats`] and handed straight to the writer.
pub(super) fn read_inspect_honor_stats(r: &mut &[u8]) -> io::Result<InspectHonorStats> {
    Ok(InspectHonorStats {
        player_guid: read_u64_le(r)?,
        highest_rank: read_u8(r)?,
        session_kills: read_u32_le(r)?,
        yesterday_hk: read_u16_le(r)?,
        unknown_old1: read_u16_le(r)?,
        last_week_hk: read_u16_le(r)?,
        unknown_old2: read_u16_le(r)?,
        this_week_hk: read_u16_le(r)?,
        unknown_old3: read_u16_le(r)?,
        lifetime_hk: read_u32_le(r)?,
        lifetime_dhk: read_u32_le(r)?,
        yesterday_honor: read_u32_le(r)?,
        last_week_honor: read_u32_le(r)?,
        this_week_honor: read_u32_le(r)?,
        last_week_rank: read_u32_le(r)?,
        rank_bar: read_u8(r)?,
    })
}

/// Body of the `MSG_INSPECT_HONOR_STATS` **request**: the inspected player's full 8-byte GUID and
/// nothing else (VERIFIED vmangos `WorldPackets::Misc::InspectHonorStats::ReadFromWorldPacket`,
/// `Server/Packets/Misc.cpp:93-96` — `recv_data >> guid`, a raw `uint64`, not a packed guid).
///
/// Server gates (`MiscHandler.cpp:962-972`) are the same three `CMSG_INSPECT` applies: the target
/// must be an online player, within `INSPECT_DISTANCE` (10.0), and not a valid attack target. Each
/// refusal is a silent `return` — there is no error reply, so a consumer must not block on an
/// answer that may never come. Unlike `CMSG_INSPECT`, this handler does **not** set our selection
/// (`HandleInspectOpcode` opens with `SetSelectionGuid`; this one does not), which is why the send
/// lives in `world::writer::pvp` rather than `writer::selection`.
pub fn inspect_honor_stats(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// An honor payout (`SMSG_PVP_CREDIT`, opcode `0x28C`/652 — VERIFIED vmangos
/// `Opcodes_1_12_1.h`; already named in `opcode_names`). A 16-byte body in the order
/// `WorldPackets::Misc::PvpCredit::AppendBodyTo` writes it (`Server/Packets/Misc.cpp:376-381`),
/// sent from `HonorMgr::SendPVPCredit` (`HonorMgr.cpp:1061-1093`) on every honor row the server
/// books — **including a dishonorable one**, which is why [`PvpCredit::honor`] is signed.
///
/// This is the *only* notice of a specific payout: the descriptor's contribution fields move too,
/// but they are running totals with no attribution. The chat line and the floating combat text
/// both have to come from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PvpCredit {
    /// Honor points, truncated to an integer server-side (`static_cast<int32>(honor)`).
    ///
    /// **Signed, and genuinely negative for a dishonorable kill**: `HonorMgr::Add` computes
    /// `honor = (type == DISHONORABLE) ? -cp : cp` (`HonorMgr.cpp:807`) and hands *that* to
    /// `SendPVPCredit` (`HonorMgr.cpp:833`). A `u32` here would read a DK penalty as ~4 billion
    /// honor.
    pub honor: i32,
    /// Who we killed. **`0` means there is no victim** — vmangos only fills the guid inside
    /// `if (victim)` (`HonorMgr.cpp:1069-1071`), and it passes the *pre-fallback* `realSource`, so
    /// an honor row booked with no source at all arrives with a zero guid. The UI needs a
    /// victim-less phrasing rather than resolving a name for guid 0.
    pub victim_guid: u64,
    /// The victim's **internal** rank — the same `[0..18]` domain as the descriptor's rank bytes
    /// and [`InspectHonorStats::highest_rank`], and therefore a direct index into the
    /// `PVP_RANK_<rank>_<team>` GlobalStrings. NOT the visual rank the badge texture uses; do the
    /// `internal > 4 ? internal - 4 : -internal` conversion (`HonorMgr.cpp:991`) at the display
    /// edge if a badge is wanted, and do not do it twice.
    ///
    /// VERIFIED `HonorMgr::SendPVPCredit` (`HonorMgr.cpp:1078-1089`): a **player** victim sends
    /// `GetHonorMgr().GetRank().rank`, a **creature** victim sends `19` iff it is a racial leader
    /// and otherwise leaves the field at its `0` default.
    ///
    /// One quirk to observe rather than imitate: for a player victim vmangos **floors the value at
    /// 5** (`if (!rank) rank = (HONOR_RANK_COUNT - POSITIVE_HONOR_RANK_COUNT) + 1`, i.e. "at least
    /// Scout"), with the comment *"Never display just 'HK:' without rank name"*. That is the
    /// server working around a client that would otherwise print an empty rank name — it is
    /// **server behaviour we receive**, not a rule we should re-implement: we render whatever
    /// number arrives, and an unranked-victim `0` from some other server must still come out
    /// sanely at the display edge.
    ///
    /// `i32` because that is the wire type; nothing vmangos sends here is negative.
    pub victim_rank: i32,
}

/// Read `SMSG_PVP_CREDIT` (see [`PvpCredit`]): `i32 honor`, `u64 victimGuid`, `i32 victimRank`.
pub(super) fn read_pvp_credit(r: &mut &[u8]) -> io::Result<PvpCredit> {
    Ok(PvpCredit {
        honor: read_i32_le(r)?,
        victim_guid: read_u64_le(r)?,
        victim_rank: read_i32_le(r)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 50-byte reply, hand-built in `InspectHonorStatsResponse::AppendBodyTo` order with the
    /// field boundaries commented — a level-60 target with a rank-11 lifetime best, kills in every
    /// bucket, and a bar three-quarters of the way through the current rank.
    #[test]
    fn inspect_honor_stats_golden() {
        #[rustfmt::skip]
        let body: [u8; 50] = [
            // [0..8) u64 playerGuid = 0x0000_0001_0000_2AB3
            0xB3, 0x2A, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            // [8] u8 highestRank = 11 (internal)
            0x0B,
            // [9..13) u32 sessionKills = 0x0002_0011 — 17 HK low, 2 DK high
            0x11, 0x00, 0x02, 0x00,
            // [13..15) u16 yesterdayHK = 41
            0x29, 0x00,
            // [15..17) u16 unknownOld1 = 0 (server always writes 0)
            0x00, 0x00,
            // [17..19) u16 lastWeekHK = 420
            0xA4, 0x01,
            // [19..21) u16 unknownOld2 = 0
            0x00, 0x00,
            // [21..23) u16 thisWeekHK = 123
            0x7B, 0x00,
            // [23..25) u16 unknownOld3 = 0
            0x00, 0x00,
            // [25..29) u32 lifetimeHK = 3907
            0x43, 0x0F, 0x00, 0x00,
            // [29..33) u32 lifetimeDHK = 12
            0x0C, 0x00, 0x00, 0x00,
            // [33..37) u32 yesterdayHonor = 640
            0x80, 0x02, 0x00, 0x00,
            // [37..41) u32 lastWeekHonor = 8431
            0xEF, 0x20, 0x00, 0x00,
            // [41..45) u32 thisWeekHonor = 1250
            0xE2, 0x04, 0x00, 0x00,
            // [45..49) u32 lastWeekRank = 57 (the STANDING)
            0x39, 0x00, 0x00, 0x00,
            // [49] u8 rankBar = 191
            0xBF,
        ];
        let mut r = body.as_slice();
        let stats = read_inspect_honor_stats(&mut r).unwrap();
        assert!(r.is_empty(), "the whole 50-byte body is consumed");
        assert_eq!(
            stats,
            InspectHonorStats {
                player_guid: 0x0000_0001_0000_2AB3,
                highest_rank: 11,
                session_kills: 0x0002_0011,
                yesterday_hk: 41,
                unknown_old1: 0,
                last_week_hk: 420,
                unknown_old2: 0,
                this_week_hk: 123,
                unknown_old3: 0,
                lifetime_hk: 3_907,
                lifetime_dhk: 12,
                yesterday_honor: 640,
                last_week_honor: 8_431,
                this_week_honor: 1_250,
                last_week_rank: 57,
                rank_bar: 191,
            }
        );
        // The packed today-kills dword splits the same way the descriptor's TWO_SHORT does.
        assert_eq!(stats.session_kills(), (17, 2));
    }

    /// A truncated reply must ERROR, not panic and not yield a half-populated struct — the wire is
    /// hostile input. 49 bytes is the nastiest case: everything but the trailing `rankBar`, which
    /// is exactly what a pre-1.6.1 server would send.
    #[test]
    fn inspect_honor_stats_truncated_body_errors() {
        let full = [0u8; 50];
        for len in [0usize, 8, 13, 49] {
            let mut r = &full[..len];
            assert!(
                read_inspect_honor_stats(&mut r).is_err(),
                "a {len}-byte inspect-honor body must be rejected"
            );
        }
        // …and the full length still parses, so the loop above is testing truncation and not a
        // decoder that rejects everything.
        let mut r = full.as_slice();
        assert!(read_inspect_honor_stats(&mut r).is_ok());
    }

    /// The request is the bare 8-byte guid — byte-exact against
    /// `InspectHonorStats::ReadFromWorldPacket`'s single `recv_data >> guid`.
    #[test]
    fn inspect_honor_stats_request_golden() {
        assert_eq!(
            inspect_honor_stats(0x0000_0001_0000_2AB3),
            vec![0xB3, 0x2A, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00],
        );
        // No sentinel handling: a zero guid is encoded as-is (the server answers by not answering).
        assert_eq!(inspect_honor_stats(0), vec![0u8; 8]);
    }

    /// The 16-byte credit body, hand-built in `PvpCredit::AppendBodyTo` order.
    #[test]
    fn pvp_credit_golden() {
        #[rustfmt::skip]
        let body: [u8; 16] = [
            // [0..4) i32 honor = 143
            0x8F, 0x00, 0x00, 0x00,
            // [4..12) u64 victimGuid = 0x0000_0001_0000_2AB3
            0xB3, 0x2A, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            // [12..16) i32 victimRank = 11 (INTERNAL rank — the PVP_RANK_11_<team> key)
            0x0B, 0x00, 0x00, 0x00,
        ];
        let mut r = body.as_slice();
        let credit = read_pvp_credit(&mut r).unwrap();
        assert!(r.is_empty(), "the whole 16-byte body is consumed");
        assert_eq!(
            credit,
            PvpCredit {
                honor: 143,
                victim_guid: 0x0000_0001_0000_2AB3,
                victim_rank: 11,
            }
        );
    }

    /// A **dishonorable** kill rides the same packet with a negative `honor`
    /// (`HonorMgr.cpp:807`, `honor = -cp`) — the reason the field is `i32`. Decoded as `u32` this
    /// body would read as 4 294 967 291 honor.
    #[test]
    fn pvp_credit_dishonorable_kill_is_negative_honor() {
        let mut body = Vec::with_capacity(16);
        body.extend_from_slice(&(-5i32).to_le_bytes());
        body.extend_from_slice(&0x0000_0001_0000_2AB3u64.to_le_bytes());
        body.extend_from_slice(&5i32.to_le_bytes()); // floored to "Scout" server-side
        let mut r = body.as_slice();
        let credit = read_pvp_credit(&mut r).unwrap();
        assert!(r.is_empty());
        assert_eq!(credit.honor, -5);
        assert_eq!(credit.victim_rank, 5);
    }

    /// The victim-less shape the UI has to phrase differently: vmangos fills the guid only inside
    /// `if (victim)` and passes the pre-fallback `realSource`, so an honor row with no source
    /// arrives as a zero guid with the rank left at its `0` default (which is also what a
    /// non-racial-leader creature victim sends).
    #[test]
    fn pvp_credit_zero_guid_means_no_victim() {
        let mut body = Vec::with_capacity(16);
        body.extend_from_slice(&5i32.to_le_bytes());
        body.extend_from_slice(&0u64.to_le_bytes()); // no victim
        body.extend_from_slice(&0i32.to_le_bytes()); // and so no rank
        let mut r = body.as_slice();
        let credit = read_pvp_credit(&mut r).unwrap();
        assert!(r.is_empty());
        assert_eq!(credit.victim_guid, 0, "zero guid = no victim");
        assert_eq!(credit.victim_rank, 0);
    }

    /// A truncated credit body errors rather than panicking.
    #[test]
    fn pvp_credit_truncated_body_errors() {
        let full = [0u8; 16];
        for len in [0usize, 4, 12, 15] {
            let mut r = &full[..len];
            assert!(
                read_pvp_credit(&mut r).is_err(),
                "a {len}-byte credit body must be rejected"
            );
        }
        let mut r = full.as_slice();
        assert!(read_pvp_credit(&mut r).is_ok());
    }
}
