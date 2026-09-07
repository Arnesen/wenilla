//! The chat SEND types and the joined-channel roster — what the app keeps beside the reference's
//! own `ChatEdit_*` machine (ChatFrame.lua l.1782-2242), which owns the edit box since the chat
//! window became the reference's (decision 1948): the sticky type, the live parse, the header,
//! the tell ring, the Tab cycle and the R/`/` bindings are all its Lua now.
//!
//! [`SendType`] names the wire kind an addon's `SendChatMessage` token maps to
//! ([`super::input::drain_addon_chat_sends`]); [`ChannelState`] is the client-side mirror of the
//! joined channels (the `/N` numbering the reference keeps C-side).

use bevy::prelude::*;

use crate::net::ChatKind;

/// The sendable chat types — `ChatTypeInfo`'s sendable keys, as the wire kind an addon's
/// `SendChatMessage` token maps to. `Whisper`/`Channel` carry their target in the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // RaidLeader/BgLeader have no slash of their own (server-promoted sends);
                    // Channel is the P6 wiring's target — the enum is the full sendable law.
pub(crate) enum SendType {
    Say,
    Yell,
    Emote,
    Whisper,
    Party,
    Raid,
    RaidLeader,
    RaidWarning,
    Guild,
    Officer,
    Battleground,
    BattlegroundLeader,
    Channel,
}

impl SendType {
    /// The chat-type TOKEN an addon passes to `SendChatMessage` (decision 1199) — the reference's
    /// own `ChatTypeInfo` keys, uppercase.
    ///
    /// `None` for a token we do not send. That is the honest answer for `"AFK"`/`"DND"` (which
    /// set a flag rather than sending a line) and for anything an addon simply made up; the
    /// caller reports it rather than guessing SAY, because a raid warning silently going to /say
    /// is worse than one that does not go.
    pub(crate) fn from_token(token: &str) -> Option<SendType> {
        Some(match token {
            "SAY" => SendType::Say,
            "YELL" => SendType::Yell,
            "EMOTE" => SendType::Emote,
            "WHISPER" => SendType::Whisper,
            "PARTY" => SendType::Party,
            "RAID" => SendType::Raid,
            "RAID_LEADER" => SendType::RaidLeader,
            "RAID_WARNING" => SendType::RaidWarning,
            "GUILD" => SendType::Guild,
            "OFFICER" => SendType::Officer,
            "BATTLEGROUND" => SendType::Battleground,
            "BATTLEGROUND_LEADER" => SendType::BattlegroundLeader,
            "CHANNEL" => SendType::Channel,
            _ => return None,
        })
    }

    /// The wire kind this type sends as.
    pub(crate) fn wire(self) -> ChatKind {
        match self {
            SendType::Say => ChatKind::Say,
            SendType::Yell => ChatKind::Yell,
            SendType::Emote => ChatKind::Emote,
            SendType::Whisper => ChatKind::Whisper,
            SendType::Party => ChatKind::Party,
            SendType::Raid => ChatKind::Raid,
            SendType::RaidLeader => ChatKind::RaidLeader,
            SendType::RaidWarning => ChatKind::RaidWarning,
            SendType::Guild => ChatKind::Guild,
            SendType::Officer => ChatKind::Officer,
            SendType::Battleground => ChatKind::Battleground,
            SendType::BattlegroundLeader => ChatKind::BattlegroundLeader,
            SendType::Channel => ChatKind::Channel,
        }
    }
}

/// How many channels the client can hold at once — its allocator refuses the eleventh
/// (`0x49b9c0: cmp ecx,0xa`), and the ten boot-seeded `CHANNEL1`…`CHANNEL10` color rows are the
/// same ten (wow-re `chat-color-table.md`).
pub(crate) const MAX_CHANNELS: usize = 10;

/// The channels this session has joined — the CLIENT-side number law (`GetChannelName(n)`): `/1`
/// is slot 1, `/2` slot 2; the numbered display form ("1. General - Elwynn Forest") and the
/// `[N. Name]` prefixes all derive from it. Fed by YOU_JOINED / YOU_LEFT notices
/// ([`super::feed`]); the zone AUTO-join walk that fills it at login is [`super::channels`].
///
/// **It is a SLOT ARRAY, and leaving punches a hole rather than closing one** (1286). The client's
/// records live in a fixed array at `[0xb4fe04]`, stride `0xa0`, with the entry's own **number**
/// at `+0x00`; the allocator `0x49b980` scans for an entry whose number is `0` and *reuses* it
/// (`0x49b9b0`: `cmp dword [edx],0` / `jz`), only growing when none is free and the count is under
/// **ten** (`0x49b9c0: cmp ecx,0xa`), and the leave path `0x49bbd0` clears that number in place
/// (`0x49bc1b: mov dword [eax+edx],0`) without shrinking the count. Lookup by index then demands
/// the entry's number equal the index asked for (`0x49bf30: cmp esi,ecx / jnz`), so a hole answers
/// "not joined" while every channel above it keeps its number.
///
/// A `Vec<String>` cannot express that: `retain` closed the hole and renumbered everything above
/// it, so walking out of a zone renamed *other* channels — the director saw General and
/// LocalDefense trade numbers on one zone change, and a `/2` typed after that went somewhere else.
#[derive(Resource, Default)]
pub(crate) struct ChannelState {
    /// Slot `i` is channel number `i + 1`; `None` is a freed slot, kept so the numbers above it
    /// do not move. Never longer than [`MAX_CHANNELS`].
    pub joined: Vec<Option<String>>,
    /// `ChatChannels.dbc`, loaded once at Startup ([`super::channels::load_chat_channels`]).
    ///
    /// It lives here because both of its consumers are this type's own business: composing the
    /// auto-join names, and answering a chat event's **arg7** — the built-in ChannelID behind a
    /// name, which is a pure function of the name (the server resolves it the same way) and so
    /// needs no extra bookkeeping at join time. Empty without an install, which degrades to
    /// "no zone channels, arg7 always 0" rather than to an error.
    pub channels: benilla_formats::ChatChannelsCatalog,
}

impl ChannelState {
    /// The 1-based number of `name` (case-insensitive), if joined.
    pub(crate) fn number_of(&self, name: &str) -> Option<u32> {
        self.joined
            .iter()
            .position(|c| c.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(name)))
            .map(|i| i as u32 + 1)
    }

    /// Give `name` a slot: **the first free one**, else a new one while under [`MAX_CHANNELS`] —
    /// the reference's allocator `0x49b980` (see [`ChannelState`]). Already-joined answers its own
    /// number rather than taking a second slot. `None` = all ten are taken.
    ///
    /// The reference also prints a chat error when full (`0x49b9c5: push 0x199` → `0x496720`); we
    /// decline the join and warn instead — one line of feedback we cannot quote without the
    /// error-string table this build indexes by id, and the structural half is what matters.
    pub(crate) fn claim_slot(&mut self, name: &str) -> Option<u32> {
        if let Some(n) = self.number_of(name) {
            return Some(n);
        }
        if let Some(i) = self.joined.iter().position(Option::is_none) {
            self.joined[i] = Some(name.to_string());
            return Some(i as u32 + 1);
        }
        if self.joined.len() >= MAX_CHANNELS {
            return None;
        }
        self.joined.push(Some(name.to_string()));
        Some(self.joined.len() as u32)
    }

    /// Free the slot holding `name` — **cleared in place** (`0x49bbd0`), so every other channel
    /// keeps its number. Answers the number that just went empty.
    pub(crate) fn free_slot(&mut self, name: &str) -> Option<u32> {
        let n = self.number_of(name)?;
        self.joined[n as usize - 1] = None;
        Some(n)
    }

    /// Fill an event's four channel slots (arg4, arg7, arg8, arg9) in place.
    ///
    /// **They are one record, not four fields.** In the reference all four are read off the
    /// client's local channel record — `slot+0x00`, `+0x04`, `+0x94`, `+0x98` — so a name that is
    /// *not* in the local list has no record to read and every one of them is empty: arg4 falls
    /// back to the bare incoming name and arg7/arg8/arg9/arg10 are `0/0/""/0` together. They are
    /// never independently populated. (wow-re `system/ui/scratch/chat-msg-event-args.md` §§4, 7-10,
    /// VERIFIED; the `"%d. %s"` prefix at `0x8445c8` is applied on the hit leg `0x49aa48`, and
    /// `0x49aa86` is the bare-name miss leg.)
    ///
    /// So: on entry `event.channel` holds the name as the wire gave it ("General - Elwynn Forest").
    /// If we are in that channel, on exit arg4 is the numbered display form, arg9 the stored name
    /// **with its " - Zone" tail intact** (§9: the DBC name column *is* the format string the
    /// client built the stored name with), arg8 the 1-based local slot and arg7 the
    /// `ChatChannels.dbc` ChannelID — 0 for a custom channel. If we are not, nothing is stamped.
    ///
    /// arg7 is resolved from the name against `ChatChannels.dbc` rather than remembered per join.
    /// That is safe *because* it only ever runs on the hit leg: the id the client stores in
    /// `slot+0x94` came from the same DBC row at join time, and vmangos resolves the name the same
    /// way (`GetChannelEntryFor`), so no two of the three can disagree.
    pub(crate) fn stamp_channel(&self, event: &mut super::event::ChatEvent) {
        // A miss leaves all four alone — see the "one record" note above.
        let Some(n) = self
            .number_of(&event.channel)
            .filter(|_| !event.channel.is_empty())
        else {
            return;
        };
        event.channel_base = event.channel.clone();
        event.zone_channel_id = self.channels.zone_channel_id(&event.channel_base);
        event.channel_number = n;
        event.channel = format!("{n}. {}", event.channel_base);
    }
}
