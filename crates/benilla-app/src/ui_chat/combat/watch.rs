//! The combat-log lines that are **not packet-driven at all** — the two families §3 of wow-re's
//! `combat-log-chat-law.md` singles out, plus the pet-loyalty leaf.
//!
//! Everything else in [`super`] hangs off an SMSG arm. These three hang off *descriptor changes*,
//! exactly as the reference does:
//!
//! - **the death line** (`UNITDIES*` / `UNITDESTROYEDOTHER`) rides the unit-death reflex
//!   `0x625190`, not a packet — there is no "X died" opcode in 1.12;
//! - **the aura lines** (`AURAADDED*`, `AURAREMOVED*`, `AURAAPPLICATIONADDED*`) ride the
//!   `UNIT_FIELD_AURA` / `UNIT_FIELD_AURAAPPLICATIONS` change callbacks `0x604d00` / `0x604ea0`
//!   through the bridges `0x612320`/`0x6123f0`/`0x612450`;
//! - **the loyalty line** rides the `UNIT_FIELD_BYTES_1` byte-1 callback (`0x5ff860` → `0x62d440`).
//!
//! Each is a slot diff against the last frame's snapshot, the same idiom
//! [`crate::creature_anim::arm_aura_state_fx`] uses for the aura state kits — and for the same
//! reason: the *edge* is the event, and a descriptor snapshot is all the wire gives us.

use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use super::{
    aura_gone_kind, classify, death_kind, in_range, periodic_kind, Family, Fills, Named,
    PendingCombat, UnitClass, Variant,
};
use crate::net::{GuidIndex, ObjectStore, Reputations, SelfGuid};
use crate::target::ring::Factions;
use crate::ui_chat::{ChatEventKind, ChatLog};
use crate::ui_party::GroupState;

/// The classification inputs all three watchers share — the same six facts
/// [`crate::net::apply::combat_chat::ChatCtx`] carries, as a `SystemParam` so a watcher's own
/// parameter list stays about *its* diff.
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct WatchCtx<'w> {
    pub self_guid: Res<'w, SelfGuid>,
    pub group: Res<'w, GroupState>,
    pub index: Res<'w, GuidIndex>,
    pub factions: Option<Res<'w, Factions>>,
    pub reputations: Res<'w, Reputations>,
    pub spells: Option<Res<'w, crate::ui_action::Spells>>,
}

impl WatchCtx<'_> {
    fn classify(&self, guid: u64, stores: &Query<(Entity, &ObjectStore)>) -> UnitClass {
        classify(
            guid,
            &self.self_guid,
            Some(&self.group),
            &self.index,
            stores,
            self.factions.as_deref(),
            &self.reputations,
        )
    }

    /// [`crate::net::apply::combat_chat::ChatCtx::spell_name`]'s law, shared: the per-spell gates
    /// (`Attributes & 0x180`, an empty localized name) apply to every spell-driven line, and an
    /// aura line is one.
    fn spell_name(&self, spell_id: u32) -> Option<String> {
        let Some(display) = self.spells.as_ref().and_then(|s| s.catalog.get(spell_id)) else {
            return Some(String::new());
        };
        const DO_NOT_DISPLAY_OR_LOG: u32 = 0x180;
        if display.attributes & DO_NOT_DISPLAY_OR_LOG != 0 || display.name.is_empty() {
            return None;
        }
        Some(display.name.clone())
    }

    /// The reference's ONE-SIDED range test — the shape 15 formatters run instead of the two-ended
    /// gate (§5.2), the death and aura lines among them. One participant, one distance.
    fn in_range(&self, guid: u64, class: UnitClass, poses: &Query<&Transform>) -> bool {
        in_range(guid, class, &self.self_guid, &self.index, poses)
    }
}

/// Queue one single-endpoint line, gated the way its formatter gates.
#[allow(clippy::too_many_arguments)] // the line's own facts, plus the two contexts the gate reads
fn queue_one(
    log: &mut ChatLog,
    ctx: &WatchCtx,
    poses: &Query<&Transform>,
    kind: ChatEventKind,
    family: Family,
    subject: (u64, UnitClass),
    fills: Fills,
    named: Named,
) {
    if !ctx.in_range(subject.0, subject.1, poses) {
        return;
    }
    log.push_combat(PendingCombat {
        kind,
        family,
        variant: Variant::of(subject.1, UnitClass::Creature),
        subject: subject.0,
        object: 0,
        fills,
        named,
        tries: 0,
    });
}

/// `Spell.dbc` `Effect[0]` values that make a unit "destroyed" rather than "dead" — `0x62c320`'s
/// byte table, read through `UNIT_CREATED_BY_SPELL`. A totem, a summoned object, a guardian: the
/// things the world *made*, which are unmade rather than killed.
const DESTROYED_EFFECTS: [u32; 10] = [50, 74, 87, 88, 89, 90, 104, 105, 106, 107];

/// The unit-death reflex's chat line (`0x625190` → `0x62c160`).
///
/// **The edge is the event.** The wire never says "this unit died"; it says the unit's health is
/// zero, or sets its dead flag, in an ordinary descriptor update — so the line comes off a diff
/// against the previous frame, and a unit that streams in *already* dead produces none (which is
/// what the reference does too: its reflex fires on the transition, not on the state).
///
/// **The XP-award suppression is deliberately not modelled**, and is named rather than dropped:
/// `[CGUnit+0xc58] bit 3`, set only in the `SMSG_LOG_XPGAIN` chain, makes the death reflex emit the
/// award line *instead of* the plain death line (§5.9). wow-re found no clearing site for the bit,
/// so whether it survives a second death is UNSETTLED there — and modelling an unsettled latch is
/// how you get a line that silently stops appearing. The visible consequence today is one extra
/// "%s dies." beside the XP line on a kill that awards experience.
pub(crate) fn death_lines(
    ctx: WatchCtx,
    stores: Query<(Entity, &ObjectStore)>,
    guids: Query<&crate::net::Guid>,
    changed: Query<Entity, Changed<ObjectStore>>,
    poses: Query<&Transform>,
    mut log: ResMut<ChatLog>,
    mut was_dead: Local<EntityHashMap<bool>>,
) {
    for entity in changed.iter() {
        let Ok((_, store)) = stores.get(entity) else {
            continue;
        };
        let dead = store.0.unit_is_dead();
        let seen = was_dead.insert(entity, dead);
        // No previous frame for this entity: it streamed in at whatever state it is in, which is
        // not a death.
        let Some(false) = seen else { continue };
        if !dead {
            continue;
        }
        let Ok(&crate::net::Guid(guid)) = guids.get(entity) else {
            continue;
        };
        let class = ctx.classify(guid, &stores);
        // A summoned thing is *destroyed*; a living one *dies*. The test is the creating spell's
        // first effect, which is why it needs the spell catalog at all.
        let destroyed = store
            .0
            .unit_created_by_spell()
            .filter(|&id| id != 0)
            .and_then(|id| ctx.spells.as_ref()?.catalog.get(id))
            .is_some_and(|d| DESTROYED_EFFECTS.contains(&d.effects[0]));
        let family = match (class, destroyed) {
            (UnitClass::Me, _) => super::UNITDIES,
            (_, true) => super::UNITDESTROYEDOTHER,
            (_, false) => super::UNITDIES,
        };
        queue_one(
            &mut log,
            &ctx,
            &poses,
            death_kind(class),
            family,
            (guid, class),
            Fills::default(),
            Named::Ready,
        );
    }
    was_dead.retain(|e, _| stores.contains(*e));
}

/// The aura lines (`0x62b480` / `0x62b800`).
///
/// **Three edges out of one diff**, because the reference reads two descriptor arrays through two
/// callbacks and words three sentences:
///
/// - a slot that filled → `AURAADDED{SELF,OTHER}{HARMFUL,HELPFUL}`, at the PERIODIC chat types
///   (harmful takes the `…_DAMAGE` row, helpful the `…_BUFFS` one) — not the `AURA_GONE` block,
///   which is the departure's alone;
/// - a slot that emptied → `AURAREMOVED{SELF,OTHER}`, at `0x41`/`0x42`/`0x43`;
/// - a slot whose stack count ROSE → `AURAAPPLICATIONADDED*`, and only when the spell can stack at
///   all. We approximate the reference's `SpellRec->StackAmount > 1` gate with the observed count:
///   a stack that rose past one is a spell that stacks. It differs only for a first application of
///   a stacking aura, which the reference words as a plain `AURAADDED*` — and so do we, because a
///   0→1 step is a *fill*, not an increase.
///
/// **HARMFUL is the SLOT INDEX, not a flag** (§4.4, three byte sites): slots `0x20`–`0x2f` are
/// harmful, every other slot helpful. `UNIT_FIELD_AURAFLAGS` is only an occupancy predicate — using
/// it here would be the wrong mechanism that happens to agree most of the time.
pub(crate) fn aura_lines(
    ctx: WatchCtx,
    stores: Query<(Entity, &ObjectStore)>,
    guids: Query<&crate::net::Guid>,
    changed: Query<Entity, Changed<ObjectStore>>,
    poses: Query<&Transform>,
    mut log: ResMut<ChatLog>,
    // Per entity, per slot: `(spell id, stacks)` as of the last frame. Slot-keyed rather than
    // spell-keyed because the sentence's harmful/helpful half IS the slot.
    mut seen: Local<EntityHashMap<Vec<(u8, u32, u8)>>>,
) {
    /// The first harmful aura slot — `0x20`. Slots below it are helpful (§4.4).
    const FIRST_HARMFUL_SLOT: u8 = 0x20;

    for entity in changed.iter() {
        let Ok((_, store)) = stores.get(entity) else {
            continue;
        };
        let now: Vec<(u8, u32, u8)> = store
            .0
            .unit_auras()
            .map(|a| (a.slot, a.spell_id, a.stacks))
            .collect();
        let prev = seen.insert(entity, now.clone());
        // First sight of this unit: its standing auras are not arrivals.
        let Some(prev) = prev else { continue };
        let Ok(&crate::net::Guid(guid)) = guids.get(entity) else {
            continue;
        };
        let class = ctx.classify(guid, &stores);
        let harmful = |slot: u8| slot >= FIRST_HARMFUL_SLOT;
        let find =
            |list: &[(u8, u32, u8)], slot: u8| list.iter().find(|(s, _, _)| *s == slot).copied();

        for &(slot, spell_id, stacks) in &now {
            let Some(spell) = ctx.spell_name(spell_id) else {
                continue;
            };
            let fills = |amount: i64| Fills {
                spell: spell.clone(),
                amount,
                ..Default::default()
            };
            match find(&prev, slot) {
                // The slot held a DIFFERENT spell, or nothing: this aura arrived.
                Some((_, was, _)) if was == spell_id => {
                    let Some((_, _, before)) = find(&prev, slot) else {
                        continue;
                    };
                    if stacks <= before || stacks <= 1 {
                        continue;
                    }
                    let Some(kind) = periodic_kind(class, !harmful(slot)) else {
                        continue;
                    };
                    queue_one(
                        &mut log,
                        &ctx,
                        &poses,
                        kind,
                        if harmful(slot) {
                            super::AURAAPPLICATIONADDED_HARMFUL
                        } else {
                            super::AURAAPPLICATIONADDED_HELPFUL
                        },
                        (guid, class),
                        fills(i64::from(stacks)),
                        Named::Ready,
                    );
                }
                _ => {
                    let Some(kind) = periodic_kind(class, !harmful(slot)) else {
                        continue;
                    };
                    queue_one(
                        &mut log,
                        &ctx,
                        &poses,
                        kind,
                        if harmful(slot) {
                            super::AURAADDED_HARMFUL
                        } else {
                            super::AURAADDED_HELPFUL
                        },
                        (guid, class),
                        fills(0),
                        Named::Ready,
                    );
                }
            }
        }
        // Departures: a slot that held a spell and no longer holds THAT spell.
        for &(slot, spell_id, _) in &prev {
            if find(&now, slot).is_some_and(|(_, id, _)| id == spell_id) {
                continue;
            }
            let Some(spell) = ctx.spell_name(spell_id) else {
                continue;
            };
            queue_one(
                &mut log,
                &ctx,
                &poses,
                aura_gone_kind(class),
                super::AURAREMOVED,
                (guid, class),
                Fills {
                    spell,
                    ..Default::default()
                },
                Named::Ready,
            );
        }
    }
    seen.retain(|e, _| stores.contains(*e));
}

/// The pet-loyalty line (`0x5ff860` → `0x62d440`) — `UNIT_FIELD_BYTES_1` byte 1 moving.
///
/// **Both legs require the pet's owner to be you**, which is the reference's own guard, and the
/// chat type is the literal `0x19` `COMBAT_MISC_INFO`. The reference emits the localized text as
/// the single `%s` argument of a bare `"%s"`; a no-slot family reaches the same sentence.
pub(crate) fn pet_loyalty_lines(
    ctx: WatchCtx,
    stores: Query<(Entity, &ObjectStore)>,
    guids: Query<&crate::net::Guid>,
    changed: Query<Entity, Changed<ObjectStore>>,
    mut log: ResMut<ChatLog>,
    mut seen: Local<EntityHashMap<u8>>,
) {
    for entity in changed.iter() {
        let Ok((_, store)) = stores.get(entity) else {
            continue;
        };
        let level = store.0.unit_loyalty_level();
        let Some(before) = seen.insert(entity, level) else {
            continue;
        };
        if level == before || level == 0 || before == 0 {
            continue;
        }
        let Ok(&crate::net::Guid(guid)) = guids.get(entity) else {
            continue;
        };
        if ctx.classify(guid, &stores) != UnitClass::MyPet {
            continue;
        }
        log.push_combat(PendingCombat {
            kind: ChatEventKind::CombatMiscInfo,
            family: if level > before {
                super::PET_LOYALTY_GAIN
            } else {
                super::PET_LOYALTY_LOSS
            },
            variant: Variant::OtherOther,
            subject: 0,
            object: 0,
            fills: Fills::default(),
            named: Named::Ready,
            tries: 0,
        });
    }
    seen.retain(|e, _| stores.contains(*e));
}
