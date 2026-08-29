//! Session-lifecycle arm bodies for [`super::apply_net_updates`]'s dispatch match — the connection
//! edges (the login stages, character select, entering the world, logout, the disconnect
//! teardown), our own teleport/worldport snaps, the server clock, and the login reputation store.
//! Each `pub(super)` fn here is exactly one arm's body; the match at the call site stays the
//! dispatcher, one call per arm.

use benilla_protocol::messages::Character;
use bevy::prelude::*;

use crate::items::Items;
use crate::names::NameCache;
use crate::ui_chat::ChatLog;
use crate::ui_gossip::GossipState;
use crate::ui_loot::LootState;
use crate::ui_mail::MailOpen;
use crate::ui_merchant::MerchantOpen;
use crate::ui_quest::QuestGiver;
use crate::ui_quest_log::QuestLog;
use crate::ui_taxi::TaxiState;
use crate::ui_trainer::TrainerOpen;

use super::super::{
    CharActionResultMessage, CharListMessage, ClientCommand, DisconnectedMessage, DroppedOpcodes,
    EnteredWorldMessage, GameTime, GuidIndex, LoggedOutMessage, LoginFailedMessage,
    LoginStageMessage, NetCommands, NetStatus, PendingTransfer, PingShared, Reputations, SelfGuid,
    ServerTime, ServerWallClock, TeleportMessage, WorldportMessage,
};

/// The pre-logon handshake reached a new stage (decision 0539) — the login screen's dialog reads it.
pub(super) fn login_stage(
    stage: benilla_protocol::LoginStage,
    out: &mut MessageWriter<LoginStageMessage>,
) {
    out.write(LoginStageMessage { stage });
}

/// A login attempt failed before the roster (decision 0539): the IO thread is back at its pre-logon
/// park, and [`crate::login`]'s policy decides what happens next.
pub(super) fn login_failed(
    refusal: Option<benilla_protocol::LoginRefusal>,
    reason: String,
    terminal: bool,
    dial: Option<benilla_protocol::DialFailure>,
    out: &mut MessageWriter<LoginFailedMessage>,
) {
    out.write(LoginFailedMessage {
        refusal,
        reason,
        terminal,
        dial,
    });
}

/// The verdict on a character create/delete (`SMSG_CHAR_CREATE`/`SMSG_CHAR_DELETE`) — the glue
/// screen turns the code into its own refusal string.
pub(super) fn char_action_result(
    action: benilla_protocol::CharAction,
    code: u8,
    out: &mut MessageWriter<CharActionResultMessage>,
) {
    out.write(CharActionResultMessage { action, code });
}

/// The account's character roster (`SMSG_CHAR_ENUM`): the world socket is authenticated and parked
/// at character select — surface the list (+ the connected realm's identity) to the glue screen.
pub(super) fn character_list(
    characters: Vec<Character>,
    realm: Option<benilla_protocol::RealmInfo>,
    status: &mut NetStatus,
    char_lists: &mut MessageWriter<CharListMessage>,
) {
    info!(
        "net: character select — {} character(s) on the account",
        characters.len()
    );
    // A roster in hand IS a live link: clear the last failure so the select banner drops its
    // "Server down" note. Without this, the logout path sticks it on permanently — the relist
    // cycle synthesizes `Disconnected("logged out")` for the world teardown (decision 0065's
    // path), and nothing else clears `last_reason` until the next world entry.
    status.last_reason = None;
    char_lists.write(CharListMessage { characters, realm });
}

/// A cinematic sequence was triggered (`SMSG_TRIGGER_CINEMATIC`).
pub(super) fn cinematic_triggered(cinematic_id: u32, net_commands: &NetCommands) {
    // benilla doesn't play cinematics yet — ack immediately (a real player's ESC
    // skip). Load-bearing, not cosmetic: while a cinematic runs unacked, vmangos
    // re-anchors object visibility to the flying camera (`Player::UpdateCinematic`)
    // and every NPC around the body despawns until relog. A first login's race intro
    // is the common trigger. The future cinematic arc hooks this event for playback.
    info!("net: cinematic {cinematic_id} triggered — skipping (unimplemented)");
    let _ = net_commands.0.send(ClientCommand::CompleteCinematic);
}

/// We are in the world (the IO thread's first in-world event): record our guid, flip the status,
/// and seed the name cache with our own name.
pub(super) fn connected(
    guid: u64,
    name: String,
    self_guid: &mut SelfGuid,
    status: &mut NetStatus,
    names: &mut NameCache,
    entered_world: &mut MessageWriter<EnteredWorldMessage>,
) {
    self_guid.0 = Some(guid);
    status.connected = true;
    status.last_reason = None;
    info!("net: in world as {name} (guid {guid})");
    // Our own name came with the login — seed the cache so "player" never queries.
    names.insert_player(guid, name, None);
    entered_world.write(EnteredWorldMessage);
}

/// The server confirmed our logout (`SMSG_LOGOUT_COMPLETE`) — back to character select.
pub(super) fn logged_out(
    commands: &mut Commands,
    index: &mut GuidIndex,
    self_guid: &mut SelfGuid,
    logged_out: &mut MessageWriter<LoggedOutMessage>,
) {
    // A deliberate logout ends this *character's* session, not just the socket: unlike
    // the disconnect teardown below (which keeps the self avatar as the local puppet for
    // a seamless same-char reconnect, decision 0065), the avatar goes too — the next
    // login may be a different character. Clearing `SelfGuid` first makes the follow-up
    // Disconnected teardown total.
    info!("net: logged out — back to character select");
    if let Some(guid) = self_guid.0.take() {
        if let Some(e) = index.0.remove(&guid) {
            commands.entity(e).despawn();
        }
    }
    logged_out.write(LoggedOutMessage);
}

/// The session ended (socket closed / handshake failure): tear down the streamed world and clear
/// every session-scoped cache.
#[allow(clippy::too_many_arguments)]
pub(super) fn disconnected(
    reason: String,
    end: benilla_protocol::SessionEnd,
    commands: &mut Commands,
    index: &mut GuidIndex,
    self_guid: &mut SelfGuid,
    status: &mut NetStatus,
    names: &mut NameCache,
    items: &mut Items,
    gossip: &mut GossipState,
    merchant: &mut MerchantOpen,
    trainer_open: &mut TrainerOpen,
    loot: &mut LootState,
    loot_latch: &mut crate::ui_loot::LootLatch,
    loot_rolls: &mut crate::ui_loot_roll::LootRolls,
    chat_log: &mut ChatLog,
    quest: &mut QuestGiver,
    quest_log: &mut QuestLog,
    death_net: &mut crate::death::DeathNet,
    group: &mut crate::ui_party::GroupState,
    taxi: &mut TaxiState,
    mail: &mut MailOpen,
    mail_pending: &mut crate::ui_mail::MailPending,
    trade: &mut crate::ui_trade::TradeSession,
    auction: &mut crate::ui_auction::AuctionOpen,
    bank: &mut crate::ui_bank::BankOpen,
    duel: &mut crate::ui_duel::DuelState,
    social: &mut crate::ui_social::SocialState,
    guild: &mut crate::ui_guild::GuildState,
    pending_transfer: &mut PendingTransfer,
    disconnects: &mut MessageWriter<DisconnectedMessage>,
) {
    // The reconnect-policy feed first (decision 0539): [`crate::login`] reads it as "the IO thread
    // is back at its pre-logon park".
    // Is the session over, or is this the pause inside one? Settled once, here, and carried on the
    // message to every other reader (decision 1262).
    let msg = DisconnectedMessage::new(reason.clone(), end);
    let over = msg.session_over;
    disconnects.write(msg);
    warn!("net: {reason} — tearing down the streamed world");
    // An announced-but-unfinished far teleport died with the socket.
    pending_transfer.0 = None;
    status.connected = false;
    // A dead socket's last RTT is stale; the write thread resets the shared ping clock itself
    // when the reconnect hands it a fresh writer. The averaged ring goes with it — the next
    // connection's latency is its own, so `GetNetStats` reads unmeasured until its first pong.
    status.clear_rtt();
    // Teardown (decision 0065): despawn every streamed entity except the self avatar —
    // it stays the local puppet (controller + camera keep working); the reconnect's
    // re-create refreshes it in place. Immediate despawn, not `DespawnFade`: a
    // connection loss is not a world event, and index-less fading entities would race
    // the reconnect's re-creates. Entities already mid-fade left the index earlier and
    // finish fading on their own.
    //
    // **Unless the session is over** (decision 1262), and then the avatar goes too, exactly as
    // [`logged_out`] takes it: 0065 spares it *for the reconnect*, and with no reconnect coming
    // that spared body is a puppet with no server behind it. Keeping it was the free camera —
    // `SelfGuid` set, no entity, an entry that never finished — so the fact that decides whether
    // anything reconnects has to be the same fact that decides whether the body stays.
    let keep = if over { None } else { self_guid.0 };
    if over {
        self_guid.0 = None;
    }
    index.0.retain(|guid, e| {
        if Some(*guid) == keep {
            return true;
        }
        commands.entity(*e).despawn();
        false
    });
    status.last_reason = Some(reason);
    // In-flight name queries died with the socket; let the next resolve re-ask.
    names.clear_pending();
    items.clear_session();
    gossip.clear_session();
    merchant.clear_session();
    trainer_open.clear_session();
    loot.clear_session();
    loot_latch.0 = None; // the kneel latch dies with the socket (unconditional here)
    loot_rolls.clear(); // open group rolls die with the socket (decision 0591)
    chat_log.clear_session();
    quest.clear_session();
    quest_log.clear_session();
    group.clear_session();
    taxi.clear_session();
    mail.clear_session();
    // The arrival countdown is login-scoped (decision 0544 P3): a fresh login re-queries
    // `MSG_QUERY_NEXT_MAIL_TIME` at world-enter, so nothing carries over across a reconnect.
    *mail_pending = crate::ui_mail::MailPending::default();
    // An open auction house dies with the socket (decision 1511): every auction command
    // re-validates the auctioneer server-side, so a session that survived a reconnect would be a
    // window whose every button silently failed.
    auction.clear_session();
    // An open trade dies with the socket too (decision 0592) — the reconnect starts with no trade.
    trade.clear_session();
    // The bank window dies with the socket (decision 0604) — a reconnect re-opens via the banker.
    bank.clear_session();
    // A pending challenge, a running duel, and its countdown all die with the socket
    // (decision 0633) — the server drops the duel too (`Player::DuelComplete(DUEL_FLED)` on
    // logout), and a stale arbiter guid would make the next AcceptDuel echo a dead object.
    *duel = crate::ui_duel::DuelState::default();
    // The friend/ignore lists and the last `/who` are session state too (decision 0668): the
    // server re-pushes both lists at the next login, and a stale ignore list would silence the
    // wrong guids after a reconnect renumbers nothing but re-streams everything.
    *social = crate::ui_social::SocialState::default();
    // The guild session is login-scoped the same way (decision 1257) — and more strictly, because
    // the next login may be a *different character*, whose guild id, rank, rights and roster share
    // nothing with this one's. The identity cache goes too: it is keyed by guild id, so it would
    // survive correctly, but the reference's own is backed by `guildcache.wdb` and re-primed
    // lazily, and keeping a cache alive across a socket only to save one query is not worth the
    // one wrong name a renamed guild would show.
    *guild = crate::ui_guild::GuildState::default();
    // The death stores are session-scoped too: a reclaim expiry, resurrect offer, or corpse
    // marker must not survive the socket (the reconnect re-sends the reclaim delay when dead).
    *death_net = crate::death::DeathNet::default();
}

/// A teleport ack request (`MSG_MOVE_TELEPORT_ACK`) — only our own matters (the ack resumes our
/// movement); the controller consumes the message.
pub(super) fn teleport(
    guid: u64,
    counter: u32,
    position: [f32; 3],
    orientation: f32,
    self_guid: &SelfGuid,
    teleports: &mut MessageWriter<TeleportMessage>,
) {
    // Only our own teleports matter to the controller (the ack resumes our movement).
    if self_guid.0 == Some(guid) {
        teleports.write(TeleportMessage {
            guid,
            counter,
            position,
            orientation,
        });
    }
}

/// The far-teleport preamble (`SMSG_TRANSFER_PENDING`): latch it for the coming worldport —
/// its transport block decides whether NEW_WORLD's coordinates are boat-local (decision 0455).
pub(super) fn transfer_pending(
    map_id: u32,
    transport_entry: Option<u32>,
    pending: &mut PendingTransfer,
) {
    match transport_entry {
        Some(entry) => info!("net: transfer pending → map {map_id} riding transport {entry}"),
        None => info!("net: transfer pending → map {map_id}"),
    }
    pending.0 = Some(crate::net::PendingTransferInfo {
        map_id,
        transport_entry,
    });
}

/// `SMSG_TRANSFER_ABORTED`: the announced transfer won't happen — clear the latch.
pub(super) fn transfer_aborted(reason: u8, pending: &mut PendingTransfer) {
    warn!("net: transfer aborted (reason {reason})");
    pending.0 = None;
}

/// A cross-map transfer (`SMSG_NEW_WORLD` / `SMSG_LOGIN_VERIFY_WORLD`): the new map streams a
/// fresh object set — drop everything we were tracking, then hand the app the destination.
///
/// One exception to the purge (decision 0455): an armed transport whose timetable touches the
/// destination map is **spared**, entity and index entry both. Transports are client-simulated
/// global objects on one continuous two-continent clock — a spared boat sails straight through
/// the seam (the `CurrentMap` flip itself flips which legs render), keeping the ride attachment
/// and the deck collider valid the whole way; the server's post-ack re-create then refreshes its
/// anchor in place. Boats whose paths never reach the new map despawn like everything else.
#[allow(clippy::too_many_arguments)] // one dispatch arm's full context, like `disconnected`
pub(super) fn worldport(
    map_id: u32,
    position: [f32; 3],
    orientation: f32,
    needs_ack: bool,
    commands: &mut Commands,
    index: &mut GuidIndex,
    pending: &mut PendingTransfer,
    transports: &Query<&crate::transport::Transport>,
    worldports: &mut MessageWriter<WorldportMessage>,
) {
    index.0.retain(|guid, e| {
        if transports.get(*e).is_ok_and(|t| t.touches_map(map_id)) {
            // info, not debug: rare (worldports only) and load-bearing — the crossing's whole
            // mechanism hangs on this line firing for the ridden boat.
            info!("worldport: sparing transport {guid:#x} (its path touches map {map_id})");
            return true;
        }
        commands.entity(*e).try_despawn();
        false
    });
    let announced = pending.0.take();
    if let Some(p) = announced {
        // vmangos pairs every NEW_WORLD with a same-map TRANSFER_PENDING; a mismatch means we
        // mis-latched (or the server changed its mind) — worth a line, not a failure.
        if p.map_id != map_id {
            warn!(
                "worldport: NEW_WORLD map {map_id} ≠ announced transfer map {} — using {map_id}",
                p.map_id
            );
        }
    }
    let transport_entry = announced.and_then(|p| p.transport_entry);
    worldports.write(WorldportMessage {
        map_id,
        position,
        orientation,
        needs_ack,
        transport_entry,
    });
}

/// The server game clock (`SMSG_LOGIN_SETTIMESPEED`) — drives the day/night lighting.
pub(super) fn time_speed(
    hours: u8,
    minutes: u8,
    day_serial: u32,
    timescale: f32,
    server_time: &mut ServerTime,
) {
    if server_time.0.is_none() {
        info!("net: server game-time {hours:02}:{minutes:02} (drives lighting)");
    }
    server_time.0 = Some(GameTime::new(hours, minutes, day_serial, timescale));
}

/// The server **wall** clock (`SMSG_QUERY_TIME_RESPONSE`, answering the world-enter
/// `CMSG_QUERY_TIME`) — the epoch the absolute descriptor stamps are dated in, and so the origin of
/// every countdown drawn from one (today: the timed-quest timer, decision 1150). Nothing to do with
/// [`time_speed`] above, which is the in-game day/night clock.
///
/// Logged once per session at info, with the skew against our own clock: that number is exactly
/// what a "the countdown is wrong by a constant" report would be about, and it costs one line.
pub(super) fn server_unix_time(unix_time: u32, clock: &mut ServerWallClock) {
    if clock.0.is_none() {
        let local = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        info!(
            "net: server wall clock {unix_time} (local clock differs by {}s)",
            local - i64::from(unix_time)
        );
    }
    clock.sample(unix_time);
}

/// The login reputation store (`SMSG_INITIALIZE_FACTIONS`).
pub(super) fn reputations(standings: Vec<(u8, i32)>, reputations: &mut Reputations) {
    info!("net: reputation store ({} slots)", standings.len());
    reputations.0 = standings;
}

/// A mid-session standing delta (`SMSG_SET_FACTION_STANDING`): overwrite the changed slots,
/// growing the store for a list id past the login snapshot (flags default 0 — the delta carries
/// none), and **auto-reveal** each one.
///
/// The auto-reveal is the client's own (wow-5875-re `reputation-panel-law.md`, the `0x124` handler):
/// gaining reputation with a faction makes it visible, unless the slot carries `HIDDEN` — which is
/// exactly what that bit is for, and is why it is not a list gate. The server pushes an
/// `SMSG_SET_FACTION_VISIBLE` for the same slot in most cases (vmangos `SetOneFactionReputation`
/// calls `SetVisible`), so this is usually belt to that braces; it matters when the reveal and the
/// standing arrive in the other order, and it is what the client does regardless.
pub(super) fn reputation_delta(
    standings: Vec<(u32, i32)>,
    reputations: &mut Reputations,
    quest: &mut QuestGiver,
) {
    use benilla_formats::faction_flags as flag;
    for (list_id, standing) in standings {
        let i = list_id as usize;
        if reputations.0.len() <= i {
            reputations.0.resize(i + 1, (0, 0));
        }
        reputations.0[i].1 = standing;
        if reputations.0[i].0 & flag::HIDDEN == 0 {
            reputations.0[i].0 |= flag::VISIBLE;
        }
    }
    // A standing change is a questgiver-status input (`SatisfyQuestReputation`, and the reaction
    // gate): the reference sweeps from this handler too (0654).
    quest.bump_reask();
}

/// A faction became visible (`SMSG_SET_FACTION_VISIBLE`): lift `FACTION_FLAG_VISIBLE` on that slot
/// and nothing else.
///
/// The server pushes this the first time the player meets a faction, and it carries **no
/// standing** — the slot's standing was already correct and stays untouched. Dropping it is the
/// silent failure it exists to prevent: the pane keys row membership off this bit, so a faction met
/// mid-session would keep accruing reputation the player could never see.
pub(super) fn reputation_visible(list_id: u32, reputations: &mut Reputations) {
    let i = list_id as usize;
    if reputations.0.len() <= i {
        reputations.0.resize(i + 1, (0, 0));
    }
    reputations.0[i].0 |= benilla_formats::faction_flags::VISIBLE;
}

/// The keepalive echo (`SMSG_PONG`): match it against the shared ping clock to measure the
/// round trip. Stored back on the clock (the next ping's lastRtt field — what the real client
/// reports), pushed into the RTT ring the UI's `GetNetStats` averages, and surfaced as the panel's
/// latency readout. A stale or mismatched sequence (a pong straddling a reconnect) is dropped.
pub(super) fn pong(sequence: u32, ping: &PingShared, status: &mut NetStatus) {
    let mut clock = ping.0.lock().expect("ping clock");
    if let Some(sent) = clock.sent_at.filter(|_| clock.sequence == sequence) {
        let rtt = sent.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
        if status.latency_ms.is_none() {
            info!("net: pong seq={sequence} rtt={rtt}ms (keepalive live)");
        }
        clock.last_rtt_ms = Some(rtt);
        status.record_rtt(rtt);
    }
}

/// The dropped-packet tally (the wire-coverage instrument): count it, and announce each opcode's
/// FIRST drop at info — visible in any log without the panel open, and one line per opcode per
/// run, so it can never flood.
pub(super) fn packet_dropped(opcode: u16, unparseable: bool, dropped: &mut DroppedOpcodes) {
    let tally = dropped.0.entry(opcode).or_default();
    if tally.unknown + tally.unparseable == 0 {
        info!(
            "net: dropped packet {opcode:#06x} ({}) — {} (first occurrence; tallying)",
            benilla_protocol::messages::opcode_name(opcode).unwrap_or("?"),
            if unparseable {
                "parser errored"
            } else {
                "no parser"
            },
        );
    }
    if unparseable {
        tally.unparseable += 1;
    } else {
        tally.unknown += 1;
    }
}
