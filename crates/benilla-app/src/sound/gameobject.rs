//! GameObject state sounds — doors/chests/buttons play their display's kit on state transitions
//! (decision 0070 slice 2). The data was always on the wire and in the DBC; this wires the two:
//! `GAMEOBJECT_STATE` transitions (0 active/open · 1 ready/closed · 2 active-alt, vmangos
//! `GOState`) → `GameObjectDisplayInfo.Sound[10]` slots — a `→0` transition plays **Open**
//! (slot 1), a `→1` transition plays **Close** (slot 3), positioned at the object. First sight
//! of an object records its state silently (a door streamed in already open must not bang).
//!
//! Loop/Destroy/Opened/Custom slots (2/4/5/6..9) wait for their triggers (destruction anims,
//! `$GO*` M2 events — slice 3); the survey's slot table is wowdev vanilla-family, flagged there.

use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use benilla_formats::{go_sound_slot, GameObjectSounds};
use benilla_protocol::EntityKind;

use crate::assets::{AssetSet, LockRecover, WorldAssets};
use crate::net::{NetEntity, ObjectStore};
use crate::schedule::WorldStage;

use super::kit::{play_kit, KitRef, SoundCategory, SoundKits};
use super::{AudioListener, SoundConfig, SoundOutput};

/// The display→sound-slots table (only displays with any non-zero slot; ~a third of the 1638).
#[derive(Resource)]
struct GoSounds(GameObjectSounds);

fn load_go_sounds(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_gameobject_sounds(&mut chain)
    };
    match loaded {
        Ok(s) => {
            info!("sound: {} GameObject displays with sound slots", s.len());
            commands.insert_resource(GoSounds(s));
        }
        Err(e) => warn!("sound: GameObject sound slots failed to load: {e:#}"),
    }
}

/// Watch streamed GameObjects' descriptor stores for `GAMEOBJECT_STATE` transitions and play the
/// matching slot kit at the object. `known` carries each entity's last-seen state; the map only
/// grows with distinct streamed GOs in a session and despawned entity ids are never re-observed
/// (Bevy generational ids), so stale entries are inert.
#[allow(clippy::too_many_arguments)]
fn go_state_sounds(
    changed: Query<(Entity, &NetEntity, &ObjectStore, &Transform), Changed<ObjectStore>>,
    mut known: Local<EntityHashMap<u32>>,
    go_sounds: Option<Res<GoSounds>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    let (Some(go_sounds), Some(mut kits), Some(assets)) = (go_sounds, kits, assets) else {
        return;
    };
    let listener = listener.pos;
    for (entity, net, store, transform) in &changed {
        if net.kind != EntityKind::GameObject {
            continue;
        }
        let Some(state) = store.0.gameobject_state() else {
            continue;
        };
        let prev = known.insert(entity, state);
        let Some(prev) = prev else {
            continue; // first sight: record silently
        };
        if prev == state {
            continue;
        }
        let slot = match state {
            0 => go_sound_slot::OPEN,
            1 => go_sound_slot::CLOSE,
            _ => continue, // active-alt: no recorded slot mapping
        };
        let kit = net
            .display_id
            .and_then(|d| go_sounds.0.slots(d))
            .map(|s| s[slot])
            .unwrap_or(0);
        if kit == 0 {
            continue;
        }
        if let Err(e) = play_kit(
            &mut kits,
            &assets,
            &mut out,
            &config,
            listener,
            KitRef::Id(kit),
            Some(transform.translation),
            SoundCategory::Sfx,
        ) {
            warn!("GO state sound (kit {kit}): {e:#}");
        }
    }
}

/// Registration hook for [`super::SoundPlugin`].
pub(super) fn plugin(app: &mut App) {
    app.add_systems(Startup, load_go_sounds.after(AssetSet::Open))
        // After the Net drain wrote this frame's descriptor deltas.
        .add_systems(Update, go_state_sounds.in_set(WorldStage::Present));
}
