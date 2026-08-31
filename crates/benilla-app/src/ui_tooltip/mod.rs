//! The world-mouseover tooltip system (decision 0274 P3) — the app half of the byte-verified
//! mouseover flow (0276): the engine rebuilds the tooltip **once per hover-target change**
//! (`world_tooltip_unit` / `world_tooltip_gameobject`: default anchor via
//! `OnTooltipSetDefaultAnchor`, the verified line laws, `UPDATE_MOUSEOVER_UNIT` for the unit
//! recolor), the health bar tracks the per-frame `set_unit` pushes in between (the HEALTH
//! watcher), and hover loss ARMS a fade (`world_tooltip_fade`) rather than hiding.
//!
//! The picks are the byte-verified pair the cursor already rides: [`Hovered`] (units) and
//! [`HoveredObject`] (GameObjects), arbitrated by [`go_is_nearest`] exactly like the click
//! router. A hovered GameObject shows its template name (gold) and, when flag-locked, the red
//! Lock.dbc requirement lines ("Requires <key item>" / "Requires <skill>") — the verified
//! `0x52aa20` law. The standalone-corpse builder joins when corpse objects stream.

use crate::ui_items::{count_of, InventoryScope};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use benilla_ui::script::{TooltipTint, UiScript, UnitState};

use crate::items::Items;
use crate::names::NameCache;
use crate::net::{NetCommands, ObjectStore, Reputations, SelfPlayer};
use crate::target::{
    go_is_nearest, ring_reaction, Factions, Hovered, HoveredObject, GO_FLAG_LOCKED, GO_TYPE_GENERIC,
};
use crate::ui_action::{PlayerActions, Spells};
use crate::ui_script::UiInput;
use crate::ui_unit::{enrich_unit, snapshot, UnitFeed};

pub struct UiTooltipPlugin;

impl Plugin for UiTooltipPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                drive_mouseover_tooltip.in_set(UnitFeed).before(UiInput),
                feed_spell_tooltips.in_set(UnitFeed).before(UiInput),
            ),
        );
    }
}

/// Everything a view needs beyond the spell catalogs — the player-dependent halves of the line
/// law: the worn set (law §3.6's equipped-item test), the bags (§3.8's reagent possession, and
/// the item-name cache the reagent names come from), the current form, and the bind point `$z`
/// substitutes against.
struct ViewCtx<'a> {
    home_area: Option<&'a str>,
    form: u8,
    store: Option<&'a ObjectStore>,
    items: &'a mut Items,
    commands: &'a NetCommands,
    sub_classes: Option<&'a benilla_formats::ItemSubClassCatalog>,
}

/// Build one spell's tooltip view (decision 0274 P2) — the verified spell line law's inputs,
/// every string resolved here where the catalogs live: the cost cell (the RESOLVED
/// `power_cost` through the power-type key array, health fallback, `_PER_TIME` composite — law
/// §3.3, 1074; rage prints wire-cost ÷ 10), the range cell ("N yd range", "N-M yd range" when
/// the row's min is nonzero — the law's `"%d-%d"` fork; "Melee Range" for the melee family —
/// INTERIM text, the proper source is SpellRange.dbc's own display-name column), the cast cell
/// (the full `52eb45` ladder: sec/min, the negative-base sentinel, "Next melee"/"Attack
/// speed"/"Channeled", and the mana-keyed Instant fork — law §3.4, 1074; None = the law's
/// passive gate, which omits the whole line), the cooldown cell
/// (`max(RecoveryTime, CategoryRecoveryTime)` — law §3.4), the required-item and required-form
/// lines (law §3.6), the reagents line (law §3.8), and the $-substituted
/// description/aura-description (byte-exact formulas, `benilla_formats::substitute`).
///
/// Line TEXT is composed in English here, as every cell in this builder is; the reference reads
/// its own GlobalStrings templates ("Reagents: ", "Requires %s"). That is this builder's standing
/// INTERIM shape, not a new debt introduced by these lines.
fn spell_tooltip_view(
    spell_id: u32,
    spells: &Spells,
    vctx: &mut ViewCtx,
) -> Option<benilla_ui::script::SpellTooltipView> {
    let d = spells.catalog.get(spell_id)?;
    let home_area = vctx.home_area;
    let form = vctx.form;
    let ctx = benilla_formats::TokenContext {
        durations: &spells.durations,
        radii: &spells.radii,
        lookup: &|id| spells.catalog.get(id),
        home_area,
    };
    // The cost cell (law §3.3, the `0x52e8ad` caller): the RESOLVED cost — `GetPowerCost`'s
    // number, the same `power_cost` the usable walk compares (0948) — through the power-type key
    // array (`0x85416c`: Mana/Rage/Focus/Energy/Happiness) with HEALTH as the out-of-range
    // fallback (the `jl`/`cmp 5` fork at `0x52e8fc` — Life Tap's and Bloodrage's −2 → "N Health";
    // B192, 1074). Rage displays wire÷10 (the `0x6e7130` per-type divisor), applied to the flat
    // and per-second values alike; a `manaPerSecond` column composes the `_PER_TIME` string
    // ("11 Health, plus 5 per sec" — Health Funnel), including the cost-0 case. Zero cost with
    // zero per-second leaves the cell EMPTY. The ref never prints a percentage — a pct-only cost
    // resolves to its flat number like any other, which is why the "% of base mana" line this
    // replaces was unfaithful (B152). No store (a DBC-only view) degrades to the flat cost.
    // HAPPINESS_COST has no GlobalStrings entry and no 5875 player spell reaches powerType 4;
    // the unit word is a dead arm kept for the array's shape.
    let resolved_cost = vctx
        .store
        .map_or(d.mana_cost, |s| crate::ui_action::usable::power_cost(d, s));
    let cost = {
        let div = if d.power_type == 1 { 10 } else { 1 };
        let unit = match d.power_type {
            0 => "Mana",
            1 => "Rage",
            2 => "Focus",
            3 => "Energy",
            4 => "Happiness",
            _ => "Health",
        };
        if resolved_cost == 0 && d.mana_per_second == 0 {
            None
        } else if d.mana_per_second > 0 {
            Some(format!(
                "{} {unit}, plus {} per sec",
                resolved_cost / div,
                d.mana_per_second / div
            ))
        } else {
            Some(format!("{} {unit}", resolved_cost / div))
        }
    };
    let range = spells.ranges.get(d.range_index).and_then(|r| {
        if r.is_melee() {
            Some("Melee Range".to_string())
        } else if r.max > 0.0 {
            // The law's fork (`0x854fb4`): a nonzero min prints the "%d-%d" pair (Charge: 8-25).
            Some(if r.min > 0.0 {
                format!("{}-{} yd range", r.min as i32, r.max as i32)
            } else {
                format!("{} yd range", r.max as i32)
            })
        } else {
            None
        }
    });
    // Law §3.4's own gate — wider than the spellbook's `passive`: a TRADE_SKILL or ATTACK
    // Effect[0] omits the line too ([`SpellDisplay::tooltip_omits_cast_line`]). The arm order is
    // the byte ladder `0x52eb45-0x52ec90` (1074): a positive time prints sec/min at the 60 s
    // threshold; a NEGATIVE SpellCastTimes base is the "Instant cast" sentinel (`52ebce`); at
    // zero — next-swing (attr 0x404) → "Next melee", auto-shot (attr 0x2) → "Attack speed",
    // channeled (attrEx 0x44) → "Channeled", else the mana fork: "Instant cast" iff
    // powerType==0 AND the resolved cost is nonzero (`52ec4b` re-reads `GetPowerCost`'s edi) —
    // every other type reads bare "Instant" whatever it costs (Sinister Strike, Life Tap).
    let cast_time = if d.tooltip_omits_cast_line() {
        None
    } else {
        let base = spells
            .cast_times
            .get(d.casting_time_index)
            .map(|c| c.base_ms as i32)
            .unwrap_or(0);
        Some(if base > 0 {
            if base >= 60_000 {
                format!("{} min cast", trim_secs(f64::from(base) / 60_000.0))
            } else {
                format!("{} sec cast", trim_secs(f64::from(base) / 1000.0))
            }
        } else if base < 0 {
            "Instant cast".to_string()
        } else if d.on_next_swing() {
            "Next melee".to_string()
        } else if d.tooltip_on_next_ranged() {
            "Attack speed".to_string()
        } else if d.tooltip_channeled() {
            "Channeled".to_string()
        } else if d.power_type == 0 && resolved_cost > 0 {
            "Instant cast".to_string()
        } else {
            "Instant".to_string()
        })
    };
    // The cooldown cell reads BOTH recovery columns (law §3.4: `max([+0x4c],[+0x50])>0` —
    // Charge's 15 s is CategoryRecoveryTime; its RecoveryTime is 0).
    let recovery_ms = d.recovery_ms.max(d.category_recovery_ms);
    let cooldown = (recovery_ms > 0).then(|| {
        let secs = f64::from(recovery_ms) / 1000.0;
        if secs >= 60.0 {
            format!("{} min cooldown", trim_secs(secs / 60.0))
        } else {
            format!("{} sec cooldown", trim_secs(secs))
        }
    });
    // The required-form line (law §3.6): the Stances mask's form names off
    // SpellShapeshiftForm.dbc, joined; bit b = form id b+1. Met against the CURRENT form.
    //
    // `Stances != 0` is NOT the gate on its own — the column is overloaded. When `AttributesEx2`
    // bit 19 is set the mask is *permissive* ("may ALSO be cast in these forms"), which is how
    // 5875 encodes Shadowform-castable priest spells, Spirit-of-Redemption-castable heals and
    // Moonkin-castable balance spells; reading those as requirements put a red "Requires
    // Shadowform" on Inner Fire and "Requires Spirit of Redemption" on Flash Heal (1483).
    //
    // Now byte-carved as wow-re §3-REQFORM, `[0x52f10a, 0x52f2ae)`: `52f115` reads AttributesEx2,
    // ands `0x80000`, and jumps *over the whole name loop* — the loop is the only writer of the
    // "nothing appended" flag `[ebp-8]`, so the AddLine is skipped entirely. The line is
    // **suppressed, not recoloured**. Three earned negatives came with it: StancesNot `+0x30` is
    // never read here, `Attributes` bit 16 is never tested, and `0x612480` is never called — the
    // builder duplicates the gate inline rather than sharing the usable walk's helper, which is
    // why the two could drift apart in the first place.
    //
    // Two ref behaviours we knowingly don't model, both unreachable for us: the composed text is
    // capped at 0x80 bytes (our longest join is "Battle Stance, Berserker Stance"), and a *met*
    // requirement is dropped in the COMPACT tooltip — benilla has no compact spell tooltip.
    let requires_form = (d.stances != 0 && !d.form_mask_is_permissive())
        .then(|| {
            let names: Vec<&str> = (0..32u32)
                .filter(|b| d.stances & (1 << b) != 0)
                .filter_map(|b| spells.forms.get(&(b + 1)).map(|f| f.name.as_str()))
                .filter(|n| !n.is_empty())
                .collect();
            (!names.is_empty()).then(|| format!("Requires {}", names.join(", ")))
        })
        .flatten();
    let form_met = form != 0 && d.stances & (1u32 << (u32::from(form) - 1)) != 0;
    // The equipped-item-class half of law §3.6 — "Requires Wands" on the wand Shoot, and "Requires
    // Melee Weapon" on Parry. A mask with several bits set is NOT a reason to print nothing (what we
    // used to do): the reference names the whole mask through ItemSubClassMask.dbc first, and only
    // falls back to comma-joining the individual subclasses. Law §3-EQUIPITEM, in the catalog.
    // §3-EQUIPITEM's three entry gates, all of which skip the line: Targets bit 0x10 set, class < 0,
    // or an empty mask.
    let requires_item = (d.targets & TARGET_ITEM == 0 && d.equipped_item_class >= 0)
        .then(|| {
            vctx.sub_classes?
                .requirement_name(d.equipped_item_class as u32, d.equipped_item_subclass_mask)
        })
        .flatten()
        .map(|name| format!("Requires {name}"));
    // The chance-to-X line (law line 10, §3-CHANCE): `Effect[0]` picks which of the player's four
    // avoidance/crit percentages to print, and — except for ATTACK, which bypasses the gate — the
    // spell must be passive. The percentages are already percents on the wire.
    let chance = chance_line(d, vctx.store);
    let item_met = vctx.store.is_none_or(|s| {
        crate::ui_action::usable::equipped_item_fits(d, s, vctx.items, vctx.commands)
    });
    // Reagents (law §3.8): the named slots, `count > 1` suffixed, a slot the player is short of
    // wrapped in the builder's inline red. A reagent whose item template hasn't streamed yet is
    // simply absent from this snapshot — `feed_spell_tooltips` re-pushes when it lands, which is
    // our shape of the ref's own query-then-redisplay callback.
    let reagents = {
        let mut parts: Vec<String> = Vec::new();
        for (entry, count) in d.reagents.iter().copied().filter(|&(e, _)| e != 0) {
            let Some(name) = vctx
                .items
                .template(entry, 0, vctx.commands)
                .map(|t| t.name.clone())
            else {
                continue;
            };
            let text = if count > 1 {
                format!("{name} ({count})")
            } else {
                name
            };
            let short = vctx.store.is_some_and(|s| {
                count_of(&s.0, vctx.items, entry, InventoryScope::CARRIED) < count
            });
            parts.push(if short {
                format!("|cffff2020{text}|r")
            } else {
                text
            });
        }
        (!parts.is_empty()).then(|| format!("Reagents: {}", parts.join(", ")))
    };
    Some(benilla_ui::script::SpellTooltipView {
        name: d.name.clone(),
        rank: d.rank.clone(),
        // The aura variant's right column (law §3-BUFF) — gated inside the catalog by
        // SpellDispelType.dbc's own `[+0x28]`, so Stealth-class auras hand back None.
        dispel_type: spells.catalog.dispel_name(d).map(str::to_string),
        cost,
        range,
        cast_time,
        cooldown,
        requires_item,
        item_met,
        requires_form,
        form_met,
        chance,
        reagents,
        description: d
            .description
            .as_deref()
            .map(|t| benilla_formats::substitute(t, d, &ctx))
            .unwrap_or_default(),
        aura_description: d
            .aura_description
            .as_deref()
            .map(|t| benilla_formats::substitute(t, d, &ctx))
            .unwrap_or_default(),
    })
}

/// `Targets` bit `0x10` — set ⇒ the equipped-item requirement line is skipped whatever the class
/// and mask say (§3-EQUIPITEM's first gate; the bit test is verified, the `TARGET_FLAG_ITEM` name
/// wow-re flags as inferred).
const TARGET_ITEM: u32 = 0x10;

/// `SPELL_EFFECT_DODGE` / `_PARRY` / `_BLOCK` / `_ATTACK` — the four `Effect[0]` values that select
/// a chance-to-X line (law §3-CHANCE's jump table).
const EFFECT_DODGE: u32 = 20;
const EFFECT_PARRY: u32 = 22;
const EFFECT_BLOCK: u32 = 23;
const EFFECT_ATTACK: u32 = 78;

/// The chance-to-X line (law line 10 / §3-CHANCE) — `None` when the spell names none of the four
/// effects, when the gate rejects it, or before the player's descriptor has streamed.
///
/// The predicate is the reference's, and its asymmetry is the point: **ATTACK bypasses the passive
/// gate** while dodge/parry/block require it. That is why Attack — whose `Attributes` carry `0x10`,
/// not the passive `0x40` — still shows a crit line, which is exactly the missing line reported.
fn chance_line(d: &benilla_formats::SpellDisplay, store: Option<&ObjectStore>) -> Option<String> {
    let (label, percentage) = match d.effects[0] {
        EFFECT_ATTACK => ("crit", store?.0.player_crit_percentage()?),
        EFFECT_DODGE if d.passive => ("dodge", store?.0.player_dodge_percentage()?),
        EFFECT_PARRY if d.passive => ("parry", store?.0.player_parry_percentage()?),
        EFFECT_BLOCK if d.passive => ("block", store?.0.player_block_percentage()?),
        _ => return None,
    };
    // The reference's `%.2f%%` lives in its GlobalStrings, not the binary — this is the same shape.
    Some(format!("{percentage:.2}% chance to {label}"))
}

/// The `%.3g`-style terse seconds (1.5 → "1.5", 2.0 → "2") — the SPELL_CAST_TIME/RECAST shape.
fn trim_secs(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

/// The push loop's change detectors — everything a view SNAPSHOTS at build time. A change to any
/// of them means every pushed view is stale, so the loop re-pushes the lot (the reference simply
/// re-runs its builder on every hover, so a stale snapshot is a benilla-only failure mode).
#[derive(Default)]
struct SpellFeedMemory {
    /// Spell ids whose view has been pushed at least once.
    pushed: std::collections::HashSet<u32>,
    /// The bind point `$z` substitutes against.
    home: Option<String>,
    /// The current shapeshift form (law §3.6's stance line, white/red).
    form: Option<u8>,
    /// The 19 worn-slot guids (law §3.6's item line, white/red).
    worn: Option<[u64; 19]>,
    /// The player's block/dodge/parry/crit percentages, as raw bit patterns so the diff needs no
    /// float comparison (law line 10's printed value — it moves with gear, buffs and talents).
    avoidance: Option<[u32; 4]>,
    /// Per reagent entry currently on show: `(owned count, item name resolved)` — law §3.8's
    /// inline red plus the ask-once template landing.
    reagents: std::collections::BTreeMap<u32, (u32, bool)>,
}

/// The spell-tooltip store's push half: every spell the UI can HOVER is owed its view at
/// arrival — the known book (spellbook/bar), the class's talent rank spells (`SetTalent`'s
/// display + next-rank reads), and the live aura spells (`SetPlayerBuff`) — so a first hover
/// never misses, exactly like the reference's all-local reads. The renderers' recorded asks
/// (the odd id outside those sets) answer through the same build as the fallback.
#[allow(clippy::too_many_arguments)]
fn feed_spell_tooltips(
    script: Option<NonSendMut<UiScript>>,
    actions: Option<Res<PlayerActions>>,
    spells: Option<Res<Spells>>,
    talents: Option<Res<crate::ui_talent::Talents>>,
    auras: Option<Res<crate::ui_aura::PlayerAuraCache>>,
    selection: Res<crate::target::Selection>,
    stores: Query<&ObjectStore>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    home_bind: Option<Res<crate::net::HomeBind>>,
    area_names: Option<Res<crate::ui_quest_log::QuestHeaderNamesRes>>,
    mut items: ResMut<Items>,
    sub_classes: Option<Res<crate::ui_items::ItemSubClasses>>,
    commands: Res<NetCommands>,
    mut memory: Local<crate::ui_script::VmMemo<SpellFeedMemory>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let memory = memory.get(&script);
    let Some(spells) = spells.as_deref() else {
        return;
    };
    let mut wanted: Vec<u32> = script.take_spell_tooltip_asks();
    if let Some(actions) = actions.as_deref() {
        wanted.extend(
            actions
                .spells
                .iter()
                .copied()
                .filter(|s| !memory.pushed.contains(s)),
        );
    }
    // The talent window's hoverables: every rank spell of the class's pages (a talent tooltip
    // reads rank max(1, current) + the next rank — pushing ALL ranks stays correct as ranks
    // are learned, and the whole class set is ~250 ids once).
    if let (Some(talents), Ok(store)) = (talents.as_deref(), self_q.single()) {
        let race = store.0.unit_race().unwrap_or(0);
        let class = store.0.unit_class().unwrap_or(0);
        for tab in talents.catalog.tabs_for_class(race, class) {
            for t in talents.catalog.talents_in_tab(tab.id) {
                wanted.extend(
                    t.ranks
                        .iter()
                        .copied()
                        .filter(|s| *s != 0 && !memory.pushed.contains(s)),
                );
            }
        }
    }
    // The buff bar's hoverables: the live aura spells, at arrival.
    if let Some(auras) = auras.as_deref() {
        wanted.extend(auras.spell_ids().filter(|s| !memory.pushed.contains(s)));
    }
    // The minimap tracking icon's hover (SetTrackingSpell): the tracking aura never enters the
    // display cache above (the rebuild's tracking-effect exclusion, `ui_aura`), so pre-feed from
    // the player's raw aura array — covers it at arrival, at worst a few extra views for
    // display-filtered auras nothing hovers.
    if let Ok(store) = self_q.single() {
        wanted.extend(
            store
                .0
                .unit_auras()
                .map(|a| a.spell_id)
                .filter(|s| !memory.pushed.contains(s)),
        );
    }
    // The target frame's aura rows (SetUnitBuff/SetUnitDebuff): the target's live aura spells,
    // at selection/arrival — same first-hover guarantee as the buff bar's.
    if let Some(store) = selection.target.and_then(|e| stores.get(e).ok()) {
        wanted.extend(
            store
                .0
                .unit_auras()
                .map(|a| a.spell_id)
                .filter(|s| !memory.pushed.contains(s)),
        );
    }
    let home_area: Option<String> = home_bind
        .as_deref()
        .and_then(|b| b.0)
        .and_then(|id| area_names.as_deref()?.0.resolve(id as i32))
        .map(str::to_string);
    // A bind-point change re-substitutes every pushed view ($z — Astral Recall's shape).
    if memory.home != home_area {
        memory.home = home_area.clone();
        wanted.extend(memory.pushed.drain());
    }
    // A stance/form change re-pushes too: the required-form line's white/red tracks the CURRENT
    // form (law §3.6), and the views are static snapshots until re-pushed.
    let form = self_q
        .single()
        .map(|s| s.0.unit_shapeshift_form())
        .unwrap_or(0);
    if memory.form != Some(form) {
        memory.form = Some(form);
        wanted.extend(memory.pushed.drain());
    }
    let self_store = self_q.single().ok();
    // …and so do the two BAG-dependent halves, which the views likewise snapshot: the worn set
    // (law §3.6's item line flips white/red on a weapon swap) and the owned counts of the
    // reagents currently on show (law §3.8's inline red, plus the item names themselves landing
    // from the ask-once template cache — the ref's query-then-redisplay callback in our shape).
    // Both signatures are tiny: 19 slot guids, and one count per DISTINCT reagent in play.
    let worn =
        self_store.map(|s| std::array::from_fn(|i| s.0.player_inv_slot(i as u8).unwrap_or(0)));
    if memory.worn != worn {
        memory.worn = worn;
        wanted.extend(memory.pushed.drain());
    }
    // …and so does the chance-to-X line's percentage (law line 10), which the views snapshot the
    // same way: a weapon swap, a buff or a talent moves it.
    let avoidance = self_store.map(|s| {
        [
            s.0.player_block_percentage(),
            s.0.player_dodge_percentage(),
            s.0.player_parry_percentage(),
            s.0.player_crit_percentage(),
        ]
        .map(|v| v.unwrap_or(0.0).to_bits())
    });
    if memory.avoidance != avoidance {
        memory.avoidance = avoidance;
        wanted.extend(memory.pushed.drain());
    }
    let watched: Vec<u32> = memory.reagents.keys().copied().collect();
    let reagent_state: std::collections::BTreeMap<u32, (u32, bool)> = watched
        .into_iter()
        .map(|entry| {
            let named = items.template(entry, 0, &commands).is_some();
            let owned = self_store.map_or(0, |s| {
                count_of(&s.0, &items, entry, InventoryScope::CARRIED)
            });
            (entry, (owned, named))
        })
        .collect();
    if memory.reagents != reagent_state {
        memory.reagents = reagent_state;
        wanted.extend(memory.pushed.drain());
    }
    let mut vctx = ViewCtx {
        home_area: home_area.as_deref(),
        form,
        store: self_store,
        items: &mut items,
        commands: &commands,
        sub_classes: sub_classes.as_deref().map(|c| &c.0),
    };
    for id in wanted {
        if let Some(view) = spell_tooltip_view(id, spells, &mut vctx) {
            // Register this spell's reagents in the watch set, seeded with the state the view was
            // just built against — so the next recompute re-pushes on a REAL change only.
            if let Some(d) = spells.catalog.get(id) {
                for (entry, _) in d.reagents.iter().copied().filter(|&(e, _)| e != 0) {
                    if let std::collections::btree_map::Entry::Vacant(slot) =
                        memory.reagents.entry(entry)
                    {
                        let named = vctx.items.template(entry, 0, vctx.commands).is_some();
                        let owned = vctx.store.map_or(0, |s| {
                            count_of(&s.0, vctx.items, entry, InventoryScope::CARRIED)
                        });
                        slot.insert((owned, named));
                    }
                }
            }
            script.set_spell_tooltip(id, view);
            memory.pushed.insert(id);
        }
    }
}

/// What the tooltip was last driven for — the change detector (the byte law rebuilds once per
/// hover-target change).
#[derive(Default, PartialEq, Clone, Copy)]
enum LastHover {
    #[default]
    None,
    Unit(u64),
    Go(u64),
    /// A hovered **corpse object** — its own plate, keyed on the corpse's guid (1729).
    Corpse(u64),
}

/// The snapshot fields the unit tooltip's LINES read (everything except the bar's
/// health/power) — the rebuild key: a change here means the rendered lines are stale.
fn lines_view(s: &UnitState) -> UnitState {
    UnitState {
        health: 0,
        max_health: 0,
        power: 0,
        max_power: 0,
        ..s.clone()
    }
}

/// `LockType` index → the requirement word (the `LOCKED_WITH_SPELL[_KNOWN]` "Requires %s" text
/// for skill locks — vanilla's small fixed vocabulary; item-key locks name the item instead).
fn lock_type_word(index: u32) -> Option<&'static str> {
    Some(match index {
        1 => "Lockpicking",
        2 => "Herbalism",
        3 => "Mining",
        4 => "Disarm Trap",
        _ => return None,
    })
}

/// The **"Locked" line's colour** (`0x52ab03`-`0x52ab43`, decision 0770).
///
/// The builder seats red `0xc0d3a8` as the default *before* it calls the resolver, then re-colours
/// on the answer. Every non-`Unmet` answer lands on the same green `0xc0d420`, by two separate
/// branches that agree: an opener was found with **no item** and no matched spell (`0x52ab22 je`
/// — the no-requirement case), or an opener was found **with** an item, i.e. a KEY (`0x52ab29
/// jne`). Only a lock nothing can open keeps the red.
///
/// `None` = a flag-locked object with no `Lock.dbc` row at all, which is the reference's
/// no-requirement arm and therefore green — the flag alone is not a refusal.
///
/// **Not modelled:** the third branch, where a *skill* opener satisfied the lock and the colour
/// becomes the difficulty ramp `0x529fa0` (grey `0xc0cf50` / green `0xc0d420` / yellow `0xc0cf18`
/// / orange `0xc0d3a4` / red `0xc0d3a8`, banded at the requirement +0/25/50/100 — the trade-skill
/// ladder). Green is that ramp's comfortable rung, so a well-skilled opener already reads right
/// and only a marginal one reads too green. Pinned in decision 0770; deliberately left for its own
/// change, since it needs the resolver to hand back the margin it currently discards.
fn locked_line_tint(outcome: Option<crate::target::lock::LockOutcome>) -> TooltipTint {
    match outcome {
        Some(crate::target::lock::LockOutcome::Unmet) => TooltipTint::Red,
        _ => TooltipTint::LockOpen,
    }
}

#[allow(clippy::too_many_arguments)]
fn drive_mouseover_tooltip(
    script: Option<NonSendMut<UiScript>>,
    hovered: Res<Hovered>,
    hovered_go: Res<HoveredObject>,
    // The cursor-arm seat of the GO anchor fork (decision 0766).
    window: Query<&Window, With<PrimaryWindow>>,
    stores: Query<&ObjectStore>,
    // The stored GAMEOBJECT_STATE the lock lines' Action gate reads (decision 0752).
    anims: Query<&crate::go_anim::GoAnim>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    mut names: ResMut<NameCache>,
    commands: Res<NetCommands>,
    factions: Option<Res<Factions>>,
    reputations: Res<Reputations>,
    // The lock chain's own data set, shared verbatim with the click router (`target::lock`) so the
    // hover and the click can never disagree about whether a lock is satisfiable — the same reason
    // `usable` and the click share one resolver (0752). Carries the go-template, Lock.dbc and
    // item caches this system used to take as three separate params.
    mut go_inputs: crate::target::lock::GoLockInputs,
    // The known-spell set the resolver's SKILL arm scans.
    player_actions: Res<crate::ui_action::PlayerActions>,
    // The cursor seat crosses the VM seam (0582/0584): the anchor below is UI units, not px.
    ui_scale: Res<crate::ui_script::UiScaleCvar>,
    mut last: Local<crate::ui_script::VmMemo<LastHover>>,
    mut last_lines: Local<crate::ui_script::VmMemo<Option<UnitState>>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let last = last.get(&script);
    let last_lines = last_lines.get(&script);
    let self_store = self_q.iter().next();

    // The hovered UNIT's snapshot (a hovered non-unit resolves no store here).
    let unit = hovered.target.zip(hovered.guid).and_then(|(entity, guid)| {
        let store = stores.get(entity).ok()?;
        let name = names.resolve(guid, &commands).map(str::to_string);
        let reaction =
            ring_reaction(factions.as_deref(), &reputations, Some(store), self_store) + 1;
        let mut s = snapshot(store, name, reaction);
        enrich_unit(&mut s, guid, &names, store, factions.as_deref(), self_store);
        Some((guid, s))
    });
    // The hovered GAMEOBJECT, when it is the nearer pick (the click router's own arbitration).
    // Deliberately NOT gated on the highlightable predicate — §5-VERIFIED (wow-re 2026-07-20,
    // 0558/0559): the mouseover publisher `0x492890` dispatches the GO tooltip builder `0x52aa20`
    // by object KIND on both branches; highlightable is never read on the tooltip path (it gates
    // the cursor and the click only). So a GENERIC(5) signpost, a pre-quest INTERACT_COND chest,
    // and an IN_USE object all show the gold name plate while showing NO interact cursor — 0466's
    // "no cursor AND no tooltip" coupling was the regression. Transports never reach here — they
    // are excluded from the pick set itself (0466's correct half).
    // "Nothing else was picked, or the GameObject is the nearer pick." **A hovered corpse counts
    // as something else picked** (decision 1723) — it has no tooltip of its own yet, and the
    // `unit.is_none()` short-circuit would otherwise hand a farther GameObject the gold plate the
    // moment the ray landed on a body instead of a unit.
    let go = hovered_go.target.zip(hovered_go.guid).filter(|_| {
        (unit.is_none() && hovered.corpse.is_none()) || go_is_nearest(&hovered, &hovered_go)
    });

    if let Some((guid, state)) = unit.filter(|_| go.is_none()) {
        // Push first (the engine's builder + the recolor's UnitReaction read the token), then
        // rebuild on a hover-target change OR when a LINE-affecting field changes under the
        // same hover — the late-arriving name/creature-info case: the first render often
        // precedes the SMSG_NAME_QUERY/CREATURE_QUERY answers, and a once-per-guid render
        // would keep the stale (even empty) lines for the whole hover. Health/power stay OUT
        // of the key: the byte law's watcher drives the BAR without a line rebuild.
        let key = lines_view(&state);
        script.set_unit("mouseover", Some(state));
        if *last != LastHover::Unit(guid) || last_lines.as_ref() != Some(&key) {
            script.world_tooltip_unit("mouseover");
            *last = LastHover::Unit(guid);
            *last_lines = Some(key);
        }
        return;
    }
    // The **corpse plate** — "Corpse of <owner>" (the reference's own builder `0x52aef0`, wow-re
    // `corpse-click-and-reclaim.md` Q6, §5 cross-checked). Ahead of the GameObject arm because the
    // corpse is the nearer pick whenever `go` came back empty.
    //
    // It is a *name plate and nothing else*: no health bar, no reaction recolour, no level or
    // creature-type line — a corpse is not a unit, and the publisher deliberately fires **no
    // event** for it, so it never becomes the Lua `mouseover` unit token either (`UnitName
    // ("mouseover")` on a body is nil in the reference, and stays nil here). Corner-seated like a
    // unit's, not cursor-seated like a signpost's.
    if let Some((entity, guid)) = hovered
        .corpse
        .zip(hovered.corpse_guid)
        .filter(|_| go.is_none())
    {
        let Some(store) = stores.get(entity).ok() else {
            return;
        };
        // A bone pile with nothing to take publishes no mouseover at all — no plate, no brighten.
        if !crate::target::corpse_mouseover_eligible(store) {
            if !matches!(*last, LastHover::None) {
                script.world_tooltip_fade();
                *last = LastHover::None;
            }
            return;
        }
        if *last == LastHover::Corpse(guid) {
            return; // already showing; nothing re-seats a corner plate
        }
        // The owner's name is the ask-once cache like any other player name — a corpse carries
        // `CORPSE_FIELD_OWNER`, never a name. Until the answer lands there is no plate to show
        // (`last` stays `None`, so it appears the moment the name arrives).
        let Some(owner) = store.0.corpse_owner() else {
            return;
        };
        let Some(name) = names.resolve(owner, &commands).map(str::to_string) else {
            return;
        };
        script.world_tooltip_gameobject(&format!("Corpse of {name}"), &[], None);
        *last = LastHover::Corpse(guid);
        return;
    }
    if let Some((entity, guid)) = go {
        // Which arm of the anchor fork this object takes (decision 0766). The reference asks the
        // object's own `[obj->vtbl+0x5c]`; what selects it is not pinned, so we key on the one
        // distinction the director's two reference observations agree on — a **GENERIC(5)**
        // signpost follows the cursor, an interactable GameObject sits in the corner.
        //
        // Not merely a guess-shaped proxy: after 0762 the only objects that are eligible for a
        // tooltip *and* never highlightable are GENERIC ones, so "GENERIC" and "not interactable"
        // pick out the same set here. They diverge only for the three always-eligible types
        // (SPELL_FOCUS 8 / DUEL_ARBITER 16 / FISHINGHOLE 25), which is exactly where a pin would
        // settle it. Flagged INTERIM in 0766 rather than presented as verified.
        let cursor_seated =
            stores.get(entity).map(|s| s.0.gameobject_type_id()) == Ok(GO_TYPE_GENERIC);
        // Window px → the VM's y-up 768-virtual units (÷s, the input seam's own conversion) —
        // the anchor this point seats is resolved in UI units, so a raw-px point lands the
        // plate (s−1)× the cursor's distance from the bottom-left corner away from it.
        let cursor_ui = cursor_seated
            .then(|| {
                window.iter().next().and_then(|w| {
                    let s = crate::ui_script::seam_scale(w.height(), ui_scale.0);
                    w.cursor_position()
                        .map(|c| (c.x / s, (w.height() - c.y) / s))
                })
            })
            .flatten();
        if *last == LastHover::Go(guid) {
            // The cursor arm follows the pointer; the corner arm has nothing to re-seat.
            if let Some((x, y)) = cursor_ui {
                script.world_tooltip_move(x, y);
            }
            return;
        }
        if cursor_seated && cursor_ui.is_none() {
            return; // cursor off-window: nothing to seat the pointer-anchored plate against
        }
        let Some(template) = go_inputs.templates.get(guid).cloned() else {
            // Template in flight: ask once and retry next frame (`last` stays, so the show
            // fires the moment the name lands).
            go_inputs.templates.request(guid, &commands);
            return;
        };
        // The lock lines, transcribed from the builder `0x52aa20` (decision 0756). Two blocks, in
        // the binary's order, and both are narrower than the sweep we used to print:
        //
        //  A) **"Locked"** — emitted iff `GAMEOBJECT_FLAGS & GO_FLAG_LOCKED` (`0x52aae5`:
        //     `shr 1; test dl,1`). That flag gates this line and nothing else.
        //  B) **ONE requirement line, from Lock.dbc SLOT 0 ONLY** (`[lockRow+4]` / `[lockRow+0x24]`
        //     — the builder never walks the other seven), and only when slot 0 passes the same
        //     per-slot Action gate the resolver uses (`0x52ab7e` → `0x5f81d0`, decision 0752):
        //       · KEY → **white** `LOCKED_WITH_ITEM` "Requires <item>" (`0x854988`; `0x52acd9`
        //         pushes `0xc0cf60` = white)
        //       · SKILL, opener unknown **and** the object is flag-locked → **nothing at all**
        //         (`0x52abf7: jne done`) — a padlocked door says "Locked" and names its key, never
        //         its lockpicking rank
        //       · SKILL, opener unknown, not flagged → **red** "Requires <word>" (the herb node)
        //
        // That is why the reference shows *"Locked" + "Requires Key to Searing Gorge"* on the
        // Searing Gorge door and not the `Requires Lockpicking (225)` line we used to add: the
        // Pick Lock requirement is that lock's slot **1**, and the builder never looks past 0.
        let go_store = stores.get(entity).ok();
        let flags = go_store.map_or(0, |s| s.0.gameobject_flags());
        let flag_locked = flags & GO_FLAG_LOCKED != 0;
        let state = go_store.map_or(benilla_formats::GO_STATE_ACTIVE, |s| {
            crate::go_anim::go_state(anims.get(entity).ok(), s)
        });
        let slots = go_inputs
            .locks
            .as_ref()
            .filter(|_| template.lock_id != 0)
            .and_then(|l| l.0.slots(template.lock_id));
        let mut lines: Vec<(String, TooltipTint)> = Vec::new();
        if flag_locked {
            // **The "Locked" line is coloured by whether you can actually open it** (director-
            // reported: a door you hold the key for read red). The builder sets red `0xc0d3a8` as
            // the default (`0x52ab03`), calls the SAME resolver the click uses (`0x52ab14` →
            // `0x5f83d0`) and re-colours on its answer (`0x52ab19`-`0x52ab43`):
            //   · resolver says NO opener   -> keep red
            //   · a KEY item satisfied it   -> `0xc0d420` green (spell out-param set AND item
            //     out-param set: `0x52ab29 jne` takes the green arm)
            //   · no lock requirement at all-> the same green (`0x52ab22 je`)
            //   · a SKILL opener satisfied it -> the difficulty ramp `0x529fa0` — NOT modelled;
            //     see this line's follow-up note in decision 0770. Green is its second rung, so a
            //     comfortably-skilled opener already reads correctly; a marginal one reads too
            //     green rather than yellow/orange.
            let facts = crate::target::lock::go_facts(go_store.map(|s| (s, state)));
            let mut matched = None;
            let outcome = slots.map(|slots| {
                crate::target::lock::resolve_lock(
                    slots,
                    &player_actions.spells,
                    go_inputs.spells.as_deref(),
                    go_inputs.skill_lines.as_ref().map(|s| &s.catalog),
                    self_store,
                    &go_inputs.items,
                    facts,
                    &mut matched,
                )
            });
            lines.push(("Locked".to_string(), locked_line_tint(outcome)));
        }
        if let Some(slot0) = slots
            .map(|s| s[0])
            .filter(|s| s.available(state, flag_locked))
        {
            match slot0.key_type {
                benilla_formats::LOCK_KEY_ITEM => {
                    if let Some(t) = go_inputs.items.template(slot0.index, 0, &commands) {
                        lines.push((format!("Requires {}", t.name), TooltipTint::White));
                    }
                }
                // The opener-*known* arm additionally wants the reference's skill-margin colour
                // ramp (`0x529fa0`) — now pinned (decision 0770) but not modelled here; what we
                // model is the unknown arm, which is what a hovering player almost always is. A
                // flagged object stays silent there, exactly as the binary does.
                benilla_formats::LOCK_KEY_SKILL if !flag_locked => {
                    if let Some(word) = lock_type_word(slot0.index) {
                        lines.push((format!("Requires {word}"), TooltipTint::Red));
                    }
                }
                _ => {}
            }
        }
        script.world_tooltip_gameobject(&template.name, &lines, cursor_ui);
        *last = LastHover::Go(guid);
        return;
    }
    if !matches!(*last, LastHover::None) {
        // Hover lost: arm the fade (the byte law's timestamped fade, never an instant hide).
        // The "mouseover" state stays until the next hover overwrites it, so the fading
        // lines/bar keep their last content.
        script.world_tooltip_fade();
        *last = LastHover::None;
    }
}

#[cfg(test)]
mod tests;
