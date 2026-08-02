//! The **submitted-line** side of the chat (decision 0288 P5) — what we DO about a line.
//! [`drain_chat_input`] routes each Entered line: a plain line sends as the box's CURRENT type (the
//! edit machine's law, [`super::edit`]); a `/`-line runs the Enter-path type switch, the reply arm,
//! then the action table. What a line *means* is [`parse`]'s ([`parse_line`] → [`ParsedChat`]);
//! which strings reach which arm is [`super::commands`]'s table.
//!
//! The emote arm is the client's `DoEmote` (`0x5ef560`) in full: the eligibility gate
//! ([`emote_send_eligible`]), the asleep gate, the stow, the **posture** branch that makes `/sit`
//! sit, then `CMSG_TEXT_EMOTE`. Opening keys + live parsing live in [`super::edit`]; the inbound
//! half is [`super::feed`]. Own sends are never echoed locally — the wire echoes back (vanilla
//! behavior).

use bevy::prelude::*;

mod parse;
pub(super) use parse::{parse_enter_type_switch, parse_line, ParsedChat};

use crate::creature_anim::{move_flags, MovementState};
use crate::net::{ClientCommand, NetCommands, SelfPlayer};
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
/// The client-local diagnostics' inputs, as one [`SystemParam`] — [`drain_chat_input`] is at the
/// 16-parameter ceiling, and a named struct beats a nested tuple nobody can read.
///
/// - `camera`/`clock` feed **`/shot`**, the framing instrument (decision 0600).
/// - `liquids`/`interior` feed **`/liquid`**, the swim diagnostic (decision 0634 follow-up): the
///   interior claim is what decides which liquid surfaces the swim query is allowed to see, so the
///   two only mean anything together.
/// - `stores`/`self_store`/`factions`/`reputations` feed **`/reaction`**, the attackability
///   diagnostic (decision 0637): the exact inputs [`crate::target::ring_reaction`] judges on, so
///   "why is this unit not attackable" is one command instead of a guess.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct ChatProbes<'w, 's> {
    camera: Query<
        'w,
        's,
        &'static GlobalTransform,
        (With<crate::player::WorldCamera>, Without<SelfPlayer>),
    >,
    clock: Option<Res<'w, crate::lighting::GameClock>>,
    liquids: Query<'w, 's, &'static crate::liquid::WaterChunkInfo>,
    interior: Res<'w, crate::wmo_portal::CurrentWmoInterior>,
    player_room: Res<'w, crate::wmo_portal::PlayerWmoRoom>,
    camera_claim: Res<'w, crate::wmo_portal::CameraInteriorClaim>,
    stores: Query<'w, 's, &'static crate::net::ObjectStore>,
    self_store: Query<'w, 's, &'static crate::net::ObjectStore, With<SelfPlayer>>,
    factions: Option<Res<'w, crate::target::Factions>>,
    reputations: Res<'w, crate::net::Reputations>,
    /// `/reaction <name>`'s resolve — so a scripted probe can ask about a player it has not
    /// clicked (the two-client duel run has no way to select the other side).
    guids: Res<'w, crate::net::GuidIndex>,
}

/// Everything the drain **queues into another subsystem's one setter** rather than applying itself,
/// bundled (the drain is at Bevy's 16-param ceiling).
///
/// - `stand`/`sheath` — the two setters `DoEmote` drives besides the packet: the **posture**
///   (`EmoteSpecProc == 1` → `SetStandState`) and the **stow** (`SetSheatheState(0, SNAP)`,
///   unconditional on every emote that passes the gates — wow-re `sheath-policy.md` §1, site
///   `0x5ef630`).
/// - `target`/`assist` — the by-name selection asks (decision 0886), answered by
///   [`crate::target`]'s shared resolver so they commit through the same SetSelection path a click
///   does. Chat never writes [`Selection`] itself.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct ChatOut<'w> {
    stand: MessageWriter<'w, crate::player::StandStateRequest>,
    sheath: MessageWriter<'w, crate::creature_anim::SheathRequest>,
    target: MessageWriter<'w, crate::target::TargetByNameRequest>,
    assist: MessageWriter<'w, crate::target::AssistRequest>,
    follow: MessageWriter<'w, crate::player::FollowRequest>,
}

// One parameter per concern — the chat drain fans out to every command's consumer.
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
    // The command table (decision 0881) — the reference's own aliases, resolved at boot.
    table: Res<super::commands::SlashCommands>,
    mut chat_out: ChatOut,
    probes: ChatProbes,
) {
    let ChatProbes {
        camera: world_camera,
        clock,
        liquids,
        interior,
        player_room,
        camera_claim,
        stores,
        self_store,
        factions,
        reputations,
        guids,
    } = &probes;
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
        match parse_line(&table, msg) {
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
            ParsedChat::Liquid => {
                // What the swim query actually sees here, and why. Prints the interior claim, the
                // resolved verdict, and EVERY candidate footprint — a surface that should not be
                // claiming shows up beside the one that should.
                let Ok((_, feet)) = self_player.single().map(|(_, _, t)| ((), t.translation()))
                else {
                    chat_log.push_event(super::event::ChatEvent::text_only(
                        super::event::ChatEventKind::System,
                        "liquid: no player yet".into(),
                    ));
                    continue;
                };
                let wow = benilla_assets::coords::bevy_to_wow(feet);
                let claim = crate::liquid::player_claim(player_room);
                let eye = crate::liquid::camera_claim(camera_claim);
                let verdict = crate::liquid::liquid_at(liquids.iter(), wow, claim);
                let mut lines = vec![
                    format!(
                        "liquid: feet [{:.2}, {:.2}, {:.2}] · claim {claim:?} ({})",
                        wow[0],
                        wow[1],
                        wow[2],
                        match interior.0 {
                            Some(k) => format!(
                                "wmo {} nameSet {} group {}",
                                k.wmo_id, k.name_set, k.group_area_id
                            ),
                            None => "no WMO interior claim".into(),
                        }
                    ),
                    // The camera EYE is a separate subject with its own claim (the reference's
                    // `[0xc7b748]`), and it is the one that decides the underwater filter. Printing
                    // only the player's is what let the two disagree unseen for a whole bug.
                    format!("liquid: EYE claim {eye:?}"),
                    match verdict {
                        Some(h) => format!(
                            "liquid: VERDICT {:?} surface z {:.2} ({:+.2} over feet)",
                            h.kind,
                            h.surface_z,
                            h.surface_z - wow[2]
                        ),
                        None => "liquid: VERDICT none — not in liquid".into(),
                    },
                ];
                let candidates = crate::liquid::describe_at(liquids.iter(), wow, claim);
                if candidates.is_empty() {
                    lines.push("liquid: no footprint covers this XY at all".into());
                }
                lines.extend(candidates.into_iter().map(|c| format!("liquid:   {c}")));
                for line in lines {
                    chat_log.push_event(super::event::ChatEvent::text_only(
                        super::event::ChatEventKind::System,
                        line,
                    ));
                }
            }
            ParsedChat::Reaction { name } => {
                // Everything the reaction ladder judges the subject on, in the order it judges:
                // the PvP rung's three duel gates first (naming the one that refused), then the
                // faction work, then the `can_attack` verdict the cast/attack paths use.
                let subject_guid = match &name {
                    Some(n) => crate::ui_duel::streamed_player_named(n, guids, &names),
                    None => selection.guid,
                };
                let target_store = subject_guid
                    .and_then(|g| guids.0.get(&g).copied())
                    .and_then(|e| stores.get(e).ok());
                let own_store = self_store.iter().next();
                let mut lines = Vec::new();
                let describe = |label: &str, s: Option<&crate::net::ObjectStore>| match s {
                    Some(s) => format!(
                        "reaction: {label} flags 0x{:08x} (player-controlled {}) faction_tpl {:?} \
                         duel_team {} duel_arbiter 0x{:016x}",
                        s.0.unit_flags(),
                        s.0.unit_flags() & (1 << 3) != 0,
                        s.0.unit_faction_template(),
                        s.0.player_duel_team(),
                        s.0.player_duel_arbiter(),
                    ),
                    None => format!("reaction: {label} — no ObjectStore"),
                };
                lines.push(format!(
                    "reaction: subject {} guid {} · self store present {}",
                    name.as_deref().unwrap_or("<current target>"),
                    subject_guid.map_or("none".to_string(), |g| format!("0x{g:016x}")),
                    own_store.is_some(),
                ));
                lines.push(describe("target", target_store));
                lines.push(describe("self  ", own_store));
                match (target_store, own_store) {
                    (Some(t), Some(o)) => {
                        lines.push(format!(
                            "reaction: duel rung {:?}",
                            crate::target::duel_rung(&t.0, &o.0)
                        ));
                    }
                    _ => {
                        lines.push("reaction: duel rung not evaluated (a store is missing)".into())
                    }
                }
                let rank = crate::target::ring_reaction(
                    factions.as_deref(),
                    reputations,
                    target_store,
                    own_store,
                );
                lines.push(format!(
                    "reaction: RANK {rank} → can_attack {}",
                    crate::target::can_attack(
                        target_store,
                        factions.as_deref(),
                        reputations,
                        own_store
                    )
                ));
                for line in lines {
                    // Also to the log: a scripted two-client run reads stdout, not the feed.
                    info!("{line}");
                    chat_log.push_event(super::event::ChatEvent::text_only(
                        super::event::ChatEventKind::System,
                        line,
                    ));
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
            // The duel verbs (decision 0633) enter the SAME intent queue the Era globals feed —
            // the reference's own slash handlers are one-liners over `StartDuel`/`CancelDuel`, so
            // routing through the queue keeps a single resolution path (spell lookup, streamed-
            // player gate, arbiter echo) instead of a second one here.
            ParsedChat::Duel { name } => {
                if let Some(name) =
                    name.or_else(|| target_player_name(&selection, &mut names, &commands))
                {
                    script.queue_duel_request(benilla_ui::script::DuelRequest::StartByName(name));
                }
            }
            ParsedChat::Forfeit => {
                script.queue_duel_request(benilla_ui::script::DuelRequest::Cancel);
            }
            // The by-name selection pair (decision 0886). Both hand the name to `crate::target`'s
            // shared resolver — the reference's own `0x493aa0`, parameterised per caller — so the
            // commit goes through the one SetSelection path a click, TAB and `TargetUnit` share.
            //
            // A bare `/target` reproduces `GetSlashCmdTarget`'s fallback (your current target's
            // name, iff it is a player) and is therefore a no-op re-select; a bare `/target` with a
            // creature selected resolves to nothing, exactly as the reference's `if
            // GetSlashCmdTarget(msg)` guard does.
            ParsedChat::Target { name } => {
                if let Some(name) =
                    name.or_else(|| target_player_name(&selection, &mut names, &commands))
                {
                    chat_out
                        .target
                        .write(crate::target::TargetByNameRequest { name });
                }
            }
            // A bare `/assist` is the ref's `AssistUnit("target")` — assist whoever is selected,
            // creature or player — so it passes `None` straight through rather than taking the
            // player-name fallback. The ref reaches the same unit by resolving its NAME first
            // (`AssistByName`), which can pick a different same-named player; going by the live
            // selection is the same intent without that ambiguity (recorded in 0886).
            ParsedChat::Assist { name } => {
                chat_out.assist.write(crate::target::AssistRequest { name });
            }
            // `/follow` (decision 0890) — the subject is resolved by `crate::target` and the motion
            // is `crate::player`'s; chat only carries the ask. Bare follows the current selection.
            ParsedChat::Follow { name } => {
                chat_out.follow.write(crate::player::FollowRequest { name });
            }
            // The ref's handler is `SlashCmdList["PVP"] = function() TogglePVP() end` — one line
            // over the same binding the popup row calls, so it enters the same intent queue
            // (decision 0646 §3; the `/duel` reasoning above, verbatim).
            ParsedChat::Pvp => script.queue_pvp_toggle(),
            // Run the reference's own slash body (see the variant's doc). The argument is a Lua
            // string literal, so it is escaped: a `/who` filter legitimately contains quotes
            // (`z-"Elwynn Forest"`), and a newline would end the statement.
            ParsedChat::Social { verb, arg } => {
                let lua = format!("{}(\"{}\")", verb.lua_fn(), parse::escape_lua_string(&arg));
                if let Err(e) = script.run(&lua) {
                    warn!("ui_chat(social): {e}");
                }
            }
            ParsedChat::Help => {
                // An honest benilla summary (the ref's HELP_TEXT_LINE pages are a settings-era
                // nicety; this stays useful and never stale-quotes them).
                for line in [
                    "Chat: /s /y /p /g /o /raid /rw /bg, /w <name>, /r, /e",
                    "Channels: /join <name> [pw], /leave <name>, /chatlist <name>",
                    "Party: /invite /uninvite /promote [name — bare uses your target]",
                    "Loot: /ffa /roundrobin /master <name>",
                    "Duel: /duel [name — bare uses your target], /forfeit (/concede /yield)",
                    "Social: /who [filter], /friends, /ignore, /trade, /inspect",
                    "Emotes: /sit /stand /sleep /kneel and every /wave-style emote",
                    "Misc: /afk, /dnd, /random [min] [max], /played, /logout, /quit",
                    "Instruments: /shot, /liquid, /reaction, /castvis, /chattest, /partytest",
                ] {
                    chat_log.push_event(super::event::ChatEvent::text_only(
                        super::event::ChatEventKind::System,
                        line.to_string(),
                    ));
                }
            }
            // `DoEmote` (`0x5ef560`) end to end — wow-re `object-layer/scratch/emote-posture-
            // gate.md` §1. The gates in the client's own order, then the two things it DOES: set a
            // posture (the `/sit` family) and send the packet.
            ParsedChat::TextEmote(text_id) => {
                let (stand_state, flags) = self_player
                    .single()
                    .map_or((0, 0), |(_, m, _)| (m.stand_state, m.flags));
                let swimming = flags & move_flags::SWIMMING != 0;
                let emote_id = emotes.as_deref().and_then(|e| e.text_emote(text_id));
                // A chat-only text emote (no Emotes.dbc row) has no EmoteFlags to test and no
                // posture to set — it always sends.
                let posture = emote_id.and_then(|id| emotes.as_deref()?.posture_state(id));
                // GATE A (`CheckEmoteEligible` `0x47db40`): suppresses the anim AND the packet.
                let eligible = match emotes.as_deref() {
                    Some(e) => emote_id
                        .and_then(|id| e.emote_flags(id))
                        .is_none_or(|f| emote_send_eligible(f, stand_state, swimming)),
                    None => true,
                };
                // GATE B (`0x5ef5f3`): asleep, only a POSTURE emote gets through — which is how
                // `/stand` (and `/sit`) is the way out of `/sleep`, while a `/wave` in bed does
                // nothing at all.
                let awake_or_posture = stand_state != 3 || posture.is_some();
                if !eligible || !awake_or_posture {
                    // Byte-verified whole-send suppression: a seated /bow sends nothing (no packet,
                    // no anim) — the client's DoEmote returns before building the packet.
                    debug!(
                        "chat: emote {text_id} suppressed (eligible {eligible}, \
                         awake-or-posture {awake_or_posture}, stand {stand_state})"
                    );
                    continue;
                }
                // The stow (`0x5ef630`, VERIFIED unconditional at this point in the flow): every
                // emote that reaches here puts the weapons away, instantly — a `/wave` mid-fight
                // sheathes in the reference too. The anim layer's one setter owns the idempotency
                // refusal, so an already-stowed body costs nothing.
                if let Ok((entity, _, _)) = self_player.single() {
                    chat_out.sheath.write(crate::creature_anim::SheathRequest {
                        entity,
                        state: 0,
                        ceremony: false,
                    });
                }
                // The posture branch (`EmoteSpecProc == 1` → `SetStandState(param)`): the emote's
                // whole visible effect, since the SERVER deliberately does nothing for a STATE text
                // emote (vmangos `HandleTextEmoteOpcode` breaks out for SIT/SLEEP/KNEEL). Through
                // the one setter in `crate::player`, which sends `CMSG_STANDSTATECHANGE`, holds the
                // local commit until the echo lands, and runs the sit-stow rider.
                if let Some(state) = posture {
                    chat_out
                        .stand
                        .write(crate::player::StandStateRequest { state: state as u8 });
                }
                let target = selection.guid.unwrap_or(0);
                match commands
                    .0
                    .send(ClientCommand::TextEmote { text_id, target })
                {
                    Ok(()) => info!("chat: sent {msg:?}"),
                    Err(_) => warn!("chat: not connected; dropped {msg:?}"),
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
                                    dest: None,
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
            // `/logout` (and `/camp`) is the reference's own `SlashCmdList["LOGOUT"]` → `Logout()`,
            // so it takes the SAME route the game menu's Logout button does (decision 0674): the
            // request queues on the script seam, `crate::ui_logout` sends it and narrates the
            // server's answer with the CAMP countdown. It used to send `CMSG_LOGOUT_REQUEST`
            // straight from here, which meant a field logout looked like nothing happening for 20 s.
            ParsedChat::Logout => {
                script.queue_session_request(benilla_ui::script::SessionRequest::Logout)
            }
            // `/quit` `/exit` — the ref's `Quit()`, same queue as the game menu's Exit button, so
            // the field countdown and the confirmation are the ones already built (decision 0674).
            ParsedChat::Quit => {
                script.queue_session_request(benilla_ui::script::SessionRequest::Quit)
            }
            // The reference's own handler body, run in the VM (the 0668 posture) — `/trade`,
            // `/inspect`, the loot-method trio, and `/script`'s raw chunk.
            ParsedChat::Lua { body } => {
                if let Err(e) = script.run(&body) {
                    // `/script` hands the player's own typo back to them; a built-in body failing
                    // is ours to see in the log.
                    warn!("ui_chat: {body:?}: {e}");
                    chat_log.push_event(super::event::ChatEvent::text_only(
                        super::event::ChatEventKind::System,
                        format!("{e}"),
                    ));
                }
            }
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
