//! The guild session's **system lines** — the `ERR_GUILD_*` chat output the engine composes.
//!
//! Every guild event and every command verdict prints one line, and the *engine* composes it, not
//! the FrameXML: `SMSG_GUILD_EVENT`'s handler `0x5e7180` routes its arms into
//! `CGGameUI::DisplayError 0x496720` (ids `0x59`, `0x5a`, `0x57`, `0x5c`, `0x6a`, `0x6b`, `0x5d`,
//! `0x6c`, `0x106`, `0x107` — wow-re RF-0077), and the invite / decline / info / command-result
//! handlers do the same (`0x5e6f65`, `0x5e6f9a`, `0x5e706b`, `0x5e7520`). Not one of the
//! `ERR_GUILD_*` keys those resolve to appears anywhere in the reference FrameXML, which is what
//! identifies them as engine-composed — [`crate::ui_social`]'s own test, applied again.
//!
//! **The `GUILD_MOTD` line is deliberately not here.** The reference's `ChatFrame_OnEvent`
//! composes `GUILD_MOTD_TEMPLATE` itself off the `GUILD_MOTD` event (`ChatFrame.lua:1335-1340`),
//! so firing the event is the whole job and composing a line beside it would double it.
//!
//! Which id maps to which GlobalStrings key is *inferred by name* (the catalog's own id→key table
//! is uncited in wow-re), exactly as [`crate::ui_party`]'s party-result table is — but the
//! sign-on/sign-off pair is better than inferred: vmangos annotates `GE_SIGNED_ON`/`GE_SIGNED_OFF`
//! with the key names themselves (`Guild/Guild.h:134-135`), and the two ids the reference uses for
//! them (`0x106`/`0x107`) sit inside the friend band `0x104..0x114`.

use benilla_protocol::messages::{
    guild_command, guild_command_error, guild_event, GuildCommandResult, GuildEventNotice,
    GuildInfo,
};

// ── The templates, quoted verbatim from the reference's own patch chain (decision 0246
//    extraction; the `GlobalStrings.lua` line is on each) ──────────────────────────────────────
const ERR_GUILD_CREATE_S: &str = "%s created."; // GlobalStrings:1597
const ERR_GUILD_INVITE_S: &str = "You have invited %s to join your guild."; // :1606
const ERR_GUILD_QUIT_S: &str = "You are no longer a member of %s."; // :1629
const ERR_GUILD_FOUNDER_S: &str = "Congratulations, you are a founding member of %s!"; // :1604
const ERR_GUILD_INTERNAL: &str = "Internal guild error."; // :1605
const ERR_ALREADY_IN_GUILD: &str = "You are already in a guild."; // :1466
const ERR_ALREADY_IN_GUILD_S: &str = "%s is already in a guild."; // :1467
const ERR_INVITED_TO_GUILD: &str = "You have already been invited into a guild."; // :1655
const ERR_ALREADY_INVITED_TO_GUILD_S: &str = "%s has already been invited to a guild."; // :1464
const ERR_GUILD_NAME_INVALID: &str = "That name contains invalid characters,  Enter a new name."; // :1616
const ERR_GUILD_NAME_EXISTS_S: &str = "There is already a guild named \"%s\"."; // :1615
pub(super) const ERR_GUILD_LEADER_LEAVE: &str =
    "You must promote a new Guild Master using /gleader before leaving the guild."; // :1610
pub(super) const ERR_GUILD_PERMISSIONS: &str = "You don't have permission to do that."; // :1624
const ERR_GUILD_PLAYER_NOT_IN_GUILD: &str = "You are not in a guild."; // :1626
const ERR_GUILD_PLAYER_NOT_IN_GUILD_S: &str = "%s is not in your guild."; // :1627
const ERR_GUILD_PLAYER_NOT_FOUND_S: &str = "\"%s\" not found."; // :1625
const ERR_GUILD_NOT_ALLIED: &str =
    "You cannot invite players from the opposing alliance into your guild"; // :1623
const ERR_GUILD_RANK_TOO_HIGH_S: &str = "%s's rank is too high"; // :1632
const ERR_GUILD_RANK_TOO_LOW_S: &str = "%s is already at the lowest rank"; // :1633
const ERR_GUILD_RANKS_LOCKED: &str = "Temporary guild error.  Please try again!"; // :1630
const ERR_GUILD_RANK_IN_USE: &str = "That guild rank is currently in use."; // :1631
const ERR_IGNORING_YOU_S: &str = "%s is ignoring you."; // :1643

const ERR_GUILD_PROMOTE_SSS: &str = "%s has promoted %s to %s."; // :1628
const ERR_GUILD_DEMOTE_SSS: &str = "%s has demoted %s to %s."; // :1599
const ERR_GUILD_JOIN_S: &str = "%s has joined the guild."; // :1607
const ERR_GUILD_LEAVE_S: &str = "%s has left the guild."; // :1614
const ERR_GUILD_REMOVE_SS: &str = "%s has been kicked out of the guild by %s."; // :1635
const ERR_GUILD_LEADER_IS_S: &str = "%s is the leader of your guild."; // :1609
const ERR_GUILD_LEADER_CHANGED_SS: &str = "%s has made %s the new Guild Master."; // :1608
const ERR_GUILD_DISBANDED: &str = "Guild has been disbanded."; // :1600
const ERR_GUILD_DECLINE_S: &str = "%s declines your guild invitation."; // :1598
const ERR_INVITED_TO_GUILD_SS: &str = "%s invites you join %s."; // :1656

/// `ERR_FRIEND_ONLINE_SS` / `ERR_FRIEND_OFFLINE_S` (`GlobalStrings:1576`, `:1575`) — a guildmate
/// signing on and off prints the **friend-list** lines (module doc), which is also why
/// [`crate::ui_social`] holds its own copy of the same two strings.
const ERR_FRIEND_ONLINE_SS: &str = "|Hplayer:%s|h[%s]|h has come online.";
/// See [`ERR_FRIEND_ONLINE_SS`].
const ERR_FRIEND_OFFLINE_S: &str = "%s has gone offline.";

/// `GUILD_NAME_TEMPLATE` / `GUILD_INFO_TEMPLATE` (`GlobalStrings:2091`, `:2079`) — the two lines
/// `/ginfo` prints, in this order: the handler `0x5e6fb0` formats the name through `0x8604f0` at
/// `0x5e700f` and the counts through `0x8604dc` at `0x5e706b`.
const GUILD_NAME_TEMPLATE: &str = "Guild: %s";
/// See [`GUILD_NAME_TEMPLATE`]. **The template is month-day-year and the wire is day-month-year** —
/// the handler's own cdecl pushes (`0x5e704d`–`0x5e7061`) swap the first two, which is exactly what
/// an enUS `m-d-y` template does to vmangos's `createdDay, createdMonth, createdYear` order.
const GUILD_INFO_TEMPLATE: &str = "Guild created %d-%d-%d, %d players, %d accounts";

/// Fill a `%s` template **positionally** — one argument per slot, in order.
///
/// Distinct from [`crate::ui_social`]'s replace-all `fill_line`, and it has to be: the guild
/// templates take *different* values in their two and three slots ("%s has promoted %s to %s.")
/// where the friend one takes the same name twice.
fn fill(template: &str, args: &[&str]) -> String {
    let mut out = template.to_string();
    for arg in args {
        out = out.replacen("%s", arg, 1);
    }
    out
}

/// [`fill`] for the one `%d` template ([`GUILD_INFO_TEMPLATE`]).
fn fill_d(template: &str, args: &[u32]) -> String {
    let mut out = template.to_string();
    for arg in args {
        out = out.replacen("%d", &arg.to_string(), 1);
    }
    out
}

/// The line one `SMSG_GUILD_EVENT` prints, if any.
///
/// `announce_signon` is the sign-on/sign-off pair's **whole display condition**, resolved by the
/// caller because three of its four conjuncts need state this function does not see. See
/// [`super::apply::event`] for the conjuncts and their byte addresses; what matters here is that
/// the pair is the only place in this table with a condition at all, and that the guid those two
/// arms carry exists to answer it.
///
/// **This argument used to be `ignored`, and that was a mislabel** (corrected 1589, from a wow-re
/// §5 dispatched for exactly this): `0x5ae810` is `FriendList::FindFriendSlot`, a **friends-list**
/// membership test — base `this+8`, stride `0x20`, bound `0x32` — not the ignore-list check at
/// `this+0x650`. Reading it as "ignore" got the behaviour backwards on both sides: an *ignored*
/// guildmate was silenced where the reference announces them, and a guildmate who is also a
/// *friend* was announced twice, because `SMSG_FRIEND_STATUS` says the same thing with no gate of
/// its own. The repo's own `system/net` ledger had `0x5ae810` right the whole time.
pub(super) fn event_line(notice: &GuildEventNotice, announce_signon: bool) -> Option<String> {
    let p = |i: usize| notice.params.get(i).map(String::as_str).unwrap_or_default();
    match notice.event {
        guild_event::PROMOTION => Some(fill(ERR_GUILD_PROMOTE_SSS, &[p(0), p(1), p(2)])),
        guild_event::DEMOTION => Some(fill(ERR_GUILD_DEMOTE_SSS, &[p(0), p(1), p(2)])),
        guild_event::JOINED => Some(fill(ERR_GUILD_JOIN_S, &[p(0)])),
        guild_event::LEFT => Some(fill(ERR_GUILD_LEAVE_S, &[p(0)])),
        guild_event::REMOVED => Some(fill(ERR_GUILD_REMOVE_SS, &[p(0), p(1)])),
        guild_event::LEADER_IS => Some(fill(ERR_GUILD_LEADER_IS_S, &[p(0)])),
        guild_event::LEADER_CHANGED => Some(fill(ERR_GUILD_LEADER_CHANGED_SS, &[p(0), p(1)])),
        guild_event::DISBANDED => Some(ERR_GUILD_DISBANDED.to_string()),
        guild_event::SIGNED_ON if announce_signon => {
            Some(fill(ERR_FRIEND_ONLINE_SS, &[p(0), p(0)]))
        }
        guild_event::SIGNED_OFF if announce_signon => Some(fill(ERR_FRIEND_OFFLINE_S, &[p(0)])),
        // MOTD's line is the FrameXML's (module doc); UPDATE_RANK_NAME and UPDATE_ROSTER are
        // silent in the reference too; TABARD_CHANGE, a sign-on the condition refuses, and
        // whatever the server invents past 0x0d show nothing. (The reference's default arm displays catalog
        // id 0x69, whose GlobalStrings key is not settled — an unnamed key shows nothing, which is
        // the GlobalStrings data-suppression every other absent key in this client wears.)
        _ => None,
    }
}

/// The line one `SMSG_GUILD_COMMAND_RESULT` prints, if any.
///
/// `result == 0` is the success side — the reference's `0x5e7550 test eax,eax` selects on exactly
/// that — and then the [`guild_command`] tag chooses the line. Otherwise the result code does,
/// except for `0x08`, whose two meanings are told apart by that same tag.
pub(super) fn command_line(result: &GuildCommandResult) -> Option<String> {
    let name = result.name.as_str();
    if result.result == guild_command_error::PLAYER_NO_MORE_IN_GUILD {
        return match result.command {
            guild_command::CREATE => Some(fill(ERR_GUILD_CREATE_S, &[name])),
            guild_command::INVITE => Some(fill(ERR_GUILD_INVITE_S, &[name])),
            guild_command::QUIT => Some(fill(ERR_GUILD_QUIT_S, &[name])),
            guild_command::FOUNDER => Some(fill(ERR_GUILD_FOUNDER_S, &[name])),
            // vmangos's own note on the rest of `Typecommand`: they "have no effect" here.
            _ => None,
        };
    }
    match result.result {
        guild_command_error::INTERNAL => Some(ERR_GUILD_INTERNAL.to_string()),
        guild_command_error::ALREADY_IN_GUILD => Some(ERR_ALREADY_IN_GUILD.to_string()),
        guild_command_error::ALREADY_IN_GUILD_S => Some(fill(ERR_ALREADY_IN_GUILD_S, &[name])),
        guild_command_error::INVITED_TO_GUILD => Some(ERR_INVITED_TO_GUILD.to_string()),
        guild_command_error::ALREADY_INVITED_TO_GUILD_S => {
            Some(fill(ERR_ALREADY_INVITED_TO_GUILD_S, &[name]))
        }
        guild_command_error::NAME_INVALID => Some(ERR_GUILD_NAME_INVALID.to_string()),
        guild_command_error::NAME_EXISTS_S => Some(fill(ERR_GUILD_NAME_EXISTS_S, &[name])),
        // The one collision: `0x08` is ERR_GUILD_LEADER_LEAVE under a QUIT and
        // ERR_GUILD_PERMISSIONS under anything else (benilla-protocol's own note on
        // `guild_command_error::LEADER_LEAVE`), which is why the command tag has to survive the
        // trip from the wire to here.
        guild_command_error::PERMISSIONS if result.command == guild_command::QUIT => {
            Some(ERR_GUILD_LEADER_LEAVE.to_string())
        }
        guild_command_error::PERMISSIONS => Some(ERR_GUILD_PERMISSIONS.to_string()),
        guild_command_error::PLAYER_NOT_IN_GUILD => Some(ERR_GUILD_PLAYER_NOT_IN_GUILD.to_string()),
        guild_command_error::PLAYER_NOT_IN_GUILD_S => {
            Some(fill(ERR_GUILD_PLAYER_NOT_IN_GUILD_S, &[name]))
        }
        guild_command_error::PLAYER_NOT_FOUND_S => {
            Some(fill(ERR_GUILD_PLAYER_NOT_FOUND_S, &[name]))
        }
        guild_command_error::NOT_ALLIED => Some(ERR_GUILD_NOT_ALLIED.to_string()),
        guild_command_error::RANK_TOO_HIGH_S => Some(fill(ERR_GUILD_RANK_TOO_HIGH_S, &[name])),
        guild_command_error::RANK_TOO_LOW_S => Some(fill(ERR_GUILD_RANK_TOO_LOW_S, &[name])),
        guild_command_error::RANKS_LOCKED => Some(ERR_GUILD_RANKS_LOCKED.to_string()),
        guild_command_error::RANK_IN_USE => Some(ERR_GUILD_RANK_IN_USE.to_string()),
        guild_command_error::IGNORING_YOU_S => Some(fill(ERR_IGNORING_YOU_S, &[name])),
        // UNK20 ("for Typecommand 0x05 only") and anything unknown: nothing.
        _ => None,
    }
}

/// `SMSG_GUILD_INVITE`'s notice line — the one the reference prints *beside* the popup
/// (`0x5e6f65 DisplayError(0x4f, inviter, guild)`; the popup is the separate
/// `GUILD_INVITE_REQUEST` fire at `0x5e6f53`).
pub(super) fn invite_line(inviter: &str, guild: &str) -> String {
    fill(ERR_INVITED_TO_GUILD_SS, &[inviter, guild])
}

/// `SMSG_GUILD_DECLINE` — our invitee said no (delivered to the inviter only).
pub(super) fn decline_line(name: &str) -> String {
    fill(ERR_GUILD_DECLINE_S, &[name])
}

/// `SMSG_GUILD_INFO` — the `/ginfo` answer, which is purely two chat lines: nothing in the packet
/// overlaps the roster and no frame reads it.
pub(super) fn info_lines(info: &GuildInfo) -> Vec<String> {
    vec![
        fill(GUILD_NAME_TEMPLATE, &[&info.name]),
        fill_d(
            GUILD_INFO_TEMPLATE,
            // month, day, year — the enUS swap, see [`GUILD_INFO_TEMPLATE`].
            &[
                info.created_month,
                info.created_day,
                info.created_year,
                info.member_count,
                info.account_count,
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `%s` slots of a guild line take DIFFERENT values, unlike the friend list's repeated
    /// name — a replace-all substitution would print "Tigole has promoted Tigole to Tigole."
    #[test]
    fn the_multi_slot_lines_fill_positionally() {
        let notice = GuildEventNotice {
            event: guild_event::PROMOTION,
            params: vec!["Tigole".into(), "Furor".into(), "Officer".into()],
            guid: None,
        };
        // Every other arm ignores the flag — a promotion prints whatever the condition says.
        assert_eq!(
            event_line(&notice, false).as_deref(),
            Some("Tigole has promoted Furor to Officer.")
        );
    }

    /// The sign-on/sign-off lines are the friend-list ones, and they are the only two arms in the
    /// table with a display condition — which is the whole reason those two carry a guid.
    ///
    /// **The polarity flipped in 1589** and this test says so on purpose: the flag used to mean
    /// "ignored" (suppress) and now means "announce", because `0x5ae810` turned out to be a
    /// friends-list test, not the ignore check the transcription named.
    #[test]
    fn sign_on_lines_are_the_friend_lines_and_obey_their_condition() {
        let notice = GuildEventNotice {
            event: guild_event::SIGNED_ON,
            params: vec!["Tigole".into()],
            guid: Some(9),
        };
        assert_eq!(
            event_line(&notice, true).as_deref(),
            Some("|Hplayer:Tigole|h[Tigole]|h has come online."),
            "the name fills the link and the bracket both"
        );
        assert_eq!(
            event_line(&notice, false),
            None,
            "a refused condition prints nothing at all"
        );
    }

    /// The MOTD's line belongs to the FrameXML, and the two silent events stay silent.
    #[test]
    fn the_silent_events_print_nothing() {
        for event in [
            guild_event::MOTD,
            guild_event::UPDATE_RANK_NAME,
            guild_event::UPDATE_ROSTER,
            guild_event::TABARD_CHANGE,
            0x77,
        ] {
            let notice = GuildEventNotice {
                event,
                params: vec!["x".into(), "y".into()],
                guid: None,
            };
            assert_eq!(event_line(&notice, false), None, "event {event:#04x}");
        }
    }

    /// Result `0x08` means two different things, and only the command tag beside it says which.
    #[test]
    fn the_two_meanings_of_result_eight_are_told_apart_by_the_command() {
        assert_eq!(
            command_line(&GuildCommandResult {
                command: guild_command::QUIT,
                name: String::new(),
                result: guild_command_error::LEADER_LEAVE,
            })
            .as_deref(),
            Some(ERR_GUILD_LEADER_LEAVE)
        );
        assert_eq!(
            command_line(&GuildCommandResult {
                command: guild_command::INVITE,
                name: String::new(),
                result: guild_command_error::PERMISSIONS,
            })
            .as_deref(),
            Some(ERR_GUILD_PERMISSIONS)
        );
    }

    /// Result `0` is the success side, and there the command tag picks the line.
    #[test]
    fn result_zero_is_the_success_side() {
        assert_eq!(
            command_line(&GuildCommandResult {
                command: guild_command::INVITE,
                name: "Kaplan".into(),
                result: guild_command_error::PLAYER_NO_MORE_IN_GUILD,
            })
            .as_deref(),
            Some("You have invited Kaplan to join your guild.")
        );
        assert_eq!(
            command_line(&GuildCommandResult {
                command: guild_command::QUIT,
                name: "Legacy of Steel".into(),
                result: guild_command_error::PLAYER_NO_MORE_IN_GUILD,
            })
            .as_deref(),
            Some("You are no longer a member of Legacy of Steel.")
        );
        assert_eq!(
            command_line(&GuildCommandResult {
                command: 0x99,
                name: String::new(),
                result: guild_command_error::PLAYER_NO_MORE_IN_GUILD,
            }),
            None,
            "a command with no message says nothing"
        );
    }

    /// `/ginfo`'s two lines, with the date swapped into the enUS month-day-year the wire is not in.
    #[test]
    fn the_ginfo_lines_swap_the_date_into_month_day_year() {
        assert_eq!(
            info_lines(&GuildInfo {
                name: "Legacy of Steel".into(),
                created_day: 14,
                created_month: 3,
                created_year: 2005,
                member_count: 42,
                account_count: 30,
            }),
            vec![
                "Guild: Legacy of Steel".to_string(),
                "Guild created 3-14-2005, 42 players, 30 accounts".to_string(),
            ]
        );
    }

    /// The invite notice names the inviter first and the guild second.
    #[test]
    fn the_invite_notice_names_both() {
        assert_eq!(
            invite_line("Tigole", "Legacy of Steel"),
            "Tigole invites you join Legacy of Steel."
        );
        assert_eq!(
            decline_line("Kaplan"),
            "Kaplan declines your guild invitation."
        );
    }
}
