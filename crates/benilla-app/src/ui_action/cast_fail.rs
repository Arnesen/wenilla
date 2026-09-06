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
//!    **This layer — and only this layer — is per-CASTER** (decision 2033). `SMSG_PET_CAST_FAILED`
//!    is not the same handler with a flag: it is `Spell_C::HandlePetCastFailed 0x6e8eb0`, a
//!    separate switch over a separate 142-byte index table, and it overrides a different ten
//!    reasons. Six of them are the `ERR_PET_SPELL_*` catalog rows, which have no other raise site
//!    in the client — "Your pet is out of range." where the player reads "Out of range." —
//!    and seven of the player's overrides are simply absent there. [`Caster`] picks the table;
//!    layers 1 and 3 are shared code in the binary and shared code here.
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
//! `0x5d` REQUIRES_AREA here (decision 1313 — `0x6e1f62`/`0x6e1fad`), plus `0x8d`
//! PREVENTED_BY_MECHANIC (decision 1948 — `0x6e2190`, and the one arm whose word is produced
//! locally rather than read off the wire), and `0x78` TOTEMS / `0x5c`
//! REAGENTS / `0x19`–`0x1b` EQUIPPED_ITEM_CLASS\* in the drain, which owns them because their
//! fills need the item caches and the query-then-redisplay cache-miss behavior ("Requires Mining
//! Pick", decisions 0545 + 0552). Still stripped rather than filled — each needs a DBC we do not
//! load yet: `0x56` ONLY_SHAPESHIFT (form-name list), `0x90` MIN_SKILL (`SkillLine.dbc`),
//! `0x31` NEED_EXOTIC_AMMO and `0x84`
//! PROSPECT_NEED_MORE. Stripping is a **deliberate divergence**, now byte-confirmed as one: on a
//! bad id or an absent word the reference jumps to the default arm with the pointer still on the
//! *unfilled* template, so it displays a literal `Requires %s` (wow-re §WIRE-ARGS C3 — the fill
//! path's own buffer swap sits after the printf and is skipped). We show the bare stem instead:
//! "Requires" reads as terse, "Requires %s" reads as broken software (§7 — judge by the result).
//! `0x0a`'s item-spell leg (`ERR_INVALID_ITEM_TARGET`) is unmodeled — the drain does not know
//! item-ness.

use benilla_formats::{AreaTableCatalog, SpellDisplay, SpellFocusCatalog, SpellMechanicCatalog};

use super::Caster;

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
    /// `SpellMechanic.dbc` (`0xc0d7c4`) — the `0x8d` arm's mechanic name (decision 1948).
    pub(super) mechanics: Option<&'a SpellMechanicCatalog>,
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
            // **`0x8d` PREVENTED_BY_MECHANIC** (`0x6e2190`) → `SpellMechanic.dbc` (`0xc0d7c4`),
            // so "Can't do that while %s" reads "Can't do that while stunned". Unlike its two
            // neighbours the word is not the wire's: this refusal is raised locally and the id
            // comes from the crowd-control ladder's own exemption scan (decisions 1941/1948).
            // A `0` or unknown id leaves the template to the strip fallback, which is also what
            // the arm does when the scan named no mechanic at all.
            0x8D => self.mechanics?.name(self.arg?).map(str::to_string),
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

/// The errorId every reason that overrides nothing falls through to — `0x2c`, whose text is the
/// bare `"%s"`, so what the player reads is the first layer's `SPELL_FAILED_*` string. benilla
/// short-circuits the identity substitution and displays that string directly; the id still
/// matters, because it is the id whose **record** decides the surface.
pub(super) const PASSTHROUGH: &str = "ERR_SPELL_FAILED_S";

/// One resolved cast-failure line: the text to show, and the **message id's key** that decides
/// where to show it.
///
/// The key rides along because the reference's second layer hands its errorId to
/// `CGGameUI::DisplayError`, and that function reads the record's kind — so the surface is a
/// property of the id this dispatch picked, not of "cast failures" as a category. Seventeen of
/// the overrides are red and `ERR_SPELL_FAILED_NOTUNSHEATHED` is **yellow**; before the catalog
/// (decision 1770) benilla painted all eighteen red, because nothing here knew which id it had
/// chosen by the time the line reached the frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CastFailLine {
    pub key: &'static str,
    pub text: String,
}

impl CastFailLine {
    /// A line the second layer left on its default errorId — the drain builds these for the three
    /// argument arms it owns (`0x78` TOTEMS, `0x5c` REAGENTS, `0x19`-`0x1b` EQUIPPED_ITEM_CLASS*),
    /// whose fills need the item caches.
    pub(super) fn passthrough(text: String) -> Self {
        Self {
            key: PASSTHROUGH,
            text,
        }
    }

    fn fill(self, name: &str) -> Self {
        Self {
            text: self.text.replace("%s", name),
            ..self
        }
    }
}

/// The displayed line for a failed cast — `None` = the reference shows nothing. `get` is the
/// VM's GlobalStrings lookup (an absent or empty key resolves to `None`, the data-suppression
/// face). Reasons beyond the table print their code — our debug affordance, not a ref string.
pub(super) fn cast_fail_text(
    caster: Caster,
    reason: u8,
    spell: Option<&SpellDisplay>,
    args: FailArgs<'_>,
    get: &dyn Fn(&str) -> Option<String>,
) -> Option<CastFailLine> {
    let get_text = |key: &str| get(key).filter(|s| !s.is_empty());
    let get_display = |key: &'static str| get_text(key).map(|text| CastFailLine { key, text });
    // The errorId overrides — the replaced-message reasons. **Which table is asked is the
    // caster's** (decision 2033): the reference does not flag one handler, it ships two, and they
    // disagree on ten reasons. Everything past this match — the `SPELL_FAILED_*` vocabulary, the
    // argument arms, the strip fallback — is shared, exactly as it is in the binary.
    match caster {
        // `Spell_C::HandlePetCastFailed 0x6e8eb0`: `cmp reason,0x8d; ja default`, then a 142-byte
        // index table (`0x6e93d0`) over 15 jump targets (`0x6e9394`). Six of the ten overrides are
        // the `ERR_PET_SPELL_*` catalog rows, which exist for this handler and nothing else —
        // "Your pet is dead." where the player reads "You are dead".
        Caster::Pet => match reason {
            0x00 => return get_display("ERR_PET_SPELL_AFFECTING_COMBAT"), // 0x14c @0x6e8fab
            0x13 => return get_display("ERR_PET_SPELL_DEAD"),             // 0x150 @0x6e9017
            0x32 => return get_display("ERR_PET_SPELL_NOPATH"),           // 0x151 @0x6e9032
            0x33 => return get_display("ERR_PET_SPELL_NOT_BEHIND"),       // 0x14e @0x6e8fe1
            0x59 => return get_display("ERR_PET_SPELL_OUT_OF_RANGE"),     // 0x14d @0x6e8fc6
            0x5F => return get_display("ERR_PET_SPELL_ROOTED"),           // 0x14b @0x6e8f4f
            0x65 => return get_display("ERR_PET_SPELL_TARGETS_DEAD"),     // 0x14f @0x6e8ffc
            // `0x6e8f2a` computes its id: `((SpellRec+0x18 & 0x10) | 0x300) >> 4`, which is only
            // ever `0x31` or `0x30`. The player's four-way (`0x6e1aab`) tests the spell CATEGORY
            // first for food and potion; the pet's does not test it at all, because a pet eats
            // and drinks nothing.
            0x3C => {
                return get_display(if spell.is_some_and(|d| d.attributes & 0x10 != 0) {
                    "ERR_ABILITY_COOLDOWN"
                } else {
                    "ERR_SPELL_COOLDOWN"
                });
            }
            // `0x6e8f6a`: health (`SpellRec+0x7c` == `-2`) takes `0x123`, everything else indexes
            // `[0x8118f0 + 4*power]`. Those five dwords are byte-identical to the player's table
            // at `0x8118dc` (`0x11f 0x120 0x121 0x122 0x168`), so this is the SAME pick, not a
            // pet-specific one — which is why [`power_keys`] serves both and a hunter pet's focus
            // ability reads "Not enough focus" either way.
            0x4D => {
                let power = spell.map_or(0, |d| d.power_type);
                return get_display(power_keys(power).0);
            }
            // What is deliberately absent is as load-bearing as what is here: the pet's index
            // table sends `0x01`/`0x02` (already at full health/power), `0x09` (no target),
            // `0x18` (equipped item), `0x28` (item cooldown), `0x41` (unsheathed) and `0x8e`
            // (play time) to the generic arm, so a pet never says any of those seven lines.
            //
            // `0x17` DONT_REPORT is the one that looks like a divergence and is not: the player's
            // handler hides it by control flow, the pet's routes it to the generic arm — but
            // `SPELL_FAILED_DONT_REPORT` has no string in 5875's `GlobalStrings.lua`, so the
            // passthrough below draws nothing and the two agree on screen.
            //
            // `0x56` ONLY_SHAPESHIFT has a real arm here (`0x6e921a`, errorId `0xd6`) that fills
            // the template with a `SpellShapeshiftForm.dbc` name and displays it through the bare
            // `"%s"` of `ERR_SPELL_FAILED_SHAPESHIFT_FORM_S` — the same net line the player's own
            // unmodeled arm would produce, and stripped the same way here for the same reason
            // (that DBC is not loaded). Same surface, same silence, no pet-specific work.
            _ => {}
        },
        // `Spell_C::HandleCastFailed 0x6e1a00` (`0x6e1aab`–`0x6e1c5f`).
        Caster::Player => match reason {
            0x01 => return get_display("ERR_SPELL_FAILED_ALREADY_AT_FULL_HEALTH"),
            0x02 => {
                let t = get_display("ERR_SPELL_FAILED_ALREADY_AT_FULL_POWER_S")?;
                let power = spell.map_or(0, |d| d.power_type);
                let name = get_text(power_keys(power).1).unwrap_or_default();
                return Some(t.fill(&name));
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
        },
    }
    // The passthrough layer: errorId 0x2c ("%s") displays GetText(SPELL_FAILED_<name>) as-is.
    let Some(key) = CAST_FAIL_KEYS.get(usize::from(reason)) else {
        return Some(CastFailLine {
            key: PASSTHROUGH,
            text: format!("Spell failed ({reason:#04x})"),
        });
    };
    let text = get_text(key)?;
    let line = |text: String| CastFailLine {
        key: PASSTHROUGH,
        text,
    };
    // The argument arms (`0x6e1d8e`): fill the template's `%s` from the reason's own DBC.
    if let Some(name) = args.fill(reason, spell) {
        return Some(line(text.replace("%s", &name)));
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
        return Some(line(
            stripped
                .trim_end_matches([' ', ':', '.', '(', ')'])
                .to_string(),
        ));
    }
    Some(line(text))
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
            (
                "ERR_SPELL_FAILED_NOTUNSHEATHED",
                "You have nothing to attack with.",
            ),
            // The pet's own six, verbatim from the shipped file — and NO `ERR_PET_SPELL_NOPATH`,
            // because 5875 ships none, which is a fact these tests rest on.
            ("ERR_PET_SPELL_AFFECTING_COMBAT", "Your pet is in combat."),
            ("ERR_PET_SPELL_DEAD", "Your pet is dead."),
            (
                "ERR_PET_SPELL_NOT_BEHIND",
                "Your pet must be behind its target.",
            ),
            ("ERR_PET_SPELL_OUT_OF_RANGE", "Your pet is out of range."),
            ("ERR_PET_SPELL_ROOTED", "Your pet is unable to move."),
            ("ERR_PET_SPELL_TARGETS_DEAD", "Your pet\'s target is dead."),
            ("SPELL_FAILED_AFFECTING_COMBAT", "You are in combat"),
            ("SPELL_FAILED_CASTER_DEAD", "You are dead"),
            ("SPELL_FAILED_NOPATH", "No path available"),
            ("SPELL_FAILED_NOT_BEHIND", "You must be behind your target"),
            ("SPELL_FAILED_ROOTED", "You are unable to move"),
            ("SPELL_FAILED_TARGETS_DEAD", "Your target is dead"),
            ("SPELL_FAILED_BAD_IMPLICIT_TARGETS", "No target"),
            ("SPELL_FAILED_NOT_UNSHEATHED", "You must be unsheathed"),
            ("ERR_OUT_OF_FOCUS", "Not enough focus"),
        ])
    }

    /// **The six lines that exist for the pet and nobody else.** Each is a row the client raises
    /// only from `Spell_C::HandlePetCastFailed 0x6e8eb0`, and each is checked against what the
    /// *player* gets for the same wire reason — because "our pet reads the same string we do" is
    /// exactly the shape of the defect decision 2033 corrects, and a one-sided assert would not
    /// have caught it.
    #[test]
    fn the_pet_speaks_its_own_six_refusals() {
        let m = gs();
        let g = getter(&m);
        let say = |caster, reason| {
            cast_fail_text(caster, reason, None, FailArgs::default(), &g).map(|l| l.text)
        };
        for (reason, pet, player) in [
            (0x00, "Your pet is in combat.", "You are in combat"),
            (0x13, "Your pet is dead.", "You are dead"),
            (
                0x33,
                "Your pet must be behind its target.",
                "You must be behind your target",
            ),
            (0x59, "Your pet is out of range.", "Out of range."),
            (
                0x5F,
                "Your pet is unable to move.",
                "You are unable to move",
            ),
            (0x65, "Your pet\'s target is dead.", "Your target is dead"),
        ] {
            assert_eq!(
                say(Caster::Pet, reason).as_deref(),
                Some(pet),
                "pet {reason:#04x}"
            );
            assert_eq!(
                say(Caster::Player, reason).as_deref(),
                Some(player),
                "player {reason:#04x}"
            );
        }
    }

    /// **NOPATH is silent for the pet and spoken for the player**, which reads like a bug and is
    /// the reference: the pet arm raises errorId `0x151`, whose key `ERR_PET_SPELL_NOPATH` has no
    /// string in 5875's `GlobalStrings.lua`, so `DisplayError` shows nothing. The player's `0x32`
    /// takes the passthrough and reads "No path available".
    #[test]
    fn the_pets_nopath_is_silent_because_5875_ships_no_string() {
        let m = gs();
        let g = getter(&m);
        assert_eq!(
            cast_fail_text(Caster::Pet, 0x32, None, FailArgs::default(), &g),
            None
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x32, None, FailArgs::default(), &g)
                .unwrap()
                .text,
            "No path available"
        );
    }

    /// The pet's `0x3c` computes its id from one bit — `((SpellRec+0x18 & 0x10) | 0x300) >> 4` —
    /// so it has only the ability/spell split. The player's tests the spell CATEGORY first and has
    /// a food and a potion leg. A pet eats nothing, so a potion-category spell that says "Item is
    /// not ready yet." for us says "Spell is not ready yet." for it.
    #[test]
    fn the_pet_has_no_food_or_potion_cooldown_leg() {
        let m = gs();
        let g = getter(&m);
        let potion = spell(0, 4, 0);
        let ability = spell(0, 0, 0x10);
        let text = |caster, d| {
            cast_fail_text(caster, 0x3C, Some(d), FailArgs::default(), &g)
                .unwrap()
                .text
        };
        assert_eq!(text(Caster::Player, &potion), "Item is not ready yet.");
        assert_eq!(text(Caster::Pet, &potion), "Spell is not ready yet.");
        // The one leg they share, so the collapse is a missing branch and not a missing table.
        assert_eq!(text(Caster::Player, &ability), "Ability is not ready yet.");
        assert_eq!(text(Caster::Pet, &ability), "Ability is not ready yet.");
    }

    /// The pet's index table sends seven of the player's overrides to its generic arm, so those
    /// lines are the player's alone. Checked on the two with the sharpest tell: `0x09`, where the
    /// text changes, and `0x41`, where the **surface** changes too — the player's override is the
    /// one yellow cast failure in the game, and the pet's passthrough is red.
    #[test]
    fn the_pet_does_not_take_the_players_overrides() {
        use benilla_ui::messages::{kind_of, MsgKind};
        let m = gs();
        let g = getter(&m);
        let line = |caster, reason| {
            cast_fail_text(caster, reason, None, FailArgs::default(), &g).expect("a line")
        };

        assert_eq!(line(Caster::Player, 0x09).text, "You have no target.");
        assert_eq!(line(Caster::Pet, 0x09).text, "No target");

        let mine = line(Caster::Player, 0x41);
        assert_eq!(mine.key, "ERR_SPELL_FAILED_NOTUNSHEATHED");
        assert_eq!(kind_of(mine.key), MsgKind::Info);
        let its = line(Caster::Pet, 0x41);
        assert_eq!(its.key, PASSTHROUGH);
        assert_eq!(its.text, "You must be unsheathed");
        assert_eq!(kind_of(its.key), MsgKind::Error);
    }

    /// `0x4d` NO_POWER is the override the two handlers **agree** on, and that is a byte fact
    /// rather than an assumption: the pet's table at `0x8118f0` holds the same five dwords as the
    /// player's at `0x8118dc`. So [`power_keys`] serves both, and a hunter pet's focus ability
    /// reads the focus line on either path.
    #[test]
    fn the_pets_no_power_pick_is_the_players() {
        let m = gs();
        let g = getter(&m);
        let focus = spell(2, 0, 0);
        for caster in [Caster::Player, Caster::Pet] {
            assert_eq!(
                cast_fail_text(caster, 0x4D, Some(&focus), FailArgs::default(), &g)
                    .unwrap()
                    .text,
                "Not enough focus"
            );
        }
    }

    /// `0x17` DONT_REPORT looks like a divergence between the two handlers and is not: the
    /// player's hides it by control flow, the pet's routes it to the generic arm — but
    /// `SPELL_FAILED_DONT_REPORT` has no string in 5875, so both draw nothing. Worth a test
    /// because the two mechanisms are different and only the *outcome* is shared, so a future
    /// change to either one should have to notice.
    #[test]
    fn dont_report_is_silent_on_both_paths_for_two_different_reasons() {
        let m = gs();
        let g = getter(&m);
        assert!(!m.contains_key("SPELL_FAILED_DONT_REPORT"));
        for caster in [Caster::Player, Caster::Pet] {
            assert_eq!(
                cast_fail_text(caster, 0x17, None, FailArgs::default(), &g),
                None
            );
        }
    }

    /// **The one cast failure that is not a red line.** Reason `0x41` overrides to
    /// `ERR_SPELL_FAILED_NOTUNSHEATHED` (id 320), and that record's kind is `1` — the yellow
    /// `UI_INFO_MESSAGE`, not the red `UI_ERROR_MESSAGE` its seventeen override neighbours take.
    ///
    /// benilla painted it red until decision 1770, and could not have done otherwise: the drain
    /// fired one event for every resolved cast-failure line, because by the time a line got there
    /// nothing knew which errorId this dispatch had chosen. Carrying the key out is what makes the
    /// surface answerable, and this test is the answer.
    ///
    /// Its two yellow siblings, `ERR_FISH_NOT_HOOKED` (318) and `ERR_FISH_ESCAPED` (319), sit one
    /// and two ids below it and were hand-traced correctly by an earlier round — which is exactly
    /// how a hand-swept surface goes wrong: two of the three neighbours seen, the third not.
    #[test]
    fn the_unsheathed_refusal_is_yellow_and_its_neighbours_are_red() {
        use benilla_ui::messages::{kind_of, MsgKind};
        let m = gs();
        let g = getter(&m);

        let line =
            cast_fail_text(Caster::Player, 0x41, None, FailArgs::default(), &g).expect("a line");
        assert_eq!(line.key, "ERR_SPELL_FAILED_NOTUNSHEATHED");
        assert_eq!(kind_of(line.key), MsgKind::Info);

        // An override that IS red, and a passthrough that falls to errorId 0x2c.
        let red =
            cast_fail_text(Caster::Player, 0x59, None, FailArgs::default(), &g).expect("a line");
        assert_eq!(red.key, "ERR_SPELL_OUT_OF_RANGE");
        assert_eq!(kind_of(red.key), MsgKind::Error);

        let through =
            cast_fail_text(Caster::Player, 0x43, None, FailArgs::default(), &g).expect("a line");
        assert_eq!(through.key, PASSTHROUGH);
        assert_eq!(kind_of(through.key), MsgKind::Error);
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
            cast_fail_text(Caster::Player, 0x43, None, FailArgs::default(), &g)
                .unwrap()
                .text,
            "Out of ammo"
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x59, None, FailArgs::default(), &g)
                .unwrap()
                .text,
            "Out of range."
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x76, None, FailArgs::default(), &g)
                .unwrap()
                .text,
            "Target too close"
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x09, None, FailArgs::default(), &g)
                .unwrap()
                .text,
            "You have no target."
        );
        // 0x3c: plain spell → spell cooldown; Attr&0x10 → ability; potion category → potion.
        let plain = spell(0, 0, 0);
        assert_eq!(
            cast_fail_text(Caster::Player, 0x3C, Some(&plain), FailArgs::default(), &g)
                .unwrap()
                .text,
            "Spell is not ready yet."
        );
        let ability = spell(1, 0, 0x10);
        assert_eq!(
            cast_fail_text(
                Caster::Player,
                0x3C,
                Some(&ability),
                FailArgs::default(),
                &g
            )
            .unwrap()
            .text,
            "Ability is not ready yet."
        );
        let potion = spell(0, 4, 0);
        assert_eq!(
            cast_fail_text(Caster::Player, 0x3C, Some(&potion), FailArgs::default(), &g)
                .unwrap()
                .text,
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
            cast_fail_text(Caster::Player, 0x4D, Some(&rage), FailArgs::default(), &g)
                .unwrap()
                .text,
            "Not enough rage"
        );
        let mana = spell(0, 0, 0);
        assert_eq!(
            cast_fail_text(Caster::Player, 0x4D, Some(&mana), FailArgs::default(), &g)
                .unwrap()
                .text,
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
        assert_eq!(
            cast_fail_text(Caster::Player, 0x17, None, FailArgs::default(), &g),
            None
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x08, None, FailArgs::default(), &g),
            None
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x92, None, FailArgs::default(), &g)
                .unwrap()
                .text,
            "Spell failed (0x92)"
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x5C, None, FailArgs::default(), &g)
                .unwrap()
                .text,
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
            cast_fail_text(Caster::Player, 0x43, None, FailArgs::default(), &g)
                .unwrap()
                .text,
            "Out of ammo"
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x59, None, FailArgs::default(), &g)
                .unwrap()
                .text,
            "Out of range."
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x3C, None, FailArgs::default(), &g)
                .unwrap()
                .text,
            "Spell is not ready yet."
        );
        let rage = spell(1, 0, 0);
        assert_eq!(
            cast_fail_text(Caster::Player, 0x4D, Some(&rage), FailArgs::default(), &g)
                .unwrap()
                .text,
            "Not enough rage"
        );
        // The environment gate's pair (decision 1056) — both are plain passthroughs, so what the
        // player reads IS the GlobalStrings value. A typo'd key here would degrade a real refusal
        // to a dead-looking button, which is what this test exists to catch.
        assert_eq!(
            cast_fail_text(Caster::Player, 0x50, None, FailArgs::default(), &g)
                .unwrap()
                .text,
            "Cannot use while swimming"
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x58, None, FailArgs::default(), &g)
                .unwrap()
                .text,
            "Can only use while swimming"
        );
        // The data-suppression face on the real file: the absent keys show nothing.
        assert_eq!(
            cast_fail_text(Caster::Player, 0x08, None, FailArgs::default(), &g),
            None
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x21, None, FailArgs::default(), &g),
            None
        );

        // The pet's table against the player's own shipped file (decision 2033). Both halves
        // matter: the six rows resolve to the pet wording, and `ERR_PET_SPELL_NOPATH` resolves to
        // NOTHING — the claim the `SILENT_IN_5875` entry rests on, checked here against the real
        // `GlobalStrings.lua` rather than against a fixture that could simply have omitted it.
        for (reason, pet, player) in [
            (0x00, "Your pet is in combat.", "You are in combat"),
            (0x13, "Your pet is dead.", "You are dead"),
            (
                0x33,
                "Your pet must be behind its target.",
                "You must be behind your target",
            ),
            (0x59, "Your pet is out of range.", "Out of range."),
            (
                0x5F,
                "Your pet is unable to move.",
                "You are unable to move",
            ),
            (0x65, "Your pet\'s target is dead.", "Your target is dead"),
        ] {
            assert_eq!(
                cast_fail_text(Caster::Pet, reason, None, FailArgs::default(), &g)
                    .map(|l| l.text)
                    .as_deref(),
                Some(pet),
                "pet {reason:#04x}"
            );
            assert_eq!(
                cast_fail_text(Caster::Player, reason, None, FailArgs::default(), &g)
                    .map(|l| l.text)
                    .as_deref(),
                Some(player),
                "player {reason:#04x}"
            );
        }
        assert_eq!(
            cast_fail_text(Caster::Pet, 0x32, None, FailArgs::default(), &g),
            None,
            "5875 ships no ERR_PET_SPELL_NOPATH, so the reference shows nothing"
        );
        assert!(
            g("PET_SPELL_NOPATH").is_some(),
            "and the near-miss key that made the old map look right IS shipped — which is the \
             whole trap"
        );

        // B255, end to end on the real data: the argument arms against the real DBCs and the real
        // GlobalStrings templates. Without the fill these read as the bare stems "Requires" and
        // "You need to be in" — which is exactly what shipped.
        let focus =
            benilla_formats::load_spell_focus_catalog(&mut chain).expect("SpellFocusObject");
        let areas = benilla_formats::load_area_table_catalog(&mut chain).expect("AreaTable");
        let mechanics =
            benilla_formats::load_spell_mechanic_catalog(&mut chain).expect("SpellMechanic");
        let args = |arg: u32| FailArgs {
            arg: Some(arg),
            focus: Some(&focus),
            areas: Some(&areas),
            mechanics: Some(&mechanics),
        };
        // **0x8d PREVENTED_BY_MECHANIC** (decision 1948) — the crowd-control ladder's renamed
        // refusal, and the one argument arm whose word is produced locally rather than read off
        // the wire. The names are lower-case adjectives in the shipped data, which is what makes
        // them read as the tail of the sentence.
        assert_eq!(
            cast_fail_text(Caster::Player, 0x8D, None, args(12), &g)
                .unwrap()
                .text,
            "Can't do that while stunned"
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x8D, None, args(5), &g)
                .unwrap()
                .text,
            "Can't do that while fleeing"
        );
        // An unknown or absent mechanic strips to the bare stem rather than showing a raw `%s`.
        assert!(!cast_fail_text(Caster::Player, 0x8D, None, args(999), &g)
            .unwrap()
            .text
            .contains('%'));

        // 0x5e REQUIRES_SPELL_FOCUS: the Crown of the Earth phials' own refusal. Focus 12 is the
        // Starbreeze Village moonwell — using the Jade Phial at any *other* pool is the report.
        assert_eq!(
            cast_fail_text(Caster::Player, 0x5E, None, args(12), &g)
                .unwrap()
                .text,
            "Requires Starbreeze Village Moonwell"
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x5E, None, args(1), &g)
                .unwrap()
                .text,
            "Requires Anvil"
        );
        // 0x5d REQUIRES_AREA: area 1657 is Darnassus.
        assert_eq!(
            cast_fail_text(Caster::Player, 0x5D, None, args(1657), &g)
                .unwrap()
                .text,
            "You need to be in Darnassus"
        );
        // The fallbacks. An id the DBC doesn't name, and a wire word the server never sent, both
        // decline the arm and fall through to the strip — never a raw `%s` on screen.
        assert_eq!(
            cast_fail_text(Caster::Player, 0x5E, None, args(999_999), &g)
                .unwrap()
                .text,
            "Requires"
        );
        assert_eq!(
            cast_fail_text(Caster::Player, 0x5D, None, FailArgs::default(), &g)
                .unwrap()
                .text,
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
                Caster::Player,
                0x5E,
                Some(&filling),
                FailArgs {
                    focus: Some(&focus),
                    ..FailArgs::default()
                },
                &g
            )
            .unwrap()
            .text,
            "Requires Shadowglen Moonwell"
        );
    }
}
