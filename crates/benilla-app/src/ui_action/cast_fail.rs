//! Cast-failure display strings — the reference's **two-layer** pipeline, byte-verified and
//! §5 cross-checked (wow-re `system/spell/scratch/cast-fail-strings.md`; decision 0427):
//!
//! 1. `HandleCastFailed 0x6e1a00` resolves the wire reason through the name-identity table
//!    `0x6e23e0` (wire order = the vmangos `SpellCastResult` enum) into
//!    `GetText("SPELL_FAILED_<name>")` — [`CAST_FAIL_KEYS`].
//! 2. A per-reason **errorId** (default `0x2c` `ERR_SPELL_FAILED_S` = `"%s"`, a pure
//!    passthrough) is handed to `CGGameUI::DisplayError 0x496720`; ~12 reasons override it,
//!    REPLACING the message entirely (their `ERR_*` string has no `%s`). This is why the
//!    screen shows "Spell is not ready yet." while `SPELL_FAILED_NOT_READY` reads
//!    "Not yet recovered", and "Not enough rage" for a rage spell's NO_POWER.
//!
//! 3. A per-reason **argument arm** (the second dispatch `0x6e1d8e`, 13 targets) fills that
//!    message's own `%s`/`%d` from a DBC or item name before it is displayed. The two arms whose
//!    tables benilla already loads live in [`FailArgs::fill`]; the drain fills three more before
//!    calling here (below), and the rest strip.
//!
//! Strings are never hardcoded here: every message resolves from the VM's loaded
//! `GlobalStrings.lua` by key, so localization rides for free. Suppression is faithful on
//! both mechanisms: reason `0x17` (DONT_REPORT) never reaches display (control-flow), and a
//! key absent from GlobalStrings displays as nothing (data — 0x08/0x21/0x75, happiness
//! NO_POWER).
//!
//! **The argument arms, and what is still approximate.** Filled: `0x5e` REQUIRES_SPELL_FOCUS and
//! `0x5d` REQUIRES_AREA here (decision 1313 — `0x6e1f62`/`0x6e1fad`), and `0x78` TOTEMS / `0x5c`
//! REAGENTS / `0x19`–`0x1b` EQUIPPED_ITEM_CLASS\* in the drain, which owns them because their
//! fills need the item caches and the query-then-redisplay cache-miss behavior ("Requires Mining
//! Pick", decisions 0545 + 0552). Still stripped rather than filled — each needs a DBC we do not
//! load yet: `0x56` ONLY_SHAPESHIFT (form-name list), `0x8d` PREVENTED_BY_MECHANIC
//! (`SpellMechanic.dbc`), `0x90` MIN_SKILL (`SkillLine.dbc`), `0x31` NEED_EXOTIC_AMMO and `0x84`
//! PROSPECT_NEED_MORE. Stripping is a **deliberate divergence**, now byte-confirmed as one: on a
//! bad id or an absent word the reference jumps to the default arm with the pointer still on the
//! *unfilled* template, so it displays a literal `Requires %s` (wow-re §WIRE-ARGS C3 — the fill
//! path's own buffer swap sits after the printf and is skipped). We show the bare stem instead:
//! "Requires" reads as terse, "Requires %s" reads as broken software (§7 — judge by the result).
//! `0x0a`'s item-spell leg (`ERR_INVALID_ITEM_TARGET`) is unmodeled — the drain does not know
//! item-ness.

use benilla_formats::{AreaTableCatalog, SpellDisplay, SpellFocusCatalog};

/// Wire reason → its `SPELL_FAILED_*` GlobalStrings key (the `0x6e23e0` table, byte-exact).
pub(super) const CAST_FAIL_KEYS: [&str; 146] = [
    "SPELL_FAILED_AFFECTING_COMBAT",             // 0x00
    "SPELL_FAILED_ALREADY_AT_FULL_HEALTH",       // 0x01
    "SPELL_FAILED_ALREADY_AT_FULL_POWER",        // 0x02
    "SPELL_FAILED_ALREADY_BEING_TAMED",          // 0x03
    "SPELL_FAILED_ALREADY_HAVE_CHARM",           // 0x04
    "SPELL_FAILED_ALREADY_HAVE_SUMMON",          // 0x05
    "SPELL_FAILED_ALREADY_OPEN",                 // 0x06
    "SPELL_FAILED_AURA_BOUNCED",                 // 0x07
    "SPELL_FAILED_AUTOTRACK_INTERRUPTED",        // 0x08
    "SPELL_FAILED_BAD_IMPLICIT_TARGETS",         // 0x09
    "SPELL_FAILED_BAD_TARGETS",                  // 0x0a
    "SPELL_FAILED_CANT_BE_CHARMED",              // 0x0b
    "SPELL_FAILED_CANT_BE_DISENCHANTED",         // 0x0c
    "SPELL_FAILED_CANT_BE_PROSPECTED",           // 0x0d
    "SPELL_FAILED_CANT_CAST_ON_TAPPED",          // 0x0e
    "SPELL_FAILED_CANT_DUEL_WHILE_INVISIBLE",    // 0x0f
    "SPELL_FAILED_CANT_DUEL_WHILE_STEALTHED",    // 0x10
    "SPELL_FAILED_CANT_STEALTH",                 // 0x11
    "SPELL_FAILED_CASTER_AURASTATE",             // 0x12
    "SPELL_FAILED_CASTER_DEAD",                  // 0x13
    "SPELL_FAILED_CHARMED",                      // 0x14
    "SPELL_FAILED_CHEST_IN_USE",                 // 0x15
    "SPELL_FAILED_CONFUSED",                     // 0x16
    "SPELL_FAILED_DONT_REPORT",                  // 0x17
    "SPELL_FAILED_EQUIPPED_ITEM",                // 0x18
    "SPELL_FAILED_EQUIPPED_ITEM_CLASS",          // 0x19
    "SPELL_FAILED_EQUIPPED_ITEM_CLASS_MAINHAND", // 0x1a
    "SPELL_FAILED_EQUIPPED_ITEM_CLASS_OFFHAND",  // 0x1b
    "SPELL_FAILED_ERROR",                        // 0x1c
    "SPELL_FAILED_FIZZLE",                       // 0x1d
    "SPELL_FAILED_FLEEING",                      // 0x1e
    "SPELL_FAILED_FOOD_LOWLEVEL",                // 0x1f
    "SPELL_FAILED_HIGHLEVEL",                    // 0x20
    "SPELL_FAILED_HUNGER_SATIATED",              // 0x21
    "SPELL_FAILED_IMMUNE",                       // 0x22
    "SPELL_FAILED_INTERRUPTED",                  // 0x23
    "SPELL_FAILED_INTERRUPTED_COMBAT",           // 0x24
    "SPELL_FAILED_ITEM_ALREADY_ENCHANTED",       // 0x25
    "SPELL_FAILED_ITEM_GONE",                    // 0x26
    "SPELL_FAILED_ITEM_NOT_FOUND",               // 0x27
    "SPELL_FAILED_ITEM_NOT_READY",               // 0x28
    "SPELL_FAILED_LEVEL_REQUIREMENT",            // 0x29
    "SPELL_FAILED_LINE_OF_SIGHT",                // 0x2a
    "SPELL_FAILED_LOWLEVEL",                     // 0x2b
    "SPELL_FAILED_LOW_CASTLEVEL",                // 0x2c
    "SPELL_FAILED_MAINHAND_EMPTY",               // 0x2d
    "SPELL_FAILED_MOVING",                       // 0x2e
    "SPELL_FAILED_NEED_AMMO",                    // 0x2f
    "SPELL_FAILED_NEED_AMMO_POUCH",              // 0x30
    "SPELL_FAILED_NEED_EXOTIC_AMMO",             // 0x31
    "SPELL_FAILED_NOPATH",                       // 0x32
    "SPELL_FAILED_NOT_BEHIND",                   // 0x33
    "SPELL_FAILED_NOT_FISHABLE",                 // 0x34
    "SPELL_FAILED_NOT_HERE",                     // 0x35
    "SPELL_FAILED_NOT_INFRONT",                  // 0x36
    "SPELL_FAILED_NOT_IN_CONTROL",               // 0x37
    "SPELL_FAILED_NOT_KNOWN",                    // 0x38
    "SPELL_FAILED_NOT_MOUNTED",                  // 0x39
    "SPELL_FAILED_NOT_ON_TAXI",                  // 0x3a
    "SPELL_FAILED_NOT_ON_TRANSPORT",             // 0x3b
    "SPELL_FAILED_NOT_READY",                    // 0x3c
    "SPELL_FAILED_NOT_SHAPESHIFT",               // 0x3d
    "SPELL_FAILED_NOT_STANDING",                 // 0x3e
    "SPELL_FAILED_NOT_TRADEABLE",                // 0x3f
    "SPELL_FAILED_NOT_TRADING",                  // 0x40
    "SPELL_FAILED_NOT_UNSHEATHED",               // 0x41
    "SPELL_FAILED_NOT_WHILE_GHOST",              // 0x42
    "SPELL_FAILED_NO_AMMO",                      // 0x43
    "SPELL_FAILED_NO_CHARGES_REMAIN",            // 0x44
    "SPELL_FAILED_NO_CHAMPION",                  // 0x45
    "SPELL_FAILED_NO_COMBO_POINTS",              // 0x46
    "SPELL_FAILED_NO_DUELING",                   // 0x47
    "SPELL_FAILED_NO_ENDURANCE",                 // 0x48
    "SPELL_FAILED_NO_FISH",                      // 0x49
    "SPELL_FAILED_NO_ITEMS_WHILE_SHAPESHIFTED",  // 0x4a
    "SPELL_FAILED_NO_MOUNTS_ALLOWED",            // 0x4b
    "SPELL_FAILED_NO_PET",                       // 0x4c
    "SPELL_FAILED_NO_POWER",                     // 0x4d
    "SPELL_FAILED_NOTHING_TO_DISPEL",            // 0x4e
    "SPELL_FAILED_NOTHING_TO_STEAL",             // 0x4f
    "SPELL_FAILED_ONLY_ABOVEWATER",              // 0x50
    "SPELL_FAILED_ONLY_DAYTIME",                 // 0x51
    "SPELL_FAILED_ONLY_INDOORS",                 // 0x52
    "SPELL_FAILED_ONLY_MOUNTED",                 // 0x53
    "SPELL_FAILED_ONLY_NIGHTTIME",               // 0x54
    "SPELL_FAILED_ONLY_OUTDOORS",                // 0x55
    "SPELL_FAILED_ONLY_SHAPESHIFT",              // 0x56
    "SPELL_FAILED_ONLY_STEALTHED",               // 0x57
    "SPELL_FAILED_ONLY_UNDERWATER",              // 0x58
    "SPELL_FAILED_OUT_OF_RANGE",                 // 0x59
    "SPELL_FAILED_PACIFIED",                     // 0x5a
    "SPELL_FAILED_POSSESSED",                    // 0x5b
    "SPELL_FAILED_REAGENTS",                     // 0x5c
    "SPELL_FAILED_REQUIRES_AREA",                // 0x5d
    "SPELL_FAILED_REQUIRES_SPELL_FOCUS",         // 0x5e
    "SPELL_FAILED_ROOTED",                       // 0x5f
    "SPELL_FAILED_SILENCED",                     // 0x60
    "SPELL_FAILED_SPELL_IN_PROGRESS",            // 0x61
    "SPELL_FAILED_SPELL_LEARNED",                // 0x62
    "SPELL_FAILED_SPELL_UNAVAILABLE",            // 0x63
    "SPELL_FAILED_STUNNED",                      // 0x64
    "SPELL_FAILED_TARGETS_DEAD",                 // 0x65
    "SPELL_FAILED_TARGET_AFFECTING_COMBAT",      // 0x66
    "SPELL_FAILED_TARGET_AURASTATE",             // 0x67
    "SPELL_FAILED_TARGET_DUELING",               // 0x68
    "SPELL_FAILED_TARGET_ENEMY",                 // 0x69
    "SPELL_FAILED_TARGET_ENRAGED",               // 0x6a
    "SPELL_FAILED_TARGET_FRIENDLY",              // 0x6b
    "SPELL_FAILED_TARGET_IN_COMBAT",             // 0x6c
    "SPELL_FAILED_TARGET_IS_PLAYER",             // 0x6d
    "SPELL_FAILED_TARGET_NOT_DEAD",              // 0x6e
    "SPELL_FAILED_TARGET_NOT_IN_PARTY",          // 0x6f
    "SPELL_FAILED_TARGET_NOT_LOOTED",            // 0x70
    "SPELL_FAILED_TARGET_NOT_PLAYER",            // 0x71
    "SPELL_FAILED_TARGET_NO_POCKETS",            // 0x72
    "SPELL_FAILED_TARGET_NO_WEAPONS",            // 0x73
    "SPELL_FAILED_TARGET_UNSKINNABLE",           // 0x74
    "SPELL_FAILED_THIRST_SATIATED",              // 0x75
    "SPELL_FAILED_TOO_CLOSE",                    // 0x76
    "SPELL_FAILED_TOO_MANY_OF_ITEM",             // 0x77
    "SPELL_FAILED_TOTEMS",                       // 0x78
    "SPELL_FAILED_TRAINING_POINTS",              // 0x79
    "SPELL_FAILED_TRY_AGAIN",                    // 0x7a
    "SPELL_FAILED_UNIT_NOT_BEHIND",              // 0x7b
    "SPELL_FAILED_UNIT_NOT_INFRONT",             // 0x7c
    "SPELL_FAILED_WRONG_PET_FOOD",               // 0x7d
    "SPELL_FAILED_NOT_WHILE_FATIGUED",           // 0x7e
    "SPELL_FAILED_TARGET_NOT_IN_INSTANCE",       // 0x7f
    "SPELL_FAILED_NOT_WHILE_TRADING",            // 0x80
    "SPELL_FAILED_TARGET_NOT_IN_RAID",           // 0x81
    "SPELL_FAILED_DISENCHANT_WHILE_LOOTING",     // 0x82
    "SPELL_FAILED_PROSPECT_WHILE_LOOTING",       // 0x83
    "SPELL_FAILED_PROSPECT_NEED_MORE",           // 0x84
    "SPELL_FAILED_TARGET_FREEFORALL",            // 0x85
    "SPELL_FAILED_NO_EDIBLE_CORPSES",            // 0x86
    "SPELL_FAILED_ONLY_BATTLEGROUNDS",           // 0x87
    "SPELL_FAILED_TARGET_NOT_GHOST",             // 0x88
    "SPELL_FAILED_TOO_MANY_SKILLS",              // 0x89
    "SPELL_FAILED_TRANSFORM_UNUSABLE",           // 0x8a
    "SPELL_FAILED_WRONG_WEATHER",                // 0x8b
    "SPELL_FAILED_DAMAGE_IMMUNE",                // 0x8c
    "SPELL_FAILED_PREVENTED_BY_MECHANIC",        // 0x8d
    "SPELL_FAILED_PLAY_TIME",                    // 0x8e
    "SPELL_FAILED_REPUTATION",                   // 0x8f
    "SPELL_FAILED_MIN_SKILL",                    // 0x90
    "SPELL_FAILED_UNKNOWN",                      // 0x91
];

/// Vanilla power types (`SpellRec+0x7c`): the NO_POWER pick table `0x8118dc` and the
/// full-power `%s` fill. Health is the wire's -2.
fn power_keys(power_type: u32) -> (&'static str, &'static str) {
    match power_type {
        1 => ("ERR_OUT_OF_RAGE", "RAGE"),
        2 => ("ERR_OUT_OF_FOCUS", "FOCUS"),
        3 => ("ERR_OUT_OF_ENERGY", "ENERGY"),
        4 => ("ERR_NOT_HAPPY_ENOUGH", "HAPPINESS"),
        0xFFFFFFFE => ("ERR_OUT_OF_HEALTH", "HEALTH"),
        _ => ("ERR_OUT_OF_MANA", "MANA"),
    }
}

/// The potion/food category test (`SpellRec+0x8`) the 0x28/0x3c errorId picks key on.
fn is_potion(spell: Option<&SpellDisplay>) -> bool {
    spell.is_some_and(|d| matches!(d.category, 4 | 9))
}
fn is_food(spell: Option<&SpellDisplay>) -> bool {
    spell.is_some_and(|d| matches!(d.category, 0xA | 0xB))
}

/// The argument arms' inputs: the wire's reason-specific word ([`super::CastFail::arg`]) and the
/// DBC name tables the arms read. Both catalogs are `Option` because a client without game data
/// has neither — the arm then declines and the template strips, exactly as an unmodeled arm does.
#[derive(Default, Clone, Copy)]
pub(super) struct FailArgs<'a> {
    pub(super) arg: Option<u32>,
    /// `SpellFocusObject.dbc` (`0xc0d800`) — the `0x5e` arm's names ("Anvil", "Forge", and the
    /// Teldrassil moonwells the Crown of the Earth phials name).
    pub(super) focus: Option<&'a SpellFocusCatalog>,
    /// `AreaTable.dbc` (`0xc0e048`) — the `0x5d` arm's `AreaName`.
    pub(super) areas: Option<&'a AreaTableCatalog>,
}

impl FailArgs<'_> {
    /// The `%s` fill for the argument-formatted reasons this module owns, or `None` to leave the
    /// template to the strip fallback.
    ///
    /// Both arms are byte-verified and §5 cross-checked (wow-re `cast-fail-strings.md`
    /// §WIRE-ARGS): each reads the **wire's** first argument word — `[ebx+8]`, the handler's own
    /// stack slot, never a re-read of `Spell.dbc` — indexes its DBC store, and `SStrPrintf`s the
    /// reason's `SPELL_FAILED_*` template. The errorId stays the default `0x2c` (`"%s"`) for both,
    /// so what the player reads IS the filled template. A single-`%s` template is what makes a
    /// plain `replace` faithful here; the reference runs a real two-pass printf, which would
    /// matter for a multi-specifier format.
    ///
    /// **`0x5e` REQUIRES_SPELL_FOCUS** (`0x6e1f62`) → `SpellFocusObject.dbc` (`0xc0d800`)
    /// `Name_Lang` at `row + 0x4 + locale*4`, so `"Requires %s"` reads "Requires Starbreeze
    /// Village Moonwell". The failing spell's own `RequiresSpellFocus` column holds the same
    /// number vmangos copied onto the wire, so we fall back to it when the word is absent — a
    /// benilla-side robustness margin, not a transcription: the client itself never reads
    /// `SpellRec+0x3c` on this path.
    ///
    /// **`0x5d` REQUIRES_AREA** (`0x6e1fad`) → `AreaTable.dbc` (`0xc0e048`) `AreaName_Lang` at
    /// `row + 0x2c + locale*4`, so `"You need to be in %s"` names the zone. This one has **no**
    /// client-side stand-in: the server derives the id from its own `spell_area` rows and nothing
    /// in `Spell.dbc` holds it, so an absent word means an unfilled message.
    fn fill(&self, reason: u8, spell: Option<&SpellDisplay>) -> Option<String> {
        match reason {
            0x5D => self.areas?.name(self.arg?).map(str::to_string),
            0x5E => {
                let id = self
                    .arg
                    .filter(|&id| id != 0)
                    .or_else(|| Some(spell?.requires_spell_focus).filter(|&id| id != 0))?;
                self.focus?.name(id).map(str::to_string)
            }
            _ => None,
        }
    }
}

/// The displayed text for a failed cast — `None` = the reference shows nothing. `get` is the
/// VM's GlobalStrings lookup (an absent or empty key resolves to `None`, the data-suppression
/// face). Reasons beyond the table print their code — our debug affordance, not a ref string.
pub(super) fn cast_fail_text(
    reason: u8,
    spell: Option<&SpellDisplay>,
    args: FailArgs<'_>,
    get: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let get_display = |key: &str| get(key).filter(|s| !s.is_empty());
    // The errorId overrides (`0x6e1aab–0x6e1c5f`): the replaced-message reasons.
    match reason {
        0x01 => return get_display("ERR_SPELL_FAILED_ALREADY_AT_FULL_HEALTH"),
        0x02 => {
            let t = get_display("ERR_SPELL_FAILED_ALREADY_AT_FULL_POWER_S")?;
            let power = spell.map_or(0, |d| d.power_type);
            let name = get_display(power_keys(power).1).unwrap_or_default();
            return Some(t.replace("%s", &name));
        }
        0x09 => return get_display("ERR_GENERIC_NO_TARGET"),
        0x17 => return None, // DONT_REPORT: control-flow hidden (jumps past DisplayError)
        0x18 => return get_display("ERR_SPELL_FAILED_EQUIPPED_ITEM"),
        0x28 => {
            return get_display(if is_potion(spell) {
                "ERR_POTION_COOLDOWN"
            } else {
                "ERR_ITEM_COOLDOWN"
            });
        }
        0x3C => {
            let key = if is_food(spell) {
                "ERR_FOOD_COOLDOWN"
            } else if is_potion(spell) {
                "ERR_POTION_COOLDOWN"
            } else if spell.is_some_and(|d| d.attributes & 0x10 != 0) {
                "ERR_ABILITY_COOLDOWN"
            } else {
                "ERR_SPELL_COOLDOWN"
            };
            return get_display(key);
        }
        0x41 => return get_display("ERR_SPELL_FAILED_NOTUNSHEATHED"),
        0x4D => {
            let power = spell.map_or(0, |d| d.power_type);
            return get_display(power_keys(power).0);
        }
        0x59 => return get_display("ERR_SPELL_OUT_OF_RANGE"),
        0x8E => return get_display("ERR_PLAY_TIME_EXCEEDED"),
        _ => {}
    }
    // The passthrough layer: errorId 0x2c ("%s") displays GetText(SPELL_FAILED_<name>) as-is.
    let Some(key) = CAST_FAIL_KEYS.get(usize::from(reason)) else {
        return Some(format!("Spell failed ({reason:#04x})"));
    };
    let text = get_display(key)?;
    // The argument arms (`0x6e1d8e`): fill the template's `%s` from the reason's own DBC.
    if let Some(name) = args.fill(reason, spell) {
        return Some(text.replace("%s", &name));
    }
    // An arm we don't model (or one whose lookup missed) — strip the tokens so the stem reads
    // clean ("Missing reagent: %s" → "Missing reagent"), never a raw % on screen.
    if text.contains('%') {
        let stripped = text
            .replace("%s", "")
            .replace("%d", "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        return Some(
            stripped
                .trim_end_matches([' ', ':', '.', '(', ')'])
                .to_string(),
        );
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// The shipped 1.12 GlobalStrings entries the tests rest on (extracted values).
    fn gs() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("SPELL_FAILED_NO_AMMO", "Out of ammo"),
            ("SPELL_FAILED_OUT_OF_RANGE", "Out of range"),
            ("SPELL_FAILED_TOO_CLOSE", "Target too close"),
            ("SPELL_FAILED_NOT_READY", "Not yet recovered"),
            ("SPELL_FAILED_REAGENTS", "Missing reagent: %s"),
            ("ERR_SPELL_OUT_OF_RANGE", "Out of range."),
            ("ERR_GENERIC_NO_TARGET", "You have no target."),
            ("ERR_SPELL_COOLDOWN", "Spell is not ready yet."),
            ("ERR_ABILITY_COOLDOWN", "Ability is not ready yet."),
            ("ERR_POTION_COOLDOWN", "Item is not ready yet."),
            ("ERR_OUT_OF_MANA", "Not enough mana"),
            ("ERR_OUT_OF_RAGE", "Not enough rage"),
        ])
    }

    fn getter<'a>(
        map: &'a HashMap<&'static str, &'static str>,
    ) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| map.get(k).map(|s| (*s).to_string())
    }

    fn spell(power_type: u32, category: u32, attributes: u32) -> SpellDisplay {
        SpellDisplay {
            power_type,
            category,
            attributes,
            ..Default::default()
        }
    }

    /// The table's byte-verified anchors (wow-re cast-fail-strings.md).
    #[test]
    fn the_key_table_holds_the_verified_anchors() {
        assert_eq!(CAST_FAIL_KEYS.len(), 146);
        assert_eq!(CAST_FAIL_KEYS[0x43], "SPELL_FAILED_NO_AMMO");
        assert_eq!(CAST_FAIL_KEYS[0x59], "SPELL_FAILED_OUT_OF_RANGE");
        assert_eq!(CAST_FAIL_KEYS[0x76], "SPELL_FAILED_TOO_CLOSE");
    }

    /// Passthrough reads the SPELL_FAILED string; the errorId overrides REPLACE it: 0x59
    /// shows the perioded ERR string, 0x3c the cooldown family (never "Not yet recovered"),
    /// 0x09 the generic no-target line.
    #[test]
    fn overrides_replace_and_passthrough_reads() {
        let m = gs();
        let g = getter(&m);
        assert_eq!(
            cast_fail_text(0x43, None, FailArgs::default(), &g).unwrap(),
            "Out of ammo"
        );
        assert_eq!(
            cast_fail_text(0x59, None, FailArgs::default(), &g).unwrap(),
            "Out of range."
        );
        assert_eq!(
            cast_fail_text(0x76, None, FailArgs::default(), &g).unwrap(),
            "Target too close"
        );
        assert_eq!(
            cast_fail_text(0x09, None, FailArgs::default(), &g).unwrap(),
            "You have no target."
        );
        // 0x3c: plain spell → spell cooldown; Attr&0x10 → ability; potion category → potion.
        let plain = spell(0, 0, 0);
        assert_eq!(
            cast_fail_text(0x3C, Some(&plain), FailArgs::default(), &g).unwrap(),
            "Spell is not ready yet."
        );
        let ability = spell(1, 0, 0x10);
        assert_eq!(
            cast_fail_text(0x3C, Some(&ability), FailArgs::default(), &g).unwrap(),
            "Ability is not ready yet."
        );
        let potion = spell(0, 4, 0);
        assert_eq!(
            cast_fail_text(0x3C, Some(&potion), FailArgs::default(), &g).unwrap(),
            "Item is not ready yet."
        );
    }

    /// NO_POWER picks the power family off the SPELL's power type — the warrior's rage
    /// ability reads "Not enough rage", never the generic power line.
    #[test]
    fn no_power_reads_the_spells_power_family() {
        let m = gs();
        let g = getter(&m);
        let rage = spell(1, 0, 0);
        assert_eq!(
            cast_fail_text(0x4D, Some(&rage), FailArgs::default(), &g).unwrap(),
            "Not enough rage"
        );
        let mana = spell(0, 0, 0);
        assert_eq!(
            cast_fail_text(0x4D, Some(&mana), FailArgs::default(), &g).unwrap(),
            "Not enough mana"
        );
    }

    /// Both suppression faces: 0x17 control-flow hidden, 0x08 data-hidden (key absent from
    /// GlobalStrings); an off-table reason keeps the debug hex; an unfilled %s template
    /// strips to its stem.
    #[test]
    fn suppression_hex_fallback_and_template_strip() {
        let m = gs();
        let g = getter(&m);
        assert_eq!(cast_fail_text(0x17, None, FailArgs::default(), &g), None);
        assert_eq!(cast_fail_text(0x08, None, FailArgs::default(), &g), None);
        assert_eq!(
            cast_fail_text(0x92, None, FailArgs::default(), &g).unwrap(),
            "Spell failed (0x92)"
        );
        assert_eq!(
            cast_fail_text(0x5C, None, FailArgs::default(), &g).unwrap(),
            "Missing reagent"
        );
    }

    /// The RUNTIME leg, end to end on the real data: the shipped `GlobalStrings.lua` executed
    /// into a real VM (the boot's `load_global_strings` path), then the drain's exact lookup —
    /// the leg whose absence shipped a fold where every red line silently vanished (the VM had
    /// no GlobalStrings at all; the fake-getter tests above couldn't see it). Skips without
    /// client data.
    #[test]
    fn the_real_boot_resolves_the_real_strings() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let src = chain
            .read_file("Interface\\FrameXML\\GlobalStrings.lua")
            .expect("GlobalStrings.lua in the chain");
        let s = benilla_ui::script::UiScript::new().expect("VM");
        s.run(&String::from_utf8_lossy(&src)).expect("runs clean");
        let g = |key: &str| s.lua().globals().get::<String>(key).ok();

        assert_eq!(
            cast_fail_text(0x43, None, FailArgs::default(), &g).unwrap(),
            "Out of ammo"
        );
        assert_eq!(
            cast_fail_text(0x59, None, FailArgs::default(), &g).unwrap(),
            "Out of range."
        );
        assert_eq!(
            cast_fail_text(0x3C, None, FailArgs::default(), &g).unwrap(),
            "Spell is not ready yet."
        );
        let rage = spell(1, 0, 0);
        assert_eq!(
            cast_fail_text(0x4D, Some(&rage), FailArgs::default(), &g).unwrap(),
            "Not enough rage"
        );
        // The environment gate's pair (decision 1056) — both are plain passthroughs, so what the
        // player reads IS the GlobalStrings value. A typo'd key here would degrade a real refusal
        // to a dead-looking button, which is what this test exists to catch.
        assert_eq!(
            cast_fail_text(0x50, None, FailArgs::default(), &g).unwrap(),
            "Cannot use while swimming"
        );
        assert_eq!(
            cast_fail_text(0x58, None, FailArgs::default(), &g).unwrap(),
            "Can only use while swimming"
        );
        // The data-suppression face on the real file: the absent keys show nothing.
        assert_eq!(cast_fail_text(0x08, None, FailArgs::default(), &g), None);
        assert_eq!(cast_fail_text(0x21, None, FailArgs::default(), &g), None);

        // B255, end to end on the real data: the argument arms against the real DBCs and the real
        // GlobalStrings templates. Without the fill these read as the bare stems "Requires" and
        // "You need to be in" — which is exactly what shipped.
        let focus =
            benilla_formats::load_spell_focus_catalog(&mut chain).expect("SpellFocusObject");
        let areas = benilla_formats::load_area_table_catalog(&mut chain).expect("AreaTable");
        let args = |arg: u32| FailArgs {
            arg: Some(arg),
            focus: Some(&focus),
            areas: Some(&areas),
        };
        // 0x5e REQUIRES_SPELL_FOCUS: the Crown of the Earth phials' own refusal. Focus 12 is the
        // Starbreeze Village moonwell — using the Jade Phial at any *other* pool is the report.
        assert_eq!(
            cast_fail_text(0x5E, None, args(12), &g).unwrap(),
            "Requires Starbreeze Village Moonwell"
        );
        assert_eq!(
            cast_fail_text(0x5E, None, args(1), &g).unwrap(),
            "Requires Anvil"
        );
        // 0x5d REQUIRES_AREA: area 1657 is Darnassus.
        assert_eq!(
            cast_fail_text(0x5D, None, args(1657), &g).unwrap(),
            "You need to be in Darnassus"
        );
        // The fallbacks. An id the DBC doesn't name, and a wire word the server never sent, both
        // decline the arm and fall through to the strip — never a raw `%s` on screen.
        assert_eq!(
            cast_fail_text(0x5E, None, args(999_999), &g).unwrap(),
            "Requires"
        );
        assert_eq!(
            cast_fail_text(0x5D, None, FailArgs::default(), &g).unwrap(),
            "You need to be in"
        );
        // 0x5e alone has a client-side stand-in: the failing spell's own `RequiresSpellFocus`
        // column is the very number the server copied onto the wire, so an absent word still
        // fills. (Spell 4976 "Filling" — the Crystal Phial's — carries focus 11.)
        let filling = SpellDisplay {
            requires_spell_focus: 11,
            ..Default::default()
        };
        assert_eq!(
            cast_fail_text(
                0x5E,
                Some(&filling),
                FailArgs {
                    focus: Some(&focus),
                    ..FailArgs::default()
                },
                &g
            )
            .unwrap(),
            "Requires Shadowglen Moonwell"
        );
    }
}
