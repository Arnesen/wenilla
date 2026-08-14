//! The chat sources → [`ChatEvent`] bridge (decision 0288 §1): every inbound line — a decoded
//! `SMSG_MESSAGECHAT`, a channel notice, a `/random` roll, a client-composed loot/system line —
//! becomes one typed event here, names resolved ask-once through [`crate::names::NameCache`]
//! (a line whose sender name is still in flight re-checks each frame, bounded), then routes
//! through [`super::frames::route`] — the one composer/color/fan-out seam.

use bevy::prelude::*;

use benilla_protocol::messages::{
    channel_notice, ChannelNoticeTail, ChatMessage, LevelUpInfo, XpGain, MACRO_EXPANDED_TYPES,
};

use benilla_assets::{LockRecover, WorldAssets};
use benilla_formats::{EmoteLine, EmoteTextCatalog};

use crate::names::NameCache;
use crate::net::{GuidIndex, NetCommands, ObjectStore, SelfGuid};

use super::edit::ChannelState;
use super::event::{flag_of_tag, kind_of_wire, language_name, ChatEvent, ChatEventKind};
use super::frames::{route, ChatWindows};

/// Give up re-checking a line's pending sender name after this many frames (a negative-cached or
/// genuinely-unknown guid never resolves; ~2s at 60fps is well past a normal name-query
/// round-trip). The line renders with a placeholder rather than being lost.
const NAME_MAX_TRIES: u16 = 120;

/// The text-emote sentence tables (decision 1274), read once off the patch chain.
#[derive(Resource)]
pub(crate) struct EmoteTexts(pub(crate) EmoteTextCatalog);

/// **The locale column is 0.** `[0xc0e080]` is the client's locale slot; only enUS is populated in
/// this install, and every other DBC catalog in the tree reads column 0 for the same reason.
const LOCALE: usize = 0;

/// Load `EmotesText.dbc` × `EmotesTextData.dbc`. `.after(benilla_assets::AssetSet::Open)` at the
/// call site is load-bearing for the reason [`super::channels::load_chat_channels`] records:
/// without it the patch chain does not exist yet and this silently loads nothing.
pub(super) fn load_emote_texts(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_emote_text_catalog(&mut chain)
    };
    match loaded {
        Ok(cat) => {
            info!("chat: {} emote sentence tables", cat.len());
            commands.insert_resource(EmoteTexts(cat));
        }
        Err(e) => warn!("chat: emote text catalog failed to load: {e:#}"),
    }
}

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
    /// An inbound addon line awaiting its SENDER's name — `CHAT_MSG_ADDON` (event 227, fired at
    /// `0x49a95f`). Parked here rather than fired at decode for the reason wow-re records: the
    /// reference fires it *downstream* of the name resolve, from the `CMSG_NAME_QUERY` callback
    /// (`0x49ccc0`), so `sender` is a NAME and never a guid.
    Addon {
        prefix: String,
        message: String,
        distribution: String,
        guid: u64,
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
    /// A text emote (`SMSG_TEXT_EMOTE`) awaiting the PERFORMER's name — decision 1274.
    ///
    /// Parked here for the reference's own reason, not merely for convenience: `0x49dbe0` composes
    /// the sentence immediately when the `NameCache` already holds the performer, and otherwise
    /// queues a `PENDINGTEXTEMOTE` node (`0x49cc00`) that the name-query callback (`0x49d0d0`)
    /// drains into the same composer. This queue *is* that node, and the bounded `tries` is its
    /// give-up edge.
    ///
    /// `target` is the raw wire name (empty = untargeted), never re-resolved — the reference
    /// passes the server's string straight into the format.
    TextEmote {
        performer: u64,
        text_id: u32,
        target: String,
        tries: u16,
    },
    /// An area discovery (`SMSG_EXPLORATION_EXPERIENCE`): the toast + conditional chat line pair
    /// (the drain fires them — the toast needs the script, which only [`feed_chat`] holds).
    Discovery { area: String, xp: u32 },
    /// A ready event (client-composed lines; name-carrying notices).
    Event(ChatEvent),
}

/// Fire `CHAT_MSG_ADDON` with the reference's four arguments, in the reference's order.
///
/// Split out of the drain so the shape that reaches Lua is testable without standing up
/// `feed_chat`'s dozen resources — the scaffolding is not what can be wrong here; the argument
/// ORDER is. wow-re carves it as `SignalEvent2(227, "%s%s%s%s", prefix, message, distribution,
/// sender)` (`0x49a95f`), and `BigWigs` independently self-delivers
/// `self:CHAT_MSG_ADDON("BigWigs", msg, "RAID", playerName)` — a 2006 addon author and the binary
/// agreeing.
///
/// Fired DIRECTLY rather than through [`route`]: `CHAT_MSG_ADDON` is not a `ChatTypeInfo` key and
/// carries four arguments against the chat family's ten, so the chat pipeline would mis-shape it
/// and `every_fired_event_name_is_a_chat_type_info_key` would rightly reject it.
pub(super) fn fire_addon_message(
    script: &mut benilla_ui::script::UiScript,
    prefix: String,
    message: String,
    distribution: String,
    sender: String,
) {
    use benilla_ui::script::ScriptValue::Str;
    script.fire_event(
        "CHAT_MSG_ADDON",
        vec![Str(prefix), Str(message), Str(distribution), Str(sender)],
    );
}

/// One text emote's chat event — the sentence plus the performer name — or `None` when the
/// sentence table has nothing to say (the reference's `0x49b4bd` tail: SIT/STAND/TRAIN ship blank
/// in every locale, so a vanilla `/sit` prints no line at all).
///
/// Split out of the drain for [`fire_addon_message`]'s reason: the scaffolding around it is not
/// what can be wrong, the **shape** is — which sentence form the facts select, and which slot the
/// performer's name lands in.
///
/// **`sender` is the PERFORMER**, and it never reaches the rendered line: TEXT_EMOTE is one of the
/// verbatim families in `ChatFrame_OnEvent`, so the sentence *is* the line and this slot only ever
/// reaches addons as arg2. VERIFIED at the bytes: the last push before the event fire at
/// `0x49b495` is `[ebp-0x4]`, the `NameCache` record resolved for the performer at `0x49b289`
/// (`record+0` is the name string).
pub(super) fn text_emote_event(
    cat: &EmoteTextCatalog,
    text_id: u32,
    line: &EmoteLine,
) -> Option<ChatEvent> {
    Some(ChatEvent {
        kind: Some(ChatEventKind::TextEmote),
        text: cat.compose(text_id, line, LOCALE)?,
        sender: line.performer.to_string(),
        ..Default::default()
    })
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

    /// Queue an inbound addon line (`SMSG_MESSAGECHAT` carrying `LANG_ADDON`) for the sender-name
    /// resolve, then `CHAT_MSG_ADDON`.
    ///
    /// **The split is the counter-intuitive part and it is the reference's** (`0x49a8d0`): the text
    /// divides on its **FIRST** tab, and with **no tab at all the whole text is the PREFIX** and
    /// the message is empty — not the other way round, which is what a reimplementation guesses.
    ///
    /// `distribution` is the remap at `0x49aff4`: only PARTY / RAID / GUILD / BATTLEGROUND have
    /// names, and every other type byte reports `"UNKNOWN"` rather than being dropped — the
    /// reference hands the addon a string it can branch on either way.
    pub(crate) fn push_addon(&mut self, text: &str, chat_type: u8, guid: u64) {
        let (prefix, message) = match text.find('\t') {
            Some(i) => (text[..i].to_string(), text[i + 1..].to_string()),
            None => (text.to_string(), String::new()),
        };
        // **The protocol constants, not hand-copied bytes.** Three of the four literals here were
        // WRONG: RAID is `0x2` and this said `0x03`, GUILD is `0x3` and this said `0x04`, and
        // BATTLEGROUND is `0x5C` and this said `0x18`. The visible effect was not a dropped
        // message but a MISLABELLED one — a real guild addon message arrived at
        // `CHAT_MSG_ADDON` as `"RAID"`, while real raid and battleground traffic fell to
        // `"UNKNOWN"` — so an addon branching on the distribution acted on the wrong lane.
        //
        // The outbound half (`net::addon_wire_chat_type`) always used the named constants and its
        // own doc calls itself "the one place a distribution becomes a wire byte". It was not: this
        // was the other one, and it disagreed. Both ends now read the same symbols, so they cannot
        // drift apart again.
        let distribution = {
            use benilla_protocol::messages as m;
            match u32::from(chat_type) {
                m::CHAT_TYPE_PARTY => "PARTY",
                m::CHAT_TYPE_RAID => "RAID",
                m::CHAT_TYPE_GUILD => "GUILD",
                m::CHAT_TYPE_BATTLEGROUND => "BATTLEGROUND",
                _ => "UNKNOWN",
            }
        }
        .to_string();
        self.pending.push(Pending::Addon {
            prefix,
            message,
            distribution,
            guid,
            tries: 0,
        });
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

    /// Queue a text emote's chat line (`SMSG_TEXT_EMOTE` → CHAT_MSG_TEXT_EMOTE; decision 1274) for
    /// the performer-name resolve, then the `EmotesText`/`EmotesTextData` composition.
    pub(crate) fn push_text_emote(&mut self, performer: u64, text_id: u32, target: String) {
        self.pending.push(Pending::TextEmote {
            performer,
            text_id,
            target,
            tries: 0,
        });
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

    /// The parked addon lines as `(prefix, message, distribution)` — test-only, so the split and
    /// the remap can be asserted without standing up a name cache and a VM.
    #[cfg(test)]
    pub(crate) fn pending_addons(&self) -> Vec<(String, String, String)> {
        self.pending
            .iter()
            .filter_map(|p| match p {
                Pending::Addon {
                    prefix,
                    message,
                    distribution,
                    ..
                } => Some((prefix.clone(), message.clone(), distribution.clone())),
                _ => None,
            })
            .collect()
    }

    /// How many parked items are headed for a CHAT WINDOW.
    ///
    /// Addon lines are excluded on purpose. They park in the same queue for the same ask-once name
    /// resolve, but they are not speech and never render — so counting them here would make
    /// `addon_chat_never_reaches_the_chat_window` fail the moment the receive half opened, which is
    /// exactly what it did. "Pending" stopped meaning "will render" when `Pending::Addon` arrived.
    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending
            .iter()
            .filter(|p| !matches!(p, Pending::Addon { .. }))
            .count()
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

/// Deliver one built event: the joined-list upkeep on **both sides** of the render, with the
/// channel stamp and [`route`] between them — the client's own split, and the reason a leave line
/// still knows its number.
///
/// **YOU_JOINED lands before, YOU_LEFT lands after.** The notice's arg4/arg7/arg8/arg9 are read off
/// the client's channel record ([`ChannelState::stamp_channel`] is our leg of that), so a record
/// torn down before the line is composed costs it the slot number — and, since the color resolves
/// through `ChatTypeInfo["CHANNEL"..arg8]` ([`super::event::resolved_color`]), its color with it.
/// The reference's YOU_LEFT arm only *flags* the teardown (`0x49c115 mov dword [ebp-0xc],1`); the
/// event fires first (`0x49c5b0 call 0x49a870`) and only then does `0x49c5b5 test` /
/// `0x49c5c2 call 0x49bbd0` destroy the record — VERIFIED by disassembly at those addresses (1275).
/// We removed first and printed "Left Channel: [General]" where the client prints "Left Channel:
/// [2. General - Elwynn Forest]"; an addon reading `GetChannelName` from its own handler also saw
/// the channel already gone, which the reference never shows it either.
///
/// **Unmodeled, and visible right there in that arm:** the teardown flag is the *YOU_LEFT* leg's
/// alone. Notice `0x03` splits on `rec+0x9c == 3` — the SUSPENDED leg (`0x49c0e9`) takes a
/// different token AND jumps past `0x49c115`, so a suspended channel keeps its record and its
/// number. We do not model that state field (it is the same one that makes `0x02` "YOU_CHANGED"),
/// so every `0x03` here is a genuine leave.
///
/// The two arms log, because this edge is where "we asked to join" becomes "the server says we are
/// in": [`super::channels`]'s walk only ever proves the request went out, and the round trip is
/// what actually arms an addon (it is the `CHAT_MSG_CHANNEL_NOTICE` Ace2's whole init gate waits
/// on). A join the server refuses is otherwise completely silent on this side. Each arm also
/// mirrors the list into the VM, where `GetChannelName` reads it (17 corpus sites across 6 addons):
/// these two are the only places it ever changes.
pub(super) fn deliver(
    script: &mut benilla_ui::script::UiScript,
    windows: &mut ChatWindows,
    channels: &mut ChannelState,
    event: &mut ChatEvent,
) {
    let notice = event
        .notice_byte()
        .filter(|_| event.kind == Some(ChatEventKind::ChannelNotice));
    if notice == Some(channel_notice::YOU_JOINED) && channels.number_of(&event.channel).is_none() {
        channels.joined.push(event.channel.clone());
        debug!(
            "chat: server confirms channel {:?} joined (slot {})",
            event.channel,
            channels.joined.len()
        );
        script.set_joined_channels(channels.joined.clone());
    }
    // The wire name, kept before `stamp_channel` decorates arg4 with the slot number.
    let leaving = (notice == Some(channel_notice::YOU_LEFT)).then(|| event.channel.clone());
    // A member-line / notice channel renders numbered when we know its slot.
    channels.stamp_channel(event);
    route(script, windows, event);
    if let Some(name) = leaving {
        debug!("chat: server confirms channel {name:?} left");
        channels.joined.retain(|c| !c.eq_ignore_ascii_case(&name));
        script.set_joined_channels(channels.joined.clone());
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
    // The text-emote sentence seam (decision 1274): the tables, plus the guid the composer's
    // "are you the performer?" test compares against.
    emote_texts: Option<Res<EmoteTexts>>,
    self_guid: Res<SelfGuid>,
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
                deliver(&mut script, &mut windows, &mut channels, &mut event);
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
            Pending::Addon {
                prefix,
                message,
                distribution,
                guid,
                tries,
            } => {
                let name = names.resolve(guid, &commands).map(str::to_string);
                if name.is_none() && tries < NAME_MAX_TRIES {
                    still.push(Pending::Addon {
                        prefix,
                        message,
                        distribution,
                        guid,
                        tries: tries + 1,
                    });
                    continue;
                }
                fire_addon_message(
                    &mut script,
                    prefix,
                    message,
                    distribution,
                    name.unwrap_or_else(|| "Unknown".into()),
                );
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
            Pending::TextEmote {
                performer,
                text_id,
                target,
                tries,
            } => {
                // Both names resolve ask-once: the performer's is the sentence's `%s`, and our own
                // is what the "is the target me?" compare needs (`GetOwnName` in the reference,
                // which never has to wait for it — we can, so we park like every other line).
                let performer_name = names.resolve(performer, &commands).map(str::to_string);
                let your_name = self_guid
                    .0
                    .and_then(|g| names.resolve(g, &commands).map(str::to_string));
                if (performer_name.is_none() || your_name.is_none()) && tries < NAME_MAX_TRIES {
                    still.push(Pending::TextEmote {
                        performer,
                        text_id,
                        target,
                        tries: tries + 1,
                    });
                    continue;
                }
                // No name for the performer ⇒ **no line**, not an "Unknown" one: the reference
                // bails outright when `NameCache::GetRecord` misses (`0x49b28c`), and that cache
                // is player-only. The emote is still an animation and a voice — those rode the
                // `EmoteMessage` path at decode and do not depend on this.
                let (Some(performer_name), Some(cat)) = (performer_name, emote_texts.as_deref())
                else {
                    debug!("chat: text emote {text_id} from {performer:#x} has no sentence source");
                    continue;
                };
                let event = text_emote_event(
                    &cat.0,
                    text_id,
                    &EmoteLine {
                        performer: &performer_name,
                        performer_is_you: self_guid.0 == Some(performer),
                        // Sex 1 = Female, from the same `SMSG_NAME_QUERY_RESPONSE` the name came
                        // from — exactly where the reference reads it (`record+0x13c`).
                        performer_female: names
                            .player_traits(performer)
                            .is_some_and(|(_, _, sex)| sex == 1),
                        target: &target,
                        your_name: your_name.as_deref().unwrap_or_default(),
                    },
                );
                // A dry ladder is a real outcome, not a failure: SIT/STAND/TRAIN ship blank in
                // every locale, so a vanilla `/sit` prints nothing.
                let Some(event) = event else { continue };
                route(&mut script, &mut windows, &event);
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

/// **`RequestTimePlayed()` out, `TIME_PLAYED_MSG` back** — the two halves of `/played` for an addon.
///
/// The request is a count-drain ([`benilla_ui::script::UiScript::take_played_time_asks`], the pvp
/// queue's shape): the packet is empty, so two asks are two sends. The answer is a one-slot mailbox
/// the net apply pass fills, and it is delivered as the event rather than a return value, because
/// that is how the API answers — `RequestTimePlayed()` itself returns nothing.
///
/// **This does NOT replace the chat breakdown beside it.** `net::apply::chat::played_time` prints
/// the TIME_PLAYED_TOTAL/LEVEL lines because we do not ship `ChatFrame_DisplayTimePlayed`, which is
/// what the reference's own `TIME_PLAYED_MSG` handler does. The two are the reference's two
/// consumers of one packet, not a doubling: an addon that registers the event does its own thing
/// with the numbers, and the player still sees `/played` answer in chat.
pub(crate) fn played_time_bridge(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    commands: Res<NetCommands>,
    answer: Option<ResMut<crate::net::PlayedTimeAnswer>>,
) {
    let Some(mut script) = script else {
        return;
    };
    for _ in 0..script.take_played_time_asks() {
        let _ = commands.0.send(crate::net::ClientCommand::PlayedTime);
    }
    let Some(mut answer) = answer else {
        return;
    };
    if let Some((total, level)) = answer.0.take() {
        script.fire_event(
            "TIME_PLAYED_MSG",
            vec![
                benilla_ui::script::ScriptValue::Int(i64::from(total)),
                benilla_ui::script::ScriptValue::Int(i64::from(level)),
            ],
        );
    }
}
