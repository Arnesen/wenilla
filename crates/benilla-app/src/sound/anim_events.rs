//! Route M2 animation event tags (`crate::creature_anim::AnimSoundEvent`) to audio — the
//! anim-driven trigger surface (decision 0070 slice 3).
//!
//! Routed here: `$SND`/`$DSO` (one-shot kit `data` at the model), `$DSL`/`$DSE` (a placed
//! doodad's ambient loop — **registered into, and released from, the emitter pool**
//! [`super::doodad_pool`], which owns everything about how it sounds),
//! `$CSD` — the **character emote clips' embedded voice** (HumanMale EmoteLaugh 70 carries
//! `$CSD 6923` = the SoundEntries kit literally named `HumanMaleEmoteLaugh`; Cry 77 → 6921,
//! Chicken 78 → 6919, Applaud 80 → 4× `ClapSounds` 6576 — probe-verified on the real 5875 M2 +
//! SoundEntries; the client's `$CSD` handler `0x623c10` → `0x459230` plays the event payload as
//! a literal SoundEntries id, byte-confirming the routing — wow-re
//! `sound/scratch/gather-sound-anim-events.md`) — and the **gathering/work pair** (decision
//! 0562, same wow-re note):
//!
//! - **`$TRD`** (`0x62faa0`): the in-flight spell's `SpellVisual` **field-14 strike sound**,
//!   positioned — **the mining pick clang** (visual 93 → 1143 "Mining Impact") and the crafting
//!   hammer (the smithing visuals carry the same field), fired at the work anims' 0.666 s
//!   impact keyframe. Fully client-side: the in-flight spell is the unit's cast hold (the
//!   client caches it from the local GO interaction, `0x6ec220` → `[CGUnit+0xc8c]`), so no
//!   server state is involved.
//! - **`$ESD`** (`0x6239f0`): the unit's `UNIT_NPC_EMOTESTATE` → `Emotes.dbc` `EventSoundID`,
//!   gated on `EmoteSpecProc == 2`, positioned at the unit — wire-driven work-state sounds
//!   (a chopwood camp worker's state 234 → 3202; state 233 carries a second mining kit 3782,
//!   which no vmangos path ever sets for a player — verified at its source).
//!
//! `$CST`/`$CSL`/`$CSR` are deliberately NOT routed: the client's handler (`0x60c940`) only
//! 3D-**repositions** the already-playing cast sound handle — it contains no play call — and
//! benilla's kit player already tracks looping cast sounds to their caster
//! (`sound/spell.rs`), so the reposition role is covered.
//!
//! The footstep family and the CreatureSoundData-driven tags (`$FD*` fidgets, `$AH*` attacks,
//! `$CSS` swings) are routed by their own consumers as those land (slice-3 tasks); unrecognized
//! tags are trace-logged so the stream is observable without spam.

use bevy::prelude::*;

use crate::creature_anim::{held_strike_sound, AnimSoundEvent, CastHold, SpellVisuals};
use crate::net::ObjectStore;
use benilla_assets::WorldAssets;
use benilla_world::schedule::WorldStage;

use super::doodad_pool::DoodadEmitterPool;
use super::emote::EmoteSounds;
use super::kit::{play_kit, KitRef, SoundCategory, SoundKits};
use super::{AudioListener, SoundConfig, SoundOutput};

#[allow(clippy::too_many_arguments)] // the standard sound-route param set + the two resolvers
pub(super) fn route_anim_events(
    mut events: MessageReader<AnimSoundEvent>,
    // GlobalTransform: `$SND` tags can fire from parented visuals (a mount child's model),
    // whose local Transform is not a world position (0441 fold-back).
    transforms: Query<&GlobalTransform, Without<Camera3d>>,
    units: Query<(Option<&ObjectStore>, Option<&CastHold>)>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    emotes: Option<Res<EmoteSounds>>,
    spells: Option<Res<crate::ui_action::Spells>>,
    visuals: Option<Res<SpellVisuals>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
    // Kit ids already complained about. A failing kit here is a PER-EVENT failure on a stream that
    // fires at doodad rates, so warning every time is a log flood, not a diagnostic: one live run
    // past Darnassus produced 420 identical lines for `NightElfLantern01`'s `$DSL(33764)`, an id
    // that is simply not in 5875's `SoundEntries` (32401 is the corpus's only other one). The
    // reference does nothing audible for an id it cannot resolve, so this is data, not an error —
    // but it is still worth saying once, because a kit that goes missing for any OTHER reason is a
    // real bug and silence would hide it.
    mut complained: Local<std::collections::HashSet<u32>>,
    // The ambient emitter pool — `$DSL`/`$DSE`'s whole destination.
    mut pool: ResMut<DoodadEmitterPool>,
) {
    if events.is_empty() {
        return;
    }
    let (Some(mut kits), Some(assets)) = (kits, assets) else {
        return;
    };
    let listener = listener.pos;
    let ring = |kits: &mut SoundKits,
                out: &mut SoundOutput,
                kit: u32,
                entity: Entity,
                complained: &mut std::collections::HashSet<u32>| {
        let pos = transforms.get(entity).map(|t| t.translation()).ok();
        if let Err(e) = play_kit(
            kits,
            &assets,
            out,
            &config,
            listener,
            KitRef::Id(kit),
            pos,
            SoundCategory::Sfx,
        ) {
            if complained.insert(kit) {
                warn!("anim event kit {kit}: {e:#} (further reports for this kit suppressed)");
            }
        }
    };
    for ev in events.read() {
        match &ev.ident {
            // `$DSL` — the DOODAD SOUND **LOOP** (`0x69521d`), VERIFIED (wow-re
            // `sound/scratch/doodad-sound-emitters.md`, §5). A persistent handle with a lifecycle,
            // one per doodad (`[CMapDoodadDef+0x168]`): crossing the marker again **repositions**
            // the existing registration (`0x462000`) and never restarts it; a DIFFERENT id
            // releases the old one (`0x461f80`) and registers the new (`0x461d80`). So there is no
            // wrap retrigger at all — which the shipped audio already implied, since
            // `NightElfStreetLampLoop` is 4.000 s on a 3.333 s sequence and `CampFireSmallLoop` is
            // 2.967 s on the same, mismatched in both directions.
            //
            // **It always loops, and consults NO flag.** Looping is an entry-point constant on the
            // reference's registration pool: `0x7a54d0` builds mode `0x1002` (`HW3D|LOOP_NORMAL`)
            // and calls `SetLoopCount(stream, -1)`, against `0x7a5490`'s `0x1000` for the one-shot
            // path. This corrects the interim that shipped with the first half of B345: the kit's
            // 0x200 bit has exactly ONE reader image-wide (`0x458840`), whose two callers are the
            // GameObject display-slot lane select and the spell-visual lane — it reaches neither
            // mode word and is a LANE SELECT, not a loop flag. (0x400, which correlated perfectly
            // with the four non-sustaining `$DSL` kits in the shipped data, is `random pitch`; the
            // correlation was authoring practice — you do not detune a sustained hum.) `force_loop`
            // here is therefore the faithful shape, not a workaround: 25 of the 60 kits a `$DSL`
            // names omit 0x200 and every one of them loops in the real client.
            // `$DSL` — the DOODAD SOUND **LOOP** (`0x69521d`), VERIFIED (wow-re
            // `sound/scratch/doodad-sound-emitters.md`). It does not start a sound. It
            // **registers this doodad's position** as one emitter of its SoundEntries id in the
            // pool at `0xb06dd8` (`0x461d80`), and re-crossing the marker only *repositions* that
            // registration (`0x462000`) — which is why there is no wrap retrigger, and why
            // `NightElfStreetLampLoop` (4.000 s of sample on a 3.333 s sequence) is not chopped
            // every cycle. Whether anything is audible, from where, and how many at once are the
            // pool pump's questions, not this scanner's: see [`super::doodad_pool`].
            b"$DSL" if ev.data != 0 => {
                if let Ok(t) = transforms.get(ev.entity) {
                    super::doodad_pool::register(
                        &mut pool,
                        ev.entity,
                        ev.data,
                        t.translation(),
                        listener,
                    );
                }
            }
            // `$DSE` — the doodad sound **STOP** token (`0x45534424`), VERIFIED in the same note:
            // it releases the doodad's registration (`0x461f80`), and its `data` is 0 on all 16
            // shipped models. Without it a `$DSL` started at a keyframe never ends — which is
            // exactly the elevator and machinery family (`GnomereganElevatorLoop`, `SubwayLoop`,
            // the Undercity and Thunder Bluff lifts, the zeppelin), where the loop is authored to
            // run for one leg of the animation and stop. Releasing a record is not stopping a
            // sound: the id keeps sounding while any *other* doodad still names it.
            b"$DSE" => super::doodad_pool::release(&mut pool, ev.entity),
            b"$SND" | b"$DSO" | b"$CSD" if ev.data != 0 => {
                ring(&mut kits, &mut out, ev.data, ev.entity, &mut complained);
            }
            b"$ESD" => {
                let Some(emotes) = emotes.as_deref() else {
                    continue;
                };
                let state = units
                    .get(ev.entity)
                    .ok()
                    .and_then(|(store, _)| store)
                    .map_or(0, |s| s.0.unit_emote_state());
                if let Some(kit) = (state != 0)
                    .then(|| emotes.state_event_sound(state))
                    .flatten()
                {
                    ring(&mut kits, &mut out, kit, ev.entity, &mut complained);
                }
            }
            b"$TRD" => {
                let (Some(spells), Some(visuals)) = (spells.as_deref(), visuals.as_deref()) else {
                    continue;
                };
                let hold = units.get(ev.entity).ok().and_then(|(_, h)| h);
                let kit = hold.and_then(|h| held_strike_sound(spells, &visuals.0, h.spell_id));
                if let Some(kit) = kit {
                    ring(&mut kits, &mut out, kit, ev.entity, &mut complained);
                }
            }
            other => {
                trace!(
                    "anim event {} (data {}) — no route yet",
                    String::from_utf8_lossy(other),
                    ev.data
                );
            }
        }
    }
}

/// Registration hook for [`super::SoundPlugin`].
pub(super) fn plugin(app: &mut App) {
    app.add_systems(Update, route_anim_events.in_set(WorldStage::Present));
}
