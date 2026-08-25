//! Group/party session state — the `SMSG_GROUP_LIST` wire mirror and the client-composed party
//! system lines (decision 0434 §D2).
//!
//! The 1.12 server sends **no text** for party events (vmangos, exhaustively grepped): joins and
//! leaves appear only as roster diffs on `SMSG_GROUP_LIST`, a kick additionally announces itself
//! to the kicked player as the empty-body `SMSG_GROUP_UNINVITE`, and `SMSG_PARTY_COMMAND_RESULT`
//! acks only the acting player. The real client composes every "%s joins the party." line
//! engine-side from those packets against the GlobalStrings templates (the FrameXML never touches
//! the `ERR_*` group strings; the mechanism is wow-re's verified errorId→GlobalStrings display,
//! `CGGameUI::DisplayError` 0x496720). benilla does the same here: [`GroupState`] mirrors the
//! wire, and its `apply_*` methods return the finished lines for the net drain to push as
//! CHAT_MSG_SYSTEM. The mapping is **byte-verified** (decision 0440's §5 fold-back, wow-re
//! `system/object-layer/scratch/party-group-wire.md`, commit `a07c311c`): each opcode prints
//! its own line and the GROUP_LIST diff prints the rest — there is NO empty-list state machine.
//!
//! The line law (0440):
//! - GROUP_LIST runs an **ungated** two-way roster diff: new guid → "%s joins the party."
//!   (`0x5e6c19` — including everyone already there on a first roster), vanished guid →
//!   "%s leaves the party." (`0x5e6d37` — including the whole roster on our own empty list).
//! - "You leave the group." comes ONLY from `SMSG_PARTY_COMMAND_RESULT(LEAVE, ok)` (`0x5e690b`).
//! - `SMSG_GROUP_UNINVITE` (kick) → "You have been removed from the group.", unconditional.
//! - `SMSG_GROUP_DESTROYED` → "Your group has been disbanded.", gated on being grouped. The
//!   vmangos 2-man collapse sends no GROUP_DESTROYED (`Disband(hideDestroy=true)`,
//!   `Group.cpp:533`), so the survivor sees only the leave-diff line — rendered faithfully
//!   (the 0086 server-divergence precedent).

use std::collections::HashMap;

use benilla_protocol::messages::{
    party_operation, party_result, GroupLootInfo, GroupMemberEntry, PartyMemberStatsInfo,
};
use bevy::prelude::*;

use crate::ui_script::UiInput;

mod feed;
pub(crate) use feed::{raid_row_guid, synthetic_raid, synthetic_roster, PARTY_TOKENS, RAID_TOKENS};

pub(crate) struct UiPartyPlugin;

impl Plugin for UiPartyPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GroupState>().add_systems(
            Update,
            (
                feed::feed_party.before(UiInput),
                feed::drain_party.after(UiInput),
            ),
        );
    }
}

/// The party/raid session mirror — `SMSG_GROUP_LIST` verbatim plus the invite/stats side-state.
/// Filled by the net drain's group arms, cleared on disconnect beside the other per-login
/// resources (decision 0434 §D2). The party *frames* (phase 2) read this through the merged view:
/// a streamed member's live `ObjectStore` wins; the [`Self::stats`] snapshot covers the rest.
#[derive(Resource, Default)]
pub struct GroupState {
    /// True while we hold a roster (any `SMSG_GROUP_LIST` with `leader != 0`; the all-zero
    /// "you left" shape flips it back off).
    pub in_group: bool,
    /// `0` party, `1` raid (`GROUPTYPE_*`, vmangos `Group.h:116-120`).
    pub group_type: u8,
    /// Our own subgroup (bits 0-2) + raid-assistant bit (`0x80`).
    pub own_flags: u8,
    /// The *other* members, wire order (the recipient never appears in its own list).
    pub members: Vec<GroupMemberEntry>,
    pub leader: u64,
    /// The loot tail — present whenever the wire list was non-empty.
    pub loot: Option<GroupLootInfo>,
    /// The inviter's name while an invite popup should be up (`SMSG_GROUP_INVITE` set it; accept/
    /// decline clears it — phase 2 wires the StaticPopup on top).
    pub pending_invite: Option<String>,
    /// Out-of-range member snapshots from `SMSG_PARTY_MEMBER_STATS(_FULL)`, keyed by guid. Delta
    /// packets merge field-wise; `_FULL` replaces. Entries for guids that leave the roster are
    /// dropped at the next list.
    pub stats: HashMap<u64, PartyMemberStatsInfo>,
    /// The raid-target icon board (`MSG_RAID_TARGET_UPDATE`): slot = icon id 0-7 (star..skull),
    /// value = marked guid, `0` = unset. Consumed by the icon renders in phase 6.
    pub raid_targets: [u64; 8],
    /// `/partytest` sandbox mode: the roster is synthetic, so the drain applies group-mutating
    /// intents to this mirror LOCALLY (mimicking the server echo that a real group would send)
    /// instead of dispatching CMSGs into a void. Any real `SMSG_GROUP_LIST` switches it off —
    /// the wire always wins.
    pub test: bool,
    /// Our saved raid lockouts (`SMSG_RAID_INSTANCE_INFO`) — the Raid tab's Raid Info panel
    /// (decision 1549). The answer replaces the list wholesale, empty included.
    ///
    /// A lockout is per-CHARACTER, not per-group, so this is the one field here that is not a
    /// group fact. It lives here anyway for the reason that matters: it is per-SESSION state that
    /// must die with the socket, and [`Self::clear_session`] is that guarantee — a second
    /// resource would be a second thing to remember to clear.
    pub saved_instances: Vec<benilla_protocol::messages::RaidInstanceEntry>,
    /// **How many times the server has answered** — one per `SMSG_RAID_INSTANCE_INFO`. A TICKET,
    /// not a flag (the [`Self::ready_check`] shape), and the difference is load-bearing: the
    /// reference decides its Raid Info button on the SECOND `UPDATE_INSTANCE_INFO`, because
    /// `RaidFrame.hasRaidInfo` swallows the first. A feed that fires that event on a *diff* never
    /// reaches the second for the ordinary player — their lockout list is empty, every answer says
    /// so, nothing ever changes, and the button they should not be able to press stays live (1561).
    ///
    /// The real client fires per PACKET, not per change. VERIFIED off the bytes: the handler at
    /// `0x49e070` takes `jbe 0x49e19d` (`0x49e0d8`) when the entry count is zero — straight past
    /// the parse loop to `mov ecx, 0x21b` (539 = `UPDATE_INSTANCE_INFO`) and the one
    /// `call FrameScript_SignalEvent 0x703e50` at `0x49e1a7`, which is the function's only exit
    /// and the event's only fire site in the binary (`re/events/event-firesites.tsv`). The empty
    /// answer signals exactly like a full one.
    pub saved_instances_answers: u32,
    /// A ready-check TICKET, not a flag: every `MSG_RAID_READY_CHECK` open bumps it, so the feed
    /// fires `READY_CHECK` on a counter edge and a second check while the first popup is still up
    /// re-arms it. A boolean could not tell the two apart.
    pub ready_check: u32,
}

// The GlobalStrings templates, quoted verbatim from the reference client's own patch chain
// (decision 0246 extraction; `GlobalStrings.lua` line numbers cited per constant).
const JOINED_PARTY: &str = "%s joins the party."; // ERR_JOINED_GROUP_S (GlobalStrings:1665)
const LEFT_PARTY: &str = "%s leaves the party."; // ERR_LEFT_GROUP_S (GlobalStrings:1670)
const LEFT_GROUP_YOU: &str = "You leave the group."; // ERR_LEFT_GROUP_YOU (GlobalStrings:1671)
const GROUP_DISBANDED: &str = "Your group has been disbanded."; // ERR_GROUP_DISBANDED (GlobalStrings:1582)
const UNINVITE_YOU: &str = "You have been removed from the group."; // ERR_UNINVITE_YOU (GlobalStrings:1904)
const INVITED_TO_GROUP: &str = "%s has invited you to join a group."; // ERR_INVITED_TO_GROUP_S (GlobalStrings:1654)
const INVITE_PLAYER: &str = "You have invited %s to join your group."; // ERR_INVITE_PLAYER_S (GlobalStrings:1657)
const DECLINE_GROUP: &str = "%s declines your group invitation."; // ERR_DECLINE_GROUP_S (GlobalStrings:1545)
const NEW_LEADER: &str = "%s is now the group leader."; // ERR_NEW_LEADER_S (GlobalStrings:1724)
const NEW_LEADER_YOU: &str = "You are now the group leader."; // ERR_NEW_LEADER_YOU (GlobalStrings:1725)
const RAID_MEMBER_ADDED: &str = "%s has joined the raid group"; // ERR_RAID_MEMBER_ADDED_S (GlobalStrings:1824)
const RAID_MEMBER_REMOVED: &str = "%s has left the raid group"; // ERR_RAID_MEMBER_REMOVED_S (GlobalStrings:1825)
const RAID_YOU_JOINED: &str = "You have joined a raid group"; // ERR_RAID_YOU_JOINED (GlobalStrings:1826)

/// `format!`-free "%s" substitution — the templates are quoted verbatim from GlobalStrings, so
/// they carry printf placeholders, not Rust ones.
fn fmt_s(template: &str, arg: &str) -> String {
    template.replacen("%s", arg, 1)
}

impl GroupState {
    /// Apply one `SMSG_GROUP_LIST` and compose the system lines its roster diff implies. The
    /// diff is **ungated** (the 0440 byte law, `0x5e6c19`/`0x5e6d37`): the cache starts empty,
    /// so a first roster prints a join line for every member already there, and the all-zero
    /// "you left" shape prints a leave line per stale member — every observable composite
    /// (kick, voluntary leave, disband) is per-opcode lines stacking over this one diff.
    pub fn apply_list(
        &mut self,
        group_type: u8,
        own_flags: u8,
        members: Vec<GroupMemberEntry>,
        leader: u64,
        loot: Option<GroupLootInfo>,
    ) -> Vec<String> {
        let mut lines = Vec::new();
        // The all-zero shape is "you are no longer in a group" (vmangos sends a default packet,
        // `Group.h:257`); `leader == 0` is its reliable discriminator (a live group always names
        // one, and a solo-leader group has an empty member list *with* a leader guid).
        let leaving = leader == 0;
        // Raid wording keys off whichever side of the transition was a raid. INTERIM (0440):
        // the §5 pinned the party line's `groupType==0` gate; the raid twin's exact trigger
        // wasn't walked.
        let (added_tpl, removed_tpl) = if group_type == 1 || (leaving && self.group_type == 1) {
            (RAID_MEMBER_ADDED, RAID_MEMBER_REMOVED)
        } else {
            (JOINED_PARTY, LEFT_PARTY)
        };
        for m in &members {
            if !self.members.iter().any(|old| old.guid == m.guid) {
                lines.push(fmt_s(added_tpl, &m.name));
            }
        }
        for old in &self.members {
            if !members.iter().any(|m| m.guid == old.guid) {
                lines.push(fmt_s(removed_tpl, &old.name));
            }
        }
        if leaving {
            *self = GroupState::default();
            return lines;
        }
        // Our own party→raid conversion (or joining straight into a raid). INTERIM like the
        // wording pick above.
        if group_type == 1 && self.group_type != 1 {
            lines.push(RAID_YOU_JOINED.to_string());
        }

        // Drop stats snapshots for members no longer on the roster.
        self.stats
            .retain(|guid, _| members.iter().any(|m| m.guid == *guid));

        self.in_group = true;
        self.group_type = group_type;
        self.own_flags = own_flags;
        self.members = members;
        self.leader = leader;
        self.loot = loot;
        // A list apply is the real wire by default; `synthetic_roster` re-raises the flag after
        // its own call — so a real group arriving mid-sandbox always wins.
        self.test = false;
        lines
    }

    /// The `party1..party4` slot view (byte law `0x5e6baa`, 0440): members of our OWN subgroup
    /// only, packet order, at most four. In a plain party every flags byte is 0, so the filter
    /// is a no-op; in a raid it keeps the compact frames on your own subgroup. Slot↔member
    /// association is rebuilt per packet — never assume it's stable across resyncs.
    pub fn party_slots(&self) -> impl Iterator<Item = &GroupMemberEntry> {
        let own = self.own_flags & 0x7f;
        self.members
            .iter()
            .filter(move |m| m.flags & 0x7f == own)
            .take(4)
    }

    /// `SMSG_GROUP_INVITE` — someone asked us in. Arms the pending invite (the popup rides it)
    /// and composes the chat-side notice line (CONFIRMED 0440: the real handler `0x5e6730`
    /// fires ERR_INVITED_TO_GROUP_S in addition to the popup's event).
    pub fn apply_invited(&mut self, inviter: &str) -> Vec<String> {
        self.pending_invite = Some(inviter.to_string());
        vec![fmt_s(INVITED_TO_GROUP, inviter)]
    }

    /// `SMSG_GROUP_DECLINE` — our invitee said no (sent to the inviter only).
    pub fn apply_declined(&mut self, name: &str) -> Vec<String> {
        vec![fmt_s(DECLINE_GROUP, name)]
    }

    /// `SMSG_GROUP_UNINVITE` (empty body) — we were kicked. Prints ERR_UNINVITE_YOU
    /// **unconditionally**, at the opcode (0440: handler `0x5e6850`); the empty roster echo
    /// that follows adds its own leave-diff lines, exactly like the reference.
    pub fn apply_uninvited(&mut self) -> Vec<String> {
        vec![UNINVITE_YOU.to_string()]
    }

    /// `SMSG_GROUP_DESTROYED` — the group is gone outright. Gated on believing we're grouped
    /// (0440: `0x5e6880` tests `0x4e86d0() != 0`); the echo list's own lines still follow.
    pub fn apply_destroyed(&mut self) -> Vec<String> {
        if self.in_group {
            vec![GROUP_DISBANDED.to_string()]
        } else {
            Vec::new()
        }
    }

    /// `SMSG_GROUP_SET_LEADER` — the broadcast leader change, by name (the guid is already in
    /// our roster; vmangos re-sends the full list right after, `Group::ChangeLeader`).
    pub fn apply_leader_changed(&mut self, name: &str, own_name: Option<&str>) -> Vec<String> {
        if own_name == Some(name) {
            vec![NEW_LEADER_YOU.to_string()]
        } else {
            vec![fmt_s(NEW_LEADER, name)]
        }
    }

    /// `SMSG_PARTY_COMMAND_RESULT` — the ack for our own invite/leave. LEAVE+ok is the ONE
    /// source of "You leave the group." (0440: `0x5e690b`, op 2 × result 0 → msgId 0x42; the
    /// empty roster never emits it).
    pub fn apply_command_result(
        &mut self,
        operation: u32,
        member: &str,
        result: u32,
    ) -> Vec<String> {
        if result == party_result::OK {
            return match operation {
                party_operation::INVITE => vec![fmt_s(INVITE_PLAYER, member)],
                party_operation::LEAVE => vec![LEFT_GROUP_YOU.to_string()],
                _ => Vec::new(),
            };
        }
        // The error table (vmangos `PartyResult`, `WorldSession.h:100-111` → the GlobalStrings
        // each errorId keys; templates quoted verbatim, GlobalStrings:1465-1861).
        let line = match result {
            party_result::BAD_PLAYER_NAME => fmt_s("Cannot find '%s'.", member),
            party_result::TARGET_NOT_IN_GROUP => fmt_s("%s is not in your party.", member),
            party_result::GROUP_FULL => "Your party is full.".to_string(),
            party_result::ALREADY_IN_GROUP => fmt_s("%s is already in a group.", member),
            party_result::NOT_IN_GROUP => "You aren't in a party.".to_string(),
            party_result::NOT_LEADER => "You are not the party leader.".to_string(),
            party_result::WRONG_FACTION => "Target is not part of your alliance.".to_string(),
            party_result::IGNORING_YOU => fmt_s("%s is ignoring you.", member),
            other => format!("Party command failed ({other})."),
        };
        vec![line]
    }

    /// `SMSG_PARTY_MEMBER_STATS(_FULL)` — merge (delta) or replace (`full`) one member's
    /// out-of-range snapshot.
    pub fn apply_stats(&mut self, guid: u64, full: bool, info: PartyMemberStatsInfo) {
        if full {
            self.stats.insert(guid, info);
            return;
        }
        let entry = self.stats.entry(guid).or_default();
        macro_rules! merge {
            ($($field:ident),* $(,)?) => {
                $(if info.$field.is_some() { entry.$field = info.$field; })*
            };
        }
        merge!(
            status,
            cur_hp,
            max_hp,
            power_type,
            cur_power,
            max_power,
            level,
            zone,
            position,
            pet_guid,
            pet_model_id,
            pet_cur_hp,
            pet_max_hp,
            pet_power_type,
            pet_cur_power,
            pet_max_power,
        );
        if info.auras.is_some() {
            entry.auras = info.auras;
        }
        if info.auras_negative.is_some() {
            entry.auras_negative = info.auras_negative;
        }
        if info.pet_name.is_some() {
            entry.pet_name = info.pet_name;
        }
        if info.pet_auras.is_some() {
            entry.pet_auras = info.pet_auras;
        }
        if info.pet_auras_negative.is_some() {
            entry.pet_auras_negative = info.pet_auras_negative;
        }
    }

    /// `MSG_RAID_TARGET_UPDATE` mode 0 — one icon changed (`guid == 0` clears it).
    pub fn apply_raid_target(&mut self, icon: u8, guid: u64) {
        if let Some(slot) = self.raid_targets.get_mut(icon as usize) {
            *slot = guid;
        }
    }

    /// `MSG_RAID_TARGET_UPDATE` mode 1 — the whole board (absent icons are unset).
    pub fn apply_raid_target_list(&mut self, entries: &[(u8, u64)]) {
        self.raid_targets = [0; 8];
        for (icon, guid) in entries {
            self.apply_raid_target(*icon, *guid);
        }
    }

    /// The mark on a unit, from the board: `1..=8` (the Lua `GetRaidTargetIndex` scale), `0`
    /// unmarked. The board stores one guid per icon slot, so this is the reverse lookup the
    /// per-unit snapshots feed from.
    pub fn raid_target_index(&self, guid: u64) -> u8 {
        if guid == 0 {
            return 0;
        }
        self.raid_targets
            .iter()
            .position(|g| *g == guid)
            .map_or(0, |i| i as u8 + 1)
    }

    /// Session teardown (decision 0065's lifecycle): everything resets with the socket.
    pub fn clear_session(&mut self) {
        *self = GroupState::default();
    }

    /// `SMSG_RAID_INSTANCE_INFO` — our saved lockouts, replacing the list wholesale (decision
    /// 1549). **The empty answer is the ordinary one and still counts as an answer**: it bumps the
    /// ticket like any other, which is what makes the feed fire `UPDATE_INSTANCE_INFO` for a
    /// player who has no lockouts at all — see [`GroupState::saved_instances_answers`] (1561).
    pub fn apply_raid_instance_info(
        &mut self,
        entries: Vec<benilla_protocol::messages::RaidInstanceEntry>,
    ) {
        self.saved_instances = entries;
        self.saved_instances_answers = self.saved_instances_answers.wrapping_add(1);
    }

    /// `MSG_RAID_READY_CHECK` (open form) — the leader started one. Bumps the ticket the feed
    /// turns into a `READY_CHECK` event edge.
    pub fn apply_ready_check(&mut self) {
        self.ready_check = self.ready_check.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, guid: u64) -> GroupMemberEntry {
        GroupMemberEntry {
            name: name.into(),
            guid,
            status: 1,
            flags: 0,
        }
    }

    /// **Every answer is an answer, including the one that says nothing changed** (1561). This is
    /// the invariant the whole Raid Info button hangs off: the reference throws its first
    /// `UPDATE_INSTANCE_INFO` away and decides the button on the second, so an empty list that
    /// answers a second time has to be distinguishable from an empty list that answered once. A
    /// flag cannot; a ticket can. Revert this to a `bool` and a player with no lockouts keeps a
    /// live button onto an empty panel — which is exactly how it shipped.
    #[test]
    fn an_unchanged_lockout_list_still_counts_as_an_answer() {
        let mut g = GroupState::default();
        assert_eq!(g.saved_instances_answers, 0, "nobody has asked yet");

        // The ordinary player: no lockouts, and the server says so every time it is asked.
        g.apply_raid_instance_info(Vec::new());
        assert_eq!(g.saved_instances_answers, 1);
        g.apply_raid_instance_info(Vec::new());
        assert_eq!(
            g.saved_instances_answers, 2,
            "the second empty answer is the one the button is decided on"
        );

        // And a list that DOES change is not counted twice for it.
        g.apply_raid_instance_info(vec![benilla_protocol::messages::RaidInstanceEntry {
            map: 409,
            reset: 86_400,
            instance: 7,
        }]);
        assert_eq!(g.saved_instances_answers, 3);
        assert_eq!(g.saved_instances.len(), 1, "and the list is the new one");

        // The socket dies, the ticket dies with it — a fresh session has not been answered.
        g.clear_session();
        assert_eq!(g.saved_instances_answers, 0);
        assert!(g.saved_instances.is_empty());
    }

    /// The ungated diff (0440 byte law): a FIRST roster prints joins for everyone already
    /// there; later rosters diff both ways.
    #[test]
    fn join_lines_come_from_roster_diffs() {
        let mut g = GroupState::default();
        // Our first list — we just accepted; Alice was already there → her join line prints
        // (the real client's cache starts empty, 0x5e6c19).
        assert_eq!(
            g.apply_list(0, 0, vec![member("Alice", 1)], 1, None),
            vec!["Alice joins the party."]
        );
        assert!(g.in_group);
        // Carol joins.
        assert_eq!(
            g.apply_list(0, 0, vec![member("Alice", 1), member("Carol", 3)], 1, None),
            vec!["Carol joins the party."]
        );
        // Carol leaves (3→2).
        assert_eq!(
            g.apply_list(0, 0, vec![member("Alice", 1)], 1, None),
            vec!["Carol leaves the party."]
        );
        // An unchanged resync (loot/status churn re-sends the list) prints nothing.
        assert!(g
            .apply_list(0, 0, vec![member("Alice", 1)], 1, None)
            .is_empty());
    }

    /// Per-opcode lines stack over the diff (0440) — no empty-list state machine.
    #[test]
    fn leave_kick_disband_lines_stack_per_opcode() {
        // Voluntary: the LEAVE ack prints; the empty list adds the leave-diff.
        let mut g = GroupState::default();
        g.apply_list(0, 0, vec![member("Alice", 1)], 1, None);
        assert_eq!(
            g.apply_command_result(party_operation::LEAVE, "Us", party_result::OK),
            vec!["You leave the group."]
        );
        assert_eq!(
            g.apply_list(0, 0, Vec::new(), 0, None),
            vec!["Alice leaves the party."]
        );
        assert!(!g.in_group);

        // Kicked: SMSG_GROUP_UNINVITE prints unconditionally at the opcode.
        let mut g = GroupState::default();
        g.apply_list(0, 0, vec![member("Alice", 1)], 1, None);
        assert_eq!(
            g.apply_uninvited(),
            vec!["You have been removed from the group."]
        );
        assert_eq!(
            g.apply_list(0, 0, Vec::new(), 0, None),
            vec!["Alice leaves the party."]
        );

        // Destroyed: GROUP_DESTROYED prints (grouped), the echo list still runs its diff.
        let mut g = GroupState::default();
        g.apply_list(0, 0, vec![member("Alice", 1)], 1, None);
        assert_eq!(g.apply_destroyed(), vec!["Your group has been disbanded."]);
        assert_eq!(
            g.apply_list(0, 0, Vec::new(), 0, None),
            vec!["Alice leaves the party."]
        );
        // Ungrouped, GROUP_DESTROYED is silent (the 0x4e86d0 gate).
        assert!(g.apply_destroyed().is_empty());

        // The 2-man collapse (vmangos sends no GROUP_DESTROYED): just the leave-diff line.
        let mut g = GroupState::default();
        g.apply_list(0, 0, vec![member("Alice", 1)], 1, None);
        assert_eq!(
            g.apply_list(0, 0, Vec::new(), 0, None),
            vec!["Alice leaves the party."]
        );
    }

    /// Raid wording: diffs use the raid strings, and the conversion prints the you-joined line.
    #[test]
    fn raid_wording_and_conversion() {
        let mut g = GroupState::default();
        g.apply_list(0, 0, vec![member("Alice", 1)], 1, None);
        // Party → raid conversion: same roster, type flips.
        assert_eq!(
            g.apply_list(1, 0, vec![member("Alice", 1)], 1, None),
            vec!["You have joined a raid group"]
        );
        // A raid join/leave uses the raid strings.
        assert_eq!(
            g.apply_list(1, 0, vec![member("Alice", 1), member("Dave", 4)], 1, None),
            vec!["Dave has joined the raid group"]
        );
        let mut lines = g.apply_list(1, 0, vec![member("Alice", 1)], 1, None);
        assert_eq!(lines.pop().as_deref(), Some("Dave has left the raid group"));
        // Leaving the raid: the ack prints the LEAVE line; the empty list diffs with raid
        // wording (keyed off the departed group's type).
        assert_eq!(
            g.apply_command_result(party_operation::LEAVE, "Us", party_result::OK),
            vec!["You leave the group."]
        );
        assert_eq!(
            g.apply_list(0, 0, Vec::new(), 0, None),
            vec!["Alice has left the raid group"]
        );
    }

    /// The party1..4 slot view filters to our own subgroup (0440: `(flags^own)&0x7f`).
    #[test]
    fn party_slots_filter_to_own_subgroup() {
        let mut g = GroupState::default();
        let mut m2 = member("Bob", 2);
        m2.flags = 0x01; // subgroup 1
        let mut m3 = member("Carol", 3);
        m3.flags = 0x80; // subgroup 0, assistant — the 0x80 bit is ignored by the filter
        g.apply_list(1, 0x00, vec![member("Alice", 1), m2, m3], 1, None);
        let slots: Vec<&str> = g.party_slots().map(|m| m.name.as_str()).collect();
        assert_eq!(slots, vec!["Alice", "Carol"]);
    }

    /// Invite-side lines: the ack, the error table, the decline, the inbound invite.
    #[test]
    fn invite_lines() {
        let mut g = GroupState::default();
        assert_eq!(
            g.apply_invited("Bob"),
            vec!["Bob has invited you to join a group."]
        );
        assert_eq!(g.pending_invite.as_deref(), Some("Bob"));
        assert_eq!(
            g.apply_command_result(party_operation::INVITE, "Carol", party_result::OK),
            vec!["You have invited Carol to join your group."]
        );
        assert_eq!(
            g.apply_command_result(
                party_operation::INVITE,
                "Carol",
                party_result::ALREADY_IN_GROUP
            ),
            vec!["Carol is already in a group."]
        );
        assert_eq!(
            g.apply_command_result(party_operation::INVITE, "Xz", party_result::BAD_PLAYER_NAME),
            vec!["Cannot find 'Xz'."]
        );
        assert_eq!(
            g.apply_declined("Carol"),
            vec!["Carol declines your group invitation."]
        );
    }

    /// Leader lines: name match against our own name picks the YOU form.
    #[test]
    fn leader_lines() {
        let mut g = GroupState::default();
        assert_eq!(
            g.apply_leader_changed("Alice", Some("Benilla")),
            vec!["Alice is now the group leader."]
        );
        assert_eq!(
            g.apply_leader_changed("Benilla", Some("Benilla")),
            vec!["You are now the group leader."]
        );
    }

    /// Stats: deltas merge field-wise, FULL replaces, roster departure drops the snapshot.
    #[test]
    fn stats_merge_and_retention() {
        let mut g = GroupState::default();
        g.apply_list(0, 0, vec![member("Alice", 1)], 1, None);
        g.apply_stats(
            1,
            false,
            PartyMemberStatsInfo {
                cur_hp: Some(50),
                max_hp: Some(100),
                ..Default::default()
            },
        );
        g.apply_stats(
            1,
            false,
            PartyMemberStatsInfo {
                cur_hp: Some(60),
                ..Default::default()
            },
        );
        let s = g.stats.get(&1).unwrap();
        assert_eq!(s.cur_hp, Some(60));
        assert_eq!(s.max_hp, Some(100), "delta merge keeps unmentioned fields");

        g.apply_stats(
            1,
            true,
            PartyMemberStatsInfo {
                cur_hp: Some(70),
                ..Default::default()
            },
        );
        let s = g.stats.get(&1).unwrap();
        assert_eq!(s.cur_hp, Some(70));
        assert_eq!(s.max_hp, None, "FULL replaces the snapshot outright");

        // Alice leaves → her snapshot drops.
        g.apply_list(0, 0, Vec::new(), 0, None);
        assert!(g.stats.is_empty());
    }

    /// The raid-target board: deltas set/clear one slot, the list form resets the board.
    #[test]
    fn raid_target_board() {
        let mut g = GroupState::default();
        g.apply_raid_target(7, 0x99); // skull
        assert_eq!(g.raid_targets[7], 0x99);
        g.apply_raid_target(7, 0); // clear
        assert_eq!(g.raid_targets[7], 0);
        g.apply_raid_target(3, 0x11);
        g.apply_raid_target_list(&[(0, 0x22), (5, 0x33)]);
        assert_eq!(g.raid_targets[0], 0x22);
        assert_eq!(g.raid_targets[5], 0x33);
        assert_eq!(g.raid_targets[3], 0, "the list form resets absent icons");
    }
}
