//! The social family — the friend list, the ignore list, and `/who` (opcodes 0x62–0x6d;
//! decision 0668).
//!
//! Like the duel family, **both ends are pinned at the bytes**. The server side is vmangos
//! `SocialMgr.{h,cpp}` (`PlayerSocial::SendFriendList`/`SendIgnoreList`,
//! `SocialMgr::SendFriendStatus`), `Server/Packets/Social.{h,cpp}` (every `AppendBodyTo`),
//! `Server/Packets/Misc.{h,cpp}` (the five client reads) and `Handlers/MiscHandler.cpp`
//! (`HandleWhoOpcode`'s `WhoListClientQueryTask` + the add/del handlers). The client side is
//! WoW.exe's own `FriendList.cpp` TU, recorded VERIFIED in wow-re `system/net/scratch/w2b.md`
//! (the 0x728-byte `FriendList` object `DAT_00c28168`, its six registered SMSG handlers, and the
//! Lua glue):
//!
//! | opcode | client fn | what it reads / writes |
//! |---|---|---|
//! | `SMSG_FRIEND_LIST` 0x67 | `FriendList::ParseFriendList 0x5ae350` | `u8` count, per slot `{guid u64, online u8, area/level/class u32}` |
//! | `SMSG_FRIEND_STATUS` 0x68 | `0x5add30` → `HandleResult 0x5acab0` | `u8` op + `u64` guid (+ the online tail) |
//! | `SMSG_IGNORE_LIST` 0x6b | the ignore-slot half of the same object | `u8` count, `u64` guids into the 25 slots @+0x650 |
//! | `SMSG_WHO` 0x63 | `0x5adde0` | count-led entry list → FrameScript |
//!
//! Two client-side facts that shape everything above this layer, both from that TU:
//!
//! - **The friend/ignore lists are guids, not names.** A slot holds `{status, note*, guid, area,
//!   level, class}` — no name — and the client resolves the display name through the **ObjectMgr
//!   name cache** (`0x55f080`, the `CMSG_NAME_QUERY` cache) at format time. benilla's
//!   [`NameCache`](../../../benilla/src/names.rs) plays that role, which is why a friend row can
//!   populate a frame late rather than never.
//! - **The client's own capacity is 50 friends / 25 ignores** (the slot arrays at `+0x008` and
//!   `+0x650`), exactly vmangos's `SOCIALMGR_FRIEND_LIMIT` / `SOCIALMGR_IGNORE_LIMIT`. The
//!   FrameXML's `MAX_IGNORE = 50` is dead data — the engine cap is 25.

use std::io::{self, Read};

use crate::wire::{read_cstring, read_u32_le, read_u64_le, read_u8};

/// `SMSG_FRIEND_STATUS`'s result byte — vmangos's `FriendsResult` (`SocialMgr.h:81-110`), which is
/// the client's own enum: `FriendList::HandleResult 0x5acab0` switches on it and displays
/// `0x496720`'s catalog ids `0x104..0x114` (one per result, `ADDED_ONLINE`/`ADDED_OFFLINE`
/// sharing `ERR_FRIEND_ADDED_S`). Every one of those rows routes **kind 0 = the chat frame**, not
/// the red error line — see [`crate::messages`]'s consumer for that mapping.
pub mod friend_result {
    /// `ERR_FRIEND_DB_ERROR` — the server's lookup failed.
    pub const DB_ERROR: u8 = 0x00;
    /// `ERR_FRIEND_LIST_FULL` — 50 friends already.
    pub const LIST_FULL: u8 = 0x01;
    /// `ERR_FRIEND_ONLINE_SS` — a friend just logged in (broadcast, not an ack).
    pub const ONLINE: u8 = 0x02;
    /// `ERR_FRIEND_OFFLINE_S` — a friend just logged out (broadcast).
    pub const OFFLINE: u8 = 0x03;
    /// `ERR_FRIEND_NOT_FOUND` — no character by that name.
    pub const NOT_FOUND: u8 = 0x04;
    /// `ERR_FRIEND_REMOVED_S` — the ack for `CMSG_DEL_FRIEND`.
    pub const REMOVED: u8 = 0x05;
    /// `ERR_FRIEND_ADDED_S`, and the added friend is online — carries the online tail.
    pub const ADDED_ONLINE: u8 = 0x06;
    /// `ERR_FRIEND_ADDED_S`, added while offline — no tail.
    pub const ADDED_OFFLINE: u8 = 0x07;
    /// `ERR_FRIEND_ALREADY_S`.
    pub const ALREADY: u8 = 0x08;
    /// `ERR_FRIEND_SELF`.
    pub const SELF: u8 = 0x09;
    /// `ERR_FRIEND_WRONG_FACTION` — cross-faction friending, refused by config.
    pub const ENEMY: u8 = 0x0A;
    /// `ERR_IGNORE_FULL` — 25 ignores already.
    pub const IGNORE_FULL: u8 = 0x0B;
    /// `ERR_IGNORE_SELF`.
    pub const IGNORE_SELF: u8 = 0x0C;
    /// `ERR_IGNORE_NOT_FOUND`.
    pub const IGNORE_NOT_FOUND: u8 = 0x0D;
    /// `ERR_IGNORE_ALREADY_S`.
    pub const IGNORE_ALREADY: u8 = 0x0E;
    /// `ERR_IGNORE_ADDED_S` — the ack for `CMSG_ADD_IGNORE`.
    pub const IGNORE_ADDED: u8 = 0x0F;
    /// `ERR_IGNORE_REMOVED_S` — the ack for `CMSG_DEL_IGNORE`.
    pub const IGNORE_REMOVED: u8 = 0x10;
    /// `ERR_IGNORE_AMBIGUOUS`.
    pub const IGNORE_AMBIGUOUS: u8 = 0x11;
    /// `ERR_FRIEND_ERROR` — "Unknown friend response from server."; vmangos's `FRIEND_UNKNOWN`.
    pub const UNKNOWN: u8 = 0x1A;
}

/// A friend slot's presence byte — vmangos's `FriendStatus` (`SocialMgr.h:36-43`). `0` is the
/// only value the wire treats specially (it suppresses the area/level/class tail); the rest are
/// the AFK/DND flavours the friends list renders as its `<AFK>` / `<DND>` tag.
pub mod friend_status {
    /// Offline — the entry carries no area/level/class.
    pub const OFFLINE: u8 = 0;
    /// Online, no away flag.
    pub const ONLINE: u8 = 1;
    /// Online and flagged AFK.
    pub const AFK: u8 = 2;
    /// Online and flagged DND (`3` is unused by 1.12 vmangos).
    pub const DND: u8 = 4;
}

/// One row of the friend list — a guid plus, when the friend is online, where they are.
///
/// The name is deliberately absent: it is not on the wire (see the module doc's first client-side
/// fact) and is resolved from the name cache by the consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FriendEntry {
    /// The friend's player guid. vmangos rebuilds it as `ObjectGuid(HIGHGUID_PLAYER, lowguid)`,
    /// so it is always a player guid.
    pub guid: u64,
    /// [`friend_status`] — `OFFLINE` means the three fields below were **not** sent and read back
    /// as zero.
    pub status: u8,
    /// The friend's zone id (`AreaTable.dbc`), `0` when offline.
    pub area: u32,
    /// The friend's level, `0` when offline.
    pub level: u32,
    /// The friend's class id, `0` when offline.
    pub class: u32,
}

impl FriendEntry {
    /// Is this friend online at all (any of ONLINE/AFK/DND)? The wire's own test — vmangos writes
    /// the area/level/class tail under exactly `if (entry.status)`, and the client's parse reads
    /// it under the same condition.
    pub fn is_online(&self) -> bool {
        self.status != friend_status::OFFLINE
    }
}

/// Read `SMSG_FRIEND_LIST` (VERIFIED vmangos `Social.cpp`
/// `WorldPackets::Social::FriendList::AppendBodyTo`, and WoW.exe
/// `FriendList::ParseFriendList 0x5ae350`): a `u8` count, then per friend a `u64` guid and a `u8`
/// status — and the `u32` area/level/class triple **only when the status is non-zero**.
///
/// The conditional tail is why this cannot be a fixed-stride read: an offline friend is 9 bytes,
/// an online one 21.
pub fn read_friend_list(r: &mut impl Read) -> io::Result<Vec<FriendEntry>> {
    let count = read_u8(r)?;
    let mut friends = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let mut entry = FriendEntry {
            guid: read_u64_le(r)?,
            status: read_u8(r)?,
            ..Default::default()
        };
        if entry.is_online() {
            entry.area = read_u32_le(r)?;
            entry.level = read_u32_le(r)?;
            entry.class = read_u32_le(r)?;
        }
        friends.push(entry);
    }
    Ok(friends)
}

/// Read `SMSG_IGNORE_LIST` (VERIFIED vmangos `Social.cpp`
/// `WorldPackets::Social::IgnoreList::AppendBodyTo`): a `u8` count then that many full guids, and
/// nothing else — an ignore slot in the client is 8 bytes wide for the same reason (the `+0x650`
/// array).
pub fn read_ignore_list(r: &mut impl Read) -> io::Result<Vec<u64>> {
    let count = read_u8(r)?;
    (0..count).map(|_| read_u64_le(r)).collect()
}

/// `SMSG_FRIEND_STATUS` — one result code about one player, and (for the two "is online" results)
/// where they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FriendStatusUpdate {
    /// A [`friend_result`] code. It is both an ack (we asked to add/remove) and a broadcast (a
    /// friend logged in or out) — the code, not the opcode, says which.
    pub result: u8,
    /// The player the result is about. Note vmangos sends the *asked-for* guid even for
    /// `NOT_FOUND`, where it is `0`.
    pub guid: u64,
    /// The online tail, present iff the result is [`friend_result::ONLINE`] or
    /// [`friend_result::ADDED_ONLINE`] (VERIFIED `SocialMgr::SendFriendStatus`'s switch).
    pub online: Option<FriendOnline>,
}

/// The presence tail `SMSG_FRIEND_STATUS` carries for an online friend — the same four fields a
/// [`FriendEntry`] holds, so a status update can refresh a list row in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FriendOnline {
    /// [`friend_status`] — 1.12 sends this byte (the `SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_8_4`
    /// arm; the 1.8 wire had no status byte here).
    pub status: u8,
    /// Zone id.
    pub area: u32,
    /// Level.
    pub level: u32,
    /// Class id.
    pub class: u32,
}

/// Read `SMSG_FRIEND_STATUS` (VERIFIED vmangos `Social.cpp`
/// `WorldPackets::Social::FriendStatus::AppendBodyTo` + `SocialMgr::SendFriendStatus`; client
/// handler `0x5add30`, which reads `u8` op + `u64` guid before handing off to
/// `HandleResult 0x5acab0`).
///
/// The tail is read on the *result code*, not on a length check: only `FRIEND_ADDED_ONLINE` and
/// `FRIEND_ONLINE` carry it. Reading it on any other code would desynchronise the parse, which is
/// exactly why the condition is spelled out rather than "read if bytes remain".
pub fn read_friend_status(r: &mut impl Read) -> io::Result<FriendStatusUpdate> {
    let result = read_u8(r)?;
    let guid = read_u64_le(r)?;
    let online = matches!(result, friend_result::ONLINE | friend_result::ADDED_ONLINE)
        .then(|| -> io::Result<FriendOnline> {
            Ok(FriendOnline {
                status: read_u8(r)?,
                area: read_u32_le(r)?,
                level: read_u32_le(r)?,
                class: read_u32_le(r)?,
            })
        })
        .transpose()?;
    Ok(FriendStatusUpdate {
        result,
        guid,
        online,
    })
}

/// One `/who` hit. Unlike the friend list, every field is on the wire — a who result is a
/// snapshot the server composed, not a reference into any client cache.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WhoEntry {
    /// Character name.
    pub name: String,
    /// Guild name, empty when unguilded (vmangos writes `GetGuildNameById(0)` = `""`).
    pub guild: String,
    /// Level.
    pub level: u32,
    /// Class id.
    pub class: u32,
    /// Race id.
    pub race: u32,
    /// Zone id (`AreaTable.dbc`) — the server sends the id, the client names it.
    pub zone: u32,
}

/// `SMSG_WHO` — the results, plus the two counts the "N players total (M displayed)" line needs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WhoResults {
    /// How many entries follow (`clientCount`, ≤ 49 — vmangos breaks the walk at 49 despite the
    /// "50 is maximum" comment).
    pub displayed: u32,
    /// How many matched in total: the full match count when it exceeds 49, otherwise the same as
    /// [`Self::displayed`] (VERIFIED `data.put(4, count > 49 ? count : clientCount)`).
    pub total: u32,
    /// The entries themselves, server order.
    pub entries: Vec<WhoEntry>,
}

/// Read `SMSG_WHO` (VERIFIED vmangos `MiscHandler.cpp`'s `WhoListClientQueryTask::operator()`):
/// two `u32` counts, then `displayed` entries of `{name, guild, level, class, race, zone}`.
///
/// 1.12 does **not** carry the trailing party-status `u32` — vmangos writes it only under
/// `SUPPORTED_CLIENT_BUILD <= CLIENT_BUILD_1_8_4`, and its own comment calls it "not actually
/// displayed anywhere".
pub fn read_who(r: &mut impl Read) -> io::Result<WhoResults> {
    let displayed = read_u32_le(r)?;
    let total = read_u32_le(r)?;
    let mut entries = Vec::with_capacity(displayed.min(64) as usize);
    for _ in 0..displayed {
        entries.push(WhoEntry {
            name: read_cstring(r)?,
            guild: read_cstring(r)?,
            level: read_u32_le(r)?,
            class: read_u32_le(r)?,
            race: read_u32_le(r)?,
            zone: read_u32_le(r)?,
        });
    }
    Ok(WhoResults {
        displayed,
        total,
        entries,
    })
}

/// A `/who` query, already parsed out of the filter string the player typed.
///
/// The *parsing* is engine work in the real client too (the FrameXML hands `SendWho` a raw string
/// like `z-"Elwynn Forest" 1-10`), and it needs `AreaTable.dbc` to turn a zone name into the id
/// the wire wants — so benilla parses it app-side, where the DBCs live, and this struct is the
/// result. [`Default`] is the query the client sends for a bare `/who` with no terms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhoRequest {
    /// Inclusive minimum level.
    pub level_min: u32,
    /// Inclusive maximum level. The real client's un-set value is **100** (vmangos's own comment:
    /// "client send in case not set max level value 100"), not 255 — so that is the default here.
    pub level_max: u32,
    /// `n-` — a name substring, empty for "any".
    pub player_name: String,
    /// `g-` — a guild-name substring, empty for "any".
    pub guild_name: String,
    /// `r-` — a bitmask over race ids (`1 << race`), all-ones for "any".
    pub race_mask: u32,
    /// `c-` — a bitmask over class ids (`1 << class`), all-ones for "any".
    pub class_mask: u32,
    /// `z-` — zone ids, resolved from names client-side. The server refuses more than 10.
    pub zones: Vec<u32>,
    /// Untagged words: the server matches each against name, guild **and** zone name (any hit
    /// keeps the row). Refused past 4.
    pub search_terms: Vec<String>,
}

impl Default for WhoRequest {
    fn default() -> Self {
        Self {
            level_min: 0,
            level_max: 100,
            player_name: String::new(),
            guild_name: String::new(),
            race_mask: u32::MAX,
            class_mask: u32::MAX,
            zones: Vec::new(),
            search_terms: Vec::new(),
        }
    }
}

/// The server's own limits on a `/who` (`HandleWhoOpcode`: `> 10` zones or `> 4` terms and the
/// whole packet is dropped, silently). Trimming to them client-side turns a dropped query into a
/// narrower one.
pub const WHO_MAX_ZONES: usize = 10;
/// See [`WHO_MAX_ZONES`].
pub const WHO_MAX_SEARCH_TERMS: usize = 4;

/// Body of `CMSG_WHO` (VERIFIED vmangos `Misc.cpp` `WorldPackets::Misc::Who::ReadFromWorldPacket`):
/// `levelMin`, `levelMax`, name cstring, guild cstring, `raceMask`, `classMask`, then a counted
/// `u32` zone array and a counted cstring array — in that order.
///
/// Both arrays are trimmed to the server's caps ([`WHO_MAX_ZONES`], [`WHO_MAX_SEARCH_TERMS`]):
/// over them the handler `return`s before doing anything, so an untrimmed query would look
/// exactly like a `/who` that matched nobody.
pub fn who(request: &WhoRequest) -> Vec<u8> {
    let zones = &request.zones[..request.zones.len().min(WHO_MAX_ZONES)];
    let terms = &request.search_terms[..request.search_terms.len().min(WHO_MAX_SEARCH_TERMS)];

    let mut body = Vec::with_capacity(32 + request.player_name.len() + request.guild_name.len());
    body.extend_from_slice(&request.level_min.to_le_bytes());
    body.extend_from_slice(&request.level_max.to_le_bytes());
    push_cstring(&mut body, &request.player_name);
    push_cstring(&mut body, &request.guild_name);
    body.extend_from_slice(&request.race_mask.to_le_bytes());
    body.extend_from_slice(&request.class_mask.to_le_bytes());
    body.extend_from_slice(&(zones.len() as u32).to_le_bytes());
    for zone in zones {
        body.extend_from_slice(&zone.to_le_bytes());
    }
    body.extend_from_slice(&(terms.len() as u32).to_le_bytes());
    for term in terms {
        push_cstring(&mut body, term);
    }
    body
}

/// Body of `CMSG_FRIEND_LIST` (VERIFIED vmangos `Opcodes.cpp` — `NullClientPacket`): empty. The
/// list also arrives unasked at login (`CharacterHandler.cpp:527`), so this is a refresh, which is
/// exactly what the FrameXML's `ShowFriends()` is for.
pub fn friend_list() -> Vec<u8> {
    Vec::new()
}

/// Body of `CMSG_ADD_FRIEND` (VERIFIED vmangos `Misc.cpp` `AddFriend::ReadFromWorldPacket`): one
/// cstring, the name to befriend. The server normalises the case itself
/// (`normalizePlayerName`), so the client need not.
pub fn add_friend(name: &str) -> Vec<u8> {
    cstring_body(name)
}

/// Body of `CMSG_DEL_FRIEND` (VERIFIED vmangos `Misc.cpp` `DelFriend::ReadFromWorldPacket`): one
/// full guid. Removal is by **guid**, not name — which is why the client keeps the list keyed by
/// guid and resolves names for display only.
pub fn del_friend(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_ADD_IGNORE` (VERIFIED vmangos `Misc.cpp` `AddIgnore::ReadFromWorldPacket`): one
/// cstring, the name to ignore.
pub fn add_ignore(name: &str) -> Vec<u8> {
    cstring_body(name)
}

/// Body of `CMSG_DEL_IGNORE` (VERIFIED vmangos `Misc.cpp` `DelIgnore::ReadFromWorldPacket`): one
/// full guid, the [`del_friend`] shape.
pub fn del_ignore(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// A body that is exactly one NUL-terminated string.
fn cstring_body(s: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(s.len() + 1);
    push_cstring(&mut body, s);
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

    /// `SMSG_FRIEND_LIST`: the area/level/class tail rides on the status byte, so an offline and
    /// an online entry have different widths in the same packet — the parse has to follow it or
    /// every later entry is garbage.
    #[test]
    fn friend_list_tail_is_conditional_on_the_status_byte() {
        let mut body = vec![2u8]; // count
        body.extend_from_slice(&7u64.to_le_bytes()); // offline friend
        body.push(friend_status::OFFLINE);
        body.extend_from_slice(&9u64.to_le_bytes()); // online friend
        body.push(friend_status::AFK);
        body.extend_from_slice(&12u32.to_le_bytes()); // area
        body.extend_from_slice(&60u32.to_le_bytes()); // level
        body.extend_from_slice(&11u32.to_le_bytes()); // class

        let friends = read_friend_list(&mut &body[..]).unwrap();
        assert_eq!(
            friends,
            vec![
                FriendEntry {
                    guid: 7,
                    status: friend_status::OFFLINE,
                    ..Default::default()
                },
                FriendEntry {
                    guid: 9,
                    status: friend_status::AFK,
                    area: 12,
                    level: 60,
                    class: 11,
                },
            ]
        );
        assert!(!friends[0].is_online() && friends[1].is_online());
    }

    /// `SMSG_IGNORE_LIST` is a `u8` count and bare guids.
    #[test]
    fn ignore_list_is_a_counted_guid_array() {
        let mut body = vec![2u8];
        body.extend_from_slice(&0x1122u64.to_le_bytes());
        body.extend_from_slice(&0x3344u64.to_le_bytes());
        assert_eq!(
            read_ignore_list(&mut &body[..]).unwrap(),
            vec![0x1122, 0x3344]
        );
    }

    /// `SMSG_FRIEND_STATUS`: only ONLINE and ADDED_ONLINE carry the presence tail. A REMOVED with
    /// trailing bytes must still read as tail-less — the code decides, not the length.
    #[test]
    fn friend_status_tail_is_keyed_on_the_result_code() {
        let mut online = vec![friend_result::ONLINE];
        online.extend_from_slice(&9u64.to_le_bytes());
        online.push(friend_status::DND);
        online.extend_from_slice(&1519u32.to_le_bytes());
        online.extend_from_slice(&60u32.to_le_bytes());
        online.extend_from_slice(&8u32.to_le_bytes());
        assert_eq!(
            read_friend_status(&mut &online[..]).unwrap(),
            FriendStatusUpdate {
                result: friend_result::ONLINE,
                guid: 9,
                online: Some(FriendOnline {
                    status: friend_status::DND,
                    area: 1519,
                    level: 60,
                    class: 8,
                }),
            }
        );

        let mut removed = vec![friend_result::REMOVED];
        removed.extend_from_slice(&9u64.to_le_bytes());
        removed.extend_from_slice(&[0xFF; 13]); // must be ignored, not parsed
        let status = read_friend_status(&mut &removed[..]).unwrap();
        assert_eq!(status.result, friend_result::REMOVED);
        assert_eq!(status.online, None);
    }

    /// `SMSG_WHO`: two counts, then name/guild cstrings and four `u32`s per entry. `total` can
    /// exceed `displayed` — that difference is what the "(N displayed)" suffix reports.
    #[test]
    fn who_results_carry_both_counts() {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // displayed
        body.extend_from_slice(&57u32.to_le_bytes()); // total
        body.extend_from_slice(b"Tigole\0");
        body.extend_from_slice(b"Legacy of Steel\0");
        body.extend_from_slice(&40u32.to_le_bytes()); // level
        body.extend_from_slice(&4u32.to_le_bytes()); // class
        body.extend_from_slice(&1u32.to_le_bytes()); // race
        body.extend_from_slice(&40u32.to_le_bytes()); // zone

        assert_eq!(
            read_who(&mut &body[..]).unwrap(),
            WhoResults {
                displayed: 1,
                total: 57,
                entries: vec![WhoEntry {
                    name: "Tigole".into(),
                    guild: "Legacy of Steel".into(),
                    level: 40,
                    class: 4,
                    race: 1,
                    zone: 40,
                }],
            }
        );
    }

    /// An unguilded who row still carries its (empty) guild cstring — dropping it would eat the
    /// next field.
    #[test]
    fn who_reads_the_empty_guild_string() {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(b"Solo\0");
        body.extend_from_slice(b"\0");
        body.extend_from_slice(&5u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&3u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        let who = read_who(&mut &body[..]).unwrap();
        assert_eq!(who.entries[0].guild, "");
        assert_eq!(who.entries[0].level, 5);
    }

    /// `CMSG_WHO`'s field order, byte for byte — the golden for the one body in this family the
    /// server actually parses field-by-field.
    #[test]
    fn who_request_body_is_byte_exact() {
        let request = WhoRequest {
            level_min: 1,
            level_max: 10,
            player_name: "bob".into(),
            guild_name: String::new(),
            race_mask: 0xFFFF_FFFF,
            class_mask: 0x0000_0002,
            zones: vec![12],
            search_terms: vec!["elw".into()],
        };
        let mut want = Vec::new();
        want.extend_from_slice(&1u32.to_le_bytes());
        want.extend_from_slice(&10u32.to_le_bytes());
        want.extend_from_slice(b"bob\0");
        want.extend_from_slice(b"\0");
        want.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        want.extend_from_slice(&2u32.to_le_bytes());
        want.extend_from_slice(&1u32.to_le_bytes());
        want.extend_from_slice(&12u32.to_le_bytes());
        want.extend_from_slice(&1u32.to_le_bytes());
        want.extend_from_slice(b"elw\0");
        assert_eq!(who(&request), want);
    }

    /// The default query is the bare `/who`: every level up to the client's own 100, every race
    /// and class, no terms.
    #[test]
    fn the_default_who_query_is_the_clients_unset_shape() {
        let body = who(&WhoRequest::default());
        assert_eq!(&body[0..4], &0u32.to_le_bytes(), "levelMin");
        assert_eq!(&body[4..8], &100u32.to_le_bytes(), "levelMax — not 255");
        assert_eq!(&body[8..10], b"\0\0", "both name filters empty");
        assert_eq!(&body[10..14], &u32::MAX.to_le_bytes(), "every race");
        assert_eq!(&body[14..18], &u32::MAX.to_le_bytes(), "every class");
        assert_eq!(&body[18..22], &0u32.to_le_bytes(), "no zones");
        assert_eq!(&body[22..26], &0u32.to_le_bytes(), "no terms");
        assert_eq!(body.len(), 26);
    }

    /// Both arrays are trimmed to the server's caps: past them `HandleWhoOpcode` drops the whole
    /// packet, so an over-long query would silently return nothing at all.
    #[test]
    fn who_trims_to_the_servers_caps() {
        let request = WhoRequest {
            zones: (0..15).collect(),
            search_terms: (0..7).map(|i| i.to_string()).collect(),
            ..Default::default()
        };
        let body = who(&request);
        let zone_count = u32::from_le_bytes(body[18..22].try_into().unwrap());
        assert_eq!(zone_count as usize, WHO_MAX_ZONES);
        let after_zones = 22 + WHO_MAX_ZONES * 4;
        let term_count = u32::from_le_bytes(body[after_zones..after_zones + 4].try_into().unwrap());
        assert_eq!(term_count as usize, WHO_MAX_SEARCH_TERMS);
    }

    /// The four small client bodies: two cstrings, two bare guids.
    #[test]
    fn the_add_and_del_bodies() {
        assert_eq!(add_friend("Bob"), b"Bob\0");
        assert_eq!(add_ignore("Bob"), b"Bob\0");
        assert_eq!(del_friend(0xDEAD_BEEF), 0xDEAD_BEEFu64.to_le_bytes());
        assert_eq!(del_ignore(0xDEAD_BEEF), 0xDEAD_BEEFu64.to_le_bytes());
        assert!(friend_list().is_empty());
    }
}
