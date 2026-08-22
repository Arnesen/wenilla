//! The window model + router + composer (decision 0288 §1): [`ChatWindows`] holds each docked
//! window's message-group registration (the ref client's own chat-cache defaults, quoted from the
//! pin's `WTF/.../chat-cache.txt`); [`route`] fans one [`ChatEvent`] across every subscribed
//! window; [`compose`] is the `ChatFrame_OnEvent` composition law transcribed — the `CHAT_*_GET`
//! patterns, the `<AFK>/<DND>/<GM>` flag prefix, the `|Hplayer:…|h[Name]|h` link (never on EMOTE
//! or monster lines), the `[Language]` header, the `[N. Name]` channel prefix with its " - Zone"
//! tail stripped (the SPEECH branch only — a notice prints arg4 whole, 1275), and the
//! `CHAT_<X>_NOTICE` channel-notice strings. Formats are QUOTED from the extracted GlobalStrings
//! (0288's pin, §2/§4); colors come from [`super::event::resolved_color`].

use bevy::prelude::*;

use benilla_protocol::messages::channel_notice as notice;

use super::event::{event_name, group_kinds, resolved_color, ChatEvent, ChatEventKind, ChatGroup};

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
    /// The frame's own `this.defaultLanguage` — the name `GetDefaultLanguage()` answers, which is
    /// the **faction** tongue (Common for every Alliance race, Orcish for every Horde one).
    ///
    /// It lives here rather than being derived per line because that is where the reference keeps
    /// it: `ChatFrame.lua` stores it on the frame and the language-header test reads it from there
    /// ([`compose`]). Empty until the self descriptors and `Languages.dbc` are both up, which
    /// suppresses no header the reference would show — an empty default only ever makes the test
    /// *more* likely to print one.
    pub default_language: String,
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
                // WINDOW 2 "Combat Log": of its chat-cache list, MONEY, COMBAT_XP_GAIN and
                // COMBAT_HONOR_GAIN are the groups the current kind set carries (the rest of the
                // combat/spell block is the combat-log arc; XP joined with the ding arc, 0304,
                // and honour with the honor arc, 1512 — the reference registers the two on the
                // same frame one line apart, ChatFrame.lua l.2428-2429).
                vec![
                    ChatGroup::Money,
                    ChatGroup::CombatXpGain,
                    ChatGroup::CombatHonorGain,
                ],
            ],
            tell_alert_left: 0.0,
            default_language: String::new(),
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

/// Route one event: compose it once and AddMessage it into every subscribed window, then fire the
/// real `CHAT_MSG_*` at the VM. Whisper receipt side-effects (the throttled `TellMessage` chime +
/// the unselected-tab flash — ref ChatFrame_OnEvent l.1470-1477) ride the render half. A kind-less
/// event (an unmodeled wire type) drops with a warn — never silently.
///
/// **Two consumers, one event, no double-print.** In the reference, C fires `CHAT_MSG_<TYPE>` and
/// *Lua* — `ChatFrame_OnEvent` — is what turns it into a line; here the composer below IS that
/// handler, transcribed into Rust (0288 §1). So the fire is additive: it exists for **addons**,
/// and our windows keep rendering exactly as they did. Nothing prints twice because our shipped
/// `ChatFrame.xml` handles exactly one event, `EXECUTE_CHAT_LINE` (assets/ui/ChatFrame.xml, its
/// `<OnEvent>`) — an addon may `ChatFrame1:RegisterEvent("CHAT_MSG_SAY")` for its own reasons and
/// our frame's handler will simply not match it.
/// `ui_chat::tests::an_addon_registering_our_own_chat_frame_does_not_double_print` is the guard.
///
/// **Render first, then fire — that ordering is the reference's, not a convenience.** The client
/// dispatches an event to its listeners in registration order (FIFO, wow-re
/// `event-dispatch-order.md`, and [`benilla_ui::script::UiScript::fire_event`] transcribes it), and
/// ChatFrame1 registers at FrameXML load — before any addon exists. So in the real client the line
/// is already in the window by the time an addon's handler runs, and an addon that reads
/// `GetNumMessages()` or re-reads the last line from its own `CHAT_MSG_*` handler depends on that.
/// Our Rust composer stands in for ChatFrame1's handler, so it has to go first for the same reason.
///
/// **The fire is unconditional; the render is not.** The client's `SignalEvent` does not consult
/// any window's message-group registration — that is `ChatFrame_OnEvent`'s job, per frame — so an
/// event reaches Lua even when neither of our windows wants it. What does *not* reach here is a
/// notice the reference declines to make an event out of at all: MODE_CHANGE is dropped upstream,
/// at the feed, exactly as the client's `0x0C` arm returns without firing
/// (`ui_chat::tests::a_mode_change_notice_never_becomes_an_event`).
pub(crate) fn route(
    script: &mut benilla_ui::script::UiScript,
    windows: &mut ChatWindows,
    event: &ChatEvent,
) {
    let Some(kind) = event.kind else {
        warn!("chat: unroutable event (no kind): {:?}", event.text);
        return;
    };
    // ── our own window: the transcribed ChatFrame_OnEvent, i.e. the first-registered listener ──
    // Cloned once rather than borrowed, because `windows` goes on to be used mutably below.
    let default_language = windows.default_language.clone();
    if let Some(line) = compose(event, kind, &default_language) {
        let color = resolved_color(event, kind);
        for idx in 0..2 {
            if windows.wants(idx, kind) {
                add(script, &format!("ChatFrame{}", idx + 1), &line, color);
            }
        }
        if kind == ChatEventKind::Whisper {
            // The tell chime, throttled to one per 5 minutes (CHAT_TELL_ALERT_TIME), and the tab
            // flash when the receiving window (1) isn't the selected dock tab. Inside this half
            // because the reference does them inside ChatFrame_OnEvent too (l.1470-1477).
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
    // ── everyone else: the addons, after the default UI, exactly as registration order says ──
    script.fire_event(event_name(kind), event.script_args());
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
