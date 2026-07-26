//! The social **Era API surface** — friends, ignores, and `/who` (decision 0668).
//!
//! The [`super::party`] shape exactly: the app pushes a [`SocialState`] snapshot built from its
//! own wire mirror ([`UiScript::set_social`]) and the getters here read that plain data; every
//! verb queues a [`SocialRequest`] the app drains ([`UiScript::take_social_requests`]) into the
//! matching `CMSG_*` send. No ECS or net reach from the engine (decision 0068 §3).
//!
//! **The snapshot is already display-ready** — names, class name, zone name, the `<AFK>` tag —
//! because every one of those is a *lookup the engine owns* in the real client too: the friend
//! slot on the wire holds a bare guid + class/area **ids**, and `FriendList`'s formatter
//! (`0x5ae160`) resolves them through the ObjectMgr name cache and the race/class/area GameTables
//! before Lua ever sees them. Pushing ids here and resolving in Lua would invent a client the
//! reference isn't.
//!
//! **Selection lives in the engine, not in Lua** — the reference keeps the selected friend and
//! ignore as *guids* on the FriendList object (`+0x648` / `+0x720`, read back by
//! `GetSelectedFriend` `0x5ad260` / `0x5ae510`), which is why `SetSelectedFriend(i)` here mutates
//! the snapshot in place *and* queues the intent: the same Lua tick reads the new value back
//! (`FriendsList_Update` does exactly that), and the app's own state follows so the next push
//! agrees.

use mlua::{Lua, Value};

use super::Model;

/// One friend row, already resolved for display — see the module doc on why the app resolves
/// rather than the VM. Mirrors `GetFriendInfo`'s six returns in field order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FriendInfo {
    /// The character's name, from the name cache. Empty while the `CMSG_NAME_QUERY` is still in
    /// flight — the FrameXML's own `if ( not name ) then name = UNKNOWN` covers that frame, so
    /// the row shows "Unknown" rather than vanishing.
    pub name: String,
    /// Level, `0` when offline (the wire sends no level for an offline friend).
    pub level: u32,
    /// Localized class name ("Warrior"), empty when offline.
    pub class: String,
    /// Zone name ("Elwynn Forest"), empty when offline or when the id has no `AreaTable` row.
    pub area: String,
    /// Online at all — `GetFriendInfo`'s fifth return, and what enables the Send Message /
    /// Group Invite buttons.
    pub connected: bool,
    /// The away tag as the friends-list template wants it: `""`, `"<AFK>"`, or `"<DND>"`.
    pub status: String,
}

/// One `/who` row — `GetWhoInfo`'s six returns in order (note the wire's class/race order is
/// swapped relative to this Lua-facing one: the API returns race *before* class).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WhoInfo {
    pub name: String,
    pub guild: String,
    pub level: u32,
    /// Localized race name ("Night Elf").
    pub race: String,
    /// Localized class name ("Druid").
    pub class: String,
    /// Zone name.
    pub zone: String,
}

/// The social snapshot, pushed whole by the app whenever it changes ([`UiScript::set_social`]).
/// `default()` is the fresh-login shape: no friends, no ignores, no `/who` run yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SocialState {
    /// The friend list, in the order the list frame shows it (the app sorts — the reference's
    /// own `FriendList` comparator `0x5ada00` does the same engine-side).
    pub friends: Vec<FriendInfo>,
    /// The selected friend as a **1-based** index, `0` = none — `GetSelectedFriend`'s scale
    /// (`0x5ad260` returns the stored slot + 1).
    pub selected_friend: u32,
    /// The ignore list: names only, which is all `GetIgnoreName` returns.
    pub ignores: Vec<String>,
    /// The selected ignore, same 1-based scale.
    pub selected_ignore: u32,
    /// The last `/who` answer's rows (≤ 49).
    pub who: Vec<WhoInfo>,
    /// The last `/who` answer's *total* match count — `GetNumWhoResults`'s second return, which
    /// can exceed `who.len()` and is what drives the "(50 displayed)" suffix.
    pub who_total: u32,
}

/// Outbound social intents queued by the Era API, drained by the app
/// ([`UiScript::take_social_requests`]). Plain data — [`super::party::PartyRequest`]'s twin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SocialRequest {
    /// `ShowFriends()` — refresh the list (`CMSG_FRIEND_LIST`).
    RefreshFriends,
    /// `AddFriend(name)`.
    AddFriend(String),
    /// `RemoveFriend(index)` — the friends frame's button, which addresses the *row*.
    RemoveFriendIndex(u32),
    /// `RemoveFriend(name)` — `/removefriend`, which addresses a name. One Era global takes
    /// either, so the queue carries both shapes and the app resolves each to the guid the wire
    /// wants.
    RemoveFriendName(String),
    /// `AddIgnore(name)`.
    AddIgnore(String),
    /// `DelIgnore(name)`.
    DelIgnore(String),
    /// `AddOrDelIgnore(name)` — `/ignore`'s toggle: ignore if not ignored, un-ignore if it is.
    /// The app decides which, because only it holds the list.
    ToggleIgnore(String),
    /// `SetSelectedFriend(index)` — mirrored into the app so the next push agrees.
    SelectFriend(u32),
    /// `SetSelectedIgnore(index)`.
    SelectIgnore(u32),
    /// `SendWho(filter)` — the raw filter string as typed; parsing it into wire fields needs the
    /// DBCs, so it happens app-side.
    Who(String),
    /// `SortWho(sortType)` — `"name"`/`"level"`/`"class"`/`"zone"`/`"guild"`/`"race"`. Sorting
    /// is client-side; the app re-orders its own results and pushes them back.
    SortWho(String),
    /// `SetWhoToUI(flag)` — where the *next* `/who` answer goes: the Who frame (true) or the chat
    /// frame (false). The WhoFrame's own OnShow/OnHide drive it.
    SetWhoToUi(bool),
}

impl super::UiScript {
    /// Push the social snapshot, replacing whatever was there. A bare setter — firing
    /// `FRIENDLIST_UPDATE`/`IGNORELIST_UPDATE`/`WHO_LIST_UPDATE` on the edges is the app's job.
    pub fn set_social(&mut self, state: SocialState) {
        self.model_mut().social = state;
    }

    /// Drain the social intents queued since the last call.
    pub fn take_social_requests(&mut self) -> Vec<SocialRequest> {
        std::mem::take(&mut self.model_mut().social_requests)
    }

    /// Queue an intent from the app side — the slash commands. In the reference these ARE Lua
    /// (`SlashCmdList["FRIENDS"]` calls `AddFriend`, …); benilla parses slash lines in Rust, so
    /// the same intents enter the same queue here. [`super::duel`]'s `queue_duel_request` twin.
    pub fn queue_social_request(&mut self, request: SocialRequest) {
        self.model_mut().social_requests.push(request);
    }
}

/// Register the social globals against the snapshot store.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // ── The friend list ──────────────────────────────────────────────────────────────────────
    // GetNumFriends() → how many friends are listed (`0x5ad000` over CountFriends `0x5ae490`).
    g.set(
        "GetNumFriends",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.social.friends.len() as i64)
        })?,
    )?;

    // GetFriendInfo(index) → name, level, class, area, connected, status (`0x5ad060`). An index
    // past the end returns nothing at all — the FrameXML tests `if ( not name )`, so nil-per-
    // return is the shape it expects, not an error.
    g.set(
        "GetFriendInfo",
        lua.create_function(|lua, index: i64| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(friend) = friend_at(&model.social.friends, index) else {
                return Ok((
                    Value::Nil,
                    Value::Nil,
                    Value::Nil,
                    Value::Nil,
                    Value::Nil,
                    Value::Nil,
                ));
            };
            Ok((
                Value::String(lua.create_string(&friend.name)?),
                Value::Integer(i64::from(friend.level)),
                Value::String(lua.create_string(&friend.class)?),
                Value::String(lua.create_string(&friend.area)?),
                // `connected` is the era 1/nil boolean the list frame branches on.
                if friend.connected {
                    Value::Integer(1)
                } else {
                    Value::Nil
                },
                Value::String(lua.create_string(&friend.status)?),
            ))
        })?,
    )?;

    // GetSelectedFriend() → the 1-based selected row, 0 when nothing is selected.
    g.set(
        "GetSelectedFriend",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(model.social.selected_friend))
        })?,
    )?;

    // SetSelectedFriend(index) — mutate now (the caller reads it back this tick) and mirror the
    // intent to the app (module doc: selection is engine state in the reference too).
    g.set(
        "SetSelectedFriend",
        lua.create_function(|lua, index: i64| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let index = clamp_index(index, model.social.friends.len());
            model.social.selected_friend = index;
            model
                .social_requests
                .push(SocialRequest::SelectFriend(index));
            Ok(())
        })?,
    )?;

    // ShowFriends() — ask the server for the list again.
    g.set(
        "ShowFriends",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.social_requests.push(SocialRequest::RefreshFriends);
            Ok(())
        })?,
    )?;

    // AddFriend(name) — an empty name is dropped here rather than sent: the server answers a
    // blank lookup with nothing at all, so it would look like a hang.
    g.set(
        "AddFriend",
        lua.create_function(|lua, name: String| {
            if !name.trim().is_empty() {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model.social_requests.push(SocialRequest::AddFriend(name));
            }
            Ok(())
        })?,
    )?;

    // RemoveFriend(indexOrName) — the one global with two callers of different shapes (the
    // frame's button passes a row index, /removefriend a name).
    g.set(
        "RemoveFriend",
        lua.create_function(|lua, who: Value| {
            let request = match who {
                Value::Integer(i) if i >= 1 => Some(SocialRequest::RemoveFriendIndex(i as u32)),
                Value::Number(n) if n >= 1.0 => Some(SocialRequest::RemoveFriendIndex(n as u32)),
                Value::String(s) => {
                    let name = s.to_str()?.to_string();
                    (!name.trim().is_empty()).then_some(SocialRequest::RemoveFriendName(name))
                }
                _ => None,
            };
            if let Some(request) = request {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model.social_requests.push(request);
            }
            Ok(())
        })?,
    )?;

    // ── The ignore list ──────────────────────────────────────────────────────────────────────
    // GetNumIgnores() → CountIgnores `0x5ae550` (the 25 slots at +0x650).
    g.set(
        "GetNumIgnores",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.social.ignores.len() as i64)
        })?,
    )?;

    // GetIgnoreName(index) → the name, or nil past the end. The ignore list frame calls this for
    // all 20 of its rows every update, most of them empty — nil is the ordinary case, not an
    // error.
    g.set(
        "GetIgnoreName",
        lua.create_function(|lua, index: i64| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let name = usize::try_from(index - 1)
                .ok()
                .and_then(|i| model.social.ignores.get(i));
            Ok(match name {
                Some(name) => Value::String(lua.create_string(name)?),
                None => Value::Nil,
            })
        })?,
    )?;

    // GetSelectedIgnore() / SetSelectedIgnore(index) — the friend pair's twin (`0x5ae630` /
    // `0x5ae5f0`).
    g.set(
        "GetSelectedIgnore",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(model.social.selected_ignore))
        })?,
    )?;
    g.set(
        "SetSelectedIgnore",
        lua.create_function(|lua, index: i64| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let index = clamp_index(index, model.social.ignores.len());
            model.social.selected_ignore = index;
            model
                .social_requests
                .push(SocialRequest::SelectIgnore(index));
            Ok(())
        })?,
    )?;

    // AddIgnore / DelIgnore / AddOrDelIgnore(name).
    for (global, make) in [
        ("AddIgnore", SocialRequest::AddIgnore as fn(String) -> _),
        ("DelIgnore", SocialRequest::DelIgnore as fn(String) -> _),
        (
            "AddOrDelIgnore",
            SocialRequest::ToggleIgnore as fn(String) -> _,
        ),
    ] {
        g.set(
            global,
            lua.create_function(move |lua, name: String| {
                if !name.trim().is_empty() {
                    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                    model.social_requests.push(make(name));
                }
                Ok(())
            })?,
        )?;
    }

    // ── /who ─────────────────────────────────────────────────────────────────────────────────
    // GetNumWhoResults() → displayed, total. The second return is the server's true match count,
    // which is why the frame can say "132 players total (50 displayed)".
    g.set(
        "GetNumWhoResults",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok((
                model.social.who.len() as i64,
                i64::from(model.social.who_total),
            ))
        })?,
    )?;

    // GetWhoInfo(index) → name, guild, level, race, class, zone.
    g.set(
        "GetWhoInfo",
        lua.create_function(|lua, index: i64| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let row = usize::try_from(index - 1)
                .ok()
                .and_then(|i| model.social.who.get(i));
            let Some(row) = row else {
                return Ok((
                    Value::Nil,
                    Value::Nil,
                    Value::Nil,
                    Value::Nil,
                    Value::Nil,
                    Value::Nil,
                ));
            };
            Ok((
                Value::String(lua.create_string(&row.name)?),
                Value::String(lua.create_string(&row.guild)?),
                Value::Integer(i64::from(row.level)),
                Value::String(lua.create_string(&row.race)?),
                Value::String(lua.create_string(&row.class)?),
                Value::String(lua.create_string(&row.zone)?),
            ))
        })?,
    )?;

    // SendWho(filter) — the filter string as typed, parsed app-side.
    g.set(
        "SendWho",
        lua.create_function(|lua, filter: Option<String>| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model
                .social_requests
                .push(SocialRequest::Who(filter.unwrap_or_default()));
            Ok(())
        })?,
    )?;

    // SortWho(sortType) — the column-header and dropdown sorts.
    g.set(
        "SortWho",
        lua.create_function(|lua, sort_type: String| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model
                .social_requests
                .push(SocialRequest::SortWho(sort_type));
            Ok(())
        })?,
    )?;

    // SetWhoToUI(flag) — the WhoFrame's OnShow/OnHide. Era passes 0/1, so anything but a falsey
    // 0/nil is "the frame wants them".
    g.set(
        "SetWhoToUI",
        lua.create_function(|lua, flag: Value| {
            let on = match flag {
                Value::Nil | Value::Boolean(false) => false,
                Value::Integer(0) => false,
                Value::Number(n) => n != 0.0,
                _ => true,
            };
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.social_requests.push(SocialRequest::SetWhoToUi(on));
            Ok(())
        })?,
    )?;

    Ok(())
}

/// The 1-based friend lookup `GetFriendInfo` does, `None` past either end.
fn friend_at(friends: &[FriendInfo], index: i64) -> Option<&FriendInfo> {
    usize::try_from(index - 1).ok().and_then(|i| friends.get(i))
}

/// Clamp a selection index to `0..=len` — `0` means "nothing selected", and a row past the end
/// selects nothing rather than a phantom.
fn clamp_index(index: i64, len: usize) -> u32 {
    if index >= 1 && index <= len as i64 {
        index as u32
    } else {
        0
    }
}
