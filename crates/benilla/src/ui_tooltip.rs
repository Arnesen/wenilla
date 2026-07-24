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

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use benilla_ui::script::{UiScript, UnitState};

use crate::go_templates::{GameObjectTemplates, Locks};
use crate::items::Items;
use crate::names::NameCache;
use crate::net::{NetCommands, ObjectStore, Reputations, SelfPlayer};
use crate::target::{go_is_nearest, ring_reaction, Factions, Hovered, HoveredObject};
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

/// Build one spell's tooltip view (decision 0274 P2) — the verified spell line law's inputs,
/// every string resolved here where the catalogs live: the cost cell (power-typed; rage prints
/// wire-cost ÷ 10; "Next melee" for on-next-swing attributes), the range cell ("N yd range",
/// "N-M yd range" when the row's min is nonzero — the law's `"%d-%d"` fork; "Melee Range" for
/// the melee family — INTERIM text, the proper source is SpellRange.dbc's own display-name
/// column), the cast cell ("Instant"/"Instant cast"/"N sec cast"; None = passive, which omits
/// the whole line), the cooldown cell (`max(RecoveryTime, CategoryRecoveryTime)` — law §3.4),
/// the required-form line (law §3.6: the `Stances` mask's form names, met/unmet against `form`),
/// and the $-substituted description/aura-description (byte-exact formulas,
/// `benilla_formats::substitute`).
fn spell_tooltip_view(
    spell_id: u32,
    spells: &Spells,
    home_area: Option<&str>,
    form: u8,
) -> Option<benilla_ui::script::SpellTooltipView> {
    let d = spells.catalog.get(spell_id)?;
    let ctx = benilla_formats::TokenContext {
        durations: &spells.durations,
        radii: &spells.radii,
        lookup: &|id| spells.catalog.get(id),
        home_area,
    };
    // The on-next-swing class reads "Next melee" in the cost cell.
    let cost = if d.on_next_swing() {
        Some("Next melee".to_string())
    } else if d.mana_cost > 0 {
        Some(match d.power_type {
            1 => format!("{} Rage", d.mana_cost / 10),
            3 => format!("{} Energy", d.mana_cost),
            _ => format!("{} Mana", d.mana_cost),
        })
    } else if d.mana_cost_pct > 0 {
        Some(format!("{}% of base mana", d.mana_cost_pct))
    } else {
        None
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
    let cast_time = if d.passive {
        None
    } else {
        let base = spells
            .cast_times
            .get(d.casting_time_index)
            .map(|c| c.base_ms)
            .unwrap_or(0);
        Some(if base == 0 {
            if cost.is_none() {
                "Instant".to_string()
            } else {
                "Instant cast".to_string()
            }
        } else {
            format!("{} sec cast", trim_secs(f64::from(base) / 1000.0))
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
    let requires_form = (d.stances != 0)
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
    Some(benilla_ui::script::SpellTooltipView {
        name: d.name.clone(),
        rank: d.rank.clone(),
        cost,
        range,
        cast_time,
        cooldown,
        requires_form,
        form_met,
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

/// The `%.3g`-style terse seconds (1.5 → "1.5", 2.0 → "2") — the SPELL_CAST_TIME/RECAST shape.
fn trim_secs(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
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
    mut pushed: Local<std::collections::HashSet<u32>>,
    mut last_home: Local<Option<String>>,
    mut last_form: Local<Option<u8>>,
) {
    let Some(mut script) = script else {
        return;
    };
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
                .filter(|s| !pushed.contains(s)),
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
                        .filter(|s| *s != 0 && !pushed.contains(s)),
                );
            }
        }
    }
    // The buff bar's hoverables: the live aura spells, at arrival.
    if let Some(auras) = auras.as_deref() {
        wanted.extend(auras.spell_ids().filter(|s| !pushed.contains(s)));
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
                .filter(|s| !pushed.contains(s)),
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
                .filter(|s| !pushed.contains(s)),
        );
    }
    let home_area: Option<String> = home_bind
        .as_deref()
        .and_then(|b| b.0)
        .and_then(|id| area_names.as_deref()?.0.resolve(id as i32))
        .map(str::to_string);
    // A bind-point change re-substitutes every pushed view ($z — Astral Recall's shape).
    if *last_home != home_area {
        *last_home = home_area.clone();
        wanted.extend(pushed.drain());
    }
    // A stance/form change re-pushes too: the required-form line's white/red tracks the CURRENT
    // form (law §3.6), and the views are static snapshots until re-pushed.
    let form = self_q
        .single()
        .map(|s| s.0.unit_shapeshift_form())
        .unwrap_or(0);
    if *last_form != Some(form) {
        *last_form = Some(form);
        wanted.extend(pushed.drain());
    }
    for id in wanted {
        if let Some(view) = spell_tooltip_view(id, spells, home_area.as_deref(), form) {
            script.set_spell_tooltip(id, view);
            pushed.insert(id);
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

#[allow(clippy::too_many_arguments)]
fn drive_mouseover_tooltip(
    script: Option<NonSendMut<UiScript>>,
    hovered: Res<Hovered>,
    hovered_go: Res<HoveredObject>,
    window: Query<&Window, With<PrimaryWindow>>,
    stores: Query<&ObjectStore>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    mut names: ResMut<NameCache>,
    commands: Res<NetCommands>,
    factions: Option<Res<Factions>>,
    reputations: Res<Reputations>,
    mut go_templates: ResMut<GameObjectTemplates>,
    locks: Option<Res<Locks>>,
    mut items: ResMut<Items>,
    mut last: Local<LastHover>,
    mut last_lines: Local<Option<UnitState>>,
) {
    let Some(mut script) = script else {
        return;
    };
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
    let go = hovered_go
        .target
        .zip(hovered_go.guid)
        .filter(|_| unit.is_none() || go_is_nearest(&hovered, &hovered_go));

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
    if let Some((entity, guid)) = go {
        // The GO plate rides the cursor (the reference's signpost hover — engine-seated at
        // the pointer, following it; 0281's corner law stays the UNIT flow's).
        let cursor_ui = window.iter().next().and_then(|w| {
            w.cursor_position()
                .map(|c| Vec2::new(c.x, w.height() - c.y))
        });
        if *last == LastHover::Go(guid) {
            if let Some(p) = cursor_ui {
                script.world_tooltip_move(p.x, p.y);
            }
            return;
        }
        let Some(template) = go_templates.get(guid).cloned() else {
            // Template in flight: ask once and retry next frame (`last` stays, so the show
            // fires the moment the name lands).
            go_templates.request(guid, &commands);
            return;
        };
        // The red requirement lines — flag-locked (GAMEOBJECT_FLAGS bit 0x2, vmangos
        // GO_FLAG_LOCKED) resolves the Lock.dbc slots: item keys name the key item (ask-once
        // through the template cache), skill locks name the profession.
        let mut requirements = Vec::new();
        let locked = stores
            .get(entity)
            .map(|s| s.0.gameobject_flags() & 0x2 != 0)
            .unwrap_or(false);
        if locked && template.lock_id != 0 {
            if let Some(slots) = locks.as_ref().and_then(|l| l.0.slots(template.lock_id)) {
                for slot in slots {
                    match slot.key_type {
                        benilla_formats::LOCK_KEY_ITEM => {
                            if let Some(t) = items.template(slot.index, 0, &commands) {
                                requirements.push(format!("Requires {}", t.name));
                            }
                        }
                        benilla_formats::LOCK_KEY_SKILL => {
                            if let Some(word) = lock_type_word(slot.index) {
                                if slot.skill > 0 {
                                    requirements.push(format!("Requires {word} ({})", slot.skill));
                                } else {
                                    requirements.push(format!("Requires {word}"));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        let Some(p) = cursor_ui else {
            return; // cursor off-window: nothing to seat against this frame
        };
        script.world_tooltip_gameobject(&template.name, &requirements, p.x, p.y);
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
mod tests {
    use super::*;
    use crate::ui_action::Spells;

    /// The full spell-tooltip view off the REAL 5875 data — Fireball rank 1 (133) end to end:
    /// the pinned columns (description 138, cast index 18→1500 ms, duration 30), the token
    /// engine's byte formulas, and the view's verified cell shapes. Skips without client data.
    #[test]
    fn fireball_view_on_real_data() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let spells = Spells {
            catalog: benilla_formats::load_spell_catalog(&mut chain).expect("Spell.dbc"),
            forms: benilla_formats::load_shapeshift_forms(&mut chain)
                .expect("SpellShapeshiftForm.dbc"),
            ranges: benilla_formats::load_spell_ranges(&mut chain).expect("SpellRange.dbc"),
            cast_times: benilla_formats::load_spell_cast_times(&mut chain)
                .expect("SpellCastTimes.dbc"),
            durations: benilla_formats::load_spell_durations(&mut chain)
                .expect("SpellDuration.dbc"),
            radii: benilla_formats::load_spell_radii(&mut chain).expect("SpellRadius.dbc"),
        };
        let v = spell_tooltip_view(133, &spells, None, 0).expect("Fireball view");
        assert_eq!(v.name, "Fireball");
        assert_eq!(v.rank.as_deref(), Some("Rank 1"));
        assert_eq!(v.cost.as_deref(), Some("30 Mana"));
        assert_eq!(v.range.as_deref(), Some("35 yd range"));
        assert_eq!(v.cast_time.as_deref(), Some("1.5 sec cast"));
        assert_eq!(
            v.cooldown, None,
            "Fireball has no recovery in either column"
        );
        assert_eq!(v.requires_form, None);
        assert!(
            v.description.starts_with("Hurls a fiery ball that causes"),
            "got: {}",
            v.description
        );
        assert!(
            v.description.contains(" to ") && v.description.contains("Fire damage"),
            "the $s range substituted: {}",
            v.description
        );
        assert!(
            !v.description.contains('$'),
            "no unsubstituted tokens: {}",
            v.description
        );

        // Charge rank 1 (100) — the director's reference shot, end to end: the dual-bound range
        // row (SpellRange 95 = {8, 25}), the CATEGORY-column cooldown (recoveryTime 0 /
        // categoryRecoveryTime 15000), and the Stances-mask form line (0x10000 → form 17).
        let v = spell_tooltip_view(100, &spells, None, 0).expect("Charge view");
        assert_eq!(v.name, "Charge");
        assert_eq!(v.rank.as_deref(), Some("Rank 1"));
        assert_eq!(v.cost, None, "Charge costs nothing (it generates rage)");
        assert_eq!(v.range.as_deref(), Some("8-25 yd range"));
        assert_eq!(v.cast_time.as_deref(), Some("Instant"));
        assert_eq!(v.cooldown.as_deref(), Some("15 sec cooldown"));
        assert_eq!(v.requires_form.as_deref(), Some("Requires Battle Stance"));
        assert!(!v.form_met, "form 0 (unshifted) does not satisfy the mask");
        assert_eq!(
            v.description,
            "Charge an enemy, generate 9 rage, and stun it for 1 sec.  Cannot be used in combat."
        );
        let v = spell_tooltip_view(100, &spells, None, 17).expect("Charge view");
        assert!(v.form_met, "form 17 = Battle Stance satisfies the mask");
    }
}
