//! The window model + router + composer (decision 0288 §1): [`ChatWindows`] holds each docked
//! window's message-group registration (the ref client's own chat-cache defaults, quoted from the
//! pin's `WTF/.../chat-cache.txt`); [`route`] fans one [`ChatEvent`] across every subscribed
//! window; [`compose`] is the `ChatFrame_OnEvent` composition law transcribed — the `CHAT_*_GET`
//! patterns, the `<AFK>/<DND>/<GM>` flag prefix, the `|Hplayer:…|h[Name]|h` link (never on EMOTE
//! or monster lines), the `[Language]` header, the `[N. Name]` channel prefix with its " - Zone"
//! tail stripped (the SPEECH branch only — a notice prints arg4 whole, 1275), and the
//! `CHAT_<X>_NOTICE` channel-notice strings. Formats are QUOTED from the extracted GlobalStrings
//! (0288's pin, §2/§4); colors come from the stock window's `ChatTypeInfo` table (1948), whose
//! shipped defaults are transcribed in [`super::event::default_color`].

use bevy::prelude::*;

use benilla_protocol::messages::channel_notice as notice;

use super::event::{event_name, ChatEvent, ChatEventKind};

/// What the app keeps beside the reference's chat frames: the default language its composer
/// needs for the log-file line, and the log files themselves. The per-window registration that
/// lived here (0288 §1) is the record's MESSAGES set now, read by the reference's own
/// `ChatFrame_RegisterForMessages` (decision 1948).
#[derive(Resource, Default)]
pub(crate) struct ChatWindows {
    /// The frame's own `this.defaultLanguage` — the name `GetDefaultLanguage()` answers, which is
    /// the **faction** tongue (Common for every Alliance race, Orcish for every Horde one).
    ///
    /// It lives here rather than being derived per line because that is where the reference keeps
    /// it: `ChatFrame.lua` stores it on the frame and the language-header test reads it from there
    /// ([`compose`]). Empty until the self descriptors and `Languages.dbc` are both up, which
    /// suppresses no header the reference would show — an empty default only ever makes the test
    /// *more* likely to print one.
    pub default_language: String,
    /// Whether a non-window observer wants [`Self::routed`] filled — set by the web bridge
    /// while a page is listening, off otherwise so an unobserved session never clones a line.
    pub observe: bool,
    /// Every event [`route`] delivered since the observer last drained it, in delivery order —
    /// the same lines the windows and the Lua fire saw, one copy each. Empty unless
    /// [`Self::observe`] is on.
    pub routed: Vec<ChatEvent>,
    /// `LoggingChat`/`LoggingCombat`'s files ([`super::logging`]) — here because every
    /// rendered line passes [`route`], which is the one place to tee them.
    pub logs: super::logging::ChatLogFiles,
}

/// Route one event: fire the real `CHAT_MSG_*` at the VM — the reference's own `ChatFrame_OnEvent`
/// composes and prints it, in every window whose MESSAGES set carries the type, with
/// `ChatTypeInfo`'s colour, the whisper chime and the tab flash (decision 1948) — and tee the
/// rendered line to the log files. A kind-less event (an unmodeled wire type) drops with a warn,
/// never silently.
///
/// The composer that used to print here survives for the log line only: `LoggingChat`'s file
/// wants the text the window shows, and the reference writes it C-side, not from Lua.
pub(crate) fn route(
    script: &mut benilla_ui::script::UiScript,
    windows: &mut ChatWindows,
    event: &ChatEvent,
) {
    let Some(kind) = event.kind else {
        warn!("chat: unroutable event (no kind): {:?}", event.text);
        return;
    };
    // The window shows what the reference's `ChatFrame_OnEvent` prints from the event — its
    // own composition, `ChatTypeInfo`'s colours, the per-window registration
    // (`ChatFrame_RegisterForMessages` over the record's MESSAGES set), the tell chime and the
    // tab flash. The app's transcription of that composition survives only for the log files,
    // which want the rendered line the window will show.
    let default_language = windows.default_language.clone();
    if let Some(line) = compose(event, kind, &default_language) {
        windows.logs.record(kind.is_combat_log(), &line);
    }
    script.fire_event(event_name(kind), event.script_args());
    // ── and the observer that is not a frame (the web bridge), after both ──
    if windows.observe {
        windows.routed.push(event.clone());
    }
}

/// `ChatFrame_OnEvent`'s composition, transcribed (ref ChatFrame.lua l.1369-1468 + the quoted
/// GlobalStrings). Returns `None` for a notice the 1.12 UI renders silently (MODE_CHANGE).
pub(crate) fn compose(
    event: &ChatEvent,
    kind: ChatEventKind,
    default_language: &str,
) -> Option<String> {
    use ChatEventKind as K;
    Some(match kind {
        // Verbatim families (l.1395-1402): the text IS the line. COMBAT_XP_GAIN rides the same
        // default handler tail (client-composed, no sender) — ref l.1425's fall-through.
        K::System
        | K::TextEmote
        | K::Skill
        | K::Loot
        | K::Money
        | K::CombatXpGain
        | K::CombatHonorGain
        | K::BgSystemNeutral
        | K::BgSystemAlliance
        | K::BgSystemHorde => event.text.clone(),
        // The whole combat-log block is verbatim too, and the reference says so by PREFIX rather
        // than by name: l.1397-1400 is two arms, `strsub(type,1,7) == "COMBAT_"` and
        // `strsub(type,1,6) == "SPELL_"`, each doing nothing but `AddMessage(arg1, …)`. The
        // sentence was already built by the time it became an event — that is what
        // [`super::combat`] is — so there is nothing left for the composer to do.
        k if k.is_combat_log() => event.text.clone(),
        // "%s is ignoring you." (CHAT_IGNORED, arg2).
        K::Ignored => format!("{} is ignoring you.", event.sender),
        // "[%s] " .. the member list (CHAT_CHANNEL_LIST_GET, l.1409) — arg4 WHOLE, see
        // [`strip_zone`]: only the speech branch runs the gsub.
        K::ChannelList => format!("[{}] {}", event.channel, event.text),
        K::ChannelNotice | K::ChannelNoticeUser => {
            return compose_notice(event);
        }
        // Everything else is the player/monster-line branch (l.1425-1467).
        _ => {
            let pflag = match event.flag.as_str() {
                "AFK" => "<AFK>",
                "DND" => "<DND>",
                "GM" => "<GM>",
                _ => "",
            };
            let monster = matches!(
                kind,
                K::MonsterSay
                    | K::MonsterYell
                    | K::MonsterEmote
                    | K::MonsterWhisper
                    | K::RaidBossEmote
            );
            // The sender as rendered: hyperlinked `[Name]` for player lines (l.1451), bare for
            // monsters + RAID_BOSS_EMOTE (l.1437-1438) and for EMOTE (l.1450's `type ~= "EMOTE"`).
            let named = if event.sender.is_empty() {
                String::new()
            } else if monster || kind == K::Emote {
                format!("{pflag}{}", event.sender)
            } else {
                format!("{pflag}|Hplayer:{0}|h[{0}]|h", event.sender)
            };
            // The language header (l.1442-1448): non-empty, non-Universal (mapped to "" by the
            // bridge), and not our own default tongue.
            //
            // **That last clause used to read `!= "Common"`** — the comment was already right and
            // the code was not, which cost every Horde character a `[Orcish]` tag on ordinary
            // faction chat and stripped the tag from Common. The reference's test is
            // `arg3 ~= this.defaultLanguage` and `GetDefaultLanguage()` answers the **faction**
            // tongue, so it reads Orcish for a Horde body (wow-re
            // `system/ui/scratch/chat-language-scramble.md` §10/§12: the tag is FrameXML's and its
            // condition is about the *default* language, never about whether the language is
            // understood — a character who knows both Common and Dwarvish still sees `[Dwarvish]`
            // on a line they read perfectly).
            //
            // The `~= "Universal"` clause of the reference's condition is deliberately absent:
            // "Universal" is in neither `Languages.dbc` nor `WoW.exe` nor `GlobalStrings.lua`, so
            // it is vestigial in 1.12 and the empty-string test is what actually suppresses
            // language 0 (see [`super::event::ChatEvent`]'s arg3 note).
            let header = if !event.language.is_empty() && event.language != default_language {
                format!("[{}] ", event.language)
            } else {
                String::new()
            };
            // MONSTER_EMOTE / RAID_BOSS_EMOTE embed their `%s` in the text itself
            // (CHAT_MONSTER_EMOTE_GET = "" — l.1437 keeps the name bare for the substitution).
            let body = if matches!(kind, K::MonsterEmote | K::RaidBossEmote) {
                format!("{header}{}", event.text.replace("%s", &named))
            } else {
                let get = get_pattern(kind);
                format!("{}{header}{}", get.replace("%s", &named), event.text)
            };
            // The channel prefix (l.1462-1466): arg4 with its " - Zone" tail stripped,
            // bracketed. arg4 arrives already numbered ("2. Trade - City") once the channel
            // wiring (P6) assigns numbers.
            if !event.channel.is_empty() {
                format!("[{}] {body}", strip_zone(&event.channel))
            } else {
                body
            }
        }
    })
}

/// The `CHAT_<TYPE>_GET` prefix patterns (GlobalStrings, quoted — `\32` spaces verbatim).
fn get_pattern(kind: ChatEventKind) -> &'static str {
    use ChatEventKind as K;
    match kind {
        K::Say => "%s says: ",
        K::Yell => "%s yells: ",
        K::Whisper => "%s whispers: ",
        K::WhisperInform => "To %s: ",
        K::Emote => "%s ",
        K::Afk => "%s is Away From Keyboard: ",
        K::Dnd => "%s does not wish to be disturbed: ",
        K::Party => "[Party] %s: ",
        K::Guild => "[Guild] %s: ",
        K::Officer => "[Officer] %s: ",
        K::Raid => "[Raid] %s: ",
        K::RaidLeader => "[Raid Leader] %s: ",
        K::RaidWarning => "[Raid Warning] %s: ",
        K::Battleground => "[Battleground] %s: ",
        K::BattlegroundLeader => "[Battleground Leader] %s: ",
        K::Channel => "%s: ",
        K::ChannelJoin => "%s joined channel.",
        K::ChannelLeave => "%s left channel.",
        K::MonsterSay => "%s says: ",
        K::MonsterYell => "%s yells: ",
        K::MonsterWhisper => "%s whispers: ",
        // Handled before get_pattern is consulted.
        _ => "%s",
    }
}

/// Strip the zone tail from a channel display name (`gsub(arg4, "%s%-%s.*", "")` —
/// "General - Elwynn Forest" → "General", "2. Trade - City" → "2. Trade").
///
/// **The speech branch is the ONLY caller, and that is the reference's own shape** (1275): the
/// gsub sits at l.1463, inside the `else` arm that builds a player/monster line, *after* every
/// notice arm has already returned. CHANNEL_NOTICE (l.1424), CHANNEL_NOTICE_USER (l.1416/1418)
/// and CHANNEL_LIST (l.1409) each pass **arg4 whole** into their format — so the real client's
/// join line reads "Joined Channel: [1. General - Elwynn Forest]" while a line spoken in that same
/// channel is prefixed "[1. General]". We stripped in all four and lost the tail from three.
fn strip_zone(channel: &str) -> &str {
    match channel.find(" - ") {
        Some(i) => &channel[..i],
        None => channel,
    }
}

/// The `SMSG_CHANNEL_NOTIFY` → chat line law: the notice byte selects the quoted
/// `CHAT_<X>_NOTICE` string (GlobalStrings 493-745); `channel` fills `%s` first, the tail names
/// (already guid-resolved by the bridge) fill the rest. `None` = the 1.12 UI shows nothing for
/// this notice (MODE_CHANGE has no NOTICE string — flag-change chatter is silent).
///
/// `chan` is arg4 **whole**, zone tail and all — see [`strip_zone`] for why the notice arms are
/// not the gsub's callers.
pub(crate) fn compose_notice(event: &ChatEvent) -> Option<String> {
    let chan = &event.channel;
    let a = &event.sender; // the notice's first name (actor / affected)
    let b = &event.target; // the second name (kicked-by style)
    let n: u8 = event.notice_byte().unwrap_or(0xFF);
    Some(match n {
        notice::YOU_JOINED => format!("Joined Channel: [{chan}]"),
        notice::YOU_LEFT => format!("Left Channel: [{chan}]"),
        notice::WRONG_PASSWORD => format!("Wrong password for {chan}."),
        notice::NOT_MEMBER => format!("Not on channel {chan}."),
        notice::NOT_MODERATOR => format!("Not a moderator of {chan}."),
        notice::PASSWORD_CHANGED => format!("[{chan}] Password changed by {a}."),
        notice::OWNER_CHANGED => format!("[{chan}] Owner changed to {a}."),
        notice::PLAYER_NOT_FOUND => format!("[{chan}] Player {a} is not on channel."),
        notice::NOT_OWNER => format!("[{chan}] You are not the channel owner."),
        notice::CHANNEL_OWNER => format!("[{chan}] Channel owner is {a}."),
        notice::MODE_CHANGE => return None, // no NOTICE string in 1.12 — silent
        notice::ANNOUNCEMENTS_ON => format!("[{chan}] Channel announcements enabled by {a}."),
        notice::ANNOUNCEMENTS_OFF => format!("[{chan}] Channel announcements disabled by {a}."),
        notice::MODERATION_ON => format!("[{chan}] Channel moderation enabled by {a}."),
        notice::MODERATION_OFF => format!("[{chan}] Channel moderation disabled by {a}."),
        notice::MUTED => format!("[{chan}] You do not have permission to speak."),
        notice::PLAYER_KICKED => format!("[{chan}] Player {a} kicked by {b}."),
        notice::BANNED => format!("[{chan}] You are banned from that channel."),
        notice::PLAYER_BANNED => format!("[{chan}] Player {a} banned by {b}."),
        notice::PLAYER_UNBANNED => format!("[{chan}] Player {a} unbanned by {b}."),
        notice::PLAYER_NOT_BANNED => format!("[{chan}] Player {a} is not banned."),
        notice::PLAYER_ALREADY_MEMBER => format!("[{chan}] Player {a} is already on the channel."),
        notice::INVITE => format!("{a} has invited you to join the channel '{chan}'."),
        notice::INVITE_WRONG_FACTION => format!("Target is in the wrong alliance for {chan}."),
        notice::WRONG_FACTION => format!("Wrong alliance for {chan}."),
        notice::INVALID_NAME => "Invalid channel name".to_string(),
        notice::NOT_MODERATED => format!("{chan} is not moderated"),
        notice::PLAYER_INVITED => format!("[{chan}] You invited {a} to join the channel"),
        notice::PLAYER_INVITE_BANNED => format!("[{chan}] {a} has been banned."),
        notice::THROTTLED => format!(
            "[{chan}] The number of messages that can be sent to this channel is limited, \
             please wait to send another message."
        ),
        _ => return None,
    })
}
