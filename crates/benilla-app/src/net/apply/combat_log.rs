//! Combat-log SMSG arm bodies for [`super::apply_net_updates`]'s dispatch match — the floating
//! combat-text **spawn table** (decision 0137 phase 2; wow-re `worldtext-spawn-and-law.md`). One
//! `pub(super)` fn per arm, mirroring the client's handler → emitter structure: each classifies
//! the damage **source** (the color law's `K` — self / owned-by-me / anything else SUPPRESSED),
//! resolves the outcome **recipient**, applies Gate A (self-anchored damage text is
//! unconditionally suppressed — outgoing damage floats over the victim, incoming never floats
//! over you), picks the category + the effective color override, and writes a
//! [`CombatTextSpawn`]. The XP emitter is the one exception (self-anchored, no gates, its own
//! row color — by the client's design). The law itself — categories, words, colors, cap,
//! timing — lives in [`crate::combat_text`].
//!
//! Each spell arm ALSO feeds the **`UNIT_COMBAT` event** ([`UnitCombatFeedback`] — the portrait
//! hit indicator, decision 0576) — that channel is the worldtext's inverse: **ungated** (no
//! Gate A, no source class; it exists to show what anyone does to you), fired at packet receive.
//! The melee arm's twin rides the swing impact keyframe instead (`ui_unit::melee_unit_combat`).

use bevy::prelude::*;

use benilla_protocol::messages::{
    DamageShield, ExplorationXp, LevelUpInfo, PeriodicAuraLog, PeriodicTick, SpellDamageLog,
    SpellEnergizeLog, SpellHealLog, SpellLogMiss, XpGain,
};

use crate::combat_text::{damage_color, miss_word, spell_text, CombatTextSpawn, DamageSource};
use crate::names::NameCache;
use crate::ui_chat::ChatLog;
use crate::ui_unit::{CombatTextEvent, UnitCombatFeedback};

use super::super::{GuidIndex, NetCommands, ObjectStore, SelfGuid};

/// Gate A + anchor resolution: the recipient's entity, unless the recipient is us (`0x607140`/
/// `0x6128b0` compare the anchor guid to the active-player cache and return before submitting).
fn gated_anchor(guid: u64, index: &GuidIndex, self_guid: &SelfGuid) -> Option<Entity> {
    if self_guid.0 == Some(guid) {
        return None;
    }
    index.0.get(&guid).copied()
}

/// The `0x5efea0` source-ownership classifier (the color law's `K`): the damage SOURCE resolved
/// against the active player — `Player` (me), `Pet` (a unit whose Summoned/CreatedBy guid is me:
/// pet, guardian, totem), or `None` = every other class, which SUPPRESSES the emit entirely
/// (another unit's damage is never drawn; the real emitter returns before submitting).
pub(super) fn classify_source(
    guid: u64,
    index: &GuidIndex,
    self_guid: &SelfGuid,
    stores: &Query<&mut ObjectStore>,
) -> Option<DamageSource> {
    let me = self_guid.0?;
    if guid == me {
        return Some(DamageSource::Player);
    }
    let entity = index.0.get(&guid).copied()?;
    let store = stores.get(entity).ok()?;
    (store.0.unit_summoned_by() == Some(me) || store.0.unit_created_by() == Some(me))
        .then_some(DamageSource::Pet)
}

// NOTE: the MELEE number/word (`SMSG_ATTACKERSTATEUPDATE`) does not spawn here — it defers to the
// attacker swing clip's impact keyframe with the rest of the victim feedback
// (`crate::combat_text::melee_impact_text`, fed by `creature_anim::impact`).

/// The color law's `B` bit for a spell-packet emit (`0x6128b0`: `recordPtr == 0 ||
/// sign(byte[SpellRec+0x25])`): melee-styled (white) when the spell id resolves to no record —
/// the client's NULL-record half — or the record carries `AttributesEx3 & 0x8000`
/// ([`benilla_formats::SpellDisplay::melee_white_damage`] — how a ranged basic shot's
/// `SMSG_SPELLNONMELEEDAMAGELOG` floats white; decision 0376). No catalog at all degrades the
/// same way the client degrades with no record: melee-styled.
fn melee_styled(spells: Option<&crate::ui_action::Spells>, spell_id: u32) -> bool {
    let Some(spells) = spells else { return true };
    spells
        .catalog
        .get(spell_id)
        .is_none_or(|d| d.melee_white_damage())
}

/// The spell arms' shared `UNIT_COMBAT` split: a landed amount is `WOUND` (`CRITICAL` descriptor
/// on crit), zero damage degrades to the full-`ABSORB`/`RESIST` descriptor, else nothing (a clean
/// spell miss arrives via `SMSG_SPELLLOGMISS` instead). PROVISIONAL string mapping pending the
/// wow-re UNIT_COMBAT emission pin (decision 0576).
fn spell_feedback(
    damage: u32,
    absorb: u32,
    resist: i32,
    crit: bool,
) -> Option<(&'static str, &'static str)> {
    if damage > 0 {
        Some(("WOUND", if crit { "CRITICAL" } else { "" }))
    } else if absorb > 0 {
        Some(("WOUND", "ABSORB"))
    } else if resist > 0 {
        Some(("WOUND", "RESIST"))
    } else {
        None
    }
}

/// Spell packet → the center text's messageType (§5-verified against the emission law, wow-re
/// `combat-text-update-emission-law.md`): landed damage is ALWAYS `SPELL_DAMAGE` — **a spell
/// crit does NOT fire DAMAGE_CRIT** (no crit-distinct type exists for spell damage; the `0x62cd80`
/// emitter fires 29 regardless, crit changes only the chat template) — zero damage the
/// `SPELL_ABSORBED`/`SPELL_RESISTED` word. Spell-side partial trailers stay a named residual (the
/// verdict pinned the melee helper-B partials; the spell emitter's partial path is unread).
fn spell_center_text(
    damage: u32,
    absorb: u32,
    resist: i32,
) -> Option<(&'static str, Option<String>)> {
    if damage > 0 {
        Some(("SPELL_DAMAGE", Some(damage.to_string())))
    } else if absorb > 0 {
        Some(("SPELL_ABSORBED", None))
    } else if resist > 0 {
        Some(("SPELL_RESISTED", None))
    } else {
        None
    }
}

/// Melee packet → the center text's messageType + args (§5-verified emission shape, wow-re
/// `combat-text-update-emission-law.md`; fired **synchronously at packet receive** — the
/// impact-keyframe deferral belongs to the worldtext/UNIT_COMBAT victim dispatch, NOT this Lua
/// event). Helper-B partials CONFIRMED: a landed hit with a partial block/absorb/resist fires
/// the word type with `(damage, partial)` — the addon renders `"25 (10 blocked)"`. The
/// partial-vs-crit precedence when both apply is unpinned — partials win here, flagged in 0580.
pub(super) fn melee_center_text(
    hit_info: u32,
    victim_state: u32,
    damage: u32,
    absorb: u32,
    resist: i32,
    blocked: u32,
) -> Option<(&'static str, Option<String>, Option<String>)> {
    match victim_state {
        2 => Some(("DODGE", None, None)),
        3 => Some(("PARRY", None, None)),
        5 => Some(("BLOCK", None, None)),
        6 => Some(("EVADE", None, None)),
        7 => Some(("IMMUNE", None, None)),
        8 => Some(("DEFLECT", None, None)),
        _ => {
            if damage > 0 {
                if absorb > 0 {
                    Some(("ABSORB", Some(damage.to_string()), Some(absorb.to_string())))
                } else if blocked > 0 {
                    Some(("BLOCK", Some(damage.to_string()), Some(blocked.to_string())))
                } else if resist > 0 {
                    Some(("RESIST", Some(damage.to_string()), Some(resist.to_string())))
                } else if hit_info & 0x80 != 0 {
                    Some(("DAMAGE_CRIT", Some(damage.to_string()), None))
                } else {
                    Some(("DAMAGE", Some(damage.to_string()), None))
                }
            } else if hit_info & 0x20 != 0 {
                Some(("ABSORB", None, None))
            } else if hit_info & 0x40 != 0 {
                Some(("RESIST", None, None))
            } else {
                Some(("MISS", None, None))
            }
        }
    }
}

/// vmangos `Powers` id → the center text's power-gain messageType (`MANA`/`RAGE`/`FOCUS`/
/// `ENERGY`; happiness has no message type — the addon shows nothing for it).
fn power_message_type(power: u32) -> Option<&'static str> {
    Some(match power {
        0 => "MANA",
        1 => "RAGE",
        2 => "FOCUS",
        3 => "ENERGY",
        _ => return None,
    })
}

/// `SMSG_SPELLNONMELEEDAMAGELOG` → the spell-damage number (crit on `SPELL_HIT_TYPE_CRIT 0x2`) or
/// the zero-damage ABSORB/RESIST word, over the target (`0x5e85e0`). Source-classified: only my
/// (gold) or my pet's (gold, PetSpellDamage-gated) spells draw. The handler pushes the same
/// record/source to number and word twin alike, so the override colors both.
#[allow(clippy::too_many_arguments)] // one dispatch arm's full writer set
pub(super) fn spell_damage_log(
    s: SpellDamageLog,
    index: &GuidIndex,
    self_guid: &SelfGuid,
    stores: &Query<&mut ObjectStore>,
    spells: Option<&crate::ui_action::Spells>,
    text: &mut MessageWriter<CombatTextSpawn>,
    feedback: &mut MessageWriter<UnitCombatFeedback>,
    center: &mut MessageWriter<CombatTextEvent>,
) {
    if benilla_assets::trace::enabled() {
        benilla_assets::trace::line(
            "fct",
            &format!(
                "recv spelldmg atk={:#x} target={:#x} dmg={} crit={}",
                s.attacker,
                s.target,
                s.damage,
                s.hit_info & 0x2 != 0
            ),
        );
    }
    // The ungated UNIT_COMBAT feed (portrait hit indicator): any source, any recipient — self
    // included — before the worldtext path's source/Gate-A returns below.
    if let (Some(&unit), Some((action, flags))) = (
        index.0.get(&s.target),
        spell_feedback(s.damage, s.absorb, s.resist, s.hit_info & 0x2 != 0),
    ) {
        feedback.write(UnitCombatFeedback {
            unit,
            action,
            flags,
            amount: s.damage,
            school: u32::from(s.school),
        });
    }
    // The center combat text (decision 0578): self recipient only.
    if self_guid.0 == Some(s.target) {
        if let Some((message_type, data)) = spell_center_text(s.damage, s.absorb, s.resist) {
            center.write(CombatTextEvent {
                message_type,
                data,
                extra: None,
            });
        }
    }
    let Some(source) = classify_source(s.attacker, index, self_guid, stores) else {
        return; // K = other: never drawn
    };
    // B = the melee-styled bit: a ranged basic shot (Throw/Auto Shot — AttributesEx3 & 0x8000)
    // floats WHITE off this spell packet, exactly like the client (decision 0376).
    let Some(color) = damage_color(source, melee_styled(spells, s.spell_id)) else {
        return; // the CombatDamage / PetSpellDamage gates
    };
    if let (Some(anchor), Some((category, body))) = (
        gated_anchor(s.target, index, self_guid),
        spell_text(s.damage, s.absorb, s.resist, s.hit_info & 0x2 != 0),
    ) {
        text.write(CombatTextSpawn {
            anchor,
            text: body,
            category,
            color,
        });
    }
}

/// `SMSG_PERIODICAURALOG` → DoT ticks float like direct damage (`0x626dd0`, never crit-category);
/// heal/energize/leech ticks float **nothing** (heals never float in 5875).
#[allow(clippy::too_many_arguments)] // one dispatch arm's full writer set
pub(super) fn periodic_aura_log(
    s: PeriodicAuraLog,
    index: &GuidIndex,
    self_guid: &SelfGuid,
    stores: &Query<&mut ObjectStore>,
    spells: Option<&crate::ui_action::Spells>,
    text: &mut MessageWriter<CombatTextSpawn>,
    feedback: &mut MessageWriter<UnitCombatFeedback>,
    center: &mut MessageWriter<CombatTextEvent>,
    names: &mut NameCache,
    net: &NetCommands,
) {
    if benilla_assets::trace::enabled() {
        benilla_assets::trace::line(
            "fct",
            &format!(
                "recv periodic caster={:#x} target={:#x} ticks={}",
                s.caster,
                s.target,
                s.ticks.len()
            ),
        );
    }
    // The center combat text (decision 0578): self recipient only — damage ticks like direct
    // damage, heal ticks PERIODIC_HEAL (arg2 = the caster's name, arg3 = amount; the name only
    // displays under COMBAT_TEXT_SHOW_FRIENDLY_NAMES), energize ticks the power-gain family.
    if self_guid.0 == Some(s.target) {
        for tick in &s.ticks {
            let ev = match *tick {
                PeriodicTick::Damage {
                    amount,
                    absorb,
                    resist,
                    ..
                } => spell_center_text(amount, absorb, resist).map(|(message_type, data)| {
                    CombatTextEvent {
                        message_type,
                        data,
                        extra: None,
                    }
                }),
                PeriodicTick::Heal { amount } => Some(CombatTextEvent {
                    message_type: "PERIODIC_HEAL",
                    data: Some(
                        names
                            .resolve(s.caster, net)
                            .map(str::to_string)
                            .unwrap_or_default(),
                    ),
                    extra: Some(amount.to_string()),
                }),
                PeriodicTick::Energize { power, amount } => {
                    power_message_type(power).map(|message_type| CombatTextEvent {
                        message_type,
                        data: Some(amount.to_string()),
                        extra: None,
                    })
                }
                PeriodicTick::ManaLeech { .. } => None,
            };
            if let Some(ev) = ev {
                center.write(ev);
            }
        }
    }
    // The ungated UNIT_COMBAT feed: every tick, any source. Damage WOUNDs; heal/energize fire
    // their own actions — they never float as worldtext in 5875, but the portrait indicator is
    // exactly where the green/blue numbers live (CombatFeedback.lua's HEAL/ENERGIZE arms).
    if let Some(&unit) = index.0.get(&s.target) {
        for tick in &s.ticks {
            let (action, flags, amount, school) = match *tick {
                PeriodicTick::Damage {
                    amount,
                    school,
                    absorb,
                    resist,
                } => {
                    let Some((action, flags)) = spell_feedback(amount, absorb, resist, false)
                    else {
                        continue;
                    };
                    (action, flags, amount, school)
                }
                PeriodicTick::Heal { amount } => ("HEAL", "", amount, 0),
                // NO ENERGIZE: 5875 never emits it (the string is absent binary-wide —
                // §5-verified, wow-re `unit-combat-event-law.md`); the power gain reaches the
                // center text only.
                PeriodicTick::Energize { .. } | PeriodicTick::ManaLeech { .. } => continue,
            };
            feedback.write(UnitCombatFeedback {
                unit,
                action,
                flags,
                amount,
                school,
            });
        }
    }
    let Some(source) = classify_source(s.caster, index, self_guid, stores) else {
        return; // K = other: never drawn
    };
    // Same B computation as the direct-damage emit (the periodic emitter `0x626dd0` pushes the
    // resolved record to the same `0x6128b0` law).
    let Some(color) = damage_color(source, melee_styled(spells, s.spell_id)) else {
        return;
    };
    let Some(anchor) = gated_anchor(s.target, index, self_guid) else {
        return;
    };
    for tick in &s.ticks {
        let PeriodicTick::Damage {
            amount,
            absorb,
            resist,
            ..
        } = *tick
        else {
            continue;
        };
        if let Some((category, body)) = spell_text(amount, absorb, resist, false) {
            text.write(CombatTextSpawn {
                anchor,
                text: body,
                category,
                color,
            });
        }
    }
}

/// `SMSG_SPELLDAMAGESHIELD` → the reflected damage lands on the **attacker** (the unit that struck
/// the shield bearer) — that's the recipient the number floats over (`0x5e84e0`). The damage
/// SOURCE is the shield bearer (the victim field); the site pushes record NULL, so it colors
/// melee-styled: my shield → white, my pet's → orange.
pub(super) fn damage_shield(
    s: DamageShield,
    index: &GuidIndex,
    self_guid: &SelfGuid,
    stores: &Query<&mut ObjectStore>,
    text: &mut MessageWriter<CombatTextSpawn>,
    feedback: &mut MessageWriter<UnitCombatFeedback>,
) {
    if s.damage == 0 {
        return;
    }
    // The ungated UNIT_COMBAT feed: the reflected damage is damage TAKEN by the attacker —
    // striking a thorns bearer wounds your own portrait.
    if let Some(&unit) = index.0.get(&s.attacker) {
        feedback.write(UnitCombatFeedback {
            unit,
            action: "WOUND",
            flags: "",
            amount: s.damage,
            school: s.school,
        });
    }
    let Some(source) = classify_source(s.victim, index, self_guid, stores) else {
        return; // K = other: never drawn
    };
    let Some(color) = damage_color(source, true) else {
        return;
    };
    if let Some(anchor) = gated_anchor(s.attacker, index, self_guid) {
        text.write(CombatTextSpawn {
            anchor,
            text: s.damage.to_string(),
            category: 0,
            color,
        });
    }
}

/// `SMSG_SPELLHEALLOG` → the direct-heal feeds: the portrait indicator's `HEAL` (green number)
/// and the center text's `HEAL`/`HEAL_CRIT` (arg2 = the healer's name, arg3 = amount). Heals
/// never float as worldtext in 5875 — no [`CombatTextSpawn`] here. Center text is self-only;
/// the UNIT_COMBAT feed fires for any streamed recipient (the target frame's API surface).
pub(super) fn spell_heal_log(
    s: SpellHealLog,
    index: &GuidIndex,
    self_guid: &SelfGuid,
    feedback: &mut MessageWriter<UnitCombatFeedback>,
    center: &mut MessageWriter<CombatTextEvent>,
    names: &mut NameCache,
    net: &NetCommands,
) {
    if let Some(&unit) = index.0.get(&s.target) {
        feedback.write(UnitCombatFeedback {
            unit,
            action: "HEAL",
            flags: if s.critical { "CRITICAL" } else { "" },
            amount: s.amount,
            school: 0,
        });
    }
    if self_guid.0 == Some(s.target) {
        center.write(CombatTextEvent {
            message_type: if s.critical { "HEAL_CRIT" } else { "HEAL" },
            data: Some(
                names
                    .resolve(s.healer, net)
                    .map(str::to_string)
                    .unwrap_or_default(),
            ),
            extra: Some(s.amount.to_string()),
        });
    }
}

/// `SMSG_SPELLENERGIZELOG` → the center text's `MANA`/`RAGE`/`FOCUS`/`ENERGY` line (self-only).
/// NO `UNIT_COMBAT` fires here: the 5875 engine has no ENERGIZE emission — the string is absent
/// from the whole binary (§5-verified, wow-re `unit-combat-event-law.md`; the shipped
/// CombatFeedback.lua ENERGIZE arm is dead code in 1.12).
pub(super) fn spell_energize_log(
    s: SpellEnergizeLog,
    self_guid: &SelfGuid,
    center: &mut MessageWriter<CombatTextEvent>,
) {
    if self_guid.0 == Some(s.target) {
        if let Some(message_type) = power_message_type(s.power) {
            center.write(CombatTextEvent {
                message_type,
                data: Some(s.amount.to_string()),
                extra: None,
            });
        }
    }
}

/// Miss code (vmangos `SpellMissInfo`, 1–11) → the center text's spell-outcome messageType (the
/// `SPELL_*` word family — decision 0578, PROVISIONAL).
fn miss_center_type(code: u8) -> Option<&'static str> {
    Some(match code {
        1 => "SPELL_MISSED",
        2 => "SPELL_RESISTED",
        3 => "SPELL_DODGED",
        4 => "SPELL_PARRIED",
        5 => "SPELL_BLOCKED",
        6 => "SPELL_EVADED",
        7 | 8 => "SPELL_IMMUNE",
        9 => "SPELL_DEFLECTED",
        10 => "SPELL_ABSORBED",
        11 => "SPELL_REFLECTED",
        _ => return None,
    })
}

/// Miss code (vmangos `SpellMissInfo`, 1–11) → the `UNIT_COMBAT` action word — the same table the
/// worldtext WORDS index, on the event's uppercase key vocabulary (`CombatFeedbackText`).
fn miss_action(code: u8) -> Option<&'static str> {
    Some(match code {
        1 => "MISS",
        2 => "RESIST",
        3 => "DODGE",
        4 => "PARRY",
        5 => "BLOCK",
        6 => "EVADE",
        7 | 8 => "IMMUNE",
        9 => "DEFLECT",
        10 => "ABSORB",
        11 => "REFLECT",
        _ => return None,
    })
}

/// `SMSG_SPELLLOGMISS` → one outcome word per missed target (`0x5e7e00`). Disjoint from the
/// SPELL_GO miss list in practice: vmangos routes only impact-time outcomes (immune, evade) here.
/// Source-classified like every emitter (the classifier lives inside the word twin too); the
/// site's record push is unpinned, so the words keep the row-default white (flagged open).
pub(super) fn spell_log_miss(
    s: SpellLogMiss,
    index: &GuidIndex,
    self_guid: &SelfGuid,
    stores: &Query<&mut ObjectStore>,
    text: &mut MessageWriter<CombatTextSpawn>,
    feedback: &mut MessageWriter<UnitCombatFeedback>,
    center: &mut MessageWriter<CombatTextEvent>,
) {
    // The ungated UNIT_COMBAT feed: the outcome word over each missed target, any source — a
    // mob's bolt you resist is YOUR portrait's "Resist". A self target also feeds the center
    // text's SPELL_* word family (decision 0578).
    for &(target, code) in &s.misses {
        if let (Some(&unit), Some(action)) = (index.0.get(&target), miss_action(code)) {
            feedback.write(UnitCombatFeedback {
                unit,
                action,
                flags: "",
                amount: 0,
                school: 0,
            });
        }
        if self_guid.0 == Some(target) {
            if let Some(message_type) = miss_center_type(code) {
                center.write(CombatTextEvent {
                    message_type,
                    data: None,
                    extra: None,
                });
            }
        }
    }
    if classify_source(s.caster, index, self_guid, stores).is_none() {
        return; // K = other: never drawn
    }
    for &(target, code) in &s.misses {
        if let (Some(anchor), Some((word, category))) =
            (gated_anchor(target, index, self_guid), miss_word(code))
        {
            text.write(CombatTextSpawn {
                anchor,
                text: word.to_string(),
                category,
                color: None,
            });
        }
    }
}

/// `SMSG_LOG_XPGAIN` → `"XP: %d"` over **self**, category 4 — the one emitter that is
/// self-anchored by design and skips Gate A (`0x607260`). Fires for kill and quest XP alike. The
/// same packet also writes the combat log's own chat line.
pub(super) fn xp_gain(
    x: XpGain,
    index: &GuidIndex,
    self_guid: &SelfGuid,
    text: &mut MessageWriter<CombatTextSpawn>,
    chat_log: &mut ChatLog,
) {
    if let Some(&me) = self_guid.0.as_ref().and_then(|g| index.0.get(g)) {
        text.write(CombatTextSpawn {
            anchor: me,
            text: format!("XP: {}", x.total),
            category: 4,
            color: None, // row 4's own purple — XP skips the damage color law entirely
        });
    }
    chat_log.push_xp_gain(&x);
}

/// `SMSG_EXPLORATION_EXPERIENCE` → the discovery announcement (decision 0828; surfaces and
/// sound byte-verified by the 0829 RE, wow-re `system/net/net.md`). Three legs, mirroring the
/// real handler's case body `[0x5e41d2, 0x5e42bb]`:
/// - the ERR_ZONE_EXPLORED toast + (xp > 0) the ERR_ZONE_EXPLORED_XP chat line, queued via
///   [`ChatLog::push_exploration`] — the area name is the packet's `AreaTable.dbc` row id
///   resolved through the shared catalog; a miss (no catalog, or an id the DBC doesn't know)
///   shows no message, faithfully;
/// - the race-keyed discovery jingle (`ChrRaces.dbc` col 3 → SoundEntries), played
///   **unconditionally** — even when the area id resolves to nothing — through the same 2D-kit
///   path as `SMSG_PLAY_SOUND`.
///
/// No floating text (the XP itself still arrives via the non-kill `SMSG_LOG_XPGAIN`).
#[allow(clippy::too_many_arguments)] // one arm of the dispatch — its param list IS its input set
pub(super) fn exploration_xp(
    x: ExplorationXp,
    area_table: Option<&crate::area::AreaTableRes>,
    sounds: Option<&crate::sound::ExplorationSounds>,
    index: &GuidIndex,
    self_guid: &SelfGuid,
    stores: &Query<&mut ObjectStore>,
    sound_out: &mut MessageWriter<super::super::ServerSoundMessage>,
    chat_log: &mut ChatLog,
) {
    let race = self_guid
        .0
        .as_ref()
        .and_then(|g| index.0.get(g))
        .and_then(|&e| stores.get(e).ok())
        .and_then(|s| s.0.unit_race());
    let kit = race.and_then(|r| sounds.and_then(|s| s.0.kit(u32::from(r))));
    if let Some(kit) = kit {
        sound_out.write(super::super::ServerSoundMessage {
            kind: super::super::ServerSoundKind::Sound2d,
            sound_id: kit,
            source: None,
        });
    }
    match area_table.and_then(|t| t.0.name(x.area_id)) {
        Some(name) => {
            info!(
                "exploration: discovered {name} (area {}, {} xp, jingle kit {})",
                x.area_id,
                x.xp,
                kit.unwrap_or(0)
            );
            chat_log.push_exploration(name, x.xp);
        }
        None => warn!(
            "exploration: discovered area {} has no AreaTable name — no message (sound only)",
            x.area_id
        ),
    }
}

/// `SMSG_LEVELUP_INFO` → the ding's chat lines (decision 0304). The talent-count arg is not on the
/// wire — the client computes `(newLevel >= 10) ? 1 : 0` (byte-verified `0x5e407c`, wow-re
/// levelup-ding.md — the 0305 fold-back), exactly this. The ding's VISUAL is deliberately absent
/// here: it rides the UNIT_FIELD_LEVEL change-watcher (`entities` spell_fx::level_up_flash), never
/// this packet.
pub(super) fn level_up(l: LevelUpInfo, chat_log: &mut ChatLog) {
    let talent_points = u32::from(l.level >= 10);
    chat_log.push_level_up(&l, talent_points);
}
