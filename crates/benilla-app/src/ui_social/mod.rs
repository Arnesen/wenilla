//! The social session — the friend list, the ignore list, `/who`, and the system lines they
//! print (decision 0668).
//!
//! [`SocialState`] mirrors the wire the way [`crate::ui_party`]'s `GroupState` does: the three
//! server packets replace it wholesale or patch one row, and the feed turns it into the VM
//! snapshot the FriendsFrame reads. Two client-side laws from wow-re's `FriendList.cpp` findings
//! (`system/net/scratch/w2b.md`, the 0x728-byte object at `DAT_00c28168`) shape everything here:
//!
//! - **The lists are guids.** A friend slot holds `{status, note*, guid, area, level, class}` —
//!   no name — and the display name comes from the ObjectMgr name cache (`0x55f080`) at format
//!   time. So does ours ([`crate::names::NameCache`]), which is why a freshly-listed friend can
//!   take a name-query round trip to show a name, and why removal (a guid on the wire) has to
//!   look the name up first.
//! - **Selection is a guid too** (`+0x648` friend, `+0x720` ignore), converted to an index on
//!   read. Ours is stored the same way, so a list that re-sorts under a selection keeps the same
//!   player selected rather than the same row.
//!
//! ## The system lines
//!
//! Every friend/ignore result prints one line, and they go to the **chat frame**, not the red
//! error line. That is byte-settled rather than assumed: `FriendList::HandleResult 0x5acab0`
//! displays through `CGGameUI::DisplayError 0x496720` with catalog ids `0x104..0x114`, and each
//! of those rows' `+0x4` **kind** field is written from the register the independently-VERIFIED
//! kind-0 row `0x8b` (`ERR_QUEST_FAILED_S` → chat) also uses — `ecx`, i.e. **kind 0 = the chat
//! composer** (`0x49a870`). Read at the bytes in the table's static initializer,
//! `0x486f78`–`0x48719c` (base `0xb4b498`, stride `0x14`); record `0x115` is the first to take a
//! different register, which is exactly where the friend band ends.
//!
//! Which result maps to which GlobalStrings key is *inferred by name* (the `ERR_FRIEND_*` /
//! `ERR_IGNORE_*` set is one-to-one with vmangos's `FriendsResult` enum, and 18 results collapse
//! to the 17 ids because both ADDED codes share `ERR_FRIEND_ADDED_S`) — the open dispatch item in
//! the record.
//!
//! ## What the ignore list is *for*
//!
//! Not the window — the silence. `FriendList::IsIgnored 0x5ae5a0` gates inbound chat and text
//! emotes (wow-re `system/ui/scratch/text-emote-composition.md`: ignored performer ⇒ dropped
//! silently, no line at all) and the duel handler's auto-decline (`0x4d4a33`). [`is_ignored`] is
//! that predicate; its callers are the chat apply arm and the duel one, which is how decision
//! 0633's stated "no ignore list yet" deviation closes.

use benilla_protocol::messages::{friend_result, friend_status, FriendEntry, FriendStatusUpdate};
use bevy::prelude::*;

use crate::ui_script::UiInput;

mod feed;
mod query;

pub(crate) use query::parse as who_query;

/// The GlobalStrings templates for the friend/ignore results, quoted verbatim from the reference
/// client's own patch chain (decision 0246 extraction; `GlobalStrings.lua` line cited per
/// constant). Composed here rather than in Lua for the same reason the party lines are
/// (decision 0434 §D2): the real client composes them engine-side, and the FrameXML never names
/// these keys.
const ERR_FRIEND_DB_ERROR: &str = "Friend lookup database error."; // GlobalStrings:1571
const ERR_FRIEND_LIST_FULL: &str = "You don't have room for any more friends."; // :1573
const ERR_FRIEND_ONLINE_SS: &str = "|Hplayer:%s|h[%s]|h has come online."; // :1576
const ERR_FRIEND_OFFLINE_S: &str = "%s has gone offline."; // :1575
const ERR_FRIEND_NOT_FOUND: &str = "Player not found."; // :1574
const ERR_FRIEND_REMOVED_S: &str = "%s removed from friends list."; // :1577
const ERR_FRIEND_ADDED_S: &str = "%s added to friends."; // :1569
const ERR_FRIEND_ALREADY_S: &str = "%s is already your friend."; // :1570
const ERR_FRIEND_SELF: &str = "You can't put yourself on your friend list."; // :1578
const ERR_FRIEND_WRONG_FACTION: &str = "Friends must be part of your alliance."; // :1579
const ERR_IGNORE_FULL: &str = "You can't ignore any more players."; // :1639
const ERR_IGNORE_SELF: &str = "You can't ignore yourself."; // :1642
const ERR_IGNORE_NOT_FOUND: &str = "Player not found."; // :1640
const ERR_IGNORE_ALREADY_S: &str = "%s is already being ignored."; // :1637
const ERR_IGNORE_ADDED_S: &str = "%s is now being ignored."; // :1636
const ERR_IGNORE_REMOVED_S: &str = "%s is no longer being ignored."; // :1641
const ERR_IGNORE_AMBIGUOUS: &str = "That name is ambiguous, type more of the player's server name"; // :1638
const ERR_FRIEND_ERROR: &str = "Unknown friend response from server."; // :1572

/// The away tags the friends-list template's third `%s` takes — `CHAT_FLAG_AFK` / `CHAT_FLAG_DND`
/// (`GlobalStrings.lua:766-767`), the same pair the chat frame prefixes a speaker's name with.
const CHAT_FLAG_AFK: &str = "<AFK>";
const CHAT_FLAG_DND: &str = "<DND>";

/// The social session mirror. Filled by the net drain's social arms, read by the feed, cleared on
/// disconnect beside the other per-login resources.
#[derive(Resource, Default)]
pub(crate) struct SocialState {
    /// The friend list as the wire sent it (`SMSG_FRIEND_LIST` order — vmangos walks a
    /// guid-keyed map). Display order is the feed's ([`Self::display_order`]).
    friends: Vec<FriendEntry>,
    /// The ignore list: guids, nothing else.
    ignores: Vec<u64>,
    /// The selected friend, stored as a **guid** like the reference's `+0x648`; `0` = none. A
    /// re-sort or a roster change keeps the same *player* selected.
    selected_friend: u64,
    /// The selected ignore (the reference's `+0x720`).
    selected_ignore: u64,
    /// The friend guids in the order the feed last showed them — written by the feed, read by the
    /// drain so a row index from Lua maps back to the same player the user clicked.
    display_order: Vec<u64>,
    /// The ignore guids in shown order, same contract.
    ignore_display_order: Vec<u64>,
    /// The last `/who` answer, and the server's true match total.
    who: Vec<benilla_protocol::messages::WhoEntry>,
    who_total: u32,
    /// The current who-list sort key (`SortWho`), `"zone"` being the frame's own default column.
    who_sort: String,
    /// `SetWhoToUI` — does the *next* `/who` answer belong to the Who frame (true) or the chat
    /// frame (false)? The WhoFrame's OnShow/OnHide drive it; a `/who` typed with the frame closed
    /// prints its results as chat lines.
    who_to_ui: bool,
    /// Results whose line is still owed, waiting on a name query (the reference's own
    /// resolve-then-compose order — `FriendList` formats from the name cache).
    pending_lines: Vec<FriendStatusUpdate>,
    /// Set whenever a list changed, so the feed knows to fire the Era update event.
    friends_dirty: bool,
    ignores_dirty: bool,
    who_dirty: bool,
}

impl SocialState {
    /// Is `guid` on the ignore list? The reference's `FriendList::IsIgnored 0x5ae5a0` — the
    /// predicate inbound chat, text emotes and duel challenges all gate on (module doc).
    pub(crate) fn is_ignored(&self, guid: u64) -> bool {
        guid != 0 && self.ignores.contains(&guid)
    }

    /// Is `guid` on the FRIEND list? The reference's `FriendList::FindFriendSlot 0x5ae810` —
    /// base `this+8`, stride `0x20`, bound `0x32`, the same triple `GetNumFriends 0x5ae490`
    /// counts over (§5, wow-re `system/object-layer/scratch/guild-signon-cvar-gate.md`).
    ///
    /// Its one consumer is the guild sign-on/sign-off line's fourth conjunct, and its purpose is
    /// **de-duplication, not suppression**: `SMSG_FRIEND_STATUS` emits the same two chat ids with
    /// no CVar gate of its own, so a guildmate who is also a friend would otherwise be announced
    /// twice. [`crate::ui_guild`]'s line path is where that matters.
    pub(crate) fn is_friend(&self, guid: u64) -> bool {
        guid != 0 && self.friends.iter().any(|f| f.guid == guid)
    }

    /// `SMSG_FRIEND_LIST` — replace the list wholesale (it is never a delta).
    fn apply_friend_list(&mut self, friends: Vec<FriendEntry>) {
        self.friends = friends;
        self.friends_dirty = true;
    }

    /// Seat the ignore list directly — test-only, for the neighbours that consult it rather than
    /// build it (decision 1764's trade-request ladder, whose leg 2 answers on its own opcode).
    #[cfg(test)]
    pub(crate) fn set_ignores_for_test(&mut self, guids: Vec<u64>) {
        self.apply_ignore_list(guids);
    }

    /// `SMSG_IGNORE_LIST` — likewise.
    fn apply_ignore_list(&mut self, guids: Vec<u64>) {
        self.ignores = guids;
        self.ignores_dirty = true;
    }

    /// `SMSG_FRIEND_STATUS` — apply the result to the list, and queue its system line.
    ///
    /// The server sends **no** fresh list after an add/remove, so the local list has to follow
    /// the result codes; a presence broadcast (ONLINE/OFFLINE) patches the row in place, which is
    /// what keeps a friend's zone current without a refresh.
    fn apply_friend_status(&mut self, update: FriendStatusUpdate) {
        match update.result {
            friend_result::ADDED_ONLINE | friend_result::ADDED_OFFLINE => {
                if !self.friends.iter().any(|f| f.guid == update.guid) {
                    let online = update.online;
                    self.friends.push(FriendEntry {
                        guid: update.guid,
                        status: online.map_or(friend_status::OFFLINE, |o| o.status),
                        area: online.map_or(0, |o| o.area),
                        level: online.map_or(0, |o| o.level),
                        class: online.map_or(0, |o| o.class),
                    });
                }
                self.friends_dirty = true;
            }
            friend_result::REMOVED => {
                self.friends.retain(|f| f.guid != update.guid);
                if self.selected_friend == update.guid {
                    self.selected_friend = 0;
                }
                self.friends_dirty = true;
            }
            friend_result::ONLINE => {
                if let (Some(entry), Some(online)) = (self.friend_mut(update.guid), update.online) {
                    entry.status = online.status;
                    entry.area = online.area;
                    entry.level = online.level;
                    entry.class = online.class;
                }
                self.friends_dirty = true;
            }
            friend_result::OFFLINE => {
                if let Some(entry) = self.friend_mut(update.guid) {
                    entry.status = friend_status::OFFLINE;
                    // The wire's own rule: an offline friend carries no area/level/class, so
                    // stale ones must not linger under the "Offline" row.
                    entry.area = 0;
                    entry.level = 0;
                    entry.class = 0;
                }
                self.friends_dirty = true;
            }
            friend_result::IGNORE_ADDED => {
                if !self.ignores.contains(&update.guid) {
                    self.ignores.push(update.guid);
                }
                self.ignores_dirty = true;
            }
            friend_result::IGNORE_REMOVED => {
                self.ignores.retain(|g| *g != update.guid);
                if self.selected_ignore == update.guid {
                    self.selected_ignore = 0;
                }
                self.ignores_dirty = true;
            }
            // Every other code is a refusal: it says something, and changes nothing.
            _ => {}
        }
        self.pending_lines.push(update);
    }

    fn friend_mut(&mut self, guid: u64) -> Option<&mut FriendEntry> {
        self.friends.iter_mut().find(|f| f.guid == guid)
    }

    /// `SMSG_WHO` — the answer to our last query.
    fn apply_who(&mut self, results: benilla_protocol::messages::WhoResults) {
        self.who = results.entries;
        self.who_total = results.total;
        self.who_dirty = true;
    }
}

/// The line one result prints, and whether it needs the subject's name first.
///
/// `%s` templates wait for the name query; the rest print immediately. This is the reference's
/// own order (resolve, then compose — wow-re's text-emote flow, VERIFIED for the sibling path).
fn result_template(result: u8) -> Option<&'static str> {
    Some(match result {
        friend_result::DB_ERROR => ERR_FRIEND_DB_ERROR,
        friend_result::LIST_FULL => ERR_FRIEND_LIST_FULL,
        friend_result::ONLINE => ERR_FRIEND_ONLINE_SS,
        friend_result::OFFLINE => ERR_FRIEND_OFFLINE_S,
        friend_result::NOT_FOUND => ERR_FRIEND_NOT_FOUND,
        friend_result::REMOVED => ERR_FRIEND_REMOVED_S,
        friend_result::ADDED_ONLINE | friend_result::ADDED_OFFLINE => ERR_FRIEND_ADDED_S,
        friend_result::ALREADY => ERR_FRIEND_ALREADY_S,
        friend_result::SELF => ERR_FRIEND_SELF,
        friend_result::ENEMY => ERR_FRIEND_WRONG_FACTION,
        friend_result::IGNORE_FULL => ERR_IGNORE_FULL,
        friend_result::IGNORE_SELF => ERR_IGNORE_SELF,
        friend_result::IGNORE_NOT_FOUND => ERR_IGNORE_NOT_FOUND,
        friend_result::IGNORE_ALREADY => ERR_IGNORE_ALREADY_S,
        friend_result::IGNORE_ADDED => ERR_IGNORE_ADDED_S,
        friend_result::IGNORE_REMOVED => ERR_IGNORE_REMOVED_S,
        friend_result::IGNORE_AMBIGUOUS => ERR_IGNORE_AMBIGUOUS,
        friend_result::UNKNOWN => ERR_FRIEND_ERROR,
        // An unknown code shows nothing — GlobalStrings data-suppression, the same face an
        // absent key wears everywhere else in this client.
        _ => return None,
    })
}

/// Fill a result template with the subject's name. `ERR_FRIEND_ONLINE_SS` takes it **twice**
/// (the `|Hplayer:%s|h[%s]|h` link the comment in GlobalStrings explicitly warns not to
/// localize), so a plain replace-all is the right substitution, not a positional one.
fn fill_line(template: &str, name: &str) -> String {
    template.replace("%s", name)
}

/// The `<AFK>`/`<DND>` tag a friend row's third `%s` takes.
fn status_tag(status: u8) -> &'static str {
    match status {
        friend_status::AFK => CHAT_FLAG_AFK,
        friend_status::DND => CHAT_FLAG_DND,
        _ => "",
    }
}

/// The net drain's `SessionEvent::Friend*`/`Who*` arms, factored here so the wire laws live
/// beside the state they drive ([`crate::ui_duel::apply`]'s shape).
pub(crate) mod apply {
    use super::*;

    /// `SMSG_FRIEND_LIST`.
    pub(crate) fn friend_list(social: &mut SocialState, friends: Vec<FriendEntry>) {
        social.apply_friend_list(friends);
    }

    /// `SMSG_IGNORE_LIST`.
    pub(crate) fn ignore_list(social: &mut SocialState, guids: Vec<u64>) {
        social.apply_ignore_list(guids);
    }

    /// `SMSG_FRIEND_STATUS`.
    pub(crate) fn friend_status(social: &mut SocialState, update: FriendStatusUpdate) {
        social.apply_friend_status(update);
    }

    /// `SMSG_WHO`.
    pub(crate) fn who(social: &mut SocialState, results: benilla_protocol::messages::WhoResults) {
        social.apply_who(results);
    }
}

/// The social window's session: the wire mirror, the VM feed, and the outbound intents.
pub(crate) struct UiSocialPlugin;

impl Plugin for UiSocialPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SocialState>().add_systems(
            Update,
            (
                feed::feed_social.before(UiInput),
                feed::drain_social.after(UiInput),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::messages::FriendOnline;

    fn status(result: u8, guid: u64) -> FriendStatusUpdate {
        FriendStatusUpdate {
            result,
            guid,
            online: None,
        }
    }

    /// The server sends no fresh list after an add or a remove, so the result codes have to keep
    /// the local list honest — otherwise a friend added stays invisible until the next login.
    #[test]
    fn add_and_remove_results_maintain_the_list() {
        let mut social = SocialState::default();
        social.apply_friend_status(FriendStatusUpdate {
            result: friend_result::ADDED_ONLINE,
            guid: 7,
            online: Some(FriendOnline {
                status: friend_status::ONLINE,
                area: 12,
                level: 60,
                class: 4,
            }),
        });
        assert_eq!(social.friends.len(), 1);
        assert_eq!(social.friends[0].level, 60);

        // The same add arriving twice must not duplicate the row.
        social.apply_friend_status(status(friend_result::ADDED_OFFLINE, 7));
        assert_eq!(social.friends.len(), 1);

        social.apply_friend_status(status(friend_result::REMOVED, 7));
        assert!(social.friends.is_empty());
    }

    /// A presence broadcast patches the row in place — and going offline clears the level/zone,
    /// because the wire stops sending them (a stale "Level 60 Rogue" under an Offline row would
    /// be the client inventing data).
    #[test]
    fn presence_broadcasts_patch_the_row() {
        let mut social = SocialState::default();
        social.apply_friend_list(vec![FriendEntry {
            guid: 7,
            ..Default::default()
        }]);

        social.apply_friend_status(FriendStatusUpdate {
            result: friend_result::ONLINE,
            guid: 7,
            online: Some(FriendOnline {
                status: friend_status::AFK,
                area: 1519,
                level: 42,
                class: 8,
            }),
        });
        assert_eq!(social.friends[0].status, friend_status::AFK);
        assert_eq!(social.friends[0].area, 1519);

        social.apply_friend_status(status(friend_result::OFFLINE, 7));
        assert!(!social.friends[0].is_online());
        assert_eq!(
            (social.friends[0].area, social.friends[0].level),
            (0, 0),
            "offline carries no zone or level"
        );
    }

    /// Removing the selected friend clears the selection rather than leaving it pointing at a
    /// player who is no longer listed.
    #[test]
    fn removing_the_selected_friend_clears_the_selection() {
        let mut social = SocialState::default();
        social.apply_friend_list(vec![FriendEntry {
            guid: 7,
            ..Default::default()
        }]);
        social.selected_friend = 7;
        social.apply_friend_status(status(friend_result::REMOVED, 7));
        assert_eq!(social.selected_friend, 0);
    }

    /// The ignore results maintain the ignore list, and [`SocialState::is_ignored`] is what the
    /// chat/duel gates read.
    #[test]
    fn ignore_results_maintain_the_ignore_list() {
        let mut social = SocialState::default();
        assert!(!social.is_ignored(9));
        social.apply_friend_status(status(friend_result::IGNORE_ADDED, 9));
        assert!(social.is_ignored(9));
        assert!(!social.is_ignored(0), "guid 0 is never ignored");
        social.apply_friend_status(status(friend_result::IGNORE_REMOVED, 9));
        assert!(!social.is_ignored(9));
    }

    /// A refusal changes nothing — but still owes a line.
    #[test]
    fn a_refusal_changes_no_state_but_prints() {
        let mut social = SocialState::default();
        social.apply_friend_status(status(friend_result::ALREADY, 7));
        assert!(social.friends.is_empty());
        assert_eq!(social.pending_lines.len(), 1);
    }

    /// Every result vmangos can send has a line, and the two ADDED codes share one — which is
    /// exactly why 18 results fit the 17 catalog ids `0x104..0x114`.
    #[test]
    fn every_result_code_maps_to_a_line() {
        for result in 0x00..=0x11u8 {
            assert!(
                result_template(result).is_some(),
                "result {result:#04x} has no line"
            );
        }
        assert_eq!(
            result_template(friend_result::ADDED_ONLINE),
            result_template(friend_result::ADDED_OFFLINE),
        );
        assert_eq!(
            result_template(friend_result::UNKNOWN),
            Some(ERR_FRIEND_ERROR)
        );
        assert_eq!(result_template(0x77), None, "an unknown code shows nothing");
    }

    /// The online line takes the name twice (the player link); the rest take it once.
    #[test]
    fn the_online_line_fills_the_name_twice() {
        assert_eq!(
            fill_line(ERR_FRIEND_ONLINE_SS, "Bob"),
            "|Hplayer:Bob|h[Bob]|h has come online."
        );
        assert_eq!(
            fill_line(ERR_FRIEND_ADDED_S, "Bob"),
            "Bob added to friends."
        );
        assert_eq!(fill_line(ERR_FRIEND_SELF, "Bob"), ERR_FRIEND_SELF);
    }
}
