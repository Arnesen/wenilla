//! The stance/shapeshift bar feed + drain — the app side of `benilla_ui::script::shapeshift`'s
//! seam, from the wow-re-verified mechanism (`system/ui/scratch/shapeshift-bar-api.md`, the §5
//! cross-check behind `GetNumShapeshiftForms 0x4b4590` / `GetShapeshiftFormInfo 0x4b45c0` /
//! `CastShapeshiftForm 0x4b4810` / `GetShapeshiftFormCooldown 0x4b49a0`):
//!
//! - **Admission** (the list build `0x4b25b0`): a KNOWN spell joins the bar when
//!   `AttributesEx2 & 0x2 == 0` AND (it carries a `SPELL_AURA_MOD_SHAPESHIFT` apply-aura effect
//!   OR `AttributesEx2 & 0x10` force-admits it). 5875 data: warrior stances (`ex2 0x1`), druid
//!   forms (`0x0`), and Stealth (`0x200000`) all pass the exclusion bit; **Ghost Wolf 2645
//!   (`ex2 0x2`) is the shipped carrier of the exclusion** — a shaman faithfully gets NO stance
//!   bar, and cancels the form at the buff frame instead (verified in the data, 2026-07-31). No
//!   shipped spell needs the force-admit bit, so its aura-scan `isActive` leg reads `false` here
//!   (a named gap, not a divergence — there is no spell to diverge on).
//! - **Order** (comparator `0x4b2bb0`): ascending `Spell.dbc` `StanceBarOrder`, negative last,
//!   spell id tiebreak. (Battle 0 / Def 1 / Berserker 2; Bear 0 … Moonkin 4; Stealth −1 → last.)
//! - **texture**: the form SPELL's icon — `ActiveIconID` while active when nonzero (druid forms'
//!   paw), else `SpellIconID`; never `SpellShapeshiftForm.dbc`'s icon.
//! - **isActive**: our form byte (`UNIT_FIELD_BYTES_1` byte 2) == the spell's MOD_SHAPESHIFT
//!   `EffectMiscValue`.
//! - **isCastable**: the active form reads hardcoded-castable; otherwise the usability predicate
//!   `0x6e3d60` — the SAME full walk the action bar's `IsUsableAction` runs
//!   ([`crate::ui_action::usable`], decision 0269's fold-back: reagents, forms, stealth, aura
//!   states, the power gate).
//! - **cooldown**: the form spell's own spell/category read ([`Cooldowns::info`]).
//! - **Click** (`CastShapeshiftForm`): the ACTIVE form CANCELS (`CMSG_CANCEL_AURA`) — unless
//!   `SpellShapeshiftForm.dbc` flags bit `0x2` blocks it (warrior stances: silent no-op, the
//!   `0x4b4963` guard); any other form casts through the shared [`send_spell_cast`] path.
//!
//! The refresh model is one diff-and-fire: the feed rebuilds the pushed list each frame
//! (identity, active, castable, cooldown) and fires `UPDATE_SHAPESHIFT_FORMS` on any change — the
//! real client's learn/unlearn edge (`0x5e9c20`/`0x5e9fe0`) plus its separate per-state repaint events
//! collapse into one, which `StanceBar.xml` documents as its deliberate divergence. Cooldown
//! remainders re-push only on a [`Cooldowns::generation`] change (`state.rs`'s own churn gate) —
//! the engine extrapolates the sweep from the absolute start between pushes.

use std::time::Instant;

use bevy::prelude::*;

use benilla_ui::script::{ShapeshiftFormView, UiScript};

use crate::cooldowns::Cooldowns;
use crate::items::Items;
use crate::net::{ClientCommand, GuidIndex, NetCommands, ObjectStore, Reputations, SelfPlayer};
use crate::target::Selection;
use crate::ui_action::{cast_target, send_spell_cast, usable, PlayerActions, Spells};
use crate::ui_script::UiInput;
use crate::ui_unit::UnitFeed;

/// `AttributesEx2` bit `0x2` — EXCLUDES a spell from the stance bar (the `0x4b25b0` gate's
/// first leg). Ghost Wolf 2645 is the shipped carrier (module docs) — the reason a shaman has
/// no stance bar.
const ATTR_EX2_STANCE_BAR_EXCLUDE: u32 = 0x2;
/// `AttributesEx2` bit `0x10` — FORCE-ADMITS a spell without a MOD_SHAPESHIFT effect (the
/// gate's or-leg). No shipped 5875 spell uses it either.
const ATTR_EX2_STANCE_BAR_FORCE: u32 = 0x10;

/// What the feed last pushed (the `state.rs` pattern). No cooldown-churn gate needed: the
/// triple carries the ABSOLUTE start, which is frame-stable for a running cooldown.
#[derive(Default)]
struct StanceMemory {
    pushed: Option<Vec<ShapeshiftFormView>>,
}

pub(crate) struct UiShapeshiftPlugin;

impl Plugin for UiShapeshiftPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                // Feed rides with the unit feed (before UiInput, like ui_action's own); the drain
                // runs after the input pass so a stance click goes out the same frame.
                feed_shapeshift_bar.in_set(UnitFeed).before(UiInput),
                drain_shapeshift_casts.after(UiInput),
            ),
        );
    }
}

/// Build the bar list from the known-spell set × the catalog, per the module-doc mechanism, and
/// diff-push it.
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // a Bevy system's full input set
fn feed_shapeshift_bar(
    script: Option<NonSendMut<UiScript>>,
    actions: Res<PlayerActions>,
    spells: Option<Res<Spells>>,
    cooldowns: Res<Cooldowns>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    selection: Res<Selection>,
    index: Res<GuidIndex>,
    units: Query<&ObjectStore, Without<SelfPlayer>>,
    factions: Option<Res<crate::target::Factions>>,
    reputations: Res<Reputations>,
    mut items: ResMut<Items>,
    commands: Res<NetCommands>,
    mut memory: Local<StanceMemory>,
) {
    let Some(mut script) = script else {
        return;
    };
    let Some(spells) = spells else {
        return;
    };
    let store = self_q.iter().next();
    let form_byte = store.map(|s| s.0.unit_shapeshift_form()).unwrap_or(0);
    let now = Instant::now();
    let ui_now = script.now();
    // The usable walk's target leg (the Execute family) reads the CURRENT TARGET, like the
    // action feed's own ctx.
    let target_store = selection
        .guid
        .and_then(|g| index.0.get(&g))
        .and_then(|&e| units.get(e).ok());

    // Admission + order (module docs).
    let mut rows: Vec<(u32, &benilla_formats::SpellDisplay)> = actions
        .spells
        .iter()
        .filter_map(|&id| {
            let d = spells.catalog.get(id)?;
            let admitted = d.attributes_ex2 & ATTR_EX2_STANCE_BAR_EXCLUDE == 0
                && (d.shapeshift_form.is_some()
                    || d.attributes_ex2 & ATTR_EX2_STANCE_BAR_FORCE != 0);
            admitted.then_some((id, d))
        })
        .collect();
    rows.sort_by_key(|&(id, d)| {
        let order = if d.stance_bar_order < 0 {
            i64::MAX
        } else {
            i64::from(d.stance_bar_order)
        };
        (order, id)
    });

    let fresh: Vec<ShapeshiftFormView> = rows
        .into_iter()
        .map(|(id, d)| {
            let active = form_byte != 0 && d.shapeshift_form == Some(u32::from(form_byte));
            // isCastable: hardcoded true for the ACTIVE form, else the full 0x6e3d60 walk —
            // the same predicate (and the same ctx shape) the action bar's IsUsableAction runs.
            let castable = active
                || store.is_none_or(|s| {
                    let ctx = usable::UsableCtx {
                        store: s,
                        target_store,
                        factions: factions.as_deref(),
                        reputations: &reputations,
                        cooldowns: &cooldowns,
                        now,
                    };
                    usable::spell_usable(id, d, &spells, &ctx, &mut items, &commands).0
                });
            let texture = if active {
                d.active_icon.clone().or_else(|| d.icon.clone())
            } else {
                d.icon.clone()
            };
            let cooldown = cooldowns.info(id, 0, Some(d), now).ui_triple(now, ui_now);
            ShapeshiftFormView {
                spell_id: id,
                texture,
                name: d.name.clone(),
                active,
                castable,
                cooldown,
            }
        })
        .collect();

    if memory.pushed.as_ref() != Some(&fresh) {
        debug!(
            "ui_shapeshift: {} form(s), active form byte {form_byte}",
            fresh.len()
        );
        memory.pushed = Some(fresh.clone());
        script.set_shapeshift_forms(fresh);
        script.fire_event("UPDATE_SHAPESHIFT_FORMS", vec![]);
    }
}

/// Drain `CastShapeshiftForm`'s queued form spells: cancel the active form (unless its
/// `SpellShapeshiftForm.dbc` flags block it — the silent no-op), cast any other.
#[allow(clippy::too_many_arguments)] // a Bevy system's full input set (send_spell_cast's own)
fn drain_shapeshift_casts(
    script: Option<NonSendMut<UiScript>>,
    targeting: cast_target::CastTargeting,
    commands: Res<NetCommands>,
    self_player: Query<(Entity, Has<crate::creature_anim::Engaged>), With<SelfPlayer>>,
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    spells: Option<Res<Spells>>,
    cast_items: Res<Items>,
    mut sheath: MessageWriter<crate::creature_anim::SheathRequest>,
    mut pending: ResMut<crate::ui_cast::PendingCast>,
    mut queued_melee: ResMut<crate::ui_cast::QueuedMeleeSpell>,
    mut cooldowns: ResMut<Cooldowns>,
    mut cast_errors: ResMut<crate::ui_action::CastErrors>,
    mut auto_repeat: ResMut<crate::ui_action::AutoRepeatActive>,
    mut trade_skill_opens: ResMut<crate::ui_tradeskill::TradeSkillOpens>,
    mut ground: ResMut<crate::ui_action::SpellTargeting>,
    mut ecs: Commands,
) {
    let Some(mut script) = script else {
        return;
    };
    for spell_id in script.take_shapeshift_casts() {
        let form_byte = self_store
            .iter()
            .next()
            .map(|s| s.0.unit_shapeshift_form())
            .unwrap_or(0);
        let d = spells.as_ref().and_then(|s| s.catalog.get(spell_id));
        // The active-form fork — shared with the plain `CastSpell` dispatcher's twin
        // (`crate::ui_action::toggle`): cancel unless the `0x4b4963` flags1-&-0x2 guard makes
        // it a silent no-op (warrior stances).
        let row = spells
            .as_ref()
            .and_then(|s| s.forms.get(&u32::from(form_byte)));
        match d.and_then(|d| crate::ui_action::toggle::form_recast_disposition(d, form_byte, row)) {
            Some(true) => {
                debug!("ui_shapeshift: cancel form aura {spell_id}");
                let _ = commands.0.send(ClientCommand::CancelAura { spell_id });
                continue;
            }
            Some(false) => continue,
            None => {}
        }
        debug!("ui_shapeshift: cast form {spell_id}");
        send_spell_cast(
            spell_id,
            &targeting.context(),
            &commands,
            &self_player,
            spells.as_deref(),
            &cast_items,
            &mut sheath,
            &mut ecs,
            &mut pending,
            &mut queued_melee,
            &mut cooldowns,
            &mut cast_errors,
            &mut auto_repeat,
            &mut trade_skill_opens,
            &mut ground,
        );
    }
}
