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
    AttackerState, DamageShield, DispelFailed, EnchantmentLog, EnvironmentalDamageLog,
    PartyKillLog, PeriodicAuraLog, PeriodicTick, SpellDamageLog, SpellDispelLog, SpellEnergizeLog,
    SpellHealLog, SpellInstaKillLog, SpellLogExecute, SpellLogMiss, SpellOutcomeLog,
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

    /// One endpoint's half of the display-range gate — the law is
    /// [`combat::in_range`]; this only supplies the context it reads.
    fn in_range(&self, guid: u64, class: UnitClass, poses: &Query<&mut Transform>) -> bool {
        combat::in_range(guid, class, self.self_guid, self.index, poses)
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
        // **Only the landed-hit family grows a trailer**, and it is the only family in the whole
        // block that can show GLANCING/CRUSHING/BLOCK: `0x628410`'s four call sites, and this is
        // the one that passes a real `blocked` and a real `HitInfo` (§4.2). A dodge or a full
        // absorb prints no amount at all — the reference does not append to those lines.
        trailers: landed.then_some(combat::Trailers {
            absorbed: s.absorb,
            resisted: s.resist,
            blocked: s.blocked,
            hit_info: s.hit_info,
        }),
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

    // **Split damage takes the packet before anything else does** (`hit_info & 8` → `0x62de60`,
    // the handler's FIRST test at `0x5e8...`, before the periodic split): a Soul Link-style share reads "%s's
    // %s causes %s %d damage.", never "hits %s for %d". It has no `…SELFSELF` key and it grows no
    // trailers.
    const SPELL_HIT_TYPE_SPLIT: u32 = 0x8;
    if s.hit_info & SPELL_HIT_TYPE_SPLIT != 0 {
        let Some(kind) = combat::spell_kind(attacker, victim, false) else {
            return;
        };
        return queue(
            log,
            ctx,
            poses,
            kind,
            combat::SPELLSPLITDAMAGE,
            (s.attacker, attacker),
            (s.target, victim),
            Fills {
                spell: spell.clone(),
                amount: i64::from(s.damage),
                ..Default::default()
            },
        );
    }
    if s.periodic {
        let Some(kind) = combat::periodic_kind(attacker, false) else {
            return;
        };
        let fills = Fills {
            spell,
            school: Some(s.school),
            amount: i64::from(s.damage),
            // `0x628341`, the periodic call site: absorb and resist only — it passes zero for
            // blocked and for HitInfo, so a periodic line can never say "(blocked)" or
            // "(crushing)".
            trailers: Some(combat::Trailers {
                absorbed: s.absorb,
                resisted: s.resist,
                blocked: 0,
                hit_info: 0,
            }),
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
    // Nothing through: the reference words it as the reason rather than as a zero, and the order
    // of the three tests is its own — absorb, then BLOCK, then resist (§4.3).
    let family = if s.damage == 0 && s.absorb > 0 {
        combat::SPELLLOGABSORB
    } else if s.damage == 0 && s.blocked > 0 {
        combat::SPELLBLOCKED
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
    // `0x62d03c`, the spell call site: it passes a real `blocked` but **HitInfo 0**, so a spell
    // line can show RESIST/VULNERABLE/BLOCK/ABSORB and never GLANCING or CRUSHING. Only the
    // families that actually print a damage number take one — the reference appends nothing to
    // the "was absorbed / was blocked / was resisted" wordings, which name the reason instead.
    let landed = family.stem.starts_with("SPELLLOG") && family.stem != "SPELLLOGABSORB";
    let fills = Fills {
        spell,
        school: Some(s.school),
        amount: i64::from(s.damage),
        trailers: landed.then_some(combat::Trailers {
            absorbed: s.absorb,
            resisted: s.resist,
            blocked: s.blocked,
            hit_info: 0,
        }),
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
    let kind = combat::damage_shield_kind(attacker);
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
    let kind = combat::damage_shield_kind(bearer);
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

/// `SMSG_PARTYKILLLOG` → "You have slain %s!" / "%s is slain by %s!".
///
/// **Only two killer classes produce a line at all** (`0x628890`, which has no selector): class 0
/// is `SELFKILLOTHER`, class 2 (a party member) is `PARTYKILLOTHER`, and **everything else —
/// including your own pet at class 1 — emits nothing**. That is the reference's own shape, not a
/// simplification: a pet's kill is announced by the plain `UNITDIES*` line instead.
///
/// The chat type comes off the **victim**, through the death selector.
pub(super) fn party_kill_log(
    s: PartyKillLog,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let killer = ctx.classify(s.killer, stores);
    let victim = ctx.classify(s.victim, stores);
    let family = match killer {
        UnitClass::Me => combat::SELFKILLOTHER,
        UnitClass::Party => combat::PARTYKILLOTHER,
        _ => return,
    };
    queue(
        log,
        ctx,
        poses,
        combat::death_kind(victim),
        family,
        (s.victim, victim),
        (s.killer, killer),
        Fills::default(),
    );
}

/// `SMSG_SPELLINSTAKILLLOG` → "You are killed by %s." / "%s is killed by %s."
///
/// `0x62cbe0` calls the spell-damage selector with the victim's class in **both** positions
/// (`0x626be0(class, class)`), so the type never splits on a second endpoint — there isn't one.
pub(super) fn spell_insta_kill_log(
    s: SpellInstaKillLog,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let victim = ctx.classify(s.victim, stores);
    let Some(spell) = ctx.spell_name(s.spell_id) else {
        return;
    };
    let Some(kind) = combat::spell_kind(victim, victim, false) else {
        return;
    };
    queue(
        log,
        ctx,
        poses,
        kind,
        combat::INSTAKILL,
        (s.victim, victim),
        (s.victim, victim),
        Fills {
            spell,
            ..Default::default()
        },
    );
}

/// `SMSG_PROCRESIST` → "%s resists %s's %s." and `SMSG_SPELLORDAMAGE_IMMUNE` → "%s is immune to
/// %s's %s." — one body, two sentences.
///
/// Both word the TARGET first (the reference's convention B), and both take their chat type from
/// the (caster, target) pair — `PROCRESIST` always through the damage/buff stub `0x627d30`, and
/// `IMMUNESPELL` through the plain damage selector `0x626be0`. The difference is real: an immunity
/// to a *helpful* spell still files under `…_DAMAGE`.
///
/// `IMMUNESPELL`'s `log_format` byte is the reference's "is periodic" flag, and it is the only
/// thing `CombatLogPeriodicSpells` would gate here — a CVar we do not read live yet, so the flag
/// changes nothing today and is named rather than dropped.
pub(super) fn spell_outcome_log(
    s: SpellOutcomeLog,
    immune: bool,
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
    let Some(kind) = combat::spell_kind(caster, target, false) else {
        return;
    };
    queue(
        log,
        ctx,
        poses,
        kind,
        if immune {
            combat::IMMUNESPELL
        } else {
            combat::PROCRESIST
        },
        (s.caster, caster),
        (s.target, target),
        Fills {
            spell,
            ..Default::default()
        },
    );
}

/// `SMSG_SPELLDISPELLOG` → "Your %s is removed." / "%s's %s is removed.", one line per aura.
///
/// The chat type is **hard-coded `0x45` `SPELL_BREAK_AURA`** (`0x62d480`) — it consults no class at
/// all, which is why this is the one arm that never calls a selector. The dispeller is classified
/// only for the range gate; the sentence names the bearer and the aura and nothing else.
pub(super) fn spell_dispel_log(
    s: &SpellDispelLog,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let bearer = ctx.classify(s.victim, stores);
    let caster = ctx.classify(s.caster, stores);
    for &spell_id in &s.spell_ids {
        let Some(spell) = ctx.spell_name(spell_id) else {
            continue;
        };
        queue(
            log,
            ctx,
            poses,
            crate::ui_chat::ChatEventKind::SpellBreakAura,
            combat::AURADISPEL,
            (s.victim, bearer),
            (s.caster, caster),
            Fills {
                spell,
                ..Default::default()
            },
        );
    }
}

/// `SMSG_DISPEL_FAILED` → "You fail to dispel %s's %s.", one line per aura that would not come off.
///
/// **The reference picks the format-string variant ONCE, before the loop** (`0x628c20`), so every
/// line in a packet shares it — which is the same answer ours reaches, since both endpoints are the
/// same for the whole packet. The chat type is re-derived per line only because the msg-id stub
/// `0x627d30` consults the spell, and the spells differ.
pub(super) fn dispel_failed(
    s: &DispelFailed,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let caster = ctx.classify(s.caster, stores);
    let victim = ctx.classify(s.victim, stores);
    let Some(kind) = combat::spell_kind(caster, victim, false) else {
        return;
    };
    for &spell_id in &s.spell_ids {
        let Some(spell) = ctx.spell_name(spell_id) else {
            continue;
        };
        queue(
            log,
            ctx,
            poses,
            kind,
            combat::DISPELFAILED,
            (s.caster, caster),
            (s.victim, victim),
            Fills {
                spell,
                ..Default::default()
            },
        );
    }
}

/// `SMSG_ENCHANTMENTLOG` → "You cast %s on your %s." / "%s has faded from your %s."
///
/// **An empty caster guid is how the server says the enchant FADED** (vmangos's own comment on the
/// field, and the reference's two-way at `0x628f40`); the fade names only the owner, which is why
/// it drops from four keys to two. The item name is the last `%s` in every variant and comes from
/// the item cache, so a line whose entry is not cached yet waits rather than printing a hole.
///
/// The chat type is the literal `0x44` `SPELL_ITEM_ENCHANTMENTS` on the fade leg and on the ADD leg
/// when `show_affiliation` is clear — the copy the server sends to the item's own owner. The
/// broadcast copy (affiliation set) takes the BUFF selector instead, which is what puts a
/// bystander's enchant into their `SPELL_*_BUFF` bucket rather than the item block.
pub(super) fn enchantment_log(
    s: EnchantmentLog,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let owner = ctx.classify(s.owner, stores);
    let caster = ctx.classify(s.caster, stores);
    let Some(spell) = ctx.spell_name(s.spell_id) else {
        return;
    };
    let fills = Fills {
        spell,
        ..Default::default()
    };
    let enchantments = crate::ui_chat::ChatEventKind::SpellItemEnchantments;
    if s.caster == 0 {
        // A fade: the owner is the only endpoint, in both the key and the range gate.
        return queue_named(
            log,
            ctx,
            poses,
            enchantments,
            combat::ITEMENCHANTMENTREMOVE,
            (s.owner, owner),
            (s.owner, owner),
            fills,
            combat::Named::Item(s.item_entry),
        );
    }
    let kind = if s.show_affiliation {
        match combat::spell_kind(caster, owner, true) {
            Some(k) => k,
            None => return,
        }
    } else {
        enchantments
    };
    queue_named(
        log,
        ctx,
        poses,
        kind,
        combat::ITEMENCHANTMENTADD,
        (s.caster, caster),
        (s.owner, owner),
        fills,
        combat::Named::Item(s.item_entry),
    );
}

/// `SMSG_SPELLLOGEXECUTE` → the lines a cast's *effects* produce, as opposed to the damage it
/// dealt: what it created, fed, interrupted, drained, dismissed or damaged.
///
/// **One packet, many formatters.** The wire is a list of groups keyed by spell-effect id, and the
/// reference's own per-effect jump table (`0x5e8074`) sends each to a different formatter with a
/// different family, a different chat type and a different argument convention. So this arm is a
/// switch, not a sentence — the seven effects below are the ones that word themselves.
///
/// **Deliberately not wired, and named rather than dropped** (decision 1703): effects 33/59
/// `OPEN_LOCK` (`OPEN_LOCK_{SELF,OTHER}` — its trailing `%s` is a *gameobject* name, and benilla
/// has no GO-name cache to resolve one from a guid), and the `SIMPLECAST*`/`SIMPLEPERFORM*`/
/// `SPELLTERSE_*` catch-all the reference falls back to for the guid-only tail of the switch (its
/// bail conditions read `AttributesEx4` and the spell's three `Effect` columns, and `Spell.dbc`
/// column 10 is not parsed into `SpellDisplay` yet). Both are additions, not corrections: nothing
/// below changes when they land.
pub(super) fn spell_log_execute(
    s: &SpellLogExecute,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    use benilla_protocol::messages::ExecuteLog as E;

    let caster = ctx.classify(s.caster, stores);
    let Some(spell) = ctx.spell_name(s.spell_id) else {
        return;
    };
    let spell_fill = || Fills {
        spell: spell.clone(),
        ..Default::default()
    };
    for (_effect, rows) in &s.effects {
        for row in rows {
            match *row {
                // Effect 8 POWER_DRAIN. Three families come out of this one row: happiness has no
                // `…_POINTS` noun so it gets its own sentence at the misc-info type, a real leech
                // multiplier says the caster GAINED what the target lost, and a zero one is a plain
                // drain. `drained == 0` drops the line — the reference's own test.
                E::PowerDrain {
                    target,
                    amount,
                    power,
                    multiplier,
                } => {
                    let victim = ctx.classify(target, stores);
                    let div = power_divisor(power);
                    let drained = i64::from(amount) / div;
                    if drained == 0 {
                        continue;
                    }
                    if power == POWER_HAPPINESS {
                        // `0x627de0`: the subject is the pet's OWNER and the named thing is the
                        // pet, at the literal misc-info type.
                        let Some(owner) = unit_owner(target, ctx, stores) else {
                            continue;
                        };
                        let owner_class = ctx.classify(owner, stores);
                        queue_named(
                            log,
                            ctx,
                            poses,
                            crate::ui_chat::ChatEventKind::CombatMiscInfo,
                            combat::SPELLHAPPINESSDRAIN,
                            (owner, owner_class),
                            (target, victim),
                            Fills {
                                amount: drained,
                                ..Default::default()
                            },
                            combat::Named::Unit(target),
                        );
                        continue;
                    }
                    let Some(kind) = combat::spell_kind(caster, victim, false) else {
                        continue;
                    };
                    // `|multiplier| >= 2^-22` is the reference's own epsilon (`[0x8029d4]`), not a
                    // round number of ours.
                    let leech = multiplier.abs() >= LEECH_EPSILON;
                    let gained = ((f64::from(amount) * f64::from(multiplier)) as i64) / div;
                    queue(
                        log,
                        ctx,
                        poses,
                        kind,
                        if leech {
                            combat::SPELLPOWERLEECH
                        } else {
                            combat::SPELLPOWERDRAIN
                        },
                        (s.caster, caster),
                        (target, victim),
                        Fills {
                            spell: spell.clone(),
                            power: Some(power),
                            power2: Some(power),
                            amount: drained,
                            amount2: gained,
                            ..Default::default()
                        },
                    );
                }
                // Effect 19 ADD_EXTRA_ATTACKS. **The caster guid is never passed** (`0x62d9f0`):
                // the sentence's only endpoint is the unit that gained the attacks, and the
                // singular form is the same key with `_SINGULAR` appended.
                E::ExtraAttacks { target, count } => {
                    let victim = ctx.classify(target, stores);
                    let Some(kind) = combat::spell_kind(victim, victim, false) else {
                        continue;
                    };
                    queue(
                        log,
                        ctx,
                        poses,
                        kind,
                        if count == 1 {
                            combat::SPELLEXTRAATTACKS_SINGULAR
                        } else {
                            combat::SPELLEXTRAATTACKS
                        },
                        (target, victim),
                        (target, victim),
                        Fills {
                            spell: spell.clone(),
                            amount: i64::from(count),
                            ..Default::default()
                        },
                    );
                }
                // Effect 24 CREATE_ITEM — the tradeskill line, at the literal `0x3e`. No target
                // guid on the wire: the two-way is on the CASTER being you.
                E::CreateItem { item_entry } => queue_named(
                    log,
                    ctx,
                    poses,
                    crate::ui_chat::ChatEventKind::SpellTradeskills,
                    combat::TRADESKILL_LOG,
                    (s.caster, caster),
                    (s.caster, caster),
                    Fills::default(),
                    combat::Named::Item(item_entry),
                ),
                // Effect 101 FEED_PET — the same shape as CREATE_ITEM, a different sentence.
                E::FeedPet { item_entry } => queue_named(
                    log,
                    ctx,
                    poses,
                    crate::ui_chat::ChatEventKind::SpellTradeskills,
                    combat::FEEDPET_LOG,
                    (s.caster, caster),
                    (s.caster, caster),
                    Fills::default(),
                    combat::Named::Item(item_entry),
                ),
                // Effect 68 INTERRUPT_CAST. **The spell the sentence names is the INTERRUPTED
                // one**, off the row, not the interrupting cast off the packet header — which is
                // the whole point of the line.
                E::InterruptCast { target, spell_id } => {
                    let victim = ctx.classify(target, stores);
                    let Some(interrupted) = ctx.spell_name(spell_id) else {
                        continue;
                    };
                    let Some(kind) = combat::spell_kind(caster, victim, false) else {
                        continue;
                    };
                    queue(
                        log,
                        ctx,
                        poses,
                        kind,
                        combat::SPELLINTERRUPT,
                        (s.caster, caster),
                        (target, victim),
                        Fills {
                            spell: interrupted,
                            ..Default::default()
                        },
                    );
                }
                // Effect 111 DURABILITY_DAMAGE. Both fields `-1` is the "all items" form, which
                // has its own family and names no item at all.
                E::DurabilityDamage {
                    target,
                    item_entry,
                    slot,
                } => {
                    let victim = ctx.classify(target, stores);
                    let Some(kind) = combat::spell_kind(caster, victim, false) else {
                        continue;
                    };
                    if item_entry < 0 && slot < 0 {
                        queue(
                            log,
                            ctx,
                            poses,
                            kind,
                            combat::SPELLDURABILITYDAMAGEALL,
                            (s.caster, caster),
                            (target, victim),
                            spell_fill(),
                        );
                        continue;
                    }
                    let Ok(entry) = u32::try_from(item_entry) else {
                        continue;
                    };
                    queue_named(
                        log,
                        ctx,
                        poses,
                        kind,
                        combat::SPELLDURABILITYDAMAGE,
                        (s.caster, caster),
                        (target, victim),
                        spell_fill(),
                        combat::Named::Item(entry),
                    );
                }
                // Effect 102 DISMISS_PET arrives in the guid-only tail; the reference's two-way is
                // on the caster being you, at the literal misc-info type, and the named thing is
                // the pet.
                E::Target { target } if *_effect == EFFECT_DISMISS_PET => {
                    let pet = ctx.classify(target, stores);
                    queue_named(
                        log,
                        ctx,
                        poses,
                        crate::ui_chat::ChatEventKind::CombatMiscInfo,
                        combat::SPELLDISMISSPET,
                        (s.caster, caster),
                        (target, pet),
                        Fills::default(),
                        combat::Named::Unit(target),
                    );
                }
                // Heals and energizes off this packet are the floating text's business, not the
                // chat log's: the reference words them from SMSG_SPELLHEALLOG / SPELLENERGIZELOG,
                // which arrive separately and already have arms. The rest of the guid-only tail is
                // the SIMPLECAST catch-all named in this function's docs.
                E::Heal { .. } | E::Energize { .. } | E::Target { .. } => {}
            }
        }
    }
}

/// vmangos `Powers`: happiness, the one power with no `…_POINTS` GlobalString — `0x6278f0` returns
/// NULL for it, which is why it takes its own family instead of a `POWERGAIN` row.
const POWER_HAPPINESS: u32 = 4;

/// vmangos `SpellEffects::SPELL_EFFECT_DISMISS_PET`.
const EFFECT_DISMISS_PET: u32 = 102;

/// `|multiplier| >= 2^-22` — the reference's own leech/drain discriminator (`[0x8029d4]`).
const LEECH_EPSILON: f32 = 1.0 / 4_194_304.0;

/// `0x6e7130(powerType)` — the divisor a power's log amounts are reported in
/// (`[powerType*4 + 0x86f978]` = `{1, 10, 1, 1, 1000}`). Rage is stored ×10 and happiness ×1000.
fn power_divisor(power: u32) -> i64 {
    match power {
        1 => 10,
        POWER_HAPPINESS => 1000,
        _ => 1,
    }
}

/// A unit's owner guid — `CHARMEDBY` first, then `CREATEDBY`, the same pair
/// [`combat::classify`] reads. `None` when the unit is not streamed or owns itself.
fn unit_owner(guid: u64, ctx: &ChatCtx, stores: &Query<&mut ObjectStore>) -> Option<u64> {
    let entity = ctx.index.0.get(&guid).copied()?;
    let store = stores.get(entity).ok()?;
    store
        .0
        .unit_charmed_by()
        .or_else(|| store.0.unit_created_by())
}

/// `SMSG_ENVIRONMENTALDAMAGELOG` → "You fall and lose %d health." and its five siblings.
///
/// **The only family with no selector at all.** `0x62aac0` builds the key by `snprintf` over a
/// 6-entry damage-type table and a plain SELF/OTHER, and calls the melee HITS msg-id selector with
/// **`ecx = edx = victimClass`** — so a fall on a party member types as `HOSTILEPLAYER_HITS`
/// through that selector's tgt<=3 override, which looks wrong and is what the reference does.
///
/// It grows trailers (absorb/resist/vulnerability), which is how a resisted fire tick reads.
pub(super) fn environmental_damage_log(
    e: EnvironmentalDamageLog,
    ctx: &ChatCtx,
    stores: &Query<&mut ObjectStore>,
    poses: &Query<&mut Transform>,
    log: &mut ChatLog,
) {
    let victim = ctx.classify(e.victim, stores);
    let Some(family) = combat::env_family(e.damage_type) else {
        return;
    };
    let Some(kind) = combat::combat_kind(victim, victim, false) else {
        return;
    };
    queue(
        log,
        ctx,
        poses,
        kind,
        family,
        (e.victim, victim),
        (e.victim, victim),
        Fills {
            amount: i64::from(e.damage),
            trailers: Some(combat::Trailers {
                absorbed: e.absorb,
                resisted: e.resist,
                // The environmental call site passes zero for both — it has no wire source for a
                // block or a HitInfo, so GLANCING/CRUSHING/BLOCK can never appear on these lines.
                blocked: 0,
                hit_info: 0,
            }),
            ..Default::default()
        },
    );
}

/// `SMSG_SET_FACTION_STANDING` → "Your %s reputation has increased by %d."
///
/// **The wire carries the new TOTAL, and the sentence wants the DELTA** — so this runs before the
/// store is overwritten, and a slot whose value did not actually move prints nothing (the
/// reference's own `0x62c5f0` guard: "only when the stored value actually changed").
///
/// It is one of the twelve formatters with **no range gate and no classifier** — the chat type is
/// the literal `0x55` `COMBAT_FACTION_CHANGE`, and the only participant is you — so it builds its
/// line directly rather than going through [`queue`], which exists to gate on two endpoints.
///
/// The wire's `reputationListId` is `Faction.dbc`'s `rep_index`, not a faction id; the name comes
/// off the row that carries that index.
pub(super) fn faction_standing(
    deltas: &[(u32, i32)],
    reputations: &Reputations,
    factions: Option<&crate::target::ring::Factions>,
    log: &mut ChatLog,
) {
    let Some(catalog) = factions.map(|f| f.catalog()) else {
        return;
    };
    for &(list_id, standing) in deltas {
        let old = reputations
            .0
            .get(list_id as usize)
            .map_or(0, |(_, standing)| *standing);
        let delta = standing - old;
        if delta == 0 {
            continue;
        }
        let Ok(index) = i32::try_from(list_id) else {
            continue;
        };
        let Some(name) = catalog
            .reputation_factions()
            .find(|(_, f)| f.rep_index == index)
            .and_then(|(id, _)| catalog.faction_name(id))
        else {
            continue;
        };
        log.push_combat(combat::PendingCombat {
            kind: crate::ui_chat::ChatEventKind::CombatFactionChange,
            family: if delta > 0 {
                combat::FACTION_STANDING_INCREASED
            } else {
                combat::FACTION_STANDING_DECREASED
            },
            // A `Single` family reads no variant, but the field is not optional; `OtherOther` is
            // the one `tests::variants_of` sweeps such a family with.
            variant: combat::Variant::OtherOther,
            subject: 0,
            object: 0,
            fills: Fills {
                named: name.to_string(),
                amount: i64::from(delta.abs()),
                ..Default::default()
            },
            named: combat::Named::Ready,
            tries: 0,
        });
    }
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
    queue_named(
        log,
        ctx,
        poses,
        kind,
        family,
        subject,
        object,
        fills,
        combat::Named::Ready,
    );
}

/// [`queue`] for a family whose `Named` slot still has to be looked up — an item entry or a unit
/// guid rides along and the drain resolves it, holding the line until it lands (§5.7).
#[allow(clippy::too_many_arguments)] // as [`queue`], plus the one key that defers the line
fn queue_named(
    log: &mut ChatLog,
    ctx: &ChatCtx,
    poses: &Query<&mut Transform>,
    kind: crate::ui_chat::ChatEventKind,
    family: Family,
    subject: (u64, UnitClass),
    object: (u64, UnitClass),
    fills: Fills,
    named: combat::Named,
) {
    // **The gate is an OR over the two endpoints, not an AND** — the §5 verdict's own wording:
    // `dist²(player, src) < range(srcClass)²` **OR** `dist²(player, tgt) < range(tgtClass)²`. 1571
    // required both and therefore dropped lines the client shows: your own pet (range 100000)
    // fighting something 80 yards away is logged by the reference and was silent here.
    if !ctx.in_range(subject.0, subject.1, poses) && !ctx.in_range(object.0, object.1, poses) {
        return;
    }
    if let Some(line) = combat::queue(kind, family, subject, object, fills, named) {
        log.push_combat(line);
    }
}
