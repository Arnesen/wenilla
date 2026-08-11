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

use crate::net::{NetEntity, ObjectStore};
use benilla_assets::{AssetSet, LockRecover, WorldAssets};
use benilla_world::schedule::WorldStage;

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

/// The GO display-slot an M2 animation-event tag addresses — the reference's GO event dispatcher
/// `0x5f3e20` (wow-re `go-display-sound-events.md`, byte-verified; the 1086 fold-back): `$GO0..5`
/// → `Sound[0..5]`, `$GC0..3` → the Custom slots `Sound[6..9]`. Every other tag is not this
/// channel's (`$SND`/`$DSO`/`$DSL` carry a literal kit id and ride the generic
/// [`crate::sound::anim_events`] arms; `$SHK` is camera shake, no audio).
fn go_event_slot(ident: &[u8; 4]) -> Option<usize> {
    match ident {
        [b'$', b'G', b'O', d @ b'0'..=b'5'] => Some((d - b'0') as usize),
        [b'$', b'G', b'C', d @ b'0'..=b'3'] => Some(6 + (d - b'0') as usize),
        _ => None,
    }
}

/// Play the display-slot kits a GameObject's animation events name — the audio half of the GO
/// M2 event kernel ([`crate::go_anim`]'s scanner feeds the stream; wow-re
/// `go-display-sound-events.md`: `0x5f4010` is the binary's ONLY reader of the display row's
/// sound columns, reached solely from the anim-event dispatcher). The load-bearing tenant is the
/// fishing bobber's bite: Custom0's `$GC0` at t≈3.87 s → display 668 `Sound6` = kit 3355
/// "Fishing Hooked" — fired **once per 0xB3** (the completion retire re-arms Stand before a
/// second pass, decision 1100), beside the server's explicit `SMSG_PLAY_OBJECT_SOUND(3355)` ~200
/// ms earlier — the client issues exactly two starts of the kit, then silence (no same-kit dedup
/// exists anywhere in the reference's play chain).
#[allow(clippy::too_many_arguments)]
fn go_event_sounds(
    mut events: MessageReader<crate::creature_anim::AnimSoundEvent>,
    gos: Query<(&NetEntity, &Transform)>,
    go_sounds: Option<Res<GoSounds>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    if events.is_empty() {
        return;
    }
    let (Some(go_sounds), Some(mut kits), Some(assets)) = (go_sounds, kits, assets) else {
        return;
    };
    let listener = listener.pos;
    for ev in events.read() {
        let Some(slot) = go_event_slot(&ev.ident) else {
            continue;
        };
        // A creature clip authoring a `$GO*`/`$GC*` tag doesn't resolve here: the query wants a
        // GameObject's display row, and only GO entities carry one in the display-slot table.
        let Ok((net, transform)) = gos.get(ev.entity) else {
            continue;
        };
        if net.kind != EntityKind::GameObject {
            continue;
        }
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
            warn!("GO event sound (slot {slot}, kit {kit}): {e:#}");
        }
    }
}

/// Registration hook for [`super::SoundPlugin`].
pub(super) fn plugin(app: &mut App) {
    app.add_systems(Startup, load_go_sounds.after(AssetSet::Open))
        // After the Net drain wrote this frame's descriptor deltas (the state watcher), and the
        // event consumer after the GO scanner's write in the same frame's Present set.
        .add_systems(
            Update,
            (go_state_sounds, go_event_sounds).in_set(WorldStage::Present),
        );
}

#[cfg(test)]
mod tests {
    use super::go_event_slot;

    /// The dispatcher's slot table (wow-re `go-display-sound-events.md`): `$GO0..5` are the
    /// first six display slots, `$GC0..3` the four Custom slots 6..9 — the bobber's splash is
    /// `$GC0` → slot 6. Out-of-range digits and other families are not this channel.
    #[test]
    fn event_tags_map_to_display_slots() {
        assert_eq!(go_event_slot(b"$GO0"), Some(0));
        assert_eq!(go_event_slot(b"$GO5"), Some(5));
        assert_eq!(go_event_slot(b"$GC0"), Some(6)); // the bobber splash
        assert_eq!(go_event_slot(b"$GC3"), Some(9));
        assert_eq!(go_event_slot(b"$GO6"), None);
        assert_eq!(go_event_slot(b"$GC4"), None);
        assert_eq!(go_event_slot(b"$SND"), None);
        assert_eq!(go_event_slot(b"$FSD"), None);
    }
}
