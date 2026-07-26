use crate::net::ChatKind;

use super::event::{default_color, ChatEvent, ChatEventKind as K};
use super::frames::compose;
use super::input::{emote_send_eligible, parse_line, ParsedChat};

/// A player-line event (the wire bridge's output shape) — sender resolved, optional flag.
fn ev(kind: K, text: &str, sender: &str) -> ChatEvent {
    ChatEvent {
        kind: Some(kind),
        text: text.into(),
        sender: sender.into(),
        ..Default::default()
    }
}

#[test]
fn player_lines_link_the_name_except_emote() {
    // The composer emits the REAL |Hplayer link now (ref ChatFrame.lua l.1451); the renderer
    // strips the markers and spans the [Name] (the P2 markup law).
    assert_eq!(
        compose(&ev(K::Say, "hi there", "Bob"), K::Say).unwrap(),
        "|Hplayer:Bob|h[Bob]|h says: hi there"
    );
    assert_eq!(
        compose(&ev(K::WhisperInform, "hey", "Bob"), K::WhisperInform).unwrap(),
        "To |Hplayer:Bob|h[Bob]|h: hey"
    );
    // EMOTE uses the bare name (l.1450 `type ~= "EMOTE"`).
    assert_eq!(
        compose(&ev(K::Emote, "dances.", "Bob"), K::Emote).unwrap(),
        "Bob dances."
    );
}

#[test]
fn group_prefixed_kinds_wear_their_brackets() {
    assert_eq!(
        compose(&ev(K::Party, "inc 3", "Ann"), K::Party).unwrap(),
        "[Party] |Hplayer:Ann|h[Ann]|h: inc 3"
    );
    assert_eq!(
        compose(&ev(K::Guild, "gz", "Ann"), K::Guild).unwrap(),
        "[Guild] |Hplayer:Ann|h[Ann]|h: gz"
    );
    assert_eq!(
        compose(&ev(K::RaidWarning, "move", "Ann"), K::RaidWarning).unwrap(),
        "[Raid Warning] |Hplayer:Ann|h[Ann]|h: move"
    );
}

#[test]
fn flags_prefix_the_name_and_afk_uses_its_get() {
    let mut e = ev(K::Say, "brb", "Bob");
    e.flag = "GM".into();
    assert_eq!(
        compose(&e, K::Say).unwrap(),
        "<GM>|Hplayer:Bob|h[Bob]|h says: brb"
    );
    // A received AFK auto-reply: CHAT_AFK_GET (whisper-pink family).
    assert_eq!(
        compose(&ev(K::Afk, "farming", "Bob"), K::Afk).unwrap(),
        "|Hplayer:Bob|h[Bob]|h is Away From Keyboard: farming"
    );
}

#[test]
fn language_header_rides_non_default_tongues() {
    let mut e = ev(K::Say, "throm-ka", "Grunk");
    e.language = "Orcish".into();
    assert_eq!(
        compose(&e, K::Say).unwrap(),
        "|Hplayer:Grunk|h[Grunk]|h says: [Orcish] throm-ka"
    );
    // Common (our default) and Universal (empty) render no header.
    e.language = "Common".into();
    assert_eq!(
        compose(&e, K::Say).unwrap(),
        "|Hplayer:Grunk|h[Grunk]|h says: throm-ka"
    );
}

#[test]
fn system_and_loot_lines_are_verbatim() {
    assert_eq!(
        compose(
            &ChatEvent::text_only(K::System, "Additem: Wool Cloth added.".into()),
            K::System
        )
        .unwrap(),
        "Additem: Wool Cloth added."
    );
    assert_eq!(
        compose(
            &ChatEvent::text_only(K::Loot, "You receive loot: [Tough Jerky].".into()),
            K::Loot
        )
        .unwrap(),
        "You receive loot: [Tough Jerky]."
    );
}

#[test]
fn level_up_lines_follow_the_reference_order_and_forms() {
    use benilla_protocol::messages::LevelUpInfo;

    // A caster ding with talent point + three stat gains: the PLAYER_LEVEL_UP handler's exact
    // line order (ChatFrame.lua:1283-1324) — LEVEL_UP, HEALTH_MANA, CHAR_POINTS, STAT × positive.
    let l = LevelUpInfo {
        level: 10,
        health: 22,
        powers: [15, 0, 0, 0, 0],
        stats: [0, 1, 2, 3, 0],
    };
    assert_eq!(
        super::feed::level_up_lines(&l, 1),
        vec![
            "Congratulations, you have reached level 10!",
            "You have gained 22 hit points and 15 mana.",
            "You have gained 1 talent point.",
            "Your Agility increases by 1.",
            "Your Stamina increases by 2.",
            "Your Intellect increases by 3.",
        ]
    );
    // A manaless early ding: LEVEL_UP_HEALTH form, no talent line (arg4 == 0 skips), the plural
    // form when more than one point.
    let l = LevelUpInfo {
        level: 2,
        health: 12,
        powers: [0; 5],
        stats: [1, 0, 1, 0, 0],
    };
    assert_eq!(
        super::feed::level_up_lines(&l, 0),
        vec![
            "Congratulations, you have reached level 2!",
            "You have gained 12 hit points.",
            "Your Strength increases by 1.",
            "Your Stamina increases by 1.",
        ]
    );
    assert_eq!(
        super::feed::level_up_lines(&l, 2)[2],
        "You have gained 2 talent points."
    );
}

#[test]
fn xp_gain_lines_pick_the_reference_form() {
    // COMBATLOG_XPGAIN_FIRSTPERSON / its EXHAUSTION1 rested form / _UNNAMED (GlobalStrings
    // :801/:789/:804).
    assert_eq!(
        super::feed::xp_gain_line(Some("Kobold Vermin"), 35, 0),
        "Kobold Vermin dies, you gain 35 experience."
    );
    assert_eq!(
        super::feed::xp_gain_line(Some("Kobold Vermin"), 52, 17),
        "Kobold Vermin dies, you gain 52 experience. (+17 exp Rested bonus)"
    );
    assert_eq!(
        super::feed::xp_gain_line(None, 120, 0),
        "You gain 120 experience."
    );
    // The XP kind wears the shipped lavender (chat-cache row 46, 0x6F6FFF).
    assert_eq!(default_color(K::CombatXpGain), [111, 111, 255]);
}

#[test]
fn monster_lines_use_the_bare_inline_name() {
    assert_eq!(
        compose(&ev(K::MonsterSay, "Intruders!", "Guard"), K::MonsterSay).unwrap(),
        "Guard says: Intruders!"
    );
    // MONSTER_EMOTE embeds %s where the name goes (CHAT_MONSTER_EMOTE_GET = "").
    assert_eq!(
        compose(
            &ev(K::MonsterEmote, "%s beckons you closer.", "Sentinel"),
            K::MonsterEmote
        )
        .unwrap(),
        "Sentinel beckons you closer."
    );
}

#[test]
fn channel_line_prefixes_the_stripped_channel() {
    let mut e = ev(K::Channel, "wts boar livers", "Bob");
    e.channel = "General - Elwynn Forest".into();
    assert_eq!(
        compose(&e, K::Channel).unwrap(),
        "[General] |Hplayer:Bob|h[Bob]|h: wts boar livers"
    );
}

#[test]
fn channel_notices_compose_by_the_notice_law() {
    let mut e = ChatEvent::text_only(K::ChannelNotice, String::new());
    e.channel = "General - Elwynn Forest".into();
    e.notice = "2".into(); // YOU_JOINED
    assert_eq!(
        compose(&e, K::ChannelNotice).unwrap(),
        "Joined Channel: [General]"
    );
    let mut kick = ChatEvent::text_only(K::ChannelNotice, String::new());
    kick.channel = "World".into();
    kick.sender = "Ann".into();
    kick.target = "Mod".into();
    kick.notice = "18".into(); // PLAYER_KICKED 0x12
    assert_eq!(
        compose(&kick, K::ChannelNotice).unwrap(),
        "[World] Player Ann kicked by Mod."
    );
    // A member join line is a CHANNEL_JOIN event, hyperlinked like any player line.
    let mut join = ev(K::ChannelJoin, "", "Ann");
    join.channel = "World".into();
    assert_eq!(
        compose(&join, K::ChannelJoin).unwrap(),
        "[World] |Hplayer:Ann|h[Ann]|h joined channel."
    );
}

#[test]
fn colors_match_the_shipped_table() {
    assert_eq!(default_color(K::Say), [255, 255, 255]);
    assert_eq!(default_color(K::System), [255, 255, 0]);
    assert_eq!(default_color(K::Yell), [255, 64, 64]);
    assert_eq!(default_color(K::Emote), [255, 128, 64]);
    assert_eq!(default_color(K::MonsterSay), [255, 255, 159]);
    assert_eq!(default_color(K::Loot), [0, 170, 0]);
    assert_eq!(default_color(K::Money), [255, 255, 0]);
    assert_eq!(default_color(K::ChannelNotice), [192, 192, 192]);
    assert_eq!(default_color(K::RaidWarning), [255, 219, 183]);
    assert_eq!(default_color(K::BgSystemAlliance), [0, 174, 239]);
}

// ── the submitted-line grammar (0288 P5): type switches + action commands ──────────────────

/// A stub `EmotesText` resolver: only "wave" resolves, to id 101 (mirrors `/wave`'s real id).
fn stub_emotes(name: &str) -> Option<u32> {
    (name == "wave").then_some(101)
}

/// The Enter-path type switch (send path — no trailing-space requirement).
fn enter_switch(text: &str) -> Option<(super::edit::TypeSwitch, String)> {
    super::input::parse_enter_type_switch(&super::edit::ChannelState::default(), text)
}

#[test]
fn enter_path_type_switch_converts_and_keeps_the_remainder() {
    use super::edit::{SendType, TypeSwitch};
    for (cmd, want) in [
        ("s", SendType::Say),
        ("say", SendType::Say),
        ("y", SendType::Yell),
        ("sh", SendType::Yell),
        ("g", SendType::Guild),
        ("gc", SendType::Guild),
        ("p", SendType::Party),
        ("rw", SendType::RaidWarning),
        ("bg", SendType::Battleground),
        ("o", SendType::Officer),
        ("e", SendType::Emote),
    ] {
        let (sw, rest) = enter_switch(&format!("/{cmd} hi there")).expect(cmd);
        match sw {
            TypeSwitch::Plain(t) => assert_eq!(t, want, "/{cmd}"),
            _ => panic!("/{cmd} is a plain type switch"),
        }
        assert_eq!(rest, "hi there");
    }
    // Case-insensitive; a bare "/g" still converts (empty remainder = the sticky commit path).
    assert!(enter_switch("/YELL loud").is_some());
    let (_, rest) = enter_switch("/g").unwrap();
    assert_eq!(rest, "");
}

#[test]
fn enter_path_whisper_takes_name_then_message() {
    use super::edit::TypeSwitch;
    for cmd in ["w", "whisper", "t", "tell", "send"] {
        let (sw, rest) = enter_switch(&format!("/{cmd} Bob hi there")).expect(cmd);
        match sw {
            TypeSwitch::Whisper(target) => assert_eq!(target, "Bob"),
            _ => panic!("expected whisper"),
        }
        assert_eq!(rest, "hi there");
    }
    // Needs a name AND a message on the enter path; a link-leading "name" is rejected.
    assert!(enter_switch("/w").is_none());
    assert!(enter_switch("/w Bob").is_none());
    assert!(enter_switch("/w |Hitem:1|h[x]|h hi").is_none());
}

#[test]
fn live_parse_waits_for_the_delimiting_space() {
    use super::edit::{parse_type_switch, ChannelState, ChatEditState, TypeSwitch};
    let mut state = ChatEditState::default();
    let chans = ChannelState::default();
    // "/g" alone: still typing (could become /gc) — no switch until the space lands.
    assert!(parse_type_switch(&state, &chans, "/g").is_none());
    let (sw, rest) = parse_type_switch(&state, &chans, "/g hi").expect("switch on space");
    assert!(matches!(
        sw,
        TypeSwitch::Plain(super::edit::SendType::Guild)
    ));
    assert_eq!(rest, "hi");
    // "/w Bob" waits for the space AFTER the name (the ref's extract trigger).
    assert!(parse_type_switch(&state, &chans, "/w Bob").is_none());
    let (sw, rest) = parse_type_switch(&state, &chans, "/w Bob ").expect("extract on space");
    assert!(matches!(sw, TypeSwitch::Whisper(t) if t == "Bob"));
    assert_eq!(rest, "");
    // "/r " loads the last teller only when one exists.
    assert!(parse_type_switch(&state, &chans, "/r hi").is_none());
    state.remember_tell("Ann");
    let (sw, _) = parse_type_switch(&state, &chans, "/r hi").expect("reply with a teller");
    assert!(matches!(sw, TypeSwitch::Whisper(t) if t == "Ann"));
}

#[test]
fn tell_ring_dedups_and_cycles() {
    let mut state = super::edit::ChatEditState::default();
    state.remember_tell("Ann");
    state.remember_tell("Bob");
    state.remember_tell("ann"); // move-to-front dedup, case-insensitive
    assert_eq!(state.last_tell.len(), 2);
    assert_eq!(state.last_tell.front().map(String::as_str), Some("ann"));
    // Tab cycle: current → next, wrapping to the most recent.
    assert_eq!(state.next_tell("ann").as_deref(), Some("Bob"));
    assert_eq!(state.next_tell("Bob").as_deref(), Some("ann"));
    assert_eq!(state.next_tell("").as_deref(), Some("ann"));
}

#[test]
fn action_commands_parse() {
    assert_eq!(
        parse_line("/join world secret", stub_emotes),
        ParsedChat::Join {
            name: "world".into(),
            password: "secret".into(),
        }
    );
    assert_eq!(
        parse_line("/leave world", stub_emotes),
        ParsedChat::Leave {
            name: "world".into()
        }
    );
    assert_eq!(
        parse_line("/chatlist world", stub_emotes),
        ParsedChat::ChatList {
            name: "world".into()
        }
    );
    assert_eq!(
        parse_line("/afk farming", stub_emotes),
        ParsedChat::AfkDnd {
            kind: ChatKind::Afk,
            msg: "farming".into(),
        }
    );
    assert_eq!(
        parse_line("/roll", stub_emotes),
        ParsedChat::Random { min: 1, max: 100 }
    );
    assert_eq!(
        parse_line("/random 50", stub_emotes),
        ParsedChat::Random { min: 1, max: 50 }
    );
    assert_eq!(
        parse_line("/random 2 8", stub_emotes),
        ParsedChat::Random { min: 2, max: 8 }
    );
    assert_eq!(parse_line("/played", stub_emotes), ParsedChat::Played);
    assert_eq!(parse_line("/help", stub_emotes), ParsedChat::Help);
    // /pvp takes no argument (decision 0646 §3): the binding has no state form, so a trailing
    // word is ignored rather than read as a target.
    assert_eq!(parse_line("/pvp", stub_emotes), ParsedChat::Pvp);
    assert_eq!(parse_line("/pvp on", stub_emotes), ParsedChat::Pvp);
    // /r rides its own arm (the reply state lives on ChatEditState).
    assert_eq!(
        parse_line("/r hey", stub_emotes),
        ParsedChat::Reply { text: "hey".into() }
    );
}

#[test]
fn emote_names_fall_through_to_the_dbc_lookup() {
    assert_eq!(parse_line("/wave", stub_emotes), ParsedChat::TextEmote(101));
    assert_eq!(parse_line("/nosuch", stub_emotes), ParsedChat::Unknown);
}

#[test]
fn logout_and_camp_parse() {
    for line in ["/logout", "/camp", "/LOGOUT", "/logout now"] {
        assert_eq!(parse_line(line, stub_emotes), ParsedChat::Logout);
    }
}

#[test]
fn castvis_parses_id_and_phase() {
    use crate::creature_anim::CastEventKind;
    let none = |_: &str| None;
    assert_eq!(
        parse_line("/castvis 133", none),
        ParsedChat::CastVis {
            spell_id: 133,
            kind: CastEventKind::Start
        }
    );
    assert_eq!(
        parse_line("/castvis 133 go", none),
        ParsedChat::CastVis {
            spell_id: 133,
            kind: CastEventKind::Go
        }
    );
    assert_eq!(
        parse_line("/castvis 689 FAIL", none),
        ParsedChat::CastVis {
            spell_id: 689,
            kind: CastEventKind::Fail
        }
    );
    assert_eq!(parse_line("/castvis", none), ParsedChat::Unknown);
    assert_eq!(parse_line("/castvis abc", none), ParsedChat::Unknown);
    assert_eq!(parse_line("/castvis 133 nope", none), ParsedChat::Unknown);
}

#[test]
fn unresolved_slash_line_falls_back_to_emote_lookup() {
    assert_eq!(parse_line("/wave", stub_emotes), ParsedChat::TextEmote(101));
}

#[test]
fn unknown_slash_command_is_dropped_not_said_aloud() {
    // The regression this grammar exists to fix: `/yell` used to literally SAY "/yell hello" —
    // any unresolved slash-line must never fall through to plain chat.
    assert_eq!(parse_line("/dancemove", stub_emotes), ParsedChat::Unknown);
    assert_eq!(parse_line("/frobnicate", stub_emotes), ParsedChat::Unknown);
}

// ── The send-side posture-eligibility gate (`emote_send_eligible`) — the director-verified rows
// from wow-re `emote-posture-gate.md` §3, real `Emotes.dbc` `EmoteFlags` values.
const BOW: u32 = 0x4801;
const RUDE: u32 = 0x0001;
const APPLAUD: u32 = 0x0000;
const CHEER: u32 = 0x0800;
const SALUTE: u32 = 0x0800;
const LAUGH: u32 = 0x0980;

#[test]
fn seated_stand_required_emotes_are_suppressed() {
    assert!(!emote_send_eligible(BOW, 1, false)); // 0x4801 has 0x1 (requires STAND)
    assert!(!emote_send_eligible(RUDE, 1, false));
}

#[test]
fn seated_non_stand_emotes_pass() {
    assert!(emote_send_eligible(APPLAUD, 1, false));
    assert!(emote_send_eligible(CHEER, 1, false));
    assert!(emote_send_eligible(LAUGH, 1, false));
    assert!(emote_send_eligible(SALUTE, 1, false));
}

#[test]
fn swimming_suppresses_only_the_0x80_emotes() {
    assert!(!emote_send_eligible(LAUGH, 0, true)); // 0x0980 has 0x80
    assert!(emote_send_eligible(CHEER, 0, true));
}

#[test]
fn standing_and_dry_everyone_is_eligible() {
    for flags in [BOW, RUDE, APPLAUD, CHEER, SALUTE, LAUGH] {
        assert!(emote_send_eligible(flags, 0, false), "flags {flags:#x}");
    }
}

#[test]
fn unconditional_and_sleep_dead_rules() {
    assert!(!emote_send_eligible(0x0400, 0, false)); // unconditional suppress
    assert!(!emote_send_eligible(0, 3, false)); // SLEEP without the allow bit
    assert!(!emote_send_eligible(0, 7, false)); // DEAD without the allow bit
    assert!(emote_send_eligible(0x0200, 3, false)); // "allowed while asleep/dead"
}
