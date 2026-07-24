//! The **submitted-line** side of the chat (decision 0288 P5): [`drain_chat_input`] routes each
//! Entered line — a plain line sends as the box's CURRENT type (the edit machine's law,
//! [`super::edit`]), a `/`-line runs the Enter-path type switch, the reply arm, the action
//! grammar ([`parse_line`] → [`ParsedChat`]: `/join`-family, `/afk`, `/random`, `/played`,
//! `/help`, `/wave`-style emotes with the send-side posture gate [`emote_send_eligible`], and
//! the dev commands). Opening keys + live parsing live in [`super::edit`]; the inbound half is
//! [`super::feed`]. Own sends are never echoed locally — the wire echoes back (vanilla behavior).

use bevy::prelude::*;

use crate::creature_anim::{move_flags, MovementState};
use crate::net::{ChatKind, ClientCommand, NetCommands, SelfPlayer};
use crate::target::Selection;

/// Send `text` as the box's CURRENT type (`ChatEdit_SendText`): whisper/channel targets ride
/// along; a sent whisper remembers its target (`ChatEdit_SetLastToldTarget`); a sticky type
/// commits (`ChatEdit_OnEnterPressed`'s `stickyType = type`).
fn send_current(state: &mut super::edit::ChatEditState, commands: &NetCommands, text: String) {
    use super::edit::SendType;
    if text.trim().is_empty() {
        if state.chat_type.sticky() {
            state.sticky = state.chat_type;
        }
        return;
    }
    let target = match state.chat_type {
        SendType::Whisper => Some(state.tell_target.clone()),
        SendType::Channel => Some(state.channel_target.clone()),
        _ => None,
    };
    if state.chat_type == SendType::Whisper {
        state.last_told = Some(state.tell_target.clone());
    }
    if state.chat_type.sticky() {
        state.sticky = state.chat_type;
    }
    let cmd = ClientCommand::Chat {
        kind: state.chat_type.wire(),
        target,
        text,
    };
    match commands.0.send(cmd) {
        Ok(()) => {}
        Err(_) => warn!("chat: not connected; line dropped"),
    }
}

/// The canonical recallable form of a submitted line — the ref's `ChatEdit_AddHistory`
/// (ChatFrame.lua 1916-1938: `SLASH_<type>1`, plus the whisper target / the channel number,
/// then the typed text) — so a recalled line re-Entered reproduces the send even after the
/// box's mode has moved on. A typed slash line recalls exactly as typed; that also stores
/// command lines (`/join …`), which the ref never recalls — a deliberate, useful divergence
/// (decision 0301).
fn history_line(state: &super::edit::ChatEditState, msg: &str) -> String {
    use super::edit::SendType;
    if msg.starts_with('/') {
        return msg.to_string();
    }
    match state.chat_type {
        SendType::Whisper => format!("/w {} {}", state.tell_target, msg),
        SendType::Channel => format!("/{} {}", state.channel_number, msg),
        t => match t.canonical_slash() {
            Some(a) => format!("/{a} {msg}"),
            None => msg.to_string(), // the leader types: no 1.12 slash — recall raw
        },
    }
}

#[allow(clippy::too_many_arguments)] // a Bevy system's param list IS its dependency set
/// The target half of the ref's `GetSlashCmdTarget` (ChatFrame.lua:650-658): a bare party
/// command falls back to the current selection iff it's a PLAYER; anything else is `None` (the
/// ref's silent no-op). The name is cache-resolved — a streamed player target is always cached.
fn target_player_name(
    selection: &Selection,
    names: &mut crate::names::NameCache,
    commands: &NetCommands,
) -> Option<String> {
    let guid = selection.guid?;
    if !benilla_protocol::guid::is_player(guid) {
        return None;
    }
    names.resolve(guid, commands).map(str::to_string)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn drain_chat_input(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut chat_log: ResMut<super::feed::ChatLog>,
    mut state: ResMut<super::edit::ChatEditState>,
    channels: Res<super::edit::ChannelState>,
    commands: Res<NetCommands>,
    emotes: Option<Res<crate::sound::EmoteSounds>>,
    selection: Res<Selection>,
    // The party slash commands (decision 0434): the roster for /promote's name→guid resolve, the
    // name cache for the bare-command player-target fallback (`GetSlashCmdTarget`).
    mut group: ResMut<crate::ui_party::GroupState>,
    mut names: ResMut<crate::names::NameCache>,
    // Our own live stand-state + move flags for the posture-eligibility gate (the pending-aware
    // component the player controller writes each frame); the entity doubles as `/castvis`'s
    // fallback subject.
    self_player: Query<(Entity, &MovementState, &GlobalTransform), With<SelfPlayer>>,
    mut cast_events: MessageWriter<crate::creature_anim::CastEvent>,
    mut play_seq: ResMut<crate::creature_anim::PlaySeq>,
    mut go_targets: MessageWriter<crate::creature_anim::SpellGoTargets>,
    world_camera: Query<&GlobalTransform, (With<crate::player::WorldCamera>, Without<SelfPlayer>)>,
    clock: Option<Res<crate::lighting::GameClock>>,
) {
    let Some(mut script) = script else {
        return;
    };
    for raw in script.take_chat_input() {
        let msg = raw.trim();
        if msg.is_empty() {
            continue; // empty Enter = cancel (sticky already committed on prior sends)
        }
        // Every non-blank submit becomes recallable (Up/Down) in its canonical form — the ref's
        // `ChatEdit_AddHistory` slot. Plain lines canonicalize from the same state the send
        // below uses, so this pre-branch placement equals the ref's post-parse one.
        script.editbox_add_history("ChatFrameEditBox", &history_line(&state, msg));
        // A plain line sends as the box's CURRENT type — the whole point of the edit machine
        // (the v1 always-SAY default dies here).
        if !msg.starts_with('/') {
            send_current(&mut state, &commands, msg.to_string());
            continue;
        }
        // A slash line typed whole + Entered (never live-parsed — no trailing space): the type
        // switch still applies on the send path (`ChatEdit_ParseText(send=1)` runs the same
        // conversion first), then the remainder sends as the new type.
        if let Some((switch, remainder)) = parse_enter_type_switch(&channels, msg) {
            match switch {
                super::edit::TypeSwitch::Plain(t) => state.chat_type = t,
                super::edit::TypeSwitch::Whisper(target) => {
                    state.chat_type = super::edit::SendType::Whisper;
                    state.tell_target = target;
                }
                super::edit::TypeSwitch::Channel { name, number } => {
                    state.chat_type = super::edit::SendType::Channel;
                    state.channel_target = name;
                    state.channel_number = number;
                }
            }
            state.header_dirty = true;
            send_current(&mut state, &commands, remainder);
            continue;
        }
        let resolve_emote = |name: &str| emotes.as_deref().and_then(|e| e.text_id(name));
        match parse_line(msg, resolve_emote) {
            ParsedChat::Reply { text } => match state.last_tell.front().cloned() {
                Some(target) => {
                    state.chat_type = super::edit::SendType::Whisper;
                    state.tell_target = target;
                    state.header_dirty = true;
                    send_current(&mut state, &commands, text);
                }
                None => {
                    // ERR_NO_REPLY_TARGET (GlobalStrings:1748).
                    chat_log.push_event(super::event::ChatEvent::text_only(
                        super::event::ChatEventKind::System,
                        "You have nobody to reply to yet.".to_string(),
                    ));
                }
            },
            ParsedChat::Join { name, password } => {
                let _ = commands
                    .0
                    .send(ClientCommand::JoinChannel { name, password });
            }
            ParsedChat::Leave { name } => {
                let _ = commands.0.send(ClientCommand::LeaveChannel { name });
            }
            ParsedChat::ChatList { name } => {
                let _ = commands.0.send(ClientCommand::ChannelList { name });
            }
            ParsedChat::AfkDnd { kind, msg } => {
                let _ = commands.0.send(ClientCommand::Chat {
                    kind,
                    target: None,
                    text: msg,
                });
            }
            ParsedChat::Random { min, max } => {
                let _ = commands.0.send(ClientCommand::RandomRoll { min, max });
            }
            ParsedChat::Played => {
                let _ = commands.0.send(ClientCommand::PlayedTime);
            }
            ParsedChat::Shot => {
                // The framing instrument (decision 0600): the CURRENT camera pose, in the raw WoW
                // coords a capture `Scenario` takes, echoed to chat and appended to
                // `config_base()`/shots.txt so a chosen spot survives the session.
                let Ok(cam) = world_camera.single() else {
                    continue;
                };
                let (_, rot, eye_bevy) = cam.to_scale_rotation_translation();
                let eye = benilla_assets::coords::bevy_to_wow(eye_bevy);
                let look = benilla_assets::coords::bevy_to_wow(eye_bevy + rot * Vec3::NEG_Z * 50.0);
                let minute = clock.as_deref().map(|c| c.minute).unwrap_or(720);
                let snippet = format!(
                    "eye: [{:.1}, {:.1}, {:.1}], look: [{:.1}, {:.1}, {:.1}], minute: {minute}",
                    eye[0], eye[1], eye[2], look[0], look[1], look[2]
                );
                chat_log.push_event(super::event::ChatEvent::text_only(
                    super::event::ChatEventKind::System,
                    format!("shot: {snippet}"),
                ));
                if let Some(base) = crate::login::config_base() {
                    let _ = std::fs::create_dir_all(&base);
                    let path = base.join("shots.txt");
                    let line = format!("{snippet}\n");
                    let appended = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
                    if appended.is_ok() {
                        chat_log.push_event(super::event::ChatEvent::text_only(
                            super::event::ChatEventKind::System,
                            format!("shot: appended to {}", path.display()),
                        ));
                    }
                }
            }
            ParsedChat::PartyTest { arg } => match arg.as_str() {
                "off" => group.clear_session(),
                "invite" => group.pending_invite = Some("Partner".to_string()),
                // Serverless mark eyeball: skull the current target on the LOCAL board (the
                // real send round-trips through the server's echo, which /partytest lacks).
                "mark" => {
                    if let Some(guid) = selection.guid {
                        group.apply_raid_target(7, guid);
                    }
                }
                arg => {
                    // The live position seats the synthetic members' blips around us.
                    let player_xy = self_player.single().ok().map(|(_, _, tf)| {
                        let w = benilla_assets::coords::bevy_to_wow(tf.translation());
                        (w[0], w[1])
                    });
                    for line in crate::ui_party::synthetic_roster(&mut group, player_xy) {
                        chat_log.push_event(super::event::ChatEvent::text_only(
                            super::event::ChatEventKind::System,
                            line,
                        ));
                    }
                    if arg == "lead" {
                        // The leader-view variant: an unmatched leader guid resolves to
                        // leader_index 0 in the feed — "we lead" — so the leader-only popup
                        // rows (promote/uninvite/the loot submenus) are eyeballable serverless.
                        group.leader = 0xF000;
                    }
                }
            },
            ParsedChat::Invite { name } => {
                if let Some(name) =
                    name.or_else(|| target_player_name(&selection, &mut names, &commands))
                {
                    let _ = commands.0.send(ClientCommand::GroupInvite { name });
                }
            }
            ParsedChat::Uninvite { name } => {
                if let Some(name) =
                    name.or_else(|| target_player_name(&selection, &mut names, &commands))
                {
                    let _ = commands.0.send(ClientCommand::GroupUninvite { name });
                }
            }
            ParsedChat::Promote { name } => {
                if let Some(name) =
                    name.or_else(|| target_player_name(&selection, &mut names, &commands))
                {
                    // The 1.12 wire form is a guid (CMSG_GROUP_SET_LEADER) — resolve against the
                    // roster; a miss answers with the server's own would-be error string
                    // (INTERIM: the ref's PromoteByName miss behavior is a 0434-dispatch item).
                    match group
                        .members
                        .iter()
                        .find(|m| m.name.eq_ignore_ascii_case(&name))
                    {
                        Some(m) => {
                            let _ = commands
                                .0
                                .send(ClientCommand::GroupSetLeader { guid: m.guid });
                        }
                        None => {
                            // ERR_TARGET_NOT_IN_GROUP_S (GlobalStrings:1861).
                            chat_log.push_event(super::event::ChatEvent::text_only(
                                super::event::ChatEventKind::System,
                                format!("{name} is not in your party."),
                            ));
                        }
                    }
                }
            }
            ParsedChat::Help => {
                // An honest benilla summary (the ref's HELP_TEXT_LINE pages are a settings-era
                // nicety; this stays useful and never stale-quotes them).
                for line in [
                    "Chat: /s /y /p /g /o /raid /rw /bg, /w <name>, /r, /e",
                    "Channels: /join <name> [pw], /leave <name>, /chatlist <name>",
                    "Party: /invite /uninvite /promote [name — bare uses your target]",
                    "Misc: /afk, /dnd, /random [min] [max], /played, /shot, /logout",
                ] {
                    chat_log.push_event(super::event::ChatEvent::text_only(
                        super::event::ChatEventKind::System,
                        line.to_string(),
                    ));
                }
            }
            ParsedChat::TextEmote(text_id) => {
                // The posture-eligibility gate (`emote_send_eligible`): a chat-only text emote (no
                // Emotes.dbc row) has no EmoteFlags to test and always sends.
                let (stand_state, flags) = self_player
                    .single()
                    .map_or((0, 0), |(_, m, _)| (m.stand_state, m.flags));
                let swimming = flags & move_flags::SWIMMING != 0;
                let eligible = match emotes.as_deref() {
                    Some(e) => e
                        .text_emote(text_id)
                        .and_then(|emote_id| e.emote_flags(emote_id))
                        .is_none_or(|f| emote_send_eligible(f, stand_state, swimming)),
                    None => true,
                };
                if eligible {
                    let target = selection.guid.unwrap_or(0);
                    match commands
                        .0
                        .send(ClientCommand::TextEmote { text_id, target })
                    {
                        Ok(()) => info!("chat: sent {msg:?}"),
                        Err(_) => warn!("chat: not connected; dropped {msg:?}"),
                    }
                } else {
                    // Byte-verified whole-send suppression: a seated /bow sends nothing (no packet,
                    // no anim) — the client's DoEmote returns before building the packet.
                    debug!(
                        "chat: emote {text_id} suppressed by posture gate (seated/asleep/swimming)"
                    );
                }
            }
            ParsedChat::CastVis { spell_id, kind } => {
                // The dev instrument: fire the synthesized cast edge at the selection, else self —
                // the same message the net bridge writes, so it exercises the whole resolve/hold/
                // release path (decision 0099's iteration loop).
                let me = self_player.single().ok().map(|(e, _, _)| e);
                let subject = selection.target.or(me);
                match subject {
                    Some(entity) => {
                        info!("castvis: spell {spell_id} {kind:?} on {entity}");
                        cast_events.write(crate::creature_anim::CastEvent {
                            entity,
                            spell_id,
                            kind,
                            seq: play_seq.next(),
                        });
                        // A selected caster's GO also fires its target list at *us* — the one
                        // live unit the dev loop always has — so a Speed>0 spell's missile and
                        // impact run end to end (0099 phase 4). A self-cast has no target: no
                        // missile, matching a hit-less GO.
                        if kind == crate::creature_anim::CastEventKind::Go {
                            if let Some(me) = me.filter(|&m| m != entity) {
                                go_targets.write(crate::creature_anim::SpellGoTargets {
                                    caster: entity,
                                    spell_id,
                                    hits: vec![me],
                                    misses: Vec::new(),
                                    // Dev stand-in only: a real GO's ammo block carries the
                                    // caster's actual ammo; `/castvis` has none, so shot
                                    // spells (`75 go`, `2480 go`) fly the Rough Arrow
                                    // display (5996).
                                    ammo_display_id: Some(5996),
                                    seq: play_seq.next(),
                                });
                            }
                        }
                    }
                    None => warn!("castvis: no selection and no self avatar — dropped"),
                }
            }
            ParsedChat::ChatTest => chattest_battery(&mut chat_log),
            ParsedChat::Logout => match commands.0.send(ClientCommand::Logout) {
                Ok(()) => info!("chat: logout requested"),
                Err(_) => warn!("chat: not connected; dropped {msg:?}"),
            },
            ParsedChat::Unknown => {
                // HELP_TEXT_SIMPLE (the ref's unknown-command reply, ChatEdit_ParseText l.2203).
                chat_log.push_event(super::event::ChatEvent::text_only(
                    super::event::ChatEventKind::System,
                    "Type '/help' for a listing of a few commands.".to_string(),
                ));
            }
        }
    }
}

/// What a submitted, already-trimmed chat line resolves to (see [`parse_line`]). Kept separate from
/// [`ClientCommand`] so the parser is unit-testable without a live [`crate::sound::EmoteSounds`]
/// resource or a network channel.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ParsedChat {
    /// `/r [text]` — reply to the last received tell ([`super::edit::ChatEditState::last_tell`]).
    Reply { text: String },
    /// `/join <name> [password]` (aliases /channel /chan — SLASH_JOIN).
    Join { name: String, password: String },
    /// `/leave <name>` (aliases /chatleave /chatexit).
    Leave { name: String },
    /// `/chatlist <name>` (aliases /chatwho /chatinfo) — the member roster ask.
    ChatList { name: String },
    /// `/afk [msg]` / `/dnd [msg]` — the away toggles (CHAT_MSG_AFK/DND sends).
    AfkDnd { kind: ChatKind, msg: String },
    /// `/random [min] [max]` (aliases /rand /rnd /roll): bare = 1-100, one number = 1-N.
    Random { min: u32, max: u32 },
    /// `/played` — CMSG_PLAYED_TIME.
    Played,
    /// `/shot` — the director's framing instrument (decision 0600): dump the CURRENT world-camera
    /// pose as a ready-to-paste capture `Scenario` (raw WoW eye/look + the rendered game minute)
    /// into chat and `config_base()`/shots.txt. Client-local, no wire traffic — how a chosen spot
    /// travels from the director's eye to the golden set as exact numbers.
    Shot,
    /// `/help` (aliases /h /?) — the command summary.
    Help,
    /// A `/name` line that resolved in `EmotesText` (`/wave` → text id 101) — sent as
    /// `CMSG_TEXT_EMOTE` targeted at the current selection.
    TextEmote(u32),
    /// The `/castvis` **dev instrument** (decision 0099 phase 2): synthesize a cast edge locally
    /// — the exact [`crate::creature_anim::CastEvent`] the wire would produce — on the selection
    /// (else self), no server round-trip. Runtime-available like every current instrument
    /// (`ui_pass::demo_enabled`'s note: decision 0026's compile-time `dev` feature is the recorded
    /// target seam, not yet built); it moves behind that feature when 0026 phase 1 lands.
    CastVis {
        spell_id: u32,
        kind: crate::creature_anim::CastEventKind,
    },
    /// `/logout` — leave the world back to character select (`CMSG_LOGOUT_REQUEST`, decision 0193);
    /// the vanilla client's own command. The confirmed round-trip (`SMSG_LOGOUT_COMPLETE`) flips
    /// [`crate::char_select::ClientState`] back to the roster screen.
    Logout,
    /// `/chattest` — the 0288 instrument: pump one synthetic line of every kind/notice/link form
    /// through the REAL router+composer, so the whole surface is eyeballable in one screen with
    /// no server choreography. Dev-runtime like `/castvis` (decision 0026's compile-time gate is
    /// the recorded target seam).
    ChatTest,
    /// `/invite` `/inv` `/i` — party invite by name (`CMSG_GROUP_INVITE`). `None` = bare, which
    /// falls back to the selected PLAYER's name (the ref's `GetSlashCmdTarget` law,
    /// ChatFrame.lua:650-658 — no name and no player target is a silent no-op).
    Invite { name: Option<String> },
    /// `/uninvite` `/un` `/u` `/kick` — kick by name (`CMSG_GROUP_UNINVITE`), same
    /// name-or-target law.
    Uninvite { name: Option<String> },
    /// `/promote` `/pr` — hand leadership over (`CMSG_GROUP_SET_LEADER`), same name-or-target
    /// law; the 1.12 wire takes a guid, resolved against the roster at dispatch.
    Promote { name: Option<String> },
    /// `/partytest [lead|invite|mark|off]` — the party-frame dev instrument (decision 0434, the
    /// `/chattest` pattern): a synthetic roster through the real apply path (`lead` = the same
    /// roster with US leading, for the leader-only popup rows), a fake pending invite for the
    /// popup, a local skull on the current target (the mark renders without a server echo), or
    /// clear. While the roster is synthetic the drain runs in SANDBOX mode
    /// ([`crate::ui_party::GroupState::test`]): the popup's group-mutating rows — promote,
    /// kick, leave, loot settings, raid marks — apply to the local mirror, so the whole menu
    /// surface is exercisable serverless.
    PartyTest { arg: String },
    /// A slash line matching neither a chat command nor an `EmotesText` name — dropped.
    Unknown,
}

/// The name half of the ref's `GetSlashCmdTarget` (ChatFrame.lua:650-658): a non-empty argument
/// is the name (first word — 1.12 names carry no spaces); empty defers to the dispatch's
/// player-target fallback.
fn slash_target_name(args: &str) -> Option<String> {
    let name = args.split_whitespace().next().unwrap_or("");
    (!name.is_empty()).then(|| name.to_string())
}

/// Parse a trimmed slash line into an ACTION (`/join`, `/afk`, `/random`, `/wave`, the dev
/// commands…). The chat-TYPE commands (`/say`-family, `/w`, `/r`) never reach here on the send
/// path — [`parse_enter_type_switch`] and the reply arm consume them first; a plain no-slash
/// line sends as the box's current type ([`drain_chat_input`]). A non-slash line reaching this
/// fn (unit tests only) is [`ParsedChat::Unknown`].
pub(super) fn parse_line(line: &str, resolve_emote: impl Fn(&str) -> Option<u32>) -> ParsedChat {
    let Some(rest) = line.strip_prefix('/') else {
        return ParsedChat::Unknown;
    };
    let rest = rest.trim();
    let (cmd, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let args = args.trim();
    // The dev instrument's grammar: `/castvis <spell_id> [go|fail]` — bare = Start (the precast
    // hold begins), `go` = the release, `fail` = the reap. Malformed → Unknown (dropped loudly).
    if cmd.eq_ignore_ascii_case("castvis") {
        use crate::creature_anim::CastEventKind;
        let mut words = args.split_whitespace();
        let Some(spell_id) = words.next().and_then(|w| w.parse::<u32>().ok()) else {
            return ParsedChat::Unknown;
        };
        let kind = match words.next() {
            None => CastEventKind::Start,
            Some(w) if w.eq_ignore_ascii_case("go") => CastEventKind::Go,
            Some(w) if w.eq_ignore_ascii_case("fail") => CastEventKind::Fail,
            Some(_) => return ParsedChat::Unknown,
        };
        return ParsedChat::CastVis { spell_id, kind };
    }
    // `/logout` (also its vanilla alias `/camp`): args are ignored, like the real client.
    if cmd.eq_ignore_ascii_case("logout") || cmd.eq_ignore_ascii_case("camp") {
        return ParsedChat::Logout;
    }
    if cmd.eq_ignore_ascii_case("chattest") {
        return ParsedChat::ChatTest;
    }
    if cmd.eq_ignore_ascii_case("partytest") {
        return ParsedChat::PartyTest {
            arg: args.to_ascii_lowercase(),
        };
    }
    match cmd.to_ascii_lowercase().as_str() {
        "r" | "reply" => ParsedChat::Reply {
            text: args.to_string(),
        },
        "join" | "channel" | "chan" => {
            let (name, password) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
            if name.is_empty() {
                // CHAT_JOIN_HELP (the ref's bare-/join reply) rides the Unknown help line.
                ParsedChat::Unknown
            } else {
                ParsedChat::Join {
                    name: name.to_string(),
                    password: password.trim().to_string(),
                }
            }
        }
        "leave" | "chatleave" | "chatexit" => {
            let name = args.split_whitespace().next().unwrap_or("");
            if name.is_empty() {
                ParsedChat::Unknown
            } else {
                ParsedChat::Leave {
                    name: name.to_string(),
                }
            }
        }
        "chatlist" | "chatwho" | "chatinfo" => {
            let name = args.split_whitespace().next().unwrap_or("");
            if name.is_empty() {
                ParsedChat::Unknown // the bare joined-channel listing is P6's (needs the list)
            } else {
                ParsedChat::ChatList {
                    name: name.to_string(),
                }
            }
        }
        "afk" => ParsedChat::AfkDnd {
            kind: ChatKind::Afk,
            msg: args.to_string(),
        },
        "dnd" => ParsedChat::AfkDnd {
            kind: ChatKind::Dnd,
            msg: args.to_string(),
        },
        "random" | "rand" | "rnd" | "roll" => {
            let mut nums = args
                .split_whitespace()
                .filter_map(|w| w.parse::<u32>().ok());
            let (a, b) = (nums.next(), nums.next());
            let (min, max) = match (a, b) {
                (Some(a), Some(b)) => (a, b),
                (Some(a), None) => (1, a),
                _ => (1, 100),
            };
            ParsedChat::Random { min, max }
        }
        "played" => ParsedChat::Played,
        "shot" => ParsedChat::Shot,
        // The party membership commands (decision 0434; aliases quoted from GlobalStrings'
        // SLASH_INVITE/UNINVITE/PROMOTE 1-8, lines 3660-3780).
        "i" | "inv" | "invite" => ParsedChat::Invite {
            name: slash_target_name(args),
        },
        "u" | "un" | "kick" | "uninvite" => ParsedChat::Uninvite {
            name: slash_target_name(args),
        },
        "pr" | "promote" => ParsedChat::Promote {
            name: slash_target_name(args),
        },
        "h" | "help" | "?" => ParsedChat::Help,
        // Not a recognized command — the EmotesText lookup against the whole slash-line
        // (`/wave`, `/lol`, …), exactly as before.
        _ => resolve_emote(rest).map_or(ParsedChat::Unknown, ParsedChat::TextEmote),
    }
}

/// The Enter-path type switch (`ChatEdit_ParseText(send=1)` runs the same conversion the live
/// parse does, but without the live parse's trailing-space requirement — "/g hi" + Enter
/// converts and sends in one stroke; "/g" alone converts and commits the sticky).
pub(super) fn parse_enter_type_switch(
    channels: &super::edit::ChannelState,
    text: &str,
) -> Option<(super::edit::TypeSwitch, String)> {
    use super::edit::{SendType, TypeSwitch};
    let rest = text.strip_prefix('/')?;
    let (cmd, args) = rest.split_once(' ').unwrap_or((rest, ""));
    let lower = cmd.to_ascii_lowercase();
    // `/2 hi` and `/c world hi` convert-and-send on the enter path too.
    if let Some(switch) = super::edit::channel_switch(channels, &lower, args) {
        return Some(switch);
    }
    if ["w", "whisper", "t", "tell", "send"].contains(&lower.as_str()) {
        let (target, remainder) = args.split_once(' ').unwrap_or((args, ""));
        if target.is_empty() || target.starts_with('|') || remainder.trim().is_empty() {
            return None; // needs a name AND a message on the enter path
        }
        return Some((
            TypeSwitch::Whisper(target.to_string()),
            remainder.to_string(),
        ));
    }
    let t = match lower.as_str() {
        "s" | "say" => SendType::Say,
        "y" | "yell" | "sh" | "shout" => SendType::Yell,
        "e" | "em" | "emote" | "me" => SendType::Emote,
        "p" | "party" => SendType::Party,
        "raid" | "ra" | "rsay" => SendType::Raid,
        "rw" => SendType::RaidWarning,
        "g" | "gc" | "gu" | "guild" => SendType::Guild,
        "o" | "osay" => SendType::Officer,
        "bg" | "battleground" => SendType::Battleground,
        _ => return None,
    };
    Some((TypeSwitch::Plain(t), args.to_string()))
}

/// `/chattest` (the 0288 instrument): one synthetic line of every renderable form through the
/// real event pipeline — kinds, flags, the language header, channel prefixes, notices, and both
/// link forms (item + player), so formats/colors/links verify in one screen.
fn chattest_battery(log: &mut super::feed::ChatLog) {
    use super::event::{ChatEvent, ChatEventKind as K};
    let player = |kind: K, text: &str, sender: &str| {
        let mut e = ChatEvent::text_only(kind, text.into());
        e.sender = sender.into();
        e
    };
    let mut battery: Vec<ChatEvent> = vec![
        player(K::Say, "the quick brown fox — say white.", "Testa"),
        player(K::Yell, "yell red!", "Testa"),
        player(
            K::Whisper,
            "whisper pink (chime + flash if Combat Log is selected).",
            "Testa",
        ),
        player(K::WhisperInform, "the To-echo.", "Testa"),
        player(K::Emote, "dances — bare name, orange.", "Testa"),
        player(K::Party, "party blue.", "Testa"),
        player(K::Guild, "guild green.", "Testa"),
        player(K::Officer, "officer deep green.", "Testa"),
        player(K::RaidWarning, "raid warning salmon.", "Testa"),
        player(K::MonsterSay, "monster say pale yellow.", "Grunt"),
        player(K::MonsterYell, "monster yell red.", "Grunt"),
        player(K::MonsterWhisper, "monster whisper gray.", "Grunt"),
        player(K::MonsterEmote, "%s looks around — emote orange.", "Grunt"),
        ChatEvent::text_only(K::System, "system yellow.".into()),
        ChatEvent::text_only(
            K::Skill,
            "Your skill in Testing has increased to 300.".into(),
        ),
        ChatEvent::text_only(
            K::Loot,
            "You receive loot: |cff1eff00|Hitem:2000:0:0:0|h[Test Blade]|h|r — click me.".into(),
        ),
        ChatEvent::text_only(K::Money, "You loot 1 Gold, 23 Silver, 45 Copper.".into()),
    ];
    // A GM-flagged line, an Orcish header, a numbered channel line, and the join/kick notices.
    let mut gm = player(K::Say, "a GM-tagged line.", "Testa");
    gm.flag = "GM".into();
    battery.push(gm);
    let mut orc = player(K::Say, "an Orcish-headered line.", "Grunk");
    orc.language = "Orcish".into();
    battery.push(orc);
    let mut chan = player(K::Channel, "channel pink.", "Testa");
    chan.channel = "1. General - Elwynn Forest".into();
    battery.push(chan);
    let mut join = player(K::ChannelJoin, "", "Testa");
    join.channel = "1. General - Elwynn Forest".into();
    battery.push(join);
    let mut notice = ChatEvent::text_only(K::ChannelNotice, String::new());
    notice.channel = "General - Elwynn Forest".into();
    notice.notice = "2".into(); // YOU_JOINED
    battery.push(notice);
    for e in battery {
        log.push_event(e);
    }
    info!("chattest: battery queued");
}

/// The send-side emote **posture-eligibility gate** (wow-re `object-layer/scratch/emote-posture-
/// gate.md`, commit `f9584b45`, §0): the real client's `CheckEmoteEligible` (`0x47db40`), the *only*
/// site that reads an `Emotes.dbc` `EmoteFlags` — called from `DoEmote` (`0x5ef560`) *before*
/// `CMSG_TEXT_EMOTE` is built, so a suppressed emote sends no packet and plays no local anim at all
/// (a seated `/bow` self-censors; the server round-trip never happens). Byte-verified predicate,
/// exactly these four tests in the note's site order — the fifth flag the note decodes (`0x4000`,
/// "requires standing still") only sets an *out* param the client acts on while fear/confuse-
/// controlled, which benilla doesn't model, so it's deliberately not implemented here. `true` =
/// eligible (send + play); `false` = suppress both.
pub(super) fn emote_send_eligible(emote_flags: u32, stand_state: u8, swimming: bool) -> bool {
    // `0x0400`: unconditional suppress (client `0x47db58`).
    if emote_flags & 0x0400 != 0 {
        return false;
    }
    // `0x0001` + non-zero stand-state: "requires STAND" (client `0x47db65`..`0x47db74`).
    if emote_flags & 0x0001 != 0 && stand_state != 0 {
        return false;
    }
    // `0x0080` while swimming (client `0x47db76`..`0x47db7d`).
    if emote_flags & 0x0080 != 0 && swimming {
        return false;
    }
    // `0x0200` ABSENT at SLEEP(3)/DEAD(7): the bit means "allowed while asleep/dead" (client
    // `0x47db8e`..`0x47db9f`).
    if emote_flags & 0x0200 == 0 && matches!(stand_state, 3 | 7) {
        return false;
    }
    true
}
