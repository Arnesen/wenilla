//! The app-side **crafting book feed** (decision 0437 phase 2) — the client-built TradeSkill
//! window around [`benilla_ui::script`]'s `tradeskill` module, the trainer feed's
//! ([`crate::ui_trainer`]) client-local twin.
//!
//! There is no wire here at all, by the byte law: casting a profession opener (`Effect[0] ==
//! SPELL_EFFECT_TRADE_SKILL`) never reaches the send — `Spell_C::TryCast 0x6e4b60` branches
//! client-side and opens the window (wow-re `wave-cast.md`, VERIFIED; 0437). benilla mirrors that
//! as the [`TradeSkillOpens`] intercept inside `ui_action::send_spell_cast`. The book itself is
//! **client-built**: every known spell ([`PlayerActions::spells`]) carrying
//! `SPELL_ATTR_IS_TRADESKILL` (`0x20` — the same bit that hides recipes from the spellbook, 0227)
//! whose `SkillLineAbility` row joins it to the open line becomes a row; reagents/tools off the
//! `Spell.dbc` columns, have-counts off the bags ([`count_of`]), the product off
//! `EffectItemType`, names/icons through the ask-once item-template cache.
//!
//! The §5 fold-back (wow-re `tradeskill` node, VERIFIED at the bytes, decision 0446): the
//! client-built list, the difficulty bands + the `low==0 → high−25` fallback, the numMade dice
//! law, the one-cast-per-item repeat machine (clamped to numAvailable at the latch, re-cast off
//! our own `SMSG_SPELL_GO`, canceled by fail/ESC/close), and the `EffectMiscValue[0] != 0`
//! Craft-vs-TradeSkill routing are all byte-confirmed. **The header law landed too:** rows group
//! by the created item's `(ItemClass, ItemSubClass)`, named from `ItemSubClass.dbc`, two-level
//! sort (0446; the filter family on top of it, 0452) — this feed resolves each recipe's `group`
//! ([`resolve_recipe`]), so the book is no longer flat. Remaining gap: the spell-focus tool never
//! rendered red (no client-side proximity model — the server refuses the cast).

use std::time::Instant;

use bevy::prelude::*;

use benilla_formats::{SpellFocusCatalog, SPELL_EFFECT_CREATE_ITEM};
use benilla_protocol::messages::PLAYER_SKILL_SLOTS;
use benilla_ui::script::{
    TradeSkillDifficulty, TradeSkillReagent, TradeSkillRecipe, TradeSkillState, UiScript,
};

use crate::assets::{AssetSet, LockRecover, WorldAssets};
use crate::creature_anim::{CastEvent, CastEventKind};
use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{NetCommands, ObjectStore, SelfPlayer};
use crate::ui_action::{cast_target, send_spell_cast, PlayerActions, Spells};
use crate::ui_items::count_of;
use crate::ui_script::UiInput;
use crate::ui_spellbook::SkillLines;
use crate::ui_unit::UnitFeed;

/// Effect-47 opener casts intercepted by `ui_action::send_spell_cast` (the TryCast branch's
/// benilla seam) — each entry is the opener's spell id, resolved to a skill line and opened by
/// [`open_trade_skill`] the same frame.
#[derive(Resource, Default)]
pub(crate) struct TradeSkillOpens(pub(crate) Vec<u32>);

/// The open crafting book: the skill line whose recipes the window shows. `None` = closed.
/// Client-local state — no wire owns it (0437).
#[derive(Resource, Default)]
pub(crate) struct TradeSkillOpen {
    pub(crate) line: Option<u32>,
}

/// The Create/Create All repeat machine (TU-D, byte-confirmed — decision 0446):
/// `DoTradeSkill(spell, n)` latches `n`, casts once, and each of our own `SMSG_SPELL_GO`s for
/// that spell decrements and re-casts until dry; any cast failure or a window close stops it cold.
#[derive(Resource, Default)]
pub(crate) struct TradeSkillRepeat {
    pub(crate) spell_id: u32,
    pub(crate) remaining: u32,
}

impl TradeSkillRepeat {
    fn clear(&mut self) {
        self.spell_id = 0;
        self.remaining = 0;
    }
}

/// `SpellFocusObject.dbc` — the "Requires: Anvil" vocabulary ([`benilla_formats::SpellFocusCatalog`]).
#[derive(Resource)]
pub(crate) struct SpellFocus {
    pub(crate) catalog: SpellFocusCatalog,
}

pub(crate) struct UiTradeSkillPlugin;

impl Plugin for UiTradeSkillPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TradeSkillOpens>()
            .init_resource::<TradeSkillOpen>()
            .init_resource::<TradeSkillRepeat>()
            .add_systems(Startup, load_spell_focus.after(AssetSet::Open))
            .add_systems(
                Update,
                (
                    // Opens resolve before the feed so an intercepted opener cast shows the window
                    // the same frame; the feed pushes before the input pass (the trainer's order);
                    // the drain + repeat machine run after it so a Create click casts this frame.
                    open_trade_skill.before(feed_trade_skill),
                    feed_trade_skill.in_set(UnitFeed).before(UiInput),
                    drain_trade_skill.after(UiInput),
                ),
            );
    }
}

fn load_spell_focus(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_spell_focus_catalog(&mut chain)
    };
    match loaded {
        Ok(catalog) => {
            debug!("ui_tradeskill: {} spell-focus name(s)", catalog.len());
            commands.insert_resource(SpellFocus { catalog });
        }
        Err(e) => warn!(
            "ui_tradeskill: SpellFocusObject.dbc failed to load — the Requires line drops the \
             focus name: {e:#}"
        ),
    }
}

/// Resolve intercepted opener casts to their skill line and open the right book. The line comes
/// from the opener's own `SkillLineAbility` row (3908 Tailoring → 197) — the client's
/// `SkillLineRecIndex_Find` hop (`0x6de040`); the Craft-vs-TradeSkill fork is the opener's
/// `EffectMiscValue[0]` (byte-VERIFIED, wow-re `tradeskill` TU-A).
fn open_trade_skill(
    mut opens: ResMut<TradeSkillOpens>,
    mut open: ResMut<TradeSkillOpen>,
    mut craft_open: ResMut<crate::ui_craft::CraftOpen>,
    skill_lines: Option<Res<SkillLines>>,
    spells: Option<Res<Spells>>,
    mut repeat: ResMut<TradeSkillRepeat>,
) {
    for spell_id in opens.0.drain(..) {
        let Some(skill_lines) = skill_lines.as_deref() else {
            warn!("ui_tradeskill: opener {spell_id} before SkillLineAbility loaded — dropped");
            continue;
        };
        let Some(line) = skill_lines.catalog.spell_to_line(spell_id) else {
            warn!("ui_tradeskill: opener {spell_id} has no SkillLineAbility row — dropped");
            continue;
        };
        // The routing key — byte-VERIFIED (wow-re `tradeskill` TU-A, `0x6e4bd7`): the opener's
        // `EffectMiscValue[0] != 0` routes to the CraftFrame (Enchanting 3, Beast Training 1);
        // zero routes to the TradeSkillFrame. NOT a skill-line test — 0437's line-333 INTERIM
        // is corrected by this fold-back.
        let to_craft = spells
            .as_deref()
            .and_then(|s| s.catalog.get(spell_id))
            .is_some_and(|d| d.effect_misc_value[0] != 0);
        if to_craft {
            debug!("ui_tradeskill: opener {spell_id} opens the CraftFrame (line {line})");
            craft_open.line = Some(line);
        } else {
            debug!("ui_tradeskill: opener {spell_id} opens skill line {line}");
            if open.line != Some(line) {
                repeat.clear();
            }
            open.line = Some(line);
        }
    }
}

/// Read a skill line's `(value, max)` off the `PLAYER_SKILL_INFO` triplets (the `ui_char`
/// `skill_pair` shape; the window's rank bar shows the raw value, bonuses unstyled — INTERIM).
fn skill_rank(store: &ObjectStore, skill_id: u32) -> (u32, u32) {
    for i in 0..PLAYER_SKILL_SLOTS {
        if let Some(s) = store.0.player_skill(i) {
            if u32::from(s.skill_id) == skill_id {
                return (u32::from(s.value), u32::from(s.max));
            }
        }
    }
    (0, 0)
}

/// The difficulty banding — byte-VERIFIED (wow-re `tradeskill` TU-C): gray at rank ≥ trivialHigh,
/// green ≥ the (low+high)/2 midpoint, yellow ≥ trivialLow, orange below; the client's one
/// fallback is `low == 0 → low = max(high − 25, 0)`. The TradeSkill window bands the RAW rank;
/// the Craft window bands the effective skill (rank + bonuses) — the caller picks.
pub(crate) fn difficulty(rank: u32, low: u32, high: u32) -> TradeSkillDifficulty {
    let low = if low == 0 {
        high.saturating_sub(25)
    } else {
        low
    };
    if rank >= high {
        TradeSkillDifficulty::Trivial
    } else if rank >= (low + high) / 2 {
        TradeSkillDifficulty::Easy
    } else if rank >= low {
        TradeSkillDifficulty::Medium
    } else {
        TradeSkillDifficulty::Optimal
    }
}

/// An item template's icon path — `ItemDisplayInfo.dbc` via the [`ItemDisplays`] catalog (the
/// container feed's own join).
fn item_icon(icons: Option<&ItemDisplays>, display_info_id: u32) -> Option<String> {
    icons
        .and_then(|i| i.catalog.get(display_info_id))
        .and_then(|d| d.icon.clone())
}

/// Build one recipe row: reagents/tools/product resolved through the ask-once template cache
/// (`None` names re-resolve next frame when the template lands — the item-row precedent).
#[allow(clippy::too_many_arguments)] // the resolver's full catalog set
fn resolve_recipe(
    spell_id: u32,
    rank: u32,
    spells: &Spells,
    skill_lines: &SkillLines,
    focus: Option<&SpellFocus>,
    icons: Option<&ItemDisplays>,
    subclasses: Option<&crate::ui_items::ItemSubClasses>,
    store: &ObjectStore,
    items: &mut Items,
    commands: &NetCommands,
    cooldowns: &crate::cooldowns::Cooldowns,
    now: Instant,
) -> Option<TradeSkillRecipe> {
    let d = spells.catalog.get(spell_id)?;
    let sla = skill_lines.catalog.ability(spell_id)?;

    // Reagents: (entry, need) pairs off Spell.dbc; names/icons ask-once; have = bag count.
    let mut reagents = Vec::new();
    let mut num_available = u32::MAX;
    for &(entry, need) in d.reagents.iter().filter(|&&(e, n)| e != 0 && n != 0) {
        let have = count_of(&store.0, items, entry);
        let (name, icon) = match items.template(entry, 0, commands) {
            Some(t) => (Some(t.name.clone()), item_icon(icons, t.display_info_id)),
            None => (None, None),
        };
        reagents.push(TradeSkillReagent {
            item: entry,
            name,
            icon,
            need,
            have,
        });
        num_available = num_available.min(have / need);
    }
    if reagents.is_empty() {
        num_available = 0;
    }

    // The product (CREATE_ITEM's EffectItemType, slot 0 — every probed recipe carries it there;
    // a multi-slot product is unobserved in 5875) — its icon fronts the row when resolved. The
    // made-count is byte-VERIFIED (wow-re `tradeskill` TU-C): min = BasePoints + BaseDice,
    // max = BasePoints + DieSides × BaseDice (multiplicative), clamped ≥ 1.
    let (product_item, min_made, max_made) = if d.effect_1 == SPELL_EFFECT_CREATE_ITEM {
        let base = d.effect_base_points[0].max(0) as u32;
        let dice = d.effect_base_dice[0].max(0) as u32;
        let die = d.effect_die_sides[0].max(0) as u32;
        let min = (base + dice).max(1);
        (d.effect_item_type[0], min, (base + die * dice).max(min))
    } else {
        (0, 1, 1)
    };
    let product_icon = (product_item != 0)
        .then(|| {
            items
                .template(product_item, 0, commands)
                .map(|t| t.display_info_id)
                .and_then(|d| item_icon(icons, d))
        })
        .flatten();
    let icon = product_icon.or_else(|| d.icon.clone());
    // The VERIFIED header key (wow-re `tradeskill` TU-B): the created item's (class, subclass),
    // named from ItemSubClass.dbc (verbose-first). `None` while the ask-once template is in
    // flight — the client's own one-frame header deferral; the engine buckets it trailing. The
    // same template answer carries the product's InventoryType — the InvSlot filter's raw input
    // (the engine folds it to a slot bit; 0 = non-equip → the catch-all) — and its ItemLevel,
    // the engine sort's secondary key (the `record+0x14` identity, pinned 2026-07-17).
    let (group, product_inv_type, product_item_level) = (product_item != 0)
        .then(|| {
            items.template(product_item, 0, commands).map(|t| {
                let name = subclasses
                    .and_then(|sc| sc.0.name(t.class, t.subclass))
                    .unwrap_or_default()
                    .to_string();
                ((t.class, t.subclass, name), t.inventory_type, t.item_level)
            })
        })
        .flatten()
        .map_or((None, 0, 0), |(g, it, il)| (Some(g), it, il));

    // Tools: the two totem items (present-not-consumed — a Blacksmith Hammer) + the spell focus
    // (Anvil/Forge/Cooking Fire; never red — module doc INTERIM).
    let mut tools = Vec::new();
    for &t in d.totems.iter().filter(|&&t| t != 0) {
        let have = count_of(&store.0, items, t) > 0;
        if let Some(info) = items.template(t, 0, commands) {
            tools.push((info.name.clone(), have));
        }
    }
    if d.requires_spell_focus != 0 {
        if let Some(name) = focus.and_then(|f| f.catalog.name(d.requires_spell_focus)) {
            tools.push((name.to_string(), true));
        }
    }

    let cd = cooldowns.info(spell_id, 0, Some(d), now);
    Some(TradeSkillRecipe {
        group,
        spell_id,
        name: d.name.clone(),
        difficulty: difficulty(rank, sla.trivial_low, sla.trivial_high),
        num_available,
        icon,
        min_made,
        max_made,
        cooldown_secs: (cd.remaining_ms > 0).then(|| u64::from(cd.remaining_ms).div_ceil(1000)),
        product_item,
        product_inv_type,
        product_item_level,
        reagents,
        tools,
    })
}

/// Build the book: the known attr-`0x20` recipes of the open line, difficulty-banded against the
/// current rank. No sort applied here — the engine owns ALL ordering (group + tier + name, the
/// VERIFIED two-level law, decision 0446 wow-re `tradeskill` TU-B).
#[allow(clippy::too_many_arguments)] // a Bevy system's full input set (the feed precedent)
fn feed_trade_skill(
    script: Option<NonSendMut<UiScript>>,
    open: Res<TradeSkillOpen>,
    actions: Res<PlayerActions>,
    spells: Option<Res<Spells>>,
    skill_lines: Option<Res<SkillLines>>,
    focus: Option<Res<SpellFocus>>,
    icons: Option<Res<ItemDisplays>>,
    subclasses: Option<Res<crate::ui_items::ItemSubClasses>>,
    repeat: Res<TradeSkillRepeat>,
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    mut items: ResMut<Items>,
    commands: Res<NetCommands>,
    cooldowns: Res<crate::cooldowns::Cooldowns>,
    mut last: Local<Option<TradeSkillState>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let fresh = (|| -> Option<TradeSkillState> {
        let line = open.line?;
        let spells = spells.as_deref()?;
        let skill_lines = skill_lines.as_deref()?;
        let store = self_store.single().ok()?;
        let (rank, max_rank) = skill_rank(store, line);
        let line_name = skill_lines
            .catalog
            .line(line)
            .map(|l| l.name.clone())
            .unwrap_or_else(|| format!("Skill {line}"));
        let now = Instant::now();
        let recipes: Vec<TradeSkillRecipe> = actions
            .spells
            .iter()
            .filter(|&&s| {
                spells
                    .catalog
                    .get(s)
                    .is_some_and(|d| d.attributes & ATTR_IS_TRADESKILL != 0)
                    && skill_lines.catalog.spell_to_line(s) == Some(line)
            })
            .filter_map(|&s| {
                resolve_recipe(
                    s,
                    rank,
                    spells,
                    skill_lines,
                    focus.as_deref(),
                    icons.as_deref(),
                    subclasses.as_deref(),
                    store,
                    &mut items,
                    &commands,
                    &cooldowns,
                    now,
                )
            })
            .collect();
        // No app-side sort: the engine owns ALL ordering (group + tier + name — the VERIFIED
        // two-level law, wow-re `tradeskill` TU-B).
        Some(TradeSkillState {
            line,
            line_name,
            rank,
            max_rank,
            recipes,
            repeat_count: repeat.remaining,
        })
    })();

    let repeat_changed = match (&*last, &fresh) {
        (Some(l), Some(f)) => l.repeat_count != f.repeat_count,
        _ => false,
    };
    if fresh == *last {
        return;
    }
    script.set_trade_skill(fresh.clone());
    match (&*last, &fresh) {
        (None, Some(f)) => {
            debug!(
                "ui_tradeskill: window opens — {} recipe(s), first group {:?}",
                f.recipes.len(),
                f.recipes.first().and_then(|r| r.group.clone())
            );
            script.fire_event("TRADE_SKILL_SHOW", vec![]);
        }
        (Some(_), Some(_)) => {
            script.fire_event("TRADE_SKILL_UPDATE", vec![]);
            if repeat_changed {
                script.fire_event("UPDATE_TRADESKILL_RECAST", vec![]);
            }
        }
        (Some(_), None) => script.fire_event("TRADE_SKILL_CLOSE", vec![]),
        (None, None) => {}
    }
    *last = fresh;
}

/// `SPELL_ATTR_IS_TRADESKILL` — the recipe marker (0227's add-gate bit; the book divert the
/// spellbook applies is the same bit that ADMITS a spell here).
const ATTR_IS_TRADESKILL: u32 = 0x20;

/// Drain the Lua intents and run the repeat machine: `DoTradeSkill` latches the count and casts
/// through the ONE cast-send path (0216 §8); our own GO for the latched spell re-casts until dry;
/// a fail or `CloseTradeSkill` stops everything.
#[allow(clippy::too_many_arguments)] // a Bevy system's full input set (send_spell_cast's own)
fn drain_trade_skill(
    script: Option<NonSendMut<UiScript>>,
    mut open: ResMut<TradeSkillOpen>,
    mut repeat: ResMut<TradeSkillRepeat>,
    mut cast_events: MessageReader<CastEvent>,
    targeting: cast_target::CastTargeting,
    commands: Res<NetCommands>,
    self_player: Query<(Entity, Has<crate::creature_anim::Engaged>), With<SelfPlayer>>,
    // The cast catalog + the item cache the pre-send totem/reagent check reads (decision
    // 0552) + the targeting mode the one cast path threads (0792), one tuple param (Bevy's
    // 16-param ceiling).
    spell_inputs: (
        Option<Res<Spells>>,
        Res<Items>,
        ResMut<crate::ui_action::SpellTargeting>,
    ),
    mut sheath: MessageWriter<crate::creature_anim::SheathRequest>,
    mut pending: ResMut<crate::ui_cast::PendingCast>,
    mut queued_melee: ResMut<crate::ui_cast::QueuedMeleeSpell>,
    mut cooldowns: ResMut<crate::cooldowns::Cooldowns>,
    mut cast_errors: ResMut<crate::ui_action::CastErrors>,
    mut auto_repeat: ResMut<crate::ui_action::AutoRepeatActive>,
    mut opens: ResMut<TradeSkillOpens>,
    mut ecs: Commands,
) {
    let (spells, cast_items, mut ground) = spell_inputs;
    let Some(mut script) = script else {
        return;
    };
    let mut cast =
        |spell_id: u32,
         ecs: &mut Commands,
         pending: &mut crate::ui_cast::PendingCast,
         queued_melee: &mut crate::ui_cast::QueuedMeleeSpell,
         cooldowns: &mut crate::cooldowns::Cooldowns,
         cast_errors: &mut crate::ui_action::CastErrors,
         auto_repeat: &mut crate::ui_action::AutoRepeatActive,
         sheath: &mut MessageWriter<crate::creature_anim::SheathRequest>| {
            send_spell_cast(
                spell_id,
                &targeting.context(),
                &commands,
                &self_player,
                spells.as_deref(),
                &cast_items,
                sheath,
                ecs,
                pending,
                queued_melee,
                cooldowns,
                cast_errors,
                auto_repeat,
                &mut opens,
                &mut ground,
            );
        };

    for (spell_id, count) in script.take_trade_skill_dos() {
        if open.line.is_none() {
            debug!("ui_tradeskill: DoTradeSkill({spell_id}) with no open book — ignored");
            continue;
        }
        debug!("ui_tradeskill: DoTradeSkill({spell_id}) ×{count}");
        repeat.spell_id = spell_id;
        repeat.remaining = count.max(1);
        cast(
            spell_id,
            &mut ecs,
            &mut pending,
            &mut queued_melee,
            &mut cooldowns,
            &mut cast_errors,
            &mut auto_repeat,
            &mut sheath,
        );
    }

    // The repeat machine's continuation: our own cast edges for the latched spell.
    let self_entity = self_player.single().ok().map(|(e, _)| e);
    for ev in cast_events.read() {
        if repeat.remaining == 0 || Some(ev.entity) != self_entity || ev.spell_id != repeat.spell_id
        {
            continue;
        }
        match ev.kind {
            CastEventKind::Go => {
                repeat.remaining -= 1;
                if repeat.remaining > 0 {
                    debug!(
                        "ui_tradeskill: recast {} ({} left)",
                        repeat.spell_id, repeat.remaining
                    );
                    cast(
                        repeat.spell_id,
                        &mut ecs,
                        &mut pending,
                        &mut queued_melee,
                        &mut cooldowns,
                        &mut cast_errors,
                        &mut auto_repeat,
                        &mut sheath,
                    );
                }
            }
            CastEventKind::Fail => {
                debug!("ui_tradeskill: cast failed — repeat stops");
                repeat.clear();
            }
            _ => {}
        }
    }

    // An engine-side list mutation this frame (a filter set, an expand/collapse) — the real
    // client's C call re-sorts and fires TRADE_SKILL_UPDATE from inside (`0x4fd710`/`0x4fd750`);
    // ours surfaces as the touched flag, answered here the same frame so the ref Lua's
    // event-driven repaint (the CollapseAll button, the filter menu clicks) lands without a
    // direct Update() call.
    let touched = script.take_trade_skill_touched();
    if touched && open.line.is_some() {
        script.fire_event("TRADE_SKILL_UPDATE", vec![]);
    }

    if script.take_trade_skill_close() {
        debug!("ui_tradeskill: client-side close (no packet)");
        open.line = None;
        repeat.clear();
    }
}
