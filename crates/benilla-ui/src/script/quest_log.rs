//! The quest-**log** bindings (the 0088 arc's second slice) — the Era-shaped `GetQuestLog*` surface
//! the reference `QuestLogFrame.lua` reads, over the same two-way seam as the questgiver panels
//! ([`super::quest`]): the app pushes a whole [`QuestLogState`] snapshot (entries from the
//! `PLAYER_QUEST_LOG` descriptor slots, the selected quest's detail resolved from the
//! `SMSG_QUEST_QUERY_RESPONSE` template cache), and the Lua drains back the **abandon** intent.
//!
//! Two engine-owned bits deliberately live *in the model*, not the pushed state:
//!
//! - **The selection** (`SelectQuestLogEntry`/`GetQuestLogSelection`). In the reference client the
//!   selection is native and *synchronous* — `QuestLogTitleButton_OnClick` calls
//!   `SelectQuestLogEntry(i)` then immediately re-reads `GetQuestLogSelection()` in the same click
//!   (ref `QuestLogFrame.lua:318,308-346`); a drain-next-frame intent would read stale. The app
//!   reads it back each frame ([`super::UiScript::quest_log_selection`]) and pushes the matching
//!   detail; the refresh lands as a `QUEST_LOG_UPDATE` event, exactly like the ref's async data.
//! - **The abandon mark** (`SetAbandonQuest`/`GetAbandonQuestName`/`AbandonQuest`) — the ref's
//!   two-step confirm (mark on button click, act on the popup's Yes — ref `QuestLogFrame.xml:463-472`,
//!   `StaticPopup.lua:749-761`). The mark pins the *entry index at click time* so a log shuffle
//!   between click and confirm can't retarget the abandon.
//!
//! A third engine-owned bit joined later: the **watch set** (`IsQuestWatched`/`AddQuestWatch`/
//! `RemoveQuestWatch`/`GetNumQuestWatches`/`GetQuestIndexForWatch`) — keyed by the entries' stable
//! [`QuestLogEntryView::quest_id`] under the index-based Era API, pruned on push, cap 5.
//!
//! v1 scope (the deliberate `nil`/false stubs, each a later slice): party share
//! (`GetQuestLogPushable`/`IsUnitOnQuest`), timed quests (`GetQuestLogTimeLeft`), reward spells
//! (`GetQuestLogRewardSpell`), and zone **headers** ([`QuestLogEntryView::is_header`] is plumbed
//! but the app pushes a flat list — headers need the QuestSort/AreaTable DBC join).

use mlua::{Lua, MultiValue, Value};

use super::quest::QuestItemView;
use super::Model;

/// One quest-log list row — the `GetQuestLogTitle` tuple's data half. Plain data; its 1-based
/// position in [`QuestLogState::entries`] is the quest-log index the whole API keys on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuestLogEntryView {
    /// The quest id occupying the row's descriptor slot. Never surfaced to Lua (no Era API returns
    /// it) — it is the engine's *stable identity* for the watch set ([`install`]'s
    /// `AddQuestWatch`/…): watching by id survives the log shuffling under the index-based API
    /// (an abandon compacts the rows; a watched *index* would silently retarget).
    pub quest_id: u32,
    /// The quest title (row label).
    pub title: String,
    /// The quest's display level (`GetQuestLogTitle` return 2; the ref colors the row by it).
    pub level: u32,
    /// The quest tag suffix (elite/dungeon/…) — `None` for a plain quest. v1 pushes `None`
    /// (the 1.12 wire's giver panels carry no tag; a template-derived tag is a later dressing).
    pub tag: Option<String>,
    /// A zone header row (the app synthesizes these from each quest's ZoneOrSort).
    pub is_header: bool,
    /// A COLLAPSED header (`GetQuestLogTitle`'s isCollapsed; meaningless on quest rows). The app
    /// owns the collapse set and omits a collapsed header's quests from `entries` — the engine
    /// only reports the flag and drains the toggle intents ([`super::UiScript::take_quest_log_collapses`]).
    pub collapsed: bool,
    /// Whole-quest state from the descriptor slot's state byte: `1` complete, `-1` failed, `0`
    /// in progress (`GetQuestLogTitle`'s `isComplete` is `1`/`-1`/`nil` off this).
    pub complete: i32,
    /// The formatted objective lines for THIS entry (`GetNumQuestLeaderBoards(i)` /
    /// `GetQuestLogLeaderBoard(j, i)` serve any index off these — the watch tracker HUD reads
    /// non-selected quests' lines). The selection's no-arg reads come from here too; the
    /// [`QuestLogDetail`] carries only what exists for the selection alone
    /// (description/rewards/money).
    pub objectives: Vec<QuestLogObjectiveView>,
}

/// One objective ("leaderboard") line of the selected quest — the `GetQuestLogLeaderBoard` tuple.
/// The app pre-formats `text` ("Kobold Vermin slain: 3/10") from the template + slot counters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuestLogObjectiveView {
    /// The display line, fully formatted.
    pub text: String,
    /// The objective kind (`"monster"` / `"item"` / `"object"` — the Era type string).
    pub kind: String,
    /// Whether this objective is done (darkens the line + `(Complete)` suffix in the ref).
    pub finished: bool,
}

/// The selected quest's detail pane — resolved by the app from the `QUEST_QUERY_RESPONSE` template
/// cache (+ item/creature name caches). `None` while the template is still in flight.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuestLogDetail {
    /// The long description (`GetQuestLogQuestText` return 1).
    pub description: String,
    /// The objectives flavor paragraph (`GetQuestLogQuestText` return 2) — *not* the leaderboard
    /// lines (those live on every [`QuestLogEntryView::objectives`], selection or not).
    pub objectives_text: String,
    /// Money the turn-in demands (`GetQuestLogRequiredMoney`), copper.
    pub required_money: u32,
    /// Reward money (`GetQuestLogRewardMoney`), copper.
    pub reward_money: u32,
    /// Choice rewards (`GetNumQuestLogChoices`/`GetQuestLogChoiceInfo`).
    pub choices: Vec<QuestItemView>,
    /// Fixed rewards (`GetNumQuestLogRewards`/`GetQuestLogRewardInfo`).
    pub rewards: Vec<QuestLogQuestItem>,
}

/// A quest-log reward row is the same shape as a questgiver panel row.
pub type QuestLogQuestItem = QuestItemView;

/// The whole quest-log snapshot the app pushes each time it changes. Unlike the questgiver panels
/// this is durable state, not a window session — an empty log is `entries: []`, never `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuestLogState {
    /// The list rows, in quest-log (descriptor slot) order. 1-based indexing on the Lua side.
    /// A collapsed header's quests are OMITTED here (the app filters) — indexes are visible rows.
    pub entries: Vec<QuestLogEntryView>,
    /// The total quest count INCLUDING quests hidden under collapsed headers — the "Quests: N/20"
    /// pill must not shrink when a header collapses (`GetNumQuestLogEntries` return 2).
    pub num_quests: u32,
    /// The detail pane for the current selection (`None` = nothing selected, selection out of
    /// range, or the template still in flight).
    pub detail: Option<QuestLogDetail>,
}

impl super::UiScript {
    /// Push the quest-log snapshot (the app calls this whenever slots/templates/selection change).
    /// Also prunes the watch set: a watched quest that left the log (abandon/turn-in) drops its
    /// watch, exactly as the real client's RemoveQuestWatch-on-removal does.
    pub fn set_quest_log(&mut self, state: QuestLogState) {
        let mut model = self.model_mut();
        model
            .quest_log_watched
            .retain(|id| state.entries.iter().any(|e| e.quest_id == *id));
        model.quest_log = state;
    }

    /// The watched quest ids, in watch order — the app fires `QUEST_WATCH_UPDATE` when a watched
    /// quest's objectives change, and this is how it knows which those are.
    pub fn quest_log_watched(&self) -> Vec<u32> {
        self.model_ref().quest_log_watched.clone()
    }

    /// The engine-owned 1-based selection (`SelectQuestLogEntry`'s last value; `0` = none) — the
    /// app reads it each frame to know which quest's detail to resolve and push.
    pub fn quest_log_selection(&self) -> u32 {
        self.model_ref().quest_log_selection
    }

    /// Drain the abandon intents: the 1-based entry index pinned by `SetAbandonQuest` at
    /// click time, confirmed by the popup's `AbandonQuest()`. The app maps index → descriptor
    /// slot → `CMSG_QUESTLOG_REMOVE_QUEST`.
    pub fn take_quest_log_abandons(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().quest_log_abandons)
    }

    /// Drain the header collapse/expand intents: `(1-based entry index, collapse)` from
    /// `CollapseQuestHeader`/`ExpandQuestHeader` — index `0` = ALL headers (the ref's
    /// collapse-all button passes 0, QuestLogFrame.lua:557/:561). The app owns the collapse
    /// set and re-feeds the filtered list (same intent pattern as the abandons above).
    pub fn take_quest_log_collapses(&mut self) -> Vec<(u32, bool)> {
        std::mem::take(&mut self.model_mut().quest_log_collapses)
    }

    /// Set the engine-owned 1-based selection from the app side — the collapse/expand rebuild
    /// shifts entry indexes, and the app re-points the selection at the SAME quest's new index
    /// (selection is an index to the Era API, but an identity to the player).
    pub fn set_quest_log_selection(&mut self, i: u32) {
        self.model_mut().quest_log_selection = i;
    }
}

/// Register the quest-log globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetNumQuestLogEntries() → numEntries, numQuests (headers count toward the first only —
    // ref QuestLogFrame.lua:108; flat v1 pushes no headers so the two are equal).
    g.set(
        "GetNumQuestLogEntries",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let entries = &model.quest_log.entries;
            Ok((entries.len() as i64, i64::from(model.quest_log.num_quests)))
        })?,
    )?;

    // GetQuestLogTitle(i) → title, level, tag, isHeader, isCollapsed, isComplete
    // (the load-bearing 6-tuple — ref QuestLogFrame.lua:144/:272/:321/:571). Out of range → nil.
    // isCollapsed is always false (no headers in v1); isComplete is 1 / -1 / nil.
    g.set(
        "GetQuestLogTitle",
        lua.create_function(|lua, i: usize| {
            let entry = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                i.checked_sub(1)
                    .and_then(|n| model.quest_log.entries.get(n))
                    .cloned()
            };
            let Some(e) = entry else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let tag = match &e.tag {
                Some(t) => Value::String(lua.create_string(t)?),
                None => Value::Nil,
            };
            let complete = match e.complete {
                0 => Value::Nil,
                c => Value::Integer(i64::from(c.signum())),
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&e.title)?),
                Value::Integer(i64::from(e.level)),
                tag,
                Value::Boolean(e.is_header),
                Value::Boolean(e.collapsed),
                complete,
            ]))
        })?,
    )?;

    // SelectQuestLogEntry(i) — the synchronous, engine-owned selection (see module doc).
    g.set(
        "SelectQuestLogEntry",
        lua.create_function(|lua, i: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.quest_log_selection = i;
            Ok(())
        })?,
    )?;

    // GetQuestLogSelection() → the 1-based selection, 0 = none.
    g.set(
        "GetQuestLogSelection",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(model.quest_log_selection))
        })?,
    )?;

    // GetQuestLogQuestText() → description, objectivesText (for the selection — the app already
    // resolved `detail` against it).
    g.set(
        "GetQuestLogQuestText",
        lua.create_function(|lua, ()| {
            let (desc, obj) = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model
                    .quest_log
                    .detail
                    .as_ref()
                    .map(|d| (d.description.clone(), d.objectives_text.clone()))
                    .unwrap_or_default()
            };
            Ok((
                Value::String(lua.create_string(&desc)?),
                Value::String(lua.create_string(&obj)?),
            ))
        })?,
    )?;

    // GetNumQuestLeaderBoards([questIndex]) — objective-line count for the given entry (1-based),
    // defaulting to the selection. Any index answers (the watch tracker HUD reads non-selected
    // quests' lines — ref QuestLogFrame.lua:613-663).
    g.set(
        "GetNumQuestLeaderBoards",
        lua.create_function(|lua, i: Option<u32>| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let index = i.unwrap_or(model.quest_log_selection) as usize;
            Ok(index
                .checked_sub(1)
                .and_then(|n| model.quest_log.entries.get(n))
                .map(|e| e.objectives.len() as i64)
                .unwrap_or(0))
        })?,
    )?;

    // GetQuestLogLeaderBoard(i[, questIndex]) → text, type, finished — same any-index rule.
    g.set(
        "GetQuestLogLeaderBoard",
        lua.create_function(|lua, (i, quest): (usize, Option<u32>)| {
            let line = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                let index = quest.unwrap_or(model.quest_log_selection) as usize;
                index
                    .checked_sub(1)
                    .and_then(|n| model.quest_log.entries.get(n))
                    .and_then(|e| i.checked_sub(1).and_then(|n| e.objectives.get(n)))
                    .cloned()
            };
            let Some(l) = line else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&l.text)?),
                Value::String(lua.create_string(&l.kind)?),
                Value::Boolean(l.finished),
            ]))
        })?,
    )?;

    // ── Detail counts + money ─────────────────────────────────────────────────────────────────────
    fn install_detail_count(
        lua: &Lua,
        name: &str,
        pick: fn(&super::quest_log::QuestLogDetail) -> i64,
    ) -> mlua::Result<()> {
        lua.globals().set(
            name,
            lua.create_function(move |lua, ()| {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                Ok(model.quest_log.detail.as_ref().map(pick).unwrap_or(0))
            })?,
        )
    }
    install_detail_count(lua, "GetNumQuestLogRewards", |d| d.rewards.len() as i64)?;
    install_detail_count(lua, "GetNumQuestLogChoices", |d| d.choices.len() as i64)?;
    install_detail_count(lua, "GetQuestLogRewardMoney", |d| i64::from(d.reward_money))?;
    install_detail_count(lua, "GetQuestLogRequiredMoney", |d| {
        i64::from(d.required_money)
    })?;

    // GetQuestLogChoiceInfo(i) / GetQuestLogRewardInfo(i) → name, texture, numItems, quality,
    // isUsable — the same 5-tuple as the giver panels' GetQuestItemInfo (they share one layout
    // routine in the ref — QuestFrame.lua:311-522).
    fn install_detail_item(
        lua: &Lua,
        name: &str,
        pick: fn(&super::quest_log::QuestLogDetail) -> &Vec<QuestItemView>,
    ) -> mlua::Result<()> {
        lua.globals().set(
            name,
            lua.create_function(move |lua, i: usize| {
                let item = {
                    let model = lua.app_data_ref::<Model>().expect("model app_data");
                    model
                        .quest_log
                        .detail
                        .as_ref()
                        .and_then(|d| i.checked_sub(1).and_then(|n| pick(d).get(n)).cloned())
                };
                let Some(it) = item else {
                    return Ok(MultiValue::from_vec(vec![Value::Nil]));
                };
                let name_v = match &it.name {
                    Some(n) => Value::String(lua.create_string(n)?),
                    None => Value::Nil,
                };
                let texture = match &it.texture {
                    Some(t) => Value::String(lua.create_string(t)?),
                    None => Value::Nil,
                };
                Ok(MultiValue::from_vec(vec![
                    name_v,
                    texture,
                    Value::Integer(i64::from(it.count)),
                    Value::Integer(i64::from(it.quality)),
                    Value::Boolean(it.usable),
                    // Benilla extension (6th): the item id, the shared tooltip store's key.
                    Value::Integer(i64::from(it.item_id)),
                ]))
            })?,
        )
    }
    install_detail_item(lua, "GetQuestLogChoiceInfo", |d| &d.choices)?;
    install_detail_item(lua, "GetQuestLogRewardInfo", |d| &d.rewards)?;

    // IsCurrentQuestFailed() → the selection's slot state is FAIL (ref appends " - (Failed)").
    g.set(
        "IsCurrentQuestFailed",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let sel = model.quest_log_selection as usize;
            Ok(sel
                .checked_sub(1)
                .and_then(|n| model.quest_log.entries.get(n))
                .is_some_and(|e| e.complete < 0))
        })?,
    )?;

    // ── The abandon two-step (mark → confirm) ─────────────────────────────────────────────────────
    // CollapseQuestHeader(i)/ExpandQuestHeader(i) — header fold intents, drained by the app
    // (which owns the collapse set and re-feeds the filtered list). Index 0 = ALL headers (the
    // ref's collapse-all button, QuestLogFrame.lua:557/:561). Same intent pattern as the abandons.
    g.set(
        "CollapseQuestHeader",
        lua.create_function(|lua, i: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.quest_log_collapses.push((i, true));
            Ok(())
        })?,
    )?;
    g.set(
        "ExpandQuestHeader",
        lua.create_function(|lua, i: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.quest_log_collapses.push((i, false));
            Ok(())
        })?,
    )?;

    // SetAbandonQuest() — pin the current selection as the abandon target (ref
    // QuestLogFrame.xml:464).
    g.set(
        "SetAbandonQuest",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.quest_log_abandon_mark = model.quest_log_selection;
            Ok(())
        })?,
    )?;
    // GetAbandonQuestName() → the pinned target's title ("" when none — the popup shows it).
    g.set(
        "GetAbandonQuestName",
        lua.create_function(|lua, ()| {
            let title = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                (model.quest_log_abandon_mark as usize)
                    .checked_sub(1)
                    .and_then(|n| model.quest_log.entries.get(n))
                    .map(|e| e.title.clone())
                    .unwrap_or_default()
            };
            Ok(Value::String(lua.create_string(&title)?))
        })?,
    )?;
    // GetAbandonQuestItems() → nil in v1 (the "you will also lose these quest items" list needs
    // the template's SrcItem/required-item join against the bags — a dressing follow-up).
    g.set(
        "GetAbandonQuestItems",
        lua.create_function(|_, ()| Ok(Value::Nil))?,
    )?;
    // AbandonQuest() — queue the pinned target as an outbound intent (the popup's Yes).
    g.set(
        "AbandonQuest",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let mark = std::mem::take(&mut model.quest_log_abandon_mark);
            if mark != 0 {
                model.quest_log_abandons.push(mark);
            }
            Ok(())
        })?,
    )?;

    // ── v1 stubs (each the seam of a named later slice — see the module doc) ─────────────────────
    g.set(
        "GetQuestLogTimeLeft",
        lua.create_function(|_, ()| Ok(Value::Nil))?,
    )?;
    g.set(
        "GetQuestLogRewardSpell",
        lua.create_function(|_, ()| Ok(Value::Nil))?,
    )?;
    g.set(
        "GetQuestLogPushable",
        lua.create_function(|_, ()| Ok(false))?,
    )?;
    g.set(
        "IsUnitOnQuest",
        lua.create_function(|_, (_q, _unit): (Value, Value)| Ok(false))?,
    )?;
    // ── The quest watch (the on-screen tracker's state — ref QuestLogFrame.lua:469-505 shift-click
    // toggle, :613-663 QuestWatch_Update). The set lives engine-side, keyed by the entries' stable
    // quest ids (see QuestLogEntryView::quest_id) while the whole Era API speaks 1-based log
    // indices; set_quest_log prunes watches whose quest left the log. MAX_WATCHABLE_QUESTS = 5
    // (ref QuestLogFrame.lua:494) — AddQuestWatch past the cap is a no-op (the Lua shows the
    // QUEST_WATCH_TOO_MANY error itself, mirroring the ref's guard order).
    fn watch_id_at(model: &Model, index: u32) -> Option<u32> {
        (index as usize)
            .checked_sub(1)
            .and_then(|n| model.quest_log.entries.get(n))
            .map(|e| e.quest_id)
    }
    g.set(
        "GetNumQuestWatches",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.quest_log_watched.len() as i64)
        })?,
    )?;
    g.set(
        "IsQuestWatched",
        lua.create_function(|lua, i: u32| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(watch_id_at(&model, i).is_some_and(|id| model.quest_log_watched.contains(&id)))
        })?,
    )?;
    g.set(
        "AddQuestWatch",
        lua.create_function(|lua, i: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some(id) = watch_id_at(&model, i) {
                if !model.quest_log_watched.contains(&id) && model.quest_log_watched.len() < 5 {
                    model.quest_log_watched.push(id);
                }
            }
            Ok(())
        })?,
    )?;
    g.set(
        "RemoveQuestWatch",
        lua.create_function(|lua, i: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some(id) = watch_id_at(&model, i) {
                model.quest_log_watched.retain(|w| *w != id);
            }
            Ok(())
        })?,
    )?;
    // GetQuestIndexForWatch(watchSlot) → the watched quest's CURRENT 1-based log index (nil if it
    // somehow isn't in the log — pruning makes that transient at worst).
    g.set(
        "GetQuestIndexForWatch",
        lua.create_function(|lua, w: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let index = w
                .checked_sub(1)
                .and_then(|n| model.quest_log_watched.get(n))
                .and_then(|id| {
                    model
                        .quest_log
                        .entries
                        .iter()
                        .position(|e| e.quest_id == *id)
                })
                .map(|p| p as i64 + 1);
            Ok(match index {
                Some(i) => Value::Integer(i),
                None => Value::Nil,
            })
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{QuestLogDetail, QuestLogEntryView, QuestLogObjectiveView, QuestLogState};
    use crate::script::{QuestItemView, UiScript};

    fn two_quests() -> QuestLogState {
        QuestLogState {
            num_quests: 2,
            entries: vec![
                QuestLogEntryView {
                    quest_id: 783,
                    title: "A Threat Within".into(),
                    level: 1,
                    complete: 0,
                    objectives: vec![QuestLogObjectiveView {
                        text: "Kobold Vermin slain: 3/10".into(),
                        kind: "monster".into(),
                        finished: false,
                    }],
                    ..Default::default()
                },
                QuestLogEntryView {
                    quest_id: 7,
                    title: "Kobold Camp Cleanup".into(),
                    level: 3,
                    complete: 1,
                    objectives: vec![QuestLogObjectiveView {
                        text: "Kobold Worker slain: 10/10".into(),
                        kind: "monster".into(),
                        finished: true,
                    }],
                    ..Default::default()
                },
            ],
            detail: Some(QuestLogDetail {
                description: "Speak with Marshal McBride.".into(),
                objectives_text: "Report to Marshal McBride.".into(),
                required_money: 0,
                reward_money: 40,
                choices: vec![],
                rewards: vec![QuestItemView {
                    item_id: 0,
                    name: Some("Militia Hammer".into()),
                    texture: None,
                    count: 1,
                    quality: 1,
                    usable: true,
                }],
            }),
        }
    }

    #[test]
    fn entries_and_title_tuple() {
        let mut s = UiScript::new().unwrap();
        // Empty log: 0, 0; out-of-range title is nil.
        assert_eq!(
            s.eval::<(i64, i64)>("return GetNumQuestLogEntries()")
                .unwrap(),
            (0, 0)
        );
        assert!(s.eval::<bool>("return GetQuestLogTitle(1) == nil").unwrap());

        s.set_quest_log(two_quests());
        assert_eq!(
            s.eval::<(i64, i64)>("return GetNumQuestLogEntries()")
                .unwrap(),
            (2, 2)
        );
        // The 6-tuple, in order; complete=0 → nil, complete=1 → 1.
        assert!(s
            .eval::<bool>(
                "local t, l, tag, h, c, done = GetQuestLogTitle(1)\n\
                 return t == 'A Threat Within' and l == 1 and tag == nil\n\
                    and h == false and c == false and done == nil"
            )
            .unwrap());
        assert!(s
            .eval::<bool>(
                "local t, _, _, _, _, done = GetQuestLogTitle(2)\n\
                 return t == 'Kobold Camp Cleanup' and done == 1"
            )
            .unwrap());
    }

    #[test]
    fn selection_is_synchronous() {
        let mut s = UiScript::new().unwrap();
        s.set_quest_log(two_quests());
        assert_eq!(s.eval::<i64>("return GetQuestLogSelection()").unwrap(), 0);
        // The ref's click flow: select then immediately re-read in the same chunk.
        assert_eq!(
            s.eval::<i64>("SelectQuestLogEntry(2); return GetQuestLogSelection()")
                .unwrap(),
            2
        );
        assert_eq!(s.quest_log_selection(), 2);
    }

    #[test]
    fn detail_getters_read() {
        let mut s = UiScript::new().unwrap();
        s.set_quest_log(two_quests());
        s.run("SelectQuestLogEntry(1)").unwrap();
        assert!(s
            .eval::<bool>(
                "local d, o = GetQuestLogQuestText()\n\
                 return d == 'Speak with Marshal McBride.' and o == 'Report to Marshal McBride.'"
            )
            .unwrap());
        assert_eq!(
            s.eval::<i64>("return GetNumQuestLeaderBoards()").unwrap(),
            1
        );
        // The optional-arg form answers for ANY entry (the tracker HUD's path).
        assert_eq!(
            s.eval::<i64>("return GetNumQuestLeaderBoards(1)").unwrap(),
            1
        );
        assert_eq!(
            s.eval::<i64>("return GetNumQuestLeaderBoards(2)").unwrap(),
            1
        );
        assert!(s
            .eval::<bool>(
                "local t, k, f = GetQuestLogLeaderBoard(1, 2)\n\
                 return t == 'Kobold Worker slain: 10/10' and k == 'monster' and f == true"
            )
            .unwrap());
        assert!(s
            .eval::<bool>(
                "local t, k, f = GetQuestLogLeaderBoard(1)\n\
                 return t == 'Kobold Vermin slain: 3/10' and k == 'monster' and f == false"
            )
            .unwrap());
        assert_eq!(
            s.eval::<i64>("return GetQuestLogRewardMoney()").unwrap(),
            40
        );
        assert_eq!(s.eval::<i64>("return GetNumQuestLogRewards()").unwrap(), 1);
        assert!(s
            .eval::<bool>(
                "local n, _, c, q, u = GetQuestLogRewardInfo(1)\n\
                 return n == 'Militia Hammer' and c == 1 and q == 1 and u == true"
            )
            .unwrap());
        assert!(s
            .eval::<bool>("return GetQuestLogChoiceInfo(1) == nil")
            .unwrap());
    }

    #[test]
    fn abandon_two_step_pins_the_click_time_target() {
        let mut s = UiScript::new().unwrap();
        s.set_quest_log(two_quests());
        s.run("SelectQuestLogEntry(2); SetAbandonQuest()").unwrap();
        assert_eq!(
            s.eval::<String>("return GetAbandonQuestName()").unwrap(),
            "Kobold Camp Cleanup"
        );
        // Selection moves before the confirm — the mark must not follow it.
        s.run("SelectQuestLogEntry(1); AbandonQuest()").unwrap();
        assert_eq!(s.take_quest_log_abandons(), vec![2]);
        assert!(s.take_quest_log_abandons().is_empty(), "drained");
        // A bare AbandonQuest with no mark queues nothing.
        s.run("AbandonQuest()").unwrap();
        assert!(s.take_quest_log_abandons().is_empty());
    }

    #[test]
    fn failed_state_reads() {
        let mut s = UiScript::new().unwrap();
        let mut state = two_quests();
        state.entries[0].complete = -1;
        s.set_quest_log(state);
        s.run("SelectQuestLogEntry(1)").unwrap();
        assert!(s.eval::<bool>("return IsCurrentQuestFailed()").unwrap());
        assert!(s
            .eval::<bool>("local _, _, _, _, _, done = GetQuestLogTitle(1)\nreturn done == -1")
            .unwrap());
        s.run("SelectQuestLogEntry(2)").unwrap();
        assert!(!s.eval::<bool>("return IsCurrentQuestFailed()").unwrap());
    }

    #[test]
    fn watch_set_is_id_keyed_and_survives_a_log_shuffle() {
        let mut s = UiScript::new().unwrap();
        s.set_quest_log(two_quests());
        // Watch entry 2 (quest 7); the index-based API reads it back.
        s.run("AddQuestWatch(2)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumQuestWatches()").unwrap(), 1);
        assert!(s.eval::<bool>("return IsQuestWatched(2)").unwrap());
        assert!(!s.eval::<bool>("return IsQuestWatched(1)").unwrap());
        assert_eq!(s.eval::<i64>("return GetQuestIndexForWatch(1)").unwrap(), 2);
        assert_eq!(s.quest_log_watched(), vec![7]);

        // The log shuffles: entry 1 (quest 783) is abandoned, quest 7 compacts to index 1 — the
        // watch FOLLOWS THE QUEST, not the index.
        let mut shuffled = two_quests();
        shuffled.entries.remove(0);
        s.set_quest_log(shuffled);
        assert_eq!(s.eval::<i64>("return GetNumQuestWatches()").unwrap(), 1);
        assert!(s.eval::<bool>("return IsQuestWatched(1)").unwrap());
        assert_eq!(s.eval::<i64>("return GetQuestIndexForWatch(1)").unwrap(), 1);

        // Unwatch by the new index; and a watched quest leaving the log prunes its watch.
        s.run("RemoveQuestWatch(1)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumQuestWatches()").unwrap(), 0);
        s.run("AddQuestWatch(1)").unwrap();
        s.set_quest_log(QuestLogState::default());
        assert_eq!(s.eval::<i64>("return GetNumQuestWatches()").unwrap(), 0);
    }

    #[test]
    fn watch_cap_is_five_and_dupes_are_ignored() {
        let mut s = UiScript::new().unwrap();
        let mut state = QuestLogState::default();
        for q in 1..=6u32 {
            state.entries.push(QuestLogEntryView {
                quest_id: q,
                title: format!("Quest {q}"),
                level: 1,
                ..Default::default()
            });
        }
        s.set_quest_log(state);
        for i in 1..=6 {
            s.run(&format!("AddQuestWatch({i})")).unwrap();
        }
        // The sixth add is a no-op (MAX_WATCHABLE_QUESTS = 5, ref QuestLogFrame.lua:494).
        assert_eq!(s.eval::<i64>("return GetNumQuestWatches()").unwrap(), 5);
        // A duplicate add neither grows nor reorders.
        s.run("AddQuestWatch(1)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumQuestWatches()").unwrap(), 5);
        assert_eq!(s.quest_log_watched(), vec![1, 2, 3, 4, 5]);
    }
}
