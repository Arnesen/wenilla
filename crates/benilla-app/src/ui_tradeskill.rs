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

// `bevy::platform::time::Instant`, not `std::time::Instant`: this flows into `crate::cooldowns`/`crate::ui_script::UiClock`, which on wasm32 (the default `web` Bevy feature) is a genuinely different type from `std::time::Instant` — a plain alias for it everywhere else.
use bevy::platform::time::Instant;

use bevy::prelude::*;

use benilla_formats::{SpellFocusCatalog, SPELL_ATTR_IS_TRADESKILL, SPELL_EFFECT_CREATE_ITEM};
use benilla_protocol::messages::PLAYER_SKILL_SLOTS;
use benilla_ui::script::{
    TradeSkillDifficulty, TradeSkillReagent, TradeSkillRecipe, TradeSkillState, UiScript,
};

use crate::creature_anim::{CastEvent, CastEventKind};
use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{NetCommands, ObjectStore, SelfPlayer};
use crate::ui_action::{cast_target, CastCommit, CastLadder, PlayerActions, Spells};
use crate::ui_items::{count_of, item_icon, InventoryScope};
use crate::ui_script::UiInput;
use crate::ui_spellbook::SkillLines;
use crate::ui_unit::UnitFeed;
use benilla_assets::{AssetSet, LockRecover, WorldAssets};

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
        // is corrected by this fold-back. The nonzero value is not merely a flag: it IS the craft
        // type the client keeps at `ds:0xbdcfb8`, and the Craft window keys both its admission
        // filter and its row comparator on it (decision 1124), so it rides along.
        let craft_type = spells
            .as_deref()
            .and_then(|s| s.catalog.get(spell_id))
            .and_then(|d| u32::try_from(d.effect_misc_value[0]).ok())
            .unwrap_or(0);
        if craft_type != 0 {
            debug!(
                "ui_tradeskill: opener {spell_id} opens the CraftFrame (line {line}, craft type {craft_type})"
            );
            craft_open.line = Some(line);
            craft_open.craft_type = craft_type;
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

/// **Law C** — the TradeSkill window's row icon, transcribing `GetTradeSkillIcon 0x4fdae0`
/// (byte-VERIFIED; wow-re `ui/scratch/spell-icon-substitution-law.md` §2, folded back by decision
/// 1107). Read `EffectItemType[0]` **unconditionally** as an item id and paint that item's icon;
/// on any miss — a zero id, a template not yet landed — return **`None`**.
///
/// Two things the binding pointedly does *not* do, both of which this used to:
///
/// - **No `CREATE_ITEM` gate.** `Effect[0]` (`+0xf4`) is never read anywhere in `[0x4fdae0,
///   0x4fdc29]`. So this takes `d.effect_item_type[0]` directly rather than the `product_item`
///   computed beside it — that variable's `CREATE_ITEM` gate belongs to the separately-verified
///   *made-count* law (wow-re `tradeskill` TU-C) and has no business steering an icon.
/// - **No spell-icon fallback.** `+0x1d4`/`+0x1d8` and the `SpellIcon.dbc` globals appear nowhere
///   in the extent — proof by exhaustion over its three return paths, the last of which is a bare
///   `lua_pushnil`. A row whose item will not resolve shows *nothing*, never the recipe's own art.
///
/// `None` while the ask-once template is in flight is the client's behaviour too: it pushes nil and
/// repaints when the async item callback rebuilds the list and fires `TRADE_SKILL_UPDATE`.
///
/// The sibling laws deliberately disagree with this one — see [`crate::ui_craft`] (Law D, always
/// the spell's own icon) and [`crate::ui_trainer::service_icon`] (Law B). There is no shared
/// resolver in the real client and there is none here.
fn recipe_icon(
    d: &benilla_formats::SpellDisplay,
    icons: Option<&ItemDisplays>,
    items: &mut Items,
    commands: &NetCommands,
) -> Option<String> {
    let item = d.effect_item_type[0];
    if item == 0 {
        return None;
    }
    let display = items.template(item, 0, commands)?.display_info_id;
    item_icon(icons, display)
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
        let have = count_of(&store.0, items, entry, InventoryScope::CARRIED);
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
    // a multi-slot product is unobserved in 5875): the tooltip channel's item and the header key's
    // source. The made-count is byte-VERIFIED (wow-re `tradeskill` TU-C): min = BasePoints +
    // BaseDice, max = BasePoints + DieSides × BaseDice (multiplicative), clamped ≥ 1.
    let (product_item, min_made, max_made) = if d.effects[0] == SPELL_EFFECT_CREATE_ITEM {
        let base = d.effect_base_points[0].max(0) as u32;
        let dice = d.effect_base_dice[0].max(0) as u32;
        let die = d.effect_die_sides[0].max(0) as u32;
        let min = (base + dice).max(1);
        (d.effect_item_type[0], min, (base + die * dice).max(min))
    } else {
        (0, 1, 1)
    };
    let icon = recipe_icon(d, icons, items, commands);
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
        let have = count_of(&store.0, items, t, InventoryScope::CARRIED) > 0;
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
    mut last: Local<crate::ui_script::VmMemo<Option<TradeSkillState>>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let last = last.get(&script);
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
                    .is_some_and(|d| d.attributes & SPELL_ATTR_IS_TRADESKILL != 0)
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

/// Drain the Lua intents and run the repeat machine: `DoTradeSkill` latches the count and casts
/// through the ONE cast-send path (0216 §8); our own GO for the latched spell re-casts until dry;
/// a fail or `CloseTradeSkill` stops everything.
fn drain_trade_skill(
    script: Option<NonSendMut<UiScript>>,
    mut open: ResMut<TradeSkillOpen>,
    mut repeat: ResMut<TradeSkillRepeat>,
    mut cast_events: MessageReader<CastEvent>,
    targeting: cast_target::CastTargeting,
    mut ladder: CastLadder,
) {
    let Some(mut script) = script else {
        return;
    };

    for (spell_id, count) in script.take_trade_skill_dos() {
        if open.line.is_none() {
            debug!("ui_tradeskill: DoTradeSkill({spell_id}) with no open book — ignored");
            continue;
        }
        debug!("ui_tradeskill: DoTradeSkill({spell_id}) ×{count}");
        repeat.spell_id = spell_id;
        repeat.remaining = count.max(1);
        ladder.send(spell_id, &targeting.context(), CastCommit::Spell);
    }

    // The repeat machine's continuation: our own cast edges for the latched spell.
    let self_entity = ladder.self_player.single().ok().map(|(e, _)| e);
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
                    ladder.send(repeat.spell_id, &targeting.context(), CastCommit::Spell);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::{test_template, TestDeps};
    use benilla_formats::{
        ItemDisplay, ItemDisplayCatalog, SpellDisplay, SPELL_EFFECT_ENCHANT_ITEM,
    };
    use std::collections::HashMap;

    /// A recipe whose `Effect[0]` is `effect` and whose `EffectItemType[0]` is `item`, carrying its
    /// own distinct spell icon so a wrong arm is *named* by the assertion, not merely unequal.
    fn recipe(effect: u32, item: u32) -> SpellDisplay {
        SpellDisplay {
            name: "Runed Copper Breastplate".into(),
            icon: Some("SPELL".into()),
            effects: [effect, 0, 0],
            effect_item_type: [item, 0, 0],
            ..Default::default()
        }
    }

    /// Item 777's template + `ItemDisplayInfo` row, landed — the icon Law C actually wants.
    fn landed_item(deps: &mut TestDeps) -> ItemDisplays {
        let mut t = test_template("Runed Copper Breastplate");
        t.display_info_id = 5;
        deps.items.insert_template(777, Some(t));
        ItemDisplays::icons_for_tests(ItemDisplayCatalog::from_displays(HashMap::from([(
            5,
            ItemDisplay {
                icon: Some("ITEM".into()),
                ..Default::default()
            },
        )])))
    }

    /// The ordinary arm: a `CREATE_ITEM` recipe fronts its product's item art, never the spell's.
    #[test]
    fn law_c_paints_the_created_items_icon() {
        let mut deps = TestDeps::new();
        let icons = landed_item(&mut deps);
        let d = recipe(SPELL_EFFECT_CREATE_ITEM, 777);
        assert_eq!(
            recipe_icon(&d, Some(&icons), &mut deps.items, &deps.commands),
            Some("ITEM".into()),
        );
    }

    /// **No `CREATE_ITEM` gate** — `Effect[0]` is never read by `0x4fdae0`. A recipe carrying a
    /// non-`CREATE_ITEM` effect in slot 0 still resolves `EffectItemType[0]` as an item id. This is
    /// the half our old code got wrong by gating; it is asserted with a deliberately absurd effect
    /// so the test fails the moment someone reintroduces a gate of any shape.
    #[test]
    fn law_c_does_not_gate_on_the_effect_type() {
        let mut deps = TestDeps::new();
        let icons = landed_item(&mut deps);
        let d = recipe(SPELL_EFFECT_ENCHANT_ITEM, 777);
        assert_eq!(
            recipe_icon(&d, Some(&icons), &mut deps.items, &deps.commands),
            Some("ITEM".into()),
        );
    }

    /// **No spell-icon fallback** — the binding's third return is a bare `lua_pushnil`. Every miss
    /// arm is `None`, and in particular NOT `"SPELL"`: a zero item id (an enchant recipe), an item
    /// whose template will never land, and a missing `ItemDisplayInfo` row.
    #[test]
    fn law_c_pushes_nil_on_every_miss_never_the_spells_icon() {
        let mut deps = TestDeps::new();
        let icons = landed_item(&mut deps);

        // EffectItemType[0] == 0: 0x55ba30 short-circuits on a zero id before hashing.
        let none = recipe(SPELL_EFFECT_ENCHANT_ITEM, 0);
        assert_eq!(
            recipe_icon(&none, Some(&icons), &mut deps.items, &deps.commands),
            None,
        );

        // A template that never lands (the async row) — nil, and the ask goes out exactly once.
        let missing = recipe(SPELL_EFFECT_CREATE_ITEM, 999);
        assert_eq!(
            recipe_icon(&missing, Some(&icons), &mut deps.items, &deps.commands),
            None,
        );
        assert_eq!(
            recipe_icon(&missing, Some(&icons), &mut deps.items, &deps.commands),
            None,
        );
        assert_eq!(deps.queried_entries(), vec![999], "ask-once, not ask-often");

        // The template landed but ItemDisplayInfo is unresolved — still nil, still not "SPELL".
        let d = recipe(SPELL_EFFECT_CREATE_ITEM, 777);
        assert_eq!(recipe_icon(&d, None, &mut deps.items, &deps.commands), None);
    }
}
