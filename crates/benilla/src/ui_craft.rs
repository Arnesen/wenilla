//! The app-side **Craft window feed** (decision 0437 phase 3) — Enchanting's client-built book,
//! [`crate::ui_tradeskill`]'s CraftFrame twin. Same law: the effect-47 opener for skill line 333
//! opens it client-side (no packet — routed here by `ui_tradeskill::open_trade_skill`); the
//! recipe list is the player's known attr-`0x20` spells joined to line 333; reagents/tools off
//! the parsed `Spell.dbc` columns; the description is the spell's own `$`-token text, resolved
//! through the tooltip arc's substitution engine.
//!
//! **The item pick** (the one thing the TradeSkill window doesn't have): an enchant is an
//! item-targeted cast (`SPELL_EFFECT_ENCHANT_ITEM`/`_TEMPORARY`). `DoCraft` on one arms
//! [`PendingItemCast`]; the engine's `PickupContainerItem` reroute (byte-VERIFIED — the wow-re
//! `tradeskill` TU-F seam at `0x4f9b30`) turns the next bag CLICK into the target pick, and the
//! drain completes the cast (`CMSG_CAST_SPELL` with `TARGET_FLAG_ITEM` + the item's packed guid —
//! the byte-pinned SPELLCAST item field). A non-item craft (a Runed rod's CREATE_ITEM) casts
//! immediately through the one cast-send path, like a tradeskill recipe. **Named INTERIMs:** no
//! targeting-cursor art while armed; no replace-enchant confirm popup yet (event 0x193 is pure
//! client gating and 5875 has no replace opcode — needs the item's enchantment fields, a later
//! slice); the paper-doll and trade-window targets are out of scope (bags only).

use bevy::prelude::*;

use benilla_formats::{
    SPELL_EFFECT_CREATE_ITEM, SPELL_EFFECT_ENCHANT_ITEM, SPELL_EFFECT_ENCHANT_ITEM_TEMPORARY,
};
use benilla_protocol::messages::PLAYER_SKILL_SLOTS;
use benilla_ui::script::{CraftReagent, CraftRecipe, CraftState, UiScript};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{NetCommands, ObjectStore, SelfPlayer};
use crate::ui_action::{cast_target, send_spell_cast, PlayerActions, Spells};
use crate::ui_items::count_of;
use crate::ui_script::UiInput;
use crate::ui_spellbook::SkillLines;
use crate::ui_tradeskill::{SpellFocus, TradeSkillOpens};
use crate::ui_unit::UnitFeed;

/// `SPELL_ATTR_IS_TRADESKILL` — the item-recipe marker (0227's add-gate bit). Craft recipes
/// (enchants, the rod crafts) do NOT carry it — they carry **`castUI != 0`** instead (pinned on
/// the live 5875 data this cycle: 7418/7421 castUI=3, attributes bare 0x10000; exactly the
/// spellbook add-gate's third exclusion leg). INTERIM admission pending a wow-re detail pass:
/// a known spell joins the craft list when its SLA line matches AND (`castUI != 0` OR the
/// tradeskill bit) and it is not an opener.
const ATTR_IS_TRADESKILL: u32 = 0x20;

/// The open Craft window: the skill line whose recipes it lists (`None` = closed). Routed here
/// by the opener's `EffectMiscValue[0] != 0` (byte-VERIFIED — wow-re `tradeskill` TU-A); a Beast
/// Training opener also routes here and simply lists nothing (its pet-ability content is its own
/// arc, 0437 out-of-scope). Client-local state, no wire.
#[derive(Resource, Default)]
pub(crate) struct CraftOpen {
    pub(crate) line: Option<u32>,
}

/// An armed item-targeted craft cast, waiting for the player to right-click the target item in a
/// bag (`0` = none armed). Completed by `ui_items::drain`'s use hook; cleared on completion,
/// window close, or a fresh `DoCraft`.
#[derive(Resource, Default)]
pub(crate) struct PendingItemCast {
    pub(crate) spell_id: u32,
}

/// The one-cast-send-path resource set `drain_craft` threads into `send_spell_cast`, tupled to
/// fit Bevy's 16-param ceiling (the apply-side grouped-tuple precedent).
type CastTail<'w> = (
    ResMut<'w, crate::ui_cast::PendingCast>,
    ResMut<'w, crate::ui_cast::QueuedMeleeSpell>,
    ResMut<'w, crate::cooldowns::Cooldowns>,
    ResMut<'w, crate::ui_action::CastErrors>,
    ResMut<'w, crate::ui_action::AutoRepeatActive>,
    ResMut<'w, TradeSkillOpens>,
);

pub(crate) struct UiCraftPlugin;

impl Plugin for UiCraftPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CraftOpen>()
            .init_resource::<PendingItemCast>()
            .add_systems(
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
                    (d.cast_ui != 0 || d.attributes & ATTR_IS_TRADESKILL != 0)
                        && d.effect_1 != benilla_formats::SPELL_EFFECT_TRADE_SKILL
                }) && skill_lines.catalog.spell_to_line(s) == Some(line)
            })
            .filter_map(|&s| {
                let d = spells.catalog.get(s)?;
                let sla = skill_lines.catalog.ability(s)?;
                let mut reagents = Vec::new();
                let mut num_available = u32::MAX;
                for &(entry, need) in d.reagents.iter().filter(|&&(e, n)| e != 0 && n != 0) {
                    let have = count_of(&store.0, &items, entry);
                    let (name, icon) = match items.template(entry, 0, &commands) {
                        Some(t) => (
                            Some(t.name.clone()),
                            icons
                                .as_deref()
                                .and_then(|i| i.catalog.get(t.display_info_id))
                                .and_then(|di| di.icon.clone()),
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
                    d.effect_1,
                    SPELL_EFFECT_ENCHANT_ITEM | SPELL_EFFECT_ENCHANT_ITEM_TEMPORARY
                );
                // A rod-making recipe (CREATE_ITEM) fronts the product's icon like a tradeskill
                // row; an enchant fronts its spell icon.
                let icon = (d.effect_1 == SPELL_EFFECT_CREATE_ITEM && d.effect_item_type[0] != 0)
                    .then(|| {
                        items
                            .template(d.effect_item_type[0], 0, &commands)
                            .map(|t| t.display_info_id)
                            .and_then(|di| {
                                icons
                                    .as_deref()
                                    .and_then(|i| i.catalog.get(di))
                                    .and_then(|x| x.icon.clone())
                            })
                    })
                    .flatten()
                    .or_else(|| d.icon.clone());
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

/// Drain the Lua intents: `DoCraft` on an item-targeted enchant ARMS the pick ([`PendingItemCast`]
/// — the container drain completes it); on a self-craft (rod making) casts through the one
/// cast-send path immediately. `CloseCraft` closes the window and disarms any pick.
#[allow(clippy::too_many_arguments)] // a Bevy system's full input set (send_spell_cast's own)
fn drain_craft(
    script: Option<NonSendMut<UiScript>>,
    mut open: ResMut<CraftOpen>,
    mut pending_pick: ResMut<PendingItemCast>,
    spells: Option<Res<Spells>>,
    skill_lines: Option<Res<SkillLines>>,
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    items: Res<Items>,
    targeting: cast_target::CastTargeting,
    commands: Res<NetCommands>,
    self_player: Query<(Entity, Has<crate::creature_anim::Engaged>), With<SelfPlayer>>,
    mut sheath: MessageWriter<crate::creature_anim::SheathRequest>,
    // The cast-send tail's resource set, tupled (Bevy's 16-param ceiling; one concern).
    cast_tail: CastTail,
    mut ecs: Commands,
) {
    let (
        mut pending,
        mut queued_melee,
        mut cooldowns,
        mut cast_errors,
        mut auto_repeat,
        mut trade_opens,
    ) = cast_tail;
    let Some(mut script) = script else {
        return;
    };
    let _ = skill_lines; // the feed resolves lines; the drain keys only on the spell's effects
    for spell_id in script.take_craft_dos() {
        if open.line.is_none() {
            debug!("ui_craft: DoCraft({spell_id}) with no open window — ignored");
            continue;
        }
        let needs_item = spells
            .as_deref()
            .and_then(|s| s.catalog.get(spell_id))
            .map(|d| {
                matches!(
                    d.effect_1,
                    SPELL_EFFECT_ENCHANT_ITEM | SPELL_EFFECT_ENCHANT_ITEM_TEMPORARY
                )
            });
        match needs_item {
            Some(true) => {
                debug!("ui_craft: DoCraft({spell_id}) — item pick armed (right-click the target)");
                pending_pick.spell_id = spell_id;
            }
            Some(false) => {
                debug!("ui_craft: DoCraft({spell_id}) — self craft");
                pending_pick.spell_id = 0;
                send_spell_cast(
                    spell_id,
                    &targeting.context(),
                    &commands,
                    &self_player,
                    spells.as_deref(),
                    &items,
                    &mut sheath,
                    &mut ecs,
                    &mut pending,
                    &mut queued_melee,
                    &mut cooldowns,
                    &mut cast_errors,
                    &mut auto_repeat,
                    &mut trade_opens,
                );
            }
            None => debug!("ui_craft: DoCraft({spell_id}) — unknown spell, ignored"),
        }
    }
    if script.take_craft_close() {
        debug!("ui_craft: client-side close (no packet)");
        open.line = None;
        pending_pick.spell_id = 0;
    }

    // Complete the armed pick: a bag click the engine rerouted (the PickupContainerItem seam)
    // names the target item — resolve its guid and send the item-targeted cast. One-shot.
    for (bag, slot) in script.take_item_picks() {
        let armed = pending_pick.spell_id;
        if armed == 0 {
            continue; // a stale click raced the disarm — nothing to cast
        }
        let slot0 = u8::try_from(slot.saturating_sub(1)).unwrap_or(0);
        let guid = self_store
            .single()
            .ok()
            .and_then(|store| crate::ui_items::slot_guid(&store.0, bag, slot0, &items));
        match guid {
            Some(item_guid) => {
                debug!("ui_craft: enchant pick — cast {armed} at item {item_guid:#x}");
                let _ = commands.0.send(crate::net::ClientCommand::CastSpellItem {
                    spell_id: armed,
                    item_guid,
                });
            }
            None => debug!("ui_craft: enchant pick on an empty slot — kept armed"),
        }
        if guid.is_some() {
            pending_pick.spell_id = 0;
        }
    }
    // Mirror the armed state into the engine each frame (cheap; the engine reroutes bag clicks
    // only while set).
    script.set_item_pick_armed(pending_pick.spell_id != 0);
}
