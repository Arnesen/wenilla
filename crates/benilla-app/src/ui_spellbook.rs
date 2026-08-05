//! The spellbook window feed (decision 0216 §8, slice 5) — the spell **source** for the cursor
//! payload system: builds a book model from `PlayerActions.spells` (`SMSG_INITIAL_SPELLS`, the
//! known-spell set `ui_action.rs` already streams) through the `SpellCatalog`/`SkillLineCatalog`
//! join, and drives `benilla_ui::script::spellbook`'s snapshot + cast-drain seam behind
//! `SpellBookFrame.xml` (the P key, `ui_script/input.rs`).
//!
//! The **add-gate** is byte-verified (decision 0227; wow-re `system/ui/scratch/
//! spellbook-book-build.md`): a known spell reaches the book only when
//! [`SpellDisplay::in_spellbook`](benilla_formats::SpellDisplay::in_spellbook) — `Attributes` not
//! `DO_NOT_DISPLAY`/`IS_TRADESKILL`, `castUI == 0`. Languages, armor/weapon proficiencies, and
//! hidden racial passives are excluded exactly as the real client excludes them, so a live
//! character's book no longer grows the junk tabs the pre-0227 build showed.
//!
//! Tab classification is byte-verified (decision 0228; wow-re
//! `system/ui/scratch/spellbook-book-build.md` §3): a spell's `SkillLineAbility` line is routed
//! through the per-race/class table (`0x6ddf90` → `SkillRaceClassInfo.dbc`) — if no row admits the
//! player's race+class, or the matching row carries `SKILL_FLAG_DISPLAY_SORTED` (`flags & 0x80`),
//! the spell lands in the **General** tab (key 0) instead of its line's own. That collapses
//! racials, generic (GenericDND), and cross-class spells (a warrior's cheated Fireball) into
//! General, while class-native abilities keep their class tab — exactly what the real client shows.
//! [`build_book`] reads the player's race/class from the self `ObjectStore` and defers the routing
//! to [`SkillLineCatalog::spell_tab`]; absent a character (capture with no self player) or the
//! `SkillRaceClassInfo` data, the collapse is skipped and each line keeps its own tab.
//!
//! Tab order + book order follow the same §5 (`0x4b3040`/`0x4b30c0`): **General/key-0 pinned
//! first**, then alphabetical by `SkillLine.dbc` name (locale collator `0x64a480` — plain byte
//! order stands in for enUS); within a tab, name → **parsed rank number** ([`spell_sort_key`]).
//!
//! INTERIM (flagged, none load-bearing): the multi-row `SkillRaceClassInfo` tie-break (first
//! admitting row wins — the client's `0x6ddf90` returns one; consistent for the lines that matter),
//! and the rank tie-break falling to the rank *string* only when the parsed number is 0 (the
//! client's `SpellLevel` fallback for a digit-less rank is unmodeled — we have no `SpellLevel`).

use std::collections::{BTreeMap, HashSet};

use bevy::prelude::*;

use benilla_formats::{SkillLineCatalog, SpellCatalog};
use benilla_ui::script::{SpellBookState, SpellSlotView, SpellTabView, UiScript};

use crate::assets::{AssetSet, LockRecover, WorldAssets};
use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{NetCommands, ObjectStore, SelfPlayer};
use crate::ui_action::{
    cast_target, melee_auto_attack_icon, ranged_weapon_icon, CastCommit, CastLadder, PlayerActions,
    Spells,
};
use crate::ui_script::UiInput;
use crate::ui_unit::UnitFeed;

/// The skill-line catalog (`SkillLine.dbc` × `SkillLineAbility.dbc`) — absent when the client
/// data failed to load (every consumer tolerates that; `ui_action::Spells`' own precedent).
#[derive(Resource)]
pub(crate) struct SkillLines {
    pub(crate) catalog: SkillLineCatalog,
}

/// The line id for a spell with no resolvable skill line — the **General tab, key 0**, which the
/// client pins FIRST in the tab strip and renders with a fixed name + icon (byte-verified: wow-re
/// spellbook-book-build.md, `GetSpellTabInfo 0x4b3ce0` — key 0 → the localized `GENERAL`
/// GlobalString name `0x846910` + the hardcoded icon `Interface\Icons\Ability_Kick` `0x8468f0`).
/// `0` is never a real vmangos `SkillType` (`SharedDefines.h`'s `SKILL_NONE = 0`), so it can't
/// collide with a real line id.
const NO_LINE: u32 = 0;

/// The General tab's icon — hardcoded in the client (`GetSpellTabInfo 0x4b3ce0`, `0x8468f0`), not
/// a `SkillLine.dbc` lookup (key 0 has no DBC row). Extensionless, as the DBC/BLP loader expects.
const GENERAL_TAB_ICON: &str = "Interface\\Icons\\Ability_Kick";

pub(crate) struct UiSpellbookPlugin;

impl Plugin for UiSpellbookPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_skill_lines.after(AssetSet::Open))
            .add_systems(
                Update,
                (
                    // Feed rides with the unit feed like ui_action's own (before UiInput, so
                    // P-key-open-this-frame already sees a populated book); the cast drain runs
                    // after UiInput so a click's CastSpell goes out the same frame.
                    // `.before(CooldownEvents)`: the per-slot cooldown triples must be in the VM
                    // before `feed_action_state`'s synchronous `SPELL_UPDATE_COOLDOWN` makes the
                    // book buttons re-read them (that set's own doc).
                    feed_spellbook
                        .in_set(UnitFeed)
                        .before(crate::ui_action::CooldownEvents)
                        .before(UiInput),
                    drain_spell_casts.after(UiInput),
                ),
            );
    }
}

fn load_skill_lines(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_skill_line_catalog(&mut chain)
    };
    match loaded {
        Ok(catalog) => {
            info!(
                "ui_spellbook: {} skill line(s) in the tab catalog",
                catalog.len()
            );
            commands.insert_resource(SkillLines { catalog });
        }
        Err(e) => warn!(
            "ui_spellbook: SkillLine.dbc failed to load — every spell falls into one \
             fallback tab: {e:#}"
        ),
    }
}

/// The feed's memory of what it last pushed, for the `SPELLS_CHANGED` diff.
#[derive(Default)]
struct FeedMemory {
    pushed: SpellBookState,
}

#[allow(clippy::too_many_arguments)] // a Bevy system's full input set
fn feed_spellbook(
    script: Option<NonSendMut<UiScript>>,
    actions: Res<PlayerActions>,
    spells: Option<Res<Spells>>,
    skill_lines: Option<Res<SkillLines>>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    mut items: ResMut<Items>,
    icons: Option<Res<ItemDisplays>>,
    commands: Res<NetCommands>,
    cooldowns: Res<crate::cooldowns::Cooldowns>,
    clock: Res<crate::ui_script::UiClock>,
    mut memory: Local<FeedMemory>,
) {
    let Some(mut script) = script else {
        return;
    };
    // Nothing to resolve a name/icon/passive from yet — try again once Spell.dbc lands.
    let Some(spells) = spells.as_deref() else {
        return;
    };
    // The player's race/class drive the General collapse (module doc). Absent (no self player
    // yet), 0/0 skips the collapse — each line keeps its own tab until the descriptor arrives.
    let store = self_q.single().ok();
    let (race, class) = store
        .map(|s| (s.0.unit_race().unwrap_or(0), s.0.unit_class().unwrap_or(0)))
        .unwrap_or((0, 0));
    // The melee auto-attack's icon is the equipped main-hand weapon (or Spell-Reset when unarmed),
    // not spell 6603's `Temp` placeholder (decision 0230) — resolved here where the self player +
    // item stores are in hand, once for the whole page (it's the same for any auto-attack spell).
    let attack_icon = store
        .map(|s| melee_auto_attack_icon(s, &spells.forms, &mut items, icons.as_deref(), &commands));
    // The ranged auto-repeat shots (Auto Shot, wand Shoot) borrow the equipped ranged weapon's
    // icon the same way (decision 0231's ranged case; `None` — unarmed/thrown — keeps the
    // spell's own icon, never Spell-Reset). Character-level like the melee icon: one resolve
    // serves the page.
    let ranged_icon =
        store.and_then(|s| ranged_weapon_icon(s, &mut items, icons.as_deref(), &commands));
    let mut fresh = build_book(
        &actions.spells,
        &spells.catalog,
        skill_lines.as_deref().map(|s| &s.catalog),
        race,
        class,
        attack_icon,
        ranged_icon,
    );
    // The `IsCurrentCast` verdict per slot — the delegate `0x4b3600` (wow-re
    // `spellbook-checked-predicate.md`, §5-verified): its OWN function, NOT the action bar's
    // `0x4e53a0`, and deliberately narrower — TWO arms only. The shapeshift arm is built here: a
    // MOD_SHAPESHIFT spell reads current exactly while the player's form byte equals its form id
    // (keyed on the FORM, so any spell/rank granting it reads true — Ghost Wolf's slot glows
    // while shifted, a druid form's likewise). The other arm (the open trade-skill window) stays
    // unmodeled with the trade-skill session itself. The action predicate's casting-now /
    // awaiting-target / item / attack arms are ABSENT in the binary — a book slot must NOT light
    // during an ordinary cast (the verdict's load-bearing negative).
    let form_byte = store.map(|s| s.0.unit_shapeshift_form()).unwrap_or(0);
    if form_byte != 0 {
        for slot in &mut fresh.slots {
            slot.current = spells
                .catalog
                .get(slot.spell_id)
                .is_some_and(|d| d.shapeshift_form == Some(u32::from(form_byte)));
        }
    }
    // Each slot's cooldown triple — the ONE store's per-spell read (`Cooldowns::info`, the
    // `GetCooldownInfo 0x6e13e0` resolve: id, category, and GCD spread alike), converted to the
    // GetTime clock exactly like the action/bag feeds (`CooldownInfo::ui_triple` is frame-stable
    // per arm, so a running cooldown never churns this diff). The XML's SPELL_UPDATE_COOLDOWN
    // handler re-reads these through `GetSpellCooldown` — the plugin's `.before(CooldownEvents)`
    // guarantees they're pushed before that event fires.
    let (anchor, ui_now) = (clock.anchor, clock.ui_now);
    for slot in &mut fresh.slots {
        slot.cooldown = cooldowns
            .info(slot.spell_id, 0, spells.catalog.get(slot.spell_id), anchor)
            .ui_triple(anchor, ui_now);
    }
    if fresh != memory.pushed {
        // Two event edges off one diff: the ref fires CURRENT_SPELL_CAST_CHANGED for a checked-
        // ring move and SPELLS_CHANGED for the book itself; a same-frame both fires both.
        let ring_moved = fresh.slots.len() != memory.pushed.slots.len()
            || fresh
                .slots
                .iter()
                .zip(&memory.pushed.slots)
                .any(|(a, b)| a.current != b.current);
        let book_changed = fresh.tabs != memory.pushed.tabs
            || fresh.slots.len() != memory.pushed.slots.len()
            || fresh.slots.iter().zip(&memory.pushed.slots).any(|(a, b)| {
                (a.spell_id, &a.name, &a.rank, &a.texture, a.passive)
                    != (b.spell_id, &b.name, &b.rank, &b.texture, b.passive)
            });
        debug!(
            "ui_spellbook: fed {} tab(s), {} spell(s) (book {book_changed}, ring {ring_moved})",
            fresh.tabs.len(),
            fresh.slots.len()
        );
        script.set_spellbook(fresh.clone());
        memory.pushed = fresh;
        if book_changed {
            script.fire_event("SPELLS_CHANGED", vec![]);
        }
        if ring_moved {
            script.fire_event("CURRENT_SPELL_CAST_CHANGED", vec![]);
        }
    }
}

/// Build the whole book (module doc): the app's own resolve — the engine holds no spell
/// knowledge, only what's pushed here.
fn build_book(
    known: &HashSet<u32>,
    catalog: &SpellCatalog,
    skill_lines: Option<&SkillLineCatalog>,
    race: u8,
    class: u8,
    attack_icon: Option<String>,
    ranged_icon: Option<String>,
) -> SpellBookState {
    let mut by_line: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &spell_id in known {
        // The byte-verified add-gate (module doc; `SpellDisplay::in_spellbook`): languages,
        // armor/weapon proficiencies, hidden passives, tradeskills, and castUI spells never
        // reach the book. A spell absent from `Spell.dbc` can't be classified or rendered, so it
        // is dropped too (there is no name/icon/line to show).
        if !catalog.get(spell_id).is_some_and(|d| d.in_spellbook()) {
            continue;
        }
        // The tab is the spell's skill line routed through the race/class General collapse
        // (module doc; `spell_tab` returns `NO_LINE`=0 for General). No skill-line catalog →
        // General for everything (no line info at all).
        let tab = skill_lines
            .map(|c| c.spell_tab(spell_id, race, class))
            .unwrap_or(NO_LINE);
        by_line.entry(tab).or_default().push(spell_id);
    }

    // Resolve each line's display face, then order the TABS the client's way (byte-verified,
    // module doc): General/key-0 pinned first, every other tab alphabetical by SkillLine.dbc
    // name. Key 0 (the General tab) has no DBC row — it takes the client's own hardcoded
    // name + icon; an unresolved REAL line id best-efforts to the same label but no icon.
    let mut lines: Vec<(u32, String, Option<String>, Vec<u32>)> = by_line
        .into_iter()
        .map(|(line_id, spell_ids)| {
            let (name, texture) = if line_id == NO_LINE {
                ("General".to_string(), Some(GENERAL_TAB_ICON.to_string()))
            } else {
                match skill_lines.and_then(|c| c.line(line_id)) {
                    Some(info) => (info.name.clone(), info.icon.clone()),
                    None => ("General".to_string(), None),
                }
            };
            (line_id, name, texture, spell_ids)
        })
        .collect();
    lines.sort_by(|a, b| (a.0 != NO_LINE).cmp(&(b.0 != NO_LINE)).then(a.1.cmp(&b.1)));

    let mut tabs = Vec::with_capacity(lines.len());
    let mut slots = Vec::new();
    for (_, name, texture, mut spell_ids) in lines {
        spell_ids.sort_by_key(|&a| spell_sort_key(catalog, a));
        let offset = slots.len() as u32;
        let num_spells = spell_ids.len() as u32;
        for spell_id in spell_ids {
            let d = catalog.get(spell_id);
            // The melee auto-attack shows the equipped weapon / Spell-Reset, not spell 6603's
            // `Temp` placeholder (decision 0231) — keyed on the effect type, the same substitution
            // the action bar makes; `attack_icon` is `None` only with no character to read.
            let texture = if d.is_some_and(|d| d.is_melee_auto_attack()) {
                attack_icon
                    .clone()
                    .or_else(|| d.and_then(|d| d.icon.clone()))
            } else if d.is_some_and(|d| d.ranged_icon_substitution()) {
                // Auto Shot / wand Shoot borrow the ranged weapon's icon; absent one (or a
                // thrown), the spell's own icon — the `0x4e6990` null-return hand-over.
                ranged_icon
                    .clone()
                    .or_else(|| d.and_then(|d| d.icon.clone()))
            } else {
                d.and_then(|d| d.icon.clone())
            };
            slots.push(SpellSlotView {
                spell_id,
                name: d.map(|d| d.name.clone()).unwrap_or_default(),
                rank: d.and_then(|d| d.rank.clone()),
                texture,
                passive: d.is_some_and(|d| d.passive),
                // The IsCurrentCast verdict and the cooldown triple are stamped by the feed
                // after the build (they need the live form/cooldown state, not the catalog).
                current: false,
                cooldown: None,
            });
        }
        tabs.push(SpellTabView {
            name,
            texture,
            offset,
            num_spells,
        });
    }
    SpellBookState { tabs, slots }
}

/// The book comparator `0x4b30c0`'s name → rank tail (module doc; the category-ordinal head is the
/// per-tab grouping itself): locale name, then the **parsed rank number** ascending, then the rank
/// string as the tie-break the client only reaches when the parsed number is 0. The client parses
/// the rank by scanning the first digit run of the `NameSubtext` string (`0x4b3c30`: `isdigit`,
/// `n = n*10 + digit`) — not `strip_prefix("Rank ")` — so this scans the same way, catching any
/// "Rank N"/localized-prefix form. (The client's `SpellLevel` fallback for a digit-less rank isn't
/// modeled — module doc.)
fn spell_sort_key(catalog: &SpellCatalog, spell_id: u32) -> (String, u32, String) {
    let d = catalog.get(spell_id);
    let name = d.map(|d| d.name.clone()).unwrap_or_default();
    let rank_str = d.and_then(|d| d.rank.clone()).unwrap_or_default();
    (name, leading_number(&rank_str), rank_str)
}

/// The first run of ASCII digits in `s` as a number, or `0` when there is none — the client's own
/// rank parse (`0x4b3c30`, module doc): skip to the first digit, then fold the digit run.
fn leading_number(s: &str) -> u32 {
    s.chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .fold(None::<u32>, |acc, c| {
            Some(acc.unwrap_or(0) * 10 + c.to_digit(10).unwrap())
        })
        .unwrap_or(0)
}

/// Drain `take_spell_casts` through the SAME cast tail `drain_action_uses` uses for a SPELL-kind
/// action (decision 0216 §8: "root-cause rule: one cast-send path") — `ui_action::send_spell_cast`.
fn drain_spell_casts(
    script: Option<NonSendMut<UiScript>>,
    targeting: cast_target::CastTargeting,
    mut ladder: CastLadder,
) {
    let Some(mut script) = script else {
        return;
    };
    for spell_id in script.take_spell_casts() {
        // The `CastSpell` dispatcher's two press-again-to-cancel forks (`0x4b3300`, wow-re
        // `shapeshift-plaincast-toggle.md`), in the ref's own order: the active-action toggle
        // (`0x4b36f0` → cancel `0x4b3466`), then the shapeshift form-match fork
        // (`0x4b348b`–`0x4b35e5`) with its non-cancelable silent no-op (`0x4b35cf`). This is the
        // leg `UseAction` does NOT have — a druid's `/cast Cat Form` powershift-out and a
        // shaman's Ghost Wolf re-cast both land here.
        if let (Some(sp), Some(store)) =
            (ladder.spells.as_ref(), targeting.self_store.iter().next())
        {
            if let Some(d) = sp.catalog.get(spell_id) {
                if crate::ui_action::toggle::active_action_toggle(spell_id, d, store) {
                    debug!("ui_spellbook: cast {spell_id} re-pressed — aura cancels");
                    let _ = ladder
                        .commands
                        .0
                        .send(crate::net::ClientCommand::CancelAura { spell_id });
                    continue;
                }
                let form = store.0.unit_shapeshift_form();
                let row = sp.forms.get(&u32::from(form));
                match crate::ui_action::toggle::form_recast_disposition(d, form, row) {
                    Some(true) => {
                        debug!("ui_spellbook: cast {spell_id} — the active form cancels");
                        let _ = ladder
                            .commands
                            .0
                            .send(crate::net::ClientCommand::CancelAura { spell_id });
                        continue;
                    }
                    Some(false) => {
                        debug!("ui_spellbook: cast {spell_id} — non-cancelable form, silent no-op");
                        continue;
                    }
                    None => {}
                }
            }
        }
        debug!(
            "ui_spellbook: cast {spell_id} (target {:?})",
            targeting.selection.guid
        );
        ladder.send(spell_id, &targeting.context(), CastCommit::Spell);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_formats::SpellDisplay;
    use std::collections::HashMap;

    #[test]
    fn leading_number_scans_the_first_digit_run() {
        assert_eq!(leading_number("Rank 1"), 1);
        assert_eq!(leading_number("Rank 10"), 10);
        assert_eq!(leading_number("Rank 2"), 2);
        assert_eq!(leading_number(""), 0);
        assert_eq!(leading_number("Racial"), 0); // digit-less → 0 (client falls to SpellLevel)
        assert_eq!(leading_number("Apprentice"), 0);
    }

    /// A minimal `SpellDisplay`: `attributes`/`cast_ui` drive the gate, `rank` the sort.
    fn spell(name: &str, rank: Option<&str>, attributes: u32) -> SpellDisplay {
        SpellDisplay {
            name: name.into(),
            rank: rank.map(Into::into),
            attributes,
            passive: attributes & 0x40 != 0,
            ..Default::default()
        }
    }

    /// The add-gate + General tab through `build_book`, no skill lines (every shown spell lands
    /// in the pinned General tab): hidden spells (DO_NOT_DISPLAY / IS_TRADESKILL) never appear,
    /// shown spells sort name→rank, and the General tab takes the client's hardcoded name + icon.
    #[test]
    fn build_book_gates_hidden_spells_and_pins_general() {
        let mut map = HashMap::new();
        map.insert(133, spell("Fireball", Some("Rank 2"), 0x10000));
        map.insert(145, spell("Fireball", Some("Rank 1"), 0x10000)); // out-of-order rank
        map.insert(2136, spell("Fire Blast", Some("Rank 1"), 0x0));
        map.insert(668, spell("Language: Common", None, 0x80)); // DO_NOT_DISPLAY → hidden
        map.insert(818, spell("Cooking", None, 0x20)); // IS_TRADESKILL → hidden
        let catalog = SpellCatalog::from_displays(map);
        let known: HashSet<u32> = [133, 145, 2136, 668, 818].into_iter().collect();

        // No skill-line catalog → every shown spell lands in General (race/class irrelevant).
        let book = build_book(&known, &catalog, None, 1, 1, None, None);

        // One General tab, hardcoded face; only the three shown spells.
        assert_eq!(book.tabs.len(), 1);
        assert_eq!(book.tabs[0].name, "General");
        assert_eq!(book.tabs[0].texture.as_deref(), Some(GENERAL_TAB_ICON));
        assert_eq!(book.tabs[0].num_spells, 3);
        // The hidden pair is gone; the shown spells sort name A-Z then rank ascending.
        let order: Vec<(&str, Option<&str>)> = book
            .slots
            .iter()
            .map(|s| (s.name.as_str(), s.rank.as_deref()))
            .collect();
        assert_eq!(
            order,
            vec![
                ("Fire Blast", Some("Rank 1")),
                ("Fireball", Some("Rank 1")),
                ("Fireball", Some("Rank 2")),
            ]
        );
    }

    /// The melee auto-attack (keyed on `Effect[0] == SPELL_EFFECT_ATTACK`, not the id) shows the
    /// pre-resolved character icon `build_book` is handed, never spell 6603's `Temp` placeholder
    /// (decision 0231). `attack_icon` carries the weapon-or-Spell-Reset resolution done in the feed;
    /// `None` (no character to read) falls back to the spell's own icon.
    #[test]
    fn build_book_attack_shows_the_resolved_icon_by_effect() {
        const ATTACK: u32 = 6603;
        let mut attack = spell("Attack", None, 0x10);
        attack.icon = Some("Interface\\Icons\\Temp".into()); // the real DBC placeholder
        attack.effect_1 = 78; // SPELL_EFFECT_ATTACK — makes it the melee auto-attack
        let catalog = SpellCatalog::from_displays(HashMap::from([(ATTACK, attack)]));
        let known: HashSet<u32> = [ATTACK].into_iter().collect();

        // The feed's resolved icon (armed → the weapon, unarmed → Spell-Reset) wins over Temp.
        for resolved in [
            "Interface\\Icons\\INV_Sword_04",
            "Interface\\Buttons\\Spell-Reset",
        ] {
            let book = build_book(&known, &catalog, None, 1, 1, Some(resolved.into()), None);
            assert_eq!(book.slots.len(), 1);
            assert_eq!(book.slots[0].spell_id, ATTACK);
            assert_eq!(book.slots[0].texture.as_deref(), Some(resolved));
        }

        // No character to read (attack_icon None): falls back to the spell's own icon.
        let bare = build_book(&known, &catalog, None, 1, 1, None, None);
        assert_eq!(
            bare.slots[0].texture.as_deref(),
            Some("Interface\\Icons\\Temp")
        );
    }

    /// The ranged auto-repeat shots (both bits: `Attributes & 0x2` + `AttributesEx2 & 0x20`)
    /// show the pre-resolved ranged weapon icon; without one they keep the spell's OWN icon —
    /// never `Spell-Reset` (the `0x4e6990` helper's null-return hand-over, decision 0231's
    /// ranged case). A ranged-slot-only spell (Throw's `0x2` alone) never substitutes.
    #[test]
    fn build_book_ranged_shots_borrow_the_ranged_weapon_icon() {
        const AUTO_SHOT: u32 = 75;
        const THROW: u32 = 2764;
        let mut auto_shot = spell("Auto Shot", None, 0x2);
        auto_shot.icon = Some("Interface\\Icons\\Ability_AutoShot".into());
        auto_shot.attributes_ex2 = 0x20;
        let mut throw = spell("Throw", None, 0x2);
        throw.icon = Some("Interface\\Icons\\Ability_Throw".into());
        let catalog =
            SpellCatalog::from_displays(HashMap::from([(AUTO_SHOT, auto_shot), (THROW, throw)]));
        let known: HashSet<u32> = [AUTO_SHOT, THROW].into_iter().collect();

        let bow = "Interface\\Icons\\INV_Weapon_Bow_02";
        let book = build_book(&known, &catalog, None, 1, 1, None, Some(bow.into()));
        let icon_of = |b: &SpellBookState, id: u32| {
            b.slots
                .iter()
                .find(|s| s.spell_id == id)
                .and_then(|s| s.texture.clone())
        };
        assert_eq!(icon_of(&book, AUTO_SHOT).as_deref(), Some(bow));
        assert_eq!(
            icon_of(&book, THROW).as_deref(),
            Some("Interface\\Icons\\Ability_Throw"),
            "ranged-slot alone (no auto-repeat bit) keeps the spell icon"
        );

        // No ranged weapon: Auto Shot keeps its own icon (no Spell-Reset on the ranged path).
        let bare = build_book(&known, &catalog, None, 1, 1, None, None);
        assert_eq!(
            icon_of(&bare, AUTO_SHOT).as_deref(),
            Some("Interface\\Icons\\Ability_AutoShot")
        );
    }
}
