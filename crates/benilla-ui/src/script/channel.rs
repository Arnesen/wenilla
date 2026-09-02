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
