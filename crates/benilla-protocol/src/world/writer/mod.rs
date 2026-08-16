//! The world session's **write half** — [`WorldWriter`] and every outbound verb the client has: one
//! method per thing the player can *do*, which is not quite one per opcode (the fifteen chat
//! flavours all ride `CMSG_MESSAGECHAT`, and four cast shapes ride `CMSG_CAST_SPELL`).
//!
//! This file holds only the type, its state, and the one private [`WorldWriter::send`] every verb
//! funnels through; the verbs themselves live in per-family sibling modules. The families **mirror
//! [`crate::messages`]'s own decomposition**, so the module a body builder lives in names the module
//! its send lives in (`messages::vendor::buy_item` ↔ `writer::vendor`'s `buy_item`) — the split rule
//! is mechanical rather than a matter of judgment, which is what keeps it from silting back up
//! (decision 0636; [`channel`]/[`group`]/[`mail`]/[`trade`]/[`duel`] were peeled off ahead of it,
//! starting with decision 0288 phase 1).
//!
//! | module | the family |
//! |---|---|
//! | [`self_movement`] | our own mover: the `MSG_MOVE_*` stream + every mover ack |
//! | [`lifecycle`] | being logged in: ping, logout, the cinematic ack |
//! | [`selection`] | the two sends that set our target: select, inspect |
//! | [`names`] | the ask-once name lookups: player, creature, pet |
//! | [`chat`] | the chat frame's wire: every `MESSAGECHAT` flavour + `/played`, `/random`, `/wave` |
//! | [`channel`] | channel administration: join/leave/list/moderation |
//! | [`spells`] | casting, the four cancels, and the aura cancel |
//! | [`attack`] | the auto-attack toggle: melee start/stop, ranged auto-repeat stop |
//! | [`action_bar`] | the one client-authoritative slot write |
//! | [`pet`] | the pet bar's four intents: press, autocast, call off, drag |
//! | [`progression`] | the talent spend |
//! | [`skills`] | the skills pane's abandon |
//! | [`pose`] | the client-volunteered body state: sheath, stand, the mounted flourish |
//! | [`items`] | bags and equipment: use, equip, swap/split/destroy, the template ask |
//! | [`loot`] | the loot window: open, take, coin, close, roll |
//! | [`death`] | the corpse run: release, corpse query, reclaim, spirit healer, res answer |
//! | [`gossip`] | the front door to every NPC service window |
//! | [`vendor`] | buy, sell, buy back, repair |
//! | [`bank`] | open, buy a slot, deposit, withdraw |
//! | [`trainer`] | the service list refresh + the purchase |
//! | [`taxi`] | the flight master: status, map, the two flight verbs |
//! | [`quest`] | the questgiver dialog walk + the two quest-log verbs |
//! | [`gameobject`] | the one USE verb + the template ask |
//! | [`group`] | party/raid: invite, kick, leader, loot method, icons, ready checks |
//! | [`guild`] | the guild: the two cache asks, invitations, member verbs, rank administration |
//! | [`trade`] | the player-trade dance |
//! | [`duel`] | accept and cancel |
//! | [`mail`] | the mailbox |
//! | [`pvp`] | the one PvP-flag toggle |
//! | [`social`] | friends, ignores, and `/who` |
//! | [`area_trigger`] | the one "I walked into trigger N" report |

use std::net::TcpStream;

use anyhow::Result;
use benilla_srp::vanilla_header::EncrypterHalf;

use super::send_packet;

mod action_bar;
mod area_trigger;
mod attack;
mod bank;
mod binder;
mod channel;
mod chat;
mod death;
mod duel;
mod gameobject;
mod gossip;
mod group;
mod guild;
mod items;
mod lifecycle;
mod loot;
mod mail;
mod names;
mod pet;
mod pose;
mod progression;
mod pvp;
mod quest;
mod reputation;
mod selection;
mod self_movement;
mod skills;
mod social;
mod spells;
mod taxi;
mod trade;
mod trainer;
mod vendor;

/// Write half of a split [`WorldSession`](super::WorldSession) — owns a cloned socket + the encrypter. Used to send our own
/// movement (`MSG_MOVE_*`). The active player must already be the confirmed mover (see
/// [`WorldSession::set_active_mover`](super::WorldSession::set_active_mover)). Coordinates are raw WoW yards; `orientation` is radians.
pub struct WorldWriter {
    pub(super) stream: TcpStream,
    pub(super) encrypter: EncrypterHalf,
    /// The language every chat send carries — the logged-in character's faction tongue,
    /// inherited from the session at split (set by `WorldSession::player_login` off the roster's
    /// race). Load-bearing: vmangos drops the whole message (dot-commands included) when the
    /// character doesn't know the language — a Horde character sending Common gets only an
    /// `SMSG_NOTIFICATION` back, never the say / the command.
    pub(super) chat_language: u32,
}

impl WorldWriter {
    /// Frame, encrypt, and write one packet. Private, and the sole write path — every verb in the
    /// family modules goes through here, so there is exactly one place framing/encryption happens.
    /// Visible to them because a private item is in scope throughout its module's descendants.
    fn send(&mut self, opcode: u16, body: &[u8]) -> Result<()> {
        send_packet(&mut self.stream, Some(&mut self.encrypter), opcode, body)
    }
}
