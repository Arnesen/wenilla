use crate::net::ChatKind;

use super::event::{default_color, ChatEvent, ChatEventKind as K};
use super::frames::compose;
use super::input::{emote_send_eligible, ParsedChat};

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
    // A LOOT line arrives already composed by `ui_loot::receive_line`, item link and all; compose
    // must pass the escapes through untouched (the quality colour is the link's, not the line's).
    assert_eq!(
        compose(
            &ChatEvent::text_only(
                K::Loot,
                "You receive loot: |cffffffff|Hitem:117:0:0:0|h[Tough Jerky]|h|r.".into()
            ),
            K::Loot
        )
        .unwrap(),
        "You receive loot: |cffffffff|Hitem:117:0:0:0|h[Tough Jerky]|h|r."
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
fn exploration_lines_pick_the_reference_form() {
    // ERR_ZONE_EXPLORED (GlobalStrings :1925) — the toast, fired on EVERY exploration packet
    // (UIErrorsFrame); ERR_ZONE_EXPLORED_XP (:1926) — the chat system line that rides
    // additionally iff xp > 0 (byte-verified branch `0x5e422f`; decisions 0828/0829).
    assert_eq!(
        super::feed::exploration_toast("Westfall"),
        "Discovered: Westfall"
    );
    assert_eq!(
        super::feed::exploration_line("Westfall", 85),
        "Discovered Westfall: 85 experience gained"
    );
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

/// The grammar fixture: a command table built from a *stub* of the reference's alias strings, in
/// the same `SLASH_<INDEX><n>` / `EMOTE<i>_CMD<j>` shape the shipped `GlobalStrings.lua` has. The
/// aliases here are fixture data for the ARGUMENT grammar; that the real ones all resolve is
/// [`real_alias_table_resolves_the_shipped_commands`]'s job, on the real files.
fn stub_table() -> super::commands::SlashCommands {
    const STRINGS: &[(&str, &str)] = &[
        ("SLASH_JOIN1", "/join"),
        ("SLASH_LEAVE1", "/leave"),
        ("SLASH_LIST_CHANNEL1", "/chatlist"),
        ("SLASH_CHAT_AFK1", "/afk"),
        ("SLASH_RANDOM1", "/random"),
        ("SLASH_RANDOM2", "/roll"),
        ("SLASH_PLAYED1", "/played"),
        ("SLASH_HELP1", "/help"),
        ("SLASH_PVP1", "/pvp"),
        ("SLASH_REPLY1", "/r"),
        ("SLASH_LOGOUT1", "/logout"),
        ("SLASH_LOGOUT2", "/camp"),
        ("SLASH_QUIT1", "/quit"),
        ("SLASH_TRADE1", "/trade"),
        ("SLASH_SCRIPT1", "/script"),
        // One emote index, in the two-table shape: the alias, and the token it resolves through.
        ("EMOTE1_CMD1", "/wave"),
        ("EMOTE1_CMD2", "/hello"), // an alias that is NOT the token — the 0881 class of bug
        ("EMOTE1_TOKEN", "WAVE"),
    ];
    super::commands::SlashCommands::build(
        |key| {
            STRINGS
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        },
        |token| (token == "WAVE").then_some(101),
    )
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
    let t = stub_table();
    let parse_line = |line: &str| super::input::parse_line(&t, line);
    assert_eq!(
        parse_line("/join world secret"),
        ParsedChat::Join {
            name: "world".into(),
            password: "secret".into(),
        }
    );
    assert_eq!(
        parse_line("/leave world"),
        ParsedChat::Leave {
            name: "world".into()
        }
    );
    assert_eq!(
        parse_line("/chatlist world"),
        ParsedChat::ChatList {
            name: "world".into()
        }
    );
    assert_eq!(
        parse_line("/afk farming"),
        ParsedChat::AfkDnd {
            kind: ChatKind::Afk,
            msg: "farming".into(),
        }
    );
    assert_eq!(parse_line("/roll"), ParsedChat::Random { min: 1, max: 100 });
    assert_eq!(
        parse_line("/random 50"),
        ParsedChat::Random { min: 1, max: 50 }
    );
    assert_eq!(
        parse_line("/random 2 8"),
        ParsedChat::Random { min: 2, max: 8 }
    );
    assert_eq!(parse_line("/played"), ParsedChat::Played);
    assert_eq!(parse_line("/help"), ParsedChat::Help);
    // /pvp takes no argument (decision 0646 §3): the binding has no state form, so a trailing
    // word is ignored rather than read as a target.
    assert_eq!(parse_line("/pvp"), ParsedChat::Pvp);
    assert_eq!(parse_line("/pvp on"), ParsedChat::Pvp);
    // /r rides its own arm (the reply state lives on ChatEditState).
    assert_eq!(
        parse_line("/r hey"),
        ParsedChat::Reply { text: "hey".into() }
    );
}

#[test]
fn emote_aliases_resolve_through_the_table() {
    let t = stub_table();
    let parse_line = |line: &str| super::input::parse_line(&t, line);
    assert_eq!(parse_line("/wave"), ParsedChat::TextEmote(101));
    // The 0881 fix in one line: an alias that is NOT its token's `EmotesText` name resolves too.
    // `/hello` (token WAVE) is the shape `/lol` (token LAUGH) has in the shipped table — before
    // the table, matching on the DBC name alone left 61 such commands unresolvable.
    assert_eq!(parse_line("/hello"), ParsedChat::TextEmote(101));
    // An emote takes an argument (`DoEmote(token, msg)`): the command is the first word only.
    assert_eq!(parse_line("/wave Bob"), ParsedChat::TextEmote(101));
    assert_eq!(parse_line("/nosuch"), ParsedChat::Unknown);
}

#[test]
fn logout_and_camp_parse() {
    let t = stub_table();
    let parse_line = |line: &str| super::input::parse_line(&t, line);
    for line in ["/logout", "/camp", "/LOGOUT", "/logout now"] {
        assert_eq!(parse_line(line), ParsedChat::Logout);
    }
    assert_eq!(parse_line("/quit"), ParsedChat::Quit);
}

#[test]
fn one_line_reference_bodies_run_in_the_vm() {
    let t = stub_table();
    let parse_line = |line: &str| super::input::parse_line(&t, line);
    // `/trade` is the ref's `InitiateTrade("target")`, verbatim.
    assert_eq!(
        parse_line("/trade"),
        ParsedChat::Lua {
            body: "InitiateTrade(\"target\")".into()
        }
    );
    // `/script` runs the typed text AS the chunk (the ref's `RunScript(msg)`); bare is a no-op.
    assert_eq!(
        parse_line("/script Print(\"hi\")"),
        ParsedChat::Lua {
            body: "Print(\"hi\")".into()
        }
    );
    assert_eq!(parse_line("/script"), ParsedChat::Unknown);
}

/// `/castvis` is one of benilla's own instruments, so 1179 gates the whole dev alias table behind
/// `run_mode::dev_affordances()` — in a player build the alias is never claimed and the line falls
/// through to the reference's "unknown command". This test therefore asserts the grammar in a dev
/// build and the *absence* of the grammar in a player one, rather than assuming the configuration
/// it happens to run in. (It assumed, until 1180's `player-tests` gate ran it the other way.)
#[test]
fn castvis_parses_id_and_phase() {
    use crate::creature_anim::CastEventKind;
    let t = stub_table();
    let parse_line = |line: &str| super::input::parse_line(&t, line);
    if !crate::run_mode::dev_affordances() {
        assert_eq!(
            parse_line("/castvis 133"),
            ParsedChat::Unknown,
            "a player build must not claim an instrument's alias"
        );
        return;
    }
    assert_eq!(
        parse_line("/castvis 133"),
        ParsedChat::CastVis {
            spell_id: 133,
            kind: CastEventKind::Start,
            ground: false
        }
    );
    assert_eq!(
        parse_line("/castvis 133 go"),
        ParsedChat::CastVis {
            spell_id: 133,
            kind: CastEventKind::Go,
            ground: false
        }
    );
    // `ground` is a GO too — the pure-dest shape, the only one that flies the location fallback.
    assert_eq!(
        parse_line("/castvis 1543 GROUND"),
        ParsedChat::CastVis {
            spell_id: 1543,
            kind: CastEventKind::Go,
            ground: true
        }
    );
    assert_eq!(
        parse_line("/castvis 689 FAIL"),
        ParsedChat::CastVis {
            spell_id: 689,
            kind: CastEventKind::Fail,
            ground: false
        }
    );
    assert_eq!(parse_line("/castvis"), ParsedChat::Unknown);
    assert_eq!(parse_line("/castvis abc"), ParsedChat::Unknown);
    assert_eq!(parse_line("/castvis 133 nope"), ParsedChat::Unknown);
}

#[test]
fn unknown_slash_command_is_dropped_not_said_aloud() {
    let t = stub_table();
    let parse_line = |line: &str| super::input::parse_line(&t, line);
    // The regression this grammar exists to fix: `/yell` used to literally SAY "/yell hello" —
    // any unresolved slash-line must never fall through to plain chat.
    assert_eq!(parse_line("/dancemove"), ParsedChat::Unknown);
    assert_eq!(parse_line("/frobnicate"), ParsedChat::Unknown);
}

/// The RUNTIME leg on the real data (the `every_mount_key_resolves…` pattern): build the table the
/// way boot does — the shipped `GlobalStrings.lua` and `ChatFrame.lua`'s token table executed into
/// a real VM, joined to the real `EmotesText.dbc` — and assert the commands 0881 was opened for.
/// Skips without client data.
#[test]
fn real_alias_table_resolves_the_shipped_commands() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = benilla_formats::open_chain(&data).expect("open chain");
    let s = benilla_ui::script::UiScript::new().expect("VM");
    for file in ["GlobalStrings.lua", "ChatFrame.lua"] {
        let src = chain
            .read_file(&format!("Interface\\FrameXML\\{file}"))
            .expect("FrameXML file in the chain");
        let src = String::from_utf8_lossy(&src).into_owned();
        // GlobalStrings runs whole (it is only assignments); ChatFrame contributes its token
        // table alone, through the production filter.
        let src = if file == "ChatFrame.lua" {
            src.lines()
                .map(str::trim)
                .filter(|l| crate::ui_script::is_emote_token_line(l))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            src
        };
        s.run(&src).expect("runs clean");
    }
    let cat = benilla_formats::load_emote_sound_catalog(&mut chain).expect("emote catalog");
    let globals = s.lua().globals();
    let table = super::commands::SlashCommands::build(
        |name| globals.get::<String>(name).ok().filter(|v| !v.is_empty()),
        |token| cat.text_id(token),
    );
    let parse_line = |line: &str| super::input::parse_line(&table, line);

    // The reported symptom: `/sit` resolves to the SIT text emote (EmotesText id 86), whose
    // `Emotes.dbc` row (13, STATE_SIT) is the posture emote that sets stand state 1.
    assert_eq!(parse_line("/sit"), ParsedChat::TextEmote(86));
    assert_eq!(
        cat.text_emote(86).and_then(|e| cat.posture_state(e)),
        Some(1)
    );
    // Every posture command the shipped tables expose, with the state it sets.
    for (line, state) in [
        ("/stand", 0),
        ("/sit", 1),
        ("/sleep", 3),
        ("/liedown", 3),
        ("/kneel", 8),
    ] {
        let ParsedChat::TextEmote(text_id) = parse_line(line) else {
            panic!("{line} is an emote");
        };
        let posture = cat.text_emote(text_id).and_then(|e| cat.posture_state(e));
        assert_eq!(posture, Some(state), "{line} sets stand state {state}");
    }
    // The dead-alias class 0881 found: an alias that differs from its token's DBC name. These all
    // returned "Type '/help'…" before the table.
    for line in [
        "/lol",
        "/hi",
        "/ty",
        "/thanks",
        "/congrats",
        "/sorry",
        "/yes",
        "/bravo",
        "/weep",
        "/goodbye",
        "/pizza",
        "/strong",
    ] {
        assert!(
            matches!(parse_line(line), ParsedChat::TextEmote(_)),
            "{line} resolves to an emote"
        );
    }
    // …and the three names the reference has NO command for, which the DBC-name match used to
    // accept as emotes.
    for line in ["/joke", "/puzzle", "/attackmytarget"] {
        assert_eq!(parse_line(line), ParsedChat::Unknown, "{line}");
    }
    // `/follow` was the sharp one in that class: the DBC-name match fired a text emote where the
    // real client's `SlashCmdList["FOLLOW"]` follows your target. 0881 made it honestly unknown;
    // 0890 makes it the real command, over all three shipped aliases (SLASH_FOLLOW1-6 → `/f`,
    // `/follow`, `/fol`).
    for line in ["/follow", "/f", "/fol"] {
        assert_eq!(
            parse_line(line),
            ParsedChat::Follow { name: None },
            "{line}"
        );
    }
    assert_eq!(
        parse_line("/follow Probeone"),
        ParsedChat::Follow {
            name: Some("Probeone".into())
        }
    );
    // A command whose handler benilla does not register answers like any unknown command.
    assert_eq!(parse_line("/ginvite"), ParsedChat::Unknown);
    // The by-name selection pair (decision 0886) — every shipped alias, and the whole-argument
    // grammar that makes a multi-word creature name ONE name. `/tar` and `/a` are the short forms
    // the shipped strings carry (SLASH_TARGET2/4, SLASH_ASSIST1/3).
    assert_eq!(
        parse_line("/target Kobold Vermin"),
        ParsedChat::Target {
            name: Some("Kobold Vermin".into())
        },
        "the argument is trimmed WHOLE — `GetSlashCmdTarget`'s gsub, not a first-word split"
    );
    assert_eq!(
        parse_line("/tar   Hogger  "),
        ParsedChat::Target {
            name: Some("Hogger".into())
        }
    );
    assert_eq!(parse_line("/target"), ParsedChat::Target { name: None });
    assert_eq!(
        parse_line("/a Bob"),
        ParsedChat::Assist {
            name: Some("Bob".into())
        }
    );
    assert_eq!(parse_line("/assist"), ParsedChat::Assist { name: None });
    // The macro family (decision 0983): `/cast` (with its `/spell` alias — SLASH_CAST1-4 spell two
    // distinct strings across four slots) runs the ref's own one-line body `CastSpellByName(msg)`;
    // `/macro`/`/m` open the window; `/macrohelp` prints the shipped five lines.
    assert_eq!(
        parse_line("/cast Fireball(Rank 1)"),
        ParsedChat::Lua {
            body: "CastSpellByName(\"Fireball(Rank 1)\")".into()
        }
    );
    assert_eq!(
        parse_line("/spell Frostbolt"),
        ParsedChat::Lua {
            body: "CastSpellByName(\"Frostbolt\")".into()
        }
    );
    assert_eq!(
        parse_line("/cast"),
        ParsedChat::Unknown,
        "a bare /cast is the ref's own no-op (`if msg ~= \"\"`)"
    );
    for line in ["/macro", "/m"] {
        assert_eq!(
            parse_line(line),
            ParsedChat::Lua {
                body: "ShowMacroFrame()".into()
            },
            "{line}"
        );
    }
    assert_eq!(parse_line("/macrohelp"), ParsedChat::MacroHelp);
    // The whole shipped surface, so a table that half-loaded fails loudly: **225 distinct emote
    // commands** over the 169 `EmotesText` names (the strings repeat — `EMOTE87_CMD1` and `_CMD2`
    // are both "/sit" — and EMOTE27 "UNUSED" has no row, so it contributes none), and **67 distinct
    // aliases** across the 34 registered `SlashCmdList` indices (0886 added TARGET's `/target`
    // `/tar` and ASSIST's `/assist` `/a` to 0881's 55; 0890 added FOLLOW's `/f` `/follow` `/fol`;
    // 0983 added CAST's `/cast` `/spell`, MACRO's `/macro` `/m`, and MACROHELP's `/macrohelp`).
    //
    // The third number is the **seam** (decision 1179): benilla's own instrument commands
    // (`/castvis` `/chattest` `/partytest` `/shot` `/liquid` `/reaction` `/react` — 7 aliases over 6
    // commands) are registered only when `run_mode::dev_affordances()`, so a player build claims
    // none of them and `/partytest` falls through to the reference's "unknown command". Asserted
    // against the predicate rather than a literal, so the row states the rule in both builds.
    let instruments = if crate::run_mode::dev_affordances() {
        7
    } else {
        0
    };
    assert_eq!(
        table.counts(),
        (67, 225, instruments),
        "(slash, emote, instrument) aliases"
    );
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
