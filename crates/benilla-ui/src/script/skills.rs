//! The skills-pane bindings (decision 0437 phase 4) — the Era-shaped `SkillFrame` surface driving
//! a faithful port of the real 1.12 Skills tab (extracted from `interface.MPQ`: FrameXML
//! `SkillFrame.{xml,lua}`). Unlike [`super::tradeskill`]'s deliberately FLAT v1 recipe list, this
//! pane needs the trainer's own GROUP/TREE machinery ([`super::trainer`], decision 0247): the app
//! pushes a flat, unordered snapshot ([`UiScript::set_skills`] — [`SkillsState::entries`], each
//! already resolved to name/category by the app from `SkillLine.dbc`/`SkillLineCategory.dbc`), and
//! the ENGINE groups by category, sorts, and folds — the trainer's synthesized-tree pattern, minus
//! the state filter (a skill line carries no green/red/gray category) and minus the wire skill-line
//! id doing double duty as both the row's identity and the group key (here the GROUP key is the
//! category, the row's identity is the skill id).
//!
//! ## The engine grouping law (INTERIM — the 0437 §5 dispatch resolved (decision 0446), but never
//! adjudicated this law; it stays unpinned, its own follow-up named in decision 0530)
//!
//! Groups ordered by `category_order` ascending (`category_id` breaks a tie, for determinism);
//! one header row per non-empty group (text = `category_name`); entries within a group sorted by
//! name ascending (the trainer's own [`collate`] — case-insensitive, raw-byte tie-break). Every
//! group starts EXPANDED (the trainer's own default-expanded rule). Visible rows are headers
//! always, plus the entries of expanded groups; every index the Lua API takes/returns is 1-based
//! into that visible list.
//!
//! ## The Era API shape (matched to the real `SkillFrame.lua`, transcribed onto this engine)
//!
//! `GetNumSkillLines()` → the visible row count. `GetSkillLineInfo(i)` → the ref's own 13-tuple
//! (`name, isHeader, isExpanded, skillRank, numTempPoints, skillModifier, skillMaxRank,
//! isAbandonable, stepCost, rankCost, minLevel, skillCostType, skillDescription`): a header row
//! shapes `(category_name, 1, expanded, 0, 0, 0, 0, nil, 0, 0, 0, nil, nil)`, an entry row shapes
//! `(name, nil, nil, value, 0, modifier, max, abandonable, 0, 0, 0, nil, description)`.
//! `numTempPoints` is always `0` (1.12 training points are dead data — `PLAYER_SKILL_INFO`
//! carries no temp/perm split the client would animate through a "buy a rank" flow);
//! `skillDescription` is REAL (`SkillLine.dbc` col 12 through the app feed — the detail pane's
//! body text); `isAbandonable` is REAL too ([`SkillEntry::abandonable`], the unlearn button's
//! gate — and `AbandonSkill(i)` is its outbound half, a VISIBLE index queued out by skill id for
//! the app's `CMSG_UNLEARN_SKILL`, [`UiScript::take_skill_abandons`]); the remaining
//! `stepCost`/`rankCost`/`minLevel`/`skillCostType` are vestigial stubs backing the ref's
//! *training-up* branches (a trainer-taught skill step), which never apply to a line that only
//! ever changes as a server descriptor delta.
//!
//! `ExpandSkillHeader(i)`/`CollapseSkillHeader(i)` take a header's VISIBLE index (`0` = all
//! groups, the trainer's own collapse-all shape). `SetSelectedSkill(i)`/`GetSelectedSkill()` are a
//! VISIBLE index too, but the engine holds the selection BY SKILL ID internally (the tradeskill's
//! own by-spell-id persistence pattern, [`super::tradeskill::UiScript::set_trade_skill`]) so it
//! survives a re-push that reorders or regroups; selecting a header (or an out-of-range index)
//! clears it. `GetAdjustedSkillPoints()` is a vestigial 1.12 leftover the ref reads; it always
//! returns `0` — there is no training-point economy behind a skill line in this client.
//!
//! The ref Lua's other globals (`SkillBar_OnClick`'s `RemoveSkillUp`/`AddSkillUp`/`BuySkillTier`,
//! `UnitCharacterPoints`) back the training-up machinery this pane doesn't ship (0437's named
//! out-of-scope) — none are transcribed, engine-side or XML-side, rather than stubbing dead call
//! sites nothing in [`SkillFrame.xml`] ever reaches. (The `UNLEARN_SKILL` popup, once in that
//! list, ships for real now — the abandon slice above.)

use std::collections::{HashMap, HashSet};

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// One known skill line off the player's `PLAYER_SKILL_INFO` block, app-resolved (0437 phase 4).
/// EXACT shape the app feed (`crates/benilla/src/ui_char.rs`) is written against — do not rename.
#[derive(Clone, Debug, PartialEq)]
pub struct SkillEntry {
    pub skill_id: u32,
    /// `SkillLine.dbc` name ("First Aid").
    pub name: String,
    /// Current rank.
    pub value: u32,
    /// Max rank (a 1.12 proficiency line can be `0/0` — render barless, the ref's own shape).
    pub max: u32,
    /// Temp+perm bonus (the green "+n"; negative possible).
    pub modifier: i32,
    /// `SkillLine.dbc` categoryId.
    pub category_id: u32,
    /// Resolved category name ("Professions") — the header text.
    pub category_name: String,
    /// `SkillLineCategory` displayOrder — the group sort key.
    pub category_order: u32,
    /// `SkillLine.dbc` description (enUS column 12) — `GetSkillLineInfo`'s 13th return, the
    /// detail pane's body text (empty when the row carries none; the XML's `SKILL_DESCRIPTION`
    /// format renders it verbatim).
    pub description: String,
    /// Whether the line can be unlearned — `GetSkillLineInfo`'s 8th return (`isAbandonable`), the
    /// detail pane's unlearn-button gate. App-resolved from `SkillRaceClassInfo.flags & 0x20`
    /// (`SKILL_FLAG_UNLEARNABLE` — the server's own `CMSG_UNLEARN_SKILL` gate, vmangos
    /// `SkillHandler.cpp`).
    pub abandonable: bool,
}

/// A flat, unordered push of every known skill line (0437 phase 4) — the ENGINE groups and sorts
/// (see the module doc's grouping law). EXACT shape the app feed is written against — do not
/// rename.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SkillsState {
    pub entries: Vec<SkillEntry>,
}

/// One category group in the synthesized display tree: the header's id/name/sort key and the
/// positions (into [`SkillsState::entries`]) of the category's lines, pre-sorted by name —
/// mirrors [`super::trainer::TrainerGroup`], but the group key is a *category*, never the wire
/// skill line itself. Engine-internal (never crosses the app seam, unlike [`SkillsState`]);
/// `pub(crate)` only because [`super::model::Model`] stores a `Vec` of them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SkillGroup {
    category_id: u32,
    name: String,
    order: u32,
    /// Positions into [`SkillsState::entries`], sorted by [`collate`]d name.
    entries: Vec<usize>,
}

impl super::UiScript {
    /// Replace the skills snapshot (0437 phase 4). Builds the display tree ([`build_groups`]) from
    /// the flat entries, prunes the collapsed set to categories that still exist in the new push
    /// (the trainer's own collapse-survives-an-update rule, [`super::trainer::UiScript::set_trainer`]),
    /// and re-anchors the selection to the SAME skill id if it's still present — else clears it
    /// (the tradeskill's own by-spell-id selection-persistence precedent).
    pub fn set_skills(&mut self, state: SkillsState) {
        let mut model = self.model_mut();
        let groups = build_groups(&state.entries);
        let live: HashSet<u32> = groups.iter().map(|g| g.category_id).collect();
        model.skills_collapsed.retain(|c| live.contains(c));
        if let Some(sid) = model.skills_selected {
            if !state.entries.iter().any(|e| e.skill_id == sid) {
                model.skills_selected = None;
            }
        }
        model.skills_groups = groups;
        model.skills = state;
    }

    /// Drain the skill line ids `AbandonSkill` queued (the unlearn seam) — the app sends each as
    /// one `CMSG_UNLEARN_SKILL` and otherwise does nothing: the removal arrives back as a server
    /// skill-field update, never a local mutation ([`Model::skill_abandons`]).
    pub fn take_skill_abandons(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().skill_abandons)
    }
}

/// One visible row of the display tree: a category **header** (carrying its group index) or an
/// **entry** (carrying its position into [`SkillsState::entries`]).
#[derive(Clone, Copy)]
enum Row {
    Header(usize),
    Entry(usize),
}

/// The WoW enUS collator, approximated (the trainer's own [`super::trainer`] helper, duplicated
/// here rather than shared — each seam module keeps its own copy, the established local
/// convention): case-insensitive alphabetical, raw bytes as a stable tie-break.
fn collate(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// Build the display tree from the flat entries (the module doc's grouping law): group by
/// category, sort each group's entries by name, sort the groups by `category_order` (category id
/// breaks a tie). Entry positions index back into the unchanged `entries` slice.
fn build_groups(entries: &[SkillEntry]) -> Vec<SkillGroup> {
    let mut map: HashMap<u32, SkillGroup> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        map.entry(e.category_id)
            .or_insert_with(|| SkillGroup {
                category_id: e.category_id,
                name: e.category_name.clone(),
                order: e.category_order,
                entries: Vec::new(),
            })
            .entries
            .push(i);
    }
    let mut groups: Vec<SkillGroup> = map.into_values().collect();
    for g in &mut groups {
        g.entries
            .sort_by(|&a, &b| collate(&entries[a].name, &entries[b].name));
    }
    groups.sort_by(|a, b| {
        a.order
            .cmp(&b.order)
            .then(a.category_id.cmp(&b.category_id))
    });
    groups
}

/// The visible rows in display order: each group's header (always shown), then — when the group
/// isn't collapsed — its entries. The Lua's 1-based `index` is a position in *this* list.
fn rows(model: &Model) -> Vec<Row> {
    let mut out = Vec::new();
    for (gi, g) in model.skills_groups.iter().enumerate() {
        out.push(Row::Header(gi));
        if !model.skills_collapsed.contains(&g.category_id) {
            for &ei in &g.entries {
                out.push(Row::Entry(ei));
            }
        }
    }
    out
}

/// The count of visible rows (headers + the entries of expanded groups).
fn num_rows(model: &Model) -> usize {
    rows(model).len()
}

/// The entry at a 1-based VISIBLE index, or `None` when that row is a header (or OOB) — so the
/// selection/info getters that read an entry safely no-op on a header row.
fn entry_at(model: &Model, index: usize) -> Option<&SkillEntry> {
    let n = index.checked_sub(1)?;
    match rows(model).get(n)? {
        Row::Entry(ei) => model.skills.entries.get(*ei),
        Row::Header(_) => None,
    }
}

/// Collapse (`collapse = true`) or expand a category by the **display index of its header row**
/// (the trainer's own `Collapse/ExpandTrainerSkillLine` shape). `id == 0` targets ALL groups (the
/// collapse-all button); `id > 0` resolves the header at that visible index to its category. A
/// non-header (or OOB) index is a no-op.
fn set_collapsed(model: &mut Model, id: usize, collapse: bool) {
    if id == 0 && !collapse {
        model.skills_collapsed.clear();
        return;
    }
    let targets: Vec<u32> = if id == 0 {
        model.skills_groups.iter().map(|g| g.category_id).collect()
    } else {
        match rows(model).get(id - 1) {
            Some(Row::Header(gi)) => model
                .skills_groups
                .get(*gi)
                .map(|g| g.category_id)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    };
    for c in targets {
        if collapse {
            model.skills_collapsed.insert(c);
        } else {
            model.skills_collapsed.remove(&c);
        }
    }
}

/// `SetSelectedSkill(index)` — resolve the 1-based VISIBLE index to a skill id and hold THAT (the
/// module doc's by-id persistence); a header row or an out-of-range index clears the selection.
fn set_selected(model: &mut Model, index: u32) {
    model.skills_selected = entry_at(model, index as usize).map(|e| e.skill_id);
}

/// `GetSelectedSkill()` — the selection's CURRENT visible index (`0` when nothing is selected, or
/// the selected id isn't visible right now, e.g. its group just got collapsed).
fn selected_index(model: &Model) -> u32 {
    let Some(sid) = model.skills_selected else {
        return 0;
    };
    rows(model)
        .iter()
        .position(|r| matches!(r, Row::Entry(ei) if model.skills.entries[*ei].skill_id == sid))
        .map_or(0, |p| (p + 1) as u32)
}

/// A `bool` as the Era `1`/`nil` shape (the trainer/tradeskill's own helper, duplicated per the
/// established per-module convention).
fn era_bool(b: bool) -> Value {
    if b {
        Value::Integer(1)
    } else {
        Value::Nil
    }
}

/// Register the skills-pane globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetNumSkillLines() → the visible row count (0 before any push).
    g.set(
        "GetNumSkillLines",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(num_rows(&model) as i64)
        })?,
    )?;

    // GetSkillLineInfo(index) → the ref's own 13-tuple (module doc). `index` 1-based into the
    // visible tree; out of range → a single nil.
    g.set(
        "GetSkillLineInfo",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(n) = index.checked_sub(1) else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let Some(row) = rows(&model).get(n).copied() else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            match row {
                Row::Header(gi) => {
                    let grp = &model.skills_groups[gi];
                    let expanded = !model.skills_collapsed.contains(&grp.category_id);
                    Ok(MultiValue::from_vec(vec![
                        Value::String(lua.create_string(&grp.name)?),
                        Value::Integer(1), // isHeader
                        era_bool(expanded),
                        Value::Integer(0), // skillRank
                        Value::Integer(0), // numTempPoints — always 0 (module doc)
                        Value::Integer(0), // skillModifier
                        Value::Integer(0), // skillMaxRank
                        Value::Nil,        // isAbandonable
                        Value::Integer(0), // stepCost
                        Value::Integer(0), // rankCost
                        Value::Integer(0), // minLevel
                        Value::Nil,        // skillCostType
                        Value::Nil,        // skillDescription
                    ]))
                }
                Row::Entry(ei) => {
                    let e = &model.skills.entries[ei];
                    Ok(MultiValue::from_vec(vec![
                        Value::String(lua.create_string(&e.name)?),
                        Value::Nil, // isHeader
                        Value::Nil, // isExpanded
                        Value::Integer(i64::from(e.value)),
                        Value::Integer(0), // numTempPoints — always 0 (module doc)
                        Value::Integer(i64::from(e.modifier)),
                        Value::Integer(i64::from(e.max)),
                        era_bool(e.abandonable), // isAbandonable
                        Value::Integer(0),       // stepCost
                        Value::Integer(0),       // rankCost
                        Value::Integer(0),       // minLevel
                        Value::Nil,              // skillCostType
                        Value::String(lua.create_string(&e.description)?), // skillDescription
                    ]))
                }
            }
        })?,
    )?;

    // Collapse/ExpandSkillHeader(id) — fold a category by the display index of its header row (id
    // 0 = all groups); a non-header (or OOB) index no-ops.
    g.set(
        "CollapseSkillHeader",
        lua.create_function(|lua, id: usize| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            set_collapsed(&mut model, id, true);
            Ok(())
        })?,
    )?;
    g.set(
        "ExpandSkillHeader",
        lua.create_function(|lua, id: usize| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            set_collapsed(&mut model, id, false);
            Ok(())
        })?,
    )?;

    // SetSelectedSkill(index) / GetSelectedSkill() — the engine-held selection, VISIBLE index in,
    // VISIBLE index out, held BY SKILL ID internally (module doc).
    g.set(
        "SetSelectedSkill",
        lua.create_function(|lua, index: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            set_selected(&mut model, index);
            Ok(())
        })?,
    )?;
    g.set(
        "GetSelectedSkill",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(selected_index(&model)))
        })?,
    )?;

    // GetAdjustedSkillPoints() → 0 — a vestigial 1.12 leftover the ref reads (module doc); this
    // client has no training-point economy behind a skill line.
    g.set(
        "GetAdjustedSkillPoints",
        lua.create_function(|_, ()| Ok(0i64))?,
    )?;

    // AbandonSkill(index) — VISIBLE index in (the ref's UNLEARN_SKILL popup passes the row it was
    // opened for), queued out BY SKILL ID for the app's CMSG_UNLEARN_SKILL send. Mutates NOTHING
    // locally: the real client waits for the server's skill-field update (vmangos SetSkill(id,0,0)
    // → our descriptor watcher → a fresh set_skills push → SKILL_LINES_CHANGED). A header or
    // out-of-range index no-ops (only entries carry the unlearn button).
    g.set(
        "AbandonSkill",
        lua.create_function(|lua, index: usize| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some(id) = entry_at(&model, index).map(|e| e.skill_id) {
                model.skill_abandons.push(id);
            }
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::UiScript;

    #[allow(clippy::too_many_arguments)]
    fn entry(
        skill_id: u32,
        name: &str,
        value: u32,
        max: u32,
        modifier: i32,
        category_id: u32,
        category_name: &str,
        category_order: u32,
    ) -> SkillEntry {
        SkillEntry {
            skill_id,
            name: name.into(),
            value,
            max,
            modifier,
            category_id,
            category_name: category_name.into(),
            category_order,
            description: format!("About {name}."),
            // Fixture rule: the Professions category (id 2) is abandonable, weapons are not —
            // the real SkillRaceClassInfo 0x20 split's shape.
            abandonable: category_id == 2,
        }
    }

    /// Two categories: "Weapon Skills" (order 1: Defense, Swords) and "Professions" (order 2:
    /// First Aid, Fishing) — a flat, unordered push (Swords precedes Defense in push order; the
    /// engine's own name sort must still show Defense first).
    fn state() -> SkillsState {
        SkillsState {
            entries: vec![
                entry(43, "Swords", 200, 300, 0, 1, "Weapon Skills", 1),
                entry(95, "Defense", 180, 300, 5, 1, "Weapon Skills", 1),
                entry(129, "First Aid", 57, 75, 3, 2, "Professions", 2),
                entry(356, "Fishing", 1, 300, 0, 2, "Professions", 2),
            ],
        }
    }

    /// Read `(name, type)` at a visible index, `type` = "header"/"entry".
    fn row_kind(s: &mut UiScript, i: i64) -> (String, String) {
        s.eval::<(String, String)>(&format!(
            "local n,h = GetSkillLineInfo({i}) local t = h and 'header' or 'entry' return n,t"
        ))
        .unwrap()
    }

    #[test]
    fn grouped_visible_rows_interleave_headers_ordered_by_category() {
        let mut s = UiScript::new().unwrap();
        assert_eq!(s.eval::<i64>("return GetNumSkillLines()").unwrap(), 0);

        s.set_skills(state());
        // 2 headers + 4 entries = 6 visible rows, category_order ascending, name ascending within.
        assert_eq!(s.eval::<i64>("return GetNumSkillLines()").unwrap(), 6);
        assert_eq!(
            row_kind(&mut s, 1),
            ("Weapon Skills".into(), "header".into())
        );
        assert_eq!(row_kind(&mut s, 2), ("Defense".into(), "entry".into()));
        assert_eq!(row_kind(&mut s, 3), ("Swords".into(), "entry".into()));
        assert_eq!(row_kind(&mut s, 4), ("Professions".into(), "header".into()));
        assert_eq!(row_kind(&mut s, 5), ("First Aid".into(), "entry".into()));
        assert_eq!(row_kind(&mut s, 6), ("Fishing".into(), "entry".into()));

        // Every group starts EXPANDED (the module doc's default rule).
        let (_, h1, e1) = s
            .eval::<(String, i64, Option<i64>)>("local n,h,e = GetSkillLineInfo(1) return n,h,e")
            .unwrap();
        assert_eq!((h1, e1), (1, Some(1)));
    }

    #[test]
    fn abandon_skill_queues_the_entrys_skill_id_and_mutates_nothing() {
        let mut s = UiScript::new().unwrap();
        s.set_skills(state());
        // The 8th return: Professions rows are abandonable (fixture rule), weapon rows and
        // headers are not — 1/nil, the 1.12 boolean shape.
        let ab = |s: &mut UiScript, i: i64| {
            s.eval::<Option<i64>>(&format!("return (select(8, GetSkillLineInfo({i})))"))
                .unwrap()
        };
        assert_eq!(ab(&mut s, 2), None, "Defense is not abandonable");
        assert_eq!(ab(&mut s, 5), Some(1), "First Aid is abandonable");
        assert_eq!(ab(&mut s, 1), None, "a header never is");

        // AbandonSkill queues BY SKILL ID; headers and out-of-range indices no-op; the list
        // itself is untouched (the server round trip owns the removal).
        s.run("AbandonSkill(5)").unwrap();
        s.run("AbandonSkill(1)").unwrap();
        s.run("AbandonSkill(99)").unwrap();
        assert_eq!(s.take_skill_abandons(), vec![129]);
        assert!(
            s.take_skill_abandons().is_empty(),
            "drain empties the queue"
        );
        assert_eq!(
            s.eval::<i64>("return GetNumSkillLines()").unwrap(),
            6,
            "no local removal — the visible tree is unchanged"
        );
    }

    #[test]
    fn collapse_hides_a_groups_entries_and_remaps_indices() {
        let mut s = UiScript::new().unwrap();
        s.set_skills(state());

        // Fold "Weapon Skills" (header at visible index 1): its two entries vanish, the header
        // stays and now reports isExpanded=nil. 6 → 4.
        s.run("CollapseSkillHeader(1)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumSkillLines()").unwrap(), 4);
        let (name, _, expanded) = s
            .eval::<(String, i64, Option<i64>)>("local n,h,e = GetSkillLineInfo(1) return n,h,e")
            .unwrap();
        assert_eq!((name.as_str(), expanded), ("Weapon Skills", None));
        assert_eq!(
            row_kind(&mut s, 2),
            ("Professions".into(), "header".into()),
            "Weapon Skills' entries are folded; Professions is now row 2"
        );

        // Expand it back.
        s.run("ExpandSkillHeader(1)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumSkillLines()").unwrap(), 6);

        // Collapse-all (id 0), then expand-all (id 0).
        s.run("CollapseSkillHeader(0)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumSkillLines()").unwrap(), 2);
        s.run("ExpandSkillHeader(0)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumSkillLines()").unwrap(), 6);
    }

    #[test]
    fn selection_and_expansion_persist_across_a_repush() {
        let mut s = UiScript::new().unwrap();
        s.set_skills(state());

        // Selecting a HEADER clears the selection (module doc).
        s.run("SetSelectedSkill(1)").unwrap();
        assert_eq!(s.eval::<i64>("return GetSelectedSkill()").unwrap(), 0);

        // Select Swords (row 3), fold Professions (row 4).
        s.run("SetSelectedSkill(3)").unwrap();
        assert_eq!(s.eval::<i64>("return GetSelectedSkill()").unwrap(), 3);
        s.run("CollapseSkillHeader(4)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumSkillLines()").unwrap(), 4);

        // A re-push (values ticked up) keeps BOTH the fold and the selection's skill identity —
        // Swords is still row 3, Professions is still collapsed.
        let mut ticked = state();
        ticked.entries[0].value = 201; // Swords
        s.set_skills(ticked);
        assert_eq!(s.eval::<i64>("return GetNumSkillLines()").unwrap(), 4);
        assert_eq!(s.eval::<i64>("return GetSelectedSkill()").unwrap(), 3);
        let (name, rank) = s
            .eval::<(String, i64)>("local n,_,_,r = GetSkillLineInfo(3) return n,r")
            .unwrap();
        assert_eq!((name.as_str(), rank), ("Swords", 201));

        // A re-push that drops the selected skill entirely clears the selection.
        let mut without_swords = state();
        without_swords.entries.remove(0);
        s.set_skills(without_swords);
        assert_eq!(s.eval::<i64>("return GetSelectedSkill()").unwrap(), 0);
    }

    #[test]
    fn header_and_entry_tuple_shapes() {
        let mut s = UiScript::new().unwrap();
        s.set_skills(state());

        // Header row 1 ("Weapon Skills"): (name, 1, expanded, 0, 0, 0, 0, nil, 0, 0, 0, nil, nil).
        let (name, is_header, is_expanded, rank, temp, modifier, max) = s
            .eval::<(String, i64, Option<i64>, i64, i64, i64, i64)>(
                "local n,h,e,r,t,m,mx = GetSkillLineInfo(1) return n,h,e,r,t,m,mx",
            )
            .unwrap();
        assert_eq!(
            (
                name.as_str(),
                is_header,
                is_expanded,
                rank,
                temp,
                modifier,
                max
            ),
            ("Weapon Skills", 1, Some(1), 0, 0, 0, 0)
        );
        let (abandon_nil, step, rank_cost, min_level, cost_type_nil, desc_nil) = s
            .eval::<(bool, i64, i64, i64, bool, bool)>(
                "local _,_,_,_,_,_,_,a,st,rc,ml,ct,d = GetSkillLineInfo(1) \
                 return a==nil, st, rc, ml, ct==nil, d==nil",
            )
            .unwrap();
        assert!(abandon_nil);
        assert_eq!((step, rank_cost, min_level), (0, 0, 0));
        assert!(cost_type_nil);
        assert!(desc_nil);

        // Entry row 2 ("Defense", value 180, max 300, modifier +5): (name, nil, nil, 180, 0, 5,
        // 300, nil, 0, 0, 0, nil, nil).
        let (name, is_header, is_expanded, rank, temp, modifier, max) = s
            .eval::<(String, Option<i64>, Option<i64>, i64, i64, i64, i64)>(
                "local n,h,e,r,t,m,mx = GetSkillLineInfo(2) return n,h,e,r,t,m,mx",
            )
            .unwrap();
        assert_eq!(
            (
                name.as_str(),
                is_header,
                is_expanded,
                rank,
                temp,
                modifier,
                max
            ),
            ("Defense", None, None, 180, 0, 5, 300)
        );
        // The 13th return is the REAL description now (SkillLine.dbc col 12 through the feed) —
        // a string on an entry row, still nil on a header (asserted above).
        assert_eq!(
            s.eval::<String>("return select(13, GetSkillLineInfo(2))")
                .unwrap(),
            "About Defense."
        );
    }

    #[test]
    fn no_push_reports_zero_rows() {
        let s = UiScript::new().unwrap();
        assert_eq!(s.eval::<i64>("return GetNumSkillLines()").unwrap(), 0);
        assert!(s.eval::<bool>("return GetSkillLineInfo(1) == nil").unwrap());
        assert_eq!(s.eval::<i64>("return GetSelectedSkill()").unwrap(), 0);
        assert_eq!(s.eval::<i64>("return GetAdjustedSkillPoints()").unwrap(), 0);
        // Collapse/expand/select on an empty pane are harmless no-ops.
        s.run("CollapseSkillHeader(0) ExpandSkillHeader(1) SetSelectedSkill(1)")
            .unwrap();
        assert_eq!(s.eval::<i64>("return GetNumSkillLines()").unwrap(), 0);
    }
}
