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
///
/// **A roster entry is a sighting** (decision 1564): every member guid is warmed into the
/// [`NameCache`] here, the same ask-once discipline `net::apply::objects` applies the moment a unit
/// streams in. The roster wire carries a member's *name*, so this is not asked for the name — it is
/// asked for the `(race, class, gender)` triple that rides the same answer, and which is the ONLY
/// source of those three for a member we never see: their descriptor never arrives. Two surfaces
/// read them and both were empty for an out-of-area member before this — the raid grid's class
/// column (`ui_party::feed::raid_roster`, whose own-row twin of this hole 1549 §7 found live), and
/// the party frame's 2D portrait stand-in (`portrait::temporary_portrait`, report B315).
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
    names: &mut NameCache,
    net_commands: &NetCommands,
) {
    for m in &members {
        let _ = names.resolve(m.guid, net_commands);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::ClientCommand;
    use benilla_protocol::guid;

    fn member(g: u64, name: &str) -> GroupMemberEntry {
        GroupMemberEntry {
            name: name.into(),
            guid: g,
            status: 1, // ONLINE
            flags: 0,
        }
    }

    /// The roster edge is where a member we may never SEE becomes askable. Their descriptor is the
    /// only other source of race/class/gender, and it never arrives while they are out of the local
    /// area — so without this ask the raid grid's class column and the party frame's portrait
    /// stand-in are both permanently blank for exactly the members that need them (B315).
    ///
    /// Ask-ONCE: a re-sent roster (every join, leave, loot-method change re-sends the whole list)
    /// must not re-ask, or a busy group would spam a query per member per packet.
    #[test]
    fn the_roster_warms_every_member_into_the_name_cache_once() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let net = NetCommands(tx);
        let (mut group, mut chat_log, mut quest) = (
            GroupState::default(),
            ChatLog::default(),
            QuestGiver::default(),
        );
        let mut names = NameCache::default();
        // A player guid: `counter | (high << 48)` — the shape `NameCache::resolve` routes on.
        let player_guid = |counter: u64| counter | (u64::from(guid::HIGH_PLAYER) << 48);
        let (leader, far) = (player_guid(7), player_guid(8));

        list(
            &mut group,
            &mut chat_log,
            &mut quest,
            0,
            0,
            vec![member(leader, "Frostshake"), member(far, "Thalyn")],
            leader,
            None,
            &mut names,
            &net,
        );

        let asked: Vec<u64> = rx
            .try_iter()
            .filter_map(|c| match c {
                ClientCommand::NameQuery { guid } => Some(guid),
                _ => None,
            })
            .collect();
        assert_eq!(
            asked,
            vec![leader, far],
            "both members asked, in roster order"
        );

        // The answer lands for one of them; the re-sent roster asks for neither.
        names.insert_player(leader, "Frostshake".into(), Some((1, 4, 1)));
        list(
            &mut group,
            &mut chat_log,
            &mut quest,
            0,
            0,
            vec![member(leader, "Frostshake"), member(far, "Thalyn")],
            leader,
            None,
            &mut names,
            &net,
        );
        assert!(
            rx.try_iter()
                .all(|c| !matches!(c, ClientCommand::NameQuery { .. })),
            "a re-sent roster re-asks nothing"
        );
        assert_eq!(names.player_traits(leader), Some((1, 4, 1)));
    }
}
