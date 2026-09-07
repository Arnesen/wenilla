//! `GetChannelName` — the joined-channel lookup, in both directions.
//!
//! **The state this reads already existed and was built for this verb.** `ui_chat::edit`'s
//! `ChannelState` has held `joined: Vec<String>` in join order since the chat arc, and its own doc
//! comment names `GetChannelName(n)` as the law it implements. Only the registration was missing —
//! the same silent-gap shape the loader arc kept finding, one layer up: the capability was built,
//! nothing exposed it, and nothing complained.
//!
//! Corpus demand, counted by reading every line rather than the grep total (1207): **17 sites
//! across 6 addons**, every one an unguarded call, no library replicated —
//! `ChatLog` 7, `Recap` 4, `SmartRes` 2, `Enchantrix` 2, `FuBar_AssistFu` 1, `_LazyPig` 1. Both
//! lookup directions are live in the corpus: by index (`GetChannelName(i)`) and by name
//! (`GetChannelName("world")`, `GetChannelName("Trade - City")`).
//!
//! Signature verified against wow-5875-re `system/ui/scratch/zone-chat-channel-autojoin.md`
//! l.374-380 — `GetChannelName = 0x4a05e0`, **three** returns:
//!
//! | # | the client's | ours |
//! |---|---|---|
//! | 1 | `slot[+0x00]`, the client-local **1-based joined-slot index** (= `CHAT_MSG_*` arg8) | the position in `joined` |
//! | 2 | `slot[+0x04]`, the channel name | the `joined` entry |
//! | 3 | `slot[+0x98]`, FrameXML's `instanceID` (= arg10) | **0** — see below |
//!
//! **Return 3 is 0, not nil, and that is a recorded position rather than a shrug.** It is the split
//! index from `YOU_JOINED`'s second u32, which `ui_chat::event`'s doc already records as "0 on every
//! vanilla emulator" and deliberately does not store. A client that one day meets a server which
//! splits channels would need the field; nothing in the corpus reads it.

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// The 1-based slot of `name`, case-insensitively — `GetChannelName`'s first return.
/// One `ChatChannels.dbc` row as the VM needs it for `JoinChannelByName` (decision 1908),
/// `Add/RemoveChatWindowChannel` and `EnumerateServerChannels` (wow-re chat-cache-grammar.md
/// §5-6): the id, the **Shortcut** (`General`, `Trade`, … — what every one of those verbs compares
/// a typed name against, whole and case-folded), the name composed for the zone the player is in
/// (`General - Elwynn Forest`; `None` while the zone text is empty, which is the verbs' nil leg),
/// and whether the row is listed here at all (a `flags & 0x10` city row is, only in a city).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneChannelRow {
    pub id: u32,
    pub shortcut: String,
    pub resolved: Option<String>,
    pub listed: bool,
}

/// A channel verb's ask, drained by the app into its `CMSG_*` (all of which
/// `benilla-protocol::messages::client` already builds). The verbs are the stock
/// `ChatFrame.lua` slash handlers' calls, one variant each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelCommand {
    /// `JoinChannelByName(name, password)` — sent on both of 1908's non-nil legs.
    Join {
        name: String,
        password: String,
    },
    /// `LeaveChannelByName(name)`.
    Leave {
        name: String,
    },
    /// `ListChannelByName(name)` — `CMSG_CHANNEL_LIST`.
    List {
        name: String,
    },
    /// `ListChannels()` — the roster of every joined channel.
    ListAll,
    /// `DisplayChannelOwner(name)` — `CMSG_CHANNEL_OWNER`.
    DisplayOwner {
        name: String,
    },
    /// `SetChannelOwner(name, player)`.
    SetOwner {
        name: String,
        player: String,
    },
    /// `SetChannelPassword(name, password)`.
    SetPassword {
        name: String,
        password: String,
    },
    Ban {
        name: String,
        player: String,
    },
    Invite {
        name: String,
        player: String,
    },
    Kick {
        name: String,
        player: String,
    },
    Moderator {
        name: String,
        player: String,
    },
    Unmoderator {
        name: String,
        player: String,
    },
    Mute {
        name: String,
        player: String,
    },
    Unmute {
        name: String,
        player: String,
    },
    Unban {
        name: String,
        player: String,
    },
    /// `ChannelModerate(name)` — toggles moderation.
    Moderate {
        name: String,
    },
    /// `ChannelToggleAnnouncements(name)`.
    ToggleAnnouncements {
        name: String,
    },
}

impl super::UiScript {
    /// The zone-channel catalog for the player's current zone — fed whenever the zone (and so
    /// every resolved name) changes.
    pub fn set_zone_channel_catalog(&mut self, rows: Vec<ZoneChannelRow>) {
        self.model_mut().zone_channel_catalog = rows;
    }

    /// Channel verbs called since the last drain, in call order.
    pub fn take_channel_commands(&mut self) -> Vec<ChannelCommand> {
        std::mem::take(&mut self.model_mut().channel_commands)
    }
}

fn push(lua: &Lua, cmd: ChannelCommand) {
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    model.channel_commands.push(cmd);
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn slot_of(model: &Model, name: &str) -> Option<usize> {
    model
        .joined_channels
        .iter()
        .position(|c| c.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(name)))
        .map(|i| i + 1)
}

/// The name occupying slot `n` (1-based), or `None` for out of range **or a freed slot**.
///
/// The hole case is the reference's, not a convenience: its by-index lookup `0x49bf30` bounds-checks
/// against the record count and then demands the entry's own number field equal the index asked for
/// (`cmp esi,ecx / jnz`), which a leave zeroed (`0x49bbd0`). So a channel left is a number that
/// answers "not joined" while every channel above it keeps its own (1286).
fn name_at(model: &Model, n: usize) -> Option<&str> {
    model.joined_channels.get(n.checked_sub(1)?)?.as_deref()
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetChannelName(indexOrName) → slot, name, instanceID.
    //
    // THE TRAP, and the whole risk in this verb: the first return is **always a NUMBER, never
    // nil** — `0` when the channel is not joined. Verified from both sides. The reference's own
    // callers compare it numerically and would raise on a nil: `ChatFrame.lua:2114`
    // `if ( channelNum > 0 )` and `l.2232` `if ( channelNum <= 0 ) then return end`; so does the
    // corpus, at `_LazyPig/LazyPig.lua:1996` `if id > 0 then`. Returning nil here would convert
    // three working call sites into "attempt to compare nil with number".
    //
    // NOT a shared helper with `JoinChannelByName`, whose first return is a DIFFERENT number — the
    // `ChatChannels.dbc` ChannelID, not this local slot index. wow-re calls that pair out
    // explicitly (`zone-chat-channel-autojoin.md` l.379) and it is an easy, silent mistake.
    g.set(
        "GetChannelName",
        lua.create_function(|lua, key: Value| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let slot = match &key {
                // The numeric form. The reference bounds-checks `1 <= n <= count` (`0x49bf30`) and
                // answers only for a CONFIRMED-joined slot; `joined` holds exactly the confirmed
                // ones (`ui_chat::feed` appends on the server's YOU_JOINED, not on the request), so
                // the bound is the whole check here.
                Value::Integer(_) | Value::Number(_) => {
                    let n = match &key {
                        Value::Integer(i) => *i,
                        Value::Number(n) => *n as i64,
                        _ => unreachable!(),
                    };
                    usize::try_from(n)
                        .ok()
                        .filter(|n| name_at(&model, *n).is_some())
                }
                // The name form. A numeric STRING arrives here and must still resolve as a number:
                // `ChatFrame.lua:2113` passes the result of a `gsub` — `GetChannelName("1")` — and
                // Lua's own coercion is what makes that work on the real client.
                Value::String(s) => {
                    let name = s.to_str()?;
                    match name.trim().parse::<usize>() {
                        Ok(n) if name_at(&model, n).is_some() => Some(n),
                        Ok(_) => None,
                        Err(_) => slot_of(&model, &name),
                    }
                }
                _ => None,
            };

            let Some(slot) = slot else {
                // **`0, nil, 0` — three values, not one.** This used to push the number alone,
                // reasoning that `channelName` is unread by every caller on this branch. True of
                // the callers; not true of the client. `0x4a05e0` pushes three on every path, and
                // slot 2 is neither the empty string nor the argument echoed back:
                // `0x4a0659 xor edx,edx` then `lua_pushstring(NULL)`, which tail-jumps to
                // `lua_pushnil`. Decision 1845.
                //
                // "Not joined" is also wider than a bad index: the lookup answers NULL while the
                // join-pending word is non-zero, so a channel already in the list but not yet
                // CONFIRMED reads `0, nil, 0` identically — which is what `joined` models here.
                return Ok(MultiValue::from_vec(vec![
                    Value::Integer(0),
                    Value::Nil,
                    Value::Integer(0),
                ]));
            };
            let name = name_at(&model, slot).unwrap_or_default().to_string();
            Ok(MultiValue::from_vec(vec![
                Value::Integer(slot as i64),
                Value::String(lua.create_string(&name)?),
                // instanceID — see the module doc: 0 on every vanilla emulator, and a number
                // rather than nil so a caller can compare it like the client's.
                Value::Integer(0),
            ]))
        })?,
    )?;

    // GetChannelList() → slot1, name1, slot2, name2, … over every joined channel, in join order.
    //
    // **The shape is settled by two independent consumers, not by a recorded signature** — wow-re
    // has the address (`0x4a02d0`, `scratch/bindings.md` l.152) and no contract:
    //
    //  · the reference's own `FCFDropDown_LoadChannels(...)` walks `for i=1, arg.n, 2` and reads
    //    `arg[i+1]` as the NAME (FloatingChatFrame.lua l.445-455) — so the pair is (slot, name),
    //    in that order, and the caller steps by two;
    //  · `ChatLog.lua:424` packs it with `{ GetChannelList() }` and tests
    //    `type(value) == "number"` to spot an id — so it is a FLAT vararg, never a table.
    //
    // A third witness pins the flatness harder: `AceComm-2.0.lua:334` unpacks TEN pairs in one
    // statement, `local _,a,_,b,…,j = GetChannelList()`.
    //
    // The slot numbering is [`slot_of`]'s — position in `joined_channels` + 1 — so this verb and
    // `GetChannelName` can never disagree about which channel is 3.
    //
    // Zero joined channels is zero returns, not nil: `{ GetChannelList() }` is then an empty
    // table, which is what every caller above already handles.
    //
    // Demand: 4 addons, and only ONE of them names it in its own source (ChatLog). The other
    // three — FuBar_BGQueueNumber, FuBar_MageFu, FuBar_TankPointsFu — reach it through their
    // embedded AceComm-2.0. That gap between "greps for the name" and "wants the name" is why the
    // survey's own read-back exists (`--why`, d2fcef94) and why a hand grep is not the oracle here.
    g.set(
        "GetChannelList",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let mut out = Vec::with_capacity(model.joined_channels.len() * 2);
            // Occupied slots only — a freed one is a number nothing is on, so it has no pair to
            // contribute (the reference walks its record array and skips the cleared entries).
            for (i, name) in model.joined_channels.iter().enumerate() {
                let Some(name) = name.as_deref() else {
                    continue;
                };
                out.push(Value::Integer(i as i64 + 1));
                out.push(Value::String(lua.create_string(name)?));
            }
            Ok(MultiValue::from_vec(out))
        })?,
    )?;

    // JoinChannelByName(name [, password [, frameId]]) — `0x49ff00` → `0x49eb70`, the contract
    // decision 1908 carved:
    //
    //   matched DBC row                        → (ChannelID, resolvedName)
    //   custom channel (no row)                → (0, nil)      — and 0 is truthy in Lua
    //   a space in the name, or a matched row  → nil           — no send
    //     whose zone substitution is empty
    //
    // `CMSG_JOIN_CHANNEL` goes out on both non-nil legs. The third argument is the window the
    // stock handler wants the channel in; that bookkeeping is `ChatFrame_AddChannel`'s (Lua), so
    // the verb ignores it — exactly as the reference does.
    g.set(
        "JoinChannelByName",
        lua.create_function(
            |lua, (name, password, _frame): (Option<String>, Option<String>, Value)| {
                let Some(name) = non_empty(name) else {
                    return Ok(MultiValue::from_vec(vec![Value::Nil]));
                };
                if name.contains(' ') {
                    return Ok(MultiValue::from_vec(vec![Value::Nil]));
                }
                let row = {
                    let model = lua.app_data_ref::<Model>().expect("model app_data");
                    model
                        .zone_channel_catalog
                        .iter()
                        .find(|r| r.shortcut.eq_ignore_ascii_case(&name))
                        .cloned()
                };
                let (id, resolved) = match row {
                    None => (0, None),
                    Some(ZoneChannelRow { resolved: None, .. }) => {
                        return Ok(MultiValue::from_vec(vec![Value::Nil]))
                    }
                    Some(ZoneChannelRow { id, resolved, .. }) => (id, resolved),
                };
                push(
                    lua,
                    ChannelCommand::Join {
                        name: resolved.clone().unwrap_or_else(|| name.clone()),
                        password: password.unwrap_or_default(),
                    },
                );
                Ok(MultiValue::from_vec(vec![
                    Value::Integer(i64::from(id)),
                    match resolved {
                        Some(r) => Value::String(lua.create_string(&r)?),
                        None => Value::Nil,
                    },
                ]))
            },
        )?,
    )?;

    // EnumerateServerChannels() — `0x4a1790` (chat-cache-grammar.md §6): the `ChatChannels.dbc`
    // **shortcuts** in row order, a `flags & 0x10` row only when the zone is a city
    // (`AreaTable.Flags & 0x8`); 0 values while the zone is unresolvable (an empty catalog here).
    // Never the composed `<name> - <zone>`: `FCFDropDown_LoadServerChannels` shows these bare.
    g.set(
        "EnumerateServerChannels",
        lua.create_function(|lua, _ignored: MultiValue| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let mut out = Vec::new();
            for row in model.zone_channel_catalog.iter().filter(|r| r.listed) {
                out.push(Value::String(lua.create_string(&row.shortcut)?));
            }
            Ok(MultiValue::from_vec(out))
        })?,
    )?;

    // The one-name verbs: each queues its `CMSG_*` and returns nothing.
    for (verb, make) in [
        (
            "LeaveChannelByName",
            (|name| ChannelCommand::Leave { name }) as fn(String) -> ChannelCommand,
        ),
        ("ListChannelByName", |name| ChannelCommand::List { name }),
        ("DisplayChannelOwner", |name| ChannelCommand::DisplayOwner {
            name,
        }),
        ("ChannelModerate", |name| ChannelCommand::Moderate { name }),
        ("ChannelToggleAnnouncements", |name| {
            ChannelCommand::ToggleAnnouncements { name }
        }),
    ] {
        g.set(
            verb,
            lua.create_function(move |lua, name: Option<String>| {
                if let Some(name) = non_empty(name) {
                    push(lua, make(name));
                }
                Ok(())
            })?,
        )?;
    }

    g.set(
        "ListChannels",
        lua.create_function(|lua, _ignored: MultiValue| {
            push(lua, ChannelCommand::ListAll);
            Ok(())
        })?,
    )?;

    // The two-name verbs: (channel, player) — or (channel, password) for the password.
    for (verb, make) in [
        (
            "SetChannelOwner",
            (|name, player| ChannelCommand::SetOwner { name, player })
                as fn(String, String) -> ChannelCommand,
        ),
        ("SetChannelPassword", |name, password| {
            ChannelCommand::SetPassword { name, password }
        }),
        ("ChannelBan", |name, player| ChannelCommand::Ban {
            name,
            player,
        }),
        ("ChannelInvite", |name, player| ChannelCommand::Invite {
            name,
            player,
        }),
        ("ChannelKick", |name, player| ChannelCommand::Kick {
            name,
            player,
        }),
        ("ChannelModerator", |name, player| {
            ChannelCommand::Moderator { name, player }
        }),
        ("ChannelUnmoderator", |name, player| {
            ChannelCommand::Unmoderator { name, player }
        }),
        ("ChannelMute", |name, player| ChannelCommand::Mute {
            name,
            player,
        }),
        ("ChannelUnmute", |name, player| ChannelCommand::Unmute {
            name,
            player,
        }),
        ("ChannelUnban", |name, player| ChannelCommand::Unban {
            name,
            player,
        }),
    ] {
        g.set(
            verb,
            lua.create_function(
                move |lua, (name, second): (Option<String>, Option<String>)| {
                    if let Some(name) = non_empty(name) {
                        // A password may legitimately be empty (`/password General` clears it); a
                        // player name may not.
                        let second = second.unwrap_or_default();
                        if verb == "SetChannelPassword" || !second.trim().is_empty() {
                            push(lua, make(name, second.trim().to_string()));
                        }
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    Ok(())
}

impl super::UiScript {
    /// Mirror the app's confirmed-joined channel list, in join order, for [`install`]'s verb.
    ///
    /// The `model.party` shape (`party.rs:172`), deliberately, and NOT the `open_chat_requests`
    /// shape the chat-window work used: that one is a QUEUE the app drains (Lua → app), and this is
    /// app state READ BY Lua, which is the opposite direction. `ui_chat::feed` owns both edges that
    /// change it — the server's YOU_JOINED and YOU_LEFT notices — so it pushes here from one place.
    pub fn set_joined_channels(&mut self, joined: Vec<Option<String>>) {
        self.model_mut().joined_channels = joined;
    }
}

#[cfg(test)]
mod command_tests {
    use super::{ChannelCommand, ZoneChannelRow};
    use crate::script::UiScript;

    fn catalog() -> Vec<ZoneChannelRow> {
        vec![
            ZoneChannelRow {
                id: 1,
                shortcut: "General".into(),
                resolved: Some("General - Elwynn Forest".into()),
                listed: true,
            },
            ZoneChannelRow {
                id: 2,
                shortcut: "Trade".into(),
                resolved: None,
                listed: false,
            },
        ]
    }

    /// Decision 1908's three legs, and that 0 is truthy.
    #[test]
    fn join_channel_by_name_answers_the_three_legs_of_1908() {
        let mut s = UiScript::new().unwrap();
        s.set_zone_channel_catalog(catalog());
        s.run(
            "A = {JoinChannelByName('General', 'pw', 1)} \
             B = {JoinChannelByName('MyChan', nil, 1)} \
             C = {JoinChannelByName('Trade', nil, 1)} \
             D = {JoinChannelByName('two words', nil, 1)}",
        )
        .unwrap();
        assert_eq!(
            s.eval::<(i64, String)>("return A[1], A[2]").unwrap(),
            (1, "General - Elwynn Forest".to_string())
        );
        assert!(s
            .eval::<bool>("return B[1] == 0 and B[2] == nil and table.getn(B) == 1")
            .unwrap());
        assert!(
            s.eval::<bool>("return C[1] == nil").unwrap(),
            "empty substitution"
        );
        assert!(s.eval::<bool>("return D[1] == nil").unwrap(), "a space");
        assert_eq!(
            s.take_channel_commands(),
            vec![
                ChannelCommand::Join {
                    name: "General - Elwynn Forest".into(),
                    password: "pw".into()
                },
                ChannelCommand::Join {
                    name: "MyChan".into(),
                    password: String::new()
                },
            ],
            "sent on both non-nil legs, the resolved name for a row"
        );
    }

    #[test]
    fn enumerate_server_channels_lists_the_shortcuts_the_zone_admits() {
        let mut s = UiScript::new().unwrap();
        assert_eq!(
            s.eval::<i64>("return select('#', EnumerateServerChannels())")
                .unwrap(),
            0,
            "no zone yet — 0 values"
        );
        s.set_zone_channel_catalog(catalog());
        assert_eq!(
            s.eval::<Vec<String>>("return {EnumerateServerChannels()}")
                .unwrap(),
            vec!["General".to_string()],
            "the shortcut, never the composed name; a city row only in a city"
        );
    }

    #[test]
    fn the_management_verbs_queue_in_call_order_and_drop_empty_names() {
        let mut s = UiScript::new().unwrap();
        s.run(
            "LeaveChannelByName('General') ListChannelByName('') ListChannels() \
             SetChannelPassword('General', '') ChannelKick('General', 'Bob') \
             ChannelKick('General', '') ChannelModerate('General') \
             ChannelToggleAnnouncements('General') SetChannelOwner('General', 'Ann')",
        )
        .unwrap();
        assert_eq!(
            s.take_channel_commands(),
            vec![
                ChannelCommand::Leave {
                    name: "General".into()
                },
                ChannelCommand::ListAll,
                ChannelCommand::SetPassword {
                    name: "General".into(),
                    password: String::new()
                },
                ChannelCommand::Kick {
                    name: "General".into(),
                    player: "Bob".into()
                },
                ChannelCommand::Moderate {
                    name: "General".into()
                },
                ChannelCommand::ToggleAnnouncements {
                    name: "General".into()
                },
                ChannelCommand::SetOwner {
                    name: "General".into(),
                    player: "Ann".into()
                },
            ]
        );
    }
}
