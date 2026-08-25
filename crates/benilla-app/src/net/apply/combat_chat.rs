//! The combat-log **chat** arms (B297) — the second consumer of the packets
//! [`super::combat_log`] and [`super::combat`] already read for the floating numbers.
//!
//! The split is the reference's own. `0x629b60` is the display dispatcher: one classification of
//! both endpoints, then a branch per outcome into a formatter that emits *text*. The floating
//! number comes off a different path entirely (`0x625010`, the world-anchored spawn), gated by
//! different CVars, with a different source law — which is why our two live in different files
//! rather than in one arm with two tails. Everything here composes; the law it composes with is
//! [`crate::ui_chat::combat`].
//!
//! **Every arm has the same three steps**: classify both endpoints, pick the family and the chat
//! type, queue the line for its names. What differs between them is only which packet field is the
//! sentence's subject, which is not always the packet's own `attacker`
//! (`SMSG_SPELLDAMAGESHIELD` is the standing counter-example).

use bevy::prelude::*;

use benilla_protocol::messages::{
    AttackerState, DamageShield, PeriodicAuraLog, PeriodicTick, SpellDamageLog, SpellEnergizeLog,
    SpellHealLog, SpellLogMiss,
};

use crate::ui_chat::combat::{self, Family, Fills, UnitClass};
use crate::ui_chat::ChatLog;

use super::super::{GuidIndex, ObjectStore, Reputations, SelfGuid};

/// The classification inputs every line needs, bundled so seven arms do not each grow six
/// parameters. Assembled once per drain in [`super::apply_net_updates`].
pub(super) struct ChatCtx<'a> {
    pub self_guid: &'a SelfGuid,
    pub group: Option<&'a crate::ui_party::GroupState>,
    pub index: &'a GuidIndex,
    pub factions: Option<&'a crate::target::ring::Factions>,
    pub reputations: &'a Reputations,
    pub spells: Option<&'a crate::ui_action::Spells>,
}

impl ChatCtx<'_> {
    fn classify(&self, guid: u64, stores: &Query<&mut ObjectStore>) -> UnitClass {
        combat::classify(
            guid,
            self.self_guid,
            self.group,
            self.index,
            stores,
            self.factions,
            self.reputations,
        )
    }

    /// One endpoint's half of the reference's display-range gate (`0x626630` → the per-class range
    /// getter `0x626810`): a 3-D squared distance from the ACTIVE PLAYER to the endpoint, against
    /// the class's range squared.
    ///
    /// **The comparison is strictly `<`** — `fcomp; fnstsw ax; test ah,5; jnp`, so `dist² == range²`
    /// is OUT and a NaN is out. 1571 used `<=`; the boundary is exact in the binary and there is no
    /// reason for us to be looser.
    ///
    /// **The ranges are the compiled-in defaults, not live CVars** — the same standing shape as
    /// [`crate::combat_text`]'s `COMBAT_DAMAGE`/`PET_*` gates, and for the same reason: the values
    /// are byte-read and correct, and the CVars (`CombatLogRangeParty` and its six siblings) can be
    /// wired to the live table without changing anything here. `UnitClass::default_range` carries
    /// them, including the two sentinels that make the gate a no-op for you and your pet
    /// (`100000.0`) and unconditional for an unresolvable unit (`0.0`).
    ///
    /// A pose we do not hold is treated as **in** range: dropping a line because a unit's transform
    /// had not landed yet would silently lose the killing blow on a mob that despawns, which is a
    /// worse failure than logging one fight too far away.
    fn in_range(&self, guid: u64, class: UnitClass, poses: &Query<&mut Transform>) -> bool {
        let range = class.default_range();
        if range >= 100_000.0 {
            return true;
        }
        if range <= 0.0 {
            return false;
        }
        let pose = |g: u64| {
            self.index
                .0
                .get(&g)
                .copied()
                .and_then(|e| poses.get(e).ok())
                .map(|t| t.translation)
        };
        let (Some(me), Some(them)) = (self.self_guid.0.and_then(pose), pose(guid)) else {
            return true;
        };
        me.distance_squared(them) < range * range
    }

    /// A spell's display name, or `None` when the reference would emit **no line at all** for this
    /// spell (§5.5 of the §5 verdict). Two gates, both of which 1571 was missing:
    ///
    /// - **`Attributes & 0x180`** — `SPELL_ATTR_DO_NOT_DISPLAY | SPELL_ATTR_DO_NOT_LOG` (the mask
    ///   and the `SpellRec+0x18` offset are VERIFIED; the two enum names are wow-re's INFERRED
    ///   corroboration from vmangos). This is what keeps the invisible book-keeping spells every
    ///   server casts constantly — proc triggers, aura tickers — out of the log entirely.
    /// - **An empty localized name.** The reference gates on the name it is about to print, so a
    ///   row with no text in this locale produces silence rather than a sentence with a hole in it.
    ///
    /// A spell id the catalog cannot answer at all is *not* gated: that is our own missing data,
    /// not the server's intent, and a nameless sentence still carries the numbers a damage meter
    /// needs. The reference degrades the same way through `GetObjectName`'s `"UKNOWNOBJECT"` tail.
    fn spell_name(&self, spell_id: u32) -> Option<String> {
        let Some(display) = self.spells.and_then(|s| s.catalog.get(spell_id)) else {
            return Some(String::new());
        };
        const DO_NOT_DISPLAY_OR_LOG: u32 = 0x180;
        if display.attributes & DO_NOT_DISPLAY_OR_LOG != 0 || display.name.is_empty() {
            return None;
        }
        Some(display.name.clone())
    }
}

/// `SMSG_ATTACKERSTATEUPDATE` → the melee line. The whole `COMBAT_*` half of the block comes from
/// this one packet.
///
/// `school` is the swing's first sub-damage school, which is what selects the `…SCHOOL` template
/// ("You hit X for 5 fire damage." against "You hit X for 5."). Physical (0) takes the plain form.
pub(super) fn attacker_state(
    s: AttackerState,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let attacker = ctx.classify(s.attacker, stores);
    let victim = ctx.classify(s.victim, stores);
    let family = combat::melee_family(s.hit_info, s.victim_state, s.damage, s.school);
    // A swing is a MISS line only for the outcomes the reference words as one; everything the
    // victim did about it (dodge, parry, block, …) is still a "misses" chat TYPE, because the type
    // pair is hits/misses and only a landed hit is "hits".
    let landed = family.stem.starts_with("COMBATHIT");
    let Some(kind) = combat::combat_kind(attacker, victim, !landed) else {
        return;
    };
    let fills = Fills {
        spell: String::new(),
        school: (s.school != 0).then_some(s.school),
        amount: i64::from(s.damage),
        ..Default::default()
    };
    queue(
        log,
        ctx,
        poses,
        kind,
        family,
        (s.attacker, attacker),
        (s.victim, victim),
        fills,
    );
}

/// `SMSG_SPELLNONMELEEDAMAGELOG` → a spell's damage line, or the fully-absorbed / fully-resisted
/// wording when nothing got through.
///
/// The packet's own `periodic` flag routes the line to the `SPELL_PERIODIC_*` types and the
/// `PERIODICAURADAMAGE` wording — a DoT tick the server chose to report here rather than through
/// `SMSG_PERIODICAURALOG` is still a periodic tick, and the reference tells them apart the same way.
pub(super) fn spell_damage_log(
    s: SpellDamageLog,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let attacker = ctx.classify(s.attacker, stores);
    let victim = ctx.classify(s.target, stores);
    // `SPELL_HIT_TYPE_CRIT` (vmangos `SpellDefines.h:179`) — the same bit the floating text reads.
    let crit = s.hit_info & 0x2 != 0;
    let Some(spell) = ctx.spell_name(s.spell_id) else {
        return;
    };

    if s.periodic {
        let Some(kind) = combat::periodic_kind(attacker, false) else {
            return;
        };
        let fills = Fills {
            spell,
            school: Some(s.school),
            amount: i64::from(s.damage),
            ..Default::default()
        };
        return queue(
            log,
            ctx,
            poses,
            kind,
            combat::PERIODICAURADAMAGE,
            (s.attacker, attacker),
            (s.target, victim),
            fills,
        );
    }

    let Some(kind) = combat::spell_kind(attacker, victim, false) else {
        return;
    };
    // Nothing through: the reference words it as the reason rather than as a zero.
    let family = if s.damage == 0 && s.absorb > 0 {
        combat::SPELLLOGABSORB
    } else if s.damage == 0 && s.resist > 0 {
        combat::SPELLRESIST
    } else {
        match (crit, s.school != 0) {
            (false, false) => combat::SPELLLOG,
            (true, false) => combat::SPELLLOGCRIT,
            (false, true) => combat::SPELLLOGSCHOOL,
            (true, true) => combat::SPELLLOGCRITSCHOOL,
        }
    };
    let fills = Fills {
        spell,
        school: Some(s.school),
        amount: i64::from(s.damage),
        ..Default::default()
    };
    queue(
        log,
        ctx,
        poses,
        kind,
        family,
        (s.attacker, attacker),
        (s.target, victim),
        fills,
    );
}

/// `SMSG_SPELLLOGMISS` → one line per target a cast failed to land on, each worded by its own
/// `SpellMissInfo`.
///
/// The packet is a *list*, and each entry is its own sentence with its own victim — so the chat
/// type is recomputed per entry, not once for the cast.
pub(super) fn spell_log_miss(
    s: &SpellLogMiss,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let attacker = ctx.classify(s.caster, stores);
    let Some(spell) = ctx.spell_name(s.spell_id) else {
        return;
    };
    // **Every line off this packet is typed as a DAMAGE-SHIELD line**, and that is not a guess:
    // the `SMSG_SPELLLOGMISS` caller passes the formatter's 5th argument as 1 (`0x5e7f31 push 1`),
    // which routes `0x62bc5a` to `0x62c140` — the damage-shield two-way selector — instead of the
    // usual eight-row spell matrix. Its five other callers pass 0 and take the normal path.
    //
    // Whether that is deliberate or a 1.12 bug is **not derivable from the binary**, and wow-re
    // says so rather than guessing. We reproduce the behaviour, because an addon filtering on
    // chat type has to see what the reference shows; the discriminator, if anyone wants it, is a
    // live cast that misses and a look at which ChatFrame filter catches the line. 1571 sent these
    // through `spell_kind` and had them land in `SPELL_*_DAMAGE`.
    let kind = if matches!(attacker, UnitClass::Me | UnitClass::MyPet) {
        crate::ui_chat::ChatEventKind::SpellDamageShieldsOnSelf
    } else {
        crate::ui_chat::ChatEventKind::SpellDamageShieldsOnOthers
    };
    for &(target, miss_info) in &s.misses {
        let family = combat::miss_family(miss_info);
        let victim = ctx.classify(target, stores);
        let fills = Fills {
            spell: spell.clone(),
            ..Default::default()
        };
        queue(
            log,
            ctx,
            poses,
            kind,
            family,
            (s.caster, attacker),
            (target, victim),
            fills,
        );
    }
}

/// `SMSG_SPELLHEALLOG` → a heal line. A heal is a **BUFF** type, not a damage one.
pub(super) fn spell_heal_log(
    s: SpellHealLog,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let healer = ctx.classify(s.healer, stores);
    let target = ctx.classify(s.target, stores);
    let Some(kind) = combat::spell_kind(healer, target, true) else {
        return;
    };
    let family = if s.critical {
        combat::HEALEDCRIT
    } else {
        combat::HEALED
    };
    let Some(spell) = ctx.spell_name(s.spell_id) else {
        return;
    };
    let fills = Fills {
        spell,
        amount: i64::from(s.amount),
        ..Default::default()
    };
    queue(
        log,
        ctx,
        poses,
        kind,
        family,
        (s.healer, healer),
        (s.target, target),
        fills,
    );
}

/// `SMSG_SPELLENERGIZELOG` → a power-gain line, also a BUFF type.
pub(super) fn spell_energize_log(
    s: SpellEnergizeLog,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let caster = ctx.classify(s.caster, stores);
    let target = ctx.classify(s.target, stores);
    let Some(kind) = combat::spell_kind(caster, target, true) else {
        return;
    };
    let Some(spell) = ctx.spell_name(s.spell_id) else {
        return;
    };
    let fills = Fills {
        spell,
        power: Some(s.power),
        amount: i64::from(s.amount),
        ..Default::default()
    };
    queue(
        log,
        ctx,
        poses,
        kind,
        combat::POWERGAIN,
        (s.caster, caster),
        (s.target, target),
        fills,
    );
}

/// `SMSG_PERIODICAURALOG` → one line per tick, worded by the tick's aura type.
///
/// The periodic chat types split DAMAGE from BUFFS and **ignore the victim entirely** (their two
/// selectors take one argument), which is why the kind is picked from `caster` alone here.
pub(super) fn periodic_aura_log(
    s: &PeriodicAuraLog,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let caster = ctx.classify(s.caster, stores);
    let target = ctx.classify(s.target, stores);
    let Some(spell) = ctx.spell_name(s.spell_id) else {
        return;
    };
    for tick in &s.ticks {
        let (family, buff, fills) = match *tick {
            PeriodicTick::Damage { amount, school, .. } => (
                combat::PERIODICAURADAMAGE,
                false,
                Fills {
                    spell: spell.clone(),
                    // The periodic packet's school is a full u32; the template's slot is the same
                    // `SPELL_SCHOOL<n>_NAME` index the direct-damage one takes.
                    school: u8::try_from(school).ok(),
                    amount: i64::from(amount),
                    ..Default::default()
                },
            ),
            PeriodicTick::Heal { amount } => (
                combat::PERIODICAURAHEAL,
                true,
                Fills {
                    spell: spell.clone(),
                    amount: i64::from(amount),
                    ..Default::default()
                },
            ),
            PeriodicTick::Energize { power, amount } => (
                combat::POWERGAIN,
                true,
                Fills {
                    spell: spell.clone(),
                    power: Some(power),
                    amount: i64::from(amount),
                    ..Default::default()
                },
            ),
            // A mana leech is one sentence about two transfers: what the victim lost and what the
            // caster gained. vmangos sends the drained amount and a multiplier, not the gain, so
            // the gained figure is `amount * multiplier` — the same product the server applies.
            PeriodicTick::ManaLeech {
                power,
                amount,
                multiplier,
            } => (
                combat::SPELLPOWERLEECH,
                true,
                Fills {
                    spell: spell.clone(),
                    power: Some(power),
                    amount: i64::from(amount),
                    amount2: (f64::from(amount) * f64::from(multiplier)) as i64,
                    power2: Some(power),
                    ..Default::default()
                },
            ),
        };
        let Some(kind) = combat::periodic_kind(caster, buff) else {
            continue;
        };
        queue(
            log,
            ctx,
            poses,
            kind,
            family,
            (s.caster, caster),
            (s.target, target),
            fills,
        );
    }
}

/// `SMSG_SPELLDAMAGESHIELD` → the Thorns-style return hit.
///
/// **The sentence's subject is the packet's `victim`.** The wire names the fields from the original
/// swing's point of view — `victim` wears the shield, `attacker` struck them and now takes the
/// damage back — while `DAMAGESHIELDSELFOTHER` is "You reflect %d %s damage to %s.", whose subject
/// is the reflector. Getting this backwards would name both endpoints in the wrong halves of every
/// line, and nothing downstream would notice.
///
/// The chat type is a plain two-way split on the **bearer**, not the usual eight-row matrix:
/// `0x62c140` returns `0x3f` (`SPELL_DAMAGESHIELDS_ON_SELF`) for class 0 or 1 and `0x40`
/// (`…ON_OTHERS`) for everything else — "damage shields on self" meaning the shield that is on you.
pub(super) fn damage_shield(
    s: DamageShield,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let bearer = ctx.classify(s.victim, stores);
    let struck = ctx.classify(s.attacker, stores);
    let kind = if matches!(bearer, UnitClass::Me | UnitClass::MyPet) {
        crate::ui_chat::ChatEventKind::SpellDamageShieldsOnSelf
    } else {
        crate::ui_chat::ChatEventKind::SpellDamageShieldsOnOthers
    };
    let fills = Fills {
        school: u8::try_from(s.school).ok(),
        amount: i64::from(s.damage),
        ..Default::default()
    };
    queue(
        log,
        ctx,
        poses,
        kind,
        combat::DAMAGESHIELD,
        (s.victim, bearer),
        (s.attacker, struck),
        fills,
    );
}

/// The one tail every arm ends in: build the queued line (dropping a class-9 endpoint) and park it
/// for its names.
#[allow(clippy::too_many_arguments)] // the tail's args ARE the line: sink, context, and the five
                                     // facts a queued line is made of. Bundling any of them would only move the list somewhere else.
fn queue(
    log: &mut ChatLog,
    ctx: &ChatCtx,
    poses: &Query<&mut Transform>,
    kind: crate::ui_chat::ChatEventKind,
    family: Family,
    subject: (u64, UnitClass),
    object: (u64, UnitClass),
    fills: Fills,
) {
    // **The gate is an OR over the two endpoints, not an AND** — the §5 verdict's own wording:
    // `dist²(player, src) < range(srcClass)²` **OR** `dist²(player, tgt) < range(tgtClass)²`. 1571
    // required both and therefore dropped lines the client shows: your own pet (range 100000)
    // fighting something 80 yards away is logged by the reference and was silent here.
    if !ctx.in_range(subject.0, subject.1, poses) && !ctx.in_range(object.0, object.1, poses) {
        return;
    }
    if let Some(line) = combat::queue(kind, family, subject, object, fills) {
        log.push_combat(line);
    }
}
