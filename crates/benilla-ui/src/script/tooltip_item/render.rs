//! The item tooltip's line law — [`render_view`], the byte-verified emission order of the
//! client's shared renderer `0x52b650` (see the parent module doc for the law's provenance and
//! the compare/red/SET summaries).

use mlua::{Lua, Table};

use crate::script::tooltip::append_line;
use crate::script::{ItemTemplateView, Model};

use super::names::*;

/// The REAL instance's contribution to the line law — the fields only a streamed item OBJECT
/// carries. A template/link hover passes `None` at the call site: the ref gates the whole
/// creator + openable/readable tail on the item-object pointer (`0x52e1c7`/`0x52e2e0` — no
/// object, no lines), and keeps the authored full max/max durability.
#[derive(Default)]
pub(super) struct ItemInstance {
    /// Live `(current, max)` durability (director-reported: the spirit healer's 25% loss
    /// showed nowhere); `None` = indestructible (max 0) or the create not yet landed.
    pub durability: Option<(u32, u32)>,
    /// The RESOLVED `ITEM_FIELD_CREATOR` name (the app's ask-once name cache — the ref's
    /// `0x55f080` probe). `None` = authorless OR the name query is still in flight: the ref
    /// emits no line either way (`0x52e209`; its resolve callback repaints the tooltip, ours
    /// is the container re-enter loop).
    pub creator: Option<String>,
    /// Instance `ITEM_FIELD_ITEM_TEXT_ID` ≠ 0 (a mail permanent copy): flips the creator line
    /// to WRITTEN_BY (`0x52e223`) and satisfies the READABLE gate (`0x52e348`).
    pub has_text: bool,
    /// Instance `ITEM_FIELD_FLAGS` — the openable lock sub-gate reads UNLOCKED `0x4`
    /// (`0x52e30c`) and the wrapped-gift arm WRAPPED `0x8` (`0x52e31d`).
    pub flags: u32,
    /// May this source emit the ITEM_OPENABLE line at all?
    ///
    /// wow-re §1-OPENABLE (byte-census 2026-07-20) reads a `[this+0x440]` gate at `0x52e2e8` as
    /// bypassing the whole openable tree for every `p6=1` binding, and concludes that
    /// `SetBagItem` — the binding your own bags hover through — can NEVER emit it. **That
    /// conclusion is contradicted by ground truth:** the director's screenshot shows a Small
    /// Barnacled Clam hovered IN A BAG with the green `<Right Click to Open>` line, and vmangos's
    /// own `ITEM_FLAG_LOOTABLE` comment names that exact client behaviour ("It or lockid set
    /// enable for client show 'Right click to open'"). A line that could only ever appear on the
    /// send-mail / auction-sell / buyback tooltips would also be behaviourally absurd. The note is
    /// additionally self-inconsistent there (§1-CREATOR puts the GIFTCREATOR read at `0x52e32a`,
    /// inside the region §1-OPENABLE calls the openable emit + jump), so the transcription — not
    /// the observation — is the suspect. A wow-re §5 re-derivation of `[0x52e2e0, 0x52e376)` is in
    /// flight; until it lands, the bag hover follows the director's eyes.
    ///
    /// The flag is kept rather than deleted because the *template-vs-instance* distinction it also
    /// carries is settled (gate 1, `0x52e2e0`: no object ⇒ no line — you can't right-click a
    /// hyperlink), and because the p6 split may still be real for the sources we cannot observe.
    pub openable_source: bool,
}

/// Render one item template into the tooltip — the BYTE-VERIFIED emission law of the shared
/// renderer `0x52b650` (wow-re `ui/scratch/tooltip-content-law.md`, §5-cross-checked 2026-07-10;
/// the proficiency-cell and SET legs byte-read directly 2026-07-11; the creator/readable
/// instance tail byte-read 2026-07-20), minus the instance-only families still unfed
/// (soulbound override, enchants, cooldown-remaining, the gift-wrap family).
pub(super) fn render_view(
    lua: &Lua,
    this: &Table,
    v: &ItemTemplateView,
    compare: bool,
    // `None` = a template/link source (the ref's no-object path).
    inst: Option<&ItemInstance>,
) -> mlua::Result<()> {
    let (req, known_spell, taught_known, set_view, equipped) = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        let knows = |id: u32| model.spellbook.slots.iter().any(|s| s.spell_id == id);
        let known = v.required_spell != 0 && knows(v.required_spell);
        // A taught spell (trigger 6, SPELL_EFFECT_LEARN) the player already knows — the
        // unconditional-red ITEM_SPELL_KNOWN "Already known" line (recipes).
        let taught = v
            .spell_triggers
            .iter()
            .any(|&(t, id, _)| t == 6 && id != 0 && knows(id));
        // The SET block's inputs: the app-resolved set view (a miss records the ask-once) and
        // the player's equipped item ids — the (owned/total) count + per-member highlight.
        let set_view = (v.item_set != 0)
            .then(|| {
                let view = model.item_sets.get(&v.item_set).cloned();
                if view.is_none() {
                    model.item_set_asks.insert(v.item_set);
                }
                view
            })
            .flatten();
        let equipped: std::collections::HashSet<u32> = model
            .inventory_slots
            .iter()
            .flatten()
            .map(|s| s.item_id)
            .collect();
        (model.player_req.clone(), known, taught, set_view, equipped)
    };
    let add = |l: (String, [f32; 4])| append_line(lua, this, l, None, false);
    let addw = |l: (String, [f32; 4])| append_line(lua, this, l, None, true);
    let add2 =
        |l: (String, [f32; 4]), r: (String, [f32; 4])| append_line(lua, this, l, Some(r), false);

    // Compare mode (the shopping tooltips): the gray CURRENTLY_EQUIPPED header (`[arg+0x18]≠0`)
    // and a WHITE name instead of the quality color (`[arg+0x14]≠0`) — both byte-verified.
    if compare {
        add(("Currently Equipped".into(), GRAY))?;
    }
    let name_color = if compare {
        WHITE
    } else {
        quality_color(v.quality)
    };
    add((v.name.clone(), name_color))?;
    // ITEM_SIGNABLE (green) — Flags bit 0x2000 (petitions).
    if v.flags & 0x2000 != 0 {
        add(("<Right Click for Details>".into(), GREEN))?;
    }
    // "Conjured Item" (Flags bit 0x2).
    if v.flags & 0x2 != 0 {
        add(("Conjured Item".into(), WHITE))?;
    }
    // The bind line (template bonding; the instance Soulbound override is a later feed).
    match v.bonding {
        1 => add(("Binds when picked up".into(), WHITE))?,
        2 => add(("Binds when equipped".into(), WHITE))?,
        3 => add(("Binds when used".into(), WHITE))?,
        4 | 5 => add(("Quest Item".into(), WHITE))?,
        _ => {}
    }
    match v.max_count {
        1 => add(("Unique".into(), WHITE))?,
        n if n > 1 => add((format!("Unique ({n})"), WHITE))?,
        _ => {}
    }
    if v.start_quest != 0 {
        add(("This Item Begins a Quest".into(), WHITE))?;
    }
    // LOCKED (red) — suppressed once the INSTANCE carries UNLOCKED `0x4` (the law's "and the
    // item is not already unlocked"; the same bit the openable sub-gate reads). The key-item
    // "Requires %s" sub-line joins with the Lock.dbc resolve (the GO-locks follow-up).
    if v.lock_id != 0 && inst.is_none_or(|i| i.flags & 0x4 == 0) {
        add(("Locked".into(), RED))?;
    }
    // Slot | type — or, for a bag, the single CONTAINER_SLOTS line in the same seat. The type
    // cell is suppressed for cloaks (InventoryType 16) and displayFlags-hidden subclasses
    // (rings/trinkets/shirts — the "Miscellaneous" family), both builder gates. The two cells
    // recolor independently (byte-read at the builder's `0x52c143..0x52c1f9` legs against the
    // verified `0x530270(this, left, right, leftColor, rightColor, wrap)` signature — NB the
    // law §10 prose has the cells swapped): a proficiency-mask miss (`0xc4d4a0[class]` bit
    // `1 << subclass`; our SMSG_SET_PROFICIENCY-fed map; no mask entry never reds) reds the
    // TYPE cell — unless a weapon's alternate subclass (ItemSubClass prereq/postreq) is
    // proficient, which reds the SLOT cell instead. Independently, an off-hand weapon
    // (InventoryType 22) reds the SLOT cell without Dual Wield (`0x5eab70` = the learned
    // effect-40 spell), even when the type itself is proficient.
    if v.container_slots > 0 {
        add((format!("{} Slot Bag", v.container_slots), WHITE))?;
    } else {
        let slot = invtype_name(v.inventory_type);
        let ty = if v.inventory_type == 16 || v.hide_subclass {
            None
        } else {
            subclass_name(v.class, v.subclass)
        };
        let mut left_red = false;
        let mut right_red = false;
        if let Some(&mask) = req.proficiency.get(&v.class) {
            if mask & (1 << v.subclass) == 0 {
                let alt_ok =
                    v.class == 2 && v.proficiency_alt.is_some_and(|a| mask & (1 << a) != 0);
                if alt_ok {
                    left_red = true;
                } else {
                    right_red = true;
                }
            }
        }
        if v.inventory_type == 22 && !req.can_dual_wield {
            left_red = true;
        }
        match (slot, ty) {
            (Some(s), Some(t)) => add2(
                (s.into(), req_color(!left_red)),
                (t.into(), req_color(!right_red)),
            )?,
            (Some(s), None) => add((s.into(), req_color(!left_red)))?,
            // No slot name: the type stands alone and takes the hard-miss color (the
            // builder's single-cell fallback keeps flag-1).
            (None, Some(t)) => add((t.into(), req_color(!right_red)))?,
            _ => {}
        }
    }
    // Damage | speed (block 1) + extra damage lines + the dps line.
    if let Some(&(min, max, sch)) = v.damages.first().filter(|d| d.1 > 0.0) {
        let mut dmg = format!(
            "{} - {}",
            (min + 0.5).floor() as i64,
            (max + 0.5).floor() as i64
        );
        if let Some(s) = school_name(sch) {
            dmg = format!("{dmg} {s}");
        }
        dmg.push_str(" Damage");
        let speed = f64::from(v.delay_ms) / 1000.0;
        if speed > 0.0 {
            add2((dmg, WHITE), (format!("Speed {speed:.2}"), WHITE))?;
        } else {
            add((dmg, WHITE))?;
        }
        for &(emin, emax, esch) in v.damages.iter().skip(1).filter(|d| d.1 > 0.0) {
            let s = school_name(esch).unwrap_or("");
            let sep = if s.is_empty() { "" } else { " " };
            add((
                format!(
                    "+ {} - {}{sep}{s} Damage",
                    (emin + 0.5).floor() as i64,
                    (emax + 0.5).floor() as i64
                ),
                WHITE,
            ))?;
        }
        // DPS — weapons only (the byte law gates on class==2), Σ(min+max)·0.5 / speed.
        if speed > 0.0 && v.class == 2 {
            let total: f32 = v
                .damages
                .iter()
                .filter(|d| d.1 > 0.0)
                .map(|&(a, b, _)| (a + b) * 0.5)
                .sum();
            add((
                format!("({:.1} damage per second)", f64::from(total) / speed),
                WHITE,
            ))?;
        }
    }
    if v.armor > 0 {
        add((format!("{} Armor", v.armor), WHITE))?;
    }
    if v.block > 0 {
        add((format!("{} Block", v.block), WHITE))?;
    }
    // Stat mods (+N Stamina …) in the client's DISPLAY order — the `0x808e88` table (byte-read:
    // 4,3,7,5,6,1,0 then 8,9,2,10 + zero padding; the builder's outer loop walks the table,
    // the inner loop scans the item's raw slots — `0x52c6b0..0x52c801`). So Strength, Agility,
    // Stamina, Intellect, Spirit, Health, Mana — never the wire order. (The table's trailing
    // ZERO entries would re-match a mana slot once per pass — a dormant client quirk nothing
    // shipped can reach: the only mana-stat item in the whole 1.12 DB is the internal "Test MP
    // Ring" 6674. Not emulated.)
    const STAT_DISPLAY_ORDER: [u32; 7] = [4, 3, 7, 5, 6, 1, 0];
    for &want in &STAT_DISPLAY_ORDER {
        for &(t, val) in &v.stats {
            if t != want || val == 0 {
                continue;
            }
            let Some(name) = stat_name(t) else { continue };
            let sign = if val > 0 { '+' } else { '-' };
            add((format!("{sign}{} {name}", val.abs()), WHITE))?;
        }
    }
    // Resistances: six equal nonzero values collapse to the ALL line; otherwise one line per
    // nonzero school with HOLY excluded from the singles loop (both byte-verified).
    const RESIST_NAMES: [&str; 6] = ["Holy", "Fire", "Nature", "Frost", "Shadow", "Arcane"];
    let first_res = v.resistances[0];
    if first_res != 0 && v.resistances.iter().all(|&r| r == first_res) {
        let sign = if first_res > 0 { '+' } else { '-' };
        add((
            format!("{sign}{} to All Resistances", first_res.abs()),
            WHITE,
        ))?;
    } else {
        for (i, &r) in v.resistances.iter().enumerate() {
            if r == 0 || i == 0 {
                continue; // Holy (i == 0) never prints singly in 1.12
            }
            let sign = if r > 0 { '+' } else { '-' };
            add((
                format!("{sign}{} {} Resistance", r.abs(), RESIST_NAMES[i]),
                WHITE,
            ))?;
        }
    }
    if let Some((cur, max)) = inst.and_then(|i| i.durability).filter(|&(_, max)| max > 0) {
        // Red iff BROKEN (durability 0) — the byte law (wow-re ui.md tooltip content law:
        // "durability (red iff broken==0)", the AddLine colour pointer `0xc0d390`, the same
        // red as the unmet-requirement lines).
        let color = if cur == 0 { RED } else { WHITE };
        add((format!("Durability {cur} / {max}"), color))?;
    } else if v.max_durability > 0 {
        add((
            format!("Durability {} / {}", v.max_durability, v.max_durability),
            WHITE,
        ))?;
    }
    // Class/race lists — red when the player's own bit is absent (the usable ask).
    if v.allowable_class > 0
        && (v.allowable_class & full_mask(&CLASS_NAMES)) != full_mask(&CLASS_NAMES)
    {
        let list: Vec<&str> = CLASS_NAMES
            .iter()
            .filter(|&&(id, _)| v.allowable_class & (1 << (id - 1)) != 0)
            .map(|&(_, n)| n)
            .collect();
        if !list.is_empty() {
            let ok = req.class_id > 0 && v.allowable_class & (1 << (req.class_id - 1)) != 0;
            add((format!("Classes: {}", list.join(", ")), req_color(ok)))?;
        }
    }
    if v.allowable_race > 0 && (v.allowable_race & full_mask(&RACE_NAMES)) != full_mask(&RACE_NAMES)
    {
        let list: Vec<&str> = RACE_NAMES
            .iter()
            .filter(|&&(id, _)| v.allowable_race & (1 << (id - 1)) != 0)
            .map(|&(_, n)| n)
            .collect();
        if !list.is_empty() {
            let ok = req.race_id > 0 && v.allowable_race & (1 << (req.race_id - 1)) != 0;
            add((format!("Races: {}", list.join(", ")), req_color(ok)))?;
        }
    }
    // ITEM_MIN_LEVEL prints only for RequiredLevel > 1 (byte-VERIFIED `0x52d2cf`: `cmp esi,0x1 /
    // jle skip`) — a level-1 requirement gates nothing a logged-in player could fail, so the
    // real client hides it on all the level-1 consumables that carry one.
    if v.required_level > 1 {
        add((
            format!("Requires Level {}", v.required_level),
            req_color(req.level >= v.required_level),
        ))?;
    }
    if let Some(skill) = &v.required_skill_name {
        let have = req.skills.get(&v.required_skill).copied().unwrap_or(0);
        let line = if v.required_skill_rank > 0 {
            format!("Requires {skill} ({})", v.required_skill_rank)
        } else {
            format!("Requires {skill}")
        };
        add((line, req_color(have >= v.required_skill_rank.max(1))))?;
    }
    if v.required_spell != 0 {
        if let Some(name) = &v.required_spell_name {
            add((format!("Requires {name}"), req_color(known_spell)))?;
        }
    }
    if let Some(rep) = &v.required_rep_line {
        // Red when the player's rank with the faction is below the requirement (the §1-RED rep
        // leg: standing `< [+0x58]`); an unfed faction reads as unmet, like the real client's
        // empty store at login (INITIALIZE_FACTIONS lands before the world does).
        let rank = req
            .rep_ranks
            .get(&v.required_rep_faction)
            .copied()
            .unwrap_or(0);
        add((
            rep.clone(),
            req_color(u32::from(rank) >= v.required_rep_rank),
        ))?;
    }
    // ITEM_SPELL_KNOWN — a taught spell the player already knows, UNCONDITIONALLY red.
    if taught_known {
        add(("Already known".into(), RED))?;
    }
    // Green trigger lines (Use:/Equip:/Chance on hit:), wrapped — the text is app-resolved (the
    // spell's name in P1; its $-substituted description via the token engine in P2).
    for &(trigger, _, ref text) in &v.spell_triggers {
        let prefix = match trigger {
            0 | 5 => "Use: ",
            1 => "Equip: ",
            2 => "Chance on hit: ",
            _ => continue,
        };
        addw((format!("{prefix}{text}"), GREEN))?;
    }
    match v.charges {
        1 => add(("1 Charge".into(), WHITE))?,
        n if n > 1 => add((format!("{n} Charges"), WHITE))?,
        _ => {}
    }
    // The item-SET block (§22, ABOVE the compact cut), byte-read at the builder's
    // `0x52d8a0..0x52e0f5`: a blank gold line, the gold "name (owned/total)" header, the
    // set-level skill line (white, red when short), the member ladder ("  name" — pale-cream
    // `0xc0d368` when equipped, gray otherwise; a member whose template is still in flight
    // waits — the app re-pushes the view as names land), a second blank, then the threshold
    // bonuses "(N) Set: text" sorted THRESHOLD-ASCENDING (the builder qsorts the slot indices
    // via `0x52e5c0` — never the DBC's stored order), green only when the skill requirement
    // is met (`0x5eaae0`) AND owned ≥ threshold. "Owned" counts EQUIPPED members on both
    // builder paths (the owner-unit 19-slot scan / the hyperlink walk with section mask 0x1).
    if let Some(set) = &set_view {
        let owned = set
            .members
            .iter()
            .filter(|(id, _)| equipped.contains(id))
            .count();
        addw((String::new(), GOLD))?;
        add((
            format!("{} ({}/{})", set.name, owned, set.members.len()),
            GOLD,
        ))?;
        let skill_met = set.required_skill == 0 || {
            let have = req.skills.get(&set.required_skill).copied().unwrap_or(0);
            have >= set.required_skill_rank
        };
        if let Some(skill) = &set.required_skill_name {
            let line = if set.required_skill_rank > 0 {
                format!("Requires {skill} ({})", set.required_skill_rank)
            } else {
                format!("Requires {skill}")
            };
            add((line, req_color(skill_met)))?;
        }
        for (id, name) in &set.members {
            let Some(name) = name else { continue };
            let color = if equipped.contains(id) { CREAM } else { GRAY };
            add((format!("  {name}"), color))?;
        }
        addw((String::new(), GOLD))?;
        let mut bonuses: Vec<&(u32, String)> = set.bonuses.iter().collect();
        bonuses.sort_by_key(|&&(threshold, _)| threshold);
        for &(threshold, ref text) in bonuses {
            let color = if skill_met && owned as u32 >= threshold {
                GREEN
            } else {
                GRAY
            };
            addw((format!("({threshold}) Set: {text}"), color))?;
        }
    }
    // The compact/compare early-return (`0x52e14c`, `[arg+0x14]≠0`): everything below —
    // description, made-by, openable/readable, money — is skipped on a shopping tooltip.
    if compare {
        return Ok(());
    }
    // The quoted flavor text — gold, wrapped, literal quotes (all three byte-verified).
    if !v.description.is_empty() {
        addw((format!("\"{}\"", v.description), GOLD))?;
    }
    // Everything below rides the REAL-INSTANCE gate (`0x52e1c7`/`0x52e2e0`): a template/link
    // hover emits no creator line and no openable/readable line — you can't right-click a
    // hyperlink open.
    let Some(inst) = inst else { return Ok(()) };
    // The creator line (`0x52e1b1..0x52e2db`, wow-re §1-CREATOR CONFIRMED): the resolved
    // `ITEM_FIELD_CREATOR` name — a letter (instance text id) is ITEM_WRITTEN_BY, anything
    // else ITEM_CREATED_BY, both the literal 1.12 GlobalStrings (the Made-by green is the
    // string's OWN `|cff00ff00` escape; the AddLine color pointer is white `0xc0cf60`, wrap 0).
    // A WRAPPED instance (`ITEM_FIELD_FLAGS` bit `0x8` — the arm gate `0x52b7b0`, instance bit
    // only) switches the whole line to ITEM_WRAPPED_BY off `ITEM_FIELD_GIFTCREATOR`; that
    // field has no feed yet, so a wrapped instance emits NOTHING here rather than a wrong
    // "Made by" (gifts join with the wrap/unwrap arc).
    if inst.flags & 0x8 == 0 {
        if let Some(name) = &inst.creator {
            if inst.has_text {
                add((format!("Written by {name}"), WHITE))?;
            } else {
                add((format!("|cff00ff00<Made by {name}>|r"), WHITE))?;
            }
        }
    }
    // ITEM_OPENABLE / ITEM_READABLE — ONE line, openable wins (the if/else at
    // `0x52e2f2..0x52e35d`, wow-re §1-OPENABLE): openable = a p6=0 source AND the template
    // loot flag `0x4` behind its lock sub-gate (LockID set → only once the instance carries
    // UNLOCKED `0x4`), or a wrapped gift (template WRAPPER `0x200` + instance WRAPPED `0x8`);
    // readable = template PageText (the CGItem vtable `+0x74` getter `0x5d9e10`) OR the
    // instance letter text. Both green.
    let openable = inst.openable_source
        && ((v.flags & 0x4 != 0 && (v.lock_id == 0 || inst.flags & 0x4 != 0))
            || (v.flags & 0x200 != 0 && inst.flags & 0x8 != 0));
    if openable {
        add(("<Right Click to Open>".into(), GREEN))?;
    } else if v.page_text != 0 || inst.has_text {
        add(("<Right Click to Read>".into(), GREEN))?;
    }
    Ok(())
}
