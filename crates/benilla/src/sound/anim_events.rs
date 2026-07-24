//! Route M2 animation event tags (`crate::creature_anim::AnimSoundEvent`) to audio — the
//! anim-driven trigger surface (decision 0070 slice 3).
//!
//! Routed here: `$SND`/`$DSO` (one-shot kit `data` at the model), `$DSL` (doodad loop kit —
//! played as a one-shot for now; looping-emitter lifecycle comes with doodad anim support),
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

use crate::assets::WorldAssets;
use crate::creature_anim::{held_strike_sound, AnimSoundEvent, CastHold, SpellVisuals};
use crate::net::ObjectStore;
use crate::schedule::WorldStage;

use super::emote::EmoteSounds;
use super::kit::{play_kit, KitRef, SoundCategory, SoundKits};
use super::{AudioListener, SoundConfig, SoundOutput};

#[allow(clippy::too_many_arguments)] // the standard sound-route param set + the two resolvers
fn route_anim_events(
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
) {
    if events.is_empty() {
        return;
    }
    let (Some(mut kits), Some(assets)) = (kits, assets) else {
        return;
    };
    let listener = listener.pos;
    let ring = |kits: &mut SoundKits, out: &mut SoundOutput, kit: u32, entity: Entity| {
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
            warn!("anim event kit {kit}: {e:#}");
        }
    };
    for ev in events.read() {
        match &ev.ident {
            b"$SND" | b"$DSO" | b"$DSL" | b"$CSD" if ev.data != 0 => {
                ring(&mut kits, &mut out, ev.data, ev.entity);
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
                    ring(&mut kits, &mut out, kit, ev.entity);
                }
            }
            b"$TRD" => {
                let (Some(spells), Some(visuals)) = (spells.as_deref(), visuals.as_deref()) else {
                    continue;
                };
                let hold = units.get(ev.entity).ok().and_then(|(_, h)| h);
                let kit = hold.and_then(|h| held_strike_sound(spells, &visuals.0, h.spell_id));
                if let Some(kit) = kit {
                    ring(&mut kits, &mut out, kit, ev.entity);
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
