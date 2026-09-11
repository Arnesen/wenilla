//! The per-action **dynamic-state feed** (decision 0137 phase 4) — the app-side computation
//! behind the engine's `IsUsableAction`/`IsActionInRange`/`IsCurrentAction`/`GetActionCooldown`
//! family: each occupied action slot's [`ActionState`], recomputed per frame, diff-pushed into
//! the VM, with the reference client's own event edges fired on the transitions the 2026-07-10
//! wow-re §5 byte-mapped (`system/ui/scratch/action-button-state-api.md`):
//!
//! - a cooldown-store change → `ACTIONBAR_UPDATE_COOLDOWN` + `SPELL_UPDATE_COOLDOWN` +
//!   `BAG_UPDATE_COOLDOWN` (the `0x4b31b0`/`0x4f93d0` flush pair the SMSG handlers call);
//! - a usable/oom change on any slot → `ACTIONBAR_UPDATE_USABLE` + `SPELL_UPDATE_USABLE`
//!   (`0x4b31c0`; the client fires only on a cache CHANGE — `0x4e5c00` — hence the diff edge);
//! - a current/auto-repeat change → `ACTIONBAR_UPDATE_STATE` + `CURRENT_SPELL_CAST_CHANGED`
//!   (`0x4b3250`);
//! - our own melee engage/disengage → `PLAYER_ENTER_COMBAT`/`PLAYER_LEAVE_COMBAT`
//!   (`0x6256ff`/`0x625778` — the attack-start/stop handlers);
//! - the live autorepeat key's edges → `START_AUTOREPEAT_SPELL` (`0x6e5952`, at cast-send) /
//!   `STOP_AUTOREPEAT_SPELL` (`0x6ea170`).
//!
//! The per-flag semantics are the §5's confirmed laws (C1–C5): `notEnoughMana` is strictly the
//! power-cost verdict, `IsCurrentAction` keys on the engaged attack GUID / the in-flight cast id,
//! `IsAutoRepeatAction` on the `0xceac30` key, and the range test is squared distance against the
//! byte-verified `GetMinMaxRange 0x6e3480` (its constants transcribed below). The usable pair
//! itself is the full `IsSpellUsableNow 0x6e3d60` gate walk — [`super::usable`], the 2026-07-10
//! §2a fold-back: reagents, forms, stealth, aura states (the Execute-family target dependence),
//! the works.

use crate::ui_items::{count_of, InventoryScope};
use std::collections::HashMap;
// `bevy::platform::time::Instant`, not `bevy::platform::time::Instant`: this flows into `crate::cooldowns`/`crate::ui_script::UiClock`, which on wasm32 (the default `web` Bevy feature) is a genuinely different type from `bevy::platform::time::Instant` — a plain alias for it everywhere else.
use bevy::platform::time::Instant;

use bevy::prelude::*;

use benilla_formats::{SpellDisplay, SpellRange};
use benilla_protocol::messages::{ACTION_KIND_ITEM, ACTION_KIND_MACRO, ACTION_KIND_SPELL};
use benilla_ui::script::{ActionState, UiScript};

use crate::cooldowns::Cooldowns;
use crate::creature_anim::{Casting, Engaged};
use crate::items::Items;
use crate::net::{GuidIndex, NetCommands, ObjectStore, SelfPlayer};
use crate::target::Selection;

use super::{usable, AutoRepeatActive, PlayerActions, Spells};

/// The feed's memory: what was last pushed, and the edge detectors.
#[derive(Default)]
pub(super) struct StateMemory {
    pushed: HashMap<u32, ActionState>,
    last_generation: Option<u64>,
    engaged: bool,
    auto_repeat: Option<u32>,
    /// Last `benilla_assets::trace` "cd tick" stamp — the once-per-second gate (trace runs only).
    last_cd_trace: Option<Instant>,
}

/// The client's cast-fail reasons for the two range refusals ("Out of range." / "Target too
/// close" in `super::cast_error_text`'s table) — what `CanTargetUnit 0x6e4440` emits when
/// `IsTargetInRange 0x6e47b0` fails on its max² / min² compare.
pub(super) const ERR_OUT_OF_RANGE: u8 = 0x59;
pub(super) const ERR_TOO_CLOSE: u8 = 0x76;

/// The **pre-send** range refusal — the client's `TryCast` ladder runs `CanTargetUnit 0x6e4440`
/// → `IsTargetInRange 0x6e47b0` BEFORE `ArmCast`/`SendCast` (`wave-cast.md`, byte-verified), so
/// an out-of-range or too-close press fails locally and the commit tail — the ranged sheath
/// snap `0x6e5930` included — never runs. This is why a too-close Throw/Auto Shot must NOT draw
/// the ranged weapon. Squared 3D distance against [`benilla_formats::min_max_range`]'s {min, max}: beyond max² →
/// [`ERR_OUT_OF_RANGE`], inside a nonzero min² → [`ERR_TOO_CLOSE`]. Untestable inputs (no range
/// row, unknown distance) pass — the server still judges the cast.
pub(super) fn cast_range_refusal(
    spell: &SpellDisplay,
    row: Option<&SpellRange>,
    self_reach: f32,
    target_reach: Option<f32>,
    dist_sq: Option<f32>,
) -> Option<u8> {
    let (min, max) = benilla_formats::min_max_range(spell, row, self_reach, target_reach)?;
    let d2 = dist_sq?;
    if d2 > max * max {
        return Some(ERR_OUT_OF_RANGE);
    }
    if min > 0.0 && d2 < min * min {
        return Some(ERR_TOO_CLOSE);
    }
    None
}

/// `PreventionType` values (`Spell.dbc` column 165): which crowd-control flag can refuse this
/// spell locally. `0` = neither.
const PREVENTION_SILENCE: u32 = 1;
const PREVENTION_PACIFY: u32 = 2;

/// The **crowd-control leg** of the same requirement validator `0x6094f0`, sitting **above** its
/// mounted block (`0x609c6c`) — so a stunned mounted caster is told about the stun (decision 1904;
/// wow-re `equipped-item-and-cc-cast-gates.md` §2.1, byte-verified). It refuses before any packet,
/// which is why it must be local.
///
/// **Six arms, in the reference's order, first match wins** — 1863's fold-back recorded four:
///
/// | # | arm | gate | reason |
/// |---|---|---|---|
/// | 1 | CHARMED | `UNIT_FIELD_CHARMEDBY != 0` and the charmer is not us | `0x14` |
/// | 2 | STUNNED | bit 18 — **no per-spell gate, every spell** | `0x64` |
/// | 3 | SILENCED | bit 13 **and** `PreventionType == 1` | `0x60` |
/// | 4 | PACIFIED | bit 17 **and** `PreventionType == 2` | `0x5a` |
/// | 5 | FLEEING | bit 23 | `0x1e` |
/// | 6 | CONFUSED | bit 22 | `0x16` |
///
/// The asymmetry is the finding: STUNNED, FLEEING, CHARMED and CONFUSED carry no per-spell gate at
/// all, while SILENCED and PACIFIED run only where the spell's own `PreventionType` names them —
/// so a silence stops casts and leaves melee abilities alone, and a pacify does the reverse.
/// Reading `UNIT_FLAG_SILENCED` as "no casting" is the mistake this replaces.
///
/// **A dead caster skips all six** (`0x60980d`, a `jle` on `UNIT_FIELD_HEALTH`).
///
/// This ladder is **TryCast-only** — one caller, no address-takes — so a button does **not** grey
/// while stunned, silenced or pacified: it refuses on the press. (Fear, confuse and charm *are*
/// greyed, by a second copy of the same helpers inside `0x6e3d60`; that copy is not built here.)
///
/// Its byte-shape was nearly missed by a census twice: SILENCED's read is `f6 c4 20 test ah,0x20`,
/// a sub-register byte-lane form with no dword immediate, invisible to an immediate scan.
///
/// **The exemption scan is built** (decision 1946, closing 1925's deferral): each arm first asks
/// whether any of the caster's own auras grants immunity to what is blocking it — a scan of
/// `UNIT_FIELD_AURA[0..47]`'s raw spell ids for an aura of the arm's own type, then
/// [`benilla_formats::grants_immunity`] on each match. A hit **lifts** the refusal; a rejection
/// names the blocking mechanic and turns the arm's own reason into `0x8d`. Each arm scans for a
/// different set of aura types — charm `{6, 177, 2}`, stun `{12}`, silence `{27, 12, 60}`, pacify
/// `{25, 12, 60}`, fear `{7}`, confuse `{5}` — and those sets are the reference's, not a family
/// resemblance.
pub(crate) fn cast_cc_refusal(
    unit_flags: u32,
    health: Option<u32>,
    charmed_by_other: bool,
    spell: Option<&SpellDisplay>,
    exempt: &mut impl FnMut(&[u32]) -> benilla_formats::CcExemption,
) -> Option<(u8, Option<u32>)> {
    use crate::player::UNIT_FLAG_STUNNED;
    /// `UNIT_FIELD_FLAGS` bits 13/17/22/23 (vmangos `UnitDefines.h`).
    const UNIT_FLAG_SILENCED: u32 = 0x0000_2000;
    const UNIT_FLAG_PACIFIED: u32 = 0x0002_0000;
    const UNIT_FLAG_CONFUSED: u32 = 0x0040_0000;
    const UNIT_FLAG_FLEEING: u32 = 0x0080_0000;

    // The dead caster's skip (`0x60980d`): a corpse is refused by an earlier rung, not this one.
    if health == Some(0) {
        return None;
    }
    let prevention = spell.map_or(0, |d| d.prevention_type);
    // One arm: ask its exemption scan first, and let its answer pick between silence and one of
    // two messages. `exempt` returning `exempt: true` means one of the caster's own auras grants
    // immunity to whatever is blocking — the arm is SKIPPED and the cast proceeds. Otherwise the
    // refusal is `0x8d` "Can't do that while %s" when the scan named a mechanic, and the arm's own
    // reason when it did not (decision 1946).
    let mut arm = |aura_types: &[u32], own_reason: u8| -> Option<(u8, Option<u32>)> {
        let scan = exempt(aura_types);
        if scan.exempt {
            return None;
        }
        Some(if scan.mechanic != 0 {
            // The mechanic rides along as the message's `%s` — the one client-LOCAL refusal that
            // carries an argument word (decision 1948).
            (REASON_PREVENTED_BY_MECHANIC, Some(scan.mechanic))
        } else {
            (own_reason, None)
        })
    };

    if charmed_by_other {
        if let Some(r) = arm(&[6, 177, 2], 0x14) {
            return Some(r);
        }
    }
    if unit_flags & UNIT_FLAG_STUNNED != 0 {
        if let Some(r) = arm(&[12], 0x64) {
            return Some(r);
        }
    }
    if unit_flags & UNIT_FLAG_SILENCED != 0 && prevention == PREVENTION_SILENCE {
        if let Some(r) = arm(&[27, 12, 60], 0x60) {
            return Some(r);
        }
    }
    if unit_flags & UNIT_FLAG_PACIFIED != 0 && prevention == PREVENTION_PACIFY {
        if let Some(r) = arm(&[25, 12, 60], 0x5a) {
            return Some(r);
        }
    }
    if unit_flags & UNIT_FLAG_FLEEING != 0 {
        if let Some(r) = arm(&[7], 0x1e) {
            return Some(r);
        }
    }
    if unit_flags & UNIT_FLAG_CONFUSED != 0 {
        if let Some(r) = arm(&[5], 0x16) {
            return Some(r);
        }
    }
    None
}

/// `SPELL_FAILED_PREVENTED_BY_MECHANIC` — "Can't do that while %s", `%s` being the blocking aura's
/// `SpellMechanic.dbc` name. Every crowd-control arm carries this **as well as** its own reason,
/// and which one appears is decided by whether the exemption scan named a mechanic.
const REASON_PREVENTED_BY_MECHANIC: u8 = 0x8d;

/// The **pre-send** mounted refusal (decision 0481) — the requirement validator `0x6094f0`'s
/// mounted block (`0x609c6c`, wow-re `mounted-action-gate.md` §5): a live
/// `UNIT_FIELD_MOUNTDISPLAYID` refuses the cast with reason `0x39` ("You are mounted") unless
/// the spell carries Attributes bit 24 (`0x01000000`, castable-while-mounted — the exemption
/// test at `0x609c6f`, the exact vmangos `SPELL_ATTR_ALLOW_WHILE_MOUNTED` mirror). A spell
/// with no loaded record has no exemption to claim — the gate holds (the ref always has the
/// record; refusing without data errs toward the ref's visible behavior). The sibling
/// mount-REQUIRED gate (reason 0x53, `0x609c05`) is recorded but unbuilt — no 1.12 player spell
/// exercises it. It is **two-armed** like the water pair below, not the single `+0x5c & 0x40`
/// read this comment used to claim: arm A is `AuraInterruptFlags & 0x40` (the `cl` at
/// `0x609c05` is the untouched low byte of `ecx = [esi+0x58]`, loaded 0x104 bytes earlier for
/// the unsheathed leg), arm B is `ChannelInterruptFlags & 0x40` at `0x609c3a` — corrected by
/// the 1063 §5 in wow-re `mounted-action-gate.md`. Note the plain mounted gate above is NOT in
/// that family: it is single-armed on `Attributes`.
pub(crate) fn cast_mounted_refusal(mounted: bool, spell: Option<&SpellDisplay>) -> bool {
    mounted && spell.is_none_or(|d| d.attributes & 0x0100_0000 == 0)
}

/// The interrupt-flag water pair — the two bits that say *which side of the surface this aura
/// can live on*: `0x80` cancels it on ENTERING water, `0x100` on LEAVING it (vmangos
/// `AURA_INTERRUPT_UNDER_WATER_CANCELS` / `AURA_INTERRUPT_ABOVE_WATER_CANCELS`, bits 7/8). The
/// requirement validator reads the same pair to refuse the *cast* whose aura could not survive
/// the caster's current side.
const INTERRUPT_UNDER_WATER: u32 = 0x80;
const INTERRUPT_ABOVE_WATER: u32 = 0x100;

/// `AttributesEx & (IS_CHANNELED 0x4 | IS_SELF_CHANNELED 0x40)` — the gate on **arm B** of every
/// leg in this family (byte-verified `0x609d6a`/`0x609db1`). A channeled spell's requirement
/// bits live in its CHANNEL column, so the validator reads that column too — but only for a
/// spell that actually channels, which is what keeps Summon Baby Shark 25849 (`Channel 0x100`,
/// `AttributesEx 0`) out of the gate.
const ATTR_EX_CHANNELED: u32 = 0x44;

/// `SPELL_FAILED_ONLY_ABOVEWATER` — "Cannot use while swimming".
pub(super) const ERR_ONLY_ABOVEWATER: u8 = 0x50;
/// `SPELL_FAILED_ONLY_UNDERWATER` — "Can only use while swimming".
pub(super) const ERR_ONLY_UNDERWATER: u8 = 0x58;

/// The **pre-send** water refusal (decisions 1056 + 1063) — the requirement validator
/// `0x6094f0`'s environment block `0x609d33–0x609de2`, byte-carved by the wow-re §5 trio
/// (`system/spell/scratch/water-cast-gate.md`). It sits after the mounted/posture/day/night
/// legs and before the moving gate (`0x609de3`), which is where the ladder runs it — so a druid
/// standing on land is refused **before** the form gate `0x612480` ever evaluates.
///
/// One gate, two faces, and each face has **two arms** reading the same bit in two columns:
///
/// | reason | bit | arm A — `AuraInterruptFlags` (+0x58) | arm B — `ChannelInterruptFlags` (+0x5c), if channeled |
/// |---|---|---|---|
/// | [`ERR_ONLY_UNDERWATER`] `0x58` | `0x100` | `0x609d36` / swim `0x609d46` | `0x609d6f` / swim `0x609d7b` |
/// | [`ERR_ONLY_ABOVEWATER`] `0x50` | `0x80` | `0x609da4` / swim `0x609dac` | `0x609db5` / swim `0x609dc2` |
///
/// The swimming state is `[[caster+0x118]+0x40] & 0x200000` — the same wire-layout movement word
/// the moving gate reads, four times over, twice in each face.
///
/// **Arm B is not dead code, and leaving it out is visibly wrong**: Fishing rank 1 (7620)
/// carries `AuraInterruptFlags 0x80`, but ranks 2–4 (7731/7732/18248) carry **zero** and reach
/// the gate only through their `ChannelInterruptFlags 0x3cac`. Arm A alone would refuse rank 1
/// mid-swim and let its own upgrades through.
///
/// What the two bits actually select in the 5875 data: `0x80` is 245 rows — every mount, Travel
/// Form, the campfires, all of Food/Drink (`0x40080`), Fishing — and `0x100` is exactly five:
/// Aquatic Form 1066, the Lava/Slime swim auras 16455/16456, Master Angler 24346/24347.
///
/// **No exemption skips this block** (every jump into the run was enumerated) — unlike the
/// mounted gate's `Attributes` bit 24. And the gate must be LOCAL: vmangos's `CheckCast` gates
/// water only on `SPELL_AURA_MOUNTED` (`Spell.cpp:6379`), so it happily grants a druid aquatic
/// form on dry cobblestone (ledger B176, with the screenshot to prove it). An uncataloged spell
/// passes, like every other data-driven rung on this ladder.
///
/// The legs are evaluated `0x58` before `0x50`, the binary's own order; they are mutually
/// exclusive on the swim bit, so the order is fidelity, not behaviour.
pub(super) fn cast_water_refusal(move_flags_word: u32, spell: Option<&SpellDisplay>) -> Option<u8> {
    let d = spell?;
    let swimming = move_flags_word & crate::creature_anim::move_flags::SWIMMING != 0;
    // Arm A always; arm B only for a spell that actually channels.
    let requires = |bit: u32| {
        d.aura_interrupt_flags & bit != 0
            || (d.attributes_ex & ATTR_EX_CHANNELED != 0 && d.channel_interrupt_flags & bit != 0)
    };
    if !swimming && requires(INTERRUPT_ABOVE_WATER) {
        return Some(ERR_ONLY_UNDERWATER);
    }
    if swimming && requires(INTERRUPT_UNDER_WATER) {
        return Some(ERR_ONLY_ABOVEWATER);
    }
    None
}

/// The AuraInterruptFlags-space MOVING|TURNING pair (`0x18`) — the moving gate's
/// "movement would matter anyway" arms test it on BOTH `AuraInterruptFlags` (+0x58) and
/// `ChannelInterruptFlags` (+0x5c), byte-verified at `0x609e0e`/`0x609e1c`.
const AURA_INTERRUPT_MOVING_TURNING: u32 = 0x18;

/// The **pre-send** moving refusal (decision 0862) — the requirement validator `0x6094f0`'s
/// moving block (`0x609de3–0x609e48`; the sole client-local emitter of reason `0x2e` "Can't do
/// that while moving". wow-re `moving-cast-gate.md`, §5 byte-verified): a press while the
/// caster's live CMovement flags carry any of {forward, backward, strafe L/R, JUMPING} refuses
/// locally — no packet, no cast bar, no GCD. Without it, vmangos *accepts* the cast (its
/// CheckCast moving-reject covers only autorepeat/sit-still spells, `Spell.cpp:5432`) and then
/// `Spell::update`'s 0.5-yd movement interrupt kills it — the start-then-cancel grief this gate
/// exists to prevent. The full reject condition, gate for gate:
///
/// - **entry**: `InterruptFlags & 0x1` (movement-interruptible — instants without it pass);
/// - **the movement word**: the WIRE `MovementFlags` layout (`[unit+0x9a8]+0x40`), mask
///   `0x200f` = forward|backward|strafe + JUMPING — turning and pitch are outside it, and so is
///   FALLINGFAR (`0x4000`; the client has NO falling/Stuck exemption — that's vmangos-only);
/// - **exemption**: an auto-repeat spell (`AttributesEx2 & 0x20` — Auto Shot, Shoot) never
///   refuses, whatever else it carries;
/// - **would movement matter**: a nonzero resolved cast time ([`super::Spells::cast_time_ms`]),
///   OR the [`AURA_INTERRUPT_MOVING_TURNING`] bits on the aura/channel interrupt columns — the
///   OR-arms are how a zero-cast-time *channel* is still refused at initiation.
///
/// An uncataloged spell passes (every record-read above needs the row; the ladder's other
/// data-driven legs — cooldown, GCD, range — share the disposition, and the server's own
/// interrupt stays the safety net). In the validator's order this sits after the mounted block
/// and before the shapeshift-form leg (`0x609e50`), which is where [`super::send_spell_cast`]
/// runs it.
pub(super) fn cast_moving_refusal(
    move_flags_word: u32,
    cast_time_ms: u32,
    spell: Option<&SpellDisplay>,
) -> bool {
    use crate::creature_anim::move_flags;
    // The verified 0x200f: ANY_MOVE (0xf) | FALLING (0x2000), in our identical wire layout.
    const MOVING_MASK: u32 = move_flags::ANY_MOVE | move_flags::FALLING;
    let Some(d) = spell else { return false };
    d.interrupt_flags & crate::ui_cast::SPELL_INTERRUPT_MOVEMENT != 0
        && move_flags_word & MOVING_MASK != 0
        && !d.auto_repeat()
        && (cast_time_ms != 0
            || d.aura_interrupt_flags & AURA_INTERRUPT_MOVING_TURNING != 0
            || d.channel_interrupt_flags & AURA_INTERRUPT_MOVING_TURNING != 0)
}

/// What a slot *is* once the MACRO indirection is applied — the reference's slot→spell resolver
/// `0x4e5a50` plus the leg of the usable compute `0x4e5050` that reads its zero (decision 1636).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotResolve {
    /// The slot IS this `(kind, id)` from here down: a SPELL or ITEM slot, or a macro whose
    /// bound spell is live (`[rec+0x564] > 0`).
    Action(u8, u32),
    /// A macro that exists but casts nothing (`[rec+0x564] == 0`): `0x4e5050`'s spell-less leg
    /// (`0x4e50f4`–`0x4e516f`) answers **usable=1** off `0x4e5030` — "the slot's macro id is in
    /// the macro table" — and computes nothing else: no cooldown, no range, no checked ring.
    BareMacro,
    /// Not usable and nothing to report: a `/cast` whose name did not resolve (`-1`, which the
    /// spell path refuses at `0x4e518b: jl`), or a slot whose macro no longer exists (`0x4e5030`
    /// is 0 and `IsActionActive 0x4e55f0` has no spell to find).
    Dead,
}

/// Resolve a slot **through** a macro before any state is computed — the reference's own shape,
/// and the reason a macro button on the bar wears its spell's cooldown swirl, usability tint,
/// range colour and checked ring while showing its own icon.
///
/// Every `Is*Action`/`GetActionCooldown` binding routes through the one slot→spell resolver
/// `0x4e5a50`, whose MACRO arm resolves the macro record and returns `[rec+0x564]` as the slot's
/// spell id (wow-re `action-spell-icon-apis.md` §2, VERIFIED). So from here down, a macro that
/// casts Fireball simply *is* the Fireball slot. `GetActionTexture` is the deliberate exception —
/// its macro arm keeps the macro's own icon (`super::feed`).
///
/// The zero is NOT "nothing to report" (0983's reading — B340's grey `.spawn` macro): the field
/// is three-valued and the usable compute reads each value differently, which [`SlotResolve`]
/// carries. Only the SPELL indirection is modelled: 1.12 has no `/use <item>` slash command, so
/// no 1.12 macro body can name an item and the resolver's item leg is unreachable from one.
fn resolve_through_macro(
    kind: u8,
    action: u32,
    bound: &crate::ui_macro::MacroBoundSpells,
) -> SlotResolve {
    use crate::ui_macro::BoundSpell;
    match kind {
        ACTION_KIND_MACRO => match bound.0.get(&action) {
            Some(BoundSpell::Spell(s)) => SlotResolve::Action(ACTION_KIND_SPELL, *s),
            Some(BoundSpell::None) => SlotResolve::BareMacro,
            Some(BoundSpell::Unresolved) | None => SlotResolve::Dead,
        },
        other => SlotResolve::Action(other, action),
    }
}

/// Compute + diff-push every occupied slot's dynamic state, and fire the reference event edges.
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // a Bevy system's full input set
pub(super) fn feed_action_state(
    script: Option<NonSendMut<UiScript>>,
    actions: Res<PlayerActions>,
    spells: Option<Res<Spells>>,
    mut cooldowns: ResMut<Cooldowns>,
    clock: Res<crate::ui_script::UiClock>,
    auto_repeat: Res<AutoRepeatActive>,
    // One tuple param (Bevy's 16-SystemParam ceiling): our own cast tracking — the in-flight
    // guard, the queued on-next-swing strike, the running channel, and the awaiting-click
    // ground targeting — plus the macro→spell binding the MACRO arm resolves through
    // (decision 0983), which rides here for the same ceiling reason.
    cast_state: (
        Res<crate::ui_cast::PendingCast>,
        Res<crate::ui_cast::QueuedMeleeSpell>,
        Res<crate::ui_cast::ActiveChannel>,
        Res<super::SpellTargeting>,
        Res<crate::ui_macro::MacroBoundSpells>,
    ),
    self_q: Query<(&ObjectStore, &Transform, Has<Engaged>, Option<&Casting>), With<SelfPlayer>>,
    selection: Res<Selection>,
    index: Res<GuidIndex>,
    units: Query<(&ObjectStore, &Transform), Without<SelfPlayer>>,
    factions: Option<Res<crate::target::Factions>>,
    reputations: Res<crate::net::Reputations>,
    mut items: ResMut<Items>,
    commands: Res<NetCommands>,
    mut memory: Local<crate::ui_script::VmMemo<StateMemory>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let memory = memory.get(&script);
    let now = Instant::now();
    // The frame's atomic clock pair — `ui_triple`'s conversion base: every cooldown is pushed as
    // its absolute start on the GetTime clock, derived through the ONE lawful pair
    // ([`crate::ui_script::UiClock`]) so a running cooldown re-derives the same start every frame.
    let (anchor, ui_now) = (clock.anchor, clock.ui_now);
    cooldowns.prune(now);
    let gen_changed = memory.last_generation != Some(cooldowns.generation);
    memory.last_generation = Some(cooldowns.generation);
    // The cooldown-clock trace (`WOW_MOVE_TRACE` sink, tag "cd"): once per second.
    let trace_cd = benilla_assets::trace::enabled()
        && memory
            .last_cd_trace
            .is_none_or(|t| now.duration_since(t).as_secs_f32() >= 1.0);
    if trace_cd {
        memory.last_cd_trace = Some(now);
    }

    let (pending, queued_melee, channel, targeting, bound) = &cast_state;
    let me = self_q.iter().next();
    let engaged = me.is_some_and(|(_, _, e, _)| e);
    let form_byte = me
        .map(|(s, _, _, _)| s.0.unit_shapeshift_form())
        .unwrap_or(0);
    let casting_spell = me.and_then(|(_, _, _, c)| c.map(|c| c.spell_id));
    let current_cast = pending.current(now).or(casting_spell);
    let self_reach = me.map_or(1.5, |(s, _, _, _)| s.0.unit_combat_reach());
    let self_pos = me.map(|(_, t, _, _)| t.translation);
    // The current target's reach + squared distance (the client tests dx²+dy²+dz² — 0x6e47b0).
    let target = selection
        .guid
        .and_then(|g| index.0.get(&g))
        .and_then(|&e| units.get(e).ok());
    let target_reach = target.map(|(s, _)| s.0.unit_combat_reach());
    let dist_sq = match (self_pos, target) {
        (Some(a), Some((_, t))) => Some(a.distance_squared(t.translation)),
        _ => None,
    };

    let mut fresh: HashMap<u32, ActionState> = HashMap::new();
    for (&slot, button) in &actions.buttons {
        let action = u32::from(slot) + 1;
        let mut st = ActionState::default();
        let (kind, id) = match resolve_through_macro(button.kind, button.action, bound) {
            SlotResolve::Action(kind, id) => (kind, id),
            SlotResolve::BareMacro => {
                // The spell-less leg of `0x4e5050`: the macro exists, so the slot is usable —
                // full colour on the bar — and there is no other state to compute (1636). The
                // leg's one gate benilla does not model is `[0xb4b3e4]`, the player-control
                // flag (wow-re `right-click-open.md` §3.1: 1 from boot, 0 only across a control
                // loss — taxi/fear/charm); for that span the reference greys every spell-less
                // macro and item.
                st.usable = true;
                fresh.insert(action, st);
                continue;
            }
            SlotResolve::Dead => {
                fresh.insert(action, st);
                continue;
            }
        };
        let button = &benilla_protocol::messages::ActionButton {
            slot,
            action: id,
            kind,
        };
        match button.kind {
            ACTION_KIND_SPELL => {
                let d = spells.as_ref().and_then(|s| s.catalog.get(button.action));
                let Some(d) = d else {
                    fresh.insert(action, st);
                    continue;
                };
                st.is_attack = d.is_melee_auto_attack();
                // C2: the Attack action is "current" while auto-attack is engaged; a castable
                // spell while it is our in-flight cast OR our queued on-next-swing strike OR our
                // running channel (the ref reads one inflight id `0xceca88` — which a queued
                // Heroic Strike *occupies* until the swing fires it — plus the channel id
                // `0xceac58`; our model splits the queue into its own slot, same observable) —
                // OR the shapeshift arm (`IsCurrentAction`'s predicate `0x4e53a0` @ `0x4e5556`,
                // wow-re `action-spell-icon-apis.md` §5): a MOD_SHAPESHIFT spell whose form ==
                // the player's form byte reads checked. Deliberately NOT the icon's aura-scan
                // predicate — the two are different functions in the binary and the asymmetry
                // is load-bearing (a form granted by a different spell lights the check without
                // swapping the icon).
                st.current = if st.is_attack {
                    engaged
                } else {
                    current_cast == Some(button.action)
                        || queued_melee.current() == Some(button.action)
                        || channel.current(now) == Some(button.action)
                        // The awaiting-target arm (`0x4e53a0` @ `0x4e54d0`: the `0x6e48e0`
                        // targeting-spell read) — checked while the ground click is pending.
                        || targeting.spell() == Some(button.action)
                        || (form_byte != 0 && d.shapeshift_form == Some(u32::from(form_byte)))
                };
                st.auto_repeat = auto_repeat.0 == Some(button.action);
                // The full usable walk (`0x6e3d60` §2a — [`super::usable`]): reagents, combo
                // points, forms, stealth, aura states, the bit-25 cooldown fold, and the power
                // gate (the sole notEnoughMana writer). Target-dependent for the Execute family
                // only. `spells` is necessarily Some here — `d` came out of it.
                if let (Some((store, _, _, _)), Some(sp)) = (me, spells.as_deref()) {
                    let ctx = usable::UsableCtx {
                        store,
                        target_store: target.map(|(s, _)| s),
                        factions: factions.as_deref(),
                        reputations: &reputations,
                        cooldowns: &cooldowns,
                    };
                    let (u, oom) =
                        usable::spell_usable(button.action, d, sp, &ctx, &mut items, &commands);
                    st.usable = u;
                    st.not_enough_mana = oom;
                } else {
                    st.usable = true;
                }
                // C4: the range verdict vs the current target; nil without one.
                let row = spells.as_ref().and_then(|s| s.ranges.get(d.range_index));
                let resolved = benilla_formats::min_max_range(d, row, self_reach, target_reach);
                st.has_range = resolved
                    .is_some_and(|(min, max)| min.abs() > f32::EPSILON || max.abs() > f32::EPSILON);
                st.in_range = match (resolved, dist_sq) {
                    (Some((min, max)), Some(d2)) if st.has_range => {
                        Some(d2 >= min * min && d2 <= max * max)
                    }
                    _ => None,
                };
                let info = cooldowns.info(button.action, 0, Some(d), now);
                st.cooldown = info.ui_triple(anchor, ui_now);
                if st.cooldown.is_some() && trace_cd {
                    // The store (Instant clock) vs the widget (GetTime clock) — the sink
                    // stamps the wall time, so drift between the two clocks reads directly.
                    benilla_assets::trace::line(
                        "cd",
                        &format!(
                            "tick action={} rem={}ms dur={}ms engine_now={ui_now:.3}",
                            button.action, info.remaining_ms, info.duration_ms,
                        ),
                    );
                }
            }
            ACTION_KIND_ITEM => {
                let template = items.template(button.action, 0, &commands).cloned();
                // `IsConsumableAction` is NOT fed from here. It reads nothing but this template
                // (`0x4e5250`), so it is slot IDENTITY, and it rides the identity feed's push
                // beside the count it gates — `super::feed`'s ITEM arm, decision 1301.
                let count = me
                    .map(|(s, _, _, _)| {
                        count_of(&s.0, &items, button.action, InventoryScope::CARRIED)
                    })
                    .unwrap_or(0);
                // Worn on any equipment slot (0..18) — the green border's IsEquippedAction.
                st.equipped = me.is_some_and(|(s, _, _, _)| {
                    (0..19).any(|i| {
                        s.0.player_inv_slot(i)
                            .and_then(|g| items.object(g))
                            .and_then(|o| o.object_entry())
                            == Some(button.action)
                    })
                });
                st.usable = count > 0 || st.equipped;
                if let Some(u) = template.as_ref().and_then(|t| t.use_spell) {
                    let d = spells.as_ref().and_then(|s| s.catalog.get(u.spell_id));
                    let info = cooldowns.info(u.spell_id, button.action, d, now);
                    st.cooldown = info.ui_triple(anchor, ui_now);
                }
            }
            _ => {}
        }
        // No between-generation carry: the triple holds the ABSOLUTE start, so one running
        // cooldown re-derives the same value every frame (no diff churn) and a re-arm derives a
        // new one (the sweep restarts). The old `(remaining, duration)` carry-the-stale-triple
        // scheme aliased a fail-clear+re-arm inside one inter-feed gap into "unchanged" — the
        // vanished-GCD-pie-on-spam bug.
        fresh.insert(action, st);
    }

    // Diff-push + collect which event families changed.
    let mut usable_changed = false;
    let mut state_changed = false;
    let keys: Vec<u32> = fresh
        .keys()
        .chain(memory.pushed.keys())
        .copied()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    for action in keys {
        let (new, old) = (fresh.get(&action), memory.pushed.get(&action));
        if new == old {
            continue;
        }
        let d = ActionState::default();
        let (n, o) = (new.unwrap_or(&d), old.unwrap_or(&d));
        if (n.usable, n.not_enough_mana) != (o.usable, o.not_enough_mana) {
            usable_changed = true;
        }
        if (n.current, n.auto_repeat) != (o.current, o.auto_repeat) {
            state_changed = true;
        }
        if n.cooldown != o.cooldown && benilla_assets::trace::enabled() {
            benilla_assets::trace::line(
                "cd",
                &format!(
                    "push action={action} cooldown={:?} engine_now={:.3}",
                    n.cooldown,
                    script.now()
                ),
            );
        }
        script.set_action_state(action, new.copied());
    }
    memory.pushed = fresh;

    // The event edges, in the client's own flush order (the ACTIONBAR_* sibling first).
    if gen_changed {
        script.fire_event("ACTIONBAR_UPDATE_COOLDOWN", vec![]);
        script.fire_event("SPELL_UPDATE_COOLDOWN", vec![]);
        script.fire_event("BAG_UPDATE_COOLDOWN", vec![]);
    }
    if usable_changed {
        script.fire_event("ACTIONBAR_UPDATE_USABLE", vec![]);
        script.fire_event("SPELL_UPDATE_USABLE", vec![]);
    }
    if state_changed {
        script.fire_event("ACTIONBAR_UPDATE_STATE", vec![]);
        script.fire_event("CURRENT_SPELL_CAST_CHANGED", vec![]);
    }
    if engaged != memory.engaged {
        memory.engaged = engaged;
        script.fire_event(
            if engaged {
                "PLAYER_ENTER_COMBAT"
            } else {
                "PLAYER_LEAVE_COMBAT"
            },
            vec![],
        );
    }
    if auto_repeat.0 != memory.auto_repeat {
        memory.auto_repeat = auto_repeat.0;
        script.fire_event(
            if auto_repeat.0.is_some() {
                "START_AUTOREPEAT_SPELL"
            } else {
                "STOP_AUTOREPEAT_SPELL"
            },
            vec![],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spell_with_range(range_index: u32, attributes: u32) -> SpellDisplay {
        SpellDisplay {
            range_index,
            attributes,
            ..Default::default()
        }
    }

    /// The pre-send refusal (`IsTargetInRange 0x6e47b0`'s two compares over the resolved
    /// bounds): Auto Shot's {8, 35} row + the unit reach pad — a point-blank target refuses
    /// TOO_CLOSE, a distant one OUT_OF_RANGE, the sweet spot passes; untestable inputs pass.
    #[test]
    fn cast_range_refusal_follows_the_two_compares() {
        let d = spell_with_range(114, 0);
        let auto_shot = SpellRange {
            min: 8.0,
            max: 35.0,
            flags: 0,
        };
        let reach = Some(1.5);
        // Both bounds carry the bare reach pad (self 1.5 + target 1.5): min = 11, max = 38.
        let refuse = |d2: f32| cast_range_refusal(&d, Some(&auto_shot), 1.5, reach, Some(d2));
        assert_eq!(refuse(3.0 * 3.0), Some(ERR_TOO_CLOSE));
        assert_eq!(refuse(20.0 * 20.0), None);
        assert_eq!(refuse(60.0 * 60.0), Some(ERR_OUT_OF_RANGE));

        // The regression: a min-0 ranged row (Fireball/Shadow Bolt) must pass point-blank —
        // its min never grows a reach pad — while the max compare still holds.
        let fireball = SpellRange {
            min: 0.0,
            max: 35.0,
            flags: 0,
        };
        let refuse = |d2: f32| cast_range_refusal(&d, Some(&fireball), 1.5, reach, Some(d2));
        assert_eq!(refuse(0.1), None);
        assert_eq!(refuse(60.0 * 60.0), Some(ERR_OUT_OF_RANGE));

        // A melee-family row has min 0 — never TOO_CLOSE, still OUT_OF_RANGE beyond reach.
        let melee = SpellRange {
            min: 0.0,
            max: 5.0,
            flags: 1,
        };
        let melee_spell = spell_with_range(2, 0);
        assert_eq!(
            cast_range_refusal(&melee_spell, Some(&melee), 1.5, reach, Some(0.1)),
            None
        );
        assert_eq!(
            cast_range_refusal(&melee_spell, Some(&melee), 1.5, reach, Some(15.0 * 15.0)),
            Some(ERR_OUT_OF_RANGE)
        );

        // No row / no distance: nothing to test locally — pass (the server judges).
        assert_eq!(cast_range_refusal(&d, None, 1.5, reach, Some(1.0)), None);
        assert_eq!(
            cast_range_refusal(&d, Some(&auto_shot), 1.5, reach, None),
            None
        );
    }

    /// A MACRO slot resolves through its bound spell for EVERY dynamic read (decision 0983) —
    /// the `0x4e5a50` law — and the three values of `[rec+0x564]` split three ways at the usable
    /// compute (decision 1636): a live spell IS that spell; a macro that casts nothing is a bare,
    /// usable button (B340's `.spawn` macro); an unresolved `/cast` — or a slot whose macro is
    /// gone — is grey.
    #[test]
    fn a_macro_slot_resolves_through_its_bound_spell() {
        use crate::ui_macro::BoundSpell;
        use benilla_protocol::messages::ACTION_KIND_MACRO;

        let mut bound = crate::ui_macro::MacroBoundSpells::default();
        bound.0.insert(3, BoundSpell::Spell(133)); // macro 3 casts Fireball
        bound.0.insert(4, BoundSpell::None); // macro 4 is `.spawn 16032`
        bound.0.insert(5, BoundSpell::Unresolved); // macro 5 is `/cast Pyroblast`, unknown

        assert_eq!(
            resolve_through_macro(ACTION_KIND_MACRO, 3, &bound),
            SlotResolve::Action(ACTION_KIND_SPELL, 133),
            "from here down the macro IS the Fireball slot"
        );
        assert_eq!(
            resolve_through_macro(ACTION_KIND_MACRO, 4, &bound),
            SlotResolve::BareMacro,
            "a macro that casts nothing is usable, with no cooldown, no range"
        );
        assert_eq!(
            resolve_through_macro(ACTION_KIND_MACRO, 5, &bound),
            SlotResolve::Dead,
            "a /cast of an unknown spell is the reference's -1: grey"
        );
        assert_eq!(
            resolve_through_macro(ACTION_KIND_MACRO, 6, &bound),
            SlotResolve::Dead,
            "a slot whose macro no longer exists: grey"
        );
        // Spell and item slots pass through untouched.
        assert_eq!(
            resolve_through_macro(ACTION_KIND_SPELL, 133, &bound),
            SlotResolve::Action(ACTION_KIND_SPELL, 133)
        );
        assert_eq!(
            resolve_through_macro(ACTION_KIND_ITEM, 117, &bound),
            SlotResolve::Action(ACTION_KIND_ITEM, 117)
        );
    }

    /// The feed end to end, at the symptom (B340): a MACRO slot whose macro casts nothing is
    /// pushed **usable** — `IsUsableAction` answers true in the VM, the full-colour icon — while
    /// a `/cast` of an unknown spell, and a slot whose macro is gone, are pushed grey. The
    /// pre-1636 feed pushed `ActionState::default()` for all three, whose `usable` is false: every
    /// GM `.spawn` macro on the bar was grey.
    #[test]
    fn the_feed_pushes_a_bare_macro_as_usable() {
        use crate::ui_macro::{BoundSpell, MacroBoundSpells};
        use benilla_protocol::messages::{ActionButton, ACTION_KIND_MACRO};

        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut app = App::new();
        let mut actions = PlayerActions::default();
        for (slot, index) in [(0u8, 1u32), (1, 2), (2, 3)] {
            actions.buttons.insert(
                slot,
                ActionButton {
                    slot,
                    action: index,
                    kind: ACTION_KIND_MACRO,
                },
            );
        }
        // Macro 1 is `.spawn 16032`, macro 2 is `/cast Pyroblast` with Pyroblast unknown, and
        // macro 3 does not exist.
        let mut bound = MacroBoundSpells::default();
        bound.0.insert(1, BoundSpell::None);
        bound.0.insert(2, BoundSpell::Unresolved);
        app.insert_resource(actions)
            .insert_resource(bound)
            .init_resource::<Cooldowns>()
            .init_resource::<crate::ui_script::UiClock>()
            .init_resource::<AutoRepeatActive>()
            .init_resource::<crate::ui_cast::PendingCast>()
            .init_resource::<crate::ui_cast::QueuedMeleeSpell>()
            .init_resource::<crate::ui_cast::ActiveChannel>()
            .init_resource::<crate::ui_action::SpellTargeting>()
            .init_resource::<Selection>()
            .init_resource::<GuidIndex>()
            .init_resource::<crate::net::Reputations>()
            .init_resource::<Items>()
            .insert_resource(NetCommands(tx));
        app.insert_non_send_resource(UiScript::new().unwrap());
        app.add_systems(Update, feed_action_state);
        app.update();

        let script = app.world().non_send_resource::<UiScript>();
        let usable = |action: u32| {
            script
                .eval::<bool>(&format!(
                    "return (IsUsableAction({action})) and true or false"
                ))
                .unwrap()
        };
        assert!(usable(1), "a macro that casts nothing is a usable button");
        assert!(
            !usable(2),
            "a /cast of an unknown spell is grey (the reference's -1)"
        );
        assert!(!usable(3), "a slot whose macro no longer exists is grey");
        assert!(
            !script
                .eval::<bool>("local _, oom = IsUsableAction(2) return oom and true or false")
                .unwrap(),
            "grey, not the out-of-power blue: notEnoughMana stays 0 on the spell-less leg"
        );
    }

    /// The mounted refusal (`0x609c6c`): a live mount blocks unless Attributes carries the
    /// bit-24 exemption (`0x609c6f`); unmounted always passes; a missing record can claim no
    /// exemption, so the gate holds.
    #[test]
    fn cast_mounted_refusal_honors_the_bit24_exemption() {
        let plain = SpellDisplay::default();
        let exempt = SpellDisplay {
            attributes: 0x0100_0000,
            ..Default::default()
        };
        assert!(cast_mounted_refusal(true, Some(&plain)));
        assert!(!cast_mounted_refusal(true, Some(&exempt)));
        assert!(!cast_mounted_refusal(false, Some(&plain)));
        assert!(!cast_mounted_refusal(false, None));
        assert!(cast_mounted_refusal(true, None), "no record, no exemption");
    }

    /// The water refusal (`0x609d33–0x609de2`) — both faces of the environment gate and both
    /// arms of each face, on the real 5875 columns: Aquatic Form's `0x100` needs the SWIMMING
    /// bit set, the mount/Travel-Form/food `0x80` needs it clear, Fishing's ranks reach it
    /// through the CHANNEL column, and a spell carrying neither bit passes on both sides.
    #[test]
    fn cast_water_refusal_reads_both_sides_of_the_surface() {
        use crate::creature_anim::move_flags as mf;
        // Aquatic Form 1066: AuraInterruptFlags 0x100 — above-water cancels it, so the cast
        // needs water. This is ledger B176: on land it must refuse, and it must not send.
        let aquatic = SpellDisplay {
            aura_interrupt_flags: 0x100,
            ..Default::default()
        };
        assert_eq!(
            cast_water_refusal(0, Some(&aquatic)),
            Some(ERR_ONLY_UNDERWATER)
        );
        assert_eq!(cast_water_refusal(mf::SWIMMING, Some(&aquatic)), None);
        // Travel Form 783 / every mount: 0x80 — entering water cancels it, so it refuses the
        // other way round.
        let travel = SpellDisplay {
            aura_interrupt_flags: 0x80,
            ..Default::default()
        };
        assert_eq!(
            cast_water_refusal(mf::SWIMMING, Some(&travel)),
            Some(ERR_ONLY_ABOVEWATER)
        );
        assert_eq!(cast_water_refusal(0, Some(&travel)), None);
        // Food 433's shape (0x40080 = STANDING_CANCELS | UNDER_WATER_CANCELS) — the eating half
        // of ledger B155 rides the same 0x80 arm.
        let food = SpellDisplay {
            aura_interrupt_flags: 0x4_0080,
            ..Default::default()
        };
        assert_eq!(
            cast_water_refusal(mf::SWIMMING, Some(&food)),
            Some(ERR_ONLY_ABOVEWATER)
        );
        assert_eq!(cast_water_refusal(0, Some(&food)), None);
        // ARM B (decision 1063): a CHANNELED spell's requirement bits live in its channel
        // column. Fishing ranks 2–4's shape — AuraInterruptFlags zero, ChannelInterruptFlags
        // 0x3cac (which carries 0x80), AttributesEx 0x21004004 (IS_CHANNELED).
        let fishing_r2 = SpellDisplay {
            aura_interrupt_flags: 0,
            channel_interrupt_flags: 0x3cac,
            attributes_ex: 0x2100_4004,
            ..Default::default()
        };
        assert_eq!(
            cast_water_refusal(mf::SWIMMING, Some(&fishing_r2)),
            Some(ERR_ONLY_ABOVEWATER),
            "arm A is empty here — only the channel column refuses it"
        );
        assert_eq!(cast_water_refusal(0, Some(&fishing_r2)), None);
        // …and the `AttributesEx & 0x44` gate on arm B is load-bearing: Summon Baby Shark
        // 25849's shape carries the channel bit but does not channel, so it is NOT gated.
        let not_channeled = SpellDisplay {
            channel_interrupt_flags: 0x100,
            attributes_ex: 0,
            ..Default::default()
        };
        assert_eq!(cast_water_refusal(0, Some(&not_channeled)), None);
        assert_eq!(cast_water_refusal(mf::SWIMMING, Some(&not_channeled)), None);
        // Cat Form 768 / Bear Form 5487 carry neither bit: usable on both sides, always.
        let cat = SpellDisplay::default();
        assert_eq!(cast_water_refusal(0, Some(&cat)), None);
        assert_eq!(cast_water_refusal(mf::SWIMMING, Some(&cat)), None);
        // No record, nothing to read — the press passes, like every other data-driven rung.
        assert_eq!(cast_water_refusal(0, None), None);
        assert_eq!(cast_water_refusal(mf::SWIMMING, None), None);
    }

    /// The water gate against the **real 5875 Spell.dbc** — the census the whole gate rests on.
    /// The leg↔bit assignment is not a naming choice we could get backwards: exactly five rows
    /// in the shipped data carry the water-REQUIRED bit, and Aquatic Form is one of them, while
    /// the water-FORBIDDEN bit is a broad set led by the mounts, Travel Form and food/drink.
    /// Skips without client data.
    #[test]
    fn the_water_bits_split_the_5875_data_the_way_the_gate_assumes() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let catalog = benilla_formats::load_spell_catalog(&mut chain).expect("Spell.dbc");

        // Every spell the client would refuse OUT of water, by id.
        let mut needs_water: Vec<u32> = catalog
            .iter()
            .filter(|(_, d)| d.aura_interrupt_flags & INTERRUPT_ABOVE_WATER != 0)
            .map(|(id, _)| id)
            .collect();
        needs_water.sort_unstable();
        assert_eq!(
            needs_water,
            vec![1066, 16455, 16456, 24346, 24347],
            "the water-required set: Aquatic Form, the Lava/Slime swim auras, Master Angler"
        );

        // The other face: broad, and led by exactly the things you cannot do mid-swim.
        let forbids_water = |id: u32| {
            catalog
                .get(id)
                .is_some_and(|d| d.aura_interrupt_flags & INTERRUPT_UNDER_WATER != 0)
        };
        assert!(forbids_water(783), "Travel Form");
        assert!(forbids_water(458), "Brown Horse");
        assert!(forbids_water(433), "Food");
        assert!(forbids_water(430), "Drink");
        assert!(forbids_water(818), "Basic Campfire");

        // ARM B's reason to exist, on the real rows (decision 1063). Fishing rank 1 carries the
        // bit in the AURA column; its own upgrades carry NOTHING there and reach the gate only
        // through the CHANNEL column. Arm A alone would refuse rank 1 mid-swim and let 2–4 fish.
        let swim = crate::creature_anim::move_flags::SWIMMING;
        assert!(forbids_water(7620), "Fishing rank 1 — arm A");
        for rank in [7731, 7732, 18248] {
            let d = catalog
                .get(rank)
                .unwrap_or_else(|| panic!("Fishing {rank} missing"));
            assert_eq!(d.aura_interrupt_flags, 0, "Fishing {rank}: arm A is empty");
            assert!(d.attributes_ex & ATTR_EX_CHANNELED != 0, "and it channels");
            assert_eq!(
                cast_water_refusal(swim, Some(d)),
                Some(ERR_ONLY_ABOVEWATER),
                "Fishing {rank} still refuses mid-swim, through the channel column"
            );
            assert_eq!(cast_water_refusal(0, Some(d)), None, "and fishes on shore");
        }

        // The forms that carry neither bit — usable on both sides, and the control that says the
        // gate is reading a real per-spell column and not a class-wide accident.
        for (id, name) in [(768, "Cat Form"), (5487, "Bear Form"), (2645, "Ghost Wolf")] {
            let d = catalog.get(id).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(d.aura_interrupt_flags & 0x180, 0, "{name} is side-agnostic");
            assert_eq!(cast_water_refusal(0, Some(d)), None, "{name} on land");
            assert_eq!(cast_water_refusal(swim, Some(d)), None, "{name} swimming");
        }

        // And end to end, on the real rows: B176's exact press.
        let aquatic = catalog.get(1066).expect("Aquatic Form");
        assert_eq!(
            cast_water_refusal(0, Some(aquatic)),
            Some(ERR_ONLY_UNDERWATER),
            "on land, Aquatic Form refuses — the B176 report"
        );
        assert_eq!(
            cast_water_refusal(crate::creature_anim::move_flags::SWIMMING, Some(aquatic)),
            None,
            "swimming, it goes through"
        );
    }

    /// The moving refusal (`0x609de3`) — every leg of the byte-verified condition: the
    /// `InterruptFlags & 0x1` entry, the `0x200f` wire mask (turn/FALLINGFAR outside it,
    /// JUMPING inside), the auto-repeat exemption, and the "would movement matter" arms (cast
    /// time / aura / channel `0x18` bits).
    #[test]
    fn cast_moving_refusal_follows_the_validator_condition() {
        use crate::creature_anim::move_flags as mf;
        // Fireball's shape: ordinary timed cast (interrupt 0xf, nonzero cast time).
        let timed = SpellDisplay {
            interrupt_flags: 0xf,
            ..Default::default()
        };
        // Moving forward refuses; standing still doesn't.
        assert!(cast_moving_refusal(mf::FORWARD, 1500, Some(&timed)));
        assert!(!cast_moving_refusal(0, 1500, Some(&timed)));
        // Strafe and JUMPING are in the mask; turn and FALLING_FAR are not (`0x200f`).
        assert!(cast_moving_refusal(mf::STRAFE_LEFT, 1500, Some(&timed)));
        assert!(cast_moving_refusal(mf::FALLING, 1500, Some(&timed)));
        assert!(!cast_moving_refusal(mf::TURN_LEFT, 1500, Some(&timed)));
        assert!(!cast_moving_refusal(mf::FALLING_FAR, 1500, Some(&timed)));
        // An instant WITHOUT the movement interrupt bit passes (Fire Blast's shape)…
        let instant = SpellDisplay {
            interrupt_flags: 0xe,
            ..Default::default()
        };
        assert!(!cast_moving_refusal(mf::FORWARD, 0, Some(&instant)));
        // …and even WITH it, a zero cast time passes unless an 0x18 arm bites.
        assert!(!cast_moving_refusal(mf::FORWARD, 0, Some(&timed)));
        // Arcane Missiles' shape: zero cast time, but the channel column's moving bits refuse
        // at initiation (the OR-arm; 0x7c0c & 0x18 != 0).
        let channel = SpellDisplay {
            interrupt_flags: 0xf,
            channel_interrupt_flags: 0x7c0c,
            ..Default::default()
        };
        assert!(cast_moving_refusal(mf::FORWARD, 0, Some(&channel)));
        // The aura-column arm (food/drink sit-still bits).
        let sit_still = SpellDisplay {
            interrupt_flags: 0x1,
            aura_interrupt_flags: 0x18,
            ..Default::default()
        };
        assert!(cast_moving_refusal(mf::FORWARD, 0, Some(&sit_still)));
        // Auto-repeat (AttributesEx2 & 0x20) is unconditionally exempt — Auto Shot fires on
        // the run whatever its columns say.
        let auto_shot = SpellDisplay {
            interrupt_flags: 0x1,
            attributes_ex2: 0x20,
            aura_interrupt_flags: 0x18,
            ..Default::default()
        };
        assert!(!cast_moving_refusal(mf::FORWARD, 0, Some(&auto_shot)));
        // No record: nothing to read, the press passes (the server stays the net).
        assert!(!cast_moving_refusal(mf::FORWARD, 1500, None));
    }
}

#[cfg(test)]
mod cc_refusal_tests {
    use super::*;

    const STUNNED: u32 = 0x0004_0000;
    const SILENCED: u32 = 0x0000_2000;
    const PACIFIED: u32 = 0x0002_0000;
    const CONFUSED: u32 = 0x0040_0000;
    const FLEEING: u32 = 0x0080_0000;

    fn spell(prevention_type: u32) -> SpellDisplay {
        SpellDisplay {
            prevention_type,
            ..Default::default()
        }
    }

    /// A scan that finds no aura at all — `exempt: false, mechanic: 0`, so every arm falls to its
    /// OWN reason. This is the ordinary case: the exemption only ever fires for a caster wearing
    /// an immunity.
    fn no_auras() -> impl FnMut(&[u32]) -> benilla_formats::CcExemption {
        |_: &[u32]| benilla_formats::CcExemption::default()
    }

    /// The reason alone, dropping the mechanic — most of these tests are about which arm fires.
    fn reason_of(v: Option<(u8, Option<u32>)>) -> Option<u8> {
        v.map(|(r, _)| r)
    }

    /// A live caster, nobody else at the reins, no auras.
    fn cc(flags: u32, d: &SpellDisplay) -> Option<u8> {
        cast_cc_refusal(flags, Some(100), false, Some(d), &mut no_auras()).map(|(r, _)| r)
    }

    /// **The arms are not symmetric** (decision 1904, widened by 1925): stun, fear, charm and
    /// confuse refuse EVERY spell; silence and pacify only the rows whose `PreventionType` names
    /// them. Reading `UNIT_FLAG_SILENCED` as "no casting at all" — which our own preflight banner
    /// used to say — is the mistake this pins.
    #[test]
    fn crowd_control_refuses_by_prevention_type_except_the_unconditional_arms() {
        // Fireball-shaped (silence-preventable), Heroic-Strike-shaped (pacify-preventable), and
        // the auto-attack's neither — the three real values, pinned in `spell_catalog`.
        let cast = spell(1);
        let melee = spell(2);
        let neither = spell(0);

        // The four arms with NO per-spell gate: every row refuses.
        for (flags, reason) in [(STUNNED, 0x64), (FLEEING, 0x1e), (CONFUSED, 0x16)] {
            for d in [&cast, &melee, &neither] {
                assert_eq!(cc(flags, d), Some(reason), "flags {flags:#x}");
            }
        }
        // …and charm, which is not a flag at all but "somebody else holds the reins".
        assert_eq!(
            reason_of(cast_cc_refusal(
                0,
                Some(100),
                true,
                Some(&neither),
                &mut no_auras()
            )),
            Some(0x14)
        );

        // SILENCED takes the casts and leaves the rest alone.
        assert_eq!(cc(SILENCED, &cast), Some(0x60));
        assert_eq!(cc(SILENCED, &melee), None);
        assert_eq!(cc(SILENCED, &neither), None);

        // PACIFIED is the mirror.
        assert_eq!(cc(PACIFIED, &melee), Some(0x5a));
        assert_eq!(cc(PACIFIED, &cast), None);

        // Nothing up, nothing refused; and no record claims no prevention.
        assert_eq!(cc(0, &cast), None);
        assert_eq!(
            reason_of(cast_cc_refusal(
                SILENCED,
                Some(100),
                false,
                None,
                &mut no_auras()
            )),
            None
        );
        assert_eq!(
            reason_of(cast_cc_refusal(
                STUNNED,
                Some(100),
                false,
                None,
                &mut no_auras()
            )),
            Some(0x64),
            "the stun needs no record"
        );
    }

    /// **The order is the reference's, first match wins** — charm outranks the stun, the stun
    /// outranks everything below it. When several hold at once the player sees exactly one line,
    /// and which one is not arbitrary.
    #[test]
    fn the_arms_are_tried_in_the_references_order() {
        let cast = spell(1);
        // Charm is arm 1: it beats a simultaneous stun.
        assert_eq!(
            reason_of(cast_cc_refusal(
                STUNNED | SILENCED,
                Some(100),
                true,
                Some(&cast),
                &mut no_auras()
            )),
            Some(0x14)
        );
        // Stun is arm 2: it beats silence, pacify, fear and confuse.
        assert_eq!(
            cc(STUNNED | SILENCED | PACIFIED | FLEEING | CONFUSED, &cast),
            Some(0x64)
        );
        // Silence (3) beats fear (5) and confuse (6).
        assert_eq!(cc(SILENCED | FLEEING | CONFUSED, &cast), Some(0x60));
        // With the spell out of silence's reach, fear takes it before confuse.
        assert_eq!(cc(SILENCED | FLEEING | CONFUSED, &spell(0)), Some(0x1e));
    }

    /// **What the exemption does to an arm** (decision 1946) — the three outcomes, at the arm.
    #[test]
    fn the_exemption_skips_an_arm_or_renames_its_refusal() {
        let cast = spell(1);
        let scan = |exempt: bool, mechanic: u32| {
            move |_: &[u32]| benilla_formats::CcExemption { exempt, mechanic }
        };

        // No aura of the arm's type: the arm's OWN reason, as everywhere else.
        assert_eq!(
            reason_of(cast_cc_refusal(
                STUNNED,
                Some(100),
                false,
                Some(&cast),
                &mut scan(false, 0)
            )),
            Some(0x64)
        );
        // A blocking aura the cast does NOT counter: the refusal survives but is renamed to
        // `0x8d` "Can't do that while %s", the mechanic naming the aura.
        assert_eq!(
            reason_of(cast_cc_refusal(
                STUNNED,
                Some(100),
                false,
                Some(&cast),
                &mut scan(false, 12)
            )),
            Some(0x8d)
        );
        // The cast grants immunity: the arm is SKIPPED and the cast goes out. This is the whole
        // point — Ice Block cast while stunned.
        assert_eq!(
            reason_of(cast_cc_refusal(
                STUNNED,
                Some(100),
                false,
                Some(&cast),
                &mut scan(true, 0)
            )),
            None
        );
        // …and skipping one arm does not skip the ladder: a silenced-and-stunned caster whose
        // spell counters only the stun still refuses on silence below it.
        assert_eq!(
            reason_of(cast_cc_refusal(
                STUNNED | SILENCED,
                Some(100),
                false,
                Some(&cast),
                &mut |types: &[u32]| {
                    // Exempt from the stun arm `{12}` only; the silence arm `{27, 12, 60}` is not.
                    benilla_formats::CcExemption {
                        exempt: types == [12],
                        mechanic: 0,
                    }
                }
            )),
            Some(0x60)
        );
    }

    /// **The mechanic rides out with the refusal** (decision 1948) — the half that turns
    /// "Can't do that while %s" into a sentence. It is the ONE client-local refusal that carries
    /// an argument word, and it is `None` for every arm that fell to its own reason.
    #[test]
    fn the_renamed_refusal_carries_the_mechanic_and_the_others_carry_nothing() {
        let cast = spell(1);
        let scan = |exempt: bool, mechanic: u32| {
            move |_: &[u32]| benilla_formats::CcExemption { exempt, mechanic }
        };

        // Rejected by a blocking aura: reason `0x8d` AND the mechanic that names it.
        assert_eq!(
            cast_cc_refusal(STUNNED, Some(100), false, Some(&cast), &mut scan(false, 12)),
            Some((0x8d, Some(12)))
        );
        // No aura: the arm's own reason, and NO argument — the message has no `%s` to fill.
        assert_eq!(
            cast_cc_refusal(STUNNED, Some(100), false, Some(&cast), &mut scan(false, 0)),
            Some((0x64, None))
        );
        // Exempt: nothing at all.
        assert_eq!(
            cast_cc_refusal(STUNNED, Some(100), false, Some(&cast), &mut scan(true, 0)),
            None
        );
    }

    /// **A dead caster skips all six** (`0x60980d`'s `jle` on `UNIT_FIELD_HEALTH`) — a corpse is
    /// refused by an earlier rung, and reporting a stun over it would be the wrong line.
    #[test]
    fn a_dead_caster_takes_no_crowd_control_refusal() {
        let cast = spell(1);
        assert_eq!(
            reason_of(cast_cc_refusal(
                STUNNED,
                Some(0),
                false,
                Some(&cast),
                &mut no_auras()
            )),
            None
        );
        assert_eq!(
            reason_of(cast_cc_refusal(
                0,
                Some(0),
                true,
                Some(&cast),
                &mut no_auras()
            )),
            None
        );
        // Alive again, and the arm is back.
        assert_eq!(
            reason_of(cast_cc_refusal(
                STUNNED,
                Some(1),
                false,
                Some(&cast),
                &mut no_auras()
            )),
            Some(0x64)
        );
        // No health field at all is not death — the descriptor simply has not landed.
        assert_eq!(
            reason_of(cast_cc_refusal(
                STUNNED,
                None,
                false,
                Some(&cast),
                &mut no_auras()
            )),
            Some(0x64)
        );
    }
}
