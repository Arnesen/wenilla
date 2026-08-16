//! The guild family — the roster, the guild query, invitations, rank administration and the event
//! broadcast (opcodes `0x54`/`0x55`, `0x81`–`0x93`, `0x231`–`0x235`, `0x2FC`).
//!
//! The server side is pinned at the bytes by vmangos `Server/Packets/Guild.{h,cpp}` (every
//! `ReadFromWorldPacket` / `AppendBodyTo`), `Guild/Guild.{h,cpp}` (`SendGuildRoster`,
//! `SendQueryResponse`, `BroadcastEvent`, and the enums) and `Handlers/GuildHandler.cpp` (which
//! `NullClientPacket` handlers take an empty body).
//!
//! The client side of `SMSG_GUILD_EVENT` is VERIFIED in wow-re
//! (`system/object-layer/scratch/rf77-smsg-chat-wire-order.md`, RF-0077 corrected): opcode `0x92`
//! registers handler `0x5e7180` (`0x5e337a mov ecx,0x92; call 0x5ab650`), which reads `u8
//! eventType`, `u8 strCount` (≤ 3 — three stack buffers), `strCount` × CString, and then — **only**
//! in the `0xc`/`0xd` arms of its `switch` (jump table `0x5e74dc`) — a trailing `u64` guid via
//! `0x4190b0`. That is the read [`read_guild_event`] implements, and it is *narrower* than what
//! vmangos writes (see that function's doc).
//!
//! The client side of `SMSG_GUILD_ROSTER` and `SMSG_GUILD_QUERY_RESPONSE` is likewise VERIFIED in
//! wow-re (`system/ui/scratch/guild-roster-wire.md`), by a §5 trio deriving each independently.
//! The two do **not** live in the guild-init block the event handler does: `0x8a` is handler
//! `0x4d0ad0` (registered `0x4d0a0e`) in a **`GuildInfo.cpp`** TU, and `0x55` is `0x555290`
//! (registered `0x555073`) in **dbcache**, delegating to decoder `0x62f260` — which wow-re already
//! held bit-exact as `PRIMITIVE:dbcache_guild`. Per-field addresses are on the two readers below.
//!
//! **The parse trap in this family is `SMSG_GUILD_ROSTER`'s `f32 lastOnlineTime`**: it rides on the
//! member's presence byte, so reading it unconditionally desynchronises every *later* member —
//! exactly the shape [`super::social::read_friend_status`] already has, and written the same way
//! (an explicit condition on the field the wire keys off, never a "read if bytes remain").
//!
//! **One reference behaviour we deliberately do not reproduce: capped string reads.** The client's
//! `0x4191b0` takes a per-field capacity (counting the NUL, so the real cap is `cap - 1`) — roster
//! name `0x30`, notes `0x80`, motd `0x200`, info text `0x7d0`, query guild name `0x60`, rank name
//! `0x40`. On overflow it does not truncate gracefully: it slams the cursor to `size + 1`
//! (`0x4192aa`) and NULs `dest[0]` (`0x4192be`), so the field arrives **empty** *and every later
//! read in that packet silently no-ops*. [`read_cstring`] is unbounded, which is strictly safer and
//! cannot poison a packet — and it costs no fidelity, because every one of those caps is far above
//! the content limit the server enforces (a 15-character rank name into a 64-byte buffer, a
//! 128-character MOTD into 512). The caps are recorded here because they explain a class of
//! "the guild pane went blank" behaviour in the reference, not because we want them.

use std::io::{self, Read};

use crate::wire::{read_cstring, read_f32_le, read_u32_le, read_u64_le, read_u8};

/// A guild always has at least this many ranks — vmangos `GUILD_RANKS_MIN_COUNT` (`Guild/Guild.h:31`);
/// the five defaults are GM / Officer / Veteran / Member / Initiate ([`guild_default_rank`]).
pub const GUILD_RANKS_MIN_COUNT: usize = 5;
/// …and at most this many — vmangos `GUILD_RANKS_MAX_COUNT` (`Guild/Guild.h:32`). Load-bearing for
/// the parse, not just a policy number: `SMSG_GUILD_QUERY_RESPONSE` always carries **exactly**
/// this many rank-name cstrings, padding unused ranks with empty strings
/// (`Guild/Guild.cpp:868-872`, whose own comment is "show always 10 ranks").
pub const GUILD_RANKS_MAX_COUNT: usize = 10;

/// Rank-name cap, in *characters* — vmangos `GUILD_RANK_MAX_LENGTH` (`Guild/Guild.h:36`).
///
/// These five caps are the reference UI's edit-box `maxLetters`, and vmangos re-checks the three
/// it can (`utf8length`, not byte length). Over-long is **not** a polite refusal on the rank ones:
/// `HandleGuildRankOpcode` / `HandleGuildAddRankOpcode` (`Handlers/GuildHandler.cpp:580-584`,
/// `:600-604`) run `ProcessAnticheatAction(… CHEAT_ACTION_KICK)`. The builders below do **not**
/// truncate — silently shortening a rank name or a note would send the wrong text, which is its
/// own bug — so the caller enforces the cap at the edit box, as the reference UI does.
pub const GUILD_RANK_MAX_LENGTH: usize = 15;
/// Guild-name cap, in characters — vmangos `GUILD_NAME_MAX_LENGTH` (`Guild/Guild.h:37`).
pub const GUILD_NAME_MAX_LENGTH: usize = 24;
/// Public/officer note cap, in characters — vmangos `GUILD_NOTE_MAX_LENGTH` (`Guild/Guild.h:38`).
pub const GUILD_NOTE_MAX_LENGTH: usize = 31;
/// Guild info text cap, in characters — vmangos `GUILD_INFO_MAX_LENGTH` (`Guild/Guild.h:39`).
pub const GUILD_INFO_MAX_LENGTH: usize = 500;
/// MOTD cap, in characters — vmangos `GUILD_MOTD_MAX_LENGTH` (`Guild/Guild.h:40`).
pub const GUILD_MOTD_MAX_LENGTH: usize = 128;

/// The five ranks every guild is created with and cannot delete — vmangos `GuildDefaultRanks`
/// (`Guild/Guild.h:44-54`). Rank ids are **0-based with 0 = guild master**, and the ordering is the
/// authority hierarchy: promote does `rank--`, demote does `rank++` (vmangos's own comments).
pub mod guild_default_rank {
    /// `GR_GUILDMASTER` — rank 0. `HandleGuildRankOpcode` force-overwrites this rank's rights with
    /// `GR_RIGHT_ALL` whatever the client sends, "to prevent loss of leader rights".
    pub const GUILDMASTER: u32 = 0;
    /// `GR_OFFICER`.
    pub const OFFICER: u32 = 1;
    /// `GR_VETERAN`.
    pub const VETERAN: u32 = 2;
    /// `GR_MEMBER`.
    pub const MEMBER: u32 = 3;
    /// `GR_INITIATE` — the lowest default rank.
    pub const INITIATE: u32 = 4;
}

/// One rank's rights bitmask — vmangos `GuildRankRights` (`Guild/Guild.h:56-74`), **with
/// `GR_RIGHT_EMPTY` factored out**.
///
/// vmangos writes every named right as `GR_RIGHT_EMPTY | bit` (`GR_RIGHT_GCHATLISTEN = 0x41`,
/// not `0x01`) because its test is `(rights & right) != GR_RIGHT_EMPTY` — the `0x40` is a sentinel
/// that makes that comparison work, not a right. The meaningful bit is the other half, and that is
/// what is named here; [`EMPTY`] is kept so the vmangos constants can be reconstructed and
/// recognised.
pub mod guild_rank_right {
    /// `GR_RIGHT_EMPTY` (`0x40`) — the sentinel described in the module doc. Carries no permission.
    pub const EMPTY: u32 = 0x0000_0040;
    /// Read guild chat (`GR_RIGHT_GCHATLISTEN`, vmangos `0x41`).
    pub const GCHAT_LISTEN: u32 = 0x0000_0001;
    /// Speak in guild chat (`GR_RIGHT_GCHATSPEAK`, vmangos `0x42`).
    pub const GCHAT_SPEAK: u32 = 0x0000_0002;
    /// Read officer chat (`GR_RIGHT_OFFCHATLISTEN`, vmangos `0x44`).
    pub const OFFCHAT_LISTEN: u32 = 0x0000_0004;
    /// Speak in officer chat (`GR_RIGHT_OFFCHATSPEAK`, vmangos `0x48`).
    pub const OFFCHAT_SPEAK: u32 = 0x0000_0008;
    /// Invite new members (`GR_RIGHT_INVITE`, vmangos `0x50`).
    pub const INVITE: u32 = 0x0000_0010;
    /// Kick members (`GR_RIGHT_REMOVE`, vmangos `0x60`).
    pub const REMOVE: u32 = 0x0000_0020;
    /// Promote members (`GR_RIGHT_PROMOTE`, vmangos `0xC0`).
    pub const PROMOTE: u32 = 0x0000_0080;
    /// Demote members (`GR_RIGHT_DEMOTE`, vmangos `0x140`).
    pub const DEMOTE: u32 = 0x0000_0100;
    /// Set the message of the day (`GR_RIGHT_SETMOTD`, vmangos `0x1040`).
    pub const SET_MOTD: u32 = 0x0000_1000;
    /// Edit any member's public note (`GR_RIGHT_EPNOTE`, vmangos `0x2040`).
    pub const EDIT_PUBLIC_NOTE: u32 = 0x0000_2000;
    /// **See** officer notes at all (`GR_RIGHT_VIEWOFFNOTE`, vmangos `0x4040`). Without it the
    /// server blanks every `officerNote` in the roster it sends us
    /// (`Guild/Guild.cpp:821`, `:844`) — the empty string is a *permission* signal, not "no note".
    pub const VIEW_OFFICER_NOTE: u32 = 0x0000_4000;
    /// Edit officer notes (`GR_RIGHT_EOFFNOTE`, vmangos `0x8040`).
    pub const EDIT_OFFICER_NOTE: u32 = 0x0000_8000;
    /// Edit the guild information text (`GR_RIGHT_MODIFY_GUILD_INFO`, vmangos `0x10040`).
    pub const MODIFY_GUILD_INFO: u32 = 0x0001_0000;
    /// `GR_RIGHT_ALL` — what the server force-writes onto rank 0 regardless of what we send.
    /// Unlike the named rights above, this is a **raw wire value, sentinel and all**: it includes
    /// [`EMPTY`], and it lights bits (`0x0000_0E00`, `0x000E_0000`) that no 1.12 right owns. Test
    /// a rank against the named bits, never against this.
    pub const ALL: u32 = 0x000F_F1FF;
}

/// The thirteen rank-right checkboxes **in the order the FrameXML API indexes them**.
///
/// `GuildControlGetRankFlags()` returns a flat list and `GuildControlSetRankFlag(i, on)` takes an
/// index into it, so **index order is the API contract** — and it is deliberately *not* bit order:
/// Promote/Demote (indices 5/6 here) sit above Invite/Remove (7/8) in bit value. Anything that
/// computes the bit as `1 << (i - 1)` is wrong; this table is the mapping.
pub const GUILD_RANK_RIGHT_ORDER: [u32; 13] = [
    guild_rank_right::GCHAT_LISTEN,
    guild_rank_right::GCHAT_SPEAK,
    guild_rank_right::OFFCHAT_LISTEN,
    guild_rank_right::OFFCHAT_SPEAK,
    guild_rank_right::PROMOTE,
    guild_rank_right::DEMOTE,
    guild_rank_right::INVITE,
    guild_rank_right::REMOVE,
    guild_rank_right::SET_MOTD,
    guild_rank_right::EDIT_PUBLIC_NOTE,
    guild_rank_right::VIEW_OFFICER_NOTE,
    guild_rank_right::EDIT_OFFICER_NOTE,
    guild_rank_right::MODIFY_GUILD_INFO,
];

/// A roster member's presence byte — vmangos `GuildRosterPresenceFlags` (`Guild/Guild.h:139-144`).
///
/// `0` (no bit at all) is the only value the wire treats specially: it is what makes the member's
/// `f32 lastOnlineTime` present. The AFK/DND bits ride alongside `ONLINE`, exactly as the friend
/// list's [`super::social::friend_status`] does.
///
/// The reference client tests these as its own chat flags, and **DND is tested first**
/// (`0x4d13e4` before `0x4d13fb`, wow-re `system/ui/scratch/guild-roster-wire.md`) — so a member
/// flagged both AFK and DND displays as DND. That precedence is a display rule, not a wire rule,
/// but it belongs with the bits.
pub mod guild_presence {
    /// Offline — and **only** this value carries the `lastOnlineTime` float.
    pub const OFFLINE: u8 = 0x00;
    /// `GRF_ONLINE`. Value INFERRED from vmangos: the reference client only ever tests the whole
    /// byte against zero, so nothing in the binary pins this bit specifically.
    pub const ONLINE: u8 = 0x01;
    /// `GRF_AFK` — set alongside [`ONLINE`]. VERIFIED as the client's own `CHAT_FLAG_AFK`.
    pub const AFK: u8 = 0x02;
    /// `GRF_DND` — set alongside [`ONLINE`]. VERIFIED as the client's own `CHAT_FLAG_DND`, and
    /// tested ahead of [`AFK`].
    pub const DND: u8 = 0x04;
}

/// `SMSG_GUILD_EVENT`'s leading byte — vmangos `GuildEvents` (`Guild/Guild.h:121-137`), which is
/// also the reference client's own `switch` (14 arms, jump table `0x5e74dc`; wow-re RF-0077).
pub mod guild_event {
    /// `GE_PROMOTION` — params: promoter, promoted, new rank name.
    pub const PROMOTION: u8 = 0x00;
    /// `GE_DEMOTION` — params: demoter, demoted, new rank name.
    pub const DEMOTION: u8 = 0x01;
    /// `GE_MOTD` — param: the new message of the day. The reference client routes this one
    /// through the profanity filter (`0x703f50(0x11c,…)`, CVar-gated), not the normal event path.
    pub const MOTD: u8 = 0x02;
    /// `GE_JOINED` — param: the joiner's name.
    pub const JOINED: u8 = 0x03;
    /// `GE_LEFT` — param: the leaver's name.
    pub const LEFT: u8 = 0x04;
    /// `GE_REMOVED` — params: the removed player, then who removed them.
    pub const REMOVED: u8 = 0x05;
    /// `GE_LEADER_IS` — param: the current leader.
    pub const LEADER_IS: u8 = 0x06;
    /// `GE_LEADER_CHANGED` — params: old leader, new leader.
    pub const LEADER_CHANGED: u8 = 0x07;
    /// `GE_DISBANDED` — no params.
    pub const DISBANDED: u8 = 0x08;
    /// `GE_TABARDCHANGE`. The reference client falls through to its default arm for this one.
    pub const TABARD_CHANGE: u8 = 0x09;
    /// `GE_UPDATE_RANK_NAME` — params: rank id, new rank name. The reference client writes it
    /// straight into the roster cache (`0x560e30`) and fires **no** FrameScript event.
    pub const UPDATE_RANK_NAME: u8 = 0x0A;
    /// `GE_UPDATE_ROSTER` — "re-ask for the roster". Silent in the reference client; vmangos never
    /// sends it (it re-sends the whole `SMSG_GUILD_ROSTER` instead).
    pub const UPDATE_ROSTER: u8 = 0x0B;
    /// `GE_SIGNED_ON` — param: the guildmate's name. **Carries the trailing guid.**
    pub const SIGNED_ON: u8 = 0x0C;
    /// `GE_SIGNED_OFF` — param: the guildmate's name. **Carries the trailing guid.**
    pub const SIGNED_OFF: u8 = 0x0D;
}

/// `SMSG_GUILD_COMMAND_RESULT`'s leading `u32` — which *verb* the result is about (vmangos
/// `Typecommand`, `Guild/Guild.h:76-85`).
///
/// It is a much coarser tag than the opcode that provoked it: vmangos answers "you're not in a
/// guild" to `CMSG_GUILD_INFO`, `_ROSTER`, `_LEAVE`, `_DISBAND` and half a dozen others all as
/// [`CREATE`], and every permission refusal as [`INVITE`]. So the pair
/// `(command, result)` names the *message*, not the request — vmangos's own comment notes that
/// values 2, 4-13 and 15-18 "have no effect" here.
pub mod guild_command {
    /// `GUILD_CREATE_S` — also vmangos's catch-all for "you are not in a guild".
    pub const CREATE: u32 = 0x00;
    /// `GUILD_INVITE_S` — also vmangos's catch-all for a permission refusal.
    pub const INVITE: u32 = 0x01;
    /// `GUILD_QUIT_S` — leaving/being removed.
    pub const QUIT: u32 = 0x03;
    /// `GUILD_FOUNDER_S`.
    pub const FOUNDER: u32 = 0x0E;
    /// `GUILD_UNK19`.
    pub const UNK19: u32 = 0x13;
    /// `GUILD_UNK20`.
    pub const UNK20: u32 = 0x14;
}

/// `SMSG_GUILD_COMMAND_RESULT`'s trailing `u32` — vmangos `CommandErrors`
/// (`Guild/Guild.h:96-119`). The string field between the two carries whatever `%s` the matching
/// `ERR_GUILD_*` GlobalString needs (a player name, a guild name, or empty).
///
/// Note [`LEADER_LEAVE`] and [`PERMISSIONS`] are the **same** value `0x08`: which one it means is
/// decided by the [`guild_command`] tag beside it (`QUIT` → the leader-can't-leave message,
/// anything else → the permission refusal). That collision is the reason the command tag has to be
/// carried through to the consumer rather than dropped.
pub mod guild_command_error {
    /// `ERR_PLAYER_NO_MORE_IN_GUILD` — vmangos's own comment: "no message/error".
    pub const PLAYER_NO_MORE_IN_GUILD: u32 = 0x00;
    /// `ERR_GUILD_INTERNAL`.
    pub const INTERNAL: u32 = 0x01;
    /// `ERR_ALREADY_IN_GUILD` — *you* are.
    pub const ALREADY_IN_GUILD: u32 = 0x02;
    /// `ERR_ALREADY_IN_GUILD_S` — *they* are (the string names them).
    pub const ALREADY_IN_GUILD_S: u32 = 0x03;
    /// `ERR_INVITED_TO_GUILD`.
    pub const INVITED_TO_GUILD: u32 = 0x04;
    /// `ERR_ALREADY_INVITED_TO_GUILD_S`.
    pub const ALREADY_INVITED_TO_GUILD_S: u32 = 0x05;
    /// `ERR_GUILD_NAME_INVALID`.
    pub const NAME_INVALID: u32 = 0x06;
    /// `ERR_GUILD_NAME_EXISTS_S`.
    pub const NAME_EXISTS_S: u32 = 0x07;
    /// `ERR_GUILD_LEADER_LEAVE` — the guild master cannot `/gquit` while anyone else remains.
    /// Same value as [`PERMISSIONS`]; disambiguated by [`guild_command::QUIT`].
    pub const LEADER_LEAVE: u32 = 0x08;
    /// `ERR_GUILD_PERMISSIONS` — see [`LEADER_LEAVE`].
    pub const PERMISSIONS: u32 = 0x08;
    /// `ERR_GUILD_PLAYER_NOT_IN_GUILD` — *you* aren't.
    pub const PLAYER_NOT_IN_GUILD: u32 = 0x09;
    /// `ERR_GUILD_PLAYER_NOT_IN_GUILD_S` — *they* aren't.
    pub const PLAYER_NOT_IN_GUILD_S: u32 = 0x0A;
    /// `ERR_GUILD_PLAYER_NOT_FOUND_S`.
    pub const PLAYER_NOT_FOUND_S: u32 = 0x0B;
    /// `ERR_GUILD_NOT_ALLIED` — cross-faction, refused by config.
    pub const NOT_ALLIED: u32 = 0x0C;
    /// `ERR_GUILD_RANK_TOO_HIGH_S`.
    pub const RANK_TOO_HIGH_S: u32 = 0x0D;
    /// `ERR_GUILD_RANK_TOO_LOW_S`.
    pub const RANK_TOO_LOW_S: u32 = 0x0E;
    /// `ERR_GUILD_RANKS_LOCKED` (`0x0F`/`0x10` are unused at 5875).
    pub const RANKS_LOCKED: u32 = 0x11;
    /// `ERR_GUILD_RANK_IN_USE` — can't delete the lowest rank while somebody holds it.
    pub const RANK_IN_USE: u32 = 0x12;
    /// `ERR_GUILD_IGNORING_YOU_S`.
    pub const IGNORING_YOU_S: u32 = 0x13;
    /// `ERR_GUILD_UNK20` — vmangos: "for Typecommand 0x05 only".
    pub const UNK20: u32 = 0x14;
}

/// `SMSG_GUILD_QUERY_RESPONSE` — one guild's public identity: its name, its ten rank names, and
/// its tabard.
///
/// This is an ask-once cache fill, the guild twin of the name query: the roster and every chat
/// line reference a guild by id, and this is what turns that id into text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuildQueryResponse {
    /// The guild id we asked about.
    pub guild_id: u32,
    /// The guild's name — and, **empty, the "no such guild" answer**. The reference client tests
    /// exactly this (`0x5552ae test al,al` on the decoded record's name, choosing the cache
    /// *insert* `0x561070` or the cache *remove* `0x561390`); there is no separate not-found flag,
    /// and the full record is consumed either way. A consumer must therefore treat an empty name
    /// as "this id names nothing", not as "a guild with a blank name".
    pub name: String,
    /// **Always exactly [`GUILD_RANKS_MAX_COUNT`] entries**, indexed by rank id (0 = guild
    /// master); ranks the guild has not created are empty strings, not absent. A guild has between
    /// [`GUILD_RANKS_MIN_COUNT`] and [`GUILD_RANKS_MAX_COUNT`] real ranks, so the tail of this
    /// array is normally empty — "how many ranks exist" is the count of leading non-empty entries,
    /// or better, the roster's `rank_rights` length.
    pub rank_names: [String; GUILD_RANKS_MAX_COUNT],
    /// Tabard emblem symbol index.
    pub emblem_style: u32,
    /// Tabard emblem colour index.
    pub emblem_color: u32,
    /// Tabard border style index.
    pub border_style: u32,
    /// Tabard border colour index.
    pub border_color: u32,
    /// Tabard background colour index.
    pub background_color: u32,
}

/// Read `SMSG_GUILD_QUERY_RESPONSE` (VERIFIED **both ends**): `u32 guildId`, the guild name, then
/// **exactly ten** rank-name cstrings, then the five tabard `u32`s.
///
/// Server: vmangos `Server/Packets/Guild.cpp:118-131` `GuildQueryResponse::AppendBodyTo`, filled by
/// `Guild/Guild.cpp:862-880` `SendQueryResponse`. Client: wow-re
/// `system/ui/scratch/guild-roster-wire.md` — handler `0x555290` (dbcache) delegating to decoder
/// `0x62f260`, reads at `0x62f26e` (id), `0x62f27b` (name), `0x62f295` × 10, then `0x62f2af`,
/// `0x62f2bd`, `0x62f2cb`, `0x62f2d9`, `0x62f2e7`.
///
/// The ten is a fixed loop over the sender's `rankNames[10]` array, **not** a counted list — the
/// server pads with empty strings for ranks the guild never created ("show always 10 ranks", its
/// own comment), and the client's own loop bound is the compile-time constant `0x62f283 mov
/// [ebp+8],0xa` on a `do`/`while`, so it always runs ten times. Reading a count first, or stopping
/// at the guild's real rank count, would land the parse in the middle of the emblem block.
pub(super) fn read_guild_query_response(r: &mut impl Read) -> io::Result<GuildQueryResponse> {
    let guild_id = read_u32_le(r)?;
    let name = read_cstring(r)?;
    let mut rank_names: [String; GUILD_RANKS_MAX_COUNT] = Default::default();
    for rank_name in &mut rank_names {
        *rank_name = read_cstring(r)?;
    }
    Ok(GuildQueryResponse {
        guild_id,
        name,
        rank_names,
        emblem_style: read_u32_le(r)?,
        emblem_color: read_u32_le(r)?,
        border_style: read_u32_le(r)?,
        border_color: read_u32_le(r)?,
        background_color: read_u32_le(r)?,
    })
}

/// One row of `SMSG_GUILD_ROSTER`.
///
/// Unlike a friend-list slot, every field the guild pane shows *is* on the wire — a roster is a
/// snapshot the server composed, so no name-cache round trip is needed to draw it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GuildRosterMember {
    /// The member's player guid (vmangos always builds it as `ObjectGuid(HIGHGUID_PLAYER, …)`).
    pub guid: u64,
    /// [`guild_presence`] bits. [`guild_presence::OFFLINE`] (`0`) is the value that makes
    /// [`Self::last_online_days`] present on the wire — see [`Self::is_online`].
    pub presence: u8,
    /// Character name.
    pub name: String,
    /// Rank id, 0-based with 0 = guild master; indexes [`GuildQueryResponse::rank_names`] and
    /// [`GuildRoster::rank_rights`].
    pub rank_id: u32,
    /// Character level. A `u8` on the wire, not a `u32` — the roster is the widest packet in the
    /// game and it is packed accordingly.
    pub level: u8,
    /// Class id (`ChrClasses.dbc`).
    pub class: u8,
    /// Zone id (`AreaTable.dbc`). For an offline member this is their *last known* zone, out of
    /// the character table.
    pub zone: u32,
    /// Days since this member last logged out, as a fraction — vmangos computes
    /// `(now - logoutTime) / DAY` (`Guild/Guild.cpp:841`). **Only on the wire for offline
    /// members**; reads back as `0.0` for online ones, where it is meaningless. The reference UI's
    /// `GetGuildRosterLastOnline` binding is what splits it into years/months/days.
    pub last_online_days: f32,
    /// The member's public note (empty if unset).
    pub public_note: String,
    /// The member's officer note — **empty when *we* lack [`guild_rank_right::VIEW_OFFICER_NOTE`]**,
    /// because the server blanks it per-viewer (`Guild/Guild.cpp:821`, `:844`). An empty string
    /// therefore does not distinguish "no note" from "not allowed to see it"; the viewer's own
    /// rank rights do.
    pub officer_note: String,
}

impl GuildRosterMember {
    /// Is this member online at all (any of ONLINE/AFK/DND)? This is the wire's own test — vmangos
    /// writes [`Self::last_online_days`] under exactly `if (!member.presenceFlags)`, and
    /// [`read_guild_roster`] reads it under the same condition.
    ///
    /// It is a **whole-byte** comparison against [`guild_presence::OFFLINE`], not `presence &
    /// ONLINE`, and that is the reference client's own predicate rather than a convenience: it
    /// derives the flag with `0x4d0c12 test dl,dl` / `0x4d0c1d setne cl` before branching on it.
    /// An unknown future presence bit therefore reads as *online*, which is the safe direction —
    /// masking `0x1` would mis-classify it and take the wrong branch on the float.
    pub fn is_online(&self) -> bool {
        self.presence != guild_presence::OFFLINE
    }
}

/// `SMSG_GUILD_ROSTER` — the whole guild in one packet: the MOTD, the info text, every rank's
/// rights, and every member.
///
/// Always a complete snapshot, never a delta: vmangos re-sends the whole thing for any change
/// worth showing (a note edit, a rank-rights change, a promotion). It is also the game's widest
/// packet, and the sender **truncates the member list** to fit `GUILD_ROSTER_MAX_LENGTH`
/// (`0x8000 - 4` bytes, `Guild/Guild.h:41`) — so [`Self::members`] can be shorter than the guild
/// for a very large guild with very long notes, which is the server's behaviour, not a parse error.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GuildRoster {
    /// The message of the day.
    pub motd: String,
    /// The guild information text (the "Guild Information" pane). 1.12-only — vmangos writes it
    /// under `SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_8_4`.
    pub info: String,
    /// One rights bitmask ([`guild_rank_right`]) per rank, indexed by rank id. Its **length is the
    /// guild's real rank count** — the one place on the wire that says how many of
    /// [`GuildQueryResponse::rank_names`]'s ten slots are live.
    ///
    /// More than [`GUILD_RANKS_MAX_COUNT`] entries is malformed. We still read them all (dropping
    /// any would desynchronise everything after) and hand them over; a consumer indexing a fixed
    /// ten-slot model must bound itself. The reference client does **not**: its loop writes
    /// straight into a 10-entry array at `[0xb726d0, 0xb726f8)`, so a `rankCount >= 12` walks over
    /// that array's own container fields — a bug to clamp around, never to reproduce (wow-re
    /// `system/ui/scratch/guild-roster-wire.md`).
    pub rank_rights: Vec<u32>,
    /// The members, **arrival order** — vmangos iterates its `members` map, i.e. by low guid.
    ///
    /// This is not display order and is not stable in any way a UI should lean on: the reference
    /// client re-`qsort`s its own copy after every packet (`0x4d0d32`). Sorting is the consumer's
    /// job; this vector is what came off the wire.
    pub members: Vec<GuildRosterMember>,
}

/// A defensive cap on the `Vec::with_capacity` hint taken from the wire's member count: a
/// corrupt/hostile `u32` must not turn into a multi-gigabyte allocation before the first read
/// fails. The real ceiling is the packet's own `0x8000`-byte budget, which cannot hold more than
/// ~1.4k members at the minimum member size.
const ROSTER_CAPACITY_HINT_CAP: usize = 2048;

/// Read `SMSG_GUILD_ROSTER` (VERIFIED **both ends**): `u32 memberCount`, the MOTD, the info text,
/// `u32 rankCount`, that many `u32` rights, then the members.
///
/// Each member is `u64 guid`, `u8 presenceFlags`, name, `u32 rankId`, `u8 level`, `u8 classId`,
/// `u32 zoneId`, **`f32 lastOnlineTime` if and only if `presenceFlags == 0`**, public note,
/// officer note.
///
/// Server: vmangos `Server/Packets/Guild.cpp:143-173` `GuildRoster::AppendBodyTo`, filled by
/// `Guild/Guild.cpp:803-858` `SendGuildRoster`. Client: wow-re
/// `system/ui/scratch/guild-roster-wire.md` — handler `0x4d0ad0`, head reads at `0x4d0af3`,
/// `0x4d0b65`, `0x4d0b82`, `0x4d0b9a`, `0x4d0bb3`; member reads at `0x4d0bfd`, `0x4d0c08`,
/// `0x4d0c43`, `0x4d0c56`, `0x4d0c61`, `0x4d0c6c`, `0x4d0ca0`, `0x4d0cc2`, `0x4d0ce7`, `0x4d0d02`.
///
/// That condition is the whole difficulty of this packet. An offline member is 4 bytes longer than
/// an online one, so a parser that reads the float unconditionally does not merely get one field
/// wrong — it walks off into the following cstrings and mis-parses **every remaining member**.
/// It is spelled out as an explicit test on the presence byte rather than a "read if bytes remain"
/// check for the same reason [`super::social::read_friend_status`] is: the wire keys the field off
/// a value, and only that value can decide it.
///
/// Three independent sources agree on it, and on its exact shape. vmangos states it twice — the
/// writer (`Guild.cpp:164-165`) and the size accounting that decides where to truncate
/// (`Guild/Guild.cpp:793-794`). The binary states it once, precisely: `0x4d0cad cmp
/// DWORD PTR [eax+0x48],edi` (with `edi` held at `0` across the loop) / `0x4d0cb0 je 0x4d0cba`,
/// whose taken leg is the guarded read `0x4d0cc2`; the other leg consumes nothing and stores `0`.
/// Two details of that carve are load-bearing here: the predicate is the **whole presence byte**
/// against zero (`0x4d0c12 test dl,dl` → `0x4d0c1d setne cl`), never `flags & ONLINE` — which is
/// why [`GuildRosterMember::is_online`] compares against [`guild_presence::OFFLINE`] rather than
/// masking a bit. And the field really is an `f32`: the client's 4-byte getters are byte-identical
/// template clones that could never settle a type, so it is pinned at two *consumers* instead —
/// `0x4d0f19 fld` / `0x4d0f1f fcomp` in the roster comparator, and `0x4d1508 fld` in
/// `GetGuildRosterLastOnline`.
pub(super) fn read_guild_roster(r: &mut impl Read) -> io::Result<GuildRoster> {
    let member_count = read_u32_le(r)?;
    let motd = read_cstring(r)?;
    let info = read_cstring(r)?;

    let rank_count = read_u32_le(r)?;
    let mut rank_rights = Vec::with_capacity((rank_count as usize).min(GUILD_RANKS_MAX_COUNT));
    for _ in 0..rank_count {
        rank_rights.push(read_u32_le(r)?);
    }

    let mut members = Vec::with_capacity((member_count as usize).min(ROSTER_CAPACITY_HINT_CAP));
    for _ in 0..member_count {
        let mut member = GuildRosterMember {
            guid: read_u64_le(r)?,
            presence: read_u8(r)?,
            name: read_cstring(r)?,
            rank_id: read_u32_le(r)?,
            level: read_u8(r)?,
            class: read_u8(r)?,
            zone: read_u32_le(r)?,
            ..Default::default()
        };
        if !member.is_online() {
            member.last_online_days = read_f32_le(r)?;
        }
        member.public_note = read_cstring(r)?;
        member.officer_note = read_cstring(r)?;
        members.push(member);
    }

    Ok(GuildRoster {
        motd,
        info,
        rank_rights,
        members,
    })
}

/// `SMSG_GUILD_EVENT` — one thing that happened in the guild, as the event id plus its
/// already-formatted `%s` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuildEventNotice {
    /// A [`guild_event`] id. The reference client's `switch` covers `0x00`–`0x0D` and treats
    /// anything above as its default arm, so an unknown id is a display question, not a parse one.
    pub event: u8,
    /// The event's string arguments, in order — at most **three** (the reference handler has
    /// exactly three stack buffers and its `strCount` is capped at 3).
    pub params: Vec<String>,
    /// The guildmate the event is about — present for [`guild_event::SIGNED_ON`] and
    /// [`guild_event::SIGNED_OFF`] only. See [`read_guild_event`] for why that is narrower than
    /// what vmangos writes.
    pub guid: Option<u64>,
}

/// Read `SMSG_GUILD_EVENT` (VERIFIED both ends: vmangos `Server/Packets/Guild.cpp:133-141`
/// `GuildEvent::AppendBodyTo`, and the reference client's own handler `0x5e7180` — wow-re RF-0077,
/// `system/object-layer/scratch/rf77-smsg-chat-wire-order.md`): `u8 event`, `u8 paramCount`, that
/// many cstrings, then the optional trailing guid.
///
/// **The two ends disagree about *when* the guid is there, and we follow the client.** vmangos
/// writes it whenever its `affectedPlayerGuid` is set, which its callers do for `GE_JOINED`
/// (`Handlers/GuildHandler.cpp:218`) and `GE_LEFT` (`:405`, `Guild/Guild.cpp:572`) as well as
/// `GE_SIGNED_ON`/`GE_SIGNED_OFF`. The reference client reads it in **only** the `0xc`/`0xd` arms
/// of its jump table — for the other two the guid is simply trailing slack it never touches (the
/// body is length-framed, so ignoring a trailing field costs nothing and desynchronises nothing).
/// benilla is the client, so this reads on the event id, and `GE_JOINED`/`GE_LEFT` report
/// `guid: None` even when the bytes are present. Nothing is lost: those two carry the player's
/// **name** in `params`, which is what the event is displayed by.
pub(super) fn read_guild_event(r: &mut impl Read) -> io::Result<GuildEventNotice> {
    let event = read_u8(r)?;
    let param_count = read_u8(r)?;
    let mut params = Vec::with_capacity(param_count as usize);
    for _ in 0..param_count {
        params.push(read_cstring(r)?);
    }
    let guid = matches!(event, guild_event::SIGNED_ON | guild_event::SIGNED_OFF)
        .then(|| read_u64_le(r))
        .transpose()?;
    Ok(GuildEventNotice {
        event,
        params,
        guid,
    })
}

/// `SMSG_GUILD_COMMAND_RESULT` — the server's verdict on a guild verb we sent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuildCommandResult {
    /// A [`guild_command`] tag. Load-bearing rather than decorative: it is what disambiguates the
    /// two meanings of result `0x08` (see [`guild_command_error::LEADER_LEAVE`]).
    pub command: u32,
    /// The `%s` the matching error string wants — a player name, a guild name, or empty.
    pub name: String,
    /// A [`guild_command_error`] code.
    pub result: u32,
}

/// Read `SMSG_GUILD_COMMAND_RESULT` (VERIFIED vmangos `Server/Packets/Guild.cpp:96-101`
/// `GuildCommandResult::AppendBodyTo`): `u32 command`, cstring, `u32 result` — the string is in the
/// **middle**, which is the field order to get right.
pub(super) fn read_guild_command_result(r: &mut impl Read) -> io::Result<GuildCommandResult> {
    Ok(GuildCommandResult {
        command: read_u32_le(r)?,
        name: read_cstring(r)?,
        result: read_u32_le(r)?,
    })
}

/// `SMSG_GUILD_INFO` — the small "founded on / N members / N accounts" summary, answering
/// `CMSG_GUILD_INFO`. Nothing here overlaps [`GuildRoster`]; it is a separate ask.
///
/// The layout — a cstring then five `u32`s — is VERIFIED at both ends. The **field names below are
/// INFERRED**: they are vmangos's, and the binary does not label them. What the binary does supply
/// is a corroboration of the first three: the `GUILD_INFO_TEMPLATE` pushes at `0x5e704d`–`0x5e7061`
/// reorder them **wire #2, wire #1, wire #3-5**, which is exactly the swap an enUS `M/D/Y` date
/// template applies to a `day, month, year` wire (wow-re `system/ui/scratch/guild-roster-wire.md`).
/// Good evidence, not a labelled proof — treat a surprising founding date as a lead, not an
/// impossibility.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuildInfo {
    /// The guild's name.
    pub name: String,
    /// Founding day-of-month (INFERRED — see the struct doc).
    pub created_day: u32,
    /// Founding month (INFERRED).
    pub created_month: u32,
    /// Founding year (INFERRED).
    pub created_year: u32,
    /// How many characters are in the guild.
    pub member_count: u32,
    /// How many distinct *accounts* those characters belong to — the number the guild pane shows
    /// alongside the member count.
    pub account_count: u32,
}

/// Read `SMSG_GUILD_INFO` (VERIFIED vmangos `Server/Packets/Guild.cpp:103-111`
/// `GuildInfo::AppendBodyTo`): the guild name, then day/month/year and the two counts, all `u32`.
pub(super) fn read_guild_info(r: &mut impl Read) -> io::Result<GuildInfo> {
    Ok(GuildInfo {
        name: read_cstring(r)?,
        created_day: read_u32_le(r)?,
        created_month: read_u32_le(r)?,
        created_year: read_u32_le(r)?,
        member_count: read_u32_le(r)?,
        account_count: read_u32_le(r)?,
    })
}

/// Read `SMSG_GUILD_INVITE` (VERIFIED vmangos `Server/Packets/Guild.cpp:85-89`
/// `GuildInviteNotification::AppendBodyTo`): two cstrings — who invited us, and into which guild.
/// Answered by `CMSG_GUILD_ACCEPT` or `CMSG_GUILD_DECLINE`, neither of which echoes anything, so
/// the pending invite is client-side state.
pub(super) fn read_guild_invite(r: &mut impl Read) -> io::Result<(String, String)> {
    let inviter = read_cstring(r)?;
    let guild = read_cstring(r)?;
    Ok((inviter, guild))
}

/// Read `SMSG_GUILD_DECLINE` (VERIFIED vmangos `Server/Packets/Guild.cpp:91-94`
/// `GuildDeclineNotification::AppendBodyTo`): one cstring — the name of the player who turned our
/// invitation down. Delivered only to the inviter (`Handlers/GuildHandler.cpp:230-236`).
pub(super) fn read_guild_decline(r: &mut impl Read) -> io::Result<String> {
    read_cstring(r)
}

/// Body of `CMSG_GUILD_QUERY` (VERIFIED vmangos `Server/Packets/Guild.cpp:8-11`
/// `GuildQuery::ReadFromWorldPacket`): one `u32`, the guild id. Answered by
/// `SMSG_GUILD_QUERY_RESPONSE` — or, if no such guild exists, by an
/// `SMSG_GUILD_COMMAND_RESULT`(`CREATE`, `PLAYER_NOT_IN_GUILD`), which is vmangos's catch-all and
/// not literally about us (`Handlers/GuildHandler.cpp:44`).
pub fn guild_query(guild_id: u32) -> Vec<u8> {
    guild_id.to_le_bytes().to_vec()
}

/// Body of `CMSG_GUILD_CREATE` (VERIFIED vmangos `Server/Packets/Guild.cpp:3-6`
/// `GuildCreate::ReadFromWorldPacket`): one cstring, the desired name.
///
/// vmangos registers this opcode `STATUS_NEVER` (`Server/Protocol/Opcodes.cpp:210`) — on a 1.12
/// realm a guild is founded through the charter/petition flow, never by this packet. Built for
/// completeness of the family; do not expect a reply.
pub fn guild_create(name: &str) -> Vec<u8> {
    cstring_body(name)
}

/// Body of `CMSG_GUILD_INVITE` (VERIFIED vmangos `Server/Packets/Guild.cpp:13-16`
/// `GuildInvite::ReadFromWorldPacket`): one cstring, the name to invite. The server normalises the
/// case itself (`normalizePlayerName`), so the client need not.
pub fn guild_invite(name: &str) -> Vec<u8> {
    cstring_body(name)
}

/// Body of `CMSG_GUILD_ACCEPT` (VERIFIED vmangos `Server/Protocol/Opcodes.cpp:213` —
/// `NullClientPacket`): empty. Which invitation it accepts is the server's own pending state
/// (`GetGuildIdInvited`), not a field.
pub fn guild_accept() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_GUILD_DECLINE` (VERIFIED vmangos `Server/Protocol/Opcodes.cpp:214` —
/// `NullClientPacket`): empty. Sends the inviter an `SMSG_GUILD_DECLINE` naming us.
pub fn guild_decline() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_GUILD_INFO` (VERIFIED vmangos `Server/Protocol/Opcodes.cpp:216` —
/// `NullClientPacket`): empty. Answered by `SMSG_GUILD_INFO` for our own guild.
pub fn guild_info() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_GUILD_ROSTER` (VERIFIED vmangos `Server/Protocol/Opcodes.cpp:218` —
/// `NullClientPacket`): empty. Answered by the whole `SMSG_GUILD_ROSTER`; the server also pushes
/// that packet unasked after any change, so this is a refresh.
pub fn guild_roster() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_GUILD_PROMOTE` (VERIFIED vmangos `Server/Packets/Guild.cpp:23-26`
/// `GuildPromote::ReadFromWorldPacket`): one cstring, who to promote. Promotion is by name and
/// moves them **up** one rank — the server does `rank--` (`Guild/Guild.h:52`).
pub fn guild_promote(name: &str) -> Vec<u8> {
    cstring_body(name)
}

/// Body of `CMSG_GUILD_DEMOTE` (VERIFIED vmangos `Server/Packets/Guild.cpp:28-31`
/// `GuildDemote::ReadFromWorldPacket`): one cstring; the server does `rank++`.
pub fn guild_demote(name: &str) -> Vec<u8> {
    cstring_body(name)
}

/// Body of `CMSG_GUILD_LEAVE` (VERIFIED vmangos `Server/Protocol/Opcodes.cpp:222` —
/// `NullClientPacket`): empty. Refused with
/// `(QUIT, LEADER_LEAVE)` if we are the guild master and anyone else is still in the guild.
pub fn guild_leave() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_GUILD_REMOVE` (VERIFIED vmangos `Server/Packets/Guild.cpp:18-21`
/// `GuildRemove::ReadFromWorldPacket`): one cstring, who to kick.
pub fn guild_remove(name: &str) -> Vec<u8> {
    cstring_body(name)
}

/// Body of `CMSG_GUILD_DISBAND` (VERIFIED vmangos `Server/Protocol/Opcodes.cpp:224` —
/// `NullClientPacket`): empty. Guild master only.
pub fn guild_disband() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_GUILD_LEADER` (VERIFIED vmangos `Server/Packets/Guild.cpp:33-36`
/// `GuildLeader::ReadFromWorldPacket`): one cstring — hand the guild over to this member.
pub fn guild_leader(name: &str) -> Vec<u8> {
    cstring_body(name)
}

/// Body of `CMSG_GUILD_MOTD` (VERIFIED vmangos `Server/Packets/Guild.cpp:38-42`
/// `GuildMOTD::ReadFromWorldPacket`): one cstring, the new message of the day.
///
/// **Clearing the MOTD sends `""` as a one-byte body (the NUL), not a zero-byte body.** vmangos's
/// read is guarded — `if (!recv_data.empty()) recv_data >> motd;` — so a *literally empty* body is
/// also accepted and also means "clear it" (its `motd` stays default-constructed). Both shapes
/// therefore work, and the guard exists to stop an empty packet throwing a `ByteBufferException`
/// rather than to define a second wire form. We emit the well-formed cstring in every case: one
/// builder, one shape, and the empty string is not a special case anywhere in this crate.
pub fn guild_motd(motd: &str) -> Vec<u8> {
    cstring_body(motd)
}

/// Body of `CMSG_GUILD_RANK` (VERIFIED vmangos `Server/Packets/Guild.cpp:78-83`
/// `GuildRank::ReadFromWorldPacket`): `u32 rankId`, `u32 rights`, then the rank name.
///
/// This one packet both renames a rank and rewrites its rights, so a caller changing only one must
/// still send the current value of the other. Two server-side facts shape the call: sending it for
/// [`guild_default_rank::GUILDMASTER`] has its `rights` **ignored and replaced** with
/// [`guild_rank_right::ALL`], and a `name` longer than [`GUILD_RANK_MAX_LENGTH`] characters gets
/// the session **kicked** by the anticheat, not refused.
pub fn guild_rank(rank_id: u32, rights: u32, name: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(9 + name.len());
    body.extend_from_slice(&rank_id.to_le_bytes());
    body.extend_from_slice(&rights.to_le_bytes());
    push_cstring(&mut body, name);
    body
}

/// Body of `CMSG_GUILD_ADD_RANK` (VERIFIED vmangos `Server/Packets/Guild.cpp:73-76`
/// `GuildAddRank::ReadFromWorldPacket`): one cstring, the new rank's name. The rank is appended at
/// the bottom with `GCHAT_LISTEN | GCHAT_SPEAK` (`Handlers/GuildHandler.cpp:623`); the server
/// silently ignores the packet once the guild already has [`GUILD_RANKS_MAX_COUNT`] ranks.
pub fn guild_add_rank(name: &str) -> Vec<u8> {
    cstring_body(name)
}

/// Body of `CMSG_GUILD_DEL_RANK` (VERIFIED vmangos `Server/Protocol/Opcodes.cpp:655` —
/// `NullClientPacket`): empty. There is no rank id: it always deletes the **lowest** rank, which is
/// why the reference UI only ever offers to remove the last row.
pub fn guild_del_rank() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_GUILD_SET_PUBLIC_NOTE` (VERIFIED vmangos `Server/Packets/Guild.cpp:61-65`
/// `GuildSetPublicNote::ReadFromWorldPacket`): the member's name, then the note.
pub fn guild_set_public_note(name: &str, note: &str) -> Vec<u8> {
    two_cstring_body(name, note)
}

/// Body of `CMSG_GUILD_SET_OFFICER_NOTE` (VERIFIED vmangos `Server/Packets/Guild.cpp:67-71`
/// `GuildSetOfficerNote::ReadFromWorldPacket`): the same two-cstring shape as
/// [`guild_set_public_note`], gated on [`guild_rank_right::EDIT_OFFICER_NOTE`] instead.
pub fn guild_set_officer_note(name: &str, note: &str) -> Vec<u8> {
    two_cstring_body(name, note)
}

/// Body of `CMSG_GUILD_INFO_TEXT` (VERIFIED vmangos `Server/Packets/Guild.cpp:44-49`
/// `GuildChangeInfoText::ReadFromWorldPacket`): one cstring, the guild information text. 1.12-only
/// — vmangos compiles both the opcode's handler and the roster's matching `guildInfo` field under
/// `SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_8_4`.
pub fn guild_info_text(text: &str) -> Vec<u8> {
    cstring_body(text)
}

/// A body that is exactly one NUL-terminated string.
fn cstring_body(s: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(s.len() + 1);
    push_cstring(&mut body, s);
    body
}

/// A body that is exactly two NUL-terminated strings.
fn two_cstring_body(a: &str, b: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(a.len() + b.len() + 2);
    push_cstring(&mut body, a);
    push_cstring(&mut body, b);
    body
}

/// Append a NUL-terminated string to `body`.
fn push_cstring(body: &mut Vec<u8>, s: &str) {
    body.extend_from_slice(s.as_bytes());
    body.push(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rights table is *not* `1 << (i - 1)`: the FrameXML checkbox order puts Promote/Demote
    /// ahead of Invite/Remove, whose bits are lower. This pins the two orders apart.
    #[test]
    fn rank_right_checkbox_order_is_not_bit_order() {
        assert_eq!(GUILD_RANK_RIGHT_ORDER[4], 0x0000_0080, "idx 5 = Promote");
        assert_eq!(GUILD_RANK_RIGHT_ORDER[5], 0x0000_0100, "idx 6 = Demote");
        assert_eq!(GUILD_RANK_RIGHT_ORDER[6], 0x0000_0010, "idx 7 = Invite");
        assert_eq!(GUILD_RANK_RIGHT_ORDER[7], 0x0000_0020, "idx 8 = Remove");
        assert!(
            GUILD_RANK_RIGHT_ORDER[4] > GUILD_RANK_RIGHT_ORDER[6],
            "the order is deliberately non-monotonic in bit value"
        );
        // Every entry is a single bit, and none of them is the GR_RIGHT_EMPTY sentinel.
        for right in GUILD_RANK_RIGHT_ORDER {
            assert_eq!(right.count_ones(), 1, "{right:#x} is one bit");
            assert_ne!(right, guild_rank_right::EMPTY);
        }
    }

    /// Each right, ORed with the `GR_RIGHT_EMPTY` sentinel, reproduces vmangos's own constant —
    /// the check that the "factor the 0x40 out" reading of `Guild/Guild.h:56-74` is right.
    #[test]
    fn rank_rights_reconstruct_the_vmangos_constants() {
        use guild_rank_right::*;
        assert_eq!(GCHAT_LISTEN | EMPTY, 0x0000_0041);
        assert_eq!(GCHAT_SPEAK | EMPTY, 0x0000_0042);
        assert_eq!(OFFCHAT_LISTEN | EMPTY, 0x0000_0044);
        assert_eq!(OFFCHAT_SPEAK | EMPTY, 0x0000_0048);
        assert_eq!(PROMOTE | EMPTY, 0x0000_00C0);
        assert_eq!(DEMOTE | EMPTY, 0x0000_0140);
        assert_eq!(INVITE | EMPTY, 0x0000_0050);
        assert_eq!(REMOVE | EMPTY, 0x0000_0060);
        assert_eq!(SET_MOTD | EMPTY, 0x0000_1040);
        assert_eq!(EDIT_PUBLIC_NOTE | EMPTY, 0x0000_2040);
        assert_eq!(VIEW_OFFICER_NOTE | EMPTY, 0x0000_4040);
        assert_eq!(EDIT_OFFICER_NOTE | EMPTY, 0x0000_8040);
        assert_eq!(MODIFY_GUILD_INFO | EMPTY, 0x0001_0040);
    }
}
