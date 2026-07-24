//! The gossip bindings (decision 0081 phase 3) — the Era-shaped NPC-dialog surface, the same
//! two-way seam as [`super::container`]: the app pushes a **gossip menu snapshot**
//! ([`UiScript::set_gossip`] — the greeting + option rows already resolved from the wire), and the
//! Lua `SelectGossipOption`/`CloseGossip` calls queue outbound **intents** the app drains
//! ([`UiScript::take_gossip_selects`] / [`UiScript::take_gossip_close`]). The engine holds no NPC
//! knowledge — an option is "a label, an icon type, and whether it's coded".
//!
//! ## The Era API shape
//!
//! 1.12's `GetGossipOptions()` returns a flat vararg of `(text, type)` pairs — the FrameXML's
//! `GossipFrameUpdate` walks it two-at-a-time, `SetText`ing each `GossipTitleButton` and mapping the
//! `type` string to an `Interface\GossipFrame\<Type>GossipIcon` texture. Benilla keeps that exact
//! shape: `GetGossipOptions()` returns `label1, type1, label2, type2, …`, where `type` is the
//! lowercase Era icon name (`"gossip"`/`"vendor"`/`"taxi"`/`"trainer"`/…) the app derived from the
//! wire `GOSSIP_ICON` byte. The one addition is `IsGossipOptionCoded(i)` — a benilla-local
//! predicate the XML uses to grey password-gated options (decision 0081 v1: coded options are
//! parsed and greyed, never selected; the real client pops a password box, out of scope here).
//! `GetGossipText()` returns the greeting body (`SMSG_NPC_TEXT_UPDATE`), `nil` while its ask-once
//! query is still in flight.

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// One gossip menu option, resolved by the app from the wire `GossipOption` (decision 0081). Plain
/// data — 1-based order in the menu is its position in [`GossipMenu::options`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GossipOptionView {
    /// The option label (`SetText` on its row button).
    pub label: String,
    /// The Era `GetGossipOptions()` second value — the lowercase icon *type* the app mapped from
    /// the wire `GOSSIP_ICON` byte (`"gossip"`/`"vendor"`/`"taxi"`/`"trainer"`/…). The XML resolves
    /// it to a `Interface\GossipFrame\<Type>GossipIcon` texture.
    pub icon_type: String,
    /// A password-gated (`coded`) option — greyed and unselectable in v1 (decision 0081).
    pub coded: bool,
}

/// One quest row riding a gossip menu (`SMSG_GOSSIP_MESSAGE`'s quest-option block). A gossip NPC
/// that also gives quests lists them above the gossip options; a click sends
/// `CMSG_QUESTGIVER_QUERY_QUEST` (decision 0088). `active` splits the row into the "current quests"
/// vs "available quests" headers (the app derives it from the wire dialog-status icon); the app maps
/// the clicked 1-based row back to its quest id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GossipQuestRow {
    pub title: String,
    pub active: bool,
}

/// One open gossip menu: the greeting body (once its query lands), the quest rows, and the option
/// rows. Pushed whole by the app; `None` means no menu is open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GossipMenu {
    /// The NPC greeting (`SMSG_NPC_TEXT_UPDATE`); `None` while the ask-once query is in flight.
    pub greeting: Option<String>,
    /// Quest rows the NPC offers/has active, riding the same packet (decision 0088).
    pub quests: Vec<GossipQuestRow>,
    pub options: Vec<GossipOptionView>,
}

impl super::UiScript {
    /// Push (or clear, with `None`) the open gossip menu.
    pub fn set_gossip(&mut self, menu: Option<GossipMenu>) {
        self.model_mut().gossip = menu;
    }

    /// Drain the 1-based option positions queued by `SelectGossipOption` since the last call.
    pub fn take_gossip_selects(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().gossip_selects)
    }

    /// Whether `CloseGossip` was called since the last drain (and clear the flag). vanilla's
    /// client-side close sends no packet — the app just clears its local menu state.
    pub fn take_gossip_close(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().gossip_close)
    }

    /// Drain the 1-based quest-row positions queued by `SelectGossipQuest` since the last call. The
    /// app maps each to the row's quest id + the open NPC and sends `CMSG_QUESTGIVER_QUERY_QUEST`.
    pub fn take_gossip_quest_selects(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().gossip_quest_selects)
    }
}

/// Register the gossip globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // → the greeting body, or nil while its query is in flight / there is no menu.
    g.set(
        "GetGossipText",
        lua.create_function(|lua, ()| {
            let text = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model.gossip.as_ref().and_then(|m| m.greeting.clone())
            };
            match text {
                Some(t) => Ok(Value::String(lua.create_string(&t)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // → flat (label, type) pairs, the Era `GetGossipOptions()` vararg shape.
    g.set(
        "GetGossipOptions",
        lua.create_function(|lua, ()| {
            // Collect owned data under one short borrow, then build the Lua values with none held.
            let pairs: Vec<(String, String)> = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model.gossip.as_ref().map_or_else(Vec::new, |m| {
                    m.options
                        .iter()
                        .map(|o| (o.label.clone(), o.icon_type.clone()))
                        .collect()
                })
            };
            let mut out: Vec<Value> = Vec::with_capacity(pairs.len() * 2);
            for (label, icon) in pairs {
                out.push(Value::String(lua.create_string(&label)?));
                out.push(Value::String(lua.create_string(&icon)?));
            }
            Ok(MultiValue::from_vec(out))
        })?,
    )?;

    // IsGossipOptionCoded(i) — benilla-local: the XML greys a coded (password) option (decision
    // 0081). `i` is 1-based; an out-of-range index is `false`.
    g.set(
        "IsGossipOptionCoded",
        lua.create_function(|lua, i: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model
                .gossip
                .as_ref()
                .and_then(|m| i.checked_sub(1).and_then(|n| m.options.get(n)))
                .is_some_and(|o| o.coded))
        })?,
    )?;

    // SelectGossipOption(i [, ...]) — queue the 1-based option position; the app maps it to the
    // wire option index + guid. Extra Era args (the code / a confirm flag) are ignored: v1 never
    // sends a code (decision 0081).
    g.set(
        "SelectGossipOption",
        lua.create_function(|lua, (i, _rest): (u32, mlua::MultiValue)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.gossip_selects.push(i);
            Ok(())
        })?,
    )?;

    // CloseGossip() — client-side close (no packet, vanilla): flag it so the app clears its menu.
    g.set(
        "CloseGossip",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.gossip_close = true;
            Ok(())
        })?,
    )?;

    // → the number of quest rows the open menu carries (0 when none / no menu).
    g.set(
        "GetNumGossipQuests",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.gossip.as_ref().map_or(0, |m| m.quests.len()) as i64)
        })?,
    )?;

    // GetGossipQuestInfo(i) → title, isActive (1-based; out of range → nil).
    g.set(
        "GetGossipQuestInfo",
        lua.create_function(|lua, i: usize| {
            let row = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model
                    .gossip
                    .as_ref()
                    .and_then(|m| i.checked_sub(1).and_then(|n| m.quests.get(n)))
                    .cloned()
            };
            match row {
                Some(r) => Ok(MultiValue::from_vec(vec![
                    Value::String(lua.create_string(&r.title)?),
                    Value::Boolean(r.active),
                ])),
                None => Ok(MultiValue::from_vec(vec![Value::Nil])),
            }
        })?,
    )?;

    // SelectGossipQuest(i) — queue the 1-based quest row; the app maps it to the quest id + guid and
    // sends CMSG_QUESTGIVER_QUERY_QUEST.
    g.set(
        "SelectGossipQuest",
        lua.create_function(|lua, i: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.gossip_quest_selects.push(i);
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{GossipMenu, GossipOptionView};
    use crate::script::UiScript;

    fn menu() -> GossipMenu {
        GossipMenu {
            greeting: Some("Greetings, traveler.".into()),
            quests: Vec::new(),
            options: vec![
                GossipOptionView {
                    label: "Let me browse your goods.".into(),
                    icon_type: "vendor".into(),
                    coded: false,
                },
                GossipOptionView {
                    label: "I would like to sign the petition.".into(),
                    icon_type: "gossip".into(),
                    coded: true,
                },
            ],
        }
    }

    #[test]
    fn gossip_snapshot_reads_and_selects_queue() {
        let mut s = UiScript::new().unwrap();
        // No menu: text nil, no options.
        assert!(s.eval::<bool>("return GetGossipText() == nil").unwrap());
        assert_eq!(
            s.eval::<i64>("return select('#', GetGossipOptions())")
                .unwrap(),
            0
        );

        s.set_gossip(Some(menu()));
        assert_eq!(
            s.eval::<String>("return GetGossipText()").unwrap(),
            "Greetings, traveler."
        );
        // Flat (label, type) pairs: 2 options → 4 return values.
        let (n, l1, t1, l2, t2) = s
            .eval::<(i64, String, String, String, String)>(
                "local a,b,c,d = GetGossipOptions()\n\
                 return select('#', GetGossipOptions()), a, b, c, d",
            )
            .unwrap();
        assert_eq!(n, 4);
        assert_eq!(
            (l1.as_str(), t1.as_str()),
            ("Let me browse your goods.", "vendor")
        );
        assert_eq!(l2, "I would like to sign the petition.");
        assert_eq!(t2, "gossip");
        // The coded (petition) option is flagged for greying; the vendor one isn't.
        assert!(!s.eval::<bool>("return IsGossipOptionCoded(1)").unwrap());
        assert!(s.eval::<bool>("return IsGossipOptionCoded(2)").unwrap());
        assert!(!s.eval::<bool>("return IsGossipOptionCoded(9)").unwrap()); // out of range

        // Selecting queues the 1-based position; closing flags the intent.
        s.run("SelectGossipOption(1)").unwrap();
        s.run("SelectGossipOption(2, 'unused-code')").unwrap(); // extra arg ignored
        assert_eq!(s.take_gossip_selects(), vec![1, 2]);
        assert!(s.take_gossip_selects().is_empty(), "drained");

        assert!(!s.take_gossip_close());
        s.run("CloseGossip()").unwrap();
        assert!(s.take_gossip_close());
        assert!(!s.take_gossip_close(), "drained");
    }

    #[test]
    fn gossip_quest_rows_read_and_select() {
        use super::GossipQuestRow;
        let mut s = UiScript::new().unwrap();
        // No menu → zero rows.
        assert_eq!(s.eval::<i64>("return GetNumGossipQuests()").unwrap(), 0);
        let mut m = menu();
        m.quests = vec![
            GossipQuestRow {
                title: "Report to Goldshire".into(),
                active: true,
            },
            GossipQuestRow {
                title: "A Threat Within".into(),
                active: false,
            },
        ];
        s.set_gossip(Some(m));
        assert_eq!(s.eval::<i64>("return GetNumGossipQuests()").unwrap(), 2);
        let (title, active) = s
            .eval::<(String, bool)>("return GetGossipQuestInfo(1)")
            .unwrap();
        assert_eq!((title.as_str(), active), ("Report to Goldshire", true));
        assert!(s
            .eval::<bool>(
                "local t, a = GetGossipQuestInfo(2) return t == 'A Threat Within' and a == false"
            )
            .unwrap());
        assert!(s
            .eval::<bool>("return GetGossipQuestInfo(9) == nil")
            .unwrap());

        s.run("SelectGossipQuest(2)").unwrap();
        assert_eq!(s.take_gossip_quest_selects(), vec![2]);
        assert!(s.take_gossip_quest_selects().is_empty(), "drained");
    }

    #[test]
    fn clearing_the_menu_empties_it() {
        let mut s = UiScript::new().unwrap();
        s.set_gossip(Some(menu()));
        s.set_gossip(None);
        assert!(s.eval::<bool>("return GetGossipText() == nil").unwrap());
        assert_eq!(
            s.eval::<i64>("return select('#', GetGossipOptions())")
                .unwrap(),
            0
        );
    }
}
