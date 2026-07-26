//! The social VM feed/drain — the systems half of [`super`]: resolve the wire's guids and ids
//! into the display-ready snapshot the FriendsFrame reads, fire the list events on their edges,
//! print the result lines, and turn the Lua-side [`SocialRequest`] intents into their sends.
//!
//! Everything resolved here is resolved *engine-side in the reference too* (`FriendList`'s
//! formatter `0x5ae160` reads the name cache and the race/class/area GameTables before Lua sees a
//! row) — see [`super`]'s module doc.

use benilla_formats::AreaTableCatalog;
use benilla_protocol::messages::WhoEntry;
use benilla_ui::script::{FriendInfo, SocialRequest, SocialState as VmSocial, UiScript, WhoInfo};
use bevy::prelude::*;

use crate::area::AreaTableRes;
use crate::names::NameCache;
use crate::net::{ClientCommand, NetCommands};
use crate::ui_chat::{ChatEvent, ChatEventKind, ChatLog};
use crate::ui_unit::{class_names, race_names};

use super::{fill_line, result_template, status_tag, SocialState};

/// `WHO_LIST_FORMAT` / `WHO_LIST_GUILD_FORMAT` / `WHO_NUM_RESULTS(_P1)` — the chat-routed `/who`
/// output (`GlobalStrings.lua:5441-5444`). These four keys appear **nowhere** in the reference's
/// FrameXML (exhaustively grepped), which is what identifies them as engine-composed: when
/// `SetWhoToUI` is off, the engine prints the results as chat lines itself. So do we.
const WHO_LIST_FORMAT: &str = "|Hplayer:%s|h[%s]|h: Level %d %s %s - %s";
const WHO_LIST_GUILD_FORMAT: &str = "|Hplayer:%s|h[%s]|h: Level %d %s %s <%s> - %s";
const WHO_NUM_RESULTS: &str = "%d player total";
const WHO_NUM_RESULTS_P1: &str = "%d players total";

/// What the feed last announced, so the Era events fire on edges rather than every frame.
#[derive(Default)]
pub(super) struct FedSocial {
    /// Has the VM been given a snapshot at all yet? The first push always fires the update
    /// events, so a frame loaded after the list arrived still populates.
    seeded: bool,
}

/// Build the display snapshot, push it to the VM, fire the list events, and drain the owed
/// result lines.
#[allow(clippy::too_many_arguments)] // a Bevy system's param list IS its dependency set
pub(super) fn feed_social(
    script: Option<NonSendMut<UiScript>>,
    mut social: ResMut<SocialState>,
    mut names: ResMut<NameCache>,
    areas: Option<Res<AreaTableRes>>,
    commands: Res<NetCommands>,
    mut chat_log: ResMut<ChatLog>,
    mut fed: Local<FedSocial>,
) {
    let Some(mut script) = script else {
        return;
    };
    let areas = areas.as_deref().map(|a| &a.0);

    // The owed system lines first: a line about a friend who just went offline should land before
    // the list update that removes their zone.
    drain_result_lines(&mut social, &mut names, &commands, &mut chat_log);

    let (friends, display_order) = friend_rows(&social, &mut names, &commands, areas);
    let (ignores, ignore_order) = ignore_rows(&social, &mut names, &commands);
    let who: Vec<WhoInfo> = social.who.iter().map(|e| who_row(e, areas)).collect();

    let selected_friend = index_of(&display_order, social.selected_friend);
    let selected_ignore = index_of(&ignore_order, social.selected_ignore);
    social.display_order = display_order;
    social.ignore_display_order = ignore_order;

    script.set_social(VmSocial {
        friends,
        selected_friend,
        ignores,
        selected_ignore,
        who,
        who_total: social.who_total,
    });

    // The three list events (`FriendsFrame_OnEvent`'s own arms). FRIENDLIST_SHOW is the answer to
    // an explicit `ShowFriends()`; a list that simply changed fires FRIENDLIST_UPDATE.
    let first = !fed.seeded;
    fed.seeded = true;
    if social.friends_dirty || first {
        social.friends_dirty = false;
        script.fire_event("FRIENDLIST_UPDATE", Vec::new());
    }
    if social.ignores_dirty || first {
        social.ignores_dirty = false;
        script.fire_event("IGNORELIST_UPDATE", Vec::new());
    }
    if social.who_dirty {
        social.who_dirty = false;
        if social.who_to_ui {
            script.fire_event("WHO_LIST_UPDATE", Vec::new());
        } else {
            // The frame isn't listening — the engine prints the results instead.
            push_who_lines(&social, &mut chat_log, areas);
        }
    }
}

/// Compose and push every result line whose name has resolved. A line needing a name waits (the
/// reference's resolve-then-compose order); one that doesn't goes out immediately.
fn drain_result_lines(
    social: &mut SocialState,
    names: &mut NameCache,
    commands: &NetCommands,
    chat_log: &mut ChatLog,
) {
    let mut still_pending = Vec::new();
    for update in std::mem::take(&mut social.pending_lines) {
        let Some(template) = result_template(update.result) else {
            continue; // an unknown code shows nothing
        };
        if !template.contains("%s") {
            system_line(chat_log, template.to_string());
            continue;
        }
        // A named line with no subject (the server answers a failed lookup with guid 0) can never
        // resolve — print nothing rather than "%s added to friends." or hold it forever.
        if update.guid == 0 {
            continue;
        }
        match names.resolve(update.guid, commands).map(str::to_string) {
            Some(name) => system_line(chat_log, fill_line(template, &name)),
            None => still_pending.push(update),
        }
    }
    social.pending_lines = still_pending;
}

/// The chat-routed `/who` output: the total line, then one line per row (module doc's four
/// engine-only templates).
fn push_who_lines(social: &SocialState, chat_log: &mut ChatLog, areas: Option<&AreaTableCatalog>) {
    let total = social.who_total;
    let template = if total == 1 {
        WHO_NUM_RESULTS
    } else {
        WHO_NUM_RESULTS_P1
    };
    system_line(chat_log, template.replace("%d", &total.to_string()));
    for entry in &social.who {
        let row = who_row(entry, areas);
        let template = if row.guild.is_empty() {
            WHO_LIST_FORMAT
        } else {
            WHO_LIST_GUILD_FORMAT
        };
        // The templates are positional-by-order, not indexed: name, name, level, race, class,
        // [guild,] zone.
        let mut line = template.to_string();
        for fill in [
            row.name.as_str(),
            row.name.as_str(),
            &row.level.to_string(),
            row.race.as_str(),
            row.class.as_str(),
        ] {
            line = replace_first_token(&line, fill);
        }
        if !row.guild.is_empty() {
            line = replace_first_token(&line, &row.guild);
        }
        line = replace_first_token(&line, &row.zone);
        system_line(chat_log, line);
    }
}

/// Replace the first `%s` or `%d` in `line` with `fill` — the templates interleave both, so a
/// per-token replace-first walk fills them in wire order.
fn replace_first_token(line: &str, fill: &str) -> String {
    match (line.find("%s"), line.find("%d")) {
        (Some(s), Some(d)) if d < s => line.replacen("%d", fill, 1),
        (Some(_), _) => line.replacen("%s", fill, 1),
        (None, Some(_)) => line.replacen("%d", fill, 1),
        (None, None) => line.to_string(),
    }
}

/// The friend rows in display order (name-sorted), plus the guid order that produced them so the
/// drain can map a row index back to a player.
fn friend_rows(
    social: &SocialState,
    names: &mut NameCache,
    commands: &NetCommands,
    areas: Option<&AreaTableCatalog>,
) -> (Vec<FriendInfo>, Vec<u64>) {
    let mut rows: Vec<(u64, FriendInfo)> = social
        .friends
        .iter()
        .map(|entry| {
            let name = names
                .resolve(entry.guid, commands)
                .map(str::to_string)
                .unwrap_or_default();
            let online = entry.is_online();
            (
                entry.guid,
                FriendInfo {
                    name,
                    level: entry.level,
                    // An offline friend has no class/zone on the wire; leaving them empty is what
                    // makes the frame print its "Offline" template instead of inventing values.
                    class: online
                        .then(|| class_names(entry.class as u8))
                        .flatten()
                        .map(|(display, _)| display.to_string())
                        .unwrap_or_default(),
                    area: online
                        .then(|| areas.and_then(|a| a.name(entry.area)))
                        .flatten()
                        .unwrap_or_default()
                        .to_string(),
                    connected: online,
                    status: status_tag(entry.status).to_string(),
                },
            )
        })
        .collect();

    // Name order, with the not-yet-resolved rows last so an in-flight name query doesn't park an
    // empty row at the top of the list.
    rows.sort_by(|(_, a), (_, b)| {
        a.name
            .is_empty()
            .cmp(&b.name.is_empty())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    rows.into_iter().map(|(guid, row)| (row, guid)).unzip()
}

/// The ignore rows: names only, same ordering rule.
fn ignore_rows(
    social: &SocialState,
    names: &mut NameCache,
    commands: &NetCommands,
) -> (Vec<String>, Vec<u64>) {
    let mut rows: Vec<(u64, String)> = social
        .ignores
        .iter()
        .map(|guid| {
            (
                *guid,
                names
                    .resolve(*guid, commands)
                    .map(str::to_string)
                    .unwrap_or_default(),
            )
        })
        .collect();
    rows.sort_by(|(_, a), (_, b)| {
        a.is_empty()
            .cmp(&b.is_empty())
            .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
    });
    rows.into_iter().map(|(guid, name)| (name, guid)).unzip()
}

/// One `/who` row, ids resolved. Note the Lua API returns race *before* class while the wire
/// carries class first — the swap happens here, once.
fn who_row(entry: &WhoEntry, areas: Option<&AreaTableCatalog>) -> WhoInfo {
    WhoInfo {
        name: entry.name.clone(),
        guild: entry.guild.clone(),
        level: entry.level,
        race: race_names(entry.race as u8)
            .map(|(display, _)| display)
            .unwrap_or_default()
            .to_string(),
        class: class_names(entry.class as u8)
            .map(|(display, _)| display)
            .unwrap_or_default()
            .to_string(),
        zone: areas
            .and_then(|a| a.name(entry.zone))
            .unwrap_or_default()
            .to_string(),
    }
}

/// The 1-based row a guid occupies in the shown order, `0` when it isn't shown — the reference's
/// guid→index conversion (`GetSelectedFriend` `0x5ae510`).
fn index_of(order: &[u64], guid: u64) -> u32 {
    if guid == 0 {
        return 0;
    }
    order
        .iter()
        .position(|g| *g == guid)
        .map_or(0, |i| i as u32 + 1)
}

fn system_line(chat_log: &mut ChatLog, text: String) {
    chat_log.push_event(ChatEvent::text_only(ChatEventKind::System, text));
}

/// Turn the Era API's social intents into their sends. Every "by index" intent resolves through
/// the display order the feed just published, and every "by name" one through the list's own
/// resolved names — because the wire removes by **guid** (see [`super`]'s module doc).
pub(super) fn drain_social(
    script: Option<NonSendMut<UiScript>>,
    mut social: ResMut<SocialState>,
    names: Res<NameCache>,
    commands: Res<NetCommands>,
    areas: Option<Res<AreaTableRes>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let requests = script.take_social_requests();
    if requests.is_empty() {
        return;
    }
    for request in requests {
        match request {
            SocialRequest::RefreshFriends => {
                let _ = commands.0.send(ClientCommand::FriendListRequest);
            }
            SocialRequest::AddFriend(name) => {
                let _ = commands.0.send(ClientCommand::AddFriend { name });
            }
            SocialRequest::RemoveFriendIndex(index) => {
                if let Some(guid) = row_guid(&social.display_order, index) {
                    let _ = commands.0.send(ClientCommand::DelFriend { guid });
                }
            }
            SocialRequest::RemoveFriendName(name) => {
                if let Some(guid) = guid_named(&social.display_order, &name, &names) {
                    let _ = commands.0.send(ClientCommand::DelFriend { guid });
                }
            }
            SocialRequest::AddIgnore(name) => {
                let _ = commands.0.send(ClientCommand::AddIgnore { name });
            }
            SocialRequest::DelIgnore(name) => {
                if let Some(guid) = guid_named(&social.ignore_display_order, &name, &names) {
                    let _ = commands.0.send(ClientCommand::DelIgnore { guid });
                }
            }
            SocialRequest::ToggleIgnore(name) => {
                // `/ignore <name>`: un-ignore if they're already on the list, else ignore them.
                match guid_named(&social.ignore_display_order, &name, &names) {
                    Some(guid) => {
                        let _ = commands.0.send(ClientCommand::DelIgnore { guid });
                    }
                    None => {
                        let _ = commands.0.send(ClientCommand::AddIgnore { name });
                    }
                }
            }
            SocialRequest::SelectFriend(index) => {
                social.selected_friend = row_guid(&social.display_order, index).unwrap_or(0);
            }
            SocialRequest::SelectIgnore(index) => {
                social.selected_ignore = row_guid(&social.ignore_display_order, index).unwrap_or(0);
            }
            SocialRequest::Who(filter) => {
                let request = super::who_query(&filter, areas.as_deref().map(|a| &a.0));
                let _ = commands.0.send(ClientCommand::Who {
                    request: Box::new(request),
                });
            }
            SocialRequest::SortWho(sort_type) => {
                social.who_sort = sort_type;
                sort_who(&mut social);
            }
            SocialRequest::SetWhoToUi(on) => social.who_to_ui = on,
        }
    }
}

/// The guid at a 1-based display row.
fn row_guid(order: &[u64], index: u32) -> Option<u64> {
    usize::try_from(index.checked_sub(1)?)
        .ok()
        .and_then(|i| order.get(i))
        .copied()
}

/// The guid of the listed player called `name`, case-insensitively — the name→guid direction the
/// `/removefriend` and `/unignore` verbs need before they can send anything.
fn guid_named(order: &[u64], name: &str, names: &NameCache) -> Option<u64> {
    order.iter().copied().find(|guid| {
        names
            .peek(*guid)
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    })
}

/// Re-order the who results in place. Client-side, like the reference's own comparator — the
/// server sends them in whatever order its player map iterates.
fn sort_who(social: &mut SocialState) {
    let key = social.who_sort.clone();
    match key.as_str() {
        "level" => social.who.sort_by_key(|e| e.level),
        "class" => social.who.sort_by_key(|e| e.class),
        "race" => social.who.sort_by_key(|e| e.race),
        "zone" => social.who.sort_by_key(|e| e.zone),
        "guild" => social.who.sort_by(|a, b| a.guild.cmp(&b.guild)),
        // "name" and anything unrecognised: alphabetical, the list's resting order.
        _ => social.who.sort_by(|a, b| a.name.cmp(&b.name)),
    }
    social.who_dirty = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::messages::friend_status;

    /// A row index maps back through the *shown* order, and 0/past-the-end map to nothing — the
    /// guard that keeps a stale click from removing a bystander.
    #[test]
    fn row_indices_map_through_the_shown_order() {
        let order = [11u64, 22, 33];
        assert_eq!(row_guid(&order, 1), Some(11));
        assert_eq!(row_guid(&order, 3), Some(33));
        assert_eq!(row_guid(&order, 0), None);
        assert_eq!(row_guid(&order, 4), None);
    }

    /// Selection survives a re-order because it is stored as a guid: the same player keeps the
    /// highlight even when the row under them moves.
    #[test]
    fn selection_follows_the_player_not_the_row() {
        assert_eq!(index_of(&[11, 22, 33], 22), 2);
        assert_eq!(index_of(&[22, 11, 33], 22), 1, "same player, new row");
        assert_eq!(index_of(&[11, 33], 22), 0, "no longer listed");
        assert_eq!(index_of(&[11, 22], 0), 0, "nothing selected");
    }

    /// The who templates interleave `%s` and `%d`, so the fills have to walk them in order.
    #[test]
    fn who_line_tokens_fill_in_wire_order() {
        let mut line = WHO_LIST_GUILD_FORMAT.to_string();
        for fill in [
            "Tigole", "Tigole", "40", "Human", "Rogue", "Legacy", "Westfall",
        ] {
            line = replace_first_token(&line, fill);
        }
        assert_eq!(
            line,
            "|Hplayer:Tigole|h[Tigole]|h: Level 40 Human Rogue <Legacy> - Westfall"
        );
    }

    /// The unguilded template is the same walk minus the guild fill.
    #[test]
    fn the_unguilded_who_line_skips_the_guild() {
        let mut line = WHO_LIST_FORMAT.to_string();
        for fill in ["Solo", "Solo", "5", "Dwarf", "Priest", "Coldridge Valley"] {
            line = replace_first_token(&line, fill);
        }
        assert_eq!(
            line,
            "|Hplayer:Solo|h[Solo]|h: Level 5 Dwarf Priest - Coldridge Valley"
        );
    }

    /// An offline friend shows no class or zone — the wire sends neither, so the row must not
    /// carry the values it had while online.
    #[test]
    fn an_offline_row_carries_no_class_or_zone() {
        let entry = benilla_protocol::messages::FriendEntry {
            guid: 7,
            status: friend_status::OFFLINE,
            area: 12,
            level: 60,
            class: 4,
        };
        // `who_row`'s sibling path, exercised through the same resolution rules the feed uses.
        let online = entry.is_online();
        assert!(!online);
        let class = online
            .then(|| class_names(entry.class as u8))
            .flatten()
            .map(|(d, _)| d.to_string())
            .unwrap_or_default();
        assert_eq!(class, "");
    }
}
