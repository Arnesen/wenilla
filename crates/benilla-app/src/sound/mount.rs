//! The dismount sound — the ONE genuine mount-transition sound the client plays (decision 0441
//! fold-back; byte-verified wow-re `mount-composition.md` Q4): the dismount handler `0x607ce0`
//! unconditionally tail-plays a FIXED global SoundEntries kit, resolved once at startup by
//! name-match — `"SpiritWolf_DONOTRENAME"` (`0x623110` → `0x8627bc`) — positioned at the
//! dismounting unit. It is NOT CreatureSoundData: no mount/dismount column exists, and the
//! attach path `0x607a00` plays nothing at all — mount-UP is silent (any summon whoosh rides
//! the summoning spell's own visual kit, spell-node, untraced). Fires on the live
//! mounted→unmounted transition of any visible unit; first sight of an unmounted unit records
//! silently, and a remount (id→id′) is not a dismount.

use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use benilla_protocol::EntityKind;

use crate::assets::WorldAssets;
use crate::net::{NetEntity, ObjectStore};
use crate::schedule::WorldStage;

use super::kit::{play_kit, KitRef, SoundCategory, SoundKits};
use super::{AudioListener, SoundConfig, SoundOutput};

/// The client's fixed dismount kit, by SoundEntries name (the reference resolves it once at
/// startup by name-match — the kit predates mounts as a spirit-wolf sound, hence the odd name;
/// the mechanism is byte-verified, its in-game character is the director's to judge on a live
/// dismount). This literal is the 5875 DBC's exact `Name` column value (one row, extracted
/// through the patch chain this session); the wow-re report transcribed the client's constant
/// as `SpiritWolf_DONOTRENAME` — a transcription-level difference, the row is unambiguous.
const DISMOUNT_KIT: &str = "SpiritWolf (DONOTRENAME)";

/// Play the fixed dismount kit on a live mounted→unmounted transition of any streamed unit.
fn dismount_sounds(
    changed: Query<(Entity, &NetEntity, &ObjectStore, &Transform), Changed<ObjectStore>>,
    mut known_mount: Local<EntityHashMap<u32>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    let (Some(mut kits), Some(assets)) = (kits, assets) else {
        return;
    };
    let listener = listener.pos;
    for (entity, net, store, transform) in &changed {
        if !matches!(net.kind, EntityKind::Unit | EntityKind::Player) {
            continue;
        }
        let mount = store.0.unit_mount_display_id();
        let was = known_mount.insert(entity, mount);
        // Only the live mounted→unmounted edge sounds — streaming in unmounted is not a dismount.
        if !(was.is_some_and(|w| w != 0) && mount == 0) {
            continue;
        }
        debug!(
            "dismount kit on {entity:?} (was mount {})",
            was.unwrap_or(0)
        );
        if let Err(e) = play_kit(
            &mut kits,
            &assets,
            &mut out,
            &config,
            listener,
            KitRef::Name(DISMOUNT_KIT),
            Some(transform.translation),
            SoundCategory::Sfx,
        ) {
            warn!("dismount kit ({DISMOUNT_KIT}): {e:#}");
        }
    }
}

/// Registration hook for [`super::SoundPlugin`].
pub(super) fn plugin(app: &mut App) {
    app.add_systems(Update, dismount_sounds.in_set(WorldStage::Present));
}
