//! The combat log's chat lines — the content pipeline decision 0288 §3 named out of its own scope
//! ("transcribing ATTACKERSTATEUPDATE/spell-log packets into COMBATHIT strings is the combat-log
//! arc"), and B297 is the bill for leaving it. The wire has been decoded for a long time; it fed
//! the floating numbers, the portrait indicator and the center text, and produced **no chat at
//! all**, which is why a damage meter or Quiver's TranqAnnouncer sees nothing to parse.
//!
//! This module is the reference's `UnitCombatLog_C` TU (`~0x625000-0x62e7xx`, wow-re
//! `object-layer/scratch/w2f2.md`), the presentation half: it owns **no** arithmetic, only the
//! composition — classify both endpoints, pick a chat type, pick a GlobalString *key*, fill it.
//! The reference's own chain is four hops and we take the same four:
//!
//! ```text
//!   packet  →  classify(attacker), classify(victim)   →  0..9  (the range table names them)
//!           →  msgType selector                       →  the chat type  (0x5e = drop)
//!           →  format-string selector                 →  a GlobalString KEY + a variant code
//!           →  FrameScript_GetText(key) → vsnprintf    →  the line
//! ```
//!
//! **We ship keys, never Blizzard's text.** The reference's selectors (`0x62a290`, `0x629f90`, the
//! other 43) return the *name* of a localized template; `0x703bf0` (`FrameScript_GetText`) resolves
//! it, and when it comes back empty `0x6269f0` prints "Warning: string %s not found" instead of a
//! line. benilla already runs the player's own `Interface\FrameXML\GlobalStrings.lua` into the VM
//! at boot ([`crate::ui_script`]'s `load_global_strings`), so [`global_string`] is that same hop:
//! the key is ours, the text is the install's. That is the licence-clean shape *and* the faithful
//! one — and it is why a non-enUS install gets its own language for free.
//!
//! **The slot orders are the one thing we author.** A family's format string is Blizzard's, but
//! which value lands in which `%s`/`%d` is a fact about that string, and we state it as an ordered
//! [`Slot`] list. Two properties make that safe rather than fragile: within a family the order is
//! **invariant across the four variants** (the variant only decides whether the `Attacker`/`Victim`
//! slots are *present* — "Your %s hits %s for %d." and "%s's %s hits %s for %d." are the same list
//! with one slot dropped), and [`tests`] fills every declared family against the *real*
//! `GlobalStrings.lua` and checks the `%s`/`%d` type signature it produces matches the shipped
//! template's. A transposition cannot survive that; a drift cannot either.

use bevy::prelude::*;

use crate::names::NameCache;
use crate::ui_chat::event::ChatEventKind;

use crate::net::{GuidIndex, NetCommands, ObjectStore, Reputations, SelfGuid};
use crate::target::ring::Factions;
use crate::ui_party::GroupState;

/// A unit's standing relative to the active player — the `0..9` index every combat-log selector in
/// the reference takes, in both parameter positions.
///
/// **The indices are VERIFIED by name, not inferred from behaviour.** `0x626810` (the combat-log
/// display-range getter) indexes a 10-entry table of `{cvarName, defaultValue}` pairs at
/// `DAT_008629e0`, and those cvar names *are* the class names — read out of `WoW.exe` at
/// `0x8629e0` (file offset `0x4629e0`):
///
/// | idx | cvar | default | meaning |
/// |---|---|---|---|
/// | 0 | *(NULL)* | `100000.0` (`[0x80dcc4]`) | the active player |
/// | 1 | *(NULL)* | `100000.0` | the active player's pet |
/// | 2 | `CombatLogRangeParty` | `50` | a party member |
/// | 3 | `CombatLogRangePartyPet` | `50` | a party member's pet |
/// | 4 | `CombatLogRangeFriendlyPlayers` | `50` | any other friendly player |
/// | 5 | `CombatLogRangeFriendlyPlayersPets` | `50` | that player's pet |
/// | 6 | `CombatLogRangeHostilePlayers` | `50` | a hostile player |
/// | 7 | `CombatLogRangeHostilePlayersPets` | `50` | that player's pet |
/// | 8 | `CombatLogRangeCreature` | `30` | a creature |
/// | 9 | `""` (the empty static `0x882748`) | `0.0` (`[0x7ffd74]`) | anything else — never logged |
///
/// Index 9's range of **zero** is why the msgType selectors can treat 8 and 9 alike: a class-9
/// endpoint never passes the range gate, so it never reaches them. Index 0/1's `100000.0` is the
/// "no gate" sentinel — your own lines are never dropped for distance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum UnitClass {
    Me = 0,
    MyPet = 1,
    Party = 2,
    PartyPet = 3,
    FriendlyPlayer = 4,
    FriendlyPet = 5,
    HostilePlayer = 6,
    HostilePet = 7,
    Creature = 8,
    /// Unresolvable — not streamed, or a guid family with no unit behind it. Range `0.0`: the
    /// reference logs nothing for it, and neither do we.
    Unknown = 9,
}

impl UnitClass {
    /// The CVar naming this class's display range, `None` for the two ungated classes (0/1).
    ///
    /// **Test-only, deliberately.** The gate itself runs on [`Self::default_range`] — the
    /// compiled-in values — following the standing shape of [`crate::combat_text`]'s own cvar
    /// gates. This function is the other half of that table, kept so
    /// `tests::the_class_range_table_is_the_binarys` can pin what was read out of `WoW.exe`; it
    /// becomes the production lookup the day the live CVar table is wired in, and until then a
    /// name that drifted would otherwise have nothing checking it.
    #[cfg(test)]
    pub(crate) fn range_cvar(self) -> Option<&'static str> {
        Some(match self {
            Self::Me | Self::MyPet | Self::Unknown => return None,
            Self::Party => "CombatLogRangeParty",
            Self::PartyPet => "CombatLogRangePartyPet",
            Self::FriendlyPlayer => "CombatLogRangeFriendlyPlayers",
            Self::FriendlyPet => "CombatLogRangeFriendlyPlayersPets",
            Self::HostilePlayer => "CombatLogRangeHostilePlayers",
            Self::HostilePet => "CombatLogRangeHostilePlayersPets",
            Self::Creature => "CombatLogRangeCreature",
        })
    }

    /// The compiled-in default range in yards, before any CVar override.
    pub(crate) fn default_range(self) -> f32 {
        match self {
            Self::Me | Self::MyPet => 100_000.0,
            Self::Creature => 30.0,
            Self::Unknown => 0.0,
            _ => 50.0,
        }
    }

    /// `true` for the local player — the `SELF` half of every `…SELFOTHER`/`…OTHERSELF` key.
    ///
    /// **Only class 0.** Your own pet is class 1 and is an `OTHER` for string-selection purposes:
    /// the reference says "Your pet hits X" through the same `…OTHEROTHER` template it uses for a
    /// stranger, and routes it to `COMBAT_PET_HITS` by the *msgType* selector instead. The two
    /// classifications are independent, which is the whole reason the selectors take the class
    /// index and the string picker takes a pair of booleans.
    fn is_me(self) -> bool {
        self == Self::Me
    }
}

/// Classify one guid against the active player — the reference's `0x5efea0(ecx = GUID*)`.
///
/// **The rule is now VERIFIED**, not inferred: the wow-re §5 this arc dispatched settled it
/// (`system/object-layer/scratch/combat-log-chat-law.md` §2), and it corrected 1571's first cut on
/// three points, each of which changed behaviour:
///
/// - **The owner field is `UNIT_FIELD_CHARMEDBY`, then `UNIT_FIELD_CREATEDBY`.** We read
///   `SUMMONEDBY` first, copying the floating text's source classifier — a different field, and a
///   charmed unit was therefore judged on its own faction instead of its charmer's.
/// - **"Party" is the four party slots, NOT the raid roster.** A raid member outside your own
///   subgroup is class 4/5 (another friendly player), not class 2/3. We keyed off the whole
///   `GroupState` roster, so in a raid every one of 39 other players was "party".
/// - **Friend-or-foe is `CanAttack`, not a reaction rank.** A same-faction duel opponent reads
///   friendly by reaction and is unambiguously hostile here — which is the case the msgType
///   matrix's own duel arms exist for, so getting it from the reaction was self-defeating.
///
/// One half of the verdict is **not** implemented and is deliberately named rather than quietly
/// dropped: the reference runs `0x606980 CanAttack` **mutually**, in both directions, and
/// [`crate::target::ring::can_attack_from_player`] is specialised to the local player as the
/// attacker (1530). We run the direction we have. It differs only for a unit that can attack you
/// while you cannot attack it, which needs the general two-unit form to answer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn classify(
    guid: u64,
    self_guid: &SelfGuid,
    group: Option<&GroupState>,
    index: &GuidIndex,
    stores: &Query<&mut ObjectStore>,
    factions: Option<&Factions>,
    reputations: &Reputations,
) -> UnitClass {
    let Some(me) = self_guid.0 else {
        return UnitClass::Unknown;
    };
    if guid == me {
        return UnitClass::Me;
    }
    let Some(entity) = index.0.get(&guid).copied() else {
        return UnitClass::Unknown;
    };
    let Ok(store) = stores.get(entity) else {
        return UnitClass::Unknown;
    };
    // The owner: CHARMEDBY first, then CREATEDBY (§2). A pet/guardian/totem/charmed unit is
    // classified by WHOSE it is, one rung above its own faction.
    let owner = store
        .0
        .unit_charmed_by()
        .or_else(|| store.0.unit_created_by());
    let owned_by = |g: u64| owner == Some(g);

    if owned_by(me) {
        return UnitClass::MyPet;
    }
    // **Party, not raid.** The reference's party slots hold your own subgroup; a raid member in
    // another subgroup is just another friendly player. `flags` bits 0-2 are the subgroup on both
    // sides of this comparison (`GroupMemberEntry::flags` / `GroupState::own_flags`), so an
    // ordinary 5-man party — where everyone is subgroup 0 — needs no special case.
    let in_party = |g: u64| {
        group.is_some_and(|s| {
            s.in_group
                && s.members
                    .iter()
                    .any(|m| m.guid == g && m.flags & 0x7 == s.own_flags & 0x7)
        })
    };
    if in_party(guid) {
        return UnitClass::Party;
    }
    if owner.is_some_and(in_party) {
        return UnitClass::PartyPet;
    }

    // Friend or foe — `CanAttack`, judged on **this unit**, never on its owner (§2). The owner
    // hop was ours and it was wrong: the reference asks the pet itself.
    let hostile = {
        let me_store = index.0.get(&me).copied().and_then(|e| stores.get(e).ok());
        crate::target::ring::can_attack_from_player(
            factions,
            reputations,
            Some(store),
            me_store,
            benilla_protocol::guid::is_player(guid),
        )
    };

    if benilla_protocol::guid::is_player(guid) {
        return if hostile {
            UnitClass::HostilePlayer
        } else {
            UnitClass::FriendlyPlayer
        };
    }
    // A creature with a PLAYER owner is that player's pet; a creature owned by nothing (or by
    // another creature) is just a creature.
    if owner.is_some_and(benilla_protocol::guid::is_player) {
        return if hostile {
            UnitClass::HostilePet
        } else {
            UnitClass::FriendlyPet
        };
    }
    UnitClass::Creature
}

// ─────────────────────────────── the msgType selectors ────────────────────────────────

/// `0x62a0d0` / `0x62a2e0` — the melee family's `(attacker, victim) → chat type`, `None` where the
/// reference returns `0x5e` (94, one past the end of the 94-entry type table = "no type", checked
/// at the emit `0x626850`).
///
/// Read off the decompiled selectors and cross-checked against wow-re's byte-verified 94-entry
/// default colour table (`system/ui/scratch/chat-color-table.md`, whose index column is the 1-based
/// `GetChatTypeIndex` value — so the `0x1b` the selector returns is that table's row 28,
/// `COMBAT_SELF_HITS`). `miss` picks the odd twin: every HITS type is immediately followed by its
/// MISSES type, which is why the reference's two selectors differ only by `+1`.
///
/// **The two reclassifying arms are real and they are the duel/PvP case.** A *party* player (2) or
/// a *friendly* player (4) whose victim is you, your pet, or a party member is reported in the
/// HOSTILEPLAYER bucket — the client decides "attacking me makes you hostile" at the log, without
/// consulting faction. Their pets (3 and 5) get no such treatment.
pub(crate) fn combat_kind(
    attacker: UnitClass,
    victim: UnitClass,
    miss: bool,
) -> Option<ChatEventKind> {
    use ChatEventKind as K;
    use UnitClass as C;
    let mine = matches!(victim, C::Me | C::MyPet | C::Party | C::PartyPet);
    let hits = match attacker {
        C::Me => K::CombatSelfHits,
        C::MyPet => K::CombatPetHits,
        C::Party if mine => K::CombatHostilePlayerHits,
        C::Party | C::PartyPet => K::CombatPartyHits,
        C::FriendlyPlayer if mine => K::CombatHostilePlayerHits,
        C::FriendlyPlayer | C::FriendlyPet => K::CombatFriendlyPlayerHits,
        C::HostilePlayer | C::HostilePet => K::CombatHostilePlayerHits,
        C::Creature | C::Unknown => match victim {
            C::Me | C::MyPet => K::CombatCreatureVsSelfHits,
            C::Party | C::PartyPet => K::CombatCreatureVsPartyHits,
            _ => K::CombatCreatureVsCreatureHits,
        },
    };
    Some(if miss { miss_twin(hits) } else { hits })
}

/// `0x627820` and its damage sibling — the direct-spell family's `(attacker, victim) → chat type`.
///
/// Byte-identical in shape to [`combat_kind`] (same ten source arms, same two reclassifying arms,
/// same victim split for a creature source); only the base row differs, and `buff` picks the odd
/// twin exactly as `miss` does there. A *heal*, a *power gain* and an *aura* are all "BUFF"; damage
/// and every failed-to-land outcome are "DAMAGE".
pub(crate) fn spell_kind(
    attacker: UnitClass,
    victim: UnitClass,
    buff: bool,
) -> Option<ChatEventKind> {
    use ChatEventKind as K;
    use UnitClass as C;
    let mine = matches!(victim, C::Me | C::MyPet | C::Party | C::PartyPet);
    let damage = match attacker {
        C::Me => K::SpellSelfDamage,
        C::MyPet => K::SpellPetDamage,
        C::Party if mine => K::SpellHostilePlayerDamage,
        C::Party | C::PartyPet => K::SpellPartyDamage,
        C::FriendlyPlayer if mine => K::SpellHostilePlayerDamage,
        C::FriendlyPlayer | C::FriendlyPet => K::SpellFriendlyPlayerDamage,
        C::HostilePlayer | C::HostilePet => K::SpellHostilePlayerDamage,
        C::Creature | C::Unknown => match victim {
            C::Me | C::MyPet => K::SpellCreatureVsSelfDamage,
            C::Party | C::PartyPet => K::SpellCreatureVsPartyDamage,
            _ => K::SpellCreatureVsCreatureDamage,
        },
    };
    Some(if buff { buff_twin(damage) } else { damage })
}

/// `0x627d80` (damage) / `0x6274a0` (buffs) — the periodic family, and it is **a different shape**.
///
/// Ten rows, not sixteen: there is no PET bucket and no `CREATURE_VS_*` split, so a pet folds into
/// its owner's row and every creature source lands on one `SPELL_PERIODIC_CREATURE_*` row. And the
/// **victim is not consulted at all** — both selectors take a single argument. A DoT you put on a
/// hostile player and a DoT that player put on you are told apart by the *source* alone.
pub(crate) fn periodic_kind(attacker: UnitClass, buff: bool) -> Option<ChatEventKind> {
    use ChatEventKind as K;
    use UnitClass as C;
    let damage = match attacker {
        C::Me | C::MyPet => K::SpellPeriodicSelfDamage,
        C::Party | C::PartyPet => K::SpellPeriodicPartyDamage,
        C::FriendlyPlayer | C::FriendlyPet => K::SpellPeriodicFriendlyPlayerDamage,
        C::HostilePlayer | C::HostilePet => K::SpellPeriodicHostilePlayerDamage,
        C::Creature | C::Unknown => K::SpellPeriodicCreatureDamage,
    };
    Some(if buff { buff_twin(damage) } else { damage })
}

/// The HITS → MISSES step. Every combat pair is adjacent in the type table, so the reference's two
/// selectors are one table apart; this is that `+1`, spelled as the pairing it encodes.
fn miss_twin(hits: ChatEventKind) -> ChatEventKind {
    use ChatEventKind as K;
    match hits {
        K::CombatSelfHits => K::CombatSelfMisses,
        K::CombatPetHits => K::CombatPetMisses,
        K::CombatPartyHits => K::CombatPartyMisses,
        K::CombatFriendlyPlayerHits => K::CombatFriendlyPlayerMisses,
        K::CombatHostilePlayerHits => K::CombatHostilePlayerMisses,
        K::CombatCreatureVsSelfHits => K::CombatCreatureVsSelfMisses,
        K::CombatCreatureVsPartyHits => K::CombatCreatureVsPartyMisses,
        K::CombatCreatureVsCreatureHits => K::CombatCreatureVsCreatureMisses,
        other => other,
    }
}

/// The DAMAGE → BUFF step — [`miss_twin`]'s spell-family twin, same `+1` in the type table.
fn buff_twin(damage: ChatEventKind) -> ChatEventKind {
    use ChatEventKind as K;
    match damage {
        K::SpellSelfDamage => K::SpellSelfBuff,
        K::SpellPetDamage => K::SpellPetBuff,
        K::SpellPartyDamage => K::SpellPartyBuff,
        K::SpellFriendlyPlayerDamage => K::SpellFriendlyPlayerBuff,
        K::SpellHostilePlayerDamage => K::SpellHostilePlayerBuff,
        K::SpellCreatureVsSelfDamage => K::SpellCreatureVsSelfBuff,
        K::SpellCreatureVsPartyDamage => K::SpellCreatureVsPartyBuff,
        K::SpellCreatureVsCreatureDamage => K::SpellCreatureVsCreatureBuff,
        K::SpellPeriodicSelfDamage => K::SpellPeriodicSelfBuffs,
        K::SpellPeriodicPartyDamage => K::SpellPeriodicPartyBuffs,
        K::SpellPeriodicFriendlyPlayerDamage => K::SpellPeriodicFriendlyPlayerBuffs,
        K::SpellPeriodicHostilePlayerDamage => K::SpellPeriodicHostilePlayerBuffs,
        K::SpellPeriodicCreatureDamage => K::SpellPeriodicCreatureBuffs,
        other => other,
    }
}

// ──────────────────────────── the format-string selectors ─────────────────────────────

/// Which of a family's four templates applies — the reference's 45 `char*(self, other, *out)`
/// selectors (wow-re `w2f2.md` §G7), whose two arguments are booleans ("the subject is not me",
/// "the object is not me") and whose `*out` variant code `0..3` is what tells the caller how many
/// names to push.
///
/// `SelfSelf` is the code-`0` case for most families: the selector returns NULL and **no line is
/// produced**. Some families do define the key (`SPELLLOGSELFSELF`, `HEALEDSELFSELF`,
/// `PERIODICAURAHEALSELFSELF`); the ones that don't simply miss the lookup, which lands on the same
/// silence by the same route the reference's "string not found" arm takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Variant {
    /// I did it to someone else — `…SELFOTHER`, variant code 2.
    SelfOther,
    /// Someone else did it to me — `…OTHERSELF`, code 1.
    OtherSelf,
    /// Someone else, to someone else — `…OTHEROTHER`, code 3.
    OtherOther,
    /// Me, to me — `…SELFSELF`, code 0 for the families that have no such key.
    SelfSelf,
}

impl Variant {
    fn of(subject: UnitClass, object: UnitClass) -> Self {
        match (subject.is_me(), object.is_me()) {
            (true, false) => Self::SelfOther,
            (false, true) => Self::OtherSelf,
            (false, false) => Self::OtherOther,
            (true, true) => Self::SelfSelf,
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Self::SelfOther => "SELFOTHER",
            Self::OtherSelf => "OTHERSELF",
            Self::OtherOther => "OTHEROTHER",
            Self::SelfSelf => "SELFSELF",
        }
    }

    /// Whether the subject is spelled by name in this variant (rather than as "You"/"Your").
    fn names_subject(self) -> bool {
        matches!(self, Self::OtherSelf | Self::OtherOther)
    }

    /// Whether the object is spelled by name in this variant.
    fn names_object(self) -> bool {
        matches!(self, Self::SelfOther | Self::OtherOther)
    }
}

/// One value slot in a family's format string, in the order the shipped template consumes it.
///
/// `Attacker` and `Victim` are **conditional** — present only when [`Variant::names_subject`] /
/// [`Variant::names_object`] says the template spells that endpoint out. Everything else is
/// unconditional. That conditionality is exactly what makes a single ordered list describe all four
/// variants of a family: "Your %s hits %s for %d." and "%s's %s hits %s for %d." are the same
/// `[Attacker?, Spell, Victim?, Amount]`, once with the first slot dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// `%s` — the subject's name, only when the variant spells it.
    Attacker,
    /// `%s` — the object's name, only when the variant spells it.
    Victim,
    /// `%s` — the spell's name.
    Spell,
    /// `%s` — the damage school's lowercase word (`SPELL_SCHOOL<n>_NAME`).
    School,
    /// `%s` — a power's word (`MANA_POINTS`/`RAGE_POINTS`/`FOCUS_POINTS`/`ENERGY_POINTS`).
    Power,
    /// `%d` — the primary amount.
    Amount,
    /// `%d` — a second amount (the leech family's "You gain %d").
    Amount2,
    /// `%s` — a second power word (the leech family's gained power).
    Power2,
}

/// A combat-log message family: the stem its four keys are built from, and the ordered slots its
/// templates consume.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Family {
    pub stem: &'static str,
    pub slots: &'static [Slot],
}

/// The value bound to each [`Slot`] for one line. Built by the caller from the packet; the family's
/// slot list decides which of these are read and in what order.
///
/// **The school and power slots hold INDICES, not words.** They are resolved to text inside
/// [`compose_line`], the same place the template itself is resolved and for the same reason: both
/// are localized strings off the install, and the packet arm that fills this struct has no VM in
/// hand. `None` in either where the family asks for it drops the line rather than printing a gap.
#[derive(Clone, Debug, Default)]
pub(crate) struct Fills {
    pub attacker: String,
    pub victim: String,
    pub spell: String,
    /// The damage school index (`SpellSchools`: 0 physical … 6 arcane) → `SPELL_SCHOOL<n>_NAME`.
    pub school: Option<u8>,
    /// The vmangos `Powers` index (0 mana … 3 energy) → `MANA_POINTS` and kin.
    pub power: Option<u32>,
    pub amount: i64,
    pub amount2: i64,
    /// The leech family's *gained* power index — usually the same as [`Self::power`].
    pub power2: Option<u32>,
}

/// Resolve a family + variant to a finished line, or `None` when the key is absent from the
/// install's `GlobalStrings.lua` (the reference's "Warning: string %s not found" arm — no line).
///
/// The `%s`/`%d` walk is `vsnprintf` with a fixed argument list, which is what the reference does
/// at `0x626850`; 1.12's templates carry no positional (`%1$s`) specifiers, and could not — the
/// MSVC CRT the client links has never supported them.
pub(crate) fn compose_line(
    lua: &benilla_ui::script::UiScript,
    family: Family,
    variant: Variant,
    fills: &Fills,
) -> Option<String> {
    let key = format!("{}{}", family.stem, variant.suffix());
    let template = global_string(lua, &key)?;
    // Resolved before the walk so the borrows outlive it.
    let school = fills.school.and_then(|s| school_word(lua, s));
    let power = fills.power.and_then(|p| power_word(lua, p));
    let power2 = fills.power2.and_then(|p| power_word(lua, p));
    let mut args: Vec<Arg> = Vec::with_capacity(family.slots.len());
    for slot in family.slots {
        match slot {
            Slot::Attacker if variant.names_subject() => args.push(Arg::Str(&fills.attacker)),
            Slot::Victim if variant.names_object() => args.push(Arg::Str(&fills.victim)),
            Slot::Attacker | Slot::Victim => {}
            Slot::Spell => args.push(Arg::Str(&fills.spell)),
            Slot::School => args.push(Arg::Str(school.as_deref()?)),
            Slot::Power => args.push(Arg::Str(power.as_deref()?)),
            Slot::Power2 => args.push(Arg::Str(power2.as_deref()?)),
            Slot::Amount => args.push(Arg::Num(fills.amount)),
            Slot::Amount2 => args.push(Arg::Num(fills.amount2)),
        }
    }
    match fill(&template, &args) {
        Some(line) => Some(line),
        None => {
            // A mismatch means our declared slot order disagrees with the shipped template — a
            // real defect, not a data condition, so it is loud. `tests` proves it cannot happen for
            // the enUS file; a locale that reshaped a string would land here rather than printing
            // a mangled sentence.
            warn!(
                "combat log: {key} does not match our slot order ({:?})",
                family.slots
            );
            None
        }
    }
}

/// One `vsnprintf` argument.
enum Arg<'a> {
    Str(&'a str),
    Num(i64),
}

/// `vsnprintf` over the `%s`/`%d` subset the combat-log templates use. `None` on any mismatch —
/// too few arguments, or a `%d` where a string is queued (and vice versa).
fn fill(template: &str, args: &[Arg<'_>]) -> Option<String> {
    let mut out = String::with_capacity(template.len() + 32);
    let mut next = args.iter();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('s') => match next.next()? {
                Arg::Str(s) => out.push_str(s),
                Arg::Num(_) => return None,
            },
            Some('d') => match next.next()? {
                Arg::Num(n) => out.push_str(&n.to_string()),
                Arg::Str(_) => return None,
            },
            // Any other conversion is outside the vocabulary these templates use; treating it as
            // a mismatch is better than silently emitting a half-filled sentence.
            _ => return None,
        }
    }
    next.next().is_none().then_some(out)
}

/// A GlobalString off the VM's globals, `None` when absent **or empty** — the same two tests
/// `0x703bf0`'s callers make (`GetPVPRankInfo` at `0x51aa1c`/`0x51aa20` is the pattern
/// [`crate::ui_script`] already follows for the pvp strings).
pub(crate) fn global_string(script: &benilla_ui::script::UiScript, key: &str) -> Option<String> {
    script
        .lua()
        .globals()
        .get::<Option<String>>(key)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

/// The lowercase school word a `…SCHOOL…` template's `%s` takes — `SPELL_SCHOOL<n>_NAME`
/// ("physical", "holy", "fire", …), resolved through the install's own strings like every other
/// template slot.
pub(crate) fn school_word(script: &benilla_ui::script::UiScript, school: u8) -> Option<String> {
    global_string(script, &format!("SPELL_SCHOOL{school}_NAME"))
}

/// The power word a `POWERGAIN`/`SPELLPOWERLEECH`/`SPELLPOWERDRAIN` template takes, by the vmangos
/// `Powers` index the wire carries (0 mana · 1 rage · 2 focus · 3 energy). Happiness (4) has no
/// GlobalString and no combat-log line, so it answers `None` and the line is dropped.
pub(crate) fn power_word(script: &benilla_ui::script::UiScript, power: u32) -> Option<String> {
    let key = match power {
        0 => "MANA_POINTS",
        1 => "RAGE_POINTS",
        2 => "FOCUS_POINTS",
        3 => "ENERGY_POINTS",
        _ => return None,
    };
    global_string(script, key)
}

/// Resolve one endpoint's display name — the reference's `GetObjectName` (`0x6264e0`), which is the
/// same ask-once name cache every other client-composed chat line waits on. `None` = not yet
/// answered; the caller re-tries next frame, exactly as the reference's deferred-name queue
/// (`DAT_00c4e208`, drained by the name-ready callback `0x6294b0`) replays its message.
pub(crate) fn object_name(
    guid: u64,
    names: &mut NameCache,
    commands: &NetCommands,
) -> Option<String> {
    // Guid 0 = "the name is already in the fills" — no wire endpoint is ever guid 0, so the
    // sentinel costs nothing and is what lets `/chattest` drive the real drain with literal names.
    if guid == 0 {
        return None;
    }
    names.resolve(guid, commands).map(str::to_owned)
}

// ────────────────────────────────── the families ──────────────────────────────────────

/// Declare a family: its key stem and the ordered slots the shipped templates consume.
///
/// The order is read off the enUS `GlobalStrings.lua` and each declaration carries that line, so a
/// reader can check it without an install. `tests::every_family_matches_the_shipped_template` is
/// what checks it *with* one.
macro_rules! family {
    ($name:ident, $stem:literal, [$($slot:ident),* $(,)?], $doc:literal) => {
        #[doc = $doc]
        pub(crate) const $name: Family = Family {
            stem: $stem,
            slots: &[$(Slot::$slot),*],
        };
    };
}

// ── melee: the swing that landed ───────────────────────────────────────────────────────
family!(
    COMBATHIT,
    "COMBATHIT",
    [Attacker, Victim, Amount],
    "`\"%s hits %s for %d.\"` (GlobalStrings.lua:777)"
);
family!(
    COMBATHITCRIT,
    "COMBATHITCRIT",
    [Attacker, Victim, Amount],
    "`\"%s crits %s for %d.\"` (:771)"
);
family!(
    COMBATHITSCHOOL,
    "COMBATHITSCHOOL",
    [Attacker, Victim, Amount, School],
    "`\"%s hits %s for %d %s damage.\"` (:779)"
);
family!(
    COMBATHITCRITSCHOOL,
    "COMBATHITCRITSCHOOL",
    [Attacker, Victim, Amount, School],
    "`\"%s crits %s for %d %s damage.\"` (:773)"
);

// ── melee: the swing that did not ──────────────────────────────────────────────────────
family!(
    MISSED,
    "MISSED",
    [Attacker, Victim],
    "`\"%s misses %s.\"` (:2698)"
);
family!(
    VSDODGE,
    "VSDODGE",
    [Attacker, Victim],
    "`\"%s attacks. %s dodges.\"` (:5400)"
);
family!(
    VSPARRY,
    "VSPARRY",
    [Attacker, Victim],
    "`\"%s attacks. %s parries.\"` (:5421)"
);
family!(
    VSBLOCK,
    "VSBLOCK",
    [Attacker, Victim],
    "`\"%s attacks. %s blocks.\"` (:5394)"
);
family!(
    VSABSORB,
    "VSABSORB",
    [Attacker, Victim],
    "`\"%s attacks. %s absorbs all the damage.\"` (:5391)"
);
family!(
    VSRESIST,
    "VSRESIST",
    [Attacker, Victim],
    "`\"%s attacks. %s resists all the damage.\"` (:5424)"
);
family!(
    VSIMMUNE,
    "VSIMMUNE",
    [Attacker, Victim],
    "`\"%s attacks but %s is immune.\"` (:5418)"
);
family!(
    VSEVADE,
    "VSEVADE",
    [Attacker, Victim],
    "`\"%s attacks. %s evades.\"` (:5415)"
);
family!(
    VSDEFLECT,
    "VSDEFLECT",
    [Attacker, Victim],
    "`\"%s attacks. %s deflects.\"` (:5397)"
);

// ── a spell's direct damage ────────────────────────────────────────────────────────────
family!(
    SPELLLOG,
    "SPELLLOG",
    [Attacker, Spell, Victim, Amount],
    "`\"%s's %s hits %s for %d.\"` (:3878)"
);
family!(
    SPELLLOGCRIT,
    "SPELLLOGCRIT",
    [Attacker, Spell, Victim, Amount],
    "`\"%s's %s crits %s for %d.\"` (:3870)"
);
family!(
    SPELLLOGSCHOOL,
    "SPELLLOGSCHOOL",
    [Attacker, Spell, Victim, Amount, School],
    "`\"%s's %s hits %s for %d %s damage.\"` (:3880)"
);
family!(
    SPELLLOGCRITSCHOOL,
    "SPELLLOGCRITSCHOOL",
    [Attacker, Spell, Victim, Amount, School],
    "`\"%s's %s crits %s for %d %s damage.\"` (:3872)"
);

// ── a spell that did not land ──────────────────────────────────────────────────────────
family!(
    SPELLMISS,
    "SPELLMISS",
    [Attacker, Spell, Victim],
    "`\"%s's %s missed %s.\"` (:3886)"
);
family!(
    SPELLRESIST,
    "SPELLRESIST",
    [Attacker, Spell, Victim],
    "`\"%s's %s was resisted by %s.\"` (:3911)"
);
family!(
    SPELLDODGED,
    "SPELLDODGED",
    [Attacker, Spell, Victim],
    "`\"%s's %s was dodged by %s.\"` (:3835)"
);
family!(
    SPELLPARRIED,
    "SPELLPARRIED",
    [Attacker, Spell, Victim],
    "`\"%s's %s was parried by %s.\"` (:3890)"
);
family!(
    SPELLBLOCKED,
    "SPELLBLOCKED",
    [Attacker, Spell, Victim],
    "`\"%s's %s was blocked by %s.\"` (:3817)"
);
family!(
    SPELLEVADED,
    "SPELLEVADED",
    [Attacker, Spell, Victim],
    "`\"%s's %s was evaded by %s.\"` (:3845)"
);
family!(
    SPELLIMMUNE,
    "SPELLIMMUNE",
    [Attacker, Spell, Victim],
    "`\"%s's %s fails. %s is immune.\"` (:3859)"
);
family!(
    SPELLDEFLECTED,
    "SPELLDEFLECTED",
    [Attacker, Spell, Victim],
    "`\"%s's %s was deflected by %s.\"` (:3829)"
);
family!(
    SPELLLOGABSORB,
    "SPELLLOGABSORB",
    [Attacker, Spell, Victim],
    "`\"%s's %s is absorbed by %s.\"` (:3866)"
);
family!(
    SPELLREFLECT,
    "SPELLREFLECT",
    [Attacker, Spell, Victim],
    "`\"%s's %s is reflected back by %s.\"` (:3907)"
);

// ── heals and power ────────────────────────────────────────────────────────────────────
family!(
    HEALED,
    "HEALED",
    [Attacker, Spell, Victim, Amount],
    "`\"%s's %s heals %s for %d.\"` (:2129)"
);
family!(
    HEALEDCRIT,
    "HEALEDCRIT",
    [Attacker, Spell, Victim, Amount],
    "`\"%s's %s critically heals %s for %d.\"` (:2125)"
);
family!(
    POWERGAIN,
    "POWERGAIN",
    [Victim, Amount, Power, Attacker, Spell],
    "`\"%s gains %d %s from %s's %s.\"` (:3091)"
);
family!(SPELLPOWERLEECH, "SPELLPOWERLEECH",
    [Attacker, Spell, Amount, Power, Victim, Attacker, Amount2, Power2],
    "`\"%s's %s drains %d %s from %s. %s gains %d %s.\"` (:3904) — the only family that spells the \
     subject TWICE, and the second one is the same name as the first (the drainer is the gainer).");

// ── periodic ticks ─────────────────────────────────────────────────────────────────────
family!(
    PERIODICAURADAMAGE,
    "PERIODICAURADAMAGE",
    [Victim, Amount, School, Attacker, Spell],
    "`\"%s suffers %d %s damage from %s's %s.\"` (:3001)"
);
family!(
    PERIODICAURAHEAL,
    "PERIODICAURAHEAL",
    [Victim, Amount, Attacker, Spell],
    "`\"%s gains %d health from %s's %s.\"` (:3005)"
);

// ── a damage shield firing back ────────────────────────────────────────────────────────
family!(
    DAMAGESHIELD,
    "DAMAGESHIELD",
    [Attacker, Amount, School, Victim],
    "`\"%s reflects %d %s damage to %s.\"` (:884) — the SUBJECT is the shield's BEARER and the \
     object is whoever struck them, which is the reverse of the packet's own field names."
);

/// Every family this module can emit — the sweep [`tests`] uses to check each one against the
/// shipped `GlobalStrings.lua`. Adding a family without adding it here is the only way to get an
/// unchecked slot order, so the list is the gate.
#[cfg(test)]
pub(crate) const ALL_FAMILIES: &[Family] = &[
    COMBATHIT,
    COMBATHITCRIT,
    COMBATHITSCHOOL,
    COMBATHITCRITSCHOOL,
    MISSED,
    VSDODGE,
    VSPARRY,
    VSBLOCK,
    VSABSORB,
    VSRESIST,
    VSIMMUNE,
    VSEVADE,
    VSDEFLECT,
    SPELLLOG,
    SPELLLOGCRIT,
    SPELLLOGSCHOOL,
    SPELLLOGCRITSCHOOL,
    SPELLMISS,
    SPELLRESIST,
    SPELLDODGED,
    SPELLPARRIED,
    SPELLBLOCKED,
    SPELLEVADED,
    SPELLIMMUNE,
    SPELLDEFLECTED,
    SPELLLOGABSORB,
    SPELLREFLECT,
    HEALED,
    HEALEDCRIT,
    POWERGAIN,
    SPELLPOWERLEECH,
    PERIODICAURADAMAGE,
    PERIODICAURAHEAL,
    DAMAGESHIELD,
];

/// The `SpellMissInfo` byte `SMSG_SPELLLOGMISS` (and `SpellGo`'s own list) carries → the family
/// that words it. vmangos `SpellDefines.h:160-174`; the reference reaches the same ten families
/// through its own miss switch.
///
/// There is no `None` arm: the reference's jump table sends `0`, `1`, `10` and everything past the
/// enum to one default, `SPELLMISS*`. `0` is `SPELL_MISS_NONE` and should never appear in a miss
/// list, but if one arrives the reference words it as a miss rather than dropping it.
pub(crate) fn miss_family(miss_info: u8) -> Family {
    match miss_info {
        2 => SPELLRESIST,
        3 => SPELLDODGED,
        4 => SPELLPARRIED,
        5 => SPELLBLOCKED,
        6 => SPELLEVADED,
        // 7 IMMUNE and 8 IMMUNE2 are one outcome with two server-side spellings.
        7 | 8 => SPELLIMMUNE,
        9 => SPELLDEFLECTED,
        11 => SPELLREFLECT,
        // **10 ABSORB lands on SPELLMISS, not SPELLLOGABSORB**, and so do 0, 1 and anything out of
        // range: `0x62bb50`'s `lea eax,[edi-2]; cmp eax,9; ja default` puts all four on the
        // default arm `0x62c0e0`. 1571 had 10 on `SPELLLOGABSORB` — a family this switch never
        // reaches (it belongs to the direct-damage path) — and dropped 0/1 entirely.
        _ => SPELLMISS,
    }
}

/// The melee family a swing's `HitInfo` + `VictimState` select — the reference's display dispatcher
/// `0x629b60`, arm for arm, and its order is load-bearing because the tests overlap.
///
/// ```text
///   HitInfo & 0x10 (MISS)          → MISSED
///   VictimState == 5 (BLOCKS)      → VSBLOCK
///   damage == 0 && HitInfo & 0x20  → VSABSORB        (ABSORB)
///   damage == 0 && HitInfo & 0x40  → VSRESIST        (RESIST)
///   VictimState == 1 && damage > 0 → COMBATHIT[CRIT][SCHOOL]
///   otherwise                      → the VictimState word
/// ```
///
/// The bit values are vmangos's `HitInfo` under the `> 1.9.4` conditional that is compile-time true
/// for 5875 (`Objects/UnitDefines.h:250-268`) and its `VictimState` (`:237-248`) — the same pair
/// [`crate::sound::combat`] and [`crate::combat_text`] already read, here named once instead of a
/// fourth set of bare literals.
pub(crate) fn melee_family(hit_info: u32, victim_state: u32, damage: u32, school: u8) -> Family {
    const MISS: u32 = 0x10;
    const ABSORB: u32 = 0x20;
    const RESIST: u32 = 0x40;
    const CRIT: u32 = 0x80;
    if hit_info & MISS != 0 {
        return MISSED;
    }
    if victim_state == 5 {
        return VSBLOCK;
    }
    if damage == 0 {
        if hit_info & ABSORB != 0 {
            return VSABSORB;
        }
        if hit_info & RESIST != 0 {
            return VSRESIST;
        }
    }
    if victim_state == 1 && damage > 0 {
        return match (hit_info & CRIT != 0, school != 0) {
            (false, false) => COMBATHIT,
            (true, false) => COMBATHITCRIT,
            (false, true) => COMBATHITSCHOOL,
            (true, true) => COMBATHITCRITSCHOOL,
        };
    }
    match victim_state {
        2 => VSDODGE,
        3 => VSPARRY,
        6 => VSEVADE,
        7 => VSIMMUNE,
        8 => VSDEFLECT,
        // VictimState 0 (UNAFFECTED, "seen in relation with HITINFO_MISS") and 4 (INTERRUPT) reach
        // here only on a shape vmangos does not send; MISSED is the reference's own fall-through
        // and is the least wrong thing to say about a swing that did nothing.
        _ => MISSED,
    }
}

// ────────────────────────── the queued line, awaiting its names ───────────────────────

/// A combat-log line with every decision already made and only its **names** outstanding.
///
/// This split is the reference's, not a convenience. `0x629b60` classifies and picks the family at
/// the packet; when a name the sentence needs is not in the cache yet, the message is pushed onto
/// the deferred queue at `DAT_00c4e208` and replayed by the name-ready callback `0x6294b0`. Doing
/// the same means the classification is made while both units are certainly streamed — a creature
/// that dies to the killing blow is gone from the object manager long before its name query
/// answers, and a classification deferred to that moment would read `Unknown` and drop the line.
#[derive(Clone, Debug)]
pub(crate) struct PendingCombat {
    pub kind: ChatEventKind,
    pub family: Family,
    pub variant: Variant,
    /// The guid whose name fills the `Attacker` slots — the sentence's SUBJECT, which for
    /// [`DAMAGESHIELD`] is the shield's bearer rather than the packet's `attacker` field.
    pub subject: u64,
    /// The guid whose name fills the `Victim` slots.
    pub object: u64,
    /// Everything the template needs that is not a name; the two name fields are filled at drain.
    pub fills: Fills,
    pub tries: u16,
}

/// Build a queued line from an already-classified pair, or `None` when the reference would emit
/// nothing: an unroutable type (`0x5e`), or an endpoint whose class is out of the range gate.
///
/// `kind` is the caller's — the three msgType matrices differ per family, so the arm that knows
/// which packet it is decides, and this only assembles.
pub(crate) fn queue(
    kind: ChatEventKind,
    family: Family,
    subject: (u64, UnitClass),
    object: (u64, UnitClass),
    fills: Fills,
) -> Option<PendingCombat> {
    // **A class-9 endpoint alone does NOT drop the line**, and 1571 had this wrong. Its range of
    // `0.0` means it can never satisfy *its own half* of the gate — but the gate is an OR over the
    // two endpoints (§4/§5.2), so a resolvable one at the other end still carries the line. What
    // drops here is only the case the reference's own resolve step fails on: neither endpoint
    // resolvable at all.
    if subject.1 == UnitClass::Unknown && object.1 == UnitClass::Unknown {
        return None;
    }
    Some(PendingCombat {
        kind,
        family,
        variant: Variant::of(subject.1, object.1),
        subject: subject.0,
        object: object.0,
        fills,
        tries: 0,
    })
}

#[cfg(test)]
mod tests;
