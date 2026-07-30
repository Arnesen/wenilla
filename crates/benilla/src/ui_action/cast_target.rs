//! The cast-arm's **target resolution** — what actually goes in `CMSG_CAST_SPELL`'s target block.
//!
//! Transcribes `Spell_C::ArmCast 0x6e5250` + `BindTarget 0x6e5b40` (wow-re `wave-cast.md`, both
//! byte-verified): the client seeds a targeting flag_word from `Spell.dbc Targets` (`SpellRec+0x34`),
//! adjusts it with the implicit-target switch (`SpellRec+0x148`, jump-table `0x6e5484`), and then
//!
//! - **flag_word == 0** ⇒ the cast needs no target at all — commit immediately, wire mask
//!   `TARGET_FLAG_SELF (0)`, nothing follows (Ice Armor, Battle Shout, Feign Death…). The server
//!   fills the target from the spell's implicit targeting. The real client **never** ships the
//!   current selection for these — doing so is exactly the "Invalid target" bug this fixes.
//! - **nonzero** ⇒ a target is required: the binder satisfies each bit against the candidate with
//!   the matching object-layer relation (assist `0x6066f0` / attack `0x606980` / corpse `0x6067d0`),
//!   binds the guid and clears the bit; only a fully-cleared word commits (wire mask
//!   `TARGET_FLAG_UNIT (0x2)` + the bound guid).
//! - a candidate that satisfies nothing falls back to the **active player** — gated on the
//!   `autoSelfCast` CVar (name `0x870dc0`, gate `[0xceac34]+0x28` at `0x6e53d7`; registered with
//!   engine default `"0"`). The classic "buffing with an enemy targeted casts on yourself".
//! - still unbound ⇒ the ref leaves the nonzero flag_word standing, which *is* its targeting-cursor
//!   mode (`SpellIsTargeting 0x6e48a0` = word != 0 — the hand cursor, click to bind). That machine
//!   is unmodeled here (INTERIM): we refuse locally with the client's own error strings instead —
//!   `0x09` "You have no target." / `0x0A` "Invalid target" — and never ship an unbindable cast.
//!
//! The **location half of the targeting cursor is modeled** (decision 0792): switch enum 16 and
//! the bare DEST word (`Targets = 0x40`) resolve to [`CastWireTarget::GroundTargeting`], which
//! [`super::targeting`] turns into the Cast/UnableCast cursor + the world-click commit. Still
//! deferred, refused-not-guessed: the pure SOURCE word (0x20 — NPC-cast data only), item bits
//! 4/14 (enchants/poisons cast *at* an item), gameobject bit 11 (the OPEN_LOCK path already has
//! its own shape), STRING bit 13, and the *unit* hand-cursor mode (the residual-unit-word
//! machine behind the autoSelfCast stand-in above).

use benilla_formats::SpellDisplay;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::net::{ObjectStore, Reputations, SelfGuid, SelfPlayer};
use crate::target::{can_attack, ring_reaction, Factions, Selection};

/// `TARGET_FLAG_*` bits of the targeting flag_word (`0xcecac0`), per the byte-verified bit table
/// (`wave-cast.md` "flag_word bits"). Only the bits the resolver consumes are named.
const TF_UNIT: u16 = 0x0002;
const TF_UNIT_RAID: u16 = 0x0004;
const TF_UNIT_PARTY: u16 = 0x0008;
const TF_UNIT_ENEMY: u16 = 0x0080;
const TF_UNIT_ASSIST: u16 = 0x0100;
const TF_CORPSE_ENEMY: u16 = 0x0200;
const TF_EXPLICIT_GATE: u16 = 0x0400;
const TF_CORPSE_ALLY: u16 = 0x8000;
/// The unit-shaped bits a selected unit (alive or dead) can satisfy.
const UNIT_BITS: u16 = TF_UNIT
    | TF_UNIT_RAID
    | TF_UNIT_PARTY
    | TF_UNIT_ENEMY
    | TF_UNIT_ASSIST
    | TF_CORPSE_ENEMY
    | TF_EXPLICIT_GATE
    | TF_CORPSE_ALLY;

/// Client-side cast-failed reasons (the `CastErrors` strings): "You have no target." /
/// "Invalid target" — the INTERIM stand-ins for the unmodeled targeting-cursor mode.
pub(crate) const ERR_NO_TARGET: u8 = 0x09;
pub(crate) const ERR_INVALID_TARGET: u8 = 0x0A;

/// The dest-location bit — the ground-cast wire mask (`BindLocation 0x6e60f0`'s bit-6 arm; the
/// source bit 5 completes `TargetingWantsLocation 0x6e6320`'s `0x60`, still refused below).
const TF_DEST_LOCATION: u16 = 0x0040;

/// What the wire's target block should carry for this cast.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CastWireTarget {
    /// flag_word 0 — mask `TARGET_FLAG_SELF (0)`, no guid (the server resolves implicitly).
    SelfImplicit,
    /// A bound unit — mask `TARGET_FLAG_UNIT (0x2)` + this guid (possibly the player's own,
    /// via the autoSelfCast fallback).
    Unit(u64),
    /// A ground-targeted cast (decision 0792) — no send yet: enter the targeting-cursor mode's
    /// location half ([`super::targeting`]); the world click commits mask `0x40` + the point.
    GroundTargeting,
    /// Nothing bindable — do NOT send; surface this client error instead.
    Refused(u8),
}

/// Everything the binder's relation checks read. Both call sites (action bar, spellbook) bundle
/// the same resources; the stores are the *selected* unit's and the player's.
#[derive(Clone, Copy)]
pub(crate) struct TargetRelations<'a> {
    pub(crate) target_store: Option<&'a ObjectStore>,
    pub(crate) self_store: Option<&'a ObjectStore>,
    pub(crate) factions: Option<&'a Factions>,
    pub(crate) reputations: &'a Reputations,
}

/// The targeting inputs [`super::send_spell_cast`] resolves with, bundled by its two callers
/// (the action bar's drain, the spellbook's) so the ONE cast path owns the whole ArmCast walk.
pub(crate) struct CastContext<'a> {
    pub(crate) selection_guid: Option<u64>,
    pub(crate) self_guid: Option<u64>,
    pub(crate) auto_self_cast: bool,
    pub(crate) rel: TargetRelations<'a>,
    /// The local range gate's inputs (`IsTargetInRange 0x6e47b0` over `GetMinMaxRange
    /// 0x6e3480`) — the caster's and the selection's position + combat reach.
    pub(crate) range: RangeInputs,
}

/// Positions + combat reaches for the pre-send range refusal ([`super::send_spell_cast`]'s
/// `cast_range_refusal` leg — the client's `TryCast` runs `CanTargetUnit 0x6e4440` →
/// `IsTargetInRange 0x6e47b0` BEFORE the cast commit, so an out-of-range/too-close press
/// refuses locally and none of the commit tail (the ranged sheath snap included) runs.
#[derive(Clone, Copy)]
pub(crate) struct RangeInputs {
    pub(crate) self_pos: Option<Vec3>,
    pub(crate) target_pos: Option<Vec3>,
    pub(crate) self_reach: f32,
    pub(crate) target_reach: Option<f32>,
}

impl Default for RangeInputs {
    fn default() -> Self {
        Self {
            self_pos: None,
            target_pos: None,
            // The descriptor default reach (the state feed's own fallback).
            self_reach: 1.5,
            target_reach: None,
        }
    }
}

/// Everything a cast-sending system needs to build a [`CastContext`], as ONE [`SystemParam`] —
/// both drains stay under Bevy's system-arity ceiling and can't drift apart on inputs.
#[derive(SystemParam)]
pub(crate) struct CastTargeting<'w, 's> {
    pub(crate) selection: Res<'w, Selection>,
    pub(crate) self_store: Query<'w, 's, &'static ObjectStore, With<SelfPlayer>>,
    stores: Query<'w, 's, &'static ObjectStore>,
    self_guid: Res<'w, SelfGuid>,
    auto_self_cast: Res<'w, AutoSelfCast>,
    factions: Option<Res<'w, Factions>>,
    reputations: Res<'w, Reputations>,
    self_transform: Query<'w, 's, &'static Transform, With<SelfPlayer>>,
    transforms: Query<'w, 's, &'static Transform>,
}

impl CastTargeting<'_, '_> {
    /// The current frame's [`CastContext`] — the selection's and player's stores resolved.
    pub(crate) fn context(&self) -> CastContext<'_> {
        let target_store = self.selection.target.and_then(|e| self.stores.get(e).ok());
        CastContext {
            selection_guid: self.selection.guid,
            self_guid: self.self_guid.0,
            auto_self_cast: self.auto_self_cast.0,
            rel: TargetRelations {
                target_store,
                self_store: self.self_store.iter().next(),
                factions: self.factions.as_deref(),
                reputations: &self.reputations,
            },
            range: RangeInputs {
                self_pos: self.self_transform.iter().next().map(|t| t.translation),
                target_pos: self
                    .selection
                    .target
                    .and_then(|e| self.transforms.get(e).ok())
                    .map(|t| t.translation),
                self_reach: self
                    .self_store
                    .iter()
                    .next()
                    .map_or(1.5, |s| s.0.unit_combat_reach()),
                target_reach: target_store.map(|s| s.0.unit_combat_reach()),
            },
        }
    }
}

/// The `autoSelfCast` knob (the ref's CVar, default `"0"`). benilla defaults it **on** — a named
/// deviation: with it off, an unbindable friendly cast falls into the ref's targeting-cursor
/// machine, which benilla doesn't model yet, leaving no path at all. Flip the default to the
/// ref's when spell targeting-cursor mode lands.
#[derive(bevy::prelude::Resource)]
pub(crate) struct AutoSelfCast(pub(crate) bool);

impl Default for AutoSelfCast {
    fn default() -> Self {
        Self(true)
    }
}

/// The cast-arm's flag_word seed + implicit-target switch (`0x6e5250` @ `6e525a`–`6e52ef`):
/// `flag_word = Targets`, then one arm keyed on `EffectImplicitTargetA[0]`. The full arm map,
/// byte-verified (`wave-cast.md`): 1→clr bit10, 5→clr bit15, 6/53→set bit7, 16→ground-target,
/// 21/45→set bit8, 23→set bit11, 25/63→set bit1, 26→set bit14, 35→set bit3, 57/61→set bit2;
/// every other enum is the default no-op arm.
pub(crate) fn cast_target_mask(def: &SpellDisplay) -> u16 {
    let mut word = def.targets as u16;
    match def.implicit_target_a1 {
        1 => word &= !TF_EXPLICIT_GATE,
        5 => word &= !TF_CORPSE_ALLY,
        6 | 53 => word |= TF_UNIT_ENEMY,
        // 16 (ground-target) sets the cursor-mode flag (the ref's `bl`), not a word bit — the
        // location bits 0x60 usually arrive via `Targets` itself; both resolve to
        // `GroundTargeting` in [`resolve_cast_target`] (decision 0792).
        21 | 45 => word |= TF_UNIT_ASSIST,
        23 => word |= 0x0800,
        25 | 63 => word |= TF_UNIT,
        26 => word |= 0x4000,
        35 => word |= TF_UNIT_PARTY,
        57 | 61 => word |= TF_UNIT_RAID,
        _ => {}
    }
    word
}

/// `BindTarget 0x6e5b40`'s unit branch for one candidate: clear every flag_word bit the unit
/// satisfies (each bit its own relation check, in the binder's priority order); the caller
/// commits only on a fully-cleared word.
///
/// Relation stand-ins, named: assist (`CanAssist 0x6066f0`) is approximated as reaction rank ≥ 4
/// (friendly) — the same `UnitReaction` core the ring and `can_attack` share — pending the §5 pin
/// in flight; party/raid (`0x606c20`/`0x606d20`) accept only the player himself until groups
/// exist; the corpse predicate (`0x6067d0`) is "assistable and health 0".
fn clear_satisfied_bits(word: u16, is_self: bool, rel: &TargetRelations) -> u16 {
    let mut word = word;
    let reaction = ring_reaction(
        rel.factions,
        rel.reputations,
        rel.target_store,
        rel.self_store,
    );
    let assist = is_self || reaction >= 4;
    let dead = rel
        .target_store
        .is_some_and(|s| s.0.unit_health() == Some(0));
    if word & TF_UNIT_PARTY != 0 && is_self {
        word &= !TF_UNIT_PARTY;
    }
    if word & TF_UNIT_RAID != 0 && is_self {
        word &= !TF_UNIT_RAID;
    }
    if word & TF_UNIT_ASSIST != 0 && assist {
        word &= !TF_UNIT_ASSIST;
    }
    if word & TF_UNIT_ENEMY != 0
        && !is_self
        && can_attack(
            rel.target_store,
            rel.factions,
            rel.reputations,
            rel.self_store,
        )
    {
        word &= !TF_UNIT_ENEMY;
    }
    // Generic UNIT (bit 1) — the binder's check is the unit-flag leg, no relation: any resolved
    // unit binds. The explicit-selection gate (bit 10) carries no guid of its own; a real
    // explicit candidate discharges it alongside any unit bind.
    if word & TF_UNIT != 0 {
        word &= !TF_UNIT;
    }
    if word & TF_EXPLICIT_GATE != 0 && !is_self {
        word &= !TF_EXPLICIT_GATE;
    }
    if word & TF_CORPSE_ALLY != 0 && assist && dead {
        word &= !TF_CORPSE_ALLY;
    }
    if word & TF_CORPSE_ENEMY != 0 && !is_self && dead {
        word &= !TF_CORPSE_ENEMY;
    }
    word
}

/// Resolve the wire target for casting `def` with `selection` as the current target — the
/// ArmCast walk. `None` def (unknown spell) keeps the legacy shape: the raw selection, or
/// self-implicit without one (the server still validates).
pub(crate) fn resolve_cast_target(
    def: Option<&SpellDisplay>,
    selection_guid: Option<u64>,
    self_guid: Option<u64>,
    auto_self_cast: bool,
    rel: &TargetRelations,
) -> CastWireTarget {
    let Some(def) = def else {
        return match selection_guid {
            Some(guid) => CastWireTarget::Unit(guid),
            None => CastWireTarget::SelfImplicit,
        };
    };
    let word = cast_target_mask(def);
    if word == 0 {
        return CastWireTarget::SelfImplicit;
    }
    // Arm 16's ground fast-defer (`6e52db` sets bl, `6e535b` returns-to-cursor): a ground-arm
    // spell (Flamestrike) drops into targeting-cursor mode BEFORE any candidate bind — but only
    // after the word==0 immediate-commit above, whose order the ref fixes (`6e5338` precedes the
    // bl test).
    if def.implicit_target_a1 == 16 {
        return CastWireTarget::GroundTargeting;
    }
    // Bits outside the unit family (item/gameobject/location/string) have no candidate here.
    // The DEST-location word (Blizzard's bare `Targets = 0x40`, default switch arm) is the
    // targeting cursor's location half (decision 0792) — in the ref it falls out of the failed
    // bind walk into cursor mode (`6e50c8`); real 5875 data never combines location bits with
    // unit bits (live spell_template sweep: `Targets & 0x60` rows are exactly 0x20 or 0x40
    // alone), so deferring before the unit walk is byte-equivalent. The pure SOURCE word (0x20)
    // is NPC-cast data (Aura of Fear kin), unreachable from a player's book, and keeps the
    // refusal with the item/GO/string machines.
    if word & !UNIT_BITS != 0 {
        if word == TF_DEST_LOCATION {
            return CastWireTarget::GroundTargeting;
        }
        return CastWireTarget::Refused(ERR_INVALID_TARGET);
    }
    // Candidate 1: the current selection (ArmCast's explicit-guid leg — for a player caster the
    // `Attributes & 0x200` "caster's own target" leg resolves to the same unit).
    if let Some(guid) = selection_guid {
        let is_self = self_guid == Some(guid);
        if clear_satisfied_bits(word, is_self, rel) == 0 {
            return CastWireTarget::Unit(guid);
        }
    }
    // Candidate 2: the active player (`0x6e53d7`), behind autoSelfCast.
    if auto_self_cast {
        if let Some(guid) = self_guid {
            let self_rel = TargetRelations {
                target_store: rel.self_store,
                ..*rel
            };
            if clear_satisfied_bits(word, true, &self_rel) == 0 {
                return CastWireTarget::Unit(guid);
            }
        }
    }
    // The ref's residual-word targeting-cursor mode, refused locally (module docs).
    CastWireTarget::Refused(if selection_guid.is_some() {
        ERR_INVALID_TARGET
    } else {
        ERR_NO_TARGET
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spell(targets: u32, implicit: u32) -> SpellDisplay {
        SpellDisplay {
            targets,
            implicit_target_a1: implicit,
            ..Default::default()
        }
    }

    /// The switch arms against their pinned rows: self/party-area spells zero out, single-enemy
    /// sets the hostile bit, single-friend the assist bit, and the `Targets` seed survives.
    #[test]
    fn mask_follows_the_arm_map() {
        assert_eq!(cast_target_mask(&spell(0, 1)), 0, "Ice Armor: self");
        assert_eq!(
            cast_target_mask(&spell(0, 20)),
            0,
            "Battle Shout: no-op arm"
        );
        assert_eq!(cast_target_mask(&spell(0, 6)), TF_UNIT_ENEMY, "Fireball");
        assert_eq!(
            cast_target_mask(&spell(0, 21)),
            TF_UNIT_ASSIST,
            "Arcane Intellect"
        );
        assert_eq!(
            cast_target_mask(&spell(0x8000, 21)),
            TF_CORPSE_ALLY | TF_UNIT_ASSIST,
            "a Targets seed ORs with the switch"
        );
        assert_eq!(
            cast_target_mask(&spell(0x402, 0)),
            TF_UNIT | TF_EXPLICIT_GATE,
            "Skinning: seed only"
        );
    }

    /// The three wire shapes without any world state: mask 0 self-commits (never the selection —
    /// the Battle Shout/Ice Armor bug), unit masks refuse without a candidate, and the no-target
    /// vs wrong-target refusals use the client's two error strings.
    #[test]
    fn resolution_wire_shapes() {
        let rel = TargetRelations {
            target_store: None,
            self_store: None,
            factions: None,
            reputations: &Reputations(Vec::new()),
        };
        let ice_armor = spell(0, 1);
        assert_eq!(
            resolve_cast_target(Some(&ice_armor), Some(42), Some(1), true, &rel),
            CastWireTarget::SelfImplicit,
            "a self spell ignores the selection entirely"
        );
        let fireball = spell(0, 6);
        assert_eq!(
            resolve_cast_target(Some(&fireball), None, Some(1), true, &rel),
            CastWireTarget::Refused(ERR_NO_TARGET)
        );
        // Reaction defaults to neutral (3) with no stores: attackable (≤3), not assistable.
        assert_eq!(
            resolve_cast_target(Some(&fireball), Some(42), Some(1), true, &rel),
            CastWireTarget::Unit(42)
        );
        let intellect = spell(0, 21);
        assert_eq!(
            resolve_cast_target(Some(&intellect), Some(42), Some(1), true, &rel),
            CastWireTarget::Unit(1),
            "a friendly-required cast on a non-friend falls back to self"
        );
        assert_eq!(
            resolve_cast_target(Some(&intellect), Some(42), Some(1), false, &rel),
            CastWireTarget::Refused(ERR_INVALID_TARGET),
            "autoSelfCast off: the fallback is gated"
        );
        assert_eq!(
            resolve_cast_target(Some(&intellect), None, Some(1), true, &rel),
            CastWireTarget::Unit(1),
            "no selection at all still self-falls-back"
        );
        // A hostile-required cast never self-binds: player fails CanAttack.
        assert_eq!(
            resolve_cast_target(Some(&fireball), None, Some(1), false, &rel),
            CastWireTarget::Refused(ERR_NO_TARGET)
        );
        // Unknown spell: the legacy passthrough.
        assert_eq!(
            resolve_cast_target(None, Some(42), Some(1), true, &rel),
            CastWireTarget::Unit(42)
        );
    }

    /// The ground family resolves to `GroundTargeting` by BOTH routes (decision 0792): the
    /// arm-16 fast-defer (Flamestrike: `Targets 0x40`, implicit 16 — and even with a selection,
    /// the ref defers before any candidate bind) and the bare DEST word falling out of the bind
    /// walk (Blizzard: `Targets 0x40`, implicit 28 = default arm). The word==0 immediate commit
    /// still precedes the arm-16 check, as the ref orders them (`6e5338` before the bl test).
    #[test]
    fn ground_masks_enter_targeting_mode() {
        let flamestrike = spell(0x40, 16);
        assert_eq!(
            resolve_cast_target(Some(&flamestrike), Some(42), Some(1), true, &rel_none()),
            CastWireTarget::GroundTargeting
        );
        let blizzard = spell(0x40, 28);
        assert_eq!(
            resolve_cast_target(Some(&blizzard), None, Some(1), true, &rel_none()),
            CastWireTarget::GroundTargeting
        );
        let self_commit_with_ground_arm = spell(0, 16);
        assert_eq!(
            resolve_cast_target(
                Some(&self_commit_with_ground_arm),
                None,
                Some(1),
                true,
                &rel_none()
            ),
            CastWireTarget::SelfImplicit,
            "word==0 commits before the arm-16 defer — the ref's order"
        );
    }

    /// The still-deferred non-unit masks (source-location, item bits, string) refuse instead of
    /// shipping a guess — the machines named in the module docs.
    #[test]
    fn non_unit_masks_refuse() {
        for targets in [0x20u32, 0x10, 0x2000, 0x60] {
            let s = spell(targets, 0);
            assert_eq!(
                resolve_cast_target(Some(&s), Some(42), Some(1), true, &rel_none()),
                CastWireTarget::Refused(ERR_INVALID_TARGET),
                "Targets {targets:#x} must stay refused"
            );
        }
    }

    fn rel_none() -> TargetRelations<'static> {
        static EMPTY: Reputations = Reputations(Vec::new());
        TargetRelations {
            target_store: None,
            self_store: None,
            factions: None,
            reputations: &EMPTY,
        }
    }
}
