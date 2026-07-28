//! The group/party arm bodies (decision 0434 §D2, superseded by 0440) for
//! [`super::apply_net_updates`]'s dispatch match. [`GroupState`] mirrors the wire and *composes*
//! the lines; these are the drain-side shims that push what it composed onto the chat log — the
//! way the reference's engine-side errorId→GlobalStrings display does (mapping byte-verified,
//! decision 0440's §5 fold-back). Each `pub(super)` fn here is exactly one arm's body; the match at
//! the call site stays the dispatcher, one call per arm.

use benilla_protocol::messages::{GroupLootInfo, GroupMemberEntry};

use crate::names::NameCache;
use crate::ui_chat::{ChatEvent, ChatEventKind, ChatLog};
use crate::ui_party::GroupState;
use crate::ui_quest::QuestGiver;

use super::super::{NetCommands, SelfGuid};

/// Push the lines a `GroupState::apply_*` composed as CHAT_MSG_SYSTEM — the same seam the other
/// client-composed feeds use (the reference's engine formats these via its errorId→GlobalStrings
/// display and fires them as system chat; benilla's composer hands us the finished strings).
fn push_group_lines(chat_log: &mut ChatLog, lines: Vec<String>) {
    for line in lines {
        chat_log.push_event(ChatEvent::text_only(ChatEventKind::System, line));
    }
}

/// `SMSG_GROUP_INVITE` — someone asked us into their group.
pub(super) fn invited(group: &mut GroupState, chat_log: &mut ChatLog, inviter: &str) {
    push_group_lines(chat_log, group.apply_invited(inviter));
}

/// `SMSG_GROUP_DECLINE` — our invitee said no (sent to the inviter only).
pub(super) fn declined(group: &mut GroupState, chat_log: &mut ChatLog, name: &str) {
    push_group_lines(chat_log, group.apply_declined(name));
}

/// `SMSG_GROUP_UNINVITE` — we were kicked.
pub(super) fn uninvited(group: &mut GroupState, chat_log: &mut ChatLog) {
    push_group_lines(chat_log, group.apply_uninvited());
}

/// `SMSG_GROUP_DESTROYED` — the group is gone outright.
pub(super) fn destroyed(group: &mut GroupState, chat_log: &mut ChatLog) {
    push_group_lines(chat_log, group.apply_destroyed());
}

/// `SMSG_GROUP_SET_LEADER` — the line reads differently when the new leader is us, so the composer
/// needs our own name. It is cache-seeded at login (`session::connected`), so this never asks.
pub(super) fn leader_changed(
    group: &mut GroupState,
    chat_log: &mut ChatLog,
    name: &str,
    self_guid: &SelfGuid,
    names: &mut NameCache,
    net_commands: &NetCommands,
) {
    let own = self_guid
        .0
        .and_then(|g| names.resolve(g, net_commands).map(str::to_string));
    push_group_lines(chat_log, group.apply_leader_changed(name, own.as_deref()));
}

/// `SMSG_GROUP_LIST` — the roster echo (and the join/leave diff's line source). Roster changes move
/// shared-quest availability, so the questgiver sweep re-asks from here (0654).
#[allow(clippy::too_many_arguments)]
pub(super) fn list(
    group: &mut GroupState,
    chat_log: &mut ChatLog,
    quest: &mut QuestGiver,
    group_type: u8,
    own_flags: u8,
    members: Vec<GroupMemberEntry>,
    leader: u64,
    loot: Option<GroupLootInfo>,
) {
    let lines = group.apply_list(group_type, own_flags, members, leader, loot);
    push_group_lines(chat_log, lines);
    quest.bump_reask();
}

/// `SMSG_PARTY_COMMAND_RESULT` — the verdict on an invite/kick/leave we asked for.
pub(super) fn command_result(
    group: &mut GroupState,
    chat_log: &mut ChatLog,
    operation: u32,
    member: &str,
    result: u32,
) {
    push_group_lines(
        chat_log,
        group.apply_command_result(operation, member, result),
    );
}
