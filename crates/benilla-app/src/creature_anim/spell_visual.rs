//! The spell-visual **data plane + cast-edge router** (decision 0099 phase 2; schemas and stage
//! wiring pinned decision 0107): `SpellVisual.dbc` × `SpellVisualKit.dbc` loaded once at startup
//! exactly like [`super::AnimData`], and [`route_cast_visuals`] — the one place spell ids resolve
//! to animations/sounds. The wire side ([`super::CastEvent`], written by the net bridge) and the
//! render side ([`super::CastHold`] + [`super::EmoteAnim`] one-shots, consumed by the driver)
//! never touch the DBC chain themselves.

#[cfg(test)]
mod tests;

use std::cell::Cell;

use bevy::ecs::entity::{EntityHashMap, EntityHashSet};
use bevy::prelude::*;

use benilla_formats::{SpellVisualCatalog, VisualKit, VisualStages, MISSILE_ATTACH_TABLE};
use benilla_protocol::EntityKind;

use crate::aura_visual::AuraProc;
use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{NetCommands, NetEntity, ObjectStore};
use benilla_assets::{LockRecover, WorldAssets};

use super::{CastEvent, CastEventKind, CastHold, EmoteAnim, SpellGoTargets, WoundAnim};

/// The client's literal missile-model fallback when the visual chain resolves to nothing
/// (`0x860c9c` — a checkerboard cube, shipped in the real MPQs; faithful, not a joke).
const ERROR_CUBE: &str = "Spells\\ErrorCube.mdx";

/// The reserved [`SpellKitFx`] reap key for the lootable-corpse effect — NOT a spell id (no 5875
/// spell id is anywhere near `u32::MAX`; the client keys these nodes by attach tag + node flag
/// instead of a spell, wow-re `loot-corpse-effect.md`). One key suffices: a unit carries at most
/// one loot effect (the client's flag8 dedup), which [`arm_loot_fx`]'s edge cache guarantees.
const LOOT_FX_KEY: u32 = u32::MAX;

/// The M2 attachment the three engine-spawned effects we ship hang from — the loot sparkle, the
/// level-up ding and the mount poof. It is also the `spell_fx` attach cascade's own last
/// fallback, so a model lacking the point lands on the unit base — exactly the reference's
/// root fallback (`0x61fb4f`–`0x61fb5f`: `HasAttachment(tag)` on `[unit+0xd8]`, else `0x13`).
///
/// **It is a table value, not an engine-wide constant** (wow-re `mount-composition.md` Q4b.1, §5
/// 2026-08-03 — the 14-entry `DAT_0080c968`, dumped and joined to the `0x8617b8` name table). It
/// happens to be `0x13` for indices **4** Loot Art, **5** Unit Level Up and **6** Mount Poof —
/// our three — and for the PetLoyalty/Meeting-Stone/Reputation rows; it is `0x11` for the two
/// breath effects and Inebriated Bubbles, and the sentinel `0x25` (spawn refused outright) for
/// the two footstep sprays. A fourth consumer must read the table row, not reuse this.
const HARDCODED_FX_ATTACH: u16 = 0x13;

/// The engine-spawned level-up effect's baked lookup name (the client's string `0x8618e0`,
/// matched by the boot name-resolve `0x61f5b0`; wow-re `levelup-ding.md` — decision 0305).
const LEVEL_UP_EFFECT: &str = "HARDCODED Unit Level Up";

/// The mount transition's cloud — hardcoded index **6**, shipped as `SpellVisualEffectName` row
/// 1185 → `Spells\DruidMorph_Impact_Base.mdx`, i.e. literally the druid-morph puff (wow-re
/// `mount-composition.md` Q4b, decision 0927).
const MOUNT_POOF_EFFECT: &str = "HARDCODED Mount Poof";

/// `SpellVisual.dbc` × `SpellVisualKit.dbc` — the stage-kit chain + each kit's anim/sound
/// (`crate::creature_anim::spell_visual` module docs). Optional like every other DBC-backed
/// resource: absent (no client data) degrades to no spell-visual playback, same shape as
/// [`super::AnimData`].
#[derive(Resource)]
pub(crate) struct SpellVisuals(pub(crate) SpellVisualCatalog);

/// A spell-visual kit's sound edge (kit field 13 → `SoundEntries.dbc`, decision 0107 verdict
/// C5-corrected: the kit's own sound column, rung at kit start). Written here, consumed by
/// `crate::sound`. One message type so the consumer sees the edges **in emission order** — a
/// GO's stop-then-release never races.
#[derive(Message, Clone, Copy, Debug)]
pub(crate) enum SpellKitSound {
    /// Ring this kit at the unit. A LOOPING kit (`SoundEntries` flag 0x200) becomes the unit's
    /// tracked **hold loop** — the client's looping-test split (`0x458830` → tracked `0x61fec0`
    /// vs one-shot `0x458870`) — sustained until [`Self::StopHold`].
    Play { entity: Entity, kit_sound: u32 },
    /// The cast/channel hold ended (GO / fail / channel-clear / a replacing cast) — reap the
    /// unit's tracked hold loop, if any (the client kills the effect's sound at `0x614150`).
    StopHold { entity: Entity },
    /// Ring this kit at a bare **world point**, with no owning unit — the kit-sound leg's
    /// `extra`-override arm (wow-re `kit-sound-leg.md`: "position = `extra` if non-NULL else the
    /// unit's own position", `60f49e`–`60f4bc`). Its one caller is the missile's ground arrival,
    /// where `0x61d870` passes the landing point: the bomb's boom belongs where it landed, not at
    /// the thrower. Always a one-shot — a LOOPING sound here would need a tracked node with an
    /// owner, and none of the six shipped ground-arrival kits carries one (census below).
    PlayAt { pos: Vec3, kit_sound: u32 },
    /// Stop exactly `kit_sound`'s channels on this unit — the aura-drop reap of a LOOPING
    /// **state**-kit sound (wow-re `kit-sound-leg.md`: a looping kit sound rides a tracked,
    /// spell-id-tagged CEffect on the unit's list, and `0x614150` stops it with a 0.15 s fade
    /// when the aura leaves). Kit-scoped so an unrelated hold loop on the same unit survives;
    /// a one-shot kit is never tracked, so for it this is a no-op.
    StopKit { entity: Entity, kit_sound: u32 },
}

/// A persistent effect instance's **lifetime class** — which owner's reap can kill it.
/// Byte-verified (wow-re `state-kit-aura-lifecycle.md`, §5 2026-07-14): the client's reap walk
/// `0x614150(spellId, force)` discriminates by node flags — every SPELL_GO / SPELL_FAILED reap
/// passes `force=0`, which **spares** stage-2 nodes (flag `0x1000`); only the aura-remove path
/// (`0x612320 → 0x5ff290`) passes `force=1` and takes them. So a cast's GO releasing its precast
/// never sweeps the same spell's aura-state models (re-eat while the food buff holds: the GO
/// must not take the bread with it). This enum is that discriminator, named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FxClass {
    /// Cast-lifecycle instances: precast/channel holds (and every self-terminating flash, whose
    /// class never matters — they die on their own clock). Reaped by the cast router's edges.
    Hold,
    /// Aura-state instances (stage 2 under a live aura) — owned and reaped only by
    /// [`arm_aura_state_fx`]'s slot watcher.
    AuraState,
}

/// Which of the reference's five kit **stages** an instance's model runs its *animation lifecycle*
/// as — the discriminator behind `PlaySpellVisualKit`'s per-stage completion-callback table
/// (`0x60edf0`'s jump table `0x60f4f8`; wow-re `ceffect-anim-lifecycle.md` §2, §5 trio 2026-08-02).
///
/// Distinct from [`FxClass`], which answers a different question — *whose reap can kill this* (the
/// `0x614150` force census). The two are not derivable from each other: a channel kit and an
/// aura-state kit are both stage 2 but reaped by different owners, while a precast and a channel
/// are both [`FxClass::Hold`] and run *different* animation lifecycles. So the stage rides its own
/// field, set at each writer from the caller table (`spell-visual-apply.md` §1.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FxStage {
    /// Stages 0/1 (kit push, cast release, impact) — callback `0x5fbf50`: destroy at the first
    /// completion. Already modelled by the instance's own span clock, so it arms no watcher.
    OneShot,
    /// Stage 2 (aura state, channel) — callback `0x5ff170`: when the birth sequence completes, arm
    /// **`Hold` (158)** if the model authors one and re-arm it for the effect's life; the reap arms
    /// **`Decay` (159)** and the instance lives out that span. A model with no `Hold` is left
    /// parked on its birth — `0x5ff170` does nothing at all in that case.
    State,
    /// Stages 3/4 (precast) — callback `0x60ed00`: **re-arm the id that just completed, forever**,
    /// with no `Hold` lookup, no deadline and no destroy. So a precast model whose birth sequence
    /// clamps still repeats it, where a loop-flag-only arm would freeze.
    Relive,
}

/// A spell-visual kit's attach-point **effect model** edge (kit fields 3–11 →
/// `SpellVisualEffectName` `.mdx` — decision 0099 phase 3; slot loop byte-pinned in wow-re
/// `spell-visual-apply.md` §1.3). Written here, consumed by `crate::entities::spell_fx` (the
/// bone-rider home). One message type so the consumer sees edges **in emission order** — a GO's
/// reap-then-begin never races.
#[derive(Message, Clone)]
pub(crate) enum SpellKitFx {
    /// Attach this kit's effect models to the unit — one instance per populated slot,
    /// `(M2 attachment id, effect model path)`. `persistent` = the precast/channel/aura-state
    /// lifetime (lives until a matching-[`FxClass`] [`Self::Reap`] — the client's stage-4/2
    /// no-self-termination); else the effect self-terminates after one pass of its model's
    /// first sequence (the stage-0/1 completion callback `0x5fbf50`). A persistent Begin
    /// replaces the unit's live persistent instances of the same `(spell_id, class)`.
    Begin {
        entity: Entity,
        spell_id: u32,
        persistent: bool,
        class: FxClass,
        /// The kit stage this play is, which decides the instances' **animation lifecycle**
        /// ([`FxStage`]) — a separate axis from `class`/`persistent`, both of which are about
        /// lifetime ownership rather than what the model plays.
        stage: FxStage,
        effects: Vec<(u16, String)>,
    },
    /// The owner's lifetime ended (GO / fail / channel-clear / a replacing cast for
    /// [`FxClass::Hold`]; the aura leaving the slots for [`FxClass::AuraState`]) — despawn the
    /// unit's **persistent** effects matching `(spell_id, class)` (the client's spell-id-keyed
    /// reap `0x614150`; self-terminating instances run out on their own clock).
    Reap {
        entity: Entity,
        spell_id: u32,
        class: FxClass,
    },
}

/// A kit whose `CharProc` slots name a **beam** just played on `entity` — the client's CharProc
/// dispatcher reaching its chain case (`0x60da79`, decision 0955). Written here for every kit play
/// that carries one (the dispatcher runs at `PlaySpellVisualKit`'s tail, `0x60f35c`, and again from
/// the channel poll, `0x612b18`); consumed by `crate::entities::chain_beam`, which owns the target
/// selection against the caster's hop array, the beam's lifetime, and its geometry.
#[derive(Message, Clone, Copy)]
pub(crate) struct ChainProcPlay {
    /// The unit the kit played on — the beam's caster end, and the owner of the hop array.
    pub(crate) entity: Entity,
    /// The kit's spell (`0` for a bare kit push, which the client's own `spellId != 0` guard at
    /// `0x6ecbd0` refuses to build a beam for).
    pub(crate) spell_id: u32,
    pub(crate) proc: benilla_formats::ChainProc,
}

/// Launch a projectile from `caster` at each target (decision 0099 phase 4) — one missile per
/// target, the client's per-target spawn on the Speed>0 GO branch (`0x6e8a50 → 0x60a3d0`).
/// Written here (the DBC columns resolve in this module, like every kit edge); consumed by
/// `crate::entities::missile`, which owns launch/flight/arrival and writes the arrival back as
/// [`super::CastEventKind::Impact`].
#[derive(Message, Clone)]
pub(crate) struct MissileSpawn {
    pub(crate) caster: Entity,
    pub(crate) spell_id: u32,
    /// The projectile's model path (`SpellVisual` field 7 → `SpellVisualEffectName`, already
    /// `.mdx`-form; the client's literal `Spells\ErrorCube.mdx` when a nonzero id is
    /// unresolvable). `None` = the visual chain names no missile (field 7 < 1, or no
    /// `SpellVisual` row at all — every basic shot spell): the spawner falls to the wire ammo
    /// model (the client's `0x479f40` branch), phase 5.
    pub(crate) path: Option<String>,
    /// The GO's ammo block — the flying arrow/bullet/thrown model when [`Self::path`] is `None`.
    pub(crate) ammo_display_id: Option<u32>,
    /// The M2 attach tag the missile homes to on a live target (`SpellVisual` field 9's ordinal
    /// through [`benilla_formats::MISSILE_ATTACH_TABLE`], the client's `0x860a18`) — `None` for
    /// an out-of-table ordinal (the target's base position, the client's `-1` sentinel path).
    pub(crate) dest_tag: Option<u16>,
    /// `Spell.dbc` Speed, world units/sec — travel time = launch distance / speed, then
    /// arrive-on-time (the client's `0x61ceb0`).
    pub(crate) speed: f32,
    /// `(target, miss)` — `None` landed (arrival plays the impact hand-off); `Some(code)` the
    /// wire's `SpellMissInfo`: the missile still flies, and arrival plays the victim's defense
    /// clip for DODGE(3)/BLOCK(5) instead (the client's `Missile_C::Update` dispatch).
    pub(crate) targets: Vec<(Entity, Option<u8>)>,
    /// The **location fallback**: when [`Self::targets`] is empty and the GO carried a ground
    /// point, exactly ONE projectile flies at it instead (the client's `0x6e8a50` empty-hit-array
    /// arm — `0x6e8aa2 test al,0x60; setne cl` latches "SOURCE or DEST location present",
    /// `0x6e8bd4` then calls the spawner once, `0x60a5b6`–`0x60a5c9` copies `targets+0x3c` into
    /// the aim point and `0x60a5ec` forces the owning unit slot to −1; wow-re
    /// `spell-go-dest-effect.md` §3). This is what a pure ground cast — Flare, a bomb thrown at
    /// empty dirt — actually shows travelling. Its arrival is [`super::CastEventKind::GroundImpact`],
    /// never the unit hand-off. Named divergence: the reference latches on `flags & 0x60` and then
    /// reads the DEST vector regardless, so a SOURCE-only cast aims at whatever the zero-initialised
    /// DEST slot holds; we carry only a real DEST (`flags & 0x40`) and launch nothing without one.
    pub(crate) ground_aim: Option<Vec3>,
    /// The caster's ranged-weapon fallback `SpellVisual` id, resolved at GO time — rides the
    /// flight and returns in [`super::CastEventKind::Impact`] so a basic shot's impact kit
    /// resolves through it at arrival (the caster may be gone by then; decision 0370).
    pub(crate) weapon_visual: Option<u32>,
    /// `SpellVisual` field 10 → the `SoundEntries.dbc` id the projectile LOOPS while it travels
    /// (the thrown weapon's `WeaponLoop`, the fireball's `FireMissileLoop`) — the client's
    /// per-missile loop handle (`CMissile+0x44`). `None` = a silent flight.
    pub(crate) missile_sound: Option<u32>,
    /// Whether the launch **waits for the cast animation's release keyframe** — the client queues
    /// every projectile on the caster (`unit+0xac`) and its GO handler flushes immediately only
    /// when the cast kit plays no body animation (`0x6e7a70`: `castKit == 0 || kit anim < 1` →
    /// `0x60c9b0`); otherwise the launch is the animation's `$CSL`/`$CSR`/`$CST`/`$BWR` event
    /// (the dispatcher `0x5ffbd0` → `0x60c940`), from the fired marker's live position — the
    /// fireball leaves the raised hand mid-throw, not the hold pose at GO.
    pub(crate) awaits_release: bool,
}

/// Load `SpellVisual.dbc` + `SpellVisualKit.dbc` off the patch chain at startup (mirrors
/// [`super::sheath::load_anim_data`]'s pattern exactly).
pub(super) fn load_spell_visuals(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_spell_visual_catalog(&mut chain)
    };
    match loaded {
        Ok(cat) => {
            info!(
                "anim: {} SpellVisual / {} SpellVisualKit rows",
                cat.len(),
                cat.kit_len()
            );
            commands.insert_resource(SpellVisuals(cat));
        }
        Err(e) => warn!("anim: SpellVisual/SpellVisualKit failed to load: {e:#}"),
    }
}

/// The lookup bundle behind the client's `0x60d450` ranged fallback: caster →
/// equipped-ranged-weapon display → `ItemDisplayInfo` col 10 substitute `SpellVisual` id
/// (wow-re `throw-ranged-attack-anim.md`, decision 0370).
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct WeaponVisualSrc<'w, 's> {
    displays: Option<Res<'w, ItemDisplays>>,
    items: Option<ResMut<'w, Items>>,
    net: Option<Res<'w, NetCommands>>,
    units: Query<'w, 's, (Option<&'static NetEntity>, &'static ObjectStore)>,
}

impl WeaponVisualSrc<'_, '_> {
    /// The caster's equipped ranged weapon's SUBSTITUTE `SpellVisual` id (`vtable+0xa0(slot 2)`
    /// → `ItemDisplayInfo` col 10). A creature carries the display id on the wire
    /// (`UNIT_VIRTUAL_ITEM_SLOT_DISPLAY[2]`); a player resolves entry → template through the
    /// ask-once item layer (cached by the time anything shoots — the equipped weapon already
    /// rendered through it). `None` = no ranged weapon, or its display names no visual (every
    /// non-ranged item).
    fn caster(&mut self, caster: Entity) -> Option<u32> {
        let (net_entity, store) = self.units.get(caster).ok()?;
        let s = &store.0;
        let display_id = match net_entity?.kind {
            EntityKind::Unit => s.unit_virtual_item_display(2),
            // 17 = vmangos EQUIPMENT_SLOT_RANGED (the equipment layer's PLAYER_HELD_SLOTS[2]).
            EntityKind::Player => s
                .player_visible_item_entry(17)
                .filter(|e| *e != 0)
                .and_then(|entry| self.items.as_deref_mut()?.held(entry, self.net.as_deref()?))
                .map(|t| t.display_info_id),
            _ => None,
        }
        .filter(|d| *d != 0)?;
        let visual = self
            .displays
            .as_deref()?
            .catalog
            .get(display_id)?
            .spell_visual;
        (visual != 0).then_some(visual)
    }
}

/// A spell's effective `SpellVisual` row (spell → `Spell.dbc` column 115 → `SpellVisual.dbc`).
/// `None` = a silent cast (no visual chain at all).
///
/// The one place the client's **ranged weapon-visual merge** applies (`0x60d450`; the law is
/// [`VisualStages::merged_over_weapon`], byte-read there): a RANGED-attribute spell
/// (`Attributes & 0x2`) takes the caster's equipped ranged weapon's substitute visual for every
/// slot its **own** row leaves at zero — precast/cast/impact kits, the missile block, the strike
/// sound; never state/channel. A basic shot (`SpellVisual1 = 0`) is only the degenerate case of
/// that fill — the whole zeroed row comes across (Throw → ReadyThrown/AttackThrown, Auto Shot →
/// LoadBow/AttackBow, wand Shoot → the gun kits), which is what decision 0370 read as the *whole*
/// mechanism; every hunter shot spell has its own impact + missile row and empty body-anim slots,
/// so it needs the per-field arm (decision 0986, bug B153). A non-ranged spell never pays the
/// lookup (`FnOnce`).
fn resolve_stages(
    spells: &crate::ui_action::Spells,
    visuals: &SpellVisualCatalog,
    spell_id: u32,
    weapon_visual: impl FnOnce() -> Option<u32>,
) -> Option<VisualStages> {
    let def = spells.catalog.get(spell_id)?;
    let own = visuals.stages(def.visual).copied();
    if !def.ranged_slot() {
        return own; // `ebx` stayed 0 at `60d468` — the fill block is a no-op
    }
    // `60d46e`–`60d4aa`: the caster's ranged display → `ItemDisplayInfo` col 10 → its row.
    let Some(weapon) = weapon_visual().and_then(|v| visuals.stages(v)) else {
        return own;
    };
    // No own row = the client's `rep stosd` zeroed `outKit` before the same fill (`60d4bc`).
    Some(own.unwrap_or_default().merged_over_weapon(weapon))
}

/// Resolve one lifecycle stage of `spell_id`'s visual chain to its kit (spell → `Spell.dbc`
/// column 115 → `SpellVisual` stage column → `SpellVisualKit` row), via the given stage selector.
/// `None` anywhere along the chain = that stage is silent for this spell (the common case — most
/// spells populate only some stages). `weapon_visual` is [`resolve_stages`]'s ranged fallback.
fn resolve_kit(
    spells: &crate::ui_action::Spells,
    visuals: &SpellVisualCatalog,
    spell_id: u32,
    stage: impl Fn(&VisualStages) -> u32,
    weapon_visual: impl FnOnce() -> Option<u32>,
) -> Option<VisualKit> {
    let kit_id = stage(&resolve_stages(spells, visuals, spell_id, weapon_visual)?);
    (kit_id != 0)
        .then(|| visuals.kit(kit_id).copied())
        .flatten()
}

/// [`resolve_kit`] plus this lane's **one trace line** (`WOW_MOVE_TRACE`, tag `fx`) — the
/// instrument the cast-edge router went without until bug B307, where "does the shooter's body
/// kit resolve at all?" cost a day and three agents to answer and this line answers in one live
/// run.
///
/// It earns its place because of *how* this chain fails. A basic ranged shot — Auto Shot 75, wand
/// Shoot 5019, Throw 2764 — carries `SpellVisual = 0`, so its **entire** body animation comes from
/// the equipped ranged weapon's `ItemDisplayInfo` col-10 substitute visual ([`resolve_stages`]'s
/// merge). Every link in that chain degrades **silently** to "no kit": an empty ranged slot, an
/// item template the ask-once layer has not answered yet, a display id naming no visual. The
/// observable of each is identical, and identical to a shooter that simply never animates — so
/// the symptom names nothing and the code must. The line separates them:
/// `weapon=not-consulted` (a non-ranged spell — the fallback is not supposed to run),
/// `weapon=none` (it ran and found nothing — the shot IS silent, the B307 shape), or the
/// resolved visual id, followed by the kit and the anim id actually requested of the body. Pair
/// it with the driver's `fct: anim play unit=… id=…` to see whether that request reached bone 0.
///
/// Free when the trace is off: the caller's lookup still runs exactly once (the [`Cell`] only
/// records what it returned), and the diagnosis re-derives only inside the `enabled()` guard —
/// re-using the recorded value rather than re-running the lookup.
fn resolve_kit_traced(
    stage_name: &str,
    entity: Entity,
    spells: &crate::ui_action::Spells,
    visuals: &SpellVisualCatalog,
    spell_id: u32,
    stage: impl Fn(&VisualStages) -> u32,
    weapon_visual: impl FnOnce() -> Option<u32>,
) -> Option<VisualKit> {
    // `None` = the closure never ran (`resolve_stages` short-circuits before the fallback for a
    // non-ranged spell); `Some(v)` = it ran and returned `v`. The two are different diagnoses.
    let consulted: Cell<Option<Option<u32>>> = Cell::new(None);
    let kit = resolve_kit(spells, visuals, spell_id, &stage, || {
        let visual = weapon_visual();
        consulted.set(Some(visual));
        visual
    });
    if benilla_assets::trace::enabled() {
        let def = spells.catalog.get(spell_id);
        let weapon = match consulted.get() {
            None => "not-consulted".to_string(),
            Some(None) => "none".to_string(),
            Some(Some(visual)) => visual.to_string(),
        };
        // Re-derived from the RECORDED weapon visual — never a second lookup.
        let kit_id = resolve_stages(spells, visuals, spell_id, || consulted.get().flatten())
            .map_or(0, |s| stage(&s));
        benilla_assets::trace::line(
            "fx",
            &format!(
                "kit {stage_name} unit={entity} spell={spell_id} own_visual={} ranged_slot={} \
                 weapon={weapon} kit={kit_id} anim={:?}",
                def.map_or(0, |d| d.visual),
                def.is_some_and(|d| d.ranged_slot()),
                kit.and_then(|k| k.anim_id),
            ),
        );
    }
    kit
}

/// The in-flight cast's **strike sound** for the `$TRD` anim event (`0x62faa0`): the handler
/// resolves the spell's visual to its 16-dword `SpellVisual.dbc` row (`0x60d450` →
/// `DAT_00c0d738[visualId]`) and plays that row's **dword 14** (`[row+0x38]`) positioned at the
/// unit — wow-re `sound/scratch/gather-sound-anim-events.md`, decision 0562. Mining's visual 93
/// carries 1143 "Mining Impact" here (the pick clang — client-side complete, no server state);
/// the smithing crafts' 395 the same hammer; Herb's 91 carries 1142 but its anim never fires
/// `$TRD`. No ranged-weapon fallback (`$TRD` rides work/craft anims, never a ranged shot's).
pub(crate) fn held_strike_sound(
    spells: &crate::ui_action::Spells,
    visuals: &SpellVisualCatalog,
    spell_id: u32,
) -> Option<u32> {
    resolve_stages(spells, visuals, spell_id, || None)?.strike_sound
}

/// A kit's populated emitter slots resolved to `(attach tag, model path)` pairs — the
/// [`SpellKitFx::Begin`] payload. Slots whose `SpellVisualEffectName` row/path is missing are
/// dropped (the client's NULL-record skip in the slot loop).
fn resolve_kit_effects(visuals: &SpellVisualCatalog, kit: &VisualKit) -> Vec<(u16, String)> {
    kit.effects()
        .filter_map(|(tag, id)| visuals.effect_path(id).map(|p| (tag, p.to_string())))
        .collect()
}

/// A server-pushed kit outside any cast (`SMSG_PLAY_SPELL_VISUAL` → the net bridge; decision
/// 0280): the client bounds-checks the u32 against `SpellVisualKit.dbc` and plays it at a
/// hardcoded **stage 0** — the instant, self-terminating flavour (spellRec NULL, so its effects
/// carry spell id 0 and are reaped only by their own completion, `0x6e98d0` → `0x60edf0`). Live
/// traffic: the eat/drink kits (406/438) re-sent every ~5 s, and mid-channel kit swaps.
#[derive(Message, Clone, Copy)]
pub(crate) struct KitPush {
    pub(crate) entity: Entity,
    pub(crate) kit_id: u32,
    /// [`super::PlaySeq`] stamp at emission (the wire drain, in packet order).
    pub(crate) seq: u64,
}

/// The writer set one discrete kit play fans out to — bundled so [`play_kit`] threads through the
/// router's arms as one argument (each writer keeps its own system-param lifetime).
struct KitOut<'a, 'w1, 'w2, 'w3, 'w4, 'w5> {
    oneshots: &'a mut MessageWriter<'w1, EmoteAnim>,
    wounds: &'a mut MessageWriter<'w2, WoundAnim>,
    sounds: &'a mut MessageWriter<'w3, SpellKitSound>,
    fx: &'a mut MessageWriter<'w4, SpellKitFx>,
    /// The kit's beam, if its `CharProc` slots name one — the dispatcher's chain case (0955).
    chain: &'a mut MessageWriter<'w5, ChainProcPlay>,
}

/// One kit played as a **discrete event** on a unit — the client's `PlaySpellVisualKit`
/// (`0x60edf0`): the kit's anim, sound, and attach-point effect models. The anim branch is the
/// client's own (`0x60f3ad` → `0x60f510`): a CombatWound-family id (8–10) lays into the wound
/// SECONDARY-blend slot (never interrupting what plays — decision 0111); anything else is the
/// ordinary over-the-gait one-shot. `persistent` is the stage's effect-model lifetime
/// (decision 0107 verdict 2).
/// One kit play's fan-out policy — which legs run, per stage:
/// - `persistent`: the effect models' lifetime (the precast/channel hold vs self-terminating).
/// - `effects`: whether the models spawn here at all. `false` for the impact hand-off's STATE
///   stage — its models belong to the aura's life and [`arm_aura_state_fx`] owns them.
/// - `sound`: whether the kit's sound rings — **ungated and positional at the unit, every
///   stage** (wow-re `kit-sound-leg.md`, decision 0852: `0x60edf0`'s sound leg carries no self
///   test; the old "state sound is self-only" reading was `0x5ff43e`'s gate on a *different*
///   aura-apply cue, and `0x5fa6d0` appears nowhere in the kit play). The flag exists for
///   plays that deliberately hand the sound to another owner (none today).
#[derive(Clone, Copy)]
struct KitPlay {
    persistent: bool,
    effects: bool,
    sound: bool,
    /// The stage this play is, for the instances' animation lifecycle ([`FxStage`]).
    stage: FxStage,
}

impl KitPlay {
    /// The ordinary discrete play (kit push, cast release, impact stage): self-terminating
    /// models, every leg on. Stages 0/1 both land here — they share `0x5fbf50`.
    const DISCRETE: Self = Self {
        persistent: false,
        effects: true,
        sound: true,
        stage: FxStage::OneShot,
    };
}

fn play_kit(
    entity: Entity,
    spell_id: u32,
    kit: &VisualKit,
    play: KitPlay,
    seq: u64,
    visuals: &SpellVisualCatalog,
    out: &mut KitOut,
) {
    if let Some(anim_id) = kit.anim_id {
        if (8..=10).contains(&anim_id) {
            out.wounds.write(WoundAnim { entity, anim_id });
        } else {
            out.oneshots.write(EmoteAnim {
                entity,
                anim_id,
                seq,
            });
        }
    }
    if let Some(kit_sound) = kit.sound.filter(|_| play.sound) {
        out.sounds.write(SpellKitSound::Play { entity, kit_sound });
    }
    // The CharProc dispatcher's beam case — run for EVERY kit play, at every stage, exactly as
    // `PlaySpellVisualKit`'s tail runs it (`0x60f35c`). It is not gated by `play.effects`: the
    // beam is not one of the kit's attach-point effect models, and a kit whose caster carries no
    // hop array simply draws nothing (`0x60db01`'s `count == 0` exit).
    if let Some(proc) = kit.chain_proc() {
        out.chain.write(ChainProcPlay {
            entity,
            spell_id,
            proc,
        });
    }
    if !play.effects {
        return;
    }
    let effects = resolve_kit_effects(visuals, kit);
    if !effects.is_empty() {
        out.fx.write(SpellKitFx::Begin {
            entity,
            spell_id,
            persistent: play.persistent,
            class: FxClass::Hold,
            stage: play.stage,
            effects,
        });
    }
}

/// The **unit-impact hand-off** (the client's `0x61dc50`, decision 0099 phase 4): the spell
/// landed on `entity` — play the impact kit (stage 1), then the state kit (stage 2). The state
/// kit plays its anim + sound only: its **effect models live for the aura's life**, owned by
/// [`arm_aura_state_fx`]'s slot watcher (a single instance, begun when the aura lands in the
/// slots and reaped when it leaves — the same packet burst as the GO, so there is no visible
/// gap). The state sound rings **ungated and positional** (wow-re `kit-sound-leg.md`, decision
/// 0852: `0x60edf0`'s sound leg has no self test — the old self-only reading belonged to a
/// different aura-apply cue); the same-frame duplicate against the aura watcher's ADD-edge play
/// collapses in the sound router. Named approximation: the client's small stage-2 gate
/// `0x61dc20` (37 B, content unpinned) is not modeled. `weapon_visual` = the CASTER's ranged
/// fallback visual (already resolved — the caster may be gone by a missile arrival), through
/// which a basic shot's impact kit resolves.
fn play_impact(
    entity: Entity,
    spell_id: u32,
    weapon_visual: Option<u32>,
    seq: u64,
    spells: &crate::ui_action::Spells,
    visuals: &SpellVisualCatalog,
    out: &mut KitOut,
) {
    for (stage, play) in [
        (
            (|s: &VisualStages| s.impact) as fn(&VisualStages) -> u32,
            KitPlay::DISCRETE,
        ),
        (
            |s: &VisualStages| s.state,
            KitPlay {
                effects: false,
                ..KitPlay::DISCRETE
            },
        ),
    ] {
        if let Some(kit) = resolve_kit(spells, visuals, spell_id, stage, || weapon_visual) {
            play_kit(entity, spell_id, &kit, play, seq, visuals, out);
        }
    }
}

/// The cast-edge router (decision 0107): wire [`CastEvent`]s + the public channel descriptor →
/// the resolved animation intents the driver renders.
///
/// - **Start** → the **precast** kit: its anim becomes the unit's [`CastHold`] (sustained until
///   the cast resolves — the client's stage-4 persistence), its sound rings.
/// - **Go** → the hold drops; the **cast** kit plays as a discrete event ([`play_kit`]), and the
///   GO's target lists ([`SpellGoTargets`]) branch on `Spell.dbc` Speed — 0 → the impacts play
///   inline ([`play_impact`] per hit, the client's instant branch `0x6e8bf0`); >0 → one
///   [`MissileSpawn`] (the projectile branch `0x6e8a50 → 0x60a3d0`), whose arrivals come back as
///   `Impact` events.
/// - **Impact** → the spell landed on this unit (a missile arrival) — [`play_impact`].
/// - **Fail** → the hold drops silently (the client's spell-id-keyed reap; no release).
/// - **Channel** (polled per frame, the client's `0x612a30` over `UNIT_CHANNEL_SPELL` — observers
///   have no packet, decision 0099): field set → the **channel** kit's anim as the hold + its
///   sound once at start; field cleared → hold drops. The per-entity edge cache is the dedup the
///   client gets from its per-tick armed-id guard — a held channel never restarts its clip.
#[allow(clippy::too_many_arguments)]
pub(super) fn route_cast_visuals(
    mut commands: Commands,
    mut events: MessageReader<CastEvent>,
    mut go_targets: MessageReader<SpellGoTargets>,
    mut pushes: MessageReader<KitPush>,
    mut oneshots: MessageWriter<EmoteAnim>,
    mut wounds: MessageWriter<WoundAnim>,
    mut sounds: MessageWriter<SpellKitSound>,
    mut fx: MessageWriter<SpellKitFx>,
    mut sheaths: MessageWriter<super::SheathRequest>,
    // One tuple param (the 16-SystemParam ceiling): the missile spawns, the dest one-shot orders
    // (0797) and the beam plays (0955) — the spawn lanes resolved here where the catalogs live,
    // built by `crate::entities`.
    mut spawns: (
        MessageWriter<MissileSpawn>,
        MessageWriter<crate::entities::dest_fx::GroundBurst>,
        MessageWriter<ChainProcPlay>,
    ),
    visuals: Option<Res<SpellVisuals>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    mut weapon_src: WeaponVisualSrc,
    units: Query<(Entity, &ObjectStore)>,
    holds: Query<&CastHold>,
    mut channel_cache: Local<EntityHashMap<u32>>,
) {
    let (Some(visuals), Some(spells)) = (visuals.as_deref(), spells.as_deref()) else {
        return; // no client data — no spell visuals (every DBC resource degrades this way)
    };
    // Disjoint field borrows: the beam writer rides `out` for the whole body while the missile and
    // burst writers stay free for the GO loop below.
    let (missiles, bursts, chains) = (&mut spawns.0, &mut spawns.1, &mut spawns.2);
    let mut out = KitOut {
        oneshots: &mut oneshots,
        wounds: &mut wounds,
        sounds: &mut sounds,
        fx: &mut fx,
        chain: chains,
    };

    // The `holds` query is one command-flush stale: an instant cast's START and GO drain from
    // the wire in the same frame, so the GO's spell-id-keyed release must see the hold its own
    // batch's START just inserted — through the query it can't, the remove is skipped, and the
    // deferred insert lands unopposed: the cast pose loops forever (the Demon Armor / Ice Armor
    // stuck cast; the real client's handlers run synchronously in packet order, so it has no
    // such gap). `pending` overlays this frame's hold writes; every hold read goes through
    // `held_spell`.
    let mut pending: EntityHashMap<Option<u32>> = EntityHashMap::default();
    let held_spell = |pending: &EntityHashMap<Option<u32>>, entity: Entity| -> Option<u32> {
        match pending.get(&entity) {
            Some(&overlaid) => overlaid,
            None => holds.get(entity).ok().map(|h| h.spell_id),
        }
    };

    for p in pushes.read() {
        // The kit-push opcode (decision 0280, stage-0 semantics): fresh transient effects/sound
        // on EVERY send — the client allocates new CEffects per call, no dedup — while the body
        // anim rides the driver's arm-level same-id dedup (`0x5fdba0`), so a looping eat/drink
        // clip free-runs across the ~5 s resends instead of restarting.
        if let Some(kit) = visuals.0.kit(p.kit_id) {
            play_kit(
                p.entity,
                0,
                kit,
                KitPlay::DISCRETE,
                p.seq,
                &visuals.0,
                &mut out,
            );
        }
    }

    for ev in events.read() {
        // **The subject may already be dead** (B130's crash, the second ever reported): every
        // despawn of an *indexed* unit runs inside the wire drain — `DESTROY_OBJECT`, the
        // out-of-range stream-out, the worldport purge — and those commands are applied at the sync
        // point this chain sits behind (`.after(WorldStage::Net)`), so a SPELL_START and its
        // subject's death drain from the same batch and the edge outlives the unit. Every arm below
        // is *about* that unit (its hold, its sound, its effect models), so a dead subject skips
        // whole rather than spraying the downstream lanes with edges for an entity that is gone —
        // and rather than warning once per component removed off a corpse.
        if commands.get_entity(ev.entity).is_err() {
            continue;
        }
        match ev.kind {
            CastEventKind::Start => {
                // A replacing cast reaps the prior hold's loop before its own sound starts —
                // and the prior spell's persistent effect models, keyed by the hold it replaces.
                out.sounds
                    .write(SpellKitSound::StopHold { entity: ev.entity });
                if let Some(prior) = held_spell(&pending, ev.entity) {
                    out.fx.write(SpellKitFx::Reap {
                        entity: ev.entity,
                        spell_id: prior,
                        class: FxClass::Hold,
                    });
                }
                // A ranged-attribute shot arms the **ranged stance** the moment its START lands —
                // the client's snap `SetSheatheState(2,1,1)` on the same gate (remote casters at
                // `0x6e78f3`; the local player armed at cast-send `0x6e5930`, whose echo START
                // lands a frame later — same look). One START per auto-repeat activation
                // (VERIFIED, vmangos `Spell::prepare`: the per-shot re-casts are triggered and
                // send only GO), and the executor's `newState == CUR` refusal makes any
                // re-activation free.
                if spells
                    .catalog
                    .get(ev.spell_id)
                    .is_some_and(|d| d.ranged_attack())
                {
                    sheaths.write(super::SheathRequest {
                        entity: ev.entity,
                        state: 2,
                        ceremony: false,
                    });
                }
                if let Some(kit) = resolve_kit_traced(
                    "precast",
                    ev.entity,
                    spells,
                    &visuals.0,
                    ev.spell_id,
                    |s| s.precast,
                    || weapon_src.caster(ev.entity),
                ) {
                    // The `0x400` weapon-visual hold, ANY caster (wow-re
                    // `ranged-sheath-exempt-autorepeat.md` §Q4): a RANGED spell's visual play
                    // sets it (`0x60d020`, sole caller inside PlaySpellVisual); any other
                    // visual play clears it (the stale-visual cleanup `0x6ec39e` — re-set only
                    // when the new visual is ranged). It is what keeps a REMOTE shooter (an NPC
                    // archer, another hunter) in the drawn Load/Hold idle between shots — the
                    // local `0x200` ([`super::AutoRepeatArmed`]) never exists off the local
                    // cast-send.
                    let ranged = spells
                        .catalog
                        .get(ev.spell_id)
                        .is_some_and(|d| d.ranged_slot());
                    // Fallible, like every hold write in this system: the skip above sees only
                    // despawns already *applied*, and `model_fade::apply_despawn_fade` is
                    // Update-unordered against this chain — its instant path (a stream-out unit with
                    // no fadeable geometry, which is what a creature streamed in and back out at
                    // flight speed *is*) can queue the despawn this frame and have it applied before
                    // our commands. An infallible `insert` panics there; nothing can see it coming.
                    if ranged {
                        commands.entity(ev.entity).try_insert(super::RangedHold);
                    } else {
                        commands.entity(ev.entity).try_remove::<super::RangedHold>();
                    }
                    if let Some(anim_id) = kit.anim_id {
                        commands.entity(ev.entity).try_insert(CastHold {
                            anim_id,
                            spell_id: ev.spell_id,
                            ranged,
                        });
                        pending.insert(ev.entity, Some(ev.spell_id));
                    }
                    if let Some(kit_sound) = kit.sound {
                        out.sounds.write(SpellKitSound::Play {
                            entity: ev.entity,
                            kit_sound,
                        });
                    }
                    // The precast's attach-point effect models (the glowing hands) — persistent,
                    // the client's stage-4 lifetime: they live until the cast resolves, and their
                    // animation re-arms forever ([`FxStage::Relive`], `0x60ed00`).
                    let effects = resolve_kit_effects(&visuals.0, &kit);
                    if !effects.is_empty() {
                        out.fx.write(SpellKitFx::Begin {
                            entity: ev.entity,
                            spell_id: ev.spell_id,
                            persistent: true,
                            class: FxClass::Hold,
                            stage: FxStage::Relive,
                            effects,
                        });
                    }
                }
            }
            CastEventKind::Go => {
                // Spell-id-keyed reap (the client's `0x614150(spellId, 0)`) — a proc's GO landing
                // mid-cast never drops another spell's hold.
                if held_spell(&pending, ev.entity) == Some(ev.spell_id) {
                    commands.entity(ev.entity).try_remove::<CastHold>();
                    pending.insert(ev.entity, None);
                }
                // The release reaps the precast's loop unconditionally-of-hold-state: an instant
                // cast's Start may have begun a loop even when its precast kit carried no anim
                // (so no CastHold). The effect-model reap is the same shape, spell-id-keyed.
                out.sounds
                    .write(SpellKitSound::StopHold { entity: ev.entity });
                out.fx.write(SpellKitFx::Reap {
                    entity: ev.entity,
                    spell_id: ev.spell_id,
                    class: FxClass::Hold,
                });
                // A ranged-slot spell's GO **re-draws ranged before its cast-kit play** — the
                // kit-play path's internal snap (`0x60f34c`, gate `Attributes & 0x2`, stages
                // {0, 4} — wow-re `ranged-sheath-exempt-autorepeat.md` Q3): SPELL_GO makes no
                // direct SetSheatheState call, but every auto-repeat shot re-snaps state 2
                // through its play. This is what re-draws a bow an emote lowered mid-volley.
                if spells
                    .catalog
                    .get(ev.spell_id)
                    .is_some_and(|d| d.ranged_slot())
                {
                    sheaths.write(super::SheathRequest {
                        entity: ev.entity,
                        state: 2,
                        ceremony: false,
                    });
                }
                // The cast kit (the release flash) — a discrete play, effects self-terminating
                // after their model's own clip span (the client's stage-0/1 completion callback).
                // For a basic shot this is the fire clip itself (the ranged fallback: Throw's
                // AttackThrown, Auto Shot's AttackBow — wow-re `throw-ranged-attack-anim.md`).
                if let Some(kit) = resolve_kit_traced(
                    "cast",
                    ev.entity,
                    spells,
                    &visuals.0,
                    ev.spell_id,
                    |s| s.cast,
                    || weapon_src.caster(ev.entity),
                ) {
                    // The `0x400` hold's per-shot re-assert (and the stale-visual clear for a
                    // non-ranged play) — the Start arm's twin; every mid-volley GO keeps a
                    // remote shooter's hold alive.
                    if spells
                        .catalog
                        .get(ev.spell_id)
                        .is_some_and(|d| d.ranged_slot())
                    {
                        commands.entity(ev.entity).try_insert(super::RangedHold);
                    } else {
                        commands.entity(ev.entity).try_remove::<super::RangedHold>();
                    }
                    play_kit(
                        ev.entity,
                        ev.spell_id,
                        &kit,
                        KitPlay::DISCRETE,
                        ev.seq,
                        &visuals.0,
                        &mut out,
                    );
                }
            }
            CastEventKind::Impact { weapon_visual } => {
                // A missile arrived on this unit (decision 0099 phase 4; speed-0 impacts play
                // inline from the GO loop below and never round-trip through here). The caster's
                // ranged fallback rode the missile — resolve through it, not the target.
                play_impact(
                    ev.entity,
                    ev.spell_id,
                    weapon_visual,
                    ev.seq,
                    spells,
                    &visuals.0,
                    &mut out,
                );
            }
            CastEventKind::GroundImpact { pos } => {
                // A missile arrived at a POINT (`0x61e1d0`'s ground arm → `0x61d870`): play
                // `SpellVisual` field 13 — the **area kit** — at stage 3 on the caster, with
                // `extra` = the landing point. The reference then walks the missile's own
                // server-recorded hit array (`CMissile+0x58/+0x5c`) for per-unit impact/state
                // kits; ours is empty by construction — this arm is only reached because the
                // GO's hit list was empty, which is what selected the location fallback.
                //
                // The kit legs, against the shipped data: all six `SpellVisual` rows that can
                // reach here (Volley 3229, the bomb/dynamite family 1704/3270, Goblin Mortar
                // 695, Arcane Bomb 4831, Firecrackers 6447 — the whole census of speed>0 ∧
                // `Targets & 0x40` ∧ field 6 ≠ 0 ∧ field 13 ≠ 0) carry **no body animation and
                // no effect slots, only a sound**. So the arrival is exactly the kit's field-13
                // `SoundEntries` id at the landing point, and the anim/slot legs that would need
                // stage 3's forever-relive lifetime ([`FxStage::Relive`]) are dead data here —
                // left unbuilt rather than guessed at.
                // `|| None`: the weapon merge pointedly skips the dest-anchored `+0x2c/+0x30/
                // +0x34` block ([`VisualStages::merged_over_weapon`]), so even Volley's area kit
                // is its own row's — the lookup would be paid for nothing.
                if let Some(kit_sound) =
                    resolve_kit(spells, &visuals.0, ev.spell_id, |s| s.area_kit, || None)
                        .and_then(|k| k.sound)
                {
                    out.sounds.write(SpellKitSound::PlayAt { pos, kit_sound });
                }
            }
            CastEventKind::Fail => {
                if held_spell(&pending, ev.entity) == Some(ev.spell_id) {
                    commands.entity(ev.entity).try_remove::<CastHold>();
                    pending.insert(ev.entity, None);
                }
                out.sounds
                    .write(SpellKitSound::StopHold { entity: ev.entity });
                out.fx.write(SpellKitFx::Reap {
                    entity: ev.entity,
                    spell_id: ev.spell_id,
                    class: FxClass::Hold,
                });
            }
        }
    }

    // The GO's target lists: `Spell.dbc` Speed picks the client's two GO branches — 0 → every
    // hit's impact plays now (`0x6e8bf0`); >0 → a projectile per target (`0x6e8a50 → 0x60a3d0`),
    // arrival routed back as `Impact` by `crate::entities::missile`. Misses fly too (the client
    // deflects them off the target; ours fizzle silently — a named approximation).
    for go in go_targets.read() {
        let Some(display) = spells.catalog.get(go.spell_id) else {
            continue;
        };
        // The caster's ranged weapon visual, resolved once per GO (the client's `0x6e802e` call
        // into `0x60d450`) — its merged row `edi` is what EVERY consumer below reads: the dest
        // one-shot (`[edi+0x18]`/`[edi+0x30]`), the cast kit (`[edi+0x8]`), the inline impacts
        // (`6e8169 mov edx,edi`) and the missile spawn (`6e8199 mov edx,edi`).
        let wv = weapon_src.caster(go.caster);
        let stages = resolve_stages(spells, &visuals.0, go.spell_id, || wv);
        // The GO's dest one-shot (decision 0797, byte-pinned `0x6e8088`–`0x6e8143`): a
        // dest-carrying GO plays the SpellVisual field-12 model ONCE at the packet's point —
        // gate `field 6 == 0` (no missile owns the arrival) ∧ `field 12 ≠ 0`. NOT gated on the
        // hit list (a pure ground cast's lists are empty and the burst still plays — the
        // captured Flamestrike shape); fired here at the GO, never waiting on the dynobj
        // create (wow-re trap #6: the burst precedes the object).
        if let Some(dest) = go.dest {
            if let Some(stages) = stages {
                if stages.missile_gate == 0 && stages.area_effect != 0 {
                    if let Some(path) = visuals.0.effect_path(stages.area_effect) {
                        bursts.write(crate::entities::dest_fx::GroundBurst {
                            path: path.to_string(),
                            pos: dest,
                        });
                    }
                }
            }
        }
        if display.speed <= 0.0 {
            for &target in &go.hits {
                play_impact(
                    target,
                    go.spell_id,
                    wv,
                    go.seq,
                    spells,
                    &visuals.0,
                    &mut out,
                );
            }
        } else {
            // The spawn gate is Speed **alone** (byte-pinned: `0x60a3d0` fires on the projectile
            // gate's fcomp; every basic shot spell has no `SpellVisual` row at all and still
            // flies). The missile-model chain: field 7 ≥ 1 → its effect model (the client's
            // literal ErrorCube when unresolvable); < 1 / no visual row → `None`, and the
            // spawner falls to the GO's wire ammo model (`0x479f40`, phase 5).
            let path = stages.and_then(|s| {
                (s.missile_model >= 1).then(|| {
                    visuals
                        .0
                        .effect_path(s.missile_model)
                        .unwrap_or(ERROR_CUBE)
                        .to_string()
                })
            });
            // Field 9's ordinal → the attach tag the missile homes to. An out-of-table ordinal
            // reads adjacent `.rdata` in the client; here it degrades to the base position.
            let dest_tag =
                stages.and_then(|s| MISSILE_ATTACH_TABLE.get(s.missile_attach as usize).copied());
            // Field 10: the flight loop the projectile carries the whole way (thrown WeaponLoop,
            // fireball FireMissileLoop) — resolved off the same row as the model/attach.
            let missile_sound = stages.and_then(|s| s.missile_sound);
            let targets: Vec<(Entity, Option<u8>)> = go
                .hits
                .iter()
                .map(|&e| (e, None))
                .chain(go.misses.iter().map(|&(e, code)| (e, Some(code))))
                .collect();
            // The location fallback ([`MissileSpawn::ground_aim`]): an empty hit array plus a
            // point on the wire flies ONE projectile at the point. The hit array wins whenever
            // it has anything in it — the client only consults the latch after
            // `0x6e8abc … je 0x6e8ba2` has found the array empty.
            let ground_aim = targets.is_empty().then_some(go.dest).flatten();
            if !targets.is_empty() || ground_aim.is_some() {
                // The release gate (the client's `0x6e7a70` flush condition, inverted): a cast
                // kit that plays a body animation defers the launch to its release keyframe;
                // no kit / no anim (`kit+8 < 1` ⇔ our `anim_id: None`) launches at GO.
                let awaits_release = stages
                    .filter(|s| s.cast != 0)
                    .and_then(|s| visuals.0.kit(s.cast))
                    .is_some_and(|k| k.anim_id.is_some());
                missiles.write(MissileSpawn {
                    caster: go.caster,
                    spell_id: go.spell_id,
                    path,
                    ammo_display_id: go.ammo_display_id,
                    dest_tag,
                    speed: display.speed,
                    targets,
                    ground_aim,
                    weapon_visual: wv,
                    missile_sound,
                    awaits_release,
                });
            }
        }
    }

    // The channel poll: an edge on the PUBLIC `UNIT_CHANNEL_SPELL` descriptor drives an observed
    // unit's channel loop (the self-only MSG_CHANNEL_* never carry this — decision 0099). Only
    // *edges* act, so a held channel's clip is started once (the client's dedup guard) and a
    // cleared field releases the hold even when the interrupt had no other wire trace.
    for (entity, store) in &units {
        let cur = store.0.unit_channel_spell();
        let prev = channel_cache.get(&entity).copied().unwrap_or(0);
        if cur == prev {
            continue;
        }
        channel_cache.insert(entity, cur);
        if cur != 0 {
            out.sounds.write(SpellKitSound::StopHold { entity });
            if prev != 0 {
                // A channel replacing a channel: the old spell's effects reap first.
                out.fx.write(SpellKitFx::Reap {
                    entity,
                    spell_id: prev,
                    class: FxClass::Hold,
                });
            }
            // No ranged fallback here: the channel poll is the descriptor-driven `0x612a30`
            // path, not the SPELL_START/GO handlers' `0x60d450` resolve — and no basic shot
            // channels anyway.
            if let Some(kit) = resolve_kit(spells, &visuals.0, cur, |s| s.channel, || None) {
                if let Some(anim_id) = kit.anim_id {
                    // Fallible for the same reason as the wire arms above — and this loop's
                    // subjects need it *more*: a unit mid-stream-out is un-indexed but still
                    // carries its `ObjectStore`, so it is in this very query while the fade lane
                    // is free to despawn it out from under us this frame.
                    commands.entity(entity).try_insert(CastHold {
                        anim_id,
                        spell_id: cur,
                        ranged: false, // no basic shot channels (the comment above)
                    });
                    pending.insert(entity, Some(cur));
                }
                if let Some(kit_sound) = kit.sound {
                    out.sounds.write(SpellKitSound::Play { entity, kit_sound });
                }
                // The CharProc dispatcher's SECOND caller (`0x612b18`, inside this very poll
                // `0x612a30`) — which is how a channelled beam exists at all: Drain Life's kit
                // is never reached by `PlaySpellVisualKit`, only from here. Emitted on the
                // channel's rising edge; the beam then lives until the field clears (0955).
                if let Some(proc) = kit.chain_proc() {
                    out.chain.write(ChainProcPlay {
                        entity,
                        spell_id: cur,
                        proc,
                    });
                }
                // The channel kit's effect models — persistent while the field holds (the
                // client's stage-2 lifetime, and so the stage-2 Birth → Hold → Decay lifecycle:
                // the channel poll `0x612a30` is one of the caller table's stage-2 sites).
                let effects = resolve_kit_effects(&visuals.0, &kit);
                if !effects.is_empty() {
                    out.fx.write(SpellKitFx::Begin {
                        entity,
                        spell_id: cur,
                        persistent: true,
                        class: FxClass::Hold,
                        stage: FxStage::State,
                        effects,
                    });
                }
            }
        } else {
            if held_spell(&pending, entity) == Some(prev) {
                // Only the ending channel's own hold is reaped — a precast for the unit's next
                // spell (already in flight when the field clears) survives.
                commands.entity(entity).try_remove::<CastHold>();
                pending.insert(entity, None);
            }
            out.sounds.write(SpellKitSound::StopHold { entity });
            out.fx.write(SpellKitFx::Reap {
                entity,
                spell_id: prev,
                class: FxClass::Hold,
            });
        }
    }
    // Streamed units despawn on range-out; drop their stale edge-cache rows with them.
    channel_cache.retain(|e, _| units.contains(*e));
}

/// Arm/reap the **aura state kit** (stage 2's real lifetime — the bread in the eater's hand):
/// a spell id appearing in a unit's `UNIT_AURA` slots arms its state kit's effect models
/// **persistent**, and the id leaving the slots reaps them. This is what makes the food bread
/// (spell 433 → visual 51 → state kit 409 → `Spells\Item_Bread.mdx` at the spell hand) sit in
/// the hand for the aura's whole life — and puts it in the hand of a unit that streamed in
/// already eating (the slots are public), which no impact-time play can do. Closes the 0107
/// approximation "the state kit plays self-terminating (no aura tracking yet)": [`play_impact`]
/// still plays the state kit as its short impact-time flash (the client's `0x61dc50` order —
/// impact, then state), which simply overlaps the persistent instance this watcher owns.
///
/// The trigger and reap are byte-verified (wow-re `state-kit-aura-lifecycle.md`, §5 2026-07-14):
/// the aura watcher `0x604d00 → 0x6123f0 → 0x5ff350` reads `SpellVisual` field 4 and plays the
/// kit at stage 2 (`0x5ff4c2: push 2`); the remove path `0x612320 → 0x5ff290` reaps with
/// `0x614150(spellId, force=1)`. The ADD edge's kit sound rings **ungated and positional** (wow-re
/// `kit-sound-leg.md`, decision 0852 — its §5 corrected the earlier "SELF-gated" reading: the
/// `0x5fa6d0` gate at `0x5ff43e` covers only a separate spellRec-driven aura-apply cue, and both
/// branches fall through to the ungated kit play at `0x5ff4c6`); a looping kit sound is tracked
/// and stops with the aura ([`SpellKitSound::StopKit`]). The body anim is NOT code-suppressed
/// (`0x60edf0`'s tail plays `kit+0x08` unconditionally) — this watcher plays effects + sound,
/// with a **named residual** for a state kit carrying a real anim: that ADD-edge replay isn't
/// built (no live kit demonstrates it; build it from the verdict when one shows).
///
/// `armed` tracks exactly the (unit, spell) pairs this watcher began, so the REMOVE edge never
/// reaps another owner's persistent instances (a channel hold whose spell also rides an aura).
/// An aura refresh keeps its slot id present (no edge); a re-apply that flickers remove→add
/// across frames reaps then re-begins — the drain's replace-on-persistent-begin keeps even the
/// same-frame corner single-instanced.
/// A state kit has **two halves** and this watcher owns the slot diff for both: the attach-point
/// effect *models* above, and the kit's `CharProc` columns — what the aura does to the body itself
/// (its translucency, its tint), emitted as [`AuraProc`] edges for [`crate::aura_visual`]. One diff
/// and one kit resolve feed both fan-outs, the same way [`KitOut`] bundles a discrete play's writers,
/// so the two halves of a kit can never disagree about when an aura came or went.
///
/// **The arm predicate is "this kit does *anything*", not "this kit has effect models"** — B114 was
/// exactly that bug: Stealth's kit 312 carries no models, no anim and no attach at all (its whole
/// visual is one proc-14 CharProc), so an effects-only test dropped it and the character showed
/// nothing.
#[allow(clippy::too_many_arguments)] // one Bevy system's full input set
pub(crate) fn arm_aura_state_fx(
    // The slot diff below is a pure function of the store's aura fields, so it re-runs only when
    // the store was written — `arm_level_up_fx`'s idiom (decision 1357's sibling gate): at the
    // LBRS pin this was ~800 full aura-slot walks/frame re-deriving an unchanged answer. The
    // unfiltered twin runs exactly once per DBC-resource arrival: a unit that streamed in before
    // `SpellVisuals`/`Spells` landed carries standing auras no store write will re-announce.
    units: Query<(Entity, &ObjectStore)>,
    changed: Query<(Entity, &ObjectStore), Changed<ObjectStore>>,
    visuals: Option<Res<SpellVisuals>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    mut fx: MessageWriter<SpellKitFx>,
    mut procs: MessageWriter<AuraProc>,
    mut sounds: MessageWriter<SpellKitSound>,
    mut armed: Local<EntityHashMap<Vec<u32>>>,
) {
    let full_sweep = visuals.as_ref().is_some_and(|v| v.is_changed())
        || spells.as_ref().is_some_and(|s| s.is_changed());
    let (Some(visuals), Some(spells)) = (visuals.as_deref(), spells.as_deref()) else {
        return; // no client data — no spell visuals (the DBC-resource degrade shape)
    };
    let scan = if full_sweep {
        units.iter().collect::<Vec<_>>()
    } else {
        changed.iter().collect::<Vec<_>>()
    };
    for (entity, store) in scan {
        let prev = armed.entry(entity).or_default();
        // Occupied slots, deduped (the same spell re-applied by two casters holds two slots —
        // one state instance either way).
        let mut cur: Vec<u32> = store.0.unit_auras().map(|a| a.spell_id).collect();
        cur.sort_unstable();
        cur.dedup();
        for &spell_id in prev.iter() {
            if cur.binary_search(&spell_id).is_err() {
                fx.write(SpellKitFx::Reap {
                    entity,
                    spell_id,
                    class: FxClass::AuraState,
                });
                procs.write(AuraProc::Reap { entity, spell_id });
                // A LOOPING state-kit sound is a tracked hold that dies with the aura
                // (`0x614150`'s 0.15 s fade — 0852); kit-scoped, a no-op for one-shots.
                if let Some(kit_sound) =
                    resolve_kit(spells, &visuals.0, spell_id, |s| s.state, || None)
                        .and_then(|k| k.sound)
                {
                    sounds.write(SpellKitSound::StopKit { entity, kit_sound });
                }
            }
        }
        let mut next = Vec::with_capacity(prev.len());
        for &spell_id in &cur {
            if prev.contains(&spell_id) {
                next.push(spell_id); // already armed, aura still live
                continue;
            }
            let Some(kit) = resolve_kit(spells, &visuals.0, spell_id, |s| s.state, || None) else {
                continue; // no state kit — not this watcher's aura
            };
            let effects = resolve_kit_effects(&visuals.0, &kit);
            let nodes: Vec<crate::aura_visual::AuraNode> = kit
                .char_procs()
                .filter_map(crate::aura_visual::node_for)
                .collect();
            if effects.is_empty() && nodes.is_empty() && kit.sound.is_none() {
                continue; // a state kit that does nothing we model — nothing to arm or reap
            }
            if !effects.is_empty() {
                fx.write(SpellKitFx::Begin {
                    entity,
                    spell_id,
                    persistent: true,
                    class: FxClass::AuraState,
                    stage: FxStage::State,
                    effects,
                });
            }
            if !nodes.is_empty() {
                procs.write(AuraProc::Begin {
                    entity,
                    spell_id,
                    nodes,
                });
            }
            // The ADD edge rings the kit sound — ungated and positional (0852: the kit play's
            // sound leg has no self test; the old self-only reading belonged to a different
            // aura-apply cue). Covers the streamed-in-mid-aura unit no impact play can; the
            // same-frame duplicate against the impact hand-off's state flash collapses in the
            // sound router.
            if let Some(kit_sound) = kit.sound {
                sounds.write(SpellKitSound::Play { entity, kit_sound });
            }
            next.push(spell_id);
        }
        *prev = next;
    }
    // Streamed units despawn on range-out; their instances die with the entity — drop the rows.
    armed.retain(|e, _| units.contains(*e));
}

/// Arm/reap the **lootable sparkle** on both things that can wear it (wow-re
/// `loot-corpse-effect.md` + `corpse-decal-and-loot-sparkle.md` §Q2, both §5 cross-checked):
/// a DEAD unit carrying `UNIT_DYNFLAG_LOOTABLE`, **and a corpse object carrying
/// `CORPSE_FIELD_DYNAMIC_FLAGS` bit 0** (decision 1723 — a battleground body's insignia). Each
/// wears the `SpellVisualEffectName` row named
/// `"HARDCODED Loot Art"` (5875: `Particles\LootFX.mdl` — a golden flare + three star-twinkle
/// emitters; cadence/size/color/blend all authored in the asset, the client sets none of them)
/// attached at `0x13` — the unit's looping its own first sequence, the corpse's arming no sequence
/// at all (the divergence is named at the `stage` line below). The real client is edge-driven off the
/// descriptor apply (watcher `0x600440`) with **no viewer/tap/distance/loot-window logic** — the
/// server already strips the flag per viewer (vmangos's "hide lootable animation for unallowed
/// players"). The falling edge (looted empty, rights lost) reaps with no fade (`0x600680`);
/// a despawn tears the instance down with the unit. Rides the spell-kit fx plumbing under
/// [`LOOT_FX_KEY`] — the client hangs loot art on the same `Effect_C` node type its spell
/// visuals use, so sharing the one attach body is the faithful shape.
pub(super) fn arm_loot_fx(
    // Dead+lootable is a pure function of store fields — the same `Changed<ObjectStore>` gate as
    // `arm_level_up_fx` below and `arm_aura_state_fx` above, with the same one-shot full sweep
    // when the DBC resource lands after units already streamed in. `NetEntity` splits the two
    // predicates: a **unit** answers `UNIT_DYNFLAG_LOOTABLE`, a **corpse object** answers its own
    // `CORPSE_FIELD_DYNAMIC_FLAGS` bit 0 (decision 1723) — different fields at different indices,
    // and a corpse descriptor has no UNIT block to ask at all.
    units: Query<(Entity, &ObjectStore, &crate::net::NetEntity)>,
    changed: Query<(Entity, &ObjectStore, &crate::net::NetEntity), Changed<ObjectStore>>,
    visuals: Option<Res<SpellVisuals>>,
    mut fx: MessageWriter<SpellKitFx>,
    mut armed: Local<EntityHashSet>,
) {
    let full_sweep = visuals.as_ref().is_some_and(|v| v.is_changed());
    let Some(path) = visuals.as_ref().and_then(|v| v.0.loot_art_path()) else {
        return; // no client data / no such row — no loot art (the DBC-resource degrade shape)
    };
    let scan = if full_sweep {
        units.iter().collect::<Vec<_>>()
    } else {
        changed.iter().collect::<Vec<_>>()
    };
    for (entity, store, net) in scan {
        // The **corpse object**'s own sparkle (wow-re `corpse-decal-and-loot-sparkle.md` §Q2, §5
        // cross-checked): `0x5d6de0` watches `CORPSE_FIELD_DYNAMIC_FLAGS`, and bit 0 rising calls
        // `0x5d6e30` → the same `"HARDCODED Loot Art"` row at the same attachment `0x13`. Same
        // asset, same tag — so it shares this body rather than growing a second one.
        let lootable = if net.kind == benilla_protocol::EntityKind::Corpse {
            store.0.corpse_lootable()
        } else {
            store.0.unit_is_dead() && store.0.unit_lootable()
        };
        if lootable && armed.insert(entity) {
            fx.write(SpellKitFx::Begin {
                entity,
                spell_id: LOOT_FX_KEY,
                persistent: true,
                class: FxClass::Hold,
                // Not a kit stage at all (this is `SpawnHardcodedEffect`, not
                // `PlaySpellVisualKit`), but wow-re `loot-corpse-effect.md`'s recorded law for the
                // loot art is "its own first sequence, LOOPING" — which is precisely what
                // [`FxStage::Relive`] renders, and unlike a bare loop-flag arm it holds even if the
                // art model's sequence were clamp-flagged.
                // **The one place the two paths differ, and it is a real divergence.** The unit's
                // loot art is an `Effect_C` node with a clip-end re-arm loop — "its own first
                // sequence, LOOPING", which is what `Relive` renders. The corpse's is a **bare
                // model instance**: `0x5d6e30` arms no sequence and registers no callback at all
                // (`[model+0x80]` stays `-1`), so the `.m2`'s emitters are the entire look and
                // nothing re-arms them. `OneShot` + `persistent` is the nearest shape this
                // plumbing expresses — no watcher, no re-arm, and it lives until the flag falls.
                // Named rather than smoothed over: if a lootable bone pile is ever seen sparkling
                // on a loop where the reference's plays out and stops, this is the line.
                stage: if net.kind == benilla_protocol::EntityKind::Corpse {
                    FxStage::OneShot
                } else {
                    FxStage::Relive
                },
                effects: vec![(HARDCODED_FX_ATTACH, path.to_string())],
            });
        } else if !lootable && armed.remove(&entity) {
            fx.write(SpellKitFx::Reap {
                entity,
                spell_id: LOOT_FX_KEY,
                class: FxClass::Hold,
            });
        }
    }
    // Streamed units despawn on range-out; their instances die with the entity — drop the rows.
    armed.retain(|e| units.contains(*e));
}

/// The engine-spawned **ding** (decision 0305, byte-verified — wow-re `levelup-ding.md`): a
/// `UNIT_FIELD_LEVEL` **change** on any streamed unit (the client's descriptor change-watcher
/// `CMirrorHandler 0x6045b0` → `SpawnHardcodedEffect(5)`) spawns [`LEVEL_UP_EFFECT`]
/// (`Spells\LevelUp\LevelUp.mdl`) at the base attach — the ding visual is DECOUPLED from
/// `SMSG_LEVELUP_INFO`, so anyone leveling nearby flashes too. First sight of a unit arms
/// silently (it streamed in *carrying* a level, it didn't just gain one). The instance
/// self-terminates on its own 1.867 s clip (spell id 0, the kit-push stage-0 shape — no reap
/// key needed, unlike the persistent loot sparkle above), and its **sound is the model's own**
/// `$SND(888)` event → `Sound\Spells\LevelUp.wav`, fired by
/// `crate::entities::spell_fx`'s event-track scanner.
pub(super) fn arm_level_up_fx(
    changed: Query<(Entity, &ObjectStore), Changed<ObjectStore>>,
    units: Query<(), With<ObjectStore>>,
    visuals: Option<Res<SpellVisuals>>,
    mut levels: Local<EntityHashMap<u32>>,
    mut fx: MessageWriter<SpellKitFx>,
) {
    for (entity, store) in &changed {
        let Some(level) = store.0.unit_level() else {
            continue;
        };
        match levels.insert(entity, level) {
            Some(prev) if prev != level => {
                let Some(path) = visuals
                    .as_ref()
                    .and_then(|v| v.0.hardcoded_effect(LEVEL_UP_EFFECT))
                else {
                    continue; // no client data / no such row (the DBC-resource degrade shape)
                };
                debug!("anim: level {prev} → {level}, the ding flashes ({entity})");
                fx.write(SpellKitFx::Begin {
                    entity,
                    spell_id: 0,
                    persistent: false,
                    class: FxClass::Hold,
                    // The ding self-terminates on its own 1.867 s clip — the stage-0 shape.
                    stage: FxStage::OneShot,
                    effects: vec![(HARDCODED_FX_ATTACH, path.to_string())],
                });
            }
            _ => {} // first sight arms; an unchanged write is a no-op
        }
    }
    // Streamed units despawn on range-out — drop their level memory with them.
    levels.retain(|e, _| units.contains(*e));
}

/// The **mount poof** — the cloud the director reports seeing at mount-up and we never drew.
///
/// The reference's `UNIT_FIELD_MOUNTDISPLAYID` change-watcher (`0x604329` → `0x604570` →
/// `0x5ffa50`) does three things on its **build** leg, and only there: tear down any old mount
/// (`0x607ce0`, gated on the OLD value), build the new one (`0x607a00`), and then spawn a
/// `HARDCODED` effect — `SMemAlloc(0x90)` → `Effect_C` ctor `0x61f490` → `0x61fae0(index=6,
/// &ownerGUID, cb=0x5fbf50)` → `0x6210e0` (`5ffa90`–`5ffad9`). Index **6** resolves through the
/// boot name-match table to `"HARDCODED Mount Poof"` → row 1185 → `Spells\DruidMorph_Impact_Base`
/// — the druid-morph cloud, which is exactly the "cloud shapeshift animation" of the report.
/// Byte-for-byte the same spawn shape as [`arm_level_up_fx`]'s index 5, so it is the same edge
/// here (wow-re `mount-composition.md` Q4b, §5 2026-08-03; decision 0927).
///
/// The three properties that decide the shape of this arm, all VERIFIED there:
/// - **Mount side only.** The build *and the entire allocation* sit behind `5ffa87 je 0x5ffade`
///   on the **NEW** field value. `N→0` jumps clean past it: **no poof on dismount.** `0→N` and
///   `N→N′` both spawn one.
/// - **Any unit.** No active-player, local-player or distance gate exists anywhere in `0x5ffa50`
///   — the same shape as the level-up ding. Range is the server's replication, not a filter.
/// - **One-shot, on the rider's own body.** `[node+0x2c] = 0` (no world-plant, no dedup marker,
///   not cancel-immune), tag `0x13` re-resolved against `[owner+0xd8]` every tick, and the
///   registered callback is `0x5fbf50` — the CEffect *end-of-clip terminator* (its looping twin
///   `0x600640` is what makes the loot sparkle repeat). So: `FxStage::OneShot`, non-persistent,
///   2.8 s of its own clock. Because the rider's body is the mount's child at slot 0 (0917), the
///   cloud rides the saddle for its whole span with no extra plumbing.
///
/// **Stated divergence.** The reference's tail `0x6208e0` is a same-record+same-tag dedup that
/// destroys a still-running poof when a new one spawns on that unit; our one-shot instances carry
/// no reap key, so a mount *swap* inside 2.8 s would show two overlapping clouds. Unreachable
/// today (the mounted gate refuses a second mount spell) and bounded at "one extra puff", so it
/// is named rather than modelled.
pub(super) fn arm_mount_poof_fx(
    changed: Query<(Entity, &ObjectStore), Changed<ObjectStore>>,
    units: Query<(), With<ObjectStore>>,
    visuals: Option<Res<SpellVisuals>>,
    mut displays: Local<EntityHashMap<u32>>,
    mut fx: MessageWriter<SpellKitFx>,
) {
    for (entity, store) in &changed {
        let mount_display = store.0.unit_mount_display_id();
        match displays.insert(entity, mount_display) {
            // The build leg's gate is on the NEW value, so a dismount (`mount_display == 0`)
            // spawns nothing — and first sight arms silently, exactly like the ding: a unit that
            // streams in already mounted did not just mount.
            Some(prev) if prev != mount_display && mount_display != 0 => {
                let Some(path) = visuals
                    .as_ref()
                    .and_then(|v| v.0.hardcoded_effect(MOUNT_POOF_EFFECT))
                else {
                    continue; // no client data / no such row (the DBC-resource degrade shape)
                };
                debug!("anim: mount display {prev} → {mount_display}, the poof puffs ({entity})");
                fx.write(SpellKitFx::Begin {
                    entity,
                    spell_id: 0,
                    persistent: false,
                    class: FxClass::Hold,
                    // `0x5fbf50` — destroy at the first completion; the shipped model runs 2.8 s.
                    stage: FxStage::OneShot,
                    effects: vec![(HARDCODED_FX_ATTACH, path.to_string())],
                });
            }
            _ => {}
        }
    }
    // Streamed units despawn on range-out — drop their mount memory with them.
    displays.retain(|e, _| units.contains(*e));
}

/// The one-slot **pending-morph latch** — the reference's `[unit+0xd54]`, a `SpellRec*` (wow-re
/// `shapeshift-morph-cloud.md`, §5 trio 2026-08-21): armed by `0x5ff0c0` on every aura **add
/// AND remove** whose spell passes [`is_morph_spell`]; consumed and cleared by the DISPLAYID
/// rebuild `0x60abe0`, which — after draining every attached effect and rebuilding the model —
/// **replays the latched spell's IMPACT kit** (stage 1, `0x6ec1e0` → `[SpellVisual+0xc]` →
/// `0x60edf0` at `0x60ad67`, cleared at `0x60ad6c`). That replay IS the druid-morph cloud the
/// player sees, both directions: shift-in's SPELL_GO impact instance dies in the drain and the
/// replay restores it in sync with the swap; shift-out has no GO at all (the cancel is
/// client-side, `shapeshift-plaincast-toggle.md`) — the aura-REMOVE handler's last call
/// (`0x6123ad`) re-arms the latch, and the aura fields precede DISPLAYID in the applier's
/// ascending field order, so the demorph revert replays the same kit. A swap with no latch — a
/// GM morph, a revive — plays nothing. One entry per unit (a one-slot field); entries die with
/// the unit ([`arm_morph_latch`]'s sweep).
#[derive(Resource, Default)]
pub(crate) struct MorphLatch(EntityHashMap<u32>);

/// The latch-arm predicate `0x5ff100`: any effect slot with `Effect == 6` (APPLY_AURA) whose
/// `EffectApplyAuraName` is 36 (MOD_SHAPESHIFT) or 56 (TRANSFORM) — slot-paired, and NO other
/// gate (no class/positive/unit-type test anywhere in `0x5ff0c0`).
fn is_morph_spell(spells: &crate::ui_action::Spells, spell_id: u32) -> bool {
    const SPELL_EFFECT_APPLY_AURA: u32 = 6;
    const SPELL_AURA_MOD_SHAPESHIFT: u32 = 36;
    const SPELL_AURA_TRANSFORM: u32 = 56;
    spells.catalog.get(spell_id).is_some_and(|d| {
        d.effects.iter().zip(&d.effect_apply_aura).any(|(&e, &a)| {
            e == SPELL_EFFECT_APPLY_AURA
                && (a == SPELL_AURA_MOD_SHAPESHIFT || a == SPELL_AURA_TRANSFORM)
        })
    })
}

/// Arm [`MorphLatch`] from the aura-slot diff — the reference's TWO arm sites folded into the
/// one place benilla sees both edges: the aura-add watcher and the aura-remove handler's tail
/// (`0x6123ad`) both call `0x5ff0c0(spellId)`, and a later arm overwrites an earlier one (one
/// slot). First sight of a unit seeds the diff baseline silently — the reference's watchers
/// fire on VALUES deltas, never on create, so a druid streaming in mid-form must not latch.
/// Keyed to its own full-slot diff rather than [`arm_aura_state_fx`]'s `armed` map, which
/// tracks only state-kit spells — the form spells carry none.
pub(super) fn arm_morph_latch(
    units: Query<(Entity, &ObjectStore)>,
    changed: Query<(Entity, &ObjectStore), Changed<ObjectStore>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    mut latch: ResMut<MorphLatch>,
    mut seen: Local<EntityHashMap<Vec<u32>>>,
) {
    for (entity, store) in &changed {
        let mut cur: Vec<u32> = store.0.unit_auras().map(|a| a.spell_id).collect();
        cur.sort_unstable();
        cur.dedup();
        match seen.entry(entity) {
            bevy::platform::collections::hash_map::Entry::Vacant(e) => {
                e.insert(cur); // first sight arms silently
            }
            bevy::platform::collections::hash_map::Entry::Occupied(mut e) => {
                let prev = e.get_mut();
                if *prev == cur {
                    continue; // some other field moved
                }
                if let Some(spells) = spells.as_deref() {
                    // Remove edges first, add edges second: a form→form swap latches the form
                    // being ENTERED (both resolve to the same cloud family regardless).
                    let removed = prev.iter().filter(|s| cur.binary_search(s).is_err());
                    let added = cur.iter().filter(|s| prev.binary_search(s).is_err());
                    for &spell_id in removed.chain(added) {
                        if is_morph_spell(spells, spell_id) {
                            debug!("anim: morph latch armed ({entity}, spell {spell_id})");
                            latch.0.insert(entity, spell_id);
                        }
                    }
                }
                *prev = cur;
            }
        }
    }
    // Streamed units despawn on range-out — the latch and the baseline die with them.
    latch.0.retain(|e, _| units.contains(*e));
    seen.retain(|e, _| units.contains(*e));
}

/// The rebuild's **impact-kit replay** — the tail of the reference's `0x60abe0` (wow-re
/// `shapeshift-morph-cloud.md`): a display swap that finds the latch armed plays the latched
/// spell's impact kit as an ordinary discrete kit play ([`KitPlay::DISCRETE`] — full
/// `PlaySpellVisualKit`: effects, sound, anim, CharProcs) and clears the latch; a swap with no
/// latch plays nothing. Runs off [`crate::entities::DisplaySwapped`], which the swap writes as
/// it tears the visual down — the message crosses to the next frame, so the replay's instance
/// spawns onto (or pends for) the rebuilt body, never the corpse of the old one. Named
/// approximation: the reference's same-record+same-tag dedup `0x6208e0` is not modelled, so a
/// cold-cache shift-in — whose SPELL_GO impact instance was still PENDING at the drain and thus
/// survived — briefly runs that instance and the replay's twin together (one denser cloud, once
/// per session per model; the warm-cache GO instance dies in the drain like the reference's).
#[allow(clippy::too_many_arguments)] // one Bevy system's full input set
pub(super) fn replay_morph_kit(
    mut swaps: MessageReader<crate::entities::DisplaySwapped>,
    visuals: Option<Res<SpellVisuals>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    mut latch: ResMut<MorphLatch>,
    mut play_seq: ResMut<super::PlaySeq>,
    mut oneshots: MessageWriter<EmoteAnim>,
    mut wounds: MessageWriter<WoundAnim>,
    mut sounds: MessageWriter<SpellKitSound>,
    mut fx: MessageWriter<SpellKitFx>,
    mut chain: MessageWriter<ChainProcPlay>,
) {
    for swap in swaps.read() {
        // Consume-and-clear even when the kit resolves to nothing — the reference clears
        // unconditionally at `0x60ad6c`.
        let Some(spell_id) = latch.0.remove(&swap.entity) else {
            continue; // no latch — a GM morph / revive swap plays nothing
        };
        let (Some(visuals), Some(spells)) = (visuals.as_deref(), spells.as_deref()) else {
            continue; // no client data — no spell visuals (the DBC-resource degrade shape)
        };
        let Some(kit) = resolve_kit(spells, &visuals.0, spell_id, |s| s.impact, || None) else {
            continue; // a morph spell with no impact kit — a silent swap
        };
        debug!("anim: morph replay ({}, spell {spell_id})", swap.entity);
        let mut out = KitOut {
            oneshots: &mut oneshots,
            wounds: &mut wounds,
            sounds: &mut sounds,
            fx: &mut fx,
            chain: &mut chain,
        };
        play_kit(
            swap.entity,
            spell_id,
            &kit,
            KitPlay::DISCRETE,
            play_seq.next(),
            &visuals.0,
            &mut out,
        );
    }
}
