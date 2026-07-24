//! The craft bindings (decision 0437 phase 3) — the Era-shaped Enchanting-window surface driving a
//! faithful port of the real 1.12 `CraftFrame` (extracted from `interface.MPQ`:
//! `Interface\FrameXML\Blizzard_CraftUI\CraftFrame.{xml,lua}`). The exact twin of [`super::tradeskill`]
//! (same two-way seam, same "no wire" law, same FLAT-list INTERIM): the app pushes a **craft
//! snapshot** ([`UiScript::set_craft`] — the recipes already resolved to name/subname/icon/
//! description/reagents/tools/color by the app from `Spell.dbc`/`SkillLineAbility.dbc`/bag counts),
//! and the Lua `DoCraft`/`CloseCraft` calls queue outbound **intents** the app drains
//! ([`UiScript::take_craft_dos`] returns spell ids / [`UiScript::take_craft_close`]). The engine holds
//! no recipe knowledge — a recipe is name/subname/icon/description/difficulty/reagents/tools, all
//! app-resolved (0437 §"The decision" item 1). `CraftRecipe` reuses [`super::TradeSkillDifficulty`]
//! outright (0437's own framing: "one solved pattern") rather than a second color enum — the Era
//! `CraftTypeColor`/`TradeSkillTypeColor` tables are byte-identical (ref `Blizzard_CraftUI.lua:6-13` vs
//! `Blizzard_TradeSkillUI.lua`'s own table), so the four color keys really are one vocabulary.
//!
//! ## Where Craft actually differs from TradeSkill (verified against the ref, not assumed)
//!
//! - **No Create All.** The 1.12 `CraftFrame` ships one action button (`DoCraft(GetCraftSelectionIndex())`,
//!   ref `Blizzard_CraftUI.xml:667`) — no repeat-count EditBox, no `GetCraftskillRepeatCount`. `DoCraft`
//!   therefore queues a bare spell id, never a `(spell id, count)` pair — [`UiScript::take_craft_dos`]
//!   returns `Vec<u32>`.
//! - **`GetCraftInfo` is a 7-tuple, not TradeSkill's 4-tuple**: `craftName, craftSubSpellName, craftType,
//!   numAvailable, isExpanded, trainingPointCost, requiredLevel` (ref l.37/168/289) — the trailing three
//!   are Beast Training's own fields (a pet ability's training-point cost + the pet's required level).
//!   Beast Training is out of scope (0437's own line — the pet arc owns it), so v1 hardcodes
//!   `trainingPointCost = 0` and `requiredLevel = 0` — never nonzero, so every ref branch gated on them
//!   (`Craft_UpdateTrainingPoints`, the per-row Cost text, the red/green pet-level Requirements line) is
//!   unreachable and dropped from the ported XML, not merely stubbed (this file's job is the tuple
//!   shape; the XML's job is not painting dead branches).
//! - **A recipe carries a `sub_name`** (`craftSubSpellName`) TradeSkill's own recipes never have — the
//!   ref's row paints it in parens beside the name (`format(TEXT(PARENS_TEMPLATE), craftSubSpellName)`,
//!   ref l.218) when non-empty. Enchanting itself rarely uses it (most enchant spells have no sub-rank
//!   text), but the field is real in the ref's own tuple, so it ships in v1 rather than being folded away.
//! - **A recipe carries a pre-resolved `description`** (`GetCraftDescription`, ref l.312) TradeSkill's
//!   own recipe never exposes to Lua at all — Enchanting's detail pane shows the spell's flavor/effect
//!   text above the reagent grid (ref `CraftDescription`), a region TradeSkill's own detail pane has no
//!   analogue for.
//! - **No product-item concept.** TradeSkill's `product_item`/`min_made`/`max_made`/`cooldown_secs`
//!   have no Craft equivalent: an enchant recipe doesn't "create" a bag item the way a tradeskill recipe
//!   does (it modifies an item in place, or resolves as a pure effect) — `CraftIcon`'s hover shows the
//!   **spell**, not a product item (`GameTooltip:SetCraftSpell`, ref l.566, vs TradeSkill's
//!   `SetTradeSkillItem` with no reagent index) — see [`super::tooltip_item`]'s `SetCraftSpell`.
//! - **`needs_item_target` has no Lua getter.** It is pure app-side bookkeeping (0437 §"What scoping
//!   verified": `TARGET_FLAG_ITEM` enchant casts need a client-side item pick after `DoCraft`) — the
//!   ref XML plays no part in that flow (no such concept exists in the real client's item-target UX
//!   either; it's the cursor/target machinery, not FrameXML). See `CraftFrame.xml`'s own header note.
//!
//! ## v1 is FLAT — no header rows (the law landed for TradeSkill; Craft is a named divergence)
//!
//! Exactly [`super::tradeskill`]'s own v1 law: [`CraftState::recipes`] renders in the app's pre-sorted
//! order, one row per recipe, `index` 1-based straight into that list. `GetCraftInfo` therefore never
//! returns the `"header"` `craftType` the ref Lua also checks for (ref l.38/77/199/252/292). The
//! header/grouping law itself is no longer pending — decision 0446 confirmed it byte-exact (wow-re
//! `tradeskill` TU-B) and TradeSkill grew the real tree engine ([`super::tradeskill`]'s `build_groups`)
//! on it. Craft never got the same port and stays on this v1 flat render; `Expand/CollapseCraftSkillLine`
//! are still literal no-ops, wired only so the ported XML's header-click handlers don't error
//! (TradeSkill's `Expand/CollapseTradeSkillSubClass` precedent). 0446 also confirmed the one live case
//! (Enchanting) renders flat either way — a single group's header suppresses under TradeSkill's own law
//! too — so the divergence is currently unobservable, but it is real and named as a follow-up in
//! decision 0530.
//!
//! ## The tooltip channels
//!
//! `GameTooltip:SetCraftItem(craftIndex, reagentIndex)` / `GameTooltip:SetCraftSpell(craftIndex)` are
//! registered in [`super::tooltip_item`], beside `SetTradeSkillItem` — see that module's doc for the
//! v1 SetCraftSpell call (a plain two-line render off this module's own `name`/`description`, not a
//! second ask-once round trip through [`super::tooltip_spell`]).

use mlua::{Lua, MultiValue, Value, Variadic};

use super::Model;

/// One reagent a recipe consumes (`GetCraftReagentInfo`): the bag-counted need/have plus the resolved
/// name/icon — both `None` until the app's ask-once item-template answer lands, the same miss path as
/// [`super::tradeskill::TradeSkillReagent`] (this struct's exact twin).
#[derive(Clone, Debug, PartialEq)]
pub struct CraftReagent {
    /// The reagent's item entry — the tooltip channel key (`GameTooltip:SetCraftItem(i, j)` resolves
    /// through this, never through `name`/`icon`).
    pub item: u32,
    /// The resolved display name, or `None` while the ask-once template answer is still in flight.
    pub name: Option<String>,
    /// The resolved icon texture path, or `None` while still in flight.
    pub icon: Option<String>,
    /// How many this recipe consumes.
    pub need: u32,
    /// How many the player's bags currently hold (`count_of`, decision 0269).
    pub have: u32,
}

/// One recipe row (`GetCraftInfo` + its detail-pane getters): a known spell carrying attr
/// `SPELL_ATTR_IS_TRADESKILL`, resolved by the app from `Spell.dbc`/`SkillLineAbility.dbc`/bag counts —
/// [`super::tradeskill::TradeSkillRecipe`]'s twin, minus the product-item concept, plus `sub_name` +
/// `description` (module doc, "Where Craft actually differs").
#[derive(Clone, Debug, PartialEq)]
pub struct CraftRecipe {
    /// The recipe spell id — what `DoCraft` queues as the `CMSG_CAST_SPELL` target.
    pub spell_id: u32,
    pub name: String,
    /// The rank subtext (`craftSubSpellName`, ref `GetCraftInfo`'s 2nd return) — `""` for most
    /// enchants; painted in parens beside the name when non-empty (ref l.217-221).
    pub sub_name: String,
    /// The color band `GetCraftInfo`'s `craftType` reports — [`super::TradeSkillDifficulty`] REUSED
    /// outright (module doc) for the four shared keys. The REAL client's Craft string table is
    /// WIDER (`0x807e00`, pinned 2026-07-17): tier codes shifted 1..4 with `0 → "none"`, plus a
    /// 5th `"used"` tier — a still-optimal recipe teaching a learnable `SPELL_EFFECT_LEARN_SPELL`,
    /// which only Beast Training recipes carry. An Enchanting book (this window's one occupant)
    /// can never produce either extra key, so the shared enum stays honest until the
    /// beast-training arc feeds one (that arc grows the vocabulary).
    pub difficulty: super::TradeSkillDifficulty,
    /// `min` over reagents of `floor(have/need)` — `0` when any reagent is short; the count
    /// `GetCraftInfo` reports as `numAvailable`.
    pub num_available: u32,
    /// The recipe's own icon (there is no separate "product" icon concept for Craft — module doc);
    /// `None` only while still in flight.
    pub icon: Option<String>,
    /// The pre-substituted spell description (`GetCraftDescription`) — app-resolved (the $-token
    /// engine lives app-side, [`super::tooltip_spell`]'s own law). `None` renders as a blank line (ref
    /// l.316: `CraftDescription:SetText(" ")`, never hidden).
    pub description: Option<String>,
    /// An `ENCHANT_ITEM`-family effect: `DoCraft` on this recipe arms an app-side item-target pick
    /// (0437 §"What scoping verified": `TARGET_FLAG_ITEM`/`TARGET_FLAG_TRADE_ITEM`) — pure app-side
    /// bookkeeping, no Lua getter (module doc).
    pub needs_item_target: bool,
    /// Consumed reagents, in display order (`GetCraftNumReagents`/`GetCraftReagentInfo`).
    pub reagents: Vec<CraftReagent>,
    /// The Requirements line's tool list — `(tool name, have)` pairs (e.g. `("Runed Copper Rod", true)`);
    /// exposed to Lua as `GetCraftSpellFocus`'s alternating multivalue
    /// ([`super::tradeskill::TradeSkillRecipe::tools`]'s twin).
    pub tools: Vec<(String, bool)>,
}

/// One open craft window: the skill line's name/rank and its (app-presorted) recipe list. Pushed whole
/// by the app ([`UiScript::set_craft`]); `None` means the window is closed.
#[derive(Clone, Debug, PartialEq)]
pub struct CraftState {
    /// The window's title (`GetCraftName`) — e.g. `"Enchanting"`. Also the skill line's display name
    /// for the rank StatusBar (`GetCraftDisplaySkillLine`'s first return) — Craft has only the one
    /// name, unlike TradeSkill's separately-fetched `GetTradeSkillLine`.
    pub name: String,
    pub rank: u32,
    pub max_rank: u32,
    /// The window's recipe rows, PRE-SORTED by the app — the engine renders this order verbatim (v1
    /// ships no grouping, module doc); `index` is 1-based straight into this list.
    pub recipes: Vec<CraftRecipe>,
}

impl super::UiScript {
    /// Push (or clear, with `None`) the open craft window's recipe snapshot. Snapshots re-push every
    /// time a resolved field changes — no diffing happens here. On a push, the engine **preserves the
    /// selection across the replace**: if the previously selected recipe's spell id still appears in
    /// the new list, the selection follows it to its new index; otherwise it clears to `0`
    /// ([`super::tradeskill::set_trade_skill`]'s own selection-survival law, verbatim). Clearing
    /// (`None`) always resets the selection.
    pub fn set_craft(&mut self, state: Option<CraftState>) {
        let mut model = self.model_mut();
        match state {
            None => {
                model.craft_selection = 0;
                model.craft = None;
            }
            Some(s) => {
                let prev_spell_id = model
                    .craft_selection
                    .checked_sub(1)
                    .and_then(|i| model.craft.as_ref().and_then(|c| c.recipes.get(i as usize)))
                    .map(|r| r.spell_id);
                model.craft_selection = prev_spell_id
                    .and_then(|sid| s.recipes.iter().position(|r| r.spell_id == sid))
                    .map_or(0, |i| (i + 1) as u32);
                model.craft = Some(s);
            }
        }
    }

    /// Drain the spell id intents `DoCraft` queued since the last call — the engine resolves each
    /// clicked recipe's INDEX to its spell id, so the app sends `CMSG_CAST_SPELL` without needing the
    /// index mapping. NO count: unlike [`super::tradeskill::take_trade_skill_dos`], the 1.12 CraftFrame
    /// has no Create All (module doc) — one `DoCraft` call, one queued spell id.
    pub fn take_craft_dos(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().craft_dos)
    }

    /// Whether `CloseCraft` was called since the last drain (and clear the flag). Like
    /// [`super::tradeskill::take_trade_skill_close`], the client-side close sends no packet.
    pub fn take_craft_close(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().craft_close)
    }

    /// Arm (or disarm) the item-targeted-cast pick: while armed, a bag click queues its
    /// `(bag, slot)` into the pick list instead of running the cursor gesture — the engine half
    /// of the CraftFrame enchant flow (byte-VERIFIED seam: the `PickupContainerItem 0x4f9b30`
    /// reroute, wow-re `tradeskill` TU-F). The app owns WHEN it's armed (an item-targeted
    /// `DoCraft`) and cleared (completion, close).
    pub fn set_item_pick_armed(&mut self, armed: bool) {
        self.model_mut().item_pick_armed = armed;
    }

    /// Drain the `(bag, slot)` clicks the armed pick consumed since the last call.
    pub fn take_item_picks(&mut self) -> Vec<(i64, u32)> {
        std::mem::take(&mut self.model_mut().item_picks)
    }
}

/// The recipe at a 1-based index, or `None` when out of range / no window is open.
fn recipe(model: &Model, index: usize) -> Option<&CraftRecipe> {
    let n = index.checked_sub(1)?;
    model.craft.as_ref()?.recipes.get(n)
}

/// The recipe count of the open window (`0` when none is open).
fn num_recipes(model: &Model) -> usize {
    model.craft.as_ref().map_or(0, |c| c.recipes.len())
}

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

/// Register the craft globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetCraftName() → the window's title ("UNKNOWN" when closed — TradeSkill's own
    // GetTradeSkillLine no-window convention, since the ref gives Craft no documented closed shape of
    // its own; CraftFrameTitleText:SetText(GetCraftName()) is called unconditionally, ref l.101).
    g.set(
        "GetCraftName",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let name = model
                .craft
                .as_ref()
                .map_or_else(|| "UNKNOWN".to_string(), |c| c.name.clone());
            Ok(Value::String(lua.create_string(&name)?))
        })?,
    )?;

    // GetCraftDisplaySkillLine() → name, rank, maxRank; name is NIL when closed (ref l.108-109 tests
    // it for truthiness to Show/Hide the whole rank StatusBar) — unlike GetCraftName above, which
    // always returns a string.
    g.set(
        "GetCraftDisplaySkillLine",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let (name, rank, max_rank) = match &model.craft {
                Some(c) => (Some(c.name.clone()), c.rank, c.max_rank),
                None => (None, 0, 0),
            };
            Ok(MultiValue::from_vec(vec![
                opt_str(lua, name.as_ref())?,
                Value::Integer(i64::from(rank)),
                Value::Integer(i64::from(max_rank)),
            ]))
        })?,
    )?;

    // → the number of recipes the open window offers (0 when closed).
    g.set(
        "GetNumCrafts",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(num_recipes(&model) as i64)
        })?,
    )?;

    // GetCraftInfo(index) → craftName, craftSubSpellName, craftType, numAvailable, isExpanded,
    // trainingPointCost, requiredLevel (ref l.37/168/289's own 7-tuple — module doc). `craftType` is
    // the difficulty color key, never "header"; `isExpanded` is always nil; the trailing two are
    // Beast Training's own fields, hardcoded 0 (out of scope, module doc). OOB → a single nil.
    g.set(
        "GetCraftInfo",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(r) = recipe(&model, index) else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&r.name)?),
                Value::String(lua.create_string(&r.sub_name)?),
                Value::String(lua.create_string(r.difficulty.as_str())?),
                Value::Integer(i64::from(r.num_available)),
                Value::Nil,
                Value::Integer(0),
                Value::Integer(0),
            ]))
        })?,
    )?;

    // GetCraftIcon(index) → icon texture path (nil while in flight / OOB).
    g.set(
        "GetCraftIcon",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            opt_str(lua, recipe(&model, index).and_then(|r| r.icon.as_ref()))
        })?,
    )?;

    // GetCraftDescription(index) → the pre-substituted description, or nil (no description / OOB) —
    // ref l.312 tests this return for truthiness before painting it.
    g.set(
        "GetCraftDescription",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            opt_str(
                lua,
                recipe(&model, index).and_then(|r| r.description.as_ref()),
            )
        })?,
    )?;

    // GetCraftNumReagents(index) → this recipe's reagent count (0 when OOB / no window).
    g.set(
        "GetCraftNumReagents",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(recipe(&model, index).map_or(0, |r| r.reagents.len()) as i64)
        })?,
    )?;

    // GetCraftReagentInfo(index, reagentIndex) → name, icon, need, have (a single nil when the
    // recipe/reagent index is OOB) — TradeSkill's own GetTradeSkillReagentInfo shape, verbatim.
    g.set(
        "GetCraftReagentInfo",
        lua.create_function(|lua, (index, reagent_index): (usize, usize)| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(reagent) = recipe(&model, index)
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

    // GetCraftSpellFocus(index) → an alternating (name, has) multivalue, one pair per Requirements
    // tool (e.g. "Runed Copper Rod", 1) — fed into BuildColoredListString (ref l.361), TradeSkill's
    // own GetTradeSkillTools shape. Empty when the recipe has no tools / index is OOB.
    g.set(
        "GetCraftSpellFocus",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let mut out = Vec::new();
            if let Some(r) = recipe(&model, index) {
                for (name, have) in &r.tools {
                    out.push(Value::String(lua.create_string(name)?));
                    out.push(era_bool(*have));
                }
            }
            Ok(MultiValue::from_vec(out))
        })?,
    )?;

    // GetCraftButtonToken() → "CREATE" (v1 constant): the ref resolves the Create button's label via
    // getglobal(GetCraftButtonToken()) (ref l.106), swapping to a "TRAIN" token for Beast Training —
    // out of scope (module doc), so this always returns the one token.
    g.set(
        "GetCraftButtonToken",
        lua.create_function(|lua, ()| Ok(Value::String(lua.create_string("CREATE")?)))?,
    )?;

    // GetCraftSelectionIndex() / SelectCraft(index) — the engine-held selection (1-based, 0 = none).
    // Out-of-range selects clear it; the getter clamps to the current recipe count — TradeSkill's own
    // selection contract, verbatim.
    g.set(
        "SelectCraft",
        lua.create_function(|lua, index: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let count = num_recipes(&model) as u32;
            model.craft_selection = if index >= 1 && index <= count {
                index
            } else {
                0
            };
            Ok(())
        })?,
    )?;
    g.set(
        "GetCraftSelectionIndex",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let count = num_recipes(&model) as u32;
            let sel = model.craft_selection;
            Ok(i64::from(if sel >= 1 && sel <= count { sel } else { 0 }))
        })?,
    )?;

    // DoCraft(index) — queue the recipe's SPELL ID (no count, module doc). Out of range → ignored.
    g.set(
        "DoCraft",
        lua.create_function(|lua, index: usize| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some(spell_id) = recipe(&model, index).map(|r| r.spell_id) {
                model.craft_dos.push(spell_id);
            }
            Ok(())
        })?,
    )?;

    // CloseCraft() — client-side close (no packet, vanilla): flag it so the app clears its local
    // state.
    g.set(
        "CloseCraft",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.craft_close = true;
            Ok(())
        })?,
    )?;

    // Expand/CollapseCraftSkillLine(index) — no-ops in v1: the recipe list is flat (the header law
    // landed for TradeSkill, decision 0446, but was never ported here — module doc), so the ported
    // XML's header-click handlers have nothing to fold — TradeSkill's own
    // Expand/CollapseTradeSkillSubClass precedent.
    g.set(
        "ExpandCraftSkillLine",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;
    g.set(
        "CollapseCraftSkillLine",
        lua.create_function(|_, _args: Variadic<Value>| Ok(()))?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::UiScript;

    /// One recipe fixture — a single-reagent, single-tool row, distinct spell ids per name.
    fn recipe(
        spell_id: u32,
        name: &str,
        difficulty: super::super::TradeSkillDifficulty,
        num_available: u32,
    ) -> CraftRecipe {
        CraftRecipe {
            spell_id,
            name: name.into(),
            sub_name: String::new(),
            difficulty,
            num_available,
            icon: Some(format!("Interface\\Icons\\Spell_{spell_id}")),
            description: Some(format!("{name} description.")),
            needs_item_target: true,
            reagents: vec![CraftReagent {
                item: 10940,
                name: Some("Illusion Dust".into()),
                icon: Some("Interface\\Icons\\INV_Enchant_EssenceMagicSmall".into()),
                need: 1,
                have: 3,
            }],
            tools: vec![("Runed Copper Rod".into(), true)],
        }
    }

    /// A two-recipe Enchanting window: Minor Beastslaying (trivial, craftable) then Minor Health
    /// (optimal, out of reagents).
    fn state() -> CraftState {
        CraftState {
            name: "Enchanting".into(),
            rank: 57,
            max_rank: 75,
            recipes: vec![
                recipe(
                    7418,
                    "Enchant Weapon - Minor Beastslaying",
                    super::super::TradeSkillDifficulty::Trivial,
                    5,
                ),
                recipe(
                    683,
                    "Enchant Weapon - Minor Health",
                    super::super::TradeSkillDifficulty::Optimal,
                    0,
                ),
            ],
        }
    }

    #[test]
    fn snapshot_feeds_the_api_tuples() {
        let mut s = UiScript::new().unwrap();
        s.set_craft(Some(state()));

        assert_eq!(
            s.eval::<String>("return GetCraftName()").unwrap(),
            "Enchanting"
        );
        let (name, rank, max_rank) = s
            .eval::<(String, i64, i64)>("return GetCraftDisplaySkillLine()")
            .unwrap();
        assert_eq!((name.as_str(), rank, max_rank), ("Enchanting", 57, 75));
        assert_eq!(s.eval::<i64>("return GetNumCrafts()").unwrap(), 2);

        let (n, sub, kind, avail, expanded, tp, lvl) = s
            .eval::<(String, String, String, i64, Option<i64>, i64, i64)>(
                "local n,s,t,a,e,tp,l = GetCraftInfo(1) return n,s,t,a,e,tp,l",
            )
            .unwrap();
        assert_eq!(
            (
                n.as_str(),
                sub.as_str(),
                kind.as_str(),
                avail,
                expanded,
                tp,
                lvl
            ),
            (
                "Enchant Weapon - Minor Beastslaying",
                "",
                "trivial",
                5,
                None,
                0,
                0
            )
        );
        let (_, _, kind2, avail2, _, _, _) = s
            .eval::<(String, String, String, i64, Option<i64>, i64, i64)>(
                "local n,s,t,a,e,tp,l = GetCraftInfo(2) return n,s,t,a,e,tp,l",
            )
            .unwrap();
        assert_eq!((kind2.as_str(), avail2), ("optimal", 0));

        assert_eq!(
            s.eval::<String>("return GetCraftIcon(1)").unwrap(),
            "Interface\\Icons\\Spell_7418"
        );
        assert_eq!(
            s.eval::<String>("return GetCraftDescription(1)").unwrap(),
            "Enchant Weapon - Minor Beastslaying description."
        );
        assert_eq!(s.eval::<i64>("return GetCraftNumReagents(1)").unwrap(), 1);
        let (rname, ricon, need, have) = s
            .eval::<(String, String, i64, i64)>("return GetCraftReagentInfo(1, 1)")
            .unwrap();
        assert_eq!(
            (rname.as_str(), ricon.as_str(), need, have),
            (
                "Illusion Dust",
                "Interface\\Icons\\INV_Enchant_EssenceMagicSmall",
                1,
                3
            )
        );
        assert_eq!(
            s.eval::<String>("return GetCraftButtonToken()").unwrap(),
            "CREATE"
        );

        // A header row never appears in v1 — GetCraftInfo's type is always a difficulty band.
        assert!(s
            .eval::<bool>("local _,_,t = GetCraftInfo(1) return t ~= 'header'")
            .unwrap());
        // Expand/CollapseCraftSkillLine are wired but do nothing observable.
        s.run("ExpandCraftSkillLine(1) CollapseCraftSkillLine(1)")
            .unwrap();
        assert_eq!(s.eval::<i64>("return GetNumCrafts()").unwrap(), 2);
    }

    #[test]
    fn selection_persists_across_a_repush_by_spell_id() {
        let mut s = UiScript::new().unwrap();
        // Minor Beastslaying (spell 7418) starts at index 2.
        let mut first = state();
        first.recipes.swap(0, 1);
        s.set_craft(Some(first));

        s.run("SelectCraft(2)").unwrap();
        assert_eq!(s.eval::<i64>("return GetCraftSelectionIndex()").unwrap(), 2);

        // Re-push with 7418 back at index 1 (a reagent-count re-list) — the selection follows it.
        s.set_craft(Some(state()));
        assert_eq!(s.eval::<i64>("return GetCraftSelectionIndex()").unwrap(), 1);

        // A re-push that drops the selected recipe entirely clears the selection.
        let mut without_7418 = state();
        without_7418.recipes.remove(0);
        s.set_craft(Some(without_7418));
        assert_eq!(s.eval::<i64>("return GetCraftSelectionIndex()").unwrap(), 0);
    }

    #[test]
    fn do_craft_drains_bare_spell_ids_no_count() {
        let mut s = UiScript::new().unwrap();
        s.set_craft(Some(state()));

        s.run("DoCraft(1) DoCraft(2)").unwrap();
        assert_eq!(s.take_craft_dos(), vec![7418, 683]);
        assert!(s.take_craft_dos().is_empty(), "drained");

        // An out-of-range index is ignored.
        s.run("DoCraft(9)").unwrap();
        assert!(s.take_craft_dos().is_empty());
    }

    #[test]
    fn no_snapshot_shapes_unknown_name_and_nil_info() {
        let mut s = UiScript::new().unwrap();
        assert_eq!(
            s.eval::<String>("return GetCraftName()").unwrap(),
            "UNKNOWN"
        );
        assert!(s
            .eval::<bool>("return GetCraftDisplaySkillLine() == nil")
            .unwrap());
        assert_eq!(s.eval::<i64>("return GetNumCrafts()").unwrap(), 0);
        assert!(s.eval::<bool>("return GetCraftInfo(1) == nil").unwrap());
        assert!(s.eval::<bool>("return GetCraftIcon(1) == nil").unwrap());
        assert!(s
            .eval::<bool>("return GetCraftDescription(1) == nil")
            .unwrap());
        assert_eq!(s.eval::<i64>("return GetCraftNumReagents(1)").unwrap(), 0);
        assert!(s
            .eval::<bool>("return GetCraftReagentInfo(1, 1) == nil")
            .unwrap());
        assert_eq!(s.eval::<i64>("return GetCraftSelectionIndex()").unwrap(), 0);

        s.run("DoCraft(1)").unwrap();
        assert!(s.take_craft_dos().is_empty(), "no window, no intent");
    }

    #[test]
    fn get_craft_spell_focus_multivalue_shape() {
        let mut s = UiScript::new().unwrap();
        let mut c = state();
        c.recipes[0].tools = vec![
            ("Runed Copper Rod".into(), true),
            ("Arcanite Rod".into(), false),
        ];
        s.set_craft(Some(c));

        let (a, b, cc, d) = s
            .eval::<(String, Option<i64>, String, Option<i64>)>(
                "local a,b,c,d = GetCraftSpellFocus(1) return a,b,c,d",
            )
            .unwrap();
        assert_eq!(
            (a.as_str(), b, cc.as_str(), d),
            ("Runed Copper Rod", Some(1), "Arcanite Rod", None)
        );

        // A recipe with no tools returns an empty multivalue (select('#', ...) == 0).
        let mut c2 = state();
        c2.recipes[0].tools.clear();
        s.set_craft(Some(c2));
        assert_eq!(
            s.eval::<i64>("return select('#', GetCraftSpellFocus(1))")
                .unwrap(),
            0
        );
    }

    #[test]
    fn close_and_clear_reset_the_window() {
        let mut s = UiScript::new().unwrap();
        s.set_craft(Some(state()));
        s.run("SelectCraft(1)").unwrap();

        assert!(!s.take_craft_close());
        s.run("CloseCraft()").unwrap();
        assert!(s.take_craft_close());
        assert!(!s.take_craft_close(), "drained");

        s.set_craft(None);
        assert_eq!(s.eval::<i64>("return GetNumCrafts()").unwrap(), 0);
        assert_eq!(s.eval::<i64>("return GetCraftSelectionIndex()").unwrap(), 0);
    }
}
