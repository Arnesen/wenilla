//! The app-side **trainer feed** (decision 0237 phase 3) — the inward half of the trainer seam
//! around [`benilla_ui::script`]'s `trainer` module, the twin of [`crate::ui_merchant`]'s merchant
//! feed.
//!
//! The net bridge fills [`TrainerOpen`] from the wire (`SMSG_TRAINER_LIST` → the trainer's services +
//! greeting, reached through the gossip trainer option). Each frame [`feed_trainer`] resolves each
//! wire [`TrainerSpell`] to a Lua-facing [`TrainerService`] — name/subtext/icon from the spell
//! catalog (`Spell.dbc`, loaded whole at startup, so no ask-once query like the item rows need), the
//! skill-requirement name from the skill-line catalog, the green/red/gray state straight off the wire
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
use benilla_protocol::messages::{trainer_spell_state, TrainerSpell};
use bevy::prelude::*;

use benilla_ui::script::{
    ScriptValue, TrainerAbilityReq, TrainerService, TrainerServiceCategory, TrainerSkillReq,
    TrainerState, UiScript,
};

use crate::names::NameCache;
use crate::net::{ClientCommand, NetCommands};
use crate::ui_action::{PlayerActions, Spells};
use crate::ui_script::UiInput;
use crate::ui_session::{close_npc_session_out_of_range, npc_switched, NpcSession};
use crate::ui_spellbook::SkillLines;

/// `SMSG_TRAINER_LIST`'s `trainer_type` for a tradeskill trainer (0 class · 1 mount · 2 tradeskill ·
/// 3 pet). Used only to classify a service as tradeskill-vs-learn-spell for the Era
/// `IsTrainerServiceTradeSkill` surface — a whole-trainer approximation (per-service typing from the
/// spell's effects is a later refinement).
const TRAINER_TYPE_TRADESKILL: u32 = 2;

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
    }

    /// Close the open window (a client-side close). Keeps nothing — a re-open re-lists.
    pub(crate) fn clear(&mut self) {
        self.trainer = None;
        self.services.clear();
        self.trainer_type = 0;
        self.greeting.clear();
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

/// The green/red/gray colour a wire `state` byte maps to (decision 0237): GRAY → known, GREEN →
/// learnable, everything else (RED + any unexpected value) → gated.
fn category(state: u8) -> TrainerServiceCategory {
    match state {
        trainer_spell_state::GRAY => TrainerServiceCategory::Used,
        trainer_spell_state::GREEN => TrainerServiceCategory::Available,
        _ => TrainerServiceCategory::Unavailable,
    }
}

/// Resolve one wire [`TrainerSpell`] into the Lua-facing [`TrainerService`]: name/subtext/icon from
/// the spell catalog (`None` only before `Spell.dbc` has loaded — the row shows a placeholder), the
/// skill-req name from the skill-line catalog (falling back to `Skill <id>` if it hasn't loaded),
/// the ability-req names + rank from the same spell catalog, the state/cost/gates straight off the
/// wire. `known` is the player's known-spell set ([`PlayerActions::spells`]) — each prerequisite
/// ability is coloured by whether the player already knows that specific spell (see below).
fn resolve_service(
    wire: &TrainerSpell,
    trainer_type: u32,
    spells: &SpellCatalog,
    skill_lines: Option<&SkillLineCatalog>,
    known: &HashSet<u32>,
) -> TrainerService {
    // The trainer offers a LEARN wrapper (decision 0247); the ability it teaches — with the skill
    // line the tree groups by, and the real display name/icon — is the taught spell. Resolve it once
    // for both. `wire.spell` (the wrapper) stays the buy id below (CMSG_TRAINER_BUY_SPELL names it).
    // A spell that teaches nothing (a plain ability, or before Spell.dbc loads) resolves to itself.
    let taught = spells.learned_spell(wire.spell).unwrap_or(wire.spell);
    let display = spells.get(taught);
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
    // The tree's grouping key (decision 0247): the TAUGHT spell's skill line (`SkillLineAbility.dbc`
    // → `SkillLine.dbc` name) — the wire wrapper id itself is never in `SkillLineAbility`, so grouping
    // MUST go through the hop above. `0` when unresolved — the engine drops such a service from the
    // tree, matching the client. A resolved id with a missing name falls back to `Skill <id>`. We log
    // a genuine miss (catalog present but no line) so DBC gaps surface rather than silently swallowing
    // a service the server offered.
    let skill_line = skill_lines
        .and_then(|c| c.spell_to_line(taught))
        .unwrap_or(0);
    if skill_line == 0 && skill_lines.is_some() {
        debug!(
            "ui_trainer: trainer spell {} (teaches {taught}) has no skill line — dropped from the tree",
            wire.spell
        );
    }
    let skill_line_name = if skill_line != 0 {
        skill_lines
            .and_then(|c| c.line(skill_line))
            .map(|l| l.name.clone())
            .unwrap_or_else(|| format!("Skill {skill_line}"))
    } else {
        String::new()
    };
    TrainerService {
        spell_id: wire.spell,
        name: display.map(|d| d.name.clone()),
        subtext: display.and_then(|d| d.rank.clone()),
        texture: display.and_then(|d| d.icon.clone()),
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
        skill_line,
        skill_line_name,
    }
}

/// Build the Lua-facing snapshot from [`TrainerOpen`] + the spell/skill catalogs — `None` when no
/// trainer is open.
fn snapshot(
    open: &TrainerOpen,
    spells: &SpellCatalog,
    skill_lines: Option<&SkillLineCatalog>,
    known: &HashSet<u32>,
) -> Option<TrainerState> {
    open.trainer?;
    Some(TrainerState {
        greeting: open.greeting.clone(),
        is_tradeskill: open.trainer_type == TRAINER_TYPE_TRADESKILL,
        services: open
            .services
            .iter()
            .map(|w| resolve_service(w, open.trainer_type, spells, skill_lines, known))
            .collect(),
        // The engine synthesizes the skill-line tree in `set_trainer` — the app pushes only the flat
        // services (each carrying its resolved skill line above).
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
    open: Res<TrainerOpen>,
    actions: Res<PlayerActions>,
    spells: Option<Res<Spells>>,
    skill_lines: Option<Res<SkillLines>>,
    mut errors: ResMut<TrainerErrors>,
    commands: Res<NetCommands>,
    mut names: ResMut<NameCache>,
    mut last: Local<Option<TrainerState>>,
    mut last_trainer: Local<Option<u64>>,
    mut last_name: Local<Option<String>>,
) {
    let Some(mut script) = script else {
        return;
    };
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
    let fresh = snapshot(
        &open,
        &spells.catalog,
        Some(&skill_lines.catalog),
        &actions.spells,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// An empty spell catalog (`Spell.dbc` not resolved) — the resolver's name/icon lookups miss.
    fn empty_catalog() -> SpellCatalog {
        SpellCatalog::from_displays(HashMap::new())
    }

    fn wire(spell: u32, state: u8, cost: u32, req_level: u8, req_skill: u32) -> TrainerSpell {
        TrainerSpell {
            spell,
            state,
            cost,
            can_learn_primary_prof: false,
            is_primary_prof_first_rank: false,
            req_level,
            req_skill,
            req_skill_value: if req_skill != 0 { 100 } else { 0 },
            req_spells: [0, 0, 0],
        }
    }

    /// The learn-spell hop end-to-end on real 5875 data (decision 0247): a warrior trainer sends the
    /// LEARN wrapper 1605 ("learn Heroic Strike"), which is not in SkillLineAbility — resolve_service
    /// must hop through the taught spell (78) to group it under Arms (26) and show its name, while the
    /// BUY id stays the wrapper (1605) the server expects. This is the exact failure that emptied the
    /// tree. Skips without client data.
    #[test]
    fn resolve_hops_the_learn_wrapper_to_the_taught_ability() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let spells = benilla_formats::load_spell_catalog(&mut chain).expect("load Spell");
        let skills =
            benilla_formats::load_skill_line_catalog(&mut chain).expect("load skill lines");

        let svc = resolve_service(
            &wire(1605, trainer_spell_state::GREEN, 10, 1, 0),
            0,
            &spells,
            Some(&skills),
            &HashSet::new(),
        );
        assert_eq!(svc.spell_id, 1605, "the buy id stays the wire wrapper");
        assert_eq!(
            svc.skill_line, 26,
            "grouped under the taught ability's Arms line"
        );
        assert_eq!(svc.skill_line_name, "Arms");
        assert_eq!(
            svc.name.as_deref(),
            Some("Heroic Strike"),
            "display resolves through the taught spell"
        );
    }

    #[test]
    fn category_maps_the_wire_state_byte() {
        assert_eq!(
            category(trainer_spell_state::GREEN),
            TrainerServiceCategory::Available
        );
        assert_eq!(
            category(trainer_spell_state::RED),
            TrainerServiceCategory::Unavailable
        );
        assert_eq!(
            category(trainer_spell_state::GRAY),
            TrainerServiceCategory::Used
        );
        // An unexpected value is treated as gated (safe default), never "learnable".
        assert_eq!(category(99), TrainerServiceCategory::Unavailable);
    }

    #[test]
    fn resolve_reads_cost_state_and_gates_with_no_catalog() {
        // Empty catalog (Spell.dbc not resolved): name/subtext/icon nil, the wire fields still land,
        // and the skill-req name falls back rather than dropping the gate.
        let spells = empty_catalog();
        let mut w = wire(2018, trainer_spell_state::RED, 1000, 20, 164);
        w.req_spells = [78, 0, 0];
        // Empty known set: the player knows nothing → the ability gate reads unmet on its own terms.
        let svc = resolve_service(&w, TRAINER_TYPE_TRADESKILL, &spells, None, &HashSet::new());
        assert_eq!(svc.spell_id, 2018);
        assert!(svc.name.is_none(), "no catalog → name in flight");
        assert_eq!(svc.cost, 1000);
        assert_eq!(svc.category, TrainerServiceCategory::Unavailable);
        assert_eq!(svc.level_req, 20);
        // Unavailable service → the SKILL gate reads unmet (coarse, from the category — no per-gate
        // wire bit); the ABILITY gate reads unmet because the empty known set doesn't contain spell 78
        // (per-gate, not from the category). No catalog → the ability name falls back to "Spell 78".
        assert_eq!(
            svc.skill_req,
            Some(TrainerSkillReq {
                name: "Skill 164".to_string(),
                rank: 100,
                met: false,
            })
        );
        assert_eq!(
            svc.ability_reqs,
            vec![TrainerAbilityReq {
                name: "Spell 78".to_string(),
                met: false,
            }]
        );
        assert!(svc.is_trade_skill, "trainer_type 2 → tradeskill");
        assert!(svc.prof_first_rank == w.is_primary_prof_first_rank);
        // No skill-line catalog → skill_line 0 (the engine drops it from the tree) and an empty name.
        assert_eq!(svc.skill_line, 0);
        assert_eq!(svc.skill_line_name, "");
        // No skill gate when req_skill is 0.
        let plain = resolve_service(
            &wire(78, trainer_spell_state::GREEN, 50, 5, 0),
            0,
            &spells,
            None,
            &HashSet::new(),
        );
        assert_eq!(plain.skill_req, None);
        assert!(plain.ability_reqs.is_empty());
        assert!(!plain.is_trade_skill);
    }

    /// A prerequisite ability's met/unmet is per-gate — whether the player KNOWS that spell — and is
    /// decoupled from the service's overall category (wow-re `system/ui/scratch/trainer-requirement.md`).
    /// The director's exact case: an UNAVAILABLE spell (gated by level) whose already-learned prev-rank
    /// prerequisite must still read met (white), not red. Deterministic (no client data needed).
    #[test]
    fn ability_req_met_tracks_known_spells_not_the_service_category() {
        let spells = empty_catalog();
        let mut w = wire(845, trainer_spell_state::RED, 100, 20, 0); // unavailable (gated by level)
        w.req_spells = [78, 0, 0]; // requires Heroic Strike (78)

        // Player doesn't know 78 → the prerequisite is unmet (red).
        let unknown = resolve_service(&w, 0, &spells, None, &HashSet::new());
        assert_eq!(unknown.category, TrainerServiceCategory::Unavailable);
        assert!(!unknown.ability_reqs[0].met, "prereq unknown → unmet");

        // Player knows 78 → the prerequisite is met (white) EVEN THOUGH the service stays unavailable.
        let known: HashSet<u32> = [78].into_iter().collect();
        let learned = resolve_service(&w, 0, &spells, None, &known);
        assert_eq!(learned.category, TrainerServiceCategory::Unavailable);
        assert!(
            learned.ability_reqs[0].met,
            "prereq known → met, decoupled from the unavailable service"
        );
    }

    /// The prerequisite name carries its rank the way the client does — `"Name (Rank)"` — resolved on
    /// real 5875 data: Heroic Strike (78) is Rank 1, so a service requiring it shows "Heroic Strike
    /// (Rank 1)", met iff the player knows 78. Skips without client data.
    #[test]
    fn ability_req_shows_the_required_rank_on_real_data() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let spells = benilla_formats::load_spell_catalog(&mut chain).expect("load Spell");

        let mut w = wire(846, trainer_spell_state::RED, 100, 20, 0);
        w.req_spells = [78, 0, 0]; // Heroic Strike Rank 1

        let known: HashSet<u32> = [78].into_iter().collect();
        let svc = resolve_service(&w, 0, &spells, None, &known);
        assert_eq!(
            svc.ability_reqs,
            vec![TrainerAbilityReq {
                name: "Heroic Strike (Rank 1)".to_string(),
                met: true,
            }],
            "the prereq shows its rank and reads met because the player knows it"
        );
        // Not known → same name, but unmet (red).
        let svc = resolve_service(&w, 0, &spells, None, &HashSet::new());
        assert!(!svc.ability_reqs[0].met);
        assert_eq!(svc.ability_reqs[0].name, "Heroic Strike (Rank 1)");
    }

    #[test]
    fn snapshot_is_none_when_closed_and_lists_services_when_open() {
        let spells = empty_catalog();
        let mut open = TrainerOpen::default();
        assert!(snapshot(&open, &spells, None, &HashSet::new()).is_none());

        open.open(
            0x42,
            0,
            vec![
                wire(78, trainer_spell_state::GREEN, 100, 10, 0),
                wire(79, trainer_spell_state::GRAY, 200, 0, 0),
            ],
            "Learn from me.".into(),
        );
        let state = snapshot(&open, &spells, None, &HashSet::new()).expect("open → Some");
        assert_eq!(state.greeting, "Learn from me.");
        assert_eq!(state.services.len(), 2);
        assert_eq!(
            state.services[0].category,
            TrainerServiceCategory::Available
        );
        assert_eq!(state.services[1].category, TrainerServiceCategory::Used);

        open.clear();
        assert!(snapshot(&open, &spells, None, &HashSet::new()).is_none());
        assert_eq!(open.trainer, None);
    }
}
