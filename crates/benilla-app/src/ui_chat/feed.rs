//! The chat sources → [`ChatEvent`] bridge (decision 0288 §1): every inbound line — a decoded
//! `SMSG_MESSAGECHAT`, a channel notice, a `/random` roll, a client-composed loot/system line —
//! becomes one typed event here, names resolved ask-once through [`crate::names::NameCache`]
//! (a line whose sender name is still in flight re-checks each frame, bounded), then routes
//! through [`super::frames::route`] — the one composer/color/fan-out seam.

use bevy::prelude::*;

use benilla_protocol::messages::{
    channel_notice, ChannelNoticeTail, ChatMessage, LevelUpInfo, XpGain, MACRO_EXPANDED_TYPES,
};

use crate::names::NameCache;
use crate::net::{GuidIndex, NetCommands, ObjectStore};

use super::edit::ChannelState;
use super::event::{flag_of_tag, kind_of_wire, language_name, ChatEvent, ChatEventKind};
use super::frames::{route, ChatWindows};

/// Give up re-checking a line's pending sender name after this many frames (a negative-cached or
/// genuinely-unknown guid never resolves; ~2s at 60fps is well past a normal name-query
/// round-trip). The line renders with a placeholder rather than being lost.
const NAME_MAX_TRIES: u16 = 120;

/// One queued item awaiting its turn through [`feed_chat`].
enum Pending {
    /// A decoded `SMSG_MESSAGECHAT`: needs kind mapping + (player kinds) an ask-once name resolve.
    Wire { msg: ChatMessage, tries: u16 },
    /// A channel notice whose tail carries guids to resolve (`a` = affected/actor, `b` = the
    /// second actor of kicked/banned/unbanned; `0` = absent).
    Notice {
        notice: u8,
        channel: String,
        a_guid: u64,
        b_guid: u64,
        tries: u16,
    },
    /// A `/random` broadcast awaiting the roller's name.
    Roll {
        min: u32,
        max: u32,
        roll: u32,
        guid: u64,
        tries: u16,
    },
    /// A kill-XP award awaiting the victim's name ("%s dies, you gain %d experience." —
    /// COMBATLOG_XPGAIN_FIRSTPERSON; decision 0304). `bonus` = rested (total − base).
    XpGain {
        victim: u64,
        total: u32,
        bonus: u32,
        tries: u16,
    },
    /// An area discovery (`SMSG_EXPLORATION_EXPERIENCE`): the toast + conditional chat line pair
    /// (the drain fires them — the toast needs the script, which only [`feed_chat`] holds).
    Discovery { area: String, xp: u32 },
    /// A ready event (client-composed lines; name-carrying notices).
    Event(ChatEvent),
}

/// The pending chat items the net/loot/quest feeds fill and [`feed_chat`] drains. Cleared on
/// disconnect (a half-resolved line from a dead session must not leak into the next).
#[derive(Resource, Default)]
pub(crate) struct ChatLog {
    pending: Vec<Pending>,
}

impl ChatLog {
    /// Queue a decoded wire line (`SMSG_MESSAGECHAT`).
    pub(crate) fn push_wire(&mut self, msg: ChatMessage) {
        self.pending.push(Pending::Wire { msg, tries: 0 });
    }

    /// Queue a ready, client-composed event (loot receive lines, quest/system lines, played
    /// time — [`ChatEvent::text_only`] covers the common case).
    pub(crate) fn push_event(&mut self, event: ChatEvent) {
        self.pending.push(Pending::Event(event));
    }

    /// Queue a decoded `SMSG_CHANNEL_NOTIFY`. JOINED/LEFT become the ref's CHANNEL_JOIN/LEAVE
    /// *events* (a member line, hyperlinked — ChatFrame.lua's ChatTypeGroup["CHANNEL"]); every
    /// other notice becomes a CHANNEL_NOTICE composed by the `CHAT_<X>_NOTICE` law. Guid tails
    /// park here for the ask-once resolve; name tails are ready immediately.
    pub(crate) fn push_channel_notice(
        &mut self,
        notice_byte: u8,
        channel: String,
        tail: &ChannelNoticeTail,
    ) {
        let (a_guid, b_guid, name) = match tail {
            ChannelNoticeTail::Guid(g) | ChannelNoticeTail::Actor(g) => (*g, 0, None),
            ChannelNoticeTail::Actors { target, source } => (*target, *source, None),
            ChannelNoticeTail::Name(n) => (0, 0, Some(n.clone())),
            ChannelNoticeTail::YouJoined { .. } | ChannelNoticeTail::Empty => (0, 0, None),
            ChannelNoticeTail::ModeChange { .. } => return, // silent in the 1.12 UI (no string)
        };
        if a_guid != 0 {
            self.pending.push(Pending::Notice {
                notice: notice_byte,
                channel,
                a_guid,
                b_guid,
                tries: 0,
            });
        } else {
            self.pending.push(Pending::Event(notice_event(
                notice_byte,
                channel,
                name,
                None,
            )));
        }
    }

    /// Queue a `/random` broadcast (`MSG_RANDOM_ROLL`) for the roller-name resolve.
    pub(crate) fn push_roll(&mut self, min: u32, max: u32, roll: u32, guid: u64) {
        self.pending.push(Pending::Roll {
            min,
            max,
            roll,
            guid,
            tries: 0,
        });
    }

    /// Queue an XP award's chat line (`SMSG_LOG_XPGAIN` → CHAT_MSG_COMBAT_XP_GAIN; decision
    /// 0304). A named kill waits on the victim's name; everything else composes immediately
    /// (COMBATLOG_XPGAIN_FIRSTPERSON_UNNAMED "You gain %d experience.").
    pub(crate) fn push_xp_gain(&mut self, x: &XpGain) {
        if x.kill && x.victim != 0 {
            self.pending.push(Pending::XpGain {
                victim: x.victim,
                total: x.total,
                bonus: x.total.saturating_sub(x.base),
                tries: 0,
            });
        } else {
            self.pending.push(Pending::Event(ChatEvent::text_only(
                ChatEventKind::CombatXpGain,
                xp_gain_line(None, x.total, 0),
            )));
        }
    }

    /// Queue an area discovery's announcement (`SMSG_EXPLORATION_EXPERIENCE`; decision 0828,
    /// surfaces corrected by the 0829 RE): the ERR_ZONE_EXPLORED toast fires on **every** packet
    /// (UIErrorsFrame, via `UI_INFO_MESSAGE` — never chat), and the ERR_ZONE_EXPLORED_XP chat
    /// system line rides **additionally** iff `xp > 0`. The caller resolved `area_name` from
    /// `AreaTable.dbc` by the packet's area id.
    pub(crate) fn push_exploration(&mut self, area_name: &str, xp: u32) {
        self.pending.push(Pending::Discovery {
            area: area_name.to_string(),
            xp,
        });
    }

    /// Queue our ding's chat lines (`SMSG_LEVELUP_INFO` → the reference PLAYER_LEVEL_UP handler;
    /// decision 0304). `talent_points` is the handler's arg4, client-derived (the packet
    /// carries none).
    pub(crate) fn push_level_up(&mut self, l: &LevelUpInfo, talent_points: u32) {
        for text in level_up_lines(l, talent_points) {
            self.pending.push(Pending::Event(ChatEvent::text_only(
                ChatEventKind::System,
                text,
            )));
        }
    }

    /// Disconnect: drop every pending item (mirrors the merchant/gossip/loot session clears).
    pub(crate) fn clear_session(&mut self) {
        self.pending.clear();
    }

    /// How many items are queued — the gates upstream of this log ([`crate::net::apply`]'s addon
    /// and ignore gates) are "the line never got here at all", so their tests read this.
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

/// The ding's chat lines — the reference PLAYER_LEVEL_UP handler transcribed
/// (ChatFrame.lua:1283-1324, GlobalStrings LEVEL_UP / LEVEL_UP_HEALTH[_MANA] /
/// LEVEL_UP_CHAR_POINTS[_P1] / LEVEL_UP_STAT × SPELL_STAT0..4), in its exact order. Decision 0304.
pub(super) fn level_up_lines(l: &LevelUpInfo, talent_points: u32) -> Vec<String> {
    let mut lines = vec![format!(
        "Congratulations, you have reached level {}!",
        l.level
    )];
    // LEVEL_UP_HEALTH_MANA when mana gained, else LEVEL_UP_HEALTH — unconditional.
    let mana = l.powers[0];
    if mana > 0 {
        lines.push(format!(
            "You have gained {} hit points and {} mana.",
            l.health, mana
        ));
    } else {
        lines.push(format!("You have gained {} hit points.", l.health));
    }
    // LEVEL_UP_CHAR_POINTS[_P1] — the GetText singular/plural pick.
    if talent_points == 1 {
        lines.push("You have gained 1 talent point.".to_string());
    } else if talent_points > 1 {
        lines.push(format!("You have gained {talent_points} talent points."));
    }
    // LEVEL_UP_STAT × each positive gain, SPELL_STAT0..4 order.
    for (name, gain) in ["Strength", "Agility", "Stamina", "Intellect", "Spirit"]
        .into_iter()
        .zip(l.stats)
    {
        if gain > 0 {
            lines.push(format!("Your {name} increases by {gain}."));
        }
    }
    lines
}

/// The XP award's chat line: COMBATLOG_XPGAIN_FIRSTPERSON ("%s dies, you gain %d experience."),
/// its EXHAUSTION1 rested form, or the UNNAMED form (no victim). INTERIM: the rested state word
/// is "Rested" — the only state the live server produces (the beta tired/exhausted penalties are
/// dead data); the client's state-word table is the in-flight 0304 §5's to pin.
pub(super) fn xp_gain_line(victim: Option<&str>, total: u32, bonus: u32) -> String {
    match victim {
        Some(name) if bonus > 0 => {
            format!("{name} dies, you gain {total} experience. (+{bonus} exp Rested bonus)")
        }
        Some(name) => format!("{name} dies, you gain {total} experience."),
        None => format!("You gain {total} experience."),
    }
}

/// The discovery toast — GlobalStrings ERR_ZONE_EXPLORED ("Discovered: %s"), fired on every
/// exploration packet to the UIErrorsFrame (byte-verified: error-table route 1 →
/// `AddErrorMessage 0x4945b0` → UI_INFO_MESSAGE; decisions 0828/0829).
pub(super) fn exploration_toast(area_name: &str) -> String {
    format!("Discovered: {area_name}")
}

/// The discovery chat line — GlobalStrings ERR_ZONE_EXPLORED_XP ("Discovered %s: %d experience
/// gained"), fired **in addition to** the toast iff the packet carried XP (byte-verified: the
/// signed `jle` skip at `0x5e422f`; route 0 → CHAT_MSG_SYSTEM; decisions 0828/0829).
pub(super) fn exploration_line(area_name: &str, xp: u32) -> String {
    format!("Discovered {area_name}: {xp} experience gained")
}

/// Build the event a channel notice becomes: JOINED/LEFT → the member-line kinds; the rest →
/// CHANNEL_NOTICE (composed by [`super::frames::compose_notice`], the notice byte riding the
/// event's `notice` field).
fn notice_event(
    notice_byte: u8,
    channel: String,
    a: Option<String>,
    b: Option<String>,
) -> ChatEvent {
    let kind = match notice_byte {
        channel_notice::JOINED => ChatEventKind::ChannelJoin,
        channel_notice::LEFT => ChatEventKind::ChannelLeave,
        _ => ChatEventKind::ChannelNotice,
    };
    ChatEvent {
        kind: Some(kind),
        sender: a.unwrap_or_default(),
        target: b.unwrap_or_default(),
        channel,
        notice: notice_byte.to_string(),
        ..Default::default()
    }
}

/// Drain [`ChatLog`]: resolve names (ask-once, bounded), build events, [`route`] them. Also ticks
/// the whisper-chime throttle.
#[allow(clippy::too_many_arguments)] // a Bevy system's param list IS its dependency set
pub(super) fn feed_chat(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut log: ResMut<ChatLog>,
    mut windows: ResMut<ChatWindows>,
    mut edit: ResMut<super::edit::ChatEditState>,
    mut channels: ResMut<ChannelState>,
    mut names: ResMut<NameCache>,
    mut bubbles: ResMut<crate::chat_bubble::BubbleQueue>,
    bubble_cfg: Res<crate::chat_bubble::BubbleConfig>,
    commands: Res<NetCommands>,
    time: Res<Time>,
    // The `$`-macro subject seam: monster/BG lines expand against the guid the line is ADDRESSED to,
    // which needs the object index + the streamed unit's descriptors. See [`macro_subject`].
    guids: Res<GuidIndex>,
    stores: Query<&ObjectStore>,
    states: Res<crate::world_state::WorldStates>,
) {
    let Some(mut script) = script else {
        return;
    };
    windows.tell_alert_left = (windows.tell_alert_left - time.delta_secs()).max(0.0);
    if log.pending.is_empty() {
        return;
    }
    let pending = std::mem::take(&mut log.pending);
    let mut still = Vec::new();
    for item in pending {
        match item {
            Pending::Event(mut event) => {
                // The joined-list upkeep (the client-side number law): YOU_JOINED appends,
                // YOU_LEFT (and the suspend form) removes.
                //
                // Logged, because this edge is where "we asked to join" becomes "the server says
                // we are in": [`super::channels`]'s walk only ever proves the request went out, and
                // the round trip is what actually arms an addon (it is the `CHAT_MSG_CHANNEL_NOTICE`
                // Ace2's whole init gate waits on). A join the server refuses is otherwise
                // completely silent on this side.
                if event.kind == Some(ChatEventKind::ChannelNotice) {
                    match event.notice.as_str() {
                        "2" if channels.number_of(&event.channel).is_none() => {
                            channels.joined.push(event.channel.clone());
                            debug!(
                                "chat: server confirms channel {:?} joined (slot {})",
                                event.channel,
                                channels.joined.len()
                            );
                        }
                        "3" => {
                            debug!("chat: server confirms channel {:?} left", event.channel);
                            channels
                                .joined
                                .retain(|c| !c.eq_ignore_ascii_case(&event.channel));
                        }
                        _ => {}
                    }
                    // Mirror the confirmed list into the VM, where `GetChannelName` reads it
                    // (17 corpus sites across 6 addons). Here rather than beside either arm
                    // because this is the ONLY place the list changes, so one push cannot drift
                    // from it — and unconditional within the notice branch so a notice that
                    // changed nothing still costs one clone rather than risking a missed edge.
                    script.set_joined_channels(channels.joined.clone());
                }
                // A member-line / notice channel renders numbered when we know its slot.
                channels.stamp_channel(&mut event);
                route(&mut script, &mut windows, &event);
            }
            Pending::Wire { msg, tries } => {
                let kind = kind_of_wire(msg.chat_type);
                if kind.is_none() {
                    warn!(
                        "chat: unmodeled wire type {:#04x} dropped: {:?}",
                        msg.chat_type, msg.text
                    );
                    continue;
                }
                // A monster line carries its name inline; a player line resolves its guid.
                let name = match &msg.sender_name {
                    Some(n) => Some(n.clone()),
                    None if needs_name(msg.chat_type) && msg.sender_guid != 0 => names
                        .resolve(msg.sender_guid, &commands)
                        .map(str::to_string),
                    _ => None,
                };
                if name.is_none()
                    && needs_name(msg.chat_type)
                    && msg.sender_guid != 0
                    && tries < NAME_MAX_TRIES
                {
                    still.push(Pending::Wire {
                        msg,
                        tries: tries + 1,
                    });
                    continue;
                }
                // The channel's base name; the numbered display form, its slot number and its
                // zone id are stamped on below ([`ChannelState::stamp_channel`]) — arg4/arg7-arg9.
                let channel_base = msg.channel.clone().unwrap_or_default();
                // `$`-macro expansion (decision 0754, corrected by 0759): the reference runs its one
                // server-text expander over the monster/boss + BG-system types and nothing else,
                // against the guid the line is ADDRESSED to. Every other type reaches the frame
                // verbatim.
                //
                // The FAILURE arms are the reference's, and they are not what a panel does: the chat
                // path never puts a `$` on screen. VERIFIED `0x49dac2-0x49db1e` (mirrored at
                // `0x49d9c9`): expanded → show; failed with a zero subject → **drop the line**;
                // failed with the name already known → **drop** (a known name means retrying cannot
                // help); failed with the name still unknown → hold the RAW text, let the name query
                // run, and re-expand when it answers — only a second failure there shows the raw
                // text. Our bounded `tries` retry IS that hold, and exhausting it is that second
                // failure.
                let expanded = if MACRO_EXPANDED_TYPES.contains(&msg.chat_type) {
                    // The addressee where the shape carries one, else the only guid it has (the
                    // `default:`-shaped BG_SYSTEM lines have no target slot at all).
                    let subject_guid = if msg.target_guid != 0 {
                        msg.target_guid
                    } else {
                        msg.sender_guid
                    };
                    let subject = crate::npc_text::subject_for_guid(
                        subject_guid,
                        &guids,
                        &stores,
                        &mut names,
                        &commands,
                    );
                    let (text, clean) = crate::npc_text::substitute_checked(
                        &msg.text,
                        &crate::npc_text::MacroContext {
                            subject: subject.as_ref(),
                            states: &states,
                        },
                    );
                    if clean {
                        Some(text)
                    } else if subject_guid == 0 || names.peek(subject_guid).is_some() {
                        debug!(
                            "chat: dropping unexpandable [{:#04x}] {:?} (subject {subject_guid:#x})",
                            msg.chat_type, msg.text
                        );
                        continue; // the reference drops it — no line at all
                    } else if tries < NAME_MAX_TRIES {
                        // Name query is already in flight (`subject_for_guid` issued it); hold the
                        // raw line and re-expand when it lands.
                        still.push(Pending::Wire {
                            msg,
                            tries: tries + 1,
                        });
                        continue;
                    } else {
                        None // the post-query second failure: show the raw text verbatim
                    }
                } else {
                    None
                };
                let text = expanded.unwrap_or_else(|| msg.text.clone());
                let mut event = ChatEvent {
                    kind,
                    text: text.clone(),
                    sender: name.unwrap_or_else(|| {
                        if needs_name(msg.chat_type) && msg.sender_guid != 0 {
                            "Unknown".to_string()
                        } else {
                            String::new()
                        }
                    }),
                    language: language_name(msg.language).to_string(),
                    channel: channel_base,
                    flag: flag_of_tag(msg.chat_tag).to_string(),
                    ..Default::default()
                };
                channels.stamp_channel(&mut event);
                // A received whisper remembers its sender (`ChatEdit_SetLastTellTarget`,
                // ChatFrame_OnEvent l.1471) — the `/r` + Tab-cycle ring.
                if event.kind == Some(ChatEventKind::Whisper) && !event.sender.is_empty() {
                    edit.remember_tell(&event.sender);
                }
                route(&mut script, &mut windows, &event);
                // The speech bubble spawns the moment the line routes — the reference's
                // `0x49acd9` sits in the same SMSG display path ([`crate::chat_bubble`]).
                if let Some(kind) = event.kind {
                    // The bubble shows the same expanded line the feed does — the reference's
                    // bubble spawn sits inside this same SMSG display path, downstream of the
                    // expander, so a `$n` must never survive into it either.
                    bubbles.push(&bubble_cfg, msg.sender_guid, kind, &text);
                }
            }
            Pending::Notice {
                notice,
                channel,
                a_guid,
                b_guid,
                tries,
            } => {
                let a = names.resolve(a_guid, &commands).map(str::to_string);
                let b = if b_guid != 0 {
                    names.resolve(b_guid, &commands).map(str::to_string)
                } else {
                    Some(String::new())
                };
                if (a.is_none() || b.is_none()) && tries < NAME_MAX_TRIES {
                    still.push(Pending::Notice {
                        notice,
                        channel,
                        a_guid,
                        b_guid,
                        tries: tries + 1,
                    });
                    continue;
                }
                // NOT stamped, deliberately — and this arm is inconsistent with the other two
                // because of it. A guid-tail notice (a join/leave member line, a kick, a
                // moderation change) reaches the composer with its channel name UNNUMBERED, so it
                // renders "[World] Ann joined channel." where the same channel's speech renders
                // "[1. World]". The reference numbers both: `ChatFrame_OnEvent` l.1463 strips only
                // the " - Zone" tail from arg4, never the number.
                //
                // Adding `channels.stamp_channel(&mut event)` here fixes it in one line — and
                // changes what the player sees, which this pass is not allowed to do. Left for the
                // director's call; the cost of leaving it is that arg7/arg8/arg9 are 0/0/empty on
                // these events alone.
                let event = notice_event(
                    notice,
                    channel,
                    Some(a.unwrap_or_else(|| "Unknown".into())),
                    b,
                );
                route(&mut script, &mut windows, &event);
            }
            Pending::Roll {
                min,
                max,
                roll,
                guid,
                tries,
            } => {
                let name = names.resolve(guid, &commands).map(str::to_string);
                if name.is_none() && tries < NAME_MAX_TRIES {
                    still.push(Pending::Roll {
                        min,
                        max,
                        roll,
                        guid,
                        tries: tries + 1,
                    });
                    continue;
                }
                // RANDOM_ROLL_RESULT = "%s rolls %d (%d-%d)" (GlobalStrings:3290).
                let line = format!(
                    "{} rolls {roll} ({min}-{max})",
                    name.unwrap_or_else(|| "Unknown".into())
                );
                route(
                    &mut script,
                    &mut windows,
                    &ChatEvent::text_only(ChatEventKind::System, line),
                );
            }
            Pending::XpGain {
                victim,
                total,
                bonus,
                tries,
            } => {
                let name = names.resolve(victim, &commands).map(str::to_string);
                if name.is_none() && tries < NAME_MAX_TRIES {
                    still.push(Pending::XpGain {
                        victim,
                        total,
                        bonus,
                        tries: tries + 1,
                    });
                    continue;
                }
                let name = name.unwrap_or_else(|| "Unknown".into());
                route(
                    &mut script,
                    &mut windows,
                    &ChatEvent::text_only(
                        ChatEventKind::CombatXpGain,
                        xp_gain_line(Some(&name), total, bonus),
                    ),
                );
            }
            Pending::Discovery { area, xp } => {
                // The toast fires every time; the chat line only rides XP (decisions 0828/0829).
                script.fire_event(
                    "UI_INFO_MESSAGE",
                    vec![benilla_ui::script::ScriptValue::Str(exploration_toast(
                        &area,
                    ))],
                );
                if xp > 0 {
                    route(
                        &mut script,
                        &mut windows,
                        &ChatEvent::text_only(ChatEventKind::System, exploration_line(&area, xp)),
                    );
                }
            }
        }
    }
    log.pending = still;
}

/// Whether a wire chat type carries a **player guid** whose name must be resolved (vs a monster
/// type with its name inline, or a nameless system line). The player-message families, now
/// including the tagged self-notice types (AFK/DND auto-replies, IGNORED) and the raid/BG set.
fn needs_name(chat_type: u8) -> bool {
    use benilla_protocol::messages as m;
    matches!(
        chat_type,
        m::CHAT_MSG_SAY
            | m::CHAT_MSG_PARTY
            | m::CHAT_MSG_RAID
            | m::CHAT_MSG_GUILD
            | m::CHAT_MSG_OFFICER
            | m::CHAT_MSG_YELL
            | m::CHAT_MSG_WHISPER
            | m::CHAT_MSG_WHISPER_INFORM
            | m::CHAT_MSG_EMOTE
            | m::CHAT_MSG_CHANNEL
            | m::CHAT_MSG_AFK
            | m::CHAT_MSG_DND
            | m::CHAT_MSG_IGNORED
            | m::CHAT_MSG_RAID_LEADER
            | m::CHAT_MSG_RAID_WARNING
            | m::CHAT_MSG_BATTLEGROUND
            | m::CHAT_MSG_BATTLEGROUND_LEADER
    )
}
