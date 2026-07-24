//! Spell-visual kit sounds (decision 0107): route [`SpellKitSound`] — the kit's own
//! `SoundEntries.dbc` id (`SpellVisualKit` field 13), resolved by the cast-edge router
//! (`crate::creature_anim::spell_visual`) — to audio at the casting unit, mirroring the client's
//! looping-test split (`0x458830`): a plain kit rings as a fire-and-forget positioned one-shot
//! (`0x458870`); a **LOOPING** kit (Fireball's precast buildup 702, Arcane Missiles' channel hum
//! 3136) becomes a channel **tracked to the caster** (`0x61fec0`) and is reaped by
//! [`SpellKitSound::StopHold`] when the hold ends (the client kills the effect's sound at
//! `0x614150`) — without the reap, a `/castvis 133` buildup loops forever past its own release.
//! Unit despawn is covered by the greeting module's source-channel reaper.

use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use crate::assets::WorldAssets;
use crate::creature_anim::SpellKitSound;
use crate::net::NetEntity;
use crate::schedule::WorldStage;

use super::kit::{kit_looping, play_kit, play_kit_ext, KitRef, SoundCategory, SoundKits};
use super::{AudioListener, SoundConfig, SoundOutput};

#[allow(clippy::too_many_arguments)] // the standard sound-route param set + the hold ledger
fn route_spell_kit_sounds(
    mut events: MessageReader<SpellKitSound>,
    transforms: Query<&Transform, Without<Camera3d>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
    // Each unit's live tracked hold-loop kit, so StopHold reaps exactly that kit's channels and
    // never a sibling tagged channel (the caster's greeting line keeps talking).
    mut hold_loops: Local<EntityHashMap<u32>>,
    mut despawned: RemovedComponents<NetEntity>,
) {
    for entity in despawned.read() {
        hold_loops.remove(&entity); // the channel itself dies via the greeting despawn reaper
    }
    if events.is_empty() {
        return;
    }
    let (Some(mut kits), Some(assets)) = (kits, assets) else {
        return;
    };
    let listener = listener.pos;
    for ev in events.read() {
        match *ev {
            SpellKitSound::Play { entity, kit_sound } => {
                let pos = transforms.get(entity).map(|t| t.translation).ok();
                let looping = kit_looping(&kits, kit_sound);
                // The kit player is otherwise invisible in logs — this line is what a headless
                // probe greps to prove a spell's sound actually fired (0451 mount-up probe).
                debug!("spell kit sound {kit_sound} on {entity:?} (looping {looping})");
                let played = if looping {
                    play_kit_ext(
                        &mut kits,
                        &assets,
                        &mut out,
                        &config,
                        listener,
                        KitRef::Id(kit_sound),
                        pos,
                        SoundCategory::Sfx,
                        None,
                        Some(entity),
                        false,
                    )
                } else {
                    play_kit(
                        &mut kits,
                        &assets,
                        &mut out,
                        &config,
                        listener,
                        KitRef::Id(kit_sound),
                        pos,
                        SoundCategory::Sfx,
                    )
                };
                match played {
                    Ok(()) if looping => {
                        hold_loops.insert(entity, kit_sound);
                    }
                    Ok(()) => {}
                    Err(e) => warn!("spell kit sound {kit_sound}: {e:#}"),
                }
            }
            SpellKitSound::StopHold { entity } => {
                if let Some(kit_sound) = hold_loops.remove(&entity) {
                    super::kit::stop_source_kit(&mut out, entity, kit_sound);
                }
            }
        }
    }
}

/// Registration hook for [`super::SoundPlugin`].
pub(super) fn plugin(app: &mut App) {
    app.add_systems(Update, route_spell_kit_sounds.in_set(WorldStage::Present));
}
