//! The GameObject **lock chain** — the client's resolver `0x5f83d0`, its per-slot **Action** gate
//! (`0x5f81d0`), and the §8.8 refusal-toast routing (decisions 0239 / 0545 / **0752**).
//!
//! **One chain, two consumers — exactly as the reference.** `CGGameObject`'s per-type strategy
//! calls the same resolver twice: from **`usable` `0x5f3130`** (which decides the cursor's grayed
//! twin *and* whether the right-click is sent at all — §4a/§8.7), and from the **USE sender
//! `0x5f33e0`** (which picks between `CMSG_GAMEOBJ_USE`, an `OPEN_LOCK` cast, and a client-local
//! toast — §8.4). That is why it lives here rather than inside either caller: the icon and the
//! click agree by construction only if they ask the same question.
//!
//! ## The Action gate — the piece that was missing (0752)
//!
//! Before the resolver will *consider* a `Lock.dbc` slot it asks `0x5f81d0(GO, Action[i])`, which
//! answers from the GameObject's own **state** and its `GO_FLAG_LOCKED` wire bit — see
//! [`benilla_formats::LockSlot::available`]. Without it, the ladder is over-permissive in a way
//! that looks arbitrary from the chair: nearly every keyed door in 5875 carries a spare
//! `Quick Open` slot (`LockType 10`, `Skill 0`, **`Action 0`**), *every* character knows spell 6247
//! "Opening", and Action 0 means "only when the object is NOT flagged locked" — so skipping the
//! gate hands every padlocked door in the game to every player. The Searing Gorge gate (lock 84)
//! was the one that refused because it is the one that carries no `Action 0` slot.
//!
//! ## Satisfaction is the SPELL's value, not the player's skill
//!
//! `0x5f850f` compares the matched spell's own OPEN_LOCK **effect value**
//! ([`benilla_formats::SpellDisplay::open_lock_skill`]) against the slot's requirement — which is
//! `Skill[i]`, or **`GAMEOBJECT_LEVEL × 5`** when `Skill[i]` is zero (`0x5f84be`). It never reads
//! the skill block. That is not an approximation on the client's part: the DBC encodes the same
//! number (Pick Lock at 60 → 300, a rogue's cap; Mining at 60 → 300).

use std::collections::HashSet;

use benilla_formats::{LockSlot, LOCK_KEY_ITEM, LOCK_KEY_SKILL};
use bevy::prelude::*;

use crate::net::ObjectStore;

/// The lock chain's full data set as ONE [`SystemParam`] (decisions 0239 / 0545 / 0752): the
/// ask-once GO-template cache, `Lock.dbc` + `LockType.dbc`, the spell catalog, and the ask-once
/// item-template cache (key-item names for the "Requires \<key\>" toast, and the key's own ON_USE
/// spell). The `Option` members are absent without client data.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct GoLockInputs<'w> {
    pub(super) templates: Res<'w, crate::go_templates::GameObjectTemplates>,
    pub(super) locks: Option<Res<'w, crate::go_templates::Locks>>,
    pub(super) lock_types: Option<Res<'w, crate::go_templates::LockTypes>>,
    pub(super) spells: Option<Res<'w, crate::ui_action::Spells>>,
    pub(super) items: ResMut<'w, crate::items::Items>,
}

/// The GameObject facts the Action gate and the requirement fallback read off the wire — gathered
/// once by the caller so the resolver stays pure.
#[derive(Clone, Copy, Debug)]
pub(super) struct GoFacts {
    /// The client's stored `GAMEOBJECT_STATE` (`go+0x27c`) — [`crate::go_anim::go_state`].
    pub(super) state: u32,
    /// `GAMEOBJECT_FLAGS & GO_FLAG_LOCKED (0x2)`.
    pub(super) flag_locked: bool,
    /// `GAMEOBJECT_LEVEL` — the `Skill[i] == 0` requirement fallback's `× 5` base. vmangos leaves
    /// it 0 on everything but transports, so on this server the fallback resolves to "no
    /// requirement" and the server does the real gating; the client law is modelled regardless.
    pub(super) level: u32,
}

/// What the resolver says about a GameObject's lock — the reference's return plus its spell-id
/// out-param, made explicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LockOutcome {
    /// No `Lock.dbc` row, or every slot empty — `0x5f8180` null / `[ebp-1]` never set. The object
    /// opens by `CMSG_GAMEOBJ_USE`.
    Unlocked,
    /// A skill slot the player satisfies with a known `OPEN_LOCK` spell — cast it at the GO.
    OpenBySpell(u32),
    /// A key slot whose item the player carries; the value is the **key's entry**. The reference
    /// resolves the item's ON_USE spell here (`0x5d8c80`) and casts that.
    OpenByKey(u32),
    /// A lock is present and no slot is satisfied — the client-local refusal, **no packet**.
    Unmet,
}

impl LockOutcome {
    /// The `usable` half: `0x5f3130`'s lock arm returns *not usable* only for [`Self::Unmet`], and
    /// **only when `GO_FLAG_LOCKED` is set** (`0x5f32a6`: `shr 1; test al,1; je`). With the flag
    /// clear the arm is skipped entirely — which is exactly why a herb node you cannot gather still
    /// shows the lit `GatherHerbs` cursor and only toasts on the click.
    pub(super) fn blocks_usable(self, flag_locked: bool) -> bool {
        flag_locked && self == LockOutcome::Unmet
    }
}

/// Read the Action gate's inputs off a hovered GameObject — its store plus the stored state the
/// caller already resolved ([`crate::go_anim::go_state`]). `None` (nothing hovered / no store yet)
/// answers with the wire defaults, which make every gated slot inapplicable rather than free.
pub(super) fn go_facts(go: Option<(&ObjectStore, u32)>) -> GoFacts {
    match go {
        Some((store, state)) => GoFacts {
            state,
            flag_locked: store.0.gameobject_flags() & GO_FLAG_LOCKED != 0,
            level: store.0.gameobject_level(),
        },
        None => GoFacts {
            state: benilla_formats::GO_STATE_ACTIVE,
            flag_locked: false,
            level: 0,
        },
    }
}

/// `GO_FLAG_LOCKED` (vmangos `GameObjectFlags`) — the wire bit that both selects the Action-1
/// ("unlock") slots and arms `usable`'s lock check (§8.8, `0x5f32a6`).
pub(crate) const GO_FLAG_LOCKED: u32 = 0x2;

/// The client's lock resolver **`0x5f83d0`**, transcribed (decision 0752).
///
/// Walks the 8 `Lock.dbc` slots **in order**, dispatching each by `Type`:
/// - **SKILL (2)** — gate on [`LockSlot::available`], then linear-scan the player's known spells
///   for one whose `SPELL_EFFECT_OPEN_LOCK` `EffectMiscValue` equals the slot's `Index`; the first
///   such match sets `matched_spell` unconditionally (`0x5f84f8`, *before* the rank test — that
///   nonzero-ness is §8.8's `0xdf`-vs-`0xe0` discriminator), then the spell's effect value is
///   compared against the requirement (`0x5f850f`). Sufficient → satisfied.
/// - **KEY (1)** — gate the same way, then look for the key item in our bags/keyring.
/// - **NONE (0)** — skipped without marking the lock real.
///
/// Any SKILL/KEY slot marks the lock **real** even when its Action gate rejects it (the binary
/// writes `[ebp-1] = 1` *before* calling `0x5f81d0`), so a door whose only opener is gated out
/// refuses rather than falling through to `CMSG_GAMEOBJ_USE`.
///
/// `matched_spell` is the out-param the toast routing needs; it is written for a LockType match
/// even when the value test then fails.
///
/// One deliberate difference: the reference walks its known-spell **array** in index order, while
/// `known` is a set. That only decides *which* of several equally-matching spells lands in
/// `matched_spell` (a miner knows four Mining ranks, all `LockType 3`), never the outcome — the
/// scan keeps going past an insufficient one, so the best-valued opener still wins.
pub(super) fn resolve_lock(
    slots: &[LockSlot],
    known: &HashSet<u32>,
    spells: Option<&crate::ui_action::Spells>,
    me: Option<&ObjectStore>,
    items: &crate::items::Items,
    go: GoFacts,
    matched_spell: &mut Option<u32>,
) -> LockOutcome {
    let caster_level = me.and_then(|s| s.0.unit_level()).unwrap_or(0);
    let mut real = false;
    for slot in slots {
        match slot.key_type {
            LOCK_KEY_SKILL => {
                real = true;
                if !slot.available(go.state, go.flag_locked) {
                    continue;
                }
                let Some(spells) = spells else { continue };
                for &id in known {
                    let Some(spell) = spells.catalog.get(id) else {
                        continue;
                    };
                    if spell.open_lock_type() != Some(slot.index) {
                        continue;
                    }
                    // `0x5f84f8` — the out-param is written on the LockType match, before the
                    // value test.
                    matched_spell.get_or_insert(id);
                    let provides = spell.open_lock_skill(caster_level).unwrap_or(0);
                    if provides >= required_skill(slot, go.level) {
                        return LockOutcome::OpenBySpell(id);
                    }
                }
            }
            LOCK_KEY_ITEM => {
                real = true;
                if !slot.available(go.state, go.flag_locked) {
                    continue;
                }
                if me.is_some_and(|s| holds_item(&s.0, items, slot.index)) {
                    return LockOutcome::OpenByKey(slot.index);
                }
            }
            _ => {}
        }
    }
    if real {
        LockOutcome::Unmet
    } else {
        LockOutcome::Unlocked
    }
}

/// The slot's required skill: `Skill[i]`, or **`GAMEOBJECT_LEVEL × 5`** when it is zero
/// (`0x5f84be..0x5f84ca` — the resolver; `0x5f3490..0x5f349f` recomputes the same value for the
/// `0xe0` toast's `%d`). A zero `Skill` is *not* "no requirement": every gathering node in the game
/// stores 0 there and leans on the object's level.
pub(super) fn required_skill(slot: &LockSlot, go_level: u32) -> i32 {
    if slot.skill != 0 {
        slot.skill as i32
    } else {
        (go_level * 5) as i32
    }
}

/// Whether we carry item `entry` — the key-slot scan's inventory walk (`0x622270` over the
/// inventory manager). Bags **and the keyring**, which is where dungeon keys actually live; the
/// bank is not walked because benilla only streams its contents while the bank window is open.
fn holds_item(
    store: &benilla_protocol::messages::ObjectFields,
    items: &crate::items::Items,
    entry: u32,
) -> bool {
    if crate::ui_items::count_of(store, items, entry) > 0 {
        return true;
    }
    (0..32).any(|i| {
        store
            .player_keyring_slot(i)
            .filter(|&g| g != 0)
            .and_then(|g| items.object(g))
            .and_then(|f| f.object_entry())
            == Some(entry)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_formats::{GO_STATE_ACTIVE, GO_STATE_READY, LOCK_KEY_NONE};

    fn skill_slot(index: u32, skill: u32, action: u32) -> LockSlot {
        LockSlot {
            key_type: LOCK_KEY_SKILL,
            index,
            skill,
            action,
        }
    }

    /// The requirement fallback (`0x5f84be`): a zero `Skill[i]` means GO-level × 5, not "free".
    #[test]
    fn zero_skill_falls_back_to_go_level_times_five() {
        assert_eq!(required_skill(&skill_slot(3, 0, 0), 0), 0);
        assert_eq!(required_skill(&skill_slot(3, 0, 0), 20), 100);
        // A nonzero Skill wins outright — the level is never consulted.
        assert_eq!(required_skill(&skill_slot(1, 280, 1), 60), 280);
    }

    /// A door whose only opener is gated out by its Action must refuse, **not** fall through to
    /// `CMSG_GAMEOBJ_USE`: the binary marks the lock real (`[ebp-1] = 1`) *before* asking
    /// `0x5f81d0`. This is the difference between "locked door refuses" and "locked door opens".
    #[test]
    fn a_gated_out_slot_still_makes_the_lock_real() {
        let slots = [
            skill_slot(10, 0, 0), // Quick Open — Action 0, gated out on a flagged-locked door
            LockSlot::default(),
        ];
        let items = crate::items::Items::default();
        let mut matched = None;
        let out = resolve_lock(
            &slots,
            &HashSet::new(),
            None,
            None,
            &items,
            GoFacts {
                state: GO_STATE_READY,
                flag_locked: true,
                level: 0,
            },
            &mut matched,
        );
        assert_eq!(out, LockOutcome::Unmet);
        assert!(
            out.blocks_usable(true),
            "a flagged-locked unmet lock grays the cursor"
        );
        // …and with the flag clear the SAME lock is not a `usable` blocker (the arm is skipped) —
        // the herb-node case: lit cursor, toast on click.
        assert!(!out.blocks_usable(false));
    }

    /// The reported bug, end to end, on the **real** shipped `Lock.dbc` / `Spell.dbc` values
    /// (decision 0752). `benilla-formats`' own `real_lock_catalog_reads_the_action_column` and
    /// `real_spell_catalog_computes_the_lock_skill_an_opener_provides` pin these numbers against
    /// the files, so this stays a pure unit test while still describing real data.
    ///
    /// Before the Action gate, every one of these doors opened for every character: "Opening"
    /// (6247) satisfied their spare `Quick Open` slot with a flat 100 ≥ 0.
    #[test]
    fn a_keyed_door_refuses_the_universally_known_opening_spell() {
        use benilla_formats::{OpenLock, SpellCatalog, SpellDisplay};

        // Scholomance Door, lockId 1159 — key 13704 / Pick Lock 280 / Quick Open / Quick Close /
        // Blasting 300; the template ships GO_FLAG_LOCKED.
        let mut scholomance = [LockSlot::default(); 8];
        scholomance[0] = LockSlot {
            key_type: LOCK_KEY_ITEM,
            index: 13704,
            skill: 0,
            action: 1,
        };
        scholomance[1] = skill_slot(1, 280, 1);
        scholomance[2] = skill_slot(10, 0, 0);
        scholomance[3] = skill_slot(11, 0, 2);
        scholomance[4] = skill_slot(16, 300, 1);

        // Two openers with their real value inputs: the "Opening" every character is created
        // with, and Pick Lock (whose value tracks 5×level).
        let opening = SpellDisplay {
            open_lock: Some(OpenLock {
                lock_type: 10,
                effect: 0,
            }),
            effect_base_points: [99, 0, 0],
            effect_base_dice: [1, 0, 0],
            ..Default::default()
        };
        let pick_lock = SpellDisplay {
            open_lock: Some(OpenLock {
                lock_type: 1,
                effect: 0,
            }),
            effect_base_points: [4, 0, 0],
            effect_base_dice: [1, 0, 0],
            effect_real_points_per_level: [5.0, 0.0, 0.0],
            spell_level: 1,
            ..Default::default()
        };
        let spells = crate::ui_action::Spells {
            catalog: SpellCatalog::from_displays(
                [(6247, opening), (1804, pick_lock)].into_iter().collect(),
            ),
            ..crate::ui_action::Spells::empty_for_tests()
        };
        let items = crate::items::Items::default();
        let locked_shut = GoFacts {
            state: GO_STATE_READY,
            flag_locked: true,
            level: 0,
        };

        // A character who knows only "Opening" — i.e. everybody — is refused.
        let mut matched = None;
        assert_eq!(
            resolve_lock(
                &scholomance,
                &HashSet::from([6247]),
                Some(&spells),
                None,
                &items,
                locked_shut,
                &mut matched,
            ),
            LockOutcome::Unmet,
        );
        assert_eq!(
            matched, None,
            "a gated-out slot never even reaches the known-spell scan"
        );

        // Clear the flag and the SAME door opens to the SAME spell — proof that the gate is what
        // refuses, not the value test. (No shipped door does this; it isolates the mechanism.)
        assert_eq!(
            resolve_lock(
                &scholomance,
                &HashSet::from([6247]),
                Some(&spells),
                None,
                &items,
                GoFacts {
                    flag_locked: false,
                    ..locked_shut
                },
                &mut None,
            ),
            LockOutcome::OpenBySpell(6247),
        );

        // Pick Lock sits on an Action 1 slot, so the flag *selects* it — but with no self store
        // the level reads 0, Pick Lock provides 5, and 5 < 280 refuses. The out-param is still
        // written, which is §8.8's `0xdf`-vs-`0xe0` discriminator.
        let mut matched = None;
        assert_eq!(
            resolve_lock(
                &scholomance,
                &HashSet::from([1804]),
                Some(&spells),
                None,
                &items,
                locked_shut,
                &mut matched,
            ),
            LockOutcome::Unmet,
        );
        assert_eq!(
            matched,
            Some(1804),
            "a LockType match writes the out-param before the value test"
        );

        // The Searing Gorge gate (lockId 84) — the reporter's counter-example. No Action-0 slot at
        // all, which is why it refused even before the gate existed; it must still refuse.
        let mut searing_gorge = [LockSlot::default(); 8];
        searing_gorge[0] = LockSlot {
            key_type: LOCK_KEY_ITEM,
            index: 5396,
            skill: 0,
            action: 1,
        };
        searing_gorge[1] = skill_slot(1, 225, 1);
        assert_eq!(
            resolve_lock(
                &searing_gorge,
                &HashSet::from([6247]),
                Some(&spells),
                None,
                &items,
                locked_shut,
                &mut None,
            ),
            LockOutcome::Unmet,
        );

        // And the gate must not touch what already worked: a Copper Vein (lockId 38 — one Mining
        // slot, Skill 0, Action 0) on an unflagged, READY object still opens for a miner.
        let mining = SpellDisplay {
            open_lock: Some(OpenLock {
                lock_type: 3,
                effect: 0,
            }),
            effect_base_points: [-1, 0, 0],
            effect_base_dice: [1, 0, 0],
            effect_real_points_per_level: [5.0, 0.0, 0.0],
            ..Default::default()
        };
        let with_mining = crate::ui_action::Spells {
            catalog: SpellCatalog::from_displays([(2575, mining)].into_iter().collect()),
            ..crate::ui_action::Spells::empty_for_tests()
        };
        let mut vein = [LockSlot::default(); 8];
        vein[0] = skill_slot(3, 0, 0);
        assert_eq!(
            resolve_lock(
                &vein,
                &HashSet::from([2575]),
                Some(&with_mining),
                None,
                &items,
                GoFacts {
                    state: GO_STATE_READY,
                    flag_locked: false,
                    level: 0
                },
                &mut None,
            ),
            LockOutcome::OpenBySpell(2575),
        );
    }

    /// An all-empty row is not a lock at all — `CMSG_GAMEOBJ_USE`.
    #[test]
    fn an_empty_row_is_unlocked() {
        let slots = [LockSlot::default(); 8];
        let items = crate::items::Items::default();
        let mut matched = None;
        assert_eq!(
            resolve_lock(
                &slots,
                &HashSet::new(),
                None,
                None,
                &items,
                GoFacts {
                    state: GO_STATE_ACTIVE,
                    flag_locked: false,
                    level: 0
                },
                &mut matched,
            ),
            LockOutcome::Unlocked
        );
        assert_eq!(slots[0].key_type, LOCK_KEY_NONE);
    }
}
