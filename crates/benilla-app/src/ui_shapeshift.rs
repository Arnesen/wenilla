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
//!   bar, and cancels the form at the buff frame instead (verified in the data, 2026-07-31). The
//!   force-admit bit is what builds the **paladin's aura bar**: 465/7294/19746/19876/19888/19891
//!   carry `ex2 0x10` with no MOD_SHAPESHIFT effect at all (decision 1302 — correcting 0270's
//!   "no 5875 spell uses it", which is what left the aura-scan `isActive` leg unbuilt here).
//! - **Order** (comparator `0x4b2bb0`): ascending `Spell.dbc` `StanceBarOrder`, negative last,
//!   spell id tiebreak. (Battle 0 / Def 1 / Berserker 2; Bear 0 … Moonkin 4; Stealth −1 → last.)
//! - **texture**: the form SPELL's icon — `ActiveIconID` while active when nonzero (druid forms'
//!   paw), else `SpellIconID`; never `SpellShapeshiftForm.dbc`'s icon. Elected inside the block
//!   BOTH `isActive` arms converge on, so a lit paladin aura wears it too ([`form_texture`]).
//! - **isActive**: two arms on the spell's own `formId` — the form-byte compare for a
//!   MOD_SHAPESHIFT spell, the 48-slot aura scan for a force-admitted one ([`form_active`]).
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
use crate::ui_action::{cast_target, usable, CastCommit, CastLadder, PlayerActions, Spells};
use crate::ui_script::UiInput;
use crate::ui_unit::UnitFeed;

/// `AttributesEx2` bit `0x2` — EXCLUDES a spell from the stance bar (the `0x4b25b0` gate's
/// first leg). Ghost Wolf 2645 is the shipped carrier (module docs) — the reason a shaman has
/// no stance bar.
const ATTR_EX2_STANCE_BAR_EXCLUDE: u32 = 0x2;
/// `AttributesEx2` bit `0x10` — FORCE-ADMITS a spell without a MOD_SHAPESHIFT effect (the
/// gate's or-leg). 44 shipped 5875 rows carry it; the live ones are the **paladin aura family**
/// (Devotion / Retribution / Concentration / the three resistances / Sanctity / Charismatic) plus
/// Ironweave Battlesuit 27733 and a band of `zzOLD*` rows. Decision 1302 corrects 0270 here.
const ATTR_EX2_STANCE_BAR_FORCE: u32 = 0x10;

/// `GetShapeshiftFormInfo 0x4b45c0`'s **isActive**, both arms (wow-re `shapeshift-bar-api.md`,
/// VERIFIED). The reference forks on the spell's own `formId` — its first `MOD_SHAPESHIFT`
/// effect's `EffectMiscValue`, or 0 when it has no such effect at all:
///
/// - `formId != 0` → active ⇔ the caster's form byte matches it (`4b46a0`–`4b46af`).
/// - `formId == 0` (a **force-admitted** row, `AttributesEx2 & 0x10`) **and `ActiveIconID != 0`**
///   (the gate at `4b46f2`) → active ⇔ this spell's own id is live in the player's 48-slot aura
///   array with **nibble bit 0** of `UNIT_FIELD_AURAFLAGS` set (`4b4739 test al,1` — the
///   *cancelable* bit). That is exactly [`crate::ui_action::toggle::active_action_toggle`], the
///   byte-twin `0x4e55f0` the main action bar's own toggle runs, which is why it is reused rather
///   than open-coded: the §5 confirmed the two scans test the same bit.
///
/// Both arms converge on **one** shared block at `4b4754` — the form-byte match *jumps* there, the
/// aura-scan hit *falls through* into it — and that block is the sole writer of `isActive = 1`.
///
/// The second arm is why a paladin's aura bar can light at all: a paladin Aura carries **no**
/// `MOD_SHAPESHIFT` effect (`formId == 0`), so the form-byte arm can never fire on one, and the
/// button reads "not active" for ever. See decision 1302 — and note that the claim this file used
/// to carry, "no shipped spell needs the force-admit bit", is false: 44 rows carry it and the live
/// ones are exactly the paladin aura family (465 Devotion / 7294 Retribution / 19746
/// Concentration / 19876 Shadow / 19888 Frost / 19891 Fire, `StanceBarOrder` 0..5,
/// `ActiveIconID` 122 apiece — read from the shipped 5875 `Spell.dbc`).
fn form_active(
    spell_id: u32,
    d: &benilla_formats::SpellDisplay,
    form_byte: u8,
    store: Option<&ObjectStore>,
) -> bool {
    match d.shapeshift_form.unwrap_or(0) {
        0 => store.is_some_and(|s| crate::ui_action::toggle::active_action_toggle(spell_id, d, s)),
        form => form == u32::from(form_byte),
    }
}

/// The button's face: `SpellIconID`, or `ActiveIconID` when the row is active and that column is
/// nonzero — **under either arm of [`form_active`] alike**.
///
/// The election happens inside the one shared block both arms reach (`4b4754`/`4b4763`/`4b4769`);
/// `4b46c6`–`4b46e8` is only the id→path tail and decides nothing.
///
/// Decision **1307** records the correction: 1302 §5 first held this swap to the MOD_SHAPESHIFT
/// arm, reasoning that the icon is picked at a *lower address* than the aura scan and so could
/// not depend on it. That inference was wrong, and instructively so: **MSVC laid
/// the deciding block at a higher address than the scan that feeds it**, so ordering the citations
/// by address yields the opposite control flow. The wow-re §5 on `0x4b45c0` carved it and
/// corrected us — a scan hit at `4b473e` falls *through* `4b4751` into the very block the form-byte
/// match jumps to, and the `mov ebx,[ebp-0xc]` at `4b4751` exists for no reason but to restore the
/// lua_State across that fall-through. So a lit paladin aura **does** wear `ActiveIconID` 122.
///
/// `active_icon` is `None` exactly when the column is 0, so the `or_else` is the reference's own
/// `4b4763 je` — a MOD_SHAPESHIFT row can report active while still painting `SpellIconID`
/// (the two facts are decided by different bytes and do not imply each other). On the force-admit
/// arm that leg is unreachable: entry to the scan is already gated on `ActiveIconID != 0`.
fn form_texture(d: &benilla_formats::SpellDisplay, active: bool) -> Option<String> {
    if active {
        d.active_icon.clone().or_else(|| d.icon.clone())
    } else {
        d.icon.clone()
    }
}

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
    clock: Res<crate::ui_script::UiClock>,
    mut memory: Local<crate::ui_script::VmMemo<StanceMemory>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let memory = memory.get(&script);
    let Some(spells) = spells else {
        return;
    };
    let store = self_q.iter().next();
    let form_byte = store.map(|s| s.0.unit_shapeshift_form()).unwrap_or(0);
    let now = Instant::now();
    // The frame's atomic clock pair for the `ui_triple` conversion (`crate::ui_script::UiClock`'s
    // own doc: converting through a locally sampled `Instant::now()` wobbles the derived start).
    let (anchor, ui_now) = (clock.anchor, clock.ui_now);
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
            let active = form_active(id, d, form_byte, store);
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
                    };
                    usable::spell_usable(id, d, &spells, &ctx, &mut items, &commands).0
                });
            let texture = form_texture(d, active);
            let cooldown = cooldowns
                .info(id, 0, Some(d), now)
                .ui_triple(anchor, ui_now);
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

/// Drain `CastShapeshiftForm`'s queued form spells. `0x4b4810` forks on the same `formId` the
/// info call does, and each arm has its own cancel test:
///
/// - **`formId != 0`** — the active form cancels, unless the `SpellShapeshiftForm.dbc`
///   `flags1 & 0x2` guard (`0x4b4963`) makes it a silent no-op (warrior stances).
/// - **`formId == 0`, `ActiveIconID != 0`** — the force-admit/aura arm: a live own aura cancels,
///   otherwise cast. There is **no DBC guard on this arm** and there cannot be — a force-admitted
///   spell has no form id to look one up with. Without it a paladin clicking their active aura
///   re-cast it instead of dropping it (decision 1302).
fn drain_shapeshift_casts(
    script: Option<NonSendMut<UiScript>>,
    targeting: cast_target::CastTargeting,
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    mut ladder: CastLadder,
) {
    let Some(mut script) = script else {
        return;
    };
    for spell_id in script.take_shapeshift_casts() {
        let store = self_store.iter().next();
        let form_byte = store.map(|s| s.0.unit_shapeshift_form()).unwrap_or(0);
        let d = ladder.spells.as_ref().and_then(|s| s.catalog.get(spell_id));
        // The active-form fork — shared with the plain `CastSpell` dispatcher's twin
        // (`crate::ui_action::toggle`): cancel unless the `0x4b4963` flags1-&-0x2 guard makes
        // it a silent no-op (warrior stances).
        let row = ladder
            .spells
            .as_ref()
            .and_then(|s| s.forms.get(&u32::from(form_byte)));
        // …and its force-admit twin, which is the SAME predicate `form_active` reads for the
        // button's latch, so a lit button and a cancelling click can never disagree.
        let disposition = d.and_then(|d| {
            if d.shapeshift_form.unwrap_or(0) == 0 {
                store
                    .filter(|s| crate::ui_action::toggle::active_action_toggle(spell_id, d, s))
                    .map(|_| true)
            } else {
                crate::ui_action::toggle::form_recast_disposition(d, form_byte, row)
            }
        });
        match disposition {
            Some(true) => {
                debug!("ui_shapeshift: cancel form aura {spell_id}");
                let _ = ladder
                    .commands
                    .0
                    .send(ClientCommand::CancelAura { spell_id });
                continue;
            }
            Some(false) => continue,
            None => {}
        }
        debug!("ui_shapeshift: cast form {spell_id}");
        ladder.send(spell_id, &targeting.context(), CastCommit::Spell);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_formats::SpellDisplay;
    use benilla_protocol::ObjectFields;

    /// UNIT_FIELD_AURA 47 / UNIT_FIELD_AURAFLAGS 95 (nibble-packed), as `ui_action::toggle`'s
    /// own tests mirror them. `0xb` = occupied (eff-index bits) + cancelable (bit 0).
    fn player_with_aura(spell_id: u32) -> ObjectStore {
        ObjectStore(ObjectFields::from_pairs(&[(47u16, spell_id), (95, 0xb)]))
    }

    /// Devotion Aura as the shipped 5875 `Spell.dbc` carries it (read 2026-08-14): force-admitted
    /// by `AttributesEx2 0x10`, **no** MOD_SHAPESHIFT effect, `ActiveIconID` 122,
    /// `StanceBarOrder` 0.
    fn devotion_aura() -> SpellDisplay {
        SpellDisplay {
            attributes_ex2: ATTR_EX2_STANCE_BAR_FORCE,
            shapeshift_form: None,
            active_icon_id: 122,
            stance_bar_order: 0,
            ..Default::default()
        }
    }

    /// The bug the director reported: a paladin's aura bar never lights. A paladin Aura has no
    /// MOD_SHAPESHIFT effect, so the form-byte arm cannot fire on it — `isActive` is the 48-slot
    /// aura scan or it is nothing.
    #[test]
    fn a_force_admitted_aura_latches_on_its_own_live_aura_not_the_form_byte() {
        let devotion = devotion_aura();
        let up = player_with_aura(465);

        assert!(
            form_active(465, &devotion, 0, Some(&up)),
            "Devotion Aura is up: the button must read active, with the form byte at 0"
        );
        assert!(
            !form_active(7294, &devotion_aura(), 0, Some(&up)),
            "a DIFFERENT aura's button stays dark while Devotion is the one that is up"
        );
        assert!(
            !form_active(
                465,
                &devotion,
                0,
                Some(&ObjectStore(ObjectFields::from_pairs(&[])))
            ),
            "no aura, no latch"
        );
        assert!(
            !form_active(465, &devotion, 0, None),
            "no player object, no latch"
        );

        // `ActiveIconID == 0` closes the arm entirely — the reference's own gate on it.
        let iconless = SpellDisplay {
            active_icon_id: 0,
            ..devotion_aura()
        };
        assert!(!form_active(465, &iconless, 0, Some(&up)));
    }

    /// The `ActiveIconID` election, both arms (1302 §5, carved by the wow-re §5 on `0x4b45c0`).
    /// Both determinations converge on the one block that elects it, so a lit aura wears the swirl
    /// exactly as a shifted druid wears the paw — and the `ActiveIconID == 0` fallback is the
    /// reference's own `4b4763 je`, reachable only on the MOD_SHAPESHIFT arm.
    #[test]
    fn the_active_icon_swap_reaches_both_arms() {
        let shield = Some("Interface\\Icons\\Spell_Holy_DevotionAura".to_string());
        let swirl = Some("Interface\\Icons\\Spell_Nature_WispSplode".to_string());

        // A druid's Cat Form: the MOD_SHAPESHIFT arm — the paw.
        let cat = SpellDisplay {
            shapeshift_form: Some(1),
            active_icon_id: 122,
            active_icon: swirl.clone(),
            icon: shield.clone(),
            ..Default::default()
        };
        assert_eq!(form_texture(&cat, true), swirl);
        assert_eq!(form_texture(&cat, false), shield);

        // A paladin aura reaches `active` through the force-admit arm — and elects the SAME
        // column. Address order misled us here; the shared block at `4b4754` is the law.
        let devotion = SpellDisplay {
            active_icon: swirl.clone(),
            icon: shield.clone(),
            ..devotion_aura()
        };
        assert_eq!(
            form_texture(&devotion, true),
            swirl,
            "the aura-scan hit falls through into the block that elects ActiveIconID"
        );
        assert_eq!(form_texture(&devotion, false), shield);

        // `4b475c` sets isActive BEFORE `4b4763` tests the column, so a MOD_SHAPESHIFT row with
        // ActiveIconID 0 is active AND still paints SpellIconID. (A warrior stance is this row.)
        let stance = SpellDisplay {
            shapeshift_form: Some(17),
            active_icon_id: 0,
            active_icon: None,
            icon: shield.clone(),
            ..Default::default()
        };
        assert_eq!(
            form_texture(&stance, true),
            shield,
            "active and unswapped are decided by different bytes and do not imply each other"
        );
    }

    /// The control: a MOD_SHAPESHIFT spell still reads the FORM BYTE, and never the aura array.
    /// Battle Stance's own aura being live must not be what lights it (a stance carries
    /// `ActiveIconID` 0, so the other arm could not fire anyway — this pins the fork, not the
    /// data).
    #[test]
    fn a_shapeshift_form_still_latches_on_the_form_byte() {
        let battle_stance = SpellDisplay {
            shapeshift_form: Some(17),
            active_icon_id: 0,
            ..Default::default()
        };
        let no_auras = ObjectStore(ObjectFields::from_pairs(&[]));
        assert!(form_active(2457, &battle_stance, 17, Some(&no_auras)));
        assert!(!form_active(2457, &battle_stance, 1, Some(&no_auras)));
        assert!(
            !form_active(2457, &battle_stance, 0, Some(&player_with_aura(2457))),
            "out of the form: a live aura row is not what the MOD_SHAPESHIFT arm reads"
        );
    }
}
