//! The Lua globals (`GetTradeSkillInfo`, `DoTradeSkill`, the filter family, …) — [`install`] wires
//! the Era `TradeSkillFrame.lua` API surface onto [`super::view`]'s display-tree/filter machinery.

use mlua::{Lua, MultiValue, Value};

use crate::script::Model;

use super::view::{
    build_groups, first_recipe_index, inv_slot_name, num_rows, present_inv_slots, recipe_at, rows,
    select, selected_visible_index, set_collapsed, Row,
};

/// An optional string as a Lua value (`nil` when absent).
fn opt_str(lua: &Lua, s: Option<&String>) -> mlua::Result<Value> {
    Ok(match s {
        Some(s) => Value::String(lua.create_string(s)?),
        None => Value::Nil,
    })
}

/// A `bool` as the Era `1`/`nil` shape.
fn era_bool(b: bool) -> Value {
    if b {
        Value::Integer(1)
    } else {
        Value::Nil
    }
}

/// Register the tradeskill globals.
pub(in crate::script) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetTradeSkillLine() → lineName, rank, maxRank; ("UNKNOWN", 0, 0) with no window open (the
    // ref's own no-tradeskill shape).
    g.set(
        "GetTradeSkillLine",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let (name, rank, max_rank) = match &model.trade_skill {
                Some(t) => (t.line_name.clone(), t.rank, t.max_rank),
                None => ("UNKNOWN".to_string(), 0, 0),
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&name)?),
                Value::Integer(i64::from(rank)),
                Value::Integer(i64::from(max_rank)),
            ]))
        })?,
    )?;

    // → the number of VISIBLE rows the open window offers — headers + the recipes of uncollapsed
    // groups (0 when closed).
    g.set(
        "GetNumTradeSkills",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(num_rows(&model) as i64)
        })?,
    )?;

    // GetTradeSkillInfo(index) → name, type, numAvailable, isExpanded. `index` 1-based into the
    // VISIBLE row list (the module doc's grouped-list law, wow-re `tradeskill` TU-B). A header row:
    // (groupName, "header", 0, isExpanded 1/nil). A recipe row: (name, difficulty color key,
    // numAvailable, nil). Out of range → a single nil.
    g.set(
        "GetTradeSkillInfo",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(n) = index.checked_sub(1) else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let Some(row) = rows(&model).get(n).cloned() else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            match row {
                Row::Header { key, name } => {
                    let expanded = !model.trade_skill_collapsed.contains(&key);
                    Ok(MultiValue::from_vec(vec![
                        Value::String(lua.create_string(&name)?),
                        Value::String(lua.create_string("header")?),
                        Value::Integer(0),
                        era_bool(expanded),
                    ]))
                }
                Row::Entry(ei) => {
                    let r = &model
                        .trade_skill
                        .as_ref()
                        .expect("rows() only yields Entry rows when trade_skill is Some")
                        .recipes[ei];
                    Ok(MultiValue::from_vec(vec![
                        Value::String(lua.create_string(&r.name)?),
                        Value::String(lua.create_string(r.difficulty.as_str())?),
                        Value::Integer(i64::from(r.num_available)),
                        Value::Nil,
                    ]))
                }
            }
        })?,
    )?;

    // GetFirstTradeSkill() → the first NON-header visible index (0 when none).
    g.set(
        "GetFirstTradeSkill",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(first_recipe_index(&model)))
        })?,
    )?;

    // GetTradeSkillSubClasses() → the current group names, in group order (VERIFIED, wow-re
    // `tradeskill` TU-B: the filter-dropdown vocabulary IS the header list) — though v1 ships no
    // filter dropdown to consume it yet (the SubClass/InvSlot filter family below stays inert).
    g.set(
        "GetTradeSkillSubClasses",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let mut out = Vec::new();
            if let Some(t) = model.trade_skill.as_ref() {
                for grp in build_groups(&t.recipes) {
                    out.push(Value::String(lua.create_string(&grp.name)?));
                }
            }
            Ok(MultiValue::from_vec(out))
        })?,
    )?;

    // ExpandTradeSkillSubClass(i) / CollapseTradeSkillSubClass(i) — fold a group by its header's
    // VISIBLE index (i == 0 = ALL groups, the CollapseAll semantics — see [`set_collapsed`]).
    g.set(
        "ExpandTradeSkillSubClass",
        lua.create_function(|lua, id: usize| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            set_collapsed(&mut model, id, false);
            model.trade_skill_touched = true;
            Ok(())
        })?,
    )?;
    g.set(
        "CollapseTradeSkillSubClass",
        lua.create_function(|lua, id: usize| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            set_collapsed(&mut model, id, true);
            model.trade_skill_touched = true;
            Ok(())
        })?,
    )?;

    // GetTradeSkillIcon(index) → icon texture path (nil while in flight / OOB / a header row).
    g.set(
        "GetTradeSkillIcon",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            opt_str(lua, recipe_at(&model, index).and_then(|r| r.icon.as_ref()))
        })?,
    )?;

    // GetTradeSkillNumMade(index) → minMade, maxMade (0, 0 when OOB / no window / a header row).
    g.set(
        "GetTradeSkillNumMade",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let (min_made, max_made) =
                recipe_at(&model, index).map_or((0, 0), |r| (r.min_made, r.max_made));
            Ok(MultiValue::from_vec(vec![
                Value::Integer(i64::from(min_made)),
                Value::Integer(i64::from(max_made)),
            ]))
        })?,
    )?;

    // GetTradeSkillCooldown(index) → remaining cooldown seconds, or nil when ready / OOB / a header
    // row (the ref Lua tests this return for truthiness).
    g.set(
        "GetTradeSkillCooldown",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(
                match recipe_at(&model, index).and_then(|r| r.cooldown_secs) {
                    Some(secs) => Value::Integer(secs as i64),
                    None => Value::Nil,
                },
            )
        })?,
    )?;

    // GetTradeSkillNumReagents(index) → this recipe's reagent count (0 when OOB / no window / a
    // header row).
    g.set(
        "GetTradeSkillNumReagents",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(recipe_at(&model, index).map_or(0, |r| r.reagents.len()) as i64)
        })?,
    )?;

    // GetTradeSkillReagentInfo(index, reagentIndex) → name, icon, need, have (a single nil when the
    // recipe/reagent index is OOB or `index` is a header row). name/icon are themselves nil while
    // the ask-once template answer is in flight — the ref grays/counts the row off exactly these
    // four.
    g.set(
        "GetTradeSkillReagentInfo",
        lua.create_function(|lua, (index, reagent_index): (usize, usize)| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(reagent) = recipe_at(&model, index)
                .and_then(|r| reagent_index.checked_sub(1).and_then(|n| r.reagents.get(n)))
            else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            Ok(MultiValue::from_vec(vec![
                opt_str(lua, reagent.name.as_ref())?,
                opt_str(lua, reagent.icon.as_ref())?,
                Value::Integer(i64::from(reagent.need)),
                Value::Integer(i64::from(reagent.have)),
            ]))
        })?,
    )?;

    // GetTradeSkillTools(index) → an alternating (name, has) multivalue, one pair per Requirements
    // tool (e.g. "Anvil", 1, "Mining Pick", nil) — the ref feeds this straight into
    // BuildColoredListString. Empty when the recipe has no tools / index is OOB / a header row.
    g.set(
        "GetTradeSkillTools",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let mut out = Vec::new();
            if let Some(r) = recipe_at(&model, index) {
                for (name, have) in &r.tools {
                    out.push(Value::String(lua.create_string(name)?));
                    out.push(era_bool(*have));
                }
            }
            Ok(MultiValue::from_vec(out))
        })?,
    )?;

    // GetTradeskillRepeatCount() → the remaining Create All repeats (note the lowercase "s" in
    // "Tradeskill" — that IS the real 1.12 API name). 0 with no window open.
    g.set(
        "GetTradeskillRepeatCount",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(
                model.trade_skill.as_ref().map_or(0, |t| t.repeat_count),
            ))
        })?,
    )?;

    // SelectTradeSkill(index) / GetTradeSkillSelectionIndex() — the engine-held selection, VISIBLE
    // index in ([`select`]: a header index is IGNORED, not cleared), VISIBLE index out
    // ([`selected_visible_index`]), held internally as a stable flat-recipe position (module doc).
    g.set(
        "SelectTradeSkill",
        lua.create_function(|lua, index: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            select(&mut model, index);
            Ok(())
        })?,
    )?;
    g.set(
        "GetTradeSkillSelectionIndex",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(i64::from(selected_visible_index(&model)))
        })?,
    )?;

    // DoTradeSkill(index, count) — queue the recipe's SPELL ID for `count` crafts (default 1, never
    // less than 1) — the app's repeat machine turns this into that many CMSG_CAST_SPELL sends. Out
    // of range / a header index → ignored.
    g.set(
        "DoTradeSkill",
        lua.create_function(|lua, (index, count): (usize, Option<i64>)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some((spell_id, avail)) =
                recipe_at(&model, index).map(|r| (r.spell_id, r.num_available))
            {
                // The client clamps the repeat to numAvailable at the latch (byte-VERIFIED —
                // wow-re `tradeskill` TU-D, `DoTradeSkill 0x500280`).
                let n = (count.unwrap_or(1).max(1) as u32).min(avail.max(1));
                model.trade_skill_dos.push((spell_id, n));
            }
            Ok(())
        })?,
    )?;

    // CloseTradeSkill() — client-side close (no packet, vanilla): flag it so the app clears its
    // local state.
    g.set(
        "CloseTradeSkill",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.trade_skill_close = true;
            Ok(())
        })?,
    )?;

    // The sub-class/inv-slot FILTER family — real now (the dropdowns ship, ref
    // Blizzard_TradeSkillUI.lua l.314-414). Indexing follows the C originals: the SubClass index
    // is 1-based into the CURRENT GetTradeSkillSubClasses order (`0x4ffc70` bounds-checks against
    // the live header count), the InvSlot index 1-based into the GetTradeSkillInvSlots order;
    // index 0 is the "All" pseudo-entry on both. `Set*(0, 1, …)` = everything back on (the ref's
    // "All Subclasses"/"All Slots" menu row); `Set*(i, 1, 1)` = EXCLUSIVE — only entry i shown
    // (every ref menu click passes exactly this shape); `Set*(i, 1)` un-hides i; `Set*(i, 0)`
    // hides it. Each set re-lists + fires TRADE_SKILL_UPDATE via the touched flag (the `0x4fd710`
    // re-sort + event 0x139 shape). `Get*(0)` answers "is everything shown" — the ref's
    // all-checked probe.
    g.set(
        "GetTradeSkillSubClassFilter",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(t) = model.trade_skill.as_ref() else {
                return Ok(era_bool(false));
            };
            let groups = build_groups(&t.recipes);
            Ok(match index.checked_sub(1) {
                None => era_bool(
                    !groups
                        .iter()
                        .any(|g| model.trade_skill_subclass_hidden.contains(&g.key)),
                ),
                Some(n) => era_bool(
                    groups
                        .get(n)
                        .is_some_and(|g| !model.trade_skill_subclass_hidden.contains(&g.key)),
                ),
            })
        })?,
    )?;
    g.set(
        "SetTradeSkillSubClassFilter",
        lua.create_function(
            |lua, (index, on, exclusive): (usize, Option<i64>, Option<i64>)| {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                let Some(t) = model.trade_skill.as_ref() else {
                    return Ok(());
                };
                let keys: Vec<(u32, u32)> = build_groups(&t.recipes)
                    .into_iter()
                    .map(|g| g.key)
                    .collect();
                let on = on.unwrap_or(1) != 0;
                let exclusive = exclusive.unwrap_or(0) != 0;
                match index.checked_sub(1) {
                    None => model.trade_skill_subclass_hidden.clear(),
                    Some(n) => {
                        let Some(&key) = keys.get(n) else {
                            return Ok(());
                        };
                        if on && exclusive {
                            model.trade_skill_subclass_hidden =
                                keys.iter().copied().filter(|&k| k != key).collect();
                        } else if on {
                            model.trade_skill_subclass_hidden.remove(&key);
                        } else {
                            model.trade_skill_subclass_hidden.insert(key);
                        }
                    }
                }
                model.trade_skill_touched = true;
                Ok(())
            },
        )?,
    )?;
    // GetTradeSkillInvSlots() → the distinct slot words of the open window's products, ascending
    // slot-bit order (the `0xbde058` bit walk — [`present_inv_slots`]).
    g.set(
        "GetTradeSkillInvSlots",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let mut out = Vec::new();
            for bit in present_inv_slots(&model) {
                out.push(Value::String(lua.create_string(inv_slot_name(bit))?));
            }
            Ok(MultiValue::from_vec(out))
        })?,
    )?;
    // The InvSlot pair reads/writes the shown-mask (`0x84dd64`) with the index resolving to the
    // (index-1)-th SET bit of the accumulated present-slots mask — present slots only, ascending
    // (TU-G §4's enumeration; `GetTradeSkillInvSlots` returns exactly that order).
    g.set(
        "GetTradeSkillInvSlotFilter",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            if model.trade_skill.is_none() {
                return Ok(era_bool(false));
            }
            let bits = present_inv_slots(&model);
            Ok(match index.checked_sub(1) {
                // The "all shown?" probe: (present & mask) == present (TU-G §4, `0x4fffd0`).
                None => era_bool(
                    bits.iter()
                        .all(|&b| model.trade_skill_invslot_mask & (1 << b) != 0),
                ),
                Some(n) => era_bool(
                    bits.get(n)
                        .is_some_and(|&b| model.trade_skill_invslot_mask & (1 << b) != 0),
                ),
            })
        })?,
    )?;
    g.set(
        "SetTradeSkillInvSlotFilter",
        lua.create_function(
            |lua, (index, on, exclusive): (usize, Option<i64>, Option<i64>)| {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                if model.trade_skill.is_none() {
                    return Ok(());
                }
                let bits = present_inv_slots(&model);
                let on = on.unwrap_or(1) != 0;
                let exclusive = exclusive.unwrap_or(0) != 0;
                // The exact mask math per path (TU-G §4, the recovered `0x4fd730` args): all →
                // 0xffffffff · off → old & ~(1<<b) · exclusive → 1<<b · add → old | (1<<b).
                match index.checked_sub(1) {
                    None => model.trade_skill_invslot_mask = u32::MAX,
                    Some(n) => {
                        let Some(&bit) = bits.get(n) else {
                            return Ok(());
                        };
                        if on && exclusive {
                            model.trade_skill_invslot_mask = 1 << bit;
                        } else if on {
                            model.trade_skill_invslot_mask |= 1 << bit;
                        } else {
                            model.trade_skill_invslot_mask &= !(1 << bit);
                        }
                    }
                }
                model.trade_skill_touched = true;
                Ok(())
            },
        )?,
    )?;

    Ok(())
}
