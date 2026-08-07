//! The app-side **Craft window feed** (decision 0437 phase 3) — Enchanting's client-built book,
//! [`crate::ui_tradeskill`]'s CraftFrame twin. Same law: the effect-47 opener for skill line 333
//! opens it client-side (no packet — routed here by `ui_tradeskill::open_trade_skill`); the
//! recipe list is the player's known attr-`0x20` spells joined to line 333; reagents/tools off
//! the parsed `Spell.dbc` columns; the description is the spell's own `$`-token text, resolved
//! through the tooltip arc's substitution engine.
//!
//! **The item pick** (the one thing the TradeSkill window doesn't have): an enchant is an
//! item-targeted cast (`SPELL_EFFECT_ENCHANT_ITEM`/`_TEMPORARY`, `Targets = 0x10`). Since
//! decision 0923 this window owns none of that machinery — `DoCraft` goes down the ONE cast
//! ladder like every other caster surface, and the resolver's item arm raises the ONE targeting
//! cursor, which the bag and paper-doll click seams complete ([`crate::ui_action::targeting`]).
//! The private `PendingItemCast` this file used to carry — a second targeting state with its own
//! arm, its own bag-click completion and its own cursor overlay, bypassing every ladder rung
//! including the reagent check an enchant most needs — is gone.
//!
//! **Named INTERIM:** no replace-enchant confirm popup yet (event 0x193 is pure client gating and
//! 5875 has no replace opcode — it needs the item's enchantment fields, a later slice).

use bevy::prelude::*;

use benilla_formats::{
    SPELL_ATTR_IS_TRADESKILL, SPELL_EFFECT_CREATE_ITEM, SPELL_EFFECT_ENCHANT_ITEM,
    SPELL_EFFECT_ENCHANT_ITEM_TEMPORARY, SPELL_EFFECT_LEARN_SPELL,
};
use benilla_protocol::messages::PLAYER_SKILL_SLOTS;
use benilla_ui::script::{CraftReagent, CraftRecipe, CraftState, CraftTooltip, UiScript};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{NetCommands, ObjectStore, SelfPlayer};
use crate::ui_action::{cast_target, CastCommit, CastLadder, PlayerActions, Spells};
use crate::ui_items::{count_of, item_icon};
use crate::ui_script::UiInput;
use crate::ui_spellbook::SkillLines;
use crate::ui_tradeskill::SpellFocus;
use crate::ui_unit::UnitFeed;

// NOTE on the admission filter below: Craft recipes (enchants, the rod crafts) do NOT carry
// `SPELL_ATTR_IS_TRADESKILL` — they carry **`castUI != 0`** instead (pinned on the live 5875 data:
// 7418/7421 castUI=3, attributes bare 0x10000; exactly the spellbook add-gate's third exclusion
// leg). INTERIM admission pending a wow-re detail pass: a known spell joins the craft list when its
// SLA line matches AND (`castUI != 0` OR the tradeskill bit) and it is not an opener.

/// The open Craft window: the skill line whose recipes it lists (`None` = closed). Routed here
/// by the opener's `EffectMiscValue[0] != 0` (byte-VERIFIED — wow-re `tradeskill` TU-A); a Beast
/// Training opener also routes here and simply lists nothing (its pet-ability content is its own
/// arc, 0437 out-of-scope). Client-local state, no wire.
#[derive(Resource, Default)]
pub(crate) struct CraftOpen {
    pub(crate) line: Option<u32>,
}

pub(crate) struct UiCraftPlugin;

impl Plugin for UiCraftPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CraftOpen>().add_systems(
            Update,
            (
                feed_craft.in_set(UnitFeed).before(UiInput),
                drain_craft.after(UiInput),
            ),
        );
    }
}

/// The line's `(rank, max, bonus)` off the skill block — the Craft window bands difficulty on
/// the EFFECTIVE skill (rank + bonuses), byte-VERIFIED unlike the TradeSkill window's raw rank
/// (wow-re `tradeskill` TU-C).
fn skill_rank(store: &ObjectStore, skill_id: u32) -> (u32, u32, i32) {
    for i in 0..PLAYER_SKILL_SLOTS {
        if let Some(s) = store.0.player_skill(i) {
            if u32::from(s.skill_id) == skill_id {
                return (
                    u32::from(s.value),
                    u32::from(s.max),
                    i32::from(s.temp_bonus) + i32::from(s.perm_bonus),
                );
            }
        }
    }
    (0, 0, 0)
}

/// **Law D** — the Craft window's row icon, transcribing `GetCraftIcon 0x4f7160` (byte-VERIFIED;
/// wow-re `ui/scratch/spell-icon-substitution-law.md` §2, folded back by decision 1107): **always**
/// this recipe's own `SpellIconID`, straight off `Spell.dbc`.
///
/// The one-liner is the point, and it is not an oversight to be "improved". The Craft window and
/// the TradeSkill window are two translation units of the *same* node, and their icon laws are
/// exact opposites: `+0x19c` (`EffectItemType[0]`) is never read anywhere in `[0x4f7160,
/// 0x4f7204]` — no item lookup, no `"%s%s%s"` path composition, no async callback. So a rod-making
/// `CREATE_ITEM` recipe fronts the *spell's* icon here while the byte-identical recipe in the
/// TradeSkill window fronts the *rod's* ([`crate::ui_tradeskill`]'s Law C). Substituting the
/// product here — which this used to do — is wrong on this surface with no diff to catch it.
fn craft_icon(d: &benilla_formats::SpellDisplay) -> Option<String> {
    d.icon.clone()
}

/// The Craft window's **tooltip law** — `SetCraftSpell 0x533e90`, byte-verified in wow-re
/// (`ui/scratch/trainer-service-tooltip-law.md` §4.1). Like the trainer's, the binding is a
/// selector into the two shared builders and emits no line of its own; unlike the trainer's, it
/// reads the **recipe's own** effect columns:
///
/// ```text
/// for i in 0..3:
///     if Effect[i] == 36 LEARN_SPELL:  return Spell(EffectTriggerSpell[i])   # NOT validated
///     if Effect[i] == 24 CREATE_ITEM:  return Item(EffectItemType[i])
/// return Spell(recipe spell id)                                             # every enchant
/// ```
///
/// Three differences from [`crate::ui_trainer::service_tooltip`] that a shared implementation would
/// erase: this tests the recipe's own `Effect[i]` where the trainer tests the *taught* spell's
/// `Attributes & 0x20`; it never tests `57 LEARN_PET_SPELL` and so never sets `altCaster`; and it
/// hops `EffectTriggerSpell[i]` **without** checking that it resolves, where the trainer's scan
/// falls through to the next slot on an unresolvable trigger. Same shape, different law — the
/// icon side's lesson (decision 1107) applies to the content side too.
fn craft_tooltip(spell_id: u32, d: &benilla_formats::SpellDisplay) -> CraftTooltip {
    for i in 0..3 {
        if d.effects[i] == SPELL_EFFECT_LEARN_SPELL {
            return CraftTooltip::Spell(d.effect_trigger_spell[i]);
        }
        if d.effects[i] == SPELL_EFFECT_CREATE_ITEM {
            return CraftTooltip::Item(d.effect_item_type[i]);
        }
    }
    CraftTooltip::Spell(spell_id)
}

/// Build the craft snapshot — `None` when the window is closed or the catalogs haven't loaded.
#[allow(clippy::too_many_arguments)] // a Bevy system's full input set (the feed precedent)
fn feed_craft(
    script: Option<NonSendMut<UiScript>>,
    open: Res<CraftOpen>,
    actions: Res<PlayerActions>,
    spells: Option<Res<Spells>>,
    skill_lines: Option<Res<SkillLines>>,
    focus: Option<Res<SpellFocus>>,
    icons: Option<Res<ItemDisplays>>,
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    mut items: ResMut<Items>,
    commands: Res<NetCommands>,
    mut last: Local<Option<CraftState>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let fresh = (|| -> Option<CraftState> {
        let line = open.line?;
        let spells = spells.as_deref()?;
        let skill_lines = skill_lines.as_deref()?;
        let store = self_store.single().ok()?;
        let (rank, max_rank, bonus) = skill_rank(store, line);
        let effective = rank.saturating_add_signed(bonus);
        let name = skill_lines
            .catalog
            .line(line)
            .map(|l| l.name.clone())
            .unwrap_or_else(|| format!("Skill {line}"));
        let ctx = benilla_formats::TokenContext {
            durations: &spells.durations,
            radii: &spells.radii,
            lookup: &|id| spells.catalog.get(id),
            home_area: None,
        };
        let mut recipes: Vec<CraftRecipe> = actions
            .spells
            .iter()
            .filter(|&&s| {
                spells.catalog.get(s).is_some_and(|d| {
                    (d.cast_ui != 0 || d.attributes & SPELL_ATTR_IS_TRADESKILL != 0)
                        && d.effects[0] != benilla_formats::SPELL_EFFECT_TRADE_SKILL
                }) && skill_lines.catalog.spell_to_line(s) == Some(line)
            })
            .filter_map(|&s| {
                let d = spells.catalog.get(s)?;
                let sla = skill_lines.catalog.ability(s)?;
                let mut reagents = Vec::new();
                let mut num_available = u32::MAX;
                for &(entry, need) in d.reagents.iter().filter(|&&(e, n)| e != 0 && n != 0) {
                    let have = count_of(&store.0, &items, entry);
                    // A reagent is an ITEM row, so it terminates in the one genuinely shared chain
                    // (wow-re §5): ItemTemplate → ItemDisplayInfo → icon. Unlike the *recipe* icon
                    // above, there is nothing per-binding about this one.
                    let (name, icon) = match items.template(entry, 0, &commands) {
                        Some(t) => (
                            Some(t.name.clone()),
                            item_icon(icons.as_deref(), t.display_info_id),
                        ),
                        None => (None, None),
                    };
                    reagents.push(CraftReagent {
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
                let mut tools = Vec::new();
                for &t in d.totems.iter().filter(|&&t| t != 0) {
                    let have = count_of(&store.0, &items, t) > 0;
                    if let Some(info) = items.template(t, 0, &commands) {
                        tools.push((info.name.clone(), have));
                    }
                }
                if d.requires_spell_focus != 0 {
                    if let Some(n) = focus
                        .as_deref()
                        .and_then(|f| f.catalog.name(d.requires_spell_focus))
                    {
                        tools.push((n.to_string(), true));
                    }
                }
                let needs_item_target = matches!(
                    d.effects[0],
                    SPELL_EFFECT_ENCHANT_ITEM | SPELL_EFFECT_ENCHANT_ITEM_TEMPORARY
                );
                let icon = craft_icon(d);
                Some(CraftRecipe {
                    spell_id: s,
                    name: d.name.clone(),
                    sub_name: d.rank.clone().unwrap_or_default(),
                    difficulty: crate::ui_tradeskill::difficulty(
                        effective,
                        sla.trivial_low,
                        sla.trivial_high,
                    ),
                    num_available,
                    icon,
                    description: d
                        .description
                        .as_deref()
                        .map(|t| benilla_formats::substitute(t, d, &ctx)),
                    needs_item_target,
                    reagents,
                    tools,
                    tooltip: craft_tooltip(s, d),
                })
            })
            .collect();
        recipes.sort_by(|a, b| {
            let req = |r: &CraftRecipe| {
                skill_lines
                    .catalog
                    .ability(r.spell_id)
                    .map(|x| x.req_skill_value)
                    .unwrap_or(0)
            };
            req(b).cmp(&req(a)).then_with(|| a.name.cmp(&b.name))
        });
        Some(CraftState {
            name,
            rank,
            max_rank,
            recipes,
        })
    })();

    if fresh == *last {
        return;
    }
    script.set_craft(fresh.clone());
    match (&*last, &fresh) {
        (None, Some(f)) => {
            debug!("ui_craft: window opens — {} recipe(s)", f.recipes.len());
            script.fire_event("CRAFT_SHOW", vec![]);
        }
        (Some(_), Some(_)) => script.fire_event("CRAFT_UPDATE", vec![]),
        (Some(_), None) => {
            debug!("ui_craft: window closes");
            script.fire_event("CRAFT_CLOSE", vec![]);
        }
        (None, None) => {}
    }
    *last = fresh;
}

/// Drain the Lua intents: every `DoCraft` goes down the ONE cast ladder, and the resolver decides
/// what happens next — an enchant's `Targets = 0x10` word arms the targeting cursor's item half
/// (decision 0923; the bag / paper-doll click completes it, in `ui_action::targeting`), a rod
/// craft's zero word commits immediately. `CloseCraft` closes the window; a pick armed by it is
/// the one targeting word, cancelled the ordinary ways (ESC, right-click, a new cast).
///
/// Before 0923 this drain owned a private `PendingItemCast` with its own arm, its own bag-click
/// completion and its own cursor overlay — a second targeting machine that skipped every rung the
/// ladder runs (reagents included, which is what an enchant most needs). It is gone: the window
/// is now just another caster surface.
fn drain_craft(
    script: Option<NonSendMut<UiScript>>,
    mut open: ResMut<CraftOpen>,
    skill_lines: Option<Res<SkillLines>>,
    targeting: cast_target::CastTargeting,
    mut ladder: CastLadder,
) {
    let Some(mut script) = script else {
        return;
    };
    let _ = skill_lines; // the feed resolves lines; the drain just casts what it is handed
    for spell_id in script.take_craft_dos() {
        if open.line.is_none() {
            debug!("ui_craft: DoCraft({spell_id}) with no open window — ignored");
            continue;
        }
        debug!("ui_craft: DoCraft({spell_id})");
        ladder.send(spell_id, &targeting.context(), CastCommit::Spell);
    }
    if script.take_craft_close() {
        debug!("ui_craft: client-side close (no packet)");
        open.line = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_formats::SpellDisplay;

    /// Law D never looks at an item. The regression this pins is the *sibling* one: a rod-making
    /// `CREATE_ITEM` recipe — the exact case our old code substituted the product for — still
    /// fronts the spell's own icon here, because `EffectItemType[0]` is never read anywhere in
    /// `GetCraftIcon`'s extent. Craft and TradeSkill disagree on purpose.
    #[test]
    fn law_d_is_always_the_spells_own_icon_even_for_a_rod_recipe() {
        let rod = SpellDisplay {
            name: "Runed Copper Rod".into(),
            icon: Some("SPELL".into()),
            effects: [SPELL_EFFECT_CREATE_ITEM, 0, 0],
            effect_item_type: [6218, 0, 0],
            ..Default::default()
        };
        assert_eq!(craft_icon(&rod), Some("SPELL".into()));
    }

    /// A recipe with no `SpellIconID` row resolved is nil — the binding's `lua_pushnil` return.
    /// There is no item arm to fall through to.
    #[test]
    fn law_d_is_nil_when_the_spell_carries_no_icon() {
        let d = SpellDisplay {
            effect_item_type: [6218, 0, 0],
            ..Default::default()
        };
        assert_eq!(craft_icon(&d), None);
    }
    /// Law D's content twin. Three arms, and the point of each is that it differs from the
    /// TRAINER's law on the same shape: this reads the recipe's OWN effect columns.
    #[test]
    fn craft_tooltip_reads_the_recipes_own_effects() {
        // An enchant — no matching slot — describes the recipe spell itself.
        let enchant = SpellDisplay {
            effects: [SPELL_EFFECT_ENCHANT_ITEM, 0, 0],
            ..Default::default()
        };
        assert_eq!(craft_tooltip(7420, &enchant), CraftTooltip::Spell(7420));

        // A rod craft — CREATE_ITEM in slot 0 — describes the ITEM, off the SAME slot.
        let rod = SpellDisplay {
            effects: [SPELL_EFFECT_CREATE_ITEM, 0, 0],
            effect_item_type: [6218, 0, 0],
            ..Default::default()
        };
        assert_eq!(craft_tooltip(7421, &rod), CraftTooltip::Item(6218));

        // A LEARN_SPELL slot hops — and, unlike the trainer's law, does NOT check that the trigger
        // resolves. There is no catalog argument here at all, which is the difference made
        // structural: a trigger id this client has never heard of is still what gets described.
        let mut teacher = SpellDisplay::default();
        teacher.effects[1] = SPELL_EFFECT_LEARN_SPELL;
        teacher.effect_trigger_spell[1] = 999_999;
        assert_eq!(craft_tooltip(5149, &teacher), CraftTooltip::Spell(999_999));
    }
}
