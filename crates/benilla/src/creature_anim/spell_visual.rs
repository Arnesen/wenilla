//! The spell-visual **data plane + cast-edge router** (decision 0099 phase 2; schemas and stage
//! wiring pinned decision 0107): `SpellVisual.dbc` × `SpellVisualKit.dbc` loaded once at startup
//! exactly like [`super::AnimData`], and [`route_cast_visuals`] — the one place spell ids resolve
//! to animations/sounds. The wire side ([`super::CastEvent`], written by the net bridge) and the
//! render side ([`super::CastHold`] + [`super::EmoteAnim`] one-shots, consumed by the driver)
//! never touch the DBC chain themselves.

#[cfg(test)]
mod tests;

use bevy::ecs::entity::{EntityHashMap, EntityHashSet};
use bevy::prelude::*;

use benilla_formats::{SpellVisualCatalog, VisualKit, VisualStages, MISSILE_ATTACH_TABLE};
use benilla_protocol::EntityKind;

use crate::assets::{LockRecover, WorldAssets};
use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{NetCommands, NetEntity, ObjectStore};

use super::{CastEvent, CastEventKind, CastHold, EmoteAnim, SpellGoTargets, WoundAnim};

/// The client's literal missile-model fallback when the visual chain resolves to nothing
/// (`0x860c9c` — a checkerboard cube, shipped in the real MPQs; faithful, not a joke).
const ERROR_CUBE: &str = "Spells\\ErrorCube.mdx";

/// The reserved [`SpellKitFx`] reap key for the lootable-corpse effect — NOT a spell id (no 5875
/// spell id is anywhere near `u32::MAX`; the client keys these nodes by attach tag + node flag
/// instead of a spell, wow-re `loot-corpse-effect.md`). One key suffices: a unit carries at most
/// one loot effect (the client's flag8 dedup), which [`arm_loot_fx`]'s edge cache guarantees.
const LOOT_FX_KEY: u32 = u32::MAX;

/// The M2 attachment the client hangs its engine-spawned effects from (`0x13` — the loot
/// sparkle and the level-up ding both). It is also the `spell_fx` attach cascade's own last
/// fallback, so a model lacking the point lands on the unit base — exactly the reference's
/// root fallback.
const HARDCODED_FX_ATTACH: u16 = 0x13;

/// The engine-spawned level-up effect's baked lookup name (the client's string `0x8618e0`,
/// matched by the boot name-resolve `0x61f5b0`; wow-re `levelup-ding.md` — decision 0305).
const LEVEL_UP_EFFECT: &str = "HARDCODED Unit Level Up";

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
/// The one place the client's **ranged weapon-visual fallback** applies (`0x60d450`, bytes
/// `60d465`–`60d4aa`; wow-re `throw-ranged-attack-anim.md`): a RANGED-attribute spell
/// (`Attributes & 0x2`) whose own visual resolves to nothing borrows `weapon_visual` — the
/// caster's equipped ranged weapon's substitute visual. Every basic shot rides this (they all
/// have `SpellVisual1 = 0`): Throw → ReadyThrown/AttackThrown, Auto Shot → LoadBow/AttackBow,
/// wand Shoot → the gun kits. A spell with its own visual never takes the fallback (the client's
/// `60d4b4` test), and a non-ranged spell never pays the lookup (`FnOnce`).
fn resolve_stages<'a>(
    spells: &crate::ui_action::Spells,
    visuals: &'a SpellVisualCatalog,
    spell_id: u32,
    weapon_visual: impl FnOnce() -> Option<u32>,
) -> Option<&'a VisualStages> {
    let def = spells.catalog.get(spell_id)?;
    match visuals.stages(def.visual) {
        own @ Some(_) => own,
        None if def.ranged_slot() => visuals.stages(weapon_visual()?),
        None => None,
    }
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
    let kit_id = stage(resolve_stages(spells, visuals, spell_id, weapon_visual)?);
    (kit_id != 0)
        .then(|| visuals.kit(kit_id).copied())
        .flatten()
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
struct KitOut<'a, 'w1, 'w2, 'w3, 'w4> {
    oneshots: &'a mut MessageWriter<'w1, EmoteAnim>,
    wounds: &'a mut MessageWriter<'w2, WoundAnim>,
    sounds: &'a mut MessageWriter<'w3, SpellKitSound>,
    fx: &'a mut MessageWriter<'w4, SpellKitFx>,
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
/// - `sound`: whether the kit's sound rings. The STATE stage's sound is SELF-ONLY
///   (byte-verified: the stage-2 play's sound leg is gated on IsActivePlayer, `0x5fa6d0` —
///   wow-re `state-kit-aura-lifecycle.md`), so observers never hear another unit's buff kit.
#[derive(Clone, Copy)]
struct KitPlay {
    persistent: bool,
    effects: bool,
    sound: bool,
}

impl KitPlay {
    /// The ordinary discrete play (kit push, cast release, impact stage): self-terminating
    /// models, every leg on.
    const DISCRETE: Self = Self {
        persistent: false,
        effects: true,
        sound: true,
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
            effects,
        });
    }
}

/// The **unit-impact hand-off** (the client's `0x61dc50`, decision 0099 phase 4): the spell
/// landed on `entity` — play the impact kit (stage 1), then the state kit (stage 2). The state
/// kit plays its anim + sound only: its **effect models live for the aura's life**, owned by
/// [`arm_aura_state_fx`]'s slot watcher (a single instance, begun when the aura lands in the
/// slots and reaped when it leaves — the same packet burst as the GO, so there is no visible
/// gap). Named approximation: the client's small stage-2 gate `0x61dc20` (37 B, content
/// unpinned) is not modeled. `weapon_visual` = the CASTER's ranged fallback visual (already
/// resolved — the caster may be gone by a missile arrival), through which a basic shot's impact
/// kit resolves. `is_self` gates the state stage's sound (self-only, see [`play_kit`]).
#[allow(clippy::too_many_arguments)] // one hand-off's full input set
fn play_impact(
    entity: Entity,
    spell_id: u32,
    weapon_visual: Option<u32>,
    is_self: bool,
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
                sound: is_self,
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
    mut missiles: MessageWriter<MissileSpawn>,
    mut sheaths: MessageWriter<super::SheathRequest>,
    visuals: Option<Res<SpellVisuals>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    mut weapon_src: WeaponVisualSrc,
    units: Query<(Entity, &ObjectStore, Has<crate::net::SelfPlayer>)>,
    holds: Query<&CastHold>,
    mut channel_cache: Local<EntityHashMap<u32>>,
) {
    let (Some(visuals), Some(spells)) = (visuals.as_deref(), spells.as_deref()) else {
        return; // no client data — no spell visuals (every DBC resource degrades this way)
    };
    let mut out = KitOut {
        oneshots: &mut oneshots,
        wounds: &mut wounds,
        sounds: &mut sounds,
        fx: &mut fx,
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
                if let Some(kit) = resolve_kit(
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
                    // the client's stage-4 lifetime: they live until the cast resolves.
                    let effects = resolve_kit_effects(&visuals.0, &kit);
                    if !effects.is_empty() {
                        out.fx.write(SpellKitFx::Begin {
                            entity: ev.entity,
                            spell_id: ev.spell_id,
                            persistent: true,
                            class: FxClass::Hold,
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
                if let Some(kit) = resolve_kit(
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
                    units.get(ev.entity).is_ok_and(|(.., is_self)| is_self),
                    ev.seq,
                    spells,
                    &visuals.0,
                    &mut out,
                );
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
        // The caster's ranged fallback visual, resolved once per GO (the client's `0x6e802e`
        // call into `0x60d450`) — feeds the inline impacts, the missile chain, and the flight.
        let wv = weapon_src.caster(go.caster);
        if display.speed <= 0.0 {
            for &target in &go.hits {
                play_impact(
                    target,
                    go.spell_id,
                    wv,
                    units.get(target).is_ok_and(|(.., is_self)| is_self),
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
            let stages = resolve_stages(spells, &visuals.0, go.spell_id, || wv);
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
            if !targets.is_empty() {
                // The release gate (the client's `0x6e7a70` flush condition, inverted): a cast
                // kit that plays a body animation defers the launch to its release keyframe;
                // no kit / no anim (`kit+8 < 1` ⇔ our `anim_id: None`) launches at GO.
                let awaits_release =
                    resolve_kit(spells, &visuals.0, go.spell_id, |s| s.cast, || wv)
                        .is_some_and(|k| k.anim_id.is_some());
                missiles.write(MissileSpawn {
                    caster: go.caster,
                    spell_id: go.spell_id,
                    path,
                    ammo_display_id: go.ammo_display_id,
                    dest_tag,
                    speed: display.speed,
                    targets,
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
    for (entity, store, _) in &units {
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
                // The channel kit's effect models — persistent while the field holds (the
                // client's stage-2 lifetime).
                let effects = resolve_kit_effects(&visuals.0, &kit);
                if !effects.is_empty() {
                    out.fx.write(SpellKitFx::Begin {
                        entity,
                        spell_id: cur,
                        persistent: true,
                        class: FxClass::Hold,
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
/// `0x614150(spellId, force=1)`. The ADD edge's sound is SELF-gated (`0x5fa6d0` IsActivePlayer),
/// so observers hear nothing — and its body anim is NOT code-suppressed (`0x60edf0`'s tail plays
/// `kit+0x08` unconditionally). This watcher plays effects only — correct for every state kit
/// whose anim column is the none-sentinel (food's 409 is), and a **named residual** for a state
/// kit that carries a real anim: that ADD-edge replay isn't built (no live kit demonstrates it;
/// build it from the verdict when one shows).
///
/// `armed` tracks exactly the (unit, spell) pairs this watcher began, so the REMOVE edge never
/// reaps another owner's persistent instances (a channel hold whose spell also rides an aura).
/// An aura refresh keeps its slot id present (no edge); a re-apply that flickers remove→add
/// across frames reaps then re-begins — the drain's replace-on-persistent-begin keeps even the
/// same-frame corner single-instanced.
pub(super) fn arm_aura_state_fx(
    units: Query<(Entity, &ObjectStore)>,
    visuals: Option<Res<SpellVisuals>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    mut fx: MessageWriter<SpellKitFx>,
    mut armed: Local<EntityHashMap<Vec<u32>>>,
) {
    let (Some(visuals), Some(spells)) = (visuals.as_deref(), spells.as_deref()) else {
        return; // no client data — no spell visuals (the DBC-resource degrade shape)
    };
    for (entity, store) in &units {
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
            if effects.is_empty() {
                continue;
            }
            fx.write(SpellKitFx::Begin {
                entity,
                spell_id,
                persistent: true,
                class: FxClass::AuraState,
                effects,
            });
            next.push(spell_id);
        }
        *prev = next;
    }
    // Streamed units despawn on range-out; their instances die with the entity — drop the rows.
    armed.retain(|e, _| units.contains(*e));
}

/// Arm/reap the **lootable-corpse sparkle** (wow-re `loot-corpse-effect.md`, §5 cross-checked):
/// a DEAD unit carrying `UNIT_DYNFLAG_LOOTABLE` wears the `SpellVisualEffectName` row named
/// `"HARDCODED Loot Art"` (5875: `Particles\LootFX.mdl` — a golden flare + three star-twinkle
/// emitters; cadence/size/color/blend all authored in the asset, the client sets none of them)
/// attached at `0x13`, its own first sequence looping. The real client is edge-driven off the
/// descriptor apply (watcher `0x600440`) with **no viewer/tap/distance/loot-window logic** — the
/// server already strips the flag per viewer (vmangos's "hide lootable animation for unallowed
/// players"). The falling edge (looted empty, rights lost) reaps with no fade (`0x600680`);
/// a despawn tears the instance down with the unit. Rides the spell-kit fx plumbing under
/// [`LOOT_FX_KEY`] — the client hangs loot art on the same `Effect_C` node type its spell
/// visuals use, so sharing the one attach body is the faithful shape.
pub(super) fn arm_loot_fx(
    units: Query<(Entity, &ObjectStore)>,
    visuals: Option<Res<SpellVisuals>>,
    mut fx: MessageWriter<SpellKitFx>,
    mut armed: Local<EntityHashSet>,
) {
    let Some(path) = visuals.as_ref().and_then(|v| v.0.loot_art_path()) else {
        return; // no client data / no such row — no loot art (the DBC-resource degrade shape)
    };
    for (entity, store) in &units {
        let lootable = store.0.unit_is_dead() && store.0.unit_lootable();
        if lootable && armed.insert(entity) {
            fx.write(SpellKitFx::Begin {
                entity,
                spell_id: LOOT_FX_KEY,
                persistent: true,
                class: FxClass::Hold,
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
                    effects: vec![(HARDCODED_FX_ATTACH, path.to_string())],
                });
            }
            _ => {} // first sight arms; an unchanged write is a no-op
        }
    }
    // Streamed units despawn on range-out — drop their level memory with them.
    levels.retain(|e, _| units.contains(*e));
}
