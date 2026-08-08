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
//! ## The tree (decisions 0247 + 1124 — the byte-verified grouping/sort model, **per trainer type**)
//!
//! The 1.12 wire (`SMSG_TRAINER_LIST`) is a **flat** service list; the client builds a **collapsible
//! tree** on top of it, and `index` is **1-based into that visible tree**, not the wire order.
//! [`UiScript::set_trainer`] takes the flat services (each already carrying its app-resolved
//! [`TrainerService::group_key`]/[`TrainerService::group_name`]) and synthesizes the tree: one
//! **header row per distinct group key**, then that group's services in the group's own order. State
//! is a per-row **colour**, never the grouping key.
//!
//! **The `trainerType` selects everything** — the list finalizer `0x4d8410` picks the row comparator
//! by `ds:0xb73a08` (`dec eax; je` chain @ `0x4d8561`) and the builder `0x4d7560` picks both the
//! group key and the header comparator by the same dword (`0x4d7786`, `0x4d79eb`). Neither the group
//! key nor the sort is one law with a "later refinement" — they are four laws, and 1124 is where
//! benilla stopped applying the class one to all of them:
//!
//! | type | rows | headers | group key |
//! |---|---|---|---|
//! | 0 class, 3 pet | `0x4d85c0` — level → skill value → name → rank | `0x4d7b90`, by name | taught spell's `SkillLine` |
//! | 1 mount ("talent") | `0x4d8850` — **state byte → name** | `0x4d7b90`, `-1` first then by name | `-1` when the service is `used`, else the `SkillLine` |
//! | 2 tradeskill | `0x4d8760` — **skill value → name**, no level key, no rank key | `0x4d7c30`, by **raw key asc** | **1 or 2**, see below |
//!
//! At type 2 the group key is **not a skill line and never resolves one**: the builder defaults it
//! to 2 and sets it to 1 iff the **wire** spell's own `Spell.dbc Effect[0..2]` contains `44`
//! `SKILL_STEP` (`0x4d77b6`), which makes exactly two groups — `TRADESKILL_SERVICE_STEP` ("Development
//! Skills", the profession-learn services) and `TRADESKILL_SERVICE_LEARN` ("Recipes") — and means
//! **no row is ever dropped at a tradeskill trainer**. That partition, not a level key, is what puts
//! "Apprentice Blacksmith" at the top of a blacksmithing trainer: `reqLevel` is inert at type 2.
//! At the other types a service whose group key is `0` (unresolved skill line) is still dropped,
//! matching the client.
//!
//! `GetNumTrainerServices`/every getter index the **visible** rows: headers always show, but a
//! service hides when its state fails the dropdown filter ([`Model::trainer_filter`]) or its group is
//! collapsed ([`Model::trainer_collapsed`]). `Collapse/ExpandTrainerSkillLine(id)` take the **display
//! index of a header row** (`id == 0` = all groups — the collapse-all button); they toggle that
//! group's visibility, never reorder.
//!
//! ## Faithful stubs (no wire data yet — kept so the ported XML runs)
//!
//! `GetTrainerServiceStepReq` returns nil (no tradeskill-step data on the wire), and per-requirement
//! `hasReq`/`met` flags are derived from the service's overall `category` (an unavailable service has
//! an unmet gate) rather than a per-gate wire bit, which 5875 does not send.
//!
//! One thing the client does that this engine does not, named rather than left to be discovered: a
//! group whose row counters go to zero **vanishes entirely, header included** (`0x4d8460`,
//! `0x4d8528`–`0x4d8549`) — which is how a type-1 trainer whose every service is `used` shows one
//! "My Talents" header and no skill-line headers at all. benilla always keeps a header. The case
//! that is *reachable* here (does the state-filter dropdown emptying a group also take its header?)
//! is an open question with wow-re; the build-time case above cannot arise, because benilla builds
//! a group only from services that exist.

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

    /// The row's **state byte** `[+0x30]` — the value `0x4d8ba0` maps to the colour string (`0` →
    /// available, `2` → used, anything else → unavailable). It is a colour on every trainer type but
    /// one: at type 1 it is also the third sort key ([`talent_order`]), which is why this exists
    /// separately from the filter slot it numerically coincides with.
    fn state_key(self) -> u8 {
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
    /// The tree's grouping key, resolved app-side by the **trainer type's own** builder law
    /// (module doc, decision 1124): the taught spell's `SkillLine` at types 0/1/3, or the
    /// `SKILL_STEP` partition's `1`/`2` at type 2. `0` = unresolved: the service is **dropped** from
    /// the tree (the client's `skillLine == 0 → drop`) — reachable only at types 0/1/3.
    pub group_key: u32,
    /// The group's localized display name — the header row's text. `SkillLine.dbc`'s name at types
    /// 0/1/3; the `TRADESKILL_SERVICE_STEP`/`_LEARN` global strings at type 2.
    pub group_name: String,
    /// What the detail-icon hover describes ([`TrainerTooltip`]) — resolved app-side, because the
    /// law reads `Spell.dbc` fields the engine cannot see. Independent of [`Self::texture`].
    pub tooltip: TrainerTooltip,
}

/// One group in the display tree (decisions 0247/1124): the header's key + name and the positions
/// (into [`TrainerState::services`]) of the group's services, pre-sorted by the trainer type's
/// within-group comparator. Synthesized by [`UiScript::set_trainer`] — the app pushes only the flat
/// services.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrainerGroup {
    /// The group key (also the collapse key).
    pub key: u32,
    /// The header's display name.
    pub name: String,
    /// The group's services, as positions into [`TrainerState::services`], in display order.
    pub services: Vec<usize>,
}

/// One open trainer window: the services, the greeting line, and the wire trainer type. Pushed whole
/// by the app; `None` means no trainer is open (the window is closed).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrainerState {
    pub services: Vec<TrainerService>,
    /// The trainer's greeting/title line (`SMSG_TRAINER_LIST`'s trailing string).
    pub greeting: String,
    /// `SMSG_TRAINER_LIST`'s `trainerType` (`ds:0xb73a08`), verbatim — **the** switch: it selects the
    /// row comparator, the header comparator and the group-key law (module doc), and it is what the
    /// two whole-trainer predicates test (`IsTradeskillTrainer 0x4d8ea0`: `== 2`; `IsTalentTrainer
    /// 0x4d8ed0`: `== 1`). 0 class · 1 mount · 2 tradeskill · 3 pet.
    pub trainer_type: u32,
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
                s.groups = build_groups(&s.services, s.trainer_type);
                let live: std::collections::HashSet<u32> = s.groups.iter().map(|g| g.key).collect();
                model.trainer_collapsed.retain(|sl| live.contains(sl));
                model.trainer = Some(s);
            }
        }
    }

    /// **Reset the state filter and the collapse set — one `SMSG_TRAINER_LIST` arriving.**
    ///
    /// Byte-verified (wow-re `system/ui/scratch/trainer-service-suppression.md`, decision 1128): the
    /// list builder writes the filter mask itself on every packet — `0x4d75d9 mov ds:0xb73a1c,3`
    /// (available|unavailable, "already known" OFF), or `5` (available|used) when `trainerType == 1`
    /// — alongside `ds:0xb73a20 = ds:0xb73a24 = 0xffffffff`, which is "no group collapsed". So the
    /// player's filter choice does NOT live in the engine across trainer visits in the reference: it
    /// lives in the saved variable `TRAINER_FILTER_*`, and the window's show handler pushes it back
    /// over this reset (decision 1128; `TrainerFrame.xml`'s `BenillaTrainerFrame_ApplyFilter`).
    ///
    /// This is the **packet** edge, not the content edge — [`Self::set_trainer`] runs on every
    /// snapshot change (an item template landing, a name resolving), and the reference's repaint path
    /// `0x4d7d40` does not touch either mask. Reset there and a filter would evaporate mid-window.
    pub fn reset_trainer_filter(&mut self, trainer_type: u32) {
        let mut model = self.model_mut();
        // Mask 5 at a mount/"talent" trainer: available + already-known, which is what makes a
        // known mount visible under its "My Talents" header at all (decision 1124's group -1).
        model.trainer_filter = if trainer_type == TRAINER_TYPE_MOUNT {
            [true, false, true]
        } else {
            [true, true, false]
        };
        model.trainer_collapsed.clear();
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

/// `SMSG_TRAINER_LIST`'s tradeskill `trainerType` (module doc's table).
const TRAINER_TYPE_TRADESKILL: u32 = 2;
/// `SMSG_TRAINER_LIST`'s mount `trainerType` — the one the client's own vocabulary calls "talent"
/// (`IsTalentTrainer 0x4d8ed0` tests it).
const TRAINER_TYPE_MOUNT: u32 = 1;

/// The already-known group a **type 1** trainer buckets its `used` services into — the client's
/// signed `-1` group key (`0x4d77e8`), whose header takes the `KNOWN_TALENTS_HEADER` global string
/// ("My Talents", 1.12.1 enUS). Modelled as `u32::MAX` because the key is otherwise a skill-line id;
/// the header comparator `0x4d7b90` tests for it explicitly and sorts it **first**, ahead of the
/// name ordering that ranks every other header.
pub const TRAINER_GROUP_KNOWN: u32 = u32::MAX;

/// A service's required-skill value (`[+0x1c]`, the wire's `reqSkillValue`) — `0` when the service
/// carries no skill gate.
fn skill_value(s: &TrainerService) -> u32 {
    s.skill_req.as_ref().map_or(0, |r| r.rank)
}

/// The within-group comparator for a **class** (type 0) or **pet** (type 3) trainer — keys ④–⑦ of
/// the verified `0x4d85c0`: required level (`[+0x14]` byte) → required-skill value (`[+0x1c]` u32) →
/// localized name → localized rank/subtext, all ascending. Pet is not special-cased anywhere in the
/// selection chain at `0x4d8561`; it lands here with class.
fn class_order(a: &TrainerService, b: &TrainerService) -> std::cmp::Ordering {
    let name = |s: &TrainerService| s.name.clone().unwrap_or_default();
    let rank = |s: &TrainerService| s.subtext.clone().unwrap_or_default();
    a.level_req
        .cmp(&b.level_req)
        .then_with(|| skill_value(a).cmp(&skill_value(b)))
        .then_with(|| collate(&name(a), &name(b)))
        .then_with(|| collate(&rank(a), &rank(b)))
}

/// The within-group comparator for a **tradeskill** trainer (type 2) — the whole of `0x4d8760`'s
/// row half: required-skill value **ascending** (`0x4d87d9`/`0x4d8801`), then the localized name.
///
/// The two keys it pointedly does *not* have are the interesting ones, and both were in benilla's
/// output until 1124: there is **no `[+0x14]` required-level key** (which is what used to sink the
/// profession-learn row — the only row with a level — to the bottom of every profession trainer),
/// and **no `[+0x204]` rank tie-break**.
fn tradeskill_order(a: &TrainerService, b: &TrainerService) -> std::cmp::Ordering {
    let name = |s: &TrainerService| s.name.clone().unwrap_or_default();
    skill_value(a)
        .cmp(&skill_value(b))
        .then_with(|| collate(&name(a), &name(b)))
}

/// The within-group comparator for a **mount** trainer (type 1, the client's "talent") — the whole
/// of `0x4d8850`'s row half: the **state byte** `[+0x30]` ascending (`0x4d88f1`, so available →
/// unavailable → used), then the localized name. The one comparator on which the green/red/gray is a
/// sort key rather than only a colour; like the tradeskill one it has no level, skill-value or rank
/// key.
fn talent_order(a: &TrainerService, b: &TrainerService) -> std::cmp::Ordering {
    let name = |s: &TrainerService| s.name.clone().unwrap_or_default();
    a.category
        .state_key()
        .cmp(&b.category.state_key())
        .then_with(|| collate(&name(a), &name(b)))
}

/// Build the display tree from the flat services (decisions 0247/1124), by the trainer type's own
/// three laws:
///
/// * **group** on the app-resolved [`TrainerService::group_key`], dropping the unresolved `0` (which
///   a type-2 list never produces — its partition is total);
/// * **order each group's services** with that type's row comparator;
/// * **order the headers** by raw key ascending at type 2 (`0x4d7c30`: one key, no name, no
///   tie-break), else by localized name with the key breaking a name tie (`0x4d7b90`).
///
/// The service positions index back into the unchanged `services` slice, so every getter still
/// resolves a row to its full service data.
fn build_groups(services: &[TrainerService], trainer_type: u32) -> Vec<TrainerGroup> {
    let mut map: std::collections::HashMap<u32, TrainerGroup> = std::collections::HashMap::new();
    for (i, s) in services.iter().enumerate() {
        if s.group_key == 0 {
            continue;
        }
        map.entry(s.group_key)
            .or_insert_with(|| TrainerGroup {
                key: s.group_key,
                name: s.group_name.clone(),
                services: Vec::new(),
            })
            .services
            .push(i);
    }
    let mut groups: Vec<TrainerGroup> = map.into_values().collect();
    let order = match trainer_type {
        TRAINER_TYPE_TRADESKILL => tradeskill_order,
        TRAINER_TYPE_MOUNT => talent_order,
        _ => class_order,
    };
    for g in &mut groups {
        g.services
            .sort_by(|&a, &b| order(&services[a], &services[b]));
    }
    if trainer_type == TRAINER_TYPE_TRADESKILL {
        groups.sort_by_key(|g| g.key);
    } else {
        // `0x4d7b90`: the `-1` (already-known) group first, then localized name. Its second key —
        // `hdr[+0x24] == 0` first, between those two — is uncarved and unreachable here: benilla's
        // headers are one per distinct skill line, so nothing ties on the name that it would break.
        groups.sort_by(|a, b| {
            (a.key != TRAINER_GROUP_KNOWN)
                .cmp(&(b.key != TRAINER_GROUP_KNOWN))
                .then_with(|| collate(&a.name, &b.name))
                .then_with(|| a.key.cmp(&b.key))
        });
    }
    groups
}

/// The visible rows in display order (decisions 0247/1124). Per group, in order: the header — **only
/// if at least one of the group's services passes the state filter** — then, when the group is not
/// collapsed, those services. The Lua's 1-based `index` is a position in *this* list.
///
/// **The filter and the collapse are deliberately asymmetric**, and that asymmetry is structural in
/// the client rather than incidental. The finalizer builds a per-group flag `hdr[+0x1c]` by walking
/// the group's per-state member counts through the **live** state mask `ds:0xb73a1c` (`0x4d8431`,
/// `0x4d8447`), and the hide it drives at `0x4d8535` has **no header-row exemption** — so a group
/// every one of whose rows the dropdown hides disappears entirely, header included, and unchecking
/// every box empties the window (`GetNumTrainerServices() == 0`). The collapse test three
/// instructions later (`0x4d8541`) *does* exempt headers (`0x4d853d`) and reads a different field
/// (`hdr[+0x20]`), so a collapsed group keeps its header — which is what makes it re-expandable.
/// benilla had both cases keeping the header, with a test asserting it; 1124 inverted the filter
/// half.
fn rows(model: &Model) -> Vec<Row> {
    let Some(t) = model.trainer.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (gi, g) in t.groups.iter().enumerate() {
        let mut shown = g
            .services
            .iter()
            .copied()
            .filter(|&si| model.trainer_filter[t.services[si].category.filter_slot()])
            .peekable();
        if shown.peek().is_none() {
            continue;
        }
        out.push(Row::Header(gi));
        if model.trainer_collapsed.contains(&g.key) {
            continue;
        }
        out.extend(shown.map(Row::Service));
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
            .map(|t| t.groups.iter().map(|g| g.key).collect())
            .unwrap_or_default()
    } else {
        match rows(model).get(id - 1) {
            Some(Row::Header(gi)) => model
                .trainer
                .as_ref()
                .and_then(|t| t.groups.get(*gi))
                .map(|g| g.key)
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
                    let expanded = !model.trainer_collapsed.contains(&g.key);
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

    // IsTradeskillTrainer() → 1/nil for the whole trainer (drives the tradeskill-vs-class layout):
    // `0x4d8ea0` tests trainerType == 2.
    g.set(
        "IsTradeskillTrainer",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(era_bool(model.trainer.as_ref().is_some_and(|t| {
                t.trainer_type == TRAINER_TYPE_TRADESKILL
            })))
        })?,
    )?;

    // IsTalentTrainer() → 1/nil: `0x4d8ed0` tests trainerType == 1 — vmangos's MOUNT trainers, which
    // the client's own vocabulary calls "talent" (decision 1124). It used to return a hardcoded nil
    // on the belief that benilla drives no such trainer; the shipped world has 23 of them.
    g.set(
        "IsTalentTrainer",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(era_bool(
                model.trainer.as_ref().is_some_and(|t| t.trainer_type == 1),
            ))
        })?,
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
mod tests;
