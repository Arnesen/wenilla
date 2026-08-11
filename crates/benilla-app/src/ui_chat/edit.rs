//! The `ChatEdit_*` state machine, transcribed (decision 0288 P5; ref ChatFrame.lua l.1782-2242):
//! the sticky type law (SAY/PARTY/RAID/GUILD/BATTLEGROUND commit on send — `ChatTypeInfo`'s
//! sticky column), the **live parse** (typing `/g hi` or `/w Bob hi` converts the box in place,
//! remainder preserved — `ChatEdit_ParseText(send=0)` on every text change), the header
//! (`CHAT_<T>_SEND` text + the type's color on header AND typed text, insets past the header —
//! `ChatEdit_UpdateHeader`), the 10-deep `lastTell` ring with Tab cycling in whisper mode, the
//! invalid-type downgrade on open (PARTY with no party → SAY — `ChatEdit_OnShow`), and the R /
//! `/` bindings (OPENCHAT-family, Bindings.xml).
//!
//! Division of labor: this module owns the STATE + the live-parse/header systems; [`super::input`]
//! owns the submitted-line routing (send-by-current-type + the action-command grammar). The box
//! widget itself (focus, history, insets mechanics) is the engine's.

use std::collections::VecDeque;

use bevy::prelude::*;

use crate::names::NameCache;
use crate::net::{ChatKind, NetCommands, SelfGuid};
use crate::ui_script::{run_or_warn, UiKeyboardCapture};

use super::event::{default_color, ChatEventKind};

/// The box's send type — `ChatTypeInfo`'s sendable keys ([`SendType::sticky`] marks the sticky
/// column's 1-entries). `Whisper`/`Channel` carry their target in [`ChatEditState`].
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
    /// `ChatTypeInfo[type].sticky == 1` (ref ChatFrame.lua l.10-114): SAY, PARTY, RAID, GUILD,
    /// BATTLEGROUND only.
    pub(crate) fn sticky(self) -> bool {
        matches!(
            self,
            SendType::Say
                | SendType::Party
                | SendType::Raid
                | SendType::Guild
                | SendType::Battleground
        )
    }

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

    /// The header's display color = the matching receive kind's table color
    /// (`ChatEdit_UpdateHeader` reads `ChatTypeInfo[type]`).
    fn color(self) -> [u8; 3] {
        use ChatEventKind as K;
        default_color(match self {
            SendType::Say => K::Say,
            SendType::Yell => K::Yell,
            SendType::Emote => K::Emote,
            SendType::Whisper => K::Whisper,
            SendType::Party => K::Party,
            SendType::Raid => K::Raid,
            SendType::RaidLeader => K::RaidLeader,
            SendType::RaidWarning => K::RaidWarning,
            SendType::Guild => K::Guild,
            SendType::Officer => K::Officer,
            SendType::Battleground => K::Battleground,
            SendType::BattlegroundLeader => K::BattlegroundLeader,
            SendType::Channel => K::Channel,
        })
    }

    /// The primary slash alias (`SLASH_<TYPE>1`) — the canonical form history recall stores
    /// (the ref's `ChatEdit_AddHistory` header). `None` for the two leader types 1.12 gives no
    /// ChatEdit slash.
    pub(super) fn canonical_slash(self) -> Option<&'static str> {
        self.aliases().first().copied()
    }

    /// The slash aliases that switch the box to this type (`SLASH_<TYPE>n`, GlobalStrings
    /// 3490-3801 — the full quoted sets).
    fn aliases(self) -> &'static [&'static str] {
        match self {
            SendType::Say => &["s", "say"],
            SendType::Yell => &["y", "yell", "sh", "shout"],
            SendType::Emote => &["e", "em", "emote", "me"],
            SendType::Whisper => &["w", "whisper", "t", "tell", "send"],
            SendType::Party => &["p", "party"],
            SendType::Raid => &["raid", "ra", "rsay"],
            SendType::RaidLeader => &[], // no ChatEdit slash in 1.12 (leader auto via /raid)
            SendType::RaidWarning => &["rw"],
            SendType::Guild => &["g", "gc", "gu", "guild"],
            SendType::Officer => &["o", "osay"],
            SendType::Battleground => &["bg", "battleground"],
            SendType::BattlegroundLeader => &[],
            SendType::Channel => &["c", "csay"],
        }
    }
}

/// The channels this session has joined, in join order — the CLIENT-side number law
/// (`GetChannelName(n)`): `/1` is the first joined, `/2` the second; the numbered display form
/// ("1. General - Elwynn Forest") and the `[N. Name]` prefixes all derive from this order.
/// Fed by YOU_JOINED / YOU_LEFT notices ([`super::feed`]); the zone AUTO-join walk that fills it
/// at login is [`super::channels`].
#[derive(Resource, Default)]
pub(crate) struct ChannelState {
    pub joined: Vec<String>,
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
            .position(|c| c.eq_ignore_ascii_case(name))
            .map(|i| i as u32 + 1)
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

/// The chat edit box's cross-open state (the fields `ChatEdit_OnLoad` seeds on the box).
#[derive(Resource)]
pub(crate) struct ChatEditState {
    pub chat_type: SendType,
    /// `stickyType` — what Escape/close reverts to and what the box opens as.
    pub sticky: SendType,
    /// The current whisper target (`editBox.tellTarget`).
    pub tell_target: String,
    /// The current channel's WIRE name (the ref's `editBox.channelTarget` is the number; the
    /// name is what the send body carries either way).
    pub channel_target: String,
    /// The current channel's 1-based number (the joined-order slot), for the header.
    pub channel_number: u32,
    /// The `lastTell` ring (`NUM_REMEMBERED_TELLS = 10`), most recent first.
    pub last_tell: VecDeque<String>,
    /// `toldTarget` — who WE last whispered (the ctrl-R / ReplyTell2 memory).
    pub last_told: Option<String>,
    /// The header/insets need a repaint (type or target changed).
    pub header_dirty: bool,
    /// The last text the live parse saw (skip re-parsing an unchanged box).
    last_text: String,
}

impl Default for ChatEditState {
    fn default() -> Self {
        ChatEditState {
            chat_type: SendType::Say,
            sticky: SendType::Say,
            tell_target: String::new(),
            channel_target: String::new(),
            channel_number: 0,
            last_tell: VecDeque::new(),
            last_told: None,
            header_dirty: true,
            last_text: String::new(),
        }
    }
}

impl ChatEditState {
    /// `ChatEdit_SetLastTellTarget`: move-to-front dedup (case-insensitive), cap 10.
    pub(crate) fn remember_tell(&mut self, target: &str) {
        self.last_tell.retain(|t| !t.eq_ignore_ascii_case(target));
        self.last_tell.push_front(target.to_string());
        self.last_tell.truncate(10);
    }

    /// `ChatEdit_GetNextTellTarget`: the ring entry after `current` (wrapping to the most
    /// recent), for Tab cycling in whisper mode.
    pub(crate) fn next_tell(&self, current: &str) -> Option<String> {
        if self.last_tell.is_empty() {
            return None;
        }
        if current.is_empty() {
            return self.last_tell.front().cloned();
        }
        let pos = self
            .last_tell
            .iter()
            .position(|t| t.eq_ignore_ascii_case(current));
        match pos {
            Some(i) if i + 1 < self.last_tell.len() => self.last_tell.get(i + 1).cloned(),
            _ => self.last_tell.front().cloned(),
        }
    }

    /// The header's text for the current type (`ChatEdit_UpdateHeader`'s strings, quoted).
    /// Channel shows the stripped name (the `[%d. %s]:` number form is the P6 wiring).
    fn header_text(&self, own_name: &str) -> String {
        match self.chat_type {
            SendType::Say => "Say: ".into(),
            SendType::Yell => "Yell: ".into(),
            SendType::Emote => format!("{own_name} "),
            SendType::Whisper => format!("Tell {}: ", self.tell_target),
            SendType::Party => "Party: ".into(),
            SendType::Raid => "Raid: ".into(),
            SendType::RaidLeader => "Raid: ".into(),
            SendType::RaidWarning => "Raid Warning: ".into(),
            SendType::Guild => "Guild: ".into(),
            SendType::Officer => "Officer: ".into(),
            SendType::Battleground => "Battleground: ".into(),
            SendType::BattlegroundLeader => "Battleground: ".into(),
            SendType::Channel => {
                // CHAT_CHANNEL_SEND = "[%d. %s]: " (the zone tail stripped like the display law).
                let name = self.channel_target.split(" - ").next().unwrap_or("");
                format!("[{}. {name}]: ", self.channel_number)
            }
        }
    }
}

/// The live parse (`ChatEdit_ParseText(send=0)`, run per frame while the box is focused): a
/// leading `/<alias> ` converts the box's TYPE in place, keeping the remainder as the draft —
/// typing `/g hi` flips to Guild with "hi" in the box. `/w`/`/t` wait for the target word to
/// complete (`ChatEdit_ExtractTellTarget` grabs the first word once a space follows it); `/r`
/// loads the last teller (`ChatEdit_GetLastTellTarget`). Action commands (`/join`, `/wave`, …)
/// are NOT consumed here — they execute on Enter ([`super::input`]).
pub(super) fn chat_edit_live(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut state: ResMut<ChatEditState>,
    channels: Res<ChannelState>,
    ui_capture: Res<UiKeyboardCapture>,
    mut names: ResMut<NameCache>,
    self_guid: Res<SelfGuid>,
    commands: Res<NetCommands>,
) {
    let Some(script) = script else {
        return;
    };
    if !ui_capture.0 {
        return; // box not focused — nothing to live-parse
    }
    let text: String = script
        .eval("return (ChatFrameEditBox and ChatFrameEditBox:GetText()) or ''")
        .unwrap_or_default();
    if text != state.last_text {
        state.last_text = text.clone();
        if let Some((new_type, remainder)) = parse_type_switch(&state, &channels, &text) {
            match new_type {
                TypeSwitch::Plain(t) => {
                    state.chat_type = t;
                }
                TypeSwitch::Whisper(target) => {
                    state.chat_type = SendType::Whisper;
                    state.tell_target = target;
                }
                TypeSwitch::Channel { name, number } => {
                    state.chat_type = SendType::Channel;
                    state.channel_target = name;
                    state.channel_number = number;
                }
            }
            state.header_dirty = true;
            state.last_text = remainder.clone();
            let lua_text = remainder.replace('\\', "\\\\").replace('"', "\\\"");
            run_or_warn(
                &script,
                &format!("ChatFrameEditBox:SetText(\"{lua_text}\")"),
            );
        }
    }
    if state.header_dirty {
        // The Emote header is the player's own name (CHAT_EMOTE_SEND = "%s "), resolved through
        // the same ask-once cache as everyone else's.
        let own = self_guid
            .0
            .and_then(|g| names.resolve(g, &commands).map(str::to_string))
            .unwrap_or_else(|| "You".to_string());
        let header = state.header_text(&own);
        let [r, g, b] = state.chat_type.color();
        let (rf, gf, bf) = (
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
        );
        let lua_header = header.replace('\\', "\\\\").replace('"', "\\\"");
        // Header text+color, typed-text color, and the insets past the measured header
        // (`SetTextInsets(15 + header:GetWidth(), 13, 0, 0)` — ref ChatEdit_UpdateHeader l.1912).
        // The width is the measure round-trip's, one frame late for a fresh string — so we re-run
        // while dirty until a nonzero width lands. GetWidth serves ONLY a measure of the CURRENT
        // text (region.rs key-checks it): after a type switch (Say → "Tell Alice:") the old
        // header's width reads 0, not stale — the settle below waits for the RIGHT measure
        // instead of latching the previous header's insets (the /w cursor-in-the-header bug).
        run_or_warn(
            &script,
            &format!(
                "ChatFrameEditBoxHeader:SetText(\"{lua_header}\")\n\
                 ChatFrameEditBoxHeader:SetTextColor({rf:.4}, {gf:.4}, {bf:.4})\n\
                 ChatFrameEditBox:SetTextColor({rf:.4}, {gf:.4}, {bf:.4})\n\
                 local w = ChatFrameEditBoxHeader:GetWidth()\n\
                 if w and w > 1 then\n\
                     ChatFrameEditBox:SetTextInsets(15 + w, 13, 0, 0)\n\
                     BenillaChatHeaderSettled = true\n\
                 end"
            ),
        );
        // Stay dirty until the measured width landed (the round-trip is a frame late on a fresh
        // header string).
        if script
            .eval::<bool>("local s = BenillaChatHeaderSettled; BenillaChatHeaderSettled = false; return s or false")
            .unwrap_or(false)
        {
            state.header_dirty = false;
        }
    }
}

/// A live type switch the parse found.
pub(super) enum TypeSwitch {
    Plain(SendType),
    Whisper(String),
    Channel { name: String, number: u32 },
}

/// The `/N` / `/c <name-or-number>` channel switch against the joined list. `cmd_lower` is the
/// first word (no slash); returns the switch + remaining draft.
pub(super) fn channel_switch(
    channels: &ChannelState,
    cmd_lower: &str,
    args: &str,
) -> Option<(TypeSwitch, String)> {
    if let Ok(n) = cmd_lower.parse::<u32>() {
        let name = channels.joined.get(n.checked_sub(1)? as usize)?;
        return Some((
            TypeSwitch::Channel {
                name: name.clone(),
                number: n,
            },
            args.to_string(),
        ));
    }
    if ["c", "csay"].contains(&cmd_lower) {
        let (chan, remainder) = args.split_once(' ').unwrap_or((args, ""));
        if chan.is_empty() {
            return None;
        }
        let number = if let Ok(n) = chan.parse::<u32>() {
            n
        } else {
            channels.number_of(chan)?
        };
        let name = channels.joined.get((number - 1) as usize)?.clone();
        return Some((TypeSwitch::Channel { name, number }, remainder.to_string()));
    }
    None
}

/// Match `text` against the type-switch grammar. Returns the switch + the box's remaining draft.
pub(super) fn parse_type_switch(
    state: &ChatEditState,
    channels: &ChannelState,
    text: &str,
) -> Option<(TypeSwitch, String)> {
    let rest = text.strip_prefix('/')?;
    let (cmd, args) = rest.split_once(' ').unwrap_or((rest, ""));
    if cmd.is_empty() {
        return None;
    }
    let lower = cmd.to_ascii_lowercase();
    // `/<digits>` — the numbered-channel switch (`ChatEdit_ParseText` l.2110-2121: a live
    // GetChannelName hit converts immediately, remainder kept) and `/c <name-or-number>`
    // (`ChatEdit_ExtractChannel`). Both wait for the delimiting space like every live switch.
    if rest.contains(' ') {
        if let Some(switch) = channel_switch(channels, &lower, args) {
            return Some(switch);
        }
    }
    // `/r` — reply: load the last teller (only when one exists; else leave the text alone).
    if (lower == "r" || lower == "reply") && rest.contains(' ') {
        let target = state.last_tell.front()?.clone();
        return Some((TypeSwitch::Whisper(target), args.to_string()));
    }
    // Whisper family: wait for the completed target word ("/w Bob " — the space after the name
    // is the ref's extract trigger).
    if SendType::Whisper.aliases().contains(&lower.as_str()) {
        let (target, remainder) = args.split_once(' ')?;
        if target.is_empty() || target.starts_with('|') {
            return None; // the ref rejects a link-leading "name"
        }
        return Some((
            TypeSwitch::Whisper(target.to_string()),
            remainder.to_string(),
        ));
    }
    // The plain type families — the switch fires as soon as the command word is delimited
    // (a trailing space), so "/g" alone doesn't convert while you're still typing "/gc".
    if !rest.contains(' ') {
        return None;
    }
    for t in [
        SendType::Say,
        SendType::Yell,
        SendType::Emote,
        SendType::Party,
        SendType::Raid,
        SendType::RaidWarning,
        SendType::Guild,
        SendType::Officer,
        SendType::Battleground,
    ] {
        if t.aliases().contains(&lower.as_str()) {
            return Some((TypeSwitch::Plain(t), args.to_string()));
        }
    }
    None
}

/// The open commands through the binding table (0997; 1.12 defaults OPENCHAT = Enter,
/// OPENCHATSLASH = `/`, REPLY = R): OPENCHAT opens with the sticky type, downgrading an invalid
/// PARTY/RAID to SAY against the live group state (the ref's `ChatEdit_UpdateHeader`
/// invalid-type law; the sticky itself keeps its value, so rejoining a group restores it —
/// GUILD/BG downgrades wait on their arcs' state); OPENCHATSLASH opens pre-slashed; REPLY opens
/// as a reply to the last teller. The dispatch already gated on nothing owning the keyboard
/// (a focused box's Enter is the box's), and Shift-Enter no longer opens chat — `SHIFT-ENTER`
/// is a different chord than `ENTER`, the exact-modifier law the reference applies.
pub(super) fn open_chat_keys(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    binds: Res<crate::bindings::BindingsState>,
    mut state: ResMut<ChatEditState>,
    group: Res<crate::ui_party::GroupState>,
) {
    let Some(mut script) = script else {
        return;
    };
    use crate::bindings::cmd;
    let open_plain = binds.fired(cmd::OPEN_CHAT);
    let open_slash = binds.fired(cmd::OPEN_CHAT_SLASH);
    let open_reply = binds.fired(cmd::REPLY) && !state.last_tell.is_empty();
    if !(open_plain || open_slash || open_reply) {
        return;
    }
    if open_reply {
        state.chat_type = SendType::Whisper;
        state.tell_target = state.last_tell.front().cloned().unwrap_or_default();
    } else {
        state.chat_type = match state.sticky {
            SendType::Party if !group.in_group => SendType::Say,
            SendType::Raid | SendType::RaidWarning
                if !(group.in_group && group.group_type == 1) =>
            {
                SendType::Say
            }
            sticky => sticky,
        };
    }
    state.header_dirty = true;
    state.last_text.clear();
    script.focus_editbox("ChatFrameEditBox");
    if open_slash {
        run_or_warn(&script, "ChatFrameEditBox:SetText(\"/\")");
        state.last_text = "/".into();
    }
}

/// The unit popup's WHISPER action (`ChatFrame_SendTell` → the engine's tell queue, decision
/// 0434 §5): open the edit box in whisper mode to the named player — the ref fills its box with
/// "/w name "; our box's whisper form is chat_type + tell_target, the reply key's exact shape.
pub(super) fn open_tell_requests(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut state: ResMut<ChatEditState>,
) {
    let Some(mut script) = script else {
        return;
    };
    let Some(name) = script.take_tell_requests().pop() else {
        return;
    };
    state.chat_type = SendType::Whisper;
    state.tell_target = name;
    state.header_dirty = true;
    state.last_text.clear();
    script.focus_editbox("ChatFrameEditBox");
}

/// Tab in the box (the engine's `OnTabPressed` → `BenillaChatTabPressed` queue): whisper mode
/// cycles the `lastTell` ring (`ChatEdit_OnTabPressed` l.1983-1991). The slash tab-COMPLETION
/// walk (cycling `SLASH_*` prefix matches) is deferred with the P8 polish — flagged, not silent.
pub(super) fn chat_tab_cycle(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut state: ResMut<ChatEditState>,
) {
    let Some(mut script) = script else {
        return;
    };
    if !script.take_chat_tab() {
        return;
    }
    if state.chat_type == SendType::Whisper {
        let current = state.tell_target.clone();
        if let Some(next) = state.next_tell(&current) {
            state.tell_target = next;
            state.header_dirty = true;
        }
    }
}
