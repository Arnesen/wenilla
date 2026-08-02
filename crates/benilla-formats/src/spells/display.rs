//! `SpellDisplay` — one spell's display + usable-walk surface (the column pins its fields
//! read are documented on `spells/mod.rs`'s module header, which this file's every intra-doc
//! link points back to). Split out of `mod.rs` purely for file size — still `crate::spells`'s
//! own type, not a separate concern.

use super::*;

/// A spell's `SPELL_EFFECT_OPEN_LOCK` effect (decision 0752) — which `LockType` it opens, and
/// which of the three effect slots carries it (so the value walk reads the right column of the
/// per-effect arrays).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenLock {
    /// `EffectMiscValue` — the `LockType.dbc` index this spell opens (pick-lock 1, herbalism 2,
    /// mining 3, "Quick Open" 10, "Open Kneeling" 13, blasting 16 …).
    pub lock_type: u32,
    /// Which of `Effect[0..3]` is the OPEN_LOCK one.
    pub effect: usize,
}

/// One spell's display identity.
pub struct SpellDisplay {
    pub name: String,
    /// `SpellNameSubtext` enUS (column 129, module docs) — "Rank N" for most leveled spells, a
    /// bare descriptor ("Racial", "Passive", …) for some, or absent for a handful of unranked
    /// spells. `None` when the column is empty ([`crate::dbc::str_at`]'s own empty-means-absent
    /// contract) — the spellbook slot then shows no second line at all.
    pub rank: Option<String>,
    /// The icon texture's MPQ path (`Interface\Icons\…`, extensionless as the DBC stores it);
    /// `None` when the spell's icon id is 0/unknown (render the fallback question mark).
    pub icon: Option<String>,
    /// `SpellVisual.dbc` id (column 115, module docs) — `0` = no visual chain (a silent cast).
    /// Resolve through [`crate::SpellVisualCatalog`] for the lifecycle-stage kits.
    pub visual: u32,
    /// The projectile travel speed, world units/sec (column 37, module docs). `0` = instant
    /// impact (decision 0099's `Speed==0` gate — no missile phase).
    pub speed: f32,
    /// `Attributes` (column 6, module docs) — consumed by [`Self::ranged_attack`] (`0x2`),
    /// [`Self::in_spellbook`] (`0x20`/`0x80`), [`Self::cooldown_on_event`] (bit 25), and the
    /// aura-bar display filter [`Self::hidden_from_aura_bar`] (`0x80`).
    pub attributes: u32,
    /// `AttributesEx` (column 7, `SpellRec+0x1c`) — only bit `0x10000000` is consumed
    /// ([`Self::hidden_from_aura_bar`]'s `SPELL_ATTR_EX_NO_AURA_ICON` half).
    pub attributes_ex: u32,
    /// `AttributesEx2` (column 8, module docs) — bit `0x20` is consumed by
    /// [`Self::ranged_attack`], bit 19 (`0x0008_0000`, allow-while-not-shapeshifted) by the form
    /// gate [`Self::form_refusal`]; the stance-bar admission bits (`0x2`/`0x10`) are read raw by
    /// `benilla::ui_shapeshift`.
    pub attributes_ex2: u32,
    /// `AttributesEx3` (column 9, `SpellRec+0x24`) — only bit `0x8000` is consumed
    /// ([`Self::melee_white_damage`]).
    pub attributes_ex3: u32,
    /// `Attributes & 0x40` (`SPELL_ATTR_PASSIVE`, module docs) — the spellbook's gray-and-refuse
    /// gate (consumed by `benilla-ui/src/script/spellbook.rs`, decision 0216 §8).
    pub passive: bool,
    /// `castUI` (column 3, `SpellRec+0xc`, module docs) — the third of the spellbook add-gate's
    /// three exclusions ([`Self::in_spellbook`]). Nonzero keeps the spell out of the book; reads
    /// 0 for every ordinary player spell.
    pub cast_ui: u32,
    /// `Effect[0]` (column 61, module docs) — the first effect type. Only
    /// [`Self::is_melee_auto_attack`] consumes it (`== SPELL_EFFECT_ATTACK`).
    pub effect_1: u32,
    /// This spell's `SPELL_EFFECT_OPEN_LOCK` effect, or `None` if it opens no lock. The GameObject
    /// interact-cast (decisions 0239/0752) matches it against a lock slot across the player's known
    /// spells: "Opening" for keyless chests, "Mining"/"Herb Gathering"/"Pick Lock" for skill locks.
    /// The skill it *provides* is [`Self::open_lock_skill`].
    pub open_lock: Option<OpenLock>,
    /// `spellLevel` (column 28, `SpellRec+0x70`) — the level this spell's effect values are quoted
    /// at; the effect-value walk scales by `casterLevel − spellLevel`, floored at 0 (`0x6e3854`).
    pub spell_level: u32,
    /// `Dispel` (column 4, `SpellRec+0x10`) — the `SpellDispelType.dbc` id. The reference client
    /// reads exactly this field for `GetPlayerBuffDispelType` / `UnitDebuff`'s third return
    /// (byte-verified, decision 0257). Name it through [`crate::SpellCatalog::dispel_name`] — the
    /// id alone does not say whether the class is named (`SpellDispelType.dbc`'s `[+0x28]` gate).
    pub dispel: u32,
    /// `Category` (column 2, [`COL_CATEGORY`]) — the shared-cooldown category; `0` = none.
    pub category: u32,
    /// `RecoveryTime` ms (column 19) — the spell's own cooldown.
    pub recovery_ms: u32,
    /// `InterruptFlags` ([`COL_INTERRUPT_FLAGS`]) — what breaks this spell's cast. Ordinary
    /// timed casts carry `0xf`; the movement bit (`0x1`, vmangos
    /// `SPELL_INTERRUPT_FLAG_MOVEMENT`) gates the cast bar's local self-cancel
    /// (`benilla::ui_cast`, where the client-side semantics are documented).
    pub interrupt_flags: u32,
    /// `AuraInterruptFlags` ([`COL_AURA_INTERRUPT_FLAGS`]) — what breaks this spell's applied
    /// aura (the food/drink "sit still" bits live here). The cast-initiation moving gate
    /// (`0x609de3`, wow-re `moving-cast-gate.md`; decision 0862) reads its `0x18`
    /// (MOVING|TURNING) bits as one of the three "would movement matter" arms. `0` for most
    /// direct casts.
    pub aura_interrupt_flags: u32,
    /// `ChannelInterruptFlags` ([`COL_CHANNEL_INTERRUPT_FLAGS`]) — what breaks this spell's
    /// running channel; the bits live in the *aura*-interrupt space (vmangos tests it with
    /// `AURA_INTERRUPT_MOVING_CANCELS`), not `InterruptFlags`'. `0` for non-channels.
    pub channel_interrupt_flags: u32,
    /// `CategoryRecoveryTime` ms (column 20) — the category's shared cooldown.
    pub category_recovery_ms: u32,
    /// `StartRecoveryCategory` (column 157) — the GCD category (133 for ordinary spells).
    pub start_recovery_category: u32,
    /// `StartRecoveryTime` ms (column 158) — the GCD duration (1500 for ordinary spells; 0 = no GCD).
    pub start_recovery_ms: u32,
    /// `powerType` (column 31) — 0 mana, 1 rage, 3 energy (vmangos `Powers`).
    pub power_type: u32,
    /// `manaCost` (column 32) — the flat cast cost in `power_type`'s unit.
    pub mana_cost: u32,
    /// `ManaCostPercentage` (column 156) — percent of base mana added to the flat cost (Judgement
    /// and kin); 0 for most spells.
    pub mana_cost_pct: u32,
    /// `rangeIndex` (column 36) — the `SpellRange.dbc` row ([`SpellRangeCatalog`]).
    pub range_index: u32,
    /// `Targets` (column 13, [`COL_TARGETS`]) — the `TARGET_FLAG_*` seed mask of the cast-arm's
    /// targeting flag_word. `0` for ordinary casts (the implicit-target switch supplies the bits).
    pub targets: u32,
    /// `EffectImplicitTargetA[0]` (column 82, [`COL_IMPLICIT_TARGET_A1`]) — the implicit-target
    /// enum the cast-arm's switch adjusts the flag_word by (and the usable walk's
    /// CanAttack/CanAssist fork inside the TargetAuraState leg keys on: 6 = enemy, 21 = friend).
    pub implicit_target_a1: u32,
    /// `Stances` (column 11) — forms the spell is *explicitly* castable in, `1 << (form-1)` each
    /// (the form gate [`Self::usable_in_form`]). 0 = no form requirement of its own.
    pub stances: u32,
    /// `StancesNot` (column 12) — forms the spell is explicitly forbidden in.
    pub stances_not: u32,
    /// `CasterAuraState` (column 16) — required caster aura-state index (`1 << (n-1)` against
    /// `UNIT_FIELD_AURASTATE`); 0 = none. Revenge's defense state (1).
    pub caster_aura_state: u32,
    /// `TargetAuraState` (column 17) — required aura-state index on the CURRENT TARGET; 0 =
    /// none. The one target-dependent usable leg (§2a leg 10): Execute's healthless-20% (2).
    pub target_aura_state: u32,
    /// `Totem[2]` (columns 40-41) — tool items that must be PRESENT (not consumed): fishing
    /// pole-less fishing, blacksmith hammers. 0 = unused slot.
    pub totems: [u32; 2],
    /// `Reagent[8]`/`ReagentCount[8]` (columns 42-49/50-57) as (item entry, count) pairs —
    /// consumed materials. Entry 0 = unused slot.
    pub reagents: [(u32, u32); 8],
    /// `EquippedItemClass` (column 58, signed) — the item class a worn item must match (2 =
    /// weapon for Auto Shot's bow); `-1` = no requirement.
    pub equipped_item_class: i32,
    /// `EquippedItemSubClassMask` (column 59) — `1 << subclass` mask refining
    /// [`Self::equipped_item_class`] (`0x4000c` = bows|guns|crossbows).
    pub equipped_item_subclass_mask: u32,
    /// `RequiresSpellFocus` ([`COL_REQUIRES_SPELL_FOCUS`]) — the `SpellFocusObject.dbc` object
    /// that must be nearby (1 Anvil, 3 Forge, 4 Cooking Fire — [`crate::spell_focus`]); 0 = none.
    /// The crafting book's "Requires: …" line (0437).
    pub requires_spell_focus: u32,
    /// The `SpellShapeshiftForm.dbc` form id this spell shifts into — the `EffectMiscValue` of
    /// its first `SPELL_AURA_MOD_SHAPESHIFT` apply-aura effect — or `None` for a non-form spell.
    /// The stance bar's admission + isActive keys (wow-re `shapeshift-bar-api.md`, VERIFIED).
    pub shapeshift_form: Option<u32>,
    /// `StanceBarOrder` ([`COL_STANCE_BAR_ORDER`], signed) — the stance bar's sort key
    /// (ascending, negative last, spell id tiebreak).
    pub stance_bar_order: i32,
    /// `ActiveIconID`'s resolved texture path ([`COL_ACTIVE_ICON_ID`]) — shown on the stance
    /// button while this form is active, when present (druid forms); `None` falls back to `icon`.
    pub active_icon: Option<String>,
    /// `ActiveIconID` raw ([`COL_ACTIVE_ICON_ID`]) — the plain-path active-action toggle's gate
    /// (`0x4e55f0`/`0x4b36f0` test the COLUMN, not the resolved texture; wow-re
    /// `shapeshift-plaincast-toggle.md`): a nonzero id marks the spell press-again-to-cancel
    /// while its own aura is live (`benilla::ui_action::toggle`).
    pub active_icon_id: u32,
    /// `Description` enUS (column 138, module docs) — the tooltip body, raw `$`-token text
    /// un-substituted (decision 0274 P2's engine resolves the tokens; this is the source text).
    /// `None` when the column is empty.
    pub description: Option<String>,
    /// `AuraDescription` enUS (column 147, module docs) — the buff/debuff-icon tooltip text (a
    /// shorter restatement of the periodic/ongoing effect); `None` when empty (most direct-damage/
    /// direct-effect spells with no aura component, e.g. Fire Blast).
    pub aura_description: Option<String>,
    /// `DurationIndex` (column 30, module docs) — the [`SpellDurationCatalog`] row. `0` resolves to
    /// no row (an instant-hit spell with no periodic/aura tail).
    pub duration_index: u32,
    /// `CastingTimeIndex` (column 18, module docs) — the [`SpellCastTimeCatalog`] row. Row 1 is the
    /// universal instant sentinel (`0` ms).
    pub casting_time_index: u32,
    /// `ProcChance` (column 25, module docs) — percent chance to trigger on the proc event
    /// `procFlags` names (not read here); vmangos's own `101` sentinel means "always, no roll".
    pub proc_chance: u32,
    /// `EffectBasePoints[3]` (column 76, module docs, **signed**) — each effect's roll floor.
    /// `-1` is the weapon-damage/no-fixed-roll sentinel (Auto Shot, Feign Death).
    pub effect_base_points: [i32; 3],
    /// `EffectDieSides[3]` (column 64, module docs, **signed**) — each effect's die size; the roll
    /// is `base_points + 1 ..= base_points + die_sides` (`die_sides <= 1` collapses to a flat value).
    pub effect_die_sides: [i32; 3],
    /// `EffectBaseDice[3]` (column 67, module docs).
    pub effect_base_dice: [i32; 3],
    /// `EffectAmplitude[3]` ms (column 94, module docs) — a periodic effect's tick period; `0` =
    /// not periodic.
    pub effect_amplitude: [u32; 3],
    /// `EffectApplyAuraName[3]` (column 91 == [`COL_EFFECT_APPLY_AURA_1`]) — each effect's
    /// `SpellAuraDefines` aura-type enum; `0` = not an apply-aura effect. Slot 0 is also read via
    /// [`Self::shapeshift_form`]'s derivation.
    pub effect_apply_aura: [u32; 3],
    /// `EffectRadiusIndex[3]` (column 88, module docs) — each effect's `SpellRadius.dbc` row (not
    /// loaded by this crate yet); `0` = no radius (a single-target effect).
    pub effect_radius_index: [u32; 3],
    /// `EffectChainTarget[3]` (column 100, module docs) — extra chain/jump targets beyond the
    /// primary; `0` for the overwhelming majority of spells (thinnest-evidence pin — module docs).
    pub effect_chain_targets: [u32; 3],
    /// `EffectMultipleValue[3]` (column 97, module docs, f32) — a chain/multi-target falloff
    /// multiplier; `0.0` for most effects.
    pub effect_multiple_value: [f32; 3],
    /// `EffectTriggerSpell[3]` (column 109 == [`COL_EFFECT_TRIGGER_1`]) — each effect's triggered
    /// spell id (a proc, a learn-wrapper's taught ability, Frost Armor's `$6136` chill proc); `0` =
    /// no trigger.
    pub effect_trigger_spell: [u32; 3],
    /// `EffectItemType[3]` ([`COL_EFFECT_ITEM_TYPE_1`]) — the item entry a
    /// `SPELL_EFFECT_CREATE_ITEM` effect creates (the crafting book's product, 0437); `0` = none.
    pub effect_item_type: [u32; 3],
    /// `EffectMiscValue[3]` (column 106 == [`COL_EFFECT_MISC_1`], **signed**) — each effect's
    /// misc payload. For an effect-47 opener, slot 0 is the **window routing key** (wow-re
    /// `tradeskill` node, VERIFIED at `0x6e4bd7`: `!= 0` → the CraftFrame — Enchanting 3, Beast
    /// Training 1 — else the TradeSkillFrame). The OPEN_LOCK/shapeshift slots are already
    /// exposed derived ([`Self::open_lock`]/[`Self::shapeshift_form`]); this is the raw
    /// array.
    pub effect_misc_value: [i32; 3],
    /// `EffectDicePerLevel[3]` ([`COL_EFFECT_DICE_PER_LEVEL_1`]) — the integer per-level term of
    /// the effect-value walk (`0x6e3871`); `0` on every 5875 opener.
    pub effect_dice_per_level: [i32; 3],
    /// `EffectRealPointsPerLevel[3]` ([`COL_EFFECT_REAL_POINTS_PER_LEVEL_1`]) — the float
    /// per-level term (`0x6e3889`). `5.0` on Pick Lock / Mining / Herb Gathering, which is what
    /// makes their provided lock skill track the 5×level profession cap.
    pub effect_real_points_per_level: [f32; 3],
}

/// Hand-rolled so `equipped_item_class` defaults to the data's own "no requirement" (`-1`) —
/// a derived 0 would be a *real* class requirement, silently tripping the usable walk's
/// equipped-item leg on every synthetic/`..Default::default()` spell.
impl Default for SpellDisplay {
    fn default() -> Self {
        SpellDisplay {
            name: String::new(),
            rank: None,
            icon: None,
            visual: 0,
            speed: 0.0,
            attributes: 0,
            attributes_ex: 0,
            attributes_ex2: 0,
            attributes_ex3: 0,
            passive: false,
            cast_ui: 0,
            effect_1: 0,
            open_lock: None,
            spell_level: 0,
            dispel: 0,
            category: 0,
            recovery_ms: 0,
            interrupt_flags: 0,
            aura_interrupt_flags: 0,
            channel_interrupt_flags: 0,
            category_recovery_ms: 0,
            start_recovery_category: 0,
            start_recovery_ms: 0,
            power_type: 0,
            mana_cost: 0,
            mana_cost_pct: 0,
            range_index: 0,
            targets: 0,
            implicit_target_a1: 0,
            stances: 0,
            stances_not: 0,
            caster_aura_state: 0,
            target_aura_state: 0,
            totems: [0; 2],
            reagents: [(0, 0); 8],
            equipped_item_class: -1,
            equipped_item_subclass_mask: 0,
            requires_spell_focus: 0,
            shapeshift_form: None,
            stance_bar_order: 0,
            active_icon: None,
            active_icon_id: 0,
            description: None,
            aura_description: None,
            duration_index: 0,
            casting_time_index: 0,
            proc_chance: 0,
            effect_base_points: [0; 3],
            effect_die_sides: [0; 3],
            effect_base_dice: [0; 3],
            effect_dice_per_level: [0; 3],
            effect_real_points_per_level: [0.0; 3],
            effect_amplitude: [0; 3],
            effect_apply_aura: [0; 3],
            effect_radius_index: [0; 3],
            effect_chain_targets: [0; 3],
            effect_multiple_value: [0.0; 3],
            effect_trigger_spell: [0; 3],
            effect_item_type: [0; 3],
            effect_misc_value: [0; 3],
        }
    }
}

// The dispel-class NAME (the aura tooltip's right column, and the `debuffType` the debuff border
// tints by) is not derivable from [`SpellDisplay::dispel`] alone: `SpellDispelType.dbc` carries
// both the string and the `[+0x28]` gate that decides whether a class is named at all. It lives on
// the catalog that loads that file — [`crate::SpellCatalog::dispel_name`].

impl SpellDisplay {
    /// The `LockType` this spell opens, if any — [`Self::open_lock`]'s index alone.
    pub fn open_lock_type(&self) -> Option<u32> {
        self.open_lock.map(|o| o.lock_type)
    }

    /// The **skill value this spell provides** to a lock at `caster_level` — the client's
    /// effect-value *min* (`0x6e3800`) put through its caller's rounding (`0x6e3760`:
    /// `round(2x ∓ 0.5) >> 1`), and the left-hand side of the lock resolver's satisfaction test
    /// (`0x5f850f`: `cmp eax,esi; jge`). `None` when the spell opens no lock.
    ///
    /// The client compares **this**, never the player's skill block — and that is faithful,
    /// because the DBC encodes the same number: Pick Lock 1804 at 60 → `4 + 1 + 5.0·(60−1)` =
    /// **300**, a rogue's Lockpicking cap; Mining 2575 at 60 → `−1 + 1 + 5.0·60` = **300**; and
    /// Small Seaforium Charge 4056 → `149 + 1` = **150**, exactly the `Blasting 150` its lock
    /// (id 92) asks for. The universally-known "Opening" spells are flat 100.
    pub fn open_lock_skill(&self, caster_level: u32) -> Option<i32> {
        let e = self.open_lock?.effect;
        // `0x6e3826`: level delta, floored at 0, and only subtracted when spellLevel > 0.
        let delta = caster_level.saturating_sub(self.spell_level) as f32;
        let v = self.effect_base_points[e] as f32
            + self.effect_base_dice[e] as f32
            + self.effect_dice_per_level[e] as f32 * delta
            + self.effect_real_points_per_level[e] * delta;
        // `0x6e3760`'s rounding verbatim: double, bias away from zero by a half, round-to-nearest
        // (x87 `fistp`), then arithmetic-halve.
        let doubled = if v >= 0.0 {
            v * 2.0 - 0.5
        } else {
            v * 2.0 + 0.5
        };
        Some((doubled.round_ties_even() as i32) >> 1)
    }

    /// The client's ranged-stance gate, verbatim: `AttributesEx2 & 0x20` (auto-repeat: Auto Shot,
    /// wand Shoot) **or** `Attributes & 0x2` (uses the ranged slot: every Shoot variant, Throw).
    /// Byte-verified in wow-re (`SpellRec+0x20&0x20 || +0x18&0x2` — the test every ranged trigger
    /// runs: the `SMSG_SPELL_START` stance/ammo sites `0x6e78b6`/`0x6e78f3` and the local cast-send
    /// site `0x6e5930`).
    pub fn ranged_attack(&self) -> bool {
        self.attributes_ex2 & ATTR_EX2_AUTO_REPEAT != 0 || self.attributes & ATTR_RANGED != 0
    }

    /// The auto-repeat attribute **alone** (`AttributesEx2 & 0x20`) — the narrower gate on the
    /// client's local auto-repeat state bit (`[+0xd58] & 0x200`, written only at the local
    /// cast-send `0x6e593b`), which drives the shooter's standing Load/Hold idle. `Throw` is
    /// ranged but not auto-repeat — it never sets this.
    pub fn auto_repeat(&self) -> bool {
        self.attributes_ex2 & ATTR_EX2_AUTO_REPEAT != 0
    }

    /// `AttributesEx3 & 0x400000` — `SPELL_ATTR_EX3_CASTING_CANCELS_AUTOREPEAT`: a *running*
    /// auto-repeat carrying this bit is cancelled when any new cast begins (the client's
    /// `0x60959e` test on the **cached** spell, wow-re `nocked-ammo-cancel.md` §Q-B-5). Exactly
    /// ONE of the 22357 `Spell.dbc` records has it — wand Shoot 5019; Auto Shot (75) survives
    /// new casts, which is how hunter shot-weaving works at all.
    pub fn casting_cancels_autorepeat(&self) -> bool {
        self.attributes_ex3 & 0x0040_0000 != 0
    }

    /// The ranged-slot attribute **alone** (`Attributes & 0x2`) — the client's weapon-visual
    /// fallback gate (`0x60d46a`): a ranged-slot spell whose own `SpellVisual` resolves to nothing
    /// borrows the equipped ranged weapon's `ItemDisplayInfo` visual for its fire animation —
    /// how Throw/Auto Shot/wand Shoot animate at all (byte-verified, wow-re
    /// `throw-ranged-attack-anim.md`; every one of them has `SpellVisual1 = 0`).
    pub fn ranged_slot(&self) -> bool {
        self.attributes & ATTR_RANGED != 0
    }

    /// `AttributesEx3 & 0x8000` — this spell's damage numbers render **melee-white**, not
    /// spell-gold: the second half of the combat-text color law's `B` bit (byte-verified: the
    /// emitter `0x6128b0` tests `sign(byte[SpellRec+0x25])` = bit 15 of the `+0x24` word, wow-re
    /// `combattext-color-law.md`; the "no record ⇒ melee" half lives at the call sites). On the
    /// real 5875 DBC the bit is set on exactly the ranged basic shots — Auto Shot 75, Shoot Bow
    /// 2480, Throw 2764, wand Shoot 5019 — whose damage ships as spell packets yet floats white
    /// (vmangos delivers them via `SMSG_SPELLNONMELEEDAMAGELOG`; the modern name is
    /// `SPELL_ATTR3_NORMAL_RANGED_ATTACK`). Decision 0376.
    pub fn melee_white_damage(&self) -> bool {
        self.attributes_ex3 & ATTR_EX3_NORMAL_RANGED_ATTACK != 0
    }

    /// The ranged-shot cooldown pad's gate (`0x6e2b60` at `0x6e2c2c`–`0x6e2c47`, byte-verified —
    /// wow-re `ranged-cooldown-sweep.md`): on the caster's own SPELL_GO self-insert, a
    /// `Attributes & 0x2` spell WITHOUT `AttributesEx2 & 0x20000`
    /// (`SPELL_ATTR_EX2_DO_NOT_RESET_COMBAT_TIMERS`) adds the caster's live
    /// `UNIT_FIELD_RANGEDATTACKTIME` to its category recovery — how the Throw / wand Shoot
    /// button sweeps the weapon's speed with `Spell.dbc` recovery all zero and no server packet
    /// (the category-0 Auto Shot inserts the same timer but no read can surface it).
    pub fn ranged_speed_cooldown(&self) -> bool {
        self.attributes & ATTR_RANGED != 0
            && self.attributes_ex2 & ATTR_EX2_DO_NOT_RESET_COMBAT_TIMERS == 0
    }

    /// Whether this spell appears in the spellbook — the client's book **add-gate**, byte-verified
    /// (decision 0227; wow-re `system/ui/scratch/spellbook-book-build.md`, `CGPlayer_C::AddSpell
    /// 0x5e9c20` → the classify+append `0x4b25b0`). A known spell is kept OUT of the book when any
    /// of three gates trips: `Attributes & 0x80` (`SPELL_ATTR_DO_NOT_DISPLAY` — every language,
    /// armor/weapon proficiency, and hidden racial passive), `Attributes & 0x20`
    /// (`SPELL_ATTR_IS_TRADESKILL` — professions divert to a name-only path, no book slot), or
    /// `castUI != 0`. Concrete: Fireball 133 (`Attributes 0x10000`, castUI 0) is shown;
    /// Language:Common 668 / Cloth 9078 / One-Handed Axes 196 (all `0xC0 = PASSIVE|DO_NOT_DISPLAY`)
    /// are hidden. Passive-but-shown spells exist (a passive is NOT itself a book gate); only the
    /// three bits above hide a spell.
    pub fn in_spellbook(&self) -> bool {
        self.attributes & (ATTR_DO_NOT_DISPLAY | ATTR_IS_TRADESKILL) == 0 && self.cast_ui == 0
    }

    /// The **melee auto-attack** — `Effect[0] == SPELL_EFFECT_ATTACK (78)`, the client's own
    /// effect-type trigger for the equipped-weapon icon substitution (decision 0231; wow-re
    /// `attack-icon-substitution.md`, resolvers `0x4b3f8a`/`0x4e59de`). In 1.12 the only spell
    /// carrying it is 6603 "Attack" (its `SpellIconID` → the `Temp` placeholder the client never
    /// shows, substituting the main-hand weapon icon instead).
    pub fn is_melee_auto_attack(&self) -> bool {
        self.effect_1 == SPELL_EFFECT_ATTACK
    }

    /// The spell **tooltip's** passive gate (wow-re `tooltip-content-law.md` §3.4): the
    /// CastTime|Cooldown line is skipped whole for `Attributes & 0x40` **or**
    /// `Effect[0] ∈ {0x2f, 0x4e}` — `SPELL_EFFECT_TRADE_SKILL` (the profession book entries) and
    /// `SPELL_EFFECT_ATTACK` (6603 "Attack"). Wider than [`Self::passive`], which is the
    /// spellbook's own gray-and-refuse gate and must keep reading the attribute alone.
    ///
    /// The law's note writes the field as "iconID `[+0xf4]`"; the offset is Effect[0]
    /// (`0xf4/4 == 61` — the same `[SpellRec+0xf4]` the byte-verified auto-attack resolvers
    /// compare against `0x4e`). Confirmed on the shipped data: 6603 "Attack" carries
    /// `Effect[0] = 78` with `Attributes = 0x10`, and the reference's spellbook hover shows it
    /// with NO cast-time line.
    pub fn tooltip_omits_cast_line(&self) -> bool {
        self.passive
            || matches!(
                self.effect_1,
                SPELL_EFFECT_TRADE_SKILL | SPELL_EFFECT_ATTACK
            )
    }

    // The equipped-item requirement's NAME used to be decided here, by a
    // `single_equipped_subclass` that answered only for a one-bit mask — a placeholder standing in
    // for the uncarved `0x6e2380`. `0x6e2380` is carved now, and it reads only ItemSubClass.dbc,
    // behind an ItemSubClassMask.dbc group lookup that names a wide mask in one word. The whole
    // rule (both spellings, both call sites) lives with the vocabulary it reads:
    // [`crate::ItemSubClassCatalog::requirement_name`].

    /// The **ranged** icon-substitution gate — `Attributes & 0x2` AND `AttributesEx2 & 0x20`,
    /// both bits (decision 0231's deferred ranged case; wow-re `attack-icon-substitution.md` §5,
    /// byte-verified: the paired tests at `0x4b3f99`/`0x4b3f9f` and `0x4e5a2e`/`0x4e5a34` gate the
    /// call into the ranged helper `0x4e6990`). Auto Shot 75 and wand Shoot 5019 carry both;
    /// Throw 2764 carries only `0x2` and keeps its own icon.
    pub fn ranged_icon_substitution(&self) -> bool {
        self.attributes & ATTR_RANGED != 0 && self.attributes_ex2 & ATTR_EX2_AUTO_REPEAT != 0
    }

    /// `SPELL_ATTR_COOLDOWN_ON_EVENT` (Attributes bit 25) — the cooldown record is stored **on
    /// hold** (timers parked) until `SMSG_COOLDOWN_EVENT` starts it ([`ATTR_COOLDOWN_ON_EVENT`]).
    pub fn cooldown_on_event(&self) -> bool {
        self.attributes & ATTR_COOLDOWN_ON_EVENT != 0
    }

    /// A **combo-point consumer** (`AttributesEx` bits 20/22, [`ATTR_EX_FINISHING_MOVE`]) — the
    /// usable walk's leg 5 greys it while the caster's combo-point byte is 0 (decision 0869). The
    /// rogue/druid finishers, and Overpower.
    pub fn needs_combo_points(&self) -> bool {
        self.attributes_ex & ATTR_EX_FINISHING_MOVE != 0
    }

    /// The on-next-swing class (`Attributes & 0x404`, [`ATTR_ON_NEXT_SWING`]) — the spell queues
    /// on the server's melee slot and fires on the next swing instead of casting. The client's
    /// one mask for the class: the already-casting exemption (a queued one never blocks another
    /// cast), the queued-melee slot tracking, and the range law's self-cast short-circuit all
    /// test exactly these bits.
    pub fn on_next_swing(&self) -> bool {
        self.attributes & ATTR_ON_NEXT_SWING != 0
    }

    /// Whether *casting* this spell turns on the melee auto-attack **at the send** —
    /// `Spell_C::TryCast`'s post-send tail (`6e51b5`), byte-verified whole by the 2026-07-14
    /// wow-re §5 (`combat-feel-law.md` @ c445713b): fires the attack entry `0x6131a0` iff the
    /// cast committed+sent this call (`[0xcead5c] == spell_id`), the rec passes
    /// `[ebp-2] = 0x6e5200 && Ex2-bit20 CLEAR`, and no attack is already running (`0x60ecb0`).
    /// With `0x6e5200 = (Attr & 0x404) || (AttrEx & 0x200) || (AttrEx2 & 0x100000)`, the
    /// bit20-clear exclusion reduces the send-time law to exactly this: on-next-swing
    /// (the queued strike needs swings to fire at all) or [`ATTR_EX_INITIATES_COMBAT`]
    /// (Rend, Sunder Armor, Slam), and not [`ATTR_EX2_INITIATE_COMBAT_POST_CAST`] (whose
    /// GO-deferred start we don't build — dormant by data). **Path-independent**: action
    /// button, spellbook, and `CastSpellByName` all share the one `TryCast` tail — 1.12 has
    /// no macro-side opt-out.
    pub fn initiates_auto_attack(&self) -> bool {
        (self.attributes & ATTR_ON_NEXT_SWING != 0
            || self.attributes_ex & ATTR_EX_INITIATES_COMBAT != 0)
            && self.attributes_ex2 & ATTR_EX2_INITIATE_COMBAT_POST_CAST == 0
    }

    /// Hidden from the player's buff bar — the cache-builder's **display filter**
    /// (`PlayerAuras_Update 0x4e4170`'s append pass, sites `0x4e42b6`–`0x4e42c8`). The `Attributes`
    /// clause is a **byte-width** read — `mov cl,[SpellRec+0x18]; test cl,cl; js` — so it tests
    /// bit `0x80` (`SPELL_ATTR_DO_NOT_DISPLAY`), NOT the dword sign bit that wow-re's
    /// `aura-display-pipeline.md` §3 originally transcribed (decision 0385 corrects 0268; vmangos
    /// corroborates: `SpellDefines.h:799` "not visible in spellbook or aura bar"). The other
    /// clause is `SpellRec+0x1c & 0x10000000` (`SPELL_ATTR_EX_NO_AURA_ICON`). The aura stays live
    /// on the wire and in `UNIT_FIELD_AURA` — the reference just never admits it to the display
    /// cache, so `GetPlayerBuff` cannot return it. Warrior stances (the `AttributesEx` clause) and
    /// internal proc auras like Defensive State 5302 (the `0x80` clause) are the everyday cases.
    pub fn hidden_from_aura_bar(&self) -> bool {
        self.attributes & ATTR_DO_NOT_DISPLAY != 0 || self.attributes_ex & ATTR_EX_NO_AURA_ICON != 0
    }

    /// A **tracking** spell — any of the three `EffectApplyAuraName` slots is
    /// `TRACK_CREATURES`/`TRACK_RESOURCES`/`TRACK_STEALTHED` ([`TRACKING_AURA_TYPES`], the
    /// byte-verified `{0x2c,0x2d,0x97}` test at `SpellRec+0x16c..+0x174`). A tracking aura is
    /// excluded from **every** aura display — the player-cache rebuild skips it *before* the
    /// insert (so the buff bar never shows Find Minerals), and `IsAuraDisplayable 0x519860`
    /// hides it from other units' rows the same way. Instead, the rebuild records the last
    /// visible tracking spell for `GetTrackingTexture` — the minimap's `MiniMapTrackingFrame`
    /// icon (its one consumer; `benilla::ui_aura` mirrors the whole law).
    pub fn tracking_aura(&self) -> bool {
        self.effect_apply_aura
            .iter()
            .any(|a| TRACKING_AURA_TYPES.contains(a))
    }

    /// The shapeshift-form gate as a boolean — the usable walk's leg 6. See
    /// [`Self::form_refusal`], the reason-carrying transcription this wraps.
    pub fn usable_in_form(&self, form: u8, form_is_stance: bool) -> bool {
        self.form_refusal(form, form_is_stance).is_none()
    }

    /// The shapeshift-form gate — `0x612480` (wow-re §2a: reads the caster form, builds
    /// `1 << (form-1)`, tests **StancesNot** then **Stances**, then the Attributes-bit-16 /
    /// AttributesEx2-bit-19 composition), which both the usable walk's leg 6 and the TryCast
    /// requirement validator (`0x6094f0`, the leg right after the mounted block) run. The
    /// composition over the form's stance flag — and the reason split — is the vmangos
    /// corroboration (`SpellEntry::GetErrorAtShapeshiftedCast`, `SpellEntry.cpp` — anchored to
    /// exactly the byte-named fields): a *stance*-flagged form (warrior stances, stealth —
    /// `SpellShapeshiftForm.flags1 & 1`) does not count as "shapeshifted", so ordinary spells
    /// stay usable in it; a true shapeshift (cat, bear, Ghost Wolf) blocks NOT_SHAPESHIFT
    /// spells; and a form-requiring spell out of its form is refused unless AttributesEx2
    /// bit 19 waives the requirement. `None` = castable in this form.
    pub fn form_refusal(&self, form: u8, form_is_stance: bool) -> Option<FormRefusal> {
        let stance_bit = if form == 0 { 0 } else { 1u32 << (form - 1) };
        if self.stances_not & stance_bit != 0 {
            return Some(FormRefusal::NotShapeshift);
        }
        if self.stances & stance_bit != 0 {
            return None;
        }
        if form != 0 && !form_is_stance {
            // A true shapeshift: NOT_SHAPESHIFT spells are blocked (0x3d); a spell needing
            // some other form is too (0x56).
            if self.attributes & ATTR_NOT_SHAPESHIFT != 0 {
                Some(FormRefusal::NotShapeshift)
            } else if self.stances != 0 {
                Some(FormRefusal::OnlyShapeshift)
            } else {
                None
            }
        } else if self.stances != 0
            && self.attributes_ex2 & ATTR_EX2_ALLOW_WHILE_NOT_SHAPESHIFTED == 0
        {
            // Unshifted (or a stance): a form-requiring spell is refused here unless waived.
            Some(FormRefusal::OnlyShapeshift)
        } else {
            None
        }
    }
}

/// A [`SpellDisplay::form_refusal`] verdict — which cast-fail reason `0x612480` writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormRefusal {
    /// `SPELL_FAILED_NOT_SHAPESHIFT` (0x3d, "Can't do that while shapeshifted"): blocked by the
    /// current form — a StancesNot hit, or a NOT_SHAPESHIFT spell in a true shapeshift.
    NotShapeshift,
    /// `SPELL_FAILED_ONLY_SHAPESHIFT` (0x56, "Must be in %s"): the spell requires a form the
    /// caster is not in.
    OnlyShapeshift,
}

impl FormRefusal {
    /// The wire/cast-fail reason byte (the `SPELL_FAILED_*` table index).
    pub fn reason(self) -> u8 {
        match self {
            FormRefusal::NotShapeshift => 0x3d,
            FormRefusal::OnlyShapeshift => 0x56,
        }
    }
}
