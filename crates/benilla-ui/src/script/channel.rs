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

/// The 1-based slot of `name` in join order, case-insensitively — `GetChannelName`'s first return.
fn slot_of(model: &Model, name: &str) -> Option<usize> {
    model
        .joined_channels
        .iter()
        .position(|c| c.eq_ignore_ascii_case(name))
        .map(|i| i + 1)
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
                        .filter(|n| *n >= 1 && *n <= model.joined_channels.len())
                }
                // The name form. A numeric STRING arrives here and must still resolve as a number:
                // `ChatFrame.lua:2113` passes the result of a `gsub` — `GetChannelName("1")` — and
                // Lua's own coercion is what makes that work on the real client.
                Value::String(s) => {
                    let name = s.to_str()?;
                    match name.trim().parse::<usize>() {
                        Ok(n) if n >= 1 && n <= model.joined_channels.len() => Some(n),
                        Ok(_) => None,
                        Err(_) => slot_of(&model, &name),
                    }
                }
                _ => None,
            };

            let Some(slot) = slot else {
                // Not joined: the number 0, and nothing else. `channelName` is unread by every
                // caller on this branch (all four sites bail on the number first).
                return Ok(MultiValue::from_vec(vec![Value::Integer(0)]));
            };
            let name = model.joined_channels[slot - 1].clone();
            Ok(MultiValue::from_vec(vec![
                Value::Integer(slot as i64),
                Value::String(lua.create_string(&name)?),
                // instanceID — see the module doc: 0 on every vanilla emulator, and a number
                // rather than nil so a caller can compare it like the client's.
                Value::Integer(0),
            ]))
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
    pub fn set_joined_channels(&mut self, joined: Vec<String>) {
        self.model_mut().joined_channels = joined;
    }
}
