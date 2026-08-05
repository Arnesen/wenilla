//! The window model + router + composer (decision 0288 §1): [`ChatWindows`] holds each docked
//! window's message-group registration (the ref client's own chat-cache defaults, quoted from the
//! pin's `WTF/.../chat-cache.txt`); [`route`] fans one [`ChatEvent`] across every subscribed
//! window; [`compose`] is the `ChatFrame_OnEvent` composition law transcribed — the `CHAT_*_GET`
//! patterns, the `<AFK>/<DND>/<GM>` flag prefix, the `|Hplayer:…|h[Name]|h` link (never on EMOTE
//! or monster lines), the `[Language]` header, the `[N. Name]` channel prefix with its " - Zone"
//! tail stripped, and the `CHAT_<X>_NOTICE` channel-notice strings. Formats are QUOTED from the
//! extracted GlobalStrings (0288's pin, §2/§4); colors come from [`super::event::default_color`].

use bevy::prelude::*;

use benilla_protocol::messages::channel_notice as notice;

use super::event::{default_color, group_kinds, ChatEvent, ChatEventKind, ChatGroup};

/// How long after a received whisper the `TellMessage` alert stays silent
/// (`CHAT_TELL_ALERT_TIME = 300` — ref ChatFrame.lua l.4: only a tell arriving ≥5 min after the
/// previous one chimes).
const TELL_ALERT_SECS: f32 = 300.0;

/// The docked windows' registrations — the shipped defaults: window 1 "General" and window 2
/// "Combat Log", exactly the ref client's own chat-cache WINDOW blocks (0288 pin §6b). Window 2's
/// combat/spell groups beyond MONEY have no sources yet (the combat-log content arc); it renders
/// what its groups receive.
#[derive(Resource)]
pub(crate) struct ChatWindows {
    /// `groups[i]` = window `i+1`'s registered message groups.
    pub groups: [Vec<ChatGroup>; 2],
    /// Seconds left before the next received whisper chimes again ([`TELL_ALERT_SECS`]).
    pub tell_alert_left: f32,
}

impl Default for ChatWindows {
    fn default() -> Self {
        ChatWindows {
            groups: [
                // WINDOW 1 "General": MESSAGES = SYSTEM SAY YELL WHISPER PARTY GUILD CREATURE
                // CHANNEL SKILL LOOT (chat-cache verbatim).
                vec![
                    ChatGroup::System,
                    ChatGroup::Say,
                    ChatGroup::Yell,
                    ChatGroup::Whisper,
                    ChatGroup::Party,
                    ChatGroup::Guild,
                    ChatGroup::Creature,
                    ChatGroup::Channel,
                    ChatGroup::Skill,
                    ChatGroup::Loot,
                ],
                // WINDOW 2 "Combat Log": of its chat-cache list, MONEY and COMBAT_XP_GAIN are
                // the groups the current kind set carries (the rest of the combat/spell block is
                // the combat-log arc; XP joined with the ding arc, 0304).
                vec![ChatGroup::Money, ChatGroup::CombatXpGain],
            ],
            tell_alert_left: 0.0,
        }
    }
}

impl ChatWindows {
    /// Whether window `idx` (0-based) subscribes to `kind`. CHANNEL speech (the numbered
    /// channels) routes by the window's channel wiring — until 0288 P6 lands those lists, it
    /// rides window 1 (the chat-cache ZONECHANNELS mask wires exactly window 1 anyway).
    fn wants(&self, idx: usize, kind: ChatEventKind) -> bool {
        if kind == ChatEventKind::Channel {
            return idx == 0;
        }
        self.groups[idx]
            .iter()
            .any(|&g| group_kinds(g).contains(&kind))
    }
}

/// Route one event: compose it once, then AddMessage into every subscribed window. Whisper
/// receipt side-effects (the throttled `TellMessage` chime + the unselected-tab flash — ref
/// ChatFrame_OnEvent l.1470-1477) fire through the script seam. A kind-less event (an
/// unmodeled wire type) drops with a warn — never silently.
pub(crate) fn route(
    script: &mut benilla_ui::script::UiScript,
    windows: &mut ChatWindows,
    event: &ChatEvent,
) {
    let Some(kind) = event.kind else {
        warn!("chat: unroutable event (no kind): {:?}", event.text);
        return;
    };
    let Some(line) = compose(event, kind) else {
        return; // composed to nothing (a silent notice)
    };
    let color = default_color(kind);
    for idx in 0..2 {
        if windows.wants(idx, kind) {
            add(script, &format!("ChatFrame{}", idx + 1), &line, color);
        }
    }
    if kind == ChatEventKind::Whisper {
        // The tell chime, throttled to one per 5 minutes (CHAT_TELL_ALERT_TIME), and the tab
        // flash when the receiving window (1) isn't the selected dock tab.
        if windows.tell_alert_left <= 0.0 {
            crate::ui_script::run_or_warn(script, "PlaySound(\"TellMessage\")");
        }
        windows.tell_alert_left = TELL_ALERT_SECS;
        crate::ui_script::run_or_warn(
            script,
            "if BenillaFCF.selected ~= 1 then BenillaFCF_FlashTab(1) end",
        );
    }
}

/// Add one composed line, converting the `0..255` table color to the seam's `0..1` floats.
fn add(script: &mut benilla_ui::script::UiScript, frame: &str, text: &str, color: [u8; 3]) {
    script.add_chat_message(
        frame,
        text,
        f32::from(color[0]) / 255.0,
        f32::from(color[1]) / 255.0,
        f32::from(color[2]) / 255.0,
    );
}

/// `ChatFrame_OnEvent`'s composition, transcribed (ref ChatFrame.lua l.1369-1468 + the quoted
/// GlobalStrings). Returns `None` for a notice the 1.12 UI renders silently (MODE_CHANGE).
pub(crate) fn compose(event: &ChatEvent, kind: ChatEventKind) -> Option<String> {
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
        | K::BgSystemNeutral
        | K::BgSystemAlliance
        | K::BgSystemHorde => event.text.clone(),
        // "%s is ignoring you." (CHAT_IGNORED, arg2).
        K::Ignored => format!("{} is ignoring you.", event.sender),
        // "[%s] " .. the member list (CHAT_CHANNEL_LIST_GET).
        K::ChannelList => format!("[{}] {}", strip_zone(&event.channel), event.text),
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
            let header = if !event.language.is_empty() && event.language != "Common" {
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
pub(crate) fn compose_notice(event: &ChatEvent) -> Option<String> {
    let chan = strip_zone(&event.channel);
    let a = &event.sender; // the notice's first name (actor / affected)
    let b = &event.target; // the second name (kicked-by style)
    let n: u8 = event.notice.parse().unwrap_or(0xFF);
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
