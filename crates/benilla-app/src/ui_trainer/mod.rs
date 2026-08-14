//! The app-side **trainer feed** (decision 0237 phase 3) — the inward half of the trainer seam
//! around [`benilla_ui::script`]'s `trainer` module, the twin of [`crate::ui_merchant`]'s merchant
//! feed.
//!
//! The net bridge fills [`TrainerOpen`] from the wire (`SMSG_TRAINER_LIST` → the trainer's services +
//! greeting, reached through the gossip trainer option). Each frame [`feed_trainer`] resolves each
//! wire [`TrainerSpell`] to a Lua-facing [`TrainerService`] — name/subtext from the spell catalog
//! (`Spell.dbc`, loaded whole at startup), the **icon** from its own byte-verified law
//! ([`service_icon`], which at a *tradeskill* trainer fronts the taught recipe's created item and so
//! does need the ask-once item-template query the item rows use), the skill-requirement name from
//! the skill-line catalog, the green/red/gray state straight off the wire
//! `state` byte — pushes the snapshot ([`UiScript::set_trainer`]), and fires `TRAINER_SHOW` on open /
//! `TRAINER_UPDATE` on a content change / `TRAINER_CLOSED` on clear. [`drain_trainer`] pulls the Lua
//! intents back out: the Train button's `BuyTrainerService` → `CMSG_TRAINER_BUY_SPELL` for the open
//! trainer, and `CloseTrainer` → a local clear (vanilla's client-side close sends no packet). A
//! successful buy answers `SMSG_TRAINER_BUY_SUCCEEDED` — the spell itself lands via
//! `SMSG_LEARNED_SPELL` (already in the book), and the net apply re-requests the list to repaint the
//! bought row green→gray; a refusal (`SMSG_TRAINER_BUY_FAILED`) stages into [`TrainerErrors`] for the
//! feed to surface on the window's red error line.
//!
//! The standardized NPC-session range guard ([`crate::ui_session`]) client-side-closes the window
//! when the player walks out of the trainer's service range (or the trainer despawns) — the same
//! `CloseTrainer` clear.

use std::collections::HashSet;

use benilla_formats::{SkillLineCatalog, SpellCatalog};
use benilla_protocol::messages::TrainerSpell;
use bevy::prelude::*;

use benilla_ui::script::{
    ScriptValue, TrainerAbilityReq, TrainerService, TrainerServiceCategory, TrainerSkillReq,
    TrainerState, UiScript,
};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::names::NameCache;
use crate::net::{ClientCommand, NetCommands};
use crate::ui_action::{PlayerActions, Spells};
use crate::ui_script::UiInput;
use crate::ui_session::{close_npc_session_out_of_range, npc_switched, NpcSession};
use crate::ui_spellbook::SkillLines;

/// `SMSG_TRAINER_LIST`'s `trainer_type` for a tradeskill trainer (0 class · 1 mount · 2 tradeskill ·
/// 3 pet). Three separate laws fork on it here — the **icon** ([`service_icon`]), the **group key**
/// ([`service_group`]), and the Era `IsTrainerServiceTradeSkill` flag, which remains a whole-trainer
/// approximation (per-service typing from the spell's effects is a later refinement; the reference
/// window's Lua never calls it).
const TRAINER_TYPE_TRADESKILL: u32 = 2;
/// `SMSG_TRAINER_LIST`'s `trainer_type` for a mount trainer — the type the client's own vocabulary
/// calls "talent" (`IsTalentTrainer 0x4d8ed0`), and the one whose grouping folds already-known
/// services into a "My Talents" bucket ([`service_group`], decision 1124).
const TRAINER_TYPE_MOUNT: u32 = 1;

/// The open trainer, filled by the net bridge ([`crate::net`]) and read by [`feed_trainer`]. Holds
/// the trainer guid and its services exactly as the wire delivered them (`SMSG_TRAINER_LIST`), plus
/// the window-framing type and the greeting title. Cleared on a client-side close and on disconnect.
#[derive(Resource, Default)]
pub(crate) struct TrainerOpen {
    /// The trainer whose window is open; `None` = no trainer open.
    pub(crate) trainer: Option<u64>,
    /// The wire services (order = 1-based display order).
    pub(crate) services: Vec<TrainerSpell>,
    /// The window-framing kind (0 class · 1 mount · 2 tradeskill · 3 pet).
    pub(crate) trainer_type: u32,
    /// The trainer's greeting line (`SMSG_TRAINER_LIST`'s trailing string).
    pub(crate) greeting: String,
    /// A `SMSG_TRAINER_LIST` has landed and the feed has not yet handed it to the engine. Drives the
    /// engine's **per-packet** filter/collapse reset ([`UiScript::reset_trainer_filter`]) — the
    /// reference's builder rewrites both masks on every list packet, and only on a packet (decision
    /// 1128). It is not the same edge as a snapshot change: those re-push the same list.
    pub(crate) fresh_list: bool,
}

impl TrainerOpen {
    /// Open (or replace) the window with a trainer's freshly-listed services.
    pub(crate) fn open(
        &mut self,
        trainer: u64,
        trainer_type: u32,
        services: Vec<TrainerSpell>,
        greeting: String,
    ) {
        self.trainer = Some(trainer);
        self.trainer_type = trainer_type;
        self.services = services;
        self.greeting = greeting;
        self.fresh_list = true;
    }

    /// Close the open window (a client-side close). Keeps nothing — a re-open re-lists.
    pub(crate) fn clear(&mut self) {
        self.trainer = None;
        self.services.clear();
        self.trainer_type = 0;
        self.greeting.clear();
        self.fresh_list = false;
    }

    /// Disconnect: drop the open window (mirrors the gossip/merchant session clears).
    pub(crate) fn clear_session(&mut self) {
        self.clear();
    }
}

/// Trainer purchase refusals (`SMSG_TRAINER_BUY_FAILED`), staged by the net apply for the feed to
/// surface on the window's red error line — the merchant [`crate::ui_merchant::MerchantErrors`]
/// twin. Each entry is a [`benilla_protocol::messages::train_fail`] code.
#[derive(Resource, Default)]
pub(crate) struct TrainerErrors(pub Vec<u32>);

pub(crate) struct UiTrainerPlugin;

impl Plugin for UiTrainerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TrainerOpen>()
            .init_resource::<TrainerErrors>()
            .add_systems(
                Update,
                (
                    // Range-close before the feed so the clear turns into TRAINER_CLOSED the same frame;
                    // push before the input pass so an open/close is on screen the same frame; drain
                    // after it (mirrors ui_merchant/ui_gossip).
                    close_npc_session_out_of_range::<TrainerOpen>.before(feed_trainer),
                    feed_trainer.before(UiInput),
                    drain_trainer.after(UiInput),
                ),
            );
    }
}

/// The red error-line text for a trainer refusal (`SMSG_TRAINER_BUY_FAILED`'s
/// [`benilla_protocol::messages::train_fail`] code). The money case is the exact enUS `GlobalString`
/// (`ERR_NOT_ENOUGH_MONEY`); the skill/unavailable cases are best-effort English — the real client
/// builds these in C++, so their exact wording is a wow-re confirm item (rarely reached: the Train
/// button is disabled unless the service is available and affordable).
fn train_error_text(code: u32) -> String {
    use benilla_protocol::messages::train_fail;
    match code {
        train_fail::NOT_ENOUGH_MONEY => "You don't have enough money.",
        train_fail::NOT_ENOUGH_SKILL => "You do not have the required skill.",
        _ => "That trainer service is not available.",
    }
    .to_string()
}

/// Resolve one wire [`TrainerSpell`] into the Lua-facing [`TrainerService`]: name/subtext/icon from
/// the spell catalog (`None` only before `Spell.dbc` has loaded — the row shows a placeholder), the
/// skill-req name from the skill-line catalog (falling back to `Skill <id>` if it hasn't loaded),
/// the ability-req names + rank from the same spell catalog, the state/cost/gates straight off the
/// wire. `known` is the player's known-spell set ([`PlayerActions::spells`]) — each prerequisite
/// ability is coloured by whether the player already knows that specific spell (see below).
#[allow(clippy::too_many_arguments)] // the resolver's full catalog set
fn resolve_service(
    wire: &TrainerSpell,
    trainer_type: u32,
    spells: &SpellCatalog,
    skill_lines: Option<&SkillLineCatalog>,
    known: &HashSet<u32>,
    icons: Option<&ItemDisplays>,
    items: &mut Items,
    commands: &NetCommands,
) -> TrainerService {
    // The trainer offers a LEARN wrapper (decision 0247); the ability it teaches is the taught
    // spell, and the tree GROUPS by that hop (`0x4d7c60` → `[skillrec+4]`). It is the only thing
    // that hops: the row's **displayed name and subtext are the WIRE spell's own** `Spell.dbc`
    // columns — `GetTrainerServiceInfo` reads `row[+0]` for both returns (`0x4d8aa0` → `[+0x1e0]`,
    // `0x4d8b50` → `[+0x204]`), with no `EffectTriggerSpell` deref anywhere in either body. 0247
    // recorded the display as hopping too; decision 1124 refutes that at the bytes, and it is not a
    // cosmetic difference: 1607 of the 4711 shipped learn wrappers (34.1 %) disagree with what they
    // teach, and that set is *every profession-learn row* — spell 2020 is "Apprentice Blacksmith"
    // with no subtext where the taught 2018 is "Blacksmithing"/"Apprentice". Pet rows agree on both
    // columns, which is how the wrong hop survived this long.
    // `wire.spell` (the wrapper) stays the buy id below (CMSG_TRAINER_BUY_SPELL names it); a spell
    // that teaches nothing (a plain ability, or before Spell.dbc loads) resolves to itself.
    let taught = spells.learned_spell(wire.spell).unwrap_or(wire.spell);
    let display = spells.get(wire.spell);
    let cat = category(wire.state);
    // The SKILL gate has no per-gate "met" bit on the 5875 wire, so approximate it from the service's
    // overall category (an unavailable service has some unmet gate). The real client computes the
    // skill gate locally too — player skill value ≥ required, like the level gate the XML already
    // checks with `UnitLevel` — so a faithful per-gate skill check waits only on threading the
    // player's skill values here; the ability gate below already does its own per-gate check, so a
    // skill-gated service is the one remaining coarse case (decision 0253).
    let skill_met = cat != TrainerServiceCategory::Unavailable;
    let skill_req = (wire.req_skill != 0).then(|| TrainerSkillReq {
        name: skill_lines
            .and_then(|c| c.line(wire.req_skill))
            .map(|l| l.name.clone())
            .unwrap_or_else(|| format!("Skill {}", wire.req_skill)),
        rank: wire.req_skill_value,
        met: skill_met,
    });
    // Each prerequisite ability is coloured by whether the player already KNOWS that specific spell —
    // the byte-verified real-client mechanism (wow-re `system/ui/scratch/trainer-requirement.md`):
    // `GetTrainerServiceAbilityReq`'s hasReq is `IsSpellKnown(reqSpellId)`, evaluated per-requirement
    // and INDEPENDENT of the service's overall category — so a spell gated only by LEVEL still shows
    // its already-learned prev-rank prerequisite WHITE, not red. The req id is a real ability id (not
    // a learn wrapper — verified there too), so there's no hop: look it up directly. The name carries
    // its rank exactly as the client does — `"Name (Rank)"` when the spell has a rank subtext, else
    // the bare name (the client's `"%s (%s)"`). The client also ORs `KnownHigherRank`; benilla has no
    // rank chain, and sequential trainer ranks never reach that clause, so the direct known-check
    // covers every real case.
    let ability_reqs = wire
        .req_spells
        .iter()
        .filter(|&&s| s != 0)
        .map(|&s| {
            let name = match spells.get(s) {
                Some(d) => match d.rank.as_deref() {
                    Some(rank) if !rank.is_empty() => format!("{} ({})", d.name, rank),
                    _ => d.name.clone(),
                },
                None => format!("Spell {s}"),
            };
            TrainerAbilityReq {
                name,
                met: known.contains(&s),
            }
        })
        .collect();
    // The tree's grouping key — its own byte-verified law, and per trainer type ([`service_group`],
    // decision 1124). Only the skill-line arm (types 0/1/3) can fail: the wire wrapper id itself is
    // never in `SkillLineAbility`, so that arm MUST go through the taught-spell hop above, and an
    // unresolved `0` drops the service from the tree exactly as the client's builder does. Log the
    // genuine miss (catalog present but no line) so DBC gaps surface rather than silently swallowing
    // a service the server offered. The tradeskill arm resolves no line at all and cannot miss.
    let (group_key, group_name) =
        service_group(wire.spell, taught, trainer_type, cat, spells, skill_lines);
    if group_key == 0 && skill_lines.is_some() {
        debug!(
            "ui_trainer: trainer spell {} (teaches {taught}) has no skill line — dropped from the tree",
            wire.spell
        );
    }
    TrainerService {
        spell_id: wire.spell,
        name: display.map(|d| d.name.clone()),
        subtext: display.and_then(|d| d.rank.clone()),
        // The icon is its own byte-verified law over the WIRE spell ([`service_icon`]) — as, since
        // 1124, are the name and subtext above. Only the GROUP key still hops to the taught spell,
        // and only because the wrapper is not in `SkillLineAbility` at all.
        texture: service_icon(wire.spell, trainer_type, spells, icons, items, commands),
        // The detail pane's description body, left empty by design. The real
        // `GetTrainerServiceDescription` returns the spell's *tooltip* — `Spell.dbc`'s Description
        // with its `$s1`/`$o1`/`$d`/`$a1` tokens substituted from the spell's effect base points,
        // duration, and radius. Feeding the raw column would render the unsubstituted tokens
        // (broken-looking, not faithful), so descriptions wait on a spell-tooltip token engine —
        // its own arc, shared with the spellbook's hover tooltips — not a lone `SpellCatalog` column.
        description: String::new(),
        cost: wire.cost,
        prof_first_rank: wire.is_primary_prof_first_rank,
        category: cat,
        level_req: u32::from(wire.req_level),
        skill_req,
        ability_reqs,
        is_trade_skill: trainer_type == TRAINER_TYPE_TRADESKILL,
        group_key,
        group_name,
        tooltip: service_tooltip(wire.spell, spells),
    }
}

/// Build the Lua-facing snapshot from [`TrainerOpen`] + the spell/skill catalogs — `None` when no
/// trainer is open.
#[allow(clippy::too_many_arguments)] // the resolver's full catalog set
fn snapshot(
    open: &TrainerOpen,
    spells: &SpellCatalog,
    skill_lines: Option<&SkillLineCatalog>,
    known: &HashSet<u32>,
    icons: Option<&ItemDisplays>,
    items: &mut Items,
    commands: &NetCommands,
) -> Option<TrainerState> {
    open.trainer?;
    Some(TrainerState {
        greeting: open.greeting.clone(),
        trainer_type: open.trainer_type,
        services: open
            .services
            .iter()
            .map(|w| {
                resolve_service(
                    w,
                    open.trainer_type,
                    spells,
                    skill_lines,
                    known,
                    icons,
                    items,
                    commands,
                )
            })
            .collect(),
        // The engine synthesizes the tree in `set_trainer` — the app pushes only the flat services
        // (each carrying its resolved group key above).
        groups: Vec::new(),
    })
}

/// Push the current trainer into the VM and fire the show/update/close events on a transition (or a
/// content change). Diffed against a `Local` memory, exactly like the gossip/merchant feeds. A
/// different trainer while the window is already open is a real close+open (the client's `ShowUIPanel`
/// early-returns when visible, so the open sound only re-plays after a hide — decision 0096).
#[allow(clippy::too_many_arguments)]
fn feed_trainer(
    script: Option<NonSendMut<UiScript>>,
    // ResMut only to consume the fresh-packet latch below — the feed never authors trainer content.
    mut open: ResMut<TrainerOpen>,
    actions: Res<PlayerActions>,
    spells: Option<Res<Spells>>,
    skill_lines: Option<Res<SkillLines>>,
    // A tradeskill trainer's rows front the CREATED ITEM's icon, so the feed needs the ask-once
    // template cache + `ItemDisplayInfo.dbc` — the tradeskill window's own pair ([`service_icon`]).
    icons: Option<Res<ItemDisplays>>,
    mut items: ResMut<Items>,
    mut errors: ResMut<TrainerErrors>,
    commands: Res<NetCommands>,
    mut names: ResMut<NameCache>,
    mut last: Local<crate::ui_script::VmMemo<Option<TrainerState>>>,
    mut last_trainer: Local<crate::ui_script::VmMemo<Option<u64>>>,
    mut last_name: Local<crate::ui_script::VmMemo<Option<String>>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let last = last.get(&script);
    let last_trainer = last_trainer.get(&script);
    let last_name = last_name.get(&script);
    // Refusals surface as the client's red error line (the merchant/equip/cast path's exact shape).
    for code in errors.0.drain(..) {
        script.fire_event(
            "UI_ERROR_MESSAGE",
            vec![ScriptValue::Str(train_error_text(code))],
        );
    }
    // Nothing to resolve a name/icon from yet — try again once Spell.dbc lands.
    let Some(spells) = spells.as_deref() else {
        return;
    };
    // The tree groups by skill line (decision 0247), so the skill-line catalog is required, not
    // optional: without it every service resolves to skill_line 0 and drops. A trainer only opens
    // well after world-entry, by when both DBCs have loaded, so this gate never actually delays a
    // real window — it just refuses to render an all-dropped empty tree.
    let Some(skill_lines) = skill_lines.as_deref() else {
        return;
    };
    // A new list packet resets the state filter and the collapse set in the engine, exactly as the
    // reference's builder does (decision 1128) — before the snapshot goes in, so `TRAINER_SHOW`
    // finds the reset mask and the window's own show handler pushes the SAVED filter back over it.
    if open.fresh_list {
        script.reset_trainer_filter(open.trainer_type);
        open.fresh_list = false;
    }
    let fresh = snapshot(
        &open,
        &spells.catalog,
        Some(&skill_lines.catalog),
        &actions.spells,
        icons.as_deref(),
        &mut items,
        &commands,
    );
    // The trainer's name resolves through the NameCache (a creature-name query, ask-once — the
    // real client's `UnitName("npc")`). `None`/empty while in flight; the title shows the static
    // "Trainer" until it lands, then re-fires TRAINER_UPDATE with the name as arg1 (the diff below
    // tracks the name too, so a name-only change still repaints the title). It rides an event arg
    // rather than a `TrainerState` field so no benilla-ui engine change is needed — the merchant
    // vendor-name pattern.
    let trainer_name = open
        .trainer
        .and_then(|g| names.resolve(g, &commands).map(str::to_string));
    let name_changed = *last_name != trainer_name;
    let switched = npc_switched(*last_trainer, open.trainer);
    if fresh == *last && !name_changed && !switched {
        return;
    }
    script.set_trainer(fresh.clone());
    let name_arg = || vec![ScriptValue::Str(trainer_name.clone().unwrap_or_default())];
    if switched {
        // Close the old trainer, open the new: the frame hides then shows. The TRAINER_CLOSED routes
        // through the window's OnHide → CloseTrainer, which queues a close intent — consume it here so
        // the drain does NOT clear the trainer we just re-opened to (the merchant switch pattern).
        script.fire_event("TRAINER_CLOSED", vec![]);
        script.fire_event("TRAINER_SHOW", name_arg());
        let _ = script.take_trainer_close();
    } else {
        match (&*last, &fresh) {
            (None, Some(_)) => script.fire_event("TRAINER_SHOW", name_arg()),
            (Some(_), Some(_)) => script.fire_event("TRAINER_UPDATE", name_arg()),
            (Some(_), None) => script.fire_event("TRAINER_CLOSED", vec![]),
            (None, None) => {}
        }
    }
    *last = fresh;
    *last_trainer = open.trainer;
    *last_name = trainer_name;
}

/// The trainer window is an NPC session: the standardized range guard ([`crate::ui_session`])
/// client-side-closes it — the exact `CloseTrainer` clear — when the player walks out of the
/// trainer's service range or the trainer despawns.
impl NpcSession for TrainerOpen {
    fn npc(&self) -> Option<u64> {
        self.trainer
    }

    fn close(&mut self) {
        self.clear();
    }
}

/// Drain the Lua intents: the Train button's buys (`BuyTrainerService` queues the chosen row's spell
/// id) → `CMSG_TRAINER_BUY_SPELL` for the open trainer; a close → a local clear (no packet, vanilla).
/// The server answers a buy with `SMSG_TRAINER_BUY_SUCCEEDED` (→ the spell lands via
/// `SMSG_LEARNED_SPELL`, and the apply re-requests the list to repaint the row gray) or
/// `SMSG_TRAINER_BUY_FAILED` (→ the window's error line via [`TrainerErrors`]).
fn drain_trainer(
    script: Option<NonSendMut<UiScript>>,
    mut open: ResMut<TrainerOpen>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    for spell_id in script.take_trainer_buys() {
        let Some(trainer) = open.trainer else {
            debug!(
                "ui_trainer: BuyTrainerService(spell {spell_id}) with no open trainer — ignored"
            );
            continue;
        };
        debug!("ui_trainer: buy service (trainer {trainer:#x}, spell {spell_id})");
        let _ = commands
            .0
            .send(ClientCommand::TrainerBuySpell { trainer, spell_id });
    }
    if script.take_trainer_close() {
        debug!("ui_trainer: client-side close (no packet)");
        open.clear();
    }
}

mod law;
use law::{category, service_group, service_icon, service_tooltip};

#[cfg(test)]
mod tests;
