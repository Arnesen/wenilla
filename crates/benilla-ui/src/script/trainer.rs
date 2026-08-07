//! The trainer bindings (decision 0237) — the Era-shaped class/profession trainer surface driving a
//! faithful port of the real 1.12 `ClassTrainerFrame` (extracted from `interface.MPQ`:
//! `Interface\FrameXML\ClassTrainerFrame.{xml,lua}`). Same two-way seam as [`super::merchant`]: the
//! app pushes a **trainer snapshot** ([`UiScript::set_trainer`] — the wire services already resolved
//! to name/icon/cost/state/requirements by the app), and the Lua `BuyTrainerService`/`CloseTrainer`
//! calls queue outbound **intents** the app drains ([`UiScript::take_trainer_buys`] returns the
//! chosen services' **spell ids** / [`UiScript::take_trainer_close`]). The engine holds no trainer
//! knowledge — a service is name/subtext/icon/cost/state/gates/description, all app-resolved.
//!
//! ## The Era API shape (matched to the real `ClassTrainerFrame.lua`)
//!
//! The reference window's `.lua` is transcribed onto these bindings, so the return *shapes* match it
//! verbatim: `GetTrainerServiceInfo → name, subText, serviceType, isExpanded`;
//! `GetTrainerServiceCost → money, talentPointCost, professionPointCost`;
//! `GetTrainerServiceSkillReq → skill, rank, hasReq`; `GetTrainerServiceAbilityReq → name, hasReq`;
//! `IsTrainerServiceLearnSpell → isLearnSpell, isPetLearnSpell`. `serviceType` is either the string
//! `"header"` (a skill-line group header, with `isExpanded` set) or the service's colour state
//! `"available"`/`"unavailable"`/`"used"` (green/red/gray, `isExpanded` nil).
//!
//! ## The tree (decision 0247 — the byte-verified grouping/sort model)
//!
//! The 1.12 wire (`SMSG_TRAINER_LIST`) is a **flat** service list; the client builds a **collapsible
//! skill-line tree** on top of it, and `index` is **1-based into that visible tree**, not the wire
//! order. [`UiScript::set_trainer`] takes the flat services (each already carrying its resolved
//! [`TrainerService::skill_line`]/name, resolved app-side from `SkillLineAbility.dbc`) and synthesizes
//! the tree: one **header row per distinct skill line** (headers sorted by name), then that line's
//! services sorted by the within-group keys (required level → required-skill value → localized name →
//! localized rank — keys 4–7 of the verified comparator `0x4d85c0`). State is a per-row **colour**, not
//! the grouping key. A service whose skill line is `0` (unresolved) is dropped, matching the client.
//!
//! `GetNumTrainerServices`/every getter index the **visible** rows: headers always show, but a
//! service hides when its state fails the dropdown filter ([`Model::trainer_filter`]) or its group is
//! collapsed ([`Model::trainer_collapsed`]). `Collapse/ExpandTrainerSkillLine(id)` take the **display
//! index of a header row** (`id == 0` = all groups — the collapse-all button); they toggle that
//! group's visibility, never reorder.
//!
//! ## Faithful stubs (no wire data yet — kept so the ported XML runs)
//!
//! `GetTrainerServiceStepReq` returns nil (no tradeskill-step data on the wire), `IsTalentTrainer` is
//! always false (benilla drives no talent trainer), and per-requirement `hasReq`/`met` flags are
//! derived from the service's overall `category` (an unavailable service has an unmet gate) rather
//! than a per-gate wire bit, which 5875 does not send. The within-group sort uses the **class**
//! comparator (`0x4d85c0`) for every trainer; the talent/tradeskill comparator variants (`0x4d8850`/
//! `0x4d8760`, documented in 0247) are a later refinement — the director's window is the class trainer.

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// The green/red/gray state of a service (the wire's `TrainerSpellState`, decision 0237), surfaced to
/// Lua as the Era `category` string `GetTrainerServiceInfo` returns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrainerServiceCategory {
    /// Green — learnable now.
    #[default]
    Available,
    /// Red — a requirement (level / skill / prerequisite) is unmet.
    Unavailable,
    /// Gray — already known.
    Used,
}

impl TrainerServiceCategory {
    /// The Era `category` string (`ClassTrainerFrame`'s colour switch reads exactly these).
    fn era_str(self) -> &'static str {
        match self {
            TrainerServiceCategory::Available => "available",
            TrainerServiceCategory::Unavailable => "unavailable",
            TrainerServiceCategory::Used => "used",
        }
    }

    /// The filter-flag slot this category occupies ([`Model::trainer_filter`]).
    fn filter_slot(self) -> usize {
        match self {
            TrainerServiceCategory::Available => 0,
            TrainerServiceCategory::Unavailable => 1,
            TrainerServiceCategory::Used => 2,
        }
    }

    /// Parse the Era filter-type string the sort checkboxes pass.
    fn from_filter_str(s: &str) -> Option<Self> {
        match s {
            "available" => Some(TrainerServiceCategory::Available),
            "unavailable" => Some(TrainerServiceCategory::Unavailable),
            "used" => Some(TrainerServiceCategory::Used),
            _ => None,
        }
    }
}

/// A skill-line requirement on a service (`GetTrainerServiceSkillReq`): the skill's display name, the
/// rank required, and whether the player meets it (`hasReq`, derived from the service `category`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrainerSkillReq {
    pub name: String,
    pub rank: u32,
    pub met: bool,
}

/// A prerequisite-ability requirement (`GetTrainerServiceAbilityReq`): the ability's display name and
/// whether the player has it (`hasReq`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrainerAbilityReq {
    pub name: String,
    pub met: bool,
}

/// What `GameTooltip:SetTrainerService` describes for one row — the pre-resolved output of the
/// **app-side** tooltip law (`ui_trainer::service_tooltip`, transcribing `SetTrainerService
/// 0x5338b0`). The engine holds no DBC, so the app decides the subject and the engine only picks a
/// renderer; the binding in the real client does exactly that too — it emits no line of its own and
/// is a three-way selector into the shared spell builder `0x52e610` or item builder `0x52b650`.
///
/// This is deliberately **not** derivable from [`TrainerService::texture`]: the tooltip and the icon
/// disagree, by design and in both directions (wow-re `ui/scratch/trainer-service-tooltip-law.md`
/// §6 — the icon needs a trainer-type gate the tooltip does not have, and the icon pins the *wire*
/// wrapper where the tooltip hops to the *taught* spell). On ~806 of the shipped corpus's trainer
/// services the reference client visibly shows one spell's icon above another spell's tooltip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrainerTooltip {
    /// The ITEM builder, on this item id — the taught spell carries `SPELL_ATTR_IS_TRADESKILL`.
    /// `0` (or a template still in flight) renders an EMPTY tooltip, which is the client's own
    /// behaviour: the builder early-outs on a cache miss and the query is enqueued.
    Item(u32),
    /// The SPELL builder, on the taught spell — or on the wire spell itself when no learn-wrapper
    /// slot resolved, the only path that describes the wrapper.
    Spell {
        spell_id: u32,
        /// `param5` altCaster — ONE gate suppressing BOTH the totems and the reagents lines
        /// (byte-verified at `0x52ed43` and `0x52f393`). Set exactly when the matched wrapper slot
        /// was `SPELL_EFFECT_LEARN_PET_SPELL`.
        alt_caster: bool,
    },
}

impl Default for TrainerTooltip {
    fn default() -> Self {
        TrainerTooltip::Spell {
            spell_id: 0,
            alt_caster: false,
        }
    }
}

/// One trainer service row, resolved by the app from the wire `TrainerSpell` (decision 0237). Plain
/// data — position in [`TrainerState::services`] is the *unsorted* wire order; the 1-based index the
/// Lua uses is a position in the visible display tree ([`rows`], built from [`TrainerState::groups`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrainerService {
    /// The service spell id — what the app sends for `CMSG_TRAINER_BUY_SPELL` when this row is bought.
    pub spell_id: u32,
    /// The spell name (`GetTrainerServiceInfo`'s first return); `None` while `Spell.dbc` is still
    /// loading (the API reports `nil`, the XML shows `UNKNOWN`).
    pub name: Option<String>,
    /// The rank/subtext line (`Spell.dbc` NameSubtext — "Rank N"); `None`/empty shows no second line.
    pub subtext: Option<String>,
    /// Icon texture path (`Interface\Icons\…`); `None` while the catalog answer is in flight.
    pub texture: Option<String>,
    /// The spell description shown in the detail pane (`GetTrainerServiceDescription`); `""` when the
    /// app has no description source yet.
    pub description: String,
    /// Money cost in copper (already reputation-discounted server-side).
    pub cost: u32,
    /// A primary-profession first rank — the real window's `cpCost2 > 0` (the profession-point cost
    /// that raises the "you can only have two professions" confirm dialog).
    pub prof_first_rank: bool,
    /// The green/red/gray colour ([`TrainerServiceCategory`]).
    pub category: TrainerServiceCategory,
    /// Character level required (0 = none).
    pub level_req: u32,
    /// The skill-line requirement, or `None` when the service has no skill gate.
    pub skill_req: Option<TrainerSkillReq>,
    /// Prerequisite-ability requirements (resolved from the wire's req-spell ids); empty when none.
    pub ability_reqs: Vec<TrainerAbilityReq>,
    /// A tradeskill step (`IsTrainerServiceTradeSkill`) vs. a plain learn-spell.
    pub is_trade_skill: bool,
    /// The taught spell's skill line (`SkillLineAbility.dbc` → `SkillLine.dbc`), resolved app-side —
    /// the tree's grouping key (decision 0247). `0` = unresolved: the service is **dropped** from the
    /// tree (the client's `skillLine == 0 → drop`).
    pub skill_line: u32,
    /// The skill line's localized display name (`SkillLine.dbc`) — the header row's text.
    pub skill_line_name: String,
    /// What the detail-icon hover describes ([`TrainerTooltip`]) — resolved app-side, because the
    /// law reads `Spell.dbc` fields the engine cannot see. Independent of [`Self::texture`].
    pub tooltip: TrainerTooltip,
}

/// One skill-line group in the display tree (decision 0247): the header's skill line + name and the
/// positions (into [`TrainerState::services`]) of the line's services, pre-sorted by the within-group
/// keys. Synthesized by [`UiScript::set_trainer`] — the app pushes only the flat services.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrainerGroup {
    /// The skill-line id (the collapse key).
    pub skill_line: u32,
    /// The header's display name.
    pub name: String,
    /// The line's services, as positions into [`TrainerState::services`], sorted (level → skill-value
    /// → name → rank).
    pub services: Vec<usize>,
}

/// One open trainer window: the services, the greeting line, and whether the trainer is a tradeskill
/// trainer (`IsTradeskillTrainer` — drives the real window's tradeskill-vs-class layout switch).
/// Pushed whole by the app; `None` means no trainer is open (the window is closed).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrainerState {
    pub services: Vec<TrainerService>,
    /// The trainer's greeting/title line (`SMSG_TRAINER_LIST`'s trailing string).
    pub greeting: String,
    /// `IsTradeskillTrainer()` — the whole-trainer kind (`trainer_type == 2`).
    pub is_tradeskill: bool,
    /// The sorted skill-line groups over `services` — **built by [`UiScript::set_trainer`]**, not
    /// pushed by the app (default-empty on a bare `TrainerState`). The display tree walks these.
    pub groups: Vec<TrainerGroup>,
}

impl super::UiScript {
    /// Push (or clear, with `None`) the open trainer's service snapshot. On a push, the engine
    /// **builds the skill-line tree** ([`build_groups`]) from the flat services and prunes the
    /// collapse set to the lines that still exist (so a collapse survives a content update — a buy
    /// re-lists — but a switch to a different trainer starts expanded). Clearing resets the selection
    /// and the collapse set (a closed window holds nothing).
    pub fn set_trainer(&mut self, state: Option<TrainerState>) {
        let mut model = self.model_mut();
        match state {
            None => {
                model.trainer_selection = 0;
                model.trainer_collapsed.clear();
                model.trainer = None;
            }
            Some(mut s) => {
                s.groups = build_groups(&s.services);
                let live: std::collections::HashSet<u32> =
                    s.groups.iter().map(|g| g.skill_line).collect();
                model.trainer_collapsed.retain(|sl| live.contains(sl));
                model.trainer = Some(s);
            }
        }
    }

    /// Drain the **spell ids** `BuyTrainerService` queued since the last call (the engine resolves each
    /// clicked filtered row to its service's spell id, so the app sends `CMSG_TRAINER_BUY_SPELL`
    /// without needing to know the filter/index mapping).
    pub fn take_trainer_buys(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().trainer_buys)
    }

    /// Whether `CloseTrainer` was called since the last drain (and clear the flag). vanilla's
    /// client-side close sends no packet — the app just clears its local trainer state.
    pub fn take_trainer_close(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().trainer_close)
    }
}

/// One visible row of the display tree: a skill-line **header** (carrying its group index) or a
/// **service** (carrying its position into [`TrainerState::services`]).
#[derive(Clone, Copy)]
enum Row {
    Header(usize),
    Service(usize),
}

/// The WoW enUS collator (`0x64a480`), approximated: case-insensitive alphabetical, with the raw
/// bytes as a stable tie-break so equal-when-folded names keep a deterministic order.
fn collate(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

/// The within-group service comparator — keys 4–7 of the verified class comparator (`0x4d85c0`,
/// decision 0247): required level → required-skill value → localized name → localized rank/subtext.
fn service_order(a: &TrainerService, b: &TrainerService) -> std::cmp::Ordering {
    let skillval = |s: &TrainerService| s.skill_req.as_ref().map_or(0, |r| r.rank);
    let name = |s: &TrainerService| s.name.clone().unwrap_or_default();
    let rank = |s: &TrainerService| s.subtext.clone().unwrap_or_default();
    a.level_req
        .cmp(&b.level_req)
        .then_with(|| skillval(a).cmp(&skillval(b)))
        .then_with(|| collate(&name(a), &name(b)))
        .then_with(|| collate(&rank(a), &rank(b)))
}

/// Build the display tree from the flat services (decision 0247): group by skill line (dropping the
/// unresolved `skill_line == 0`), sort each group's services by [`service_order`], and sort the groups
/// by localized name (skill-line id breaks a name tie). The service positions index back into the
/// unchanged `services` slice, so every getter still resolves a row to its full service data.
fn build_groups(services: &[TrainerService]) -> Vec<TrainerGroup> {
    let mut map: std::collections::HashMap<u32, TrainerGroup> = std::collections::HashMap::new();
    for (i, s) in services.iter().enumerate() {
        if s.skill_line == 0 {
            continue;
        }
        map.entry(s.skill_line)
            .or_insert_with(|| TrainerGroup {
                skill_line: s.skill_line,
                name: s.skill_line_name.clone(),
                services: Vec::new(),
            })
            .services
            .push(i);
    }
    let mut groups: Vec<TrainerGroup> = map.into_values().collect();
    for g in &mut groups {
        g.services
            .sort_by(|&a, &b| service_order(&services[a], &services[b]));
    }
    groups.sort_by(|a, b| collate(&a.name, &b.name).then(a.skill_line.cmp(&b.skill_line)));
    groups
}

/// The visible rows in display order (decision 0247): each group's header (always shown), then — when
/// the group is not collapsed — its services that pass the state filter. The Lua's 1-based `index` is
/// a position in *this* list.
fn rows(model: &Model) -> Vec<Row> {
    let Some(t) = model.trainer.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (gi, g) in t.groups.iter().enumerate() {
        out.push(Row::Header(gi));
        if !model.trainer_collapsed.contains(&g.skill_line) {
            for &si in &g.services {
                if model.trainer_filter[t.services[si].category.filter_slot()] {
                    out.push(Row::Service(si));
                }
            }
        }
    }
    out
}

/// The service at a 1-based visible index, or `None` when that row is a **header** (or OOB / no
/// trainer) — so the getters/buy that read a service safely no-op on a header row. `pub(super)`:
/// `GameTooltip:SetTrainerService` lives in the tooltip channel and must resolve its index through
/// the same VISIBLE mapping, never a raw `services[]` position.
pub(super) fn service(model: &Model, index: usize) -> Option<&TrainerService> {
    let n = index.checked_sub(1)?;
    match rows(model).get(n)? {
        Row::Service(si) => model.trainer.as_ref()?.services.get(*si),
        Row::Header(_) => None,
    }
}

/// The count of visible rows (headers + unfiltered, uncollapsed services).
fn num_services(model: &Model) -> usize {
    rows(model).len()
}

/// Collapse (`collapse = true`) or expand a skill line by the **display index of its header row**
/// (decision 0247's `Collapse/ExpandTrainerSkillLine`). `id == 0` targets **all** groups (the
/// collapse-all button); `id > 0` resolves the header at that visible index to its skill line. A
/// non-header (or OOB) index is a no-op.
fn set_collapsed(model: &mut Model, id: usize, collapse: bool) {
    if id == 0 && !collapse {
        model.trainer_collapsed.clear();
        return;
    }
    let targets: Vec<u32> = if id == 0 {
        model
            .trainer
            .as_ref()
            .map(|t| t.groups.iter().map(|g| g.skill_line).collect())
            .unwrap_or_default()
    } else {
        match rows(model).get(id - 1) {
            Some(Row::Header(gi)) => model
                .trainer
                .as_ref()
                .and_then(|t| t.groups.get(*gi))
                .map(|g| g.skill_line)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    };
    for sl in targets {
        if collapse {
            model.trainer_collapsed.insert(sl);
        } else {
            model.trainer_collapsed.remove(&sl);
        }
    }
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

/// Register the trainer globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // → the number of services the open trainer offers under the current filter (0 when closed).
    g.set(
        "GetNumTrainerServices",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(num_services(&model) as i64)
        })?,
    )?;

    // GetTrainerServiceInfo(index) → name, subText, serviceType, isExpanded. `index` 1-based into the
    // visible tree; out of range → a single nil. A HEADER row returns (skillLineName, nil, "header",
    // isExpanded); a SERVICE row returns (name, subText, stateString, nil) (decision 0247).
    g.set(
        "GetTrainerServiceInfo",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(n) = index.checked_sub(1) else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let Some(row) = rows(&model).get(n).copied() else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            let t = model
                .trainer
                .as_ref()
                .expect("a visible row ⇒ an open trainer");
            match row {
                Row::Header(gi) => {
                    let g = &t.groups[gi];
                    let expanded = !model.trainer_collapsed.contains(&g.skill_line);
                    Ok(MultiValue::from_vec(vec![
                        Value::String(lua.create_string(&g.name)?),
                        Value::Nil,
                        Value::String(lua.create_string("header")?),
                        era_bool(expanded),
                    ]))
                }
                Row::Service(si) => {
                    let s = &t.services[si];
                    Ok(MultiValue::from_vec(vec![
                        opt_str(lua, s.name.as_ref())?,
                        opt_str(lua, s.subtext.as_ref())?,
                        Value::String(lua.create_string(s.category.era_str())?),
                        Value::Nil,
                    ]))
                }
            }
        })?,
    )?;

    // GetTrainerServiceIcon(index) → texture path (nil while in flight / OOB).
    g.set(
        "GetTrainerServiceIcon",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            opt_str(lua, service(&model, index).and_then(|s| s.texture.as_ref()))
        })?,
    )?;

    // GetTrainerServiceDescription(index) → the spell description ("" when the app has none / OOB).
    g.set(
        "GetTrainerServiceDescription",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let text = service(&model, index)
                .map(|s| s.description.clone())
                .unwrap_or_default();
            lua.create_string(&text)
        })?,
    )?;

    // GetTrainerServiceCost(index) → money, talentPointCost, professionPointCost. Talent cost is
    // always 0 (no talent trainer); profession cost is 1 for a primary-profession first rank.
    g.set(
        "GetTrainerServiceCost",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let (money, prof) = service(&model, index)
                .map(|s| (s.cost, u32::from(s.prof_first_rank)))
                .unwrap_or((0, 0));
            Ok(MultiValue::from_vec(vec![
                Value::Integer(i64::from(money)),
                Value::Integer(0),
                Value::Integer(i64::from(prof)),
            ]))
        })?,
    )?;

    // GetTrainerServiceLevelReq(index) → required character level (0 = none).
    g.set(
        "GetTrainerServiceLevelReq",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(service(&model, index).map_or(0, |s| s.level_req) as i64)
        })?,
    )?;

    // GetTrainerServiceSkillReq(index) → skillName, skillRank, hasReq (nil when no skill gate / OOB).
    g.set(
        "GetTrainerServiceSkillReq",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(req) = service(&model, index).and_then(|s| s.skill_req.as_ref()) else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&req.name)?),
                Value::Integer(i64::from(req.rank)),
                era_bool(req.met),
            ]))
        })?,
    )?;

    // GetTrainerServiceNumAbilityReq(index) → how many prerequisite abilities the service lists.
    g.set(
        "GetTrainerServiceNumAbilityReq",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(service(&model, index).map_or(0, |s| s.ability_reqs.len()) as i64)
        })?,
    )?;

    // GetTrainerServiceAbilityReq(index, i) → abilityName, hasReq (nil OOB).
    g.set(
        "GetTrainerServiceAbilityReq",
        lua.create_function(|lua, (index, i): (usize, usize)| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let Some(req) = service(&model, index)
                .and_then(|s| i.checked_sub(1).and_then(|n| s.ability_reqs.get(n)))
            else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(&req.name)?),
                era_bool(req.met),
            ]))
        })?,
    )?;

    // GetTrainerServiceStepReq(index) → step, met — always nil (no tradeskill-step data on the wire).
    g.set(
        "GetTrainerServiceStepReq",
        lua.create_function(|_, _index: usize| Ok(Value::Nil))?,
    )?;

    // IsTrainerServiceTradeSkill(index) → 1/nil (a tradeskill step vs. a learn-spell).
    g.set(
        "IsTrainerServiceTradeSkill",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(era_bool(
                service(&model, index).is_some_and(|s| s.is_trade_skill),
            ))
        })?,
    )?;

    // IsTrainerServiceLearnSpell(index) → isLearnSpell, isPetLearnSpell. Pet-learn is always nil
    // (benilla drives no pet trainer); a non-tradeskill service is a learn-spell.
    g.set(
        "IsTrainerServiceLearnSpell",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let learn = service(&model, index).is_some_and(|s| !s.is_trade_skill);
            Ok(MultiValue::from_vec(vec![era_bool(learn), Value::Nil]))
        })?,
    )?;

    // IsTradeskillTrainer() → 1/nil for the whole trainer (drives the tradeskill-vs-class layout).
    g.set(
        "IsTradeskillTrainer",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(era_bool(
                model.trainer.as_ref().is_some_and(|t| t.is_tradeskill),
            ))
        })?,
    )?;

    // IsTalentTrainer() → always nil (benilla drives no talent trainer; the ref window's talent hacks
    // guard on this).
    g.set(
        "IsTalentTrainer",
        lua.create_function(|_, ()| Ok(Value::Nil))?,
    )?;

    // GetTrainerGreetingText() → the trainer's greeting line ("" when no trainer is open).
    g.set(
        "GetTrainerGreetingText",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let text = model
                .trainer
                .as_ref()
                .map(|t| t.greeting.clone())
                .unwrap_or_default();
            lua.create_string(&text)
        })?,
    )?;

    // GetTrainerServiceTypeFilter(type) → 1/nil (whether that category is shown). SetTrainerService-
    // TypeFilter(type, on) toggles it — the sort checkboxes' real client-side filtering.
    g.set(
        "GetTrainerServiceTypeFilter",
        lua.create_function(|lua, kind: String| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(match TrainerServiceCategory::from_filter_str(&kind) {
                Some(c) => era_bool(model.trainer_filter[c.filter_slot()]),
                None => Value::Nil,
            })
        })?,
    )?;
    g.set(
        "SetTrainerServiceTypeFilter",
        lua.create_function(|lua, (kind, on): (String, Value)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some(c) = TrainerServiceCategory::from_filter_str(&kind) {
                // Era passes 1 / 0 (or true/nil); anything truthy-but-not-0 enables.
                let enable = !matches!(on, Value::Nil | Value::Integer(0) | Value::Boolean(false));
                model.trainer_filter[c.filter_slot()] = enable;
            }
            Ok(())
        })?,
    )?;

    // Collapse/ExpandTrainerSkillLine(id) — fold a skill line by the display index of its header row
    // (id 0 = all groups, the collapse-all button); a non-header index no-ops (decision 0247).
    g.set(
        "CollapseTrainerSkillLine",
        lua.create_function(|lua, id: usize| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            set_collapsed(&mut model, id, true);
            Ok(())
        })?,
    )?;
    g.set(
        "ExpandTrainerSkillLine",
        lua.create_function(|lua, id: usize| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            set_collapsed(&mut model, id, false);
            Ok(())
        })?,
    )?;

    // SelectTrainerService(index) — set the engine-held selection (1-based filtered; 0/OOB clears it).
    g.set(
        "SelectTrainerService",
        lua.create_function(|lua, index: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let count = num_services(&model) as u32;
            model.trainer_selection = if index >= 1 && index <= count {
                index
            } else {
                0
            };
            Ok(())
        })?,
    )?;

    // GetTrainerSelectionIndex() → the selected 1-based filtered row, or 0 if nothing is selected
    // (the ref reads it as a plain number: `GetTrainerSelectionIndex() > 1`). Clamped to the current
    // filtered count so a selection left over from a previous filter/trainer reads as 0.
    g.set(
        "GetTrainerSelectionIndex",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let count = num_services(&model) as u32;
            let sel = model.trainer_selection;
            Ok(i64::from(if sel >= 1 && sel <= count { sel } else { 0 }))
        })?,
    )?;

    // BuyTrainerService(index) — queue the selected filtered row's SPELL ID for purchase (the app
    // sends CMSG_TRAINER_BUY_SPELL). Out of range → ignored.
    g.set(
        "BuyTrainerService",
        lua.create_function(|lua, index: usize| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some(spell_id) = service(&model, index).map(|s| s.spell_id) {
                model.trainer_buys.push(spell_id);
            }
            Ok(())
        })?,
    )?;

    // CloseTrainer() — client-side close (no packet, vanilla): flag it so the app clears its state.
    g.set(
        "CloseTrainer",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.trainer_close = true;
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::UiScript;

    fn svc(
        spell_id: u32,
        name: &str,
        cat: TrainerServiceCategory,
        skill_line: u32,
        line_name: &str,
    ) -> TrainerService {
        TrainerService {
            spell_id,
            // The fixture's default subject: the wire spell, no hop (the "no learn wrapper"
            // fallback); the tooltip tests set the arm they mean explicitly.
            tooltip: TrainerTooltip::Spell {
                spell_id,
                alt_caster: false,
            },
            name: Some(name.into()),
            subtext: Some("Rank 1".into()),
            texture: Some(format!("Interface\\Icons\\Spell_{spell_id}")),
            description: format!("Teaches {name}."),
            cost: 100,
            prof_first_rank: false,
            category: cat,
            level_req: 10,
            skill_req: None,
            ability_reqs: vec![],
            is_trade_skill: false,
            skill_line,
            skill_line_name: line_name.into(),
        }
    }

    /// A two-line warrior trainer. Groups sort by name (Arms < Fury); within Arms, the level keys
    /// order Heroic Strike (level 1) before Cleave (level 20). The default-expanded tree is thus:
    /// `[H:Arms, Heroic Strike(avail), Cleave(used), H:Fury, Bloodrage(unavail)]` — a 5-row list whose
    /// service rows sit at indices 2, 3, 5.
    fn trainer() -> TrainerState {
        let mut hs = svc(
            78,
            "Heroic Strike",
            TrainerServiceCategory::Available,
            26,
            "Arms",
        );
        hs.level_req = 1;
        let mut cl = svc(284, "Cleave", TrainerServiceCategory::Used, 26, "Arms");
        cl.level_req = 20;
        let br = svc(
            285,
            "Bloodrage",
            TrainerServiceCategory::Unavailable,
            256,
            "Fury",
        );
        TrainerState {
            greeting: "What would you like to learn?".into(),
            is_tradeskill: false,
            groups: Vec::new(),
            services: vec![hs, cl, br],
        }
    }

    #[test]
    fn tree_interleaves_headers_and_ordered_services() {
        let mut s = UiScript::new().unwrap();
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 0);
        assert!(s
            .eval::<bool>("return GetTrainerServiceInfo(1) == nil")
            .unwrap());

        s.set_trainer(Some(trainer()));
        // 2 headers + 3 services = 5 visible rows.
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);

        // Row 1 is the "Arms" header: name, nil subtext, "header", isExpanded=1.
        let (hn, hsub, ht, hexp) = s
            .eval::<(String, Option<String>, String, Option<i64>)>(
                "local n,s,t,e = GetTrainerServiceInfo(1) return n,s,t,e",
            )
            .unwrap();
        assert_eq!((hn.as_str(), ht.as_str()), ("Arms", "header"));
        assert_eq!((hsub, hexp), (None, Some(1)));

        // Rows 2..5: services (name/state) then the Fury header then its service — the sorted tree.
        let info = |s: &mut UiScript, i: i64| {
            s.eval::<(String, String)>(&format!(
                "local n,_,t = GetTrainerServiceInfo({i}) return n,t"
            ))
            .unwrap()
        };
        assert_eq!(
            info(&mut s, 2),
            ("Heroic Strike".into(), "available".into())
        );
        assert_eq!(info(&mut s, 3), ("Cleave".into(), "used".into()));
        assert_eq!(info(&mut s, 4), ("Fury".into(), "header".into()));
        assert_eq!(info(&mut s, 5), ("Bloodrage".into(), "unavailable".into()));
    }

    #[test]
    fn service_getters_read_the_row_at_a_visible_index() {
        let mut s = UiScript::new().unwrap();
        let mut t = trainer();
        // Heroic Strike (services[0], the row at index 2) carries a full gate set.
        t.services[0].skill_req = Some(TrainerSkillReq {
            name: "Blacksmithing".into(),
            rank: 100,
            met: true,
        });
        t.services[0].ability_reqs = vec![TrainerAbilityReq {
            name: "Apprentice".into(),
            met: false,
        }];
        s.set_trainer(Some(t));

        // Cost/level/desc/reqs all resolve at the SERVICE index 2 (not the header at 1).
        assert_eq!(
            s.eval::<(i64, i64, i64)>("return GetTrainerServiceCost(2)")
                .unwrap(),
            (100, 0, 0)
        );
        assert_eq!(
            s.eval::<i64>("return GetTrainerServiceLevelReq(2)")
                .unwrap(),
            1
        );
        assert_eq!(
            s.eval::<String>("return GetTrainerServiceDescription(2)")
                .unwrap(),
            "Teaches Heroic Strike."
        );
        let (skill, rank, has) = s
            .eval::<(String, i64, i64)>("return GetTrainerServiceSkillReq(2)")
            .unwrap();
        assert_eq!((skill.as_str(), rank, has), ("Blacksmithing", 100, 1));
        assert!(s
            .eval::<bool>(
                "local n,h = GetTrainerServiceAbilityReq(2,1) return n=='Apprentice' and h==nil"
            )
            .unwrap());
        assert!(s
            .eval::<bool>("local l,p = IsTrainerServiceLearnSpell(2) return l==1 and p==nil")
            .unwrap());

        // A HEADER row (index 1) has no service data — the getters no-op to defaults.
        assert_eq!(
            s.eval::<i64>("return GetTrainerServiceLevelReq(1)")
                .unwrap(),
            0
        );
        assert!(s
            .eval::<bool>("return GetTrainerServiceIcon(1) == nil")
            .unwrap());
        assert!(s
            .eval::<bool>("return GetTrainerServiceSkillReq(1) == nil")
            .unwrap());
    }

    #[test]
    fn state_filter_hides_services_not_headers() {
        let mut s = UiScript::new().unwrap();
        s.set_trainer(Some(trainer()));
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);

        // Hide "used": Cleave drops (5 → 4), but BOTH headers stay — a state filter culls services
        // only. Row 3 is now the Fury header (Cleave was there).
        s.run("SetTrainerServiceTypeFilter('used', 0)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 4);
        let types: Vec<String> = (1..=4)
            .map(|i| {
                s.eval::<String>(&format!(
                    "local _,_,t = GetTrainerServiceInfo({i}) return t"
                ))
                .unwrap()
            })
            .collect();
        assert_eq!(types, ["header", "available", "header", "unavailable"]);

        // Hide ALL states: the two headers remain (each group keeps its header even with no services).
        s.run("SetTrainerServiceTypeFilter('available', 0)")
            .unwrap();
        s.run("SetTrainerServiceTypeFilter('unavailable', 0)")
            .unwrap();
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 2);
        assert!(s
            .eval::<bool>(
                "local _,_,t1 = GetTrainerServiceInfo(1) \
                 local _,_,t2 = GetTrainerServiceInfo(2) \
                 return t1=='header' and t2=='header'"
            )
            .unwrap());
    }

    #[test]
    fn collapse_by_header_index_and_collapse_all() {
        let mut s = UiScript::new().unwrap();
        s.set_trainer(Some(trainer()));

        // Collapse the Arms group by its header's display index (1): its two services vanish, the
        // header stays and now reports isExpanded=nil. 5 → 3 (H:Arms, H:Fury, Bloodrage).
        s.run("CollapseTrainerSkillLine(1)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 3);
        assert!(s
            .eval::<bool>("local _,_,t,e = GetTrainerServiceInfo(1) return t=='header' and e==nil")
            .unwrap());
        assert_eq!(
            s.eval::<String>("local n = GetTrainerServiceInfo(2) return n")
                .unwrap(),
            "Fury",
            "Arms' services are folded; Fury's header is now row 2"
        );

        // Expand it back by the same header index.
        s.run("ExpandTrainerSkillLine(1)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);

        // Collapse-all (id 0): both groups fold → just the two headers.
        s.run("CollapseTrainerSkillLine(0)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 2);
        // Expand-all (id 0): back to the full tree.
        s.run("ExpandTrainerSkillLine(0)").unwrap();
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);
    }

    #[test]
    fn collapse_survives_a_content_update_and_resets_on_close() {
        let mut s = UiScript::new().unwrap();
        s.set_trainer(Some(trainer()));
        s.run("CollapseTrainerSkillLine(1)").unwrap(); // fold Arms
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 3);

        // A re-list (same skill lines) keeps the fold — a buy re-lists and the tree shouldn't jump open.
        s.set_trainer(Some(trainer()));
        assert_eq!(
            s.eval::<i64>("return GetNumTrainerServices()").unwrap(),
            3,
            "Arms stays collapsed across a content update"
        );

        // Close → the collapse set resets; a re-open is fully expanded.
        s.set_trainer(None);
        s.set_trainer(Some(trainer()));
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);
    }

    #[test]
    fn buy_queues_the_selected_services_spell_id_headers_no_op() {
        let mut s = UiScript::new().unwrap();
        s.set_trainer(Some(trainer()));
        // Row 5 is Bloodrage (spell 285). Buying it queues its spell id, not its index.
        s.run("BuyTrainerService(5)").unwrap();
        assert_eq!(s.take_trainer_buys(), vec![285]);
        assert!(s.take_trainer_buys().is_empty(), "drained");

        // Buying a HEADER row (index 1) queues nothing.
        s.run("BuyTrainerService(1)").unwrap();
        assert!(s.take_trainer_buys().is_empty(), "a header is not buyable");
    }

    #[test]
    fn selection_and_close_intents() {
        let mut s = UiScript::new().unwrap();
        s.set_trainer(Some(trainer()));
        assert_eq!(
            s.eval::<i64>("return GetTrainerSelectionIndex()").unwrap(),
            0
        );
        s.run("SelectTrainerService(2)").unwrap();
        assert_eq!(
            s.eval::<i64>("return GetTrainerSelectionIndex()").unwrap(),
            2
        );
        s.run("SelectTrainerService(9)").unwrap(); // OOB (past the 5 rows) clears
        assert_eq!(
            s.eval::<i64>("return GetTrainerSelectionIndex()").unwrap(),
            0
        );

        assert!(!s.take_trainer_close());
        s.run("CloseTrainer()").unwrap();
        assert!(s.take_trainer_close());
        assert!(!s.take_trainer_close(), "drained");
    }

    #[test]
    fn tradeskill_and_talent_flags() {
        let mut s = UiScript::new().unwrap();
        assert!(s
            .eval::<bool>("return IsTradeskillTrainer() == nil")
            .unwrap());
        let mut t = trainer();
        t.is_tradeskill = true;
        t.services[0].is_trade_skill = true; // Heroic Strike → the row at index 2
        s.set_trainer(Some(t));
        assert!(s.eval::<bool>("return IsTradeskillTrainer() == 1").unwrap());
        assert!(s
            .eval::<bool>("return IsTrainerServiceTradeSkill(2) == 1")
            .unwrap());
        assert!(s.eval::<bool>("return IsTalentTrainer() == nil").unwrap());
    }

    #[test]
    fn unresolved_skill_line_is_dropped() {
        let mut s = UiScript::new().unwrap();
        let mut t = trainer();
        // A service whose skill line didn't resolve (0) is dropped from the tree entirely.
        t.services.push(svc(
            999,
            "Orphan Spell",
            TrainerServiceCategory::Available,
            0,
            "",
        ));
        s.set_trainer(Some(t));
        // Still 5 rows — the orphan contributes neither a header nor a service row.
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 5);
    }

    #[test]
    fn clearing_empties_and_resets_selection() {
        let mut s = UiScript::new().unwrap();
        s.set_trainer(Some(trainer()));
        s.run("SelectTrainerService(2)").unwrap();
        s.set_trainer(None);
        assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 0);
        assert_eq!(
            s.eval::<i64>("return GetTrainerSelectionIndex()").unwrap(),
            0
        );
    }
}
