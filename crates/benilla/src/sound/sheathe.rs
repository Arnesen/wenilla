//! Draw/stow sounds — the sheath swap moment routed through `SheatheSoundLookups`.
//!
//! The trigger is [`SheathSwapMessage`] — the *ceremony's* hand-touches-weapon moment (the
//! `VisualSheath` release at the one-shot's authored swap point). Ceremony-only: snap
//! transitions (attack auto-draw, every reactive stow, remote units) are **silent**, like the
//! client's — the sound lives in the ceremony playback, and `bInstant` paths play no clip
//! (director-verified on the ref). NOT the `$SHL`/`$SHR` anim tags: those exist only on Sheath
//! (89, the back-stow ceremony) — **HipSheath (90, every 1H draw) carries no sound events at
//! all** (probe-verified on HumanMale; director-caught silence on a sword-and-board draw), so
//! the event track cannot be the client's trigger. Each wielded hand resolves its item through
//! `SheatheSoundLookups` (`(class, subclass, material)` → stow/draw kit pair — metal/wood
//! weapons, shields); empty hands are silent.
//!
//! INTERIM: item material isn't on the wire for players — wood for staves/polearms/fishing
//! poles, metal otherwise (the same heuristic as `combat`); creatures could carry the virtual
//! item's material byte through `Wielded` later.

use bevy::prelude::*;

use benilla_formats::SheatheSoundCatalog;

use crate::assets::{AssetSet, LockRecover, WorldAssets};
use crate::creature_anim::{SheathSwapMessage, Wielded};
use crate::net::NetEntity;
use crate::schedule::WorldStage;

use super::kit::{play_kit, KitRef, SoundCategory, SoundKits};
use super::{AudioListener, SoundConfig, SoundOutput};

/// Wood-bodied weapon subclasses (staff/polearm/fishing pole) — the material heuristic.
const WOODEN: [u32; 3] = [6, 10, 20];

#[derive(Resource)]
struct SheatheSounds(SheatheSoundCatalog);

fn load_sheathe_sounds(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_sheathe_sound_catalog(&mut chain)
    };
    match loaded {
        Ok(cat) => {
            info!("sound: {} sheathe sound rows", cat.len());
            commands.insert_resource(SheatheSounds(cat));
        }
        Err(e) => warn!("sound: sheathe sounds failed to load: {e:#}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn sheathe_sounds(
    mut swaps: MessageReader<SheathSwapMessage>,
    units: Query<(&Transform, &Wielded), With<NetEntity>>,
    sounds: Option<Res<SheatheSounds>>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    if swaps.is_empty() {
        return;
    }
    let (Some(sounds), Some(mut kits), Some(assets)) = (sounds, kits, assets) else {
        return;
    };
    let listener = listener.pos;
    for swap in swaps.read() {
        let Ok((transform, wielded)) = units.get(swap.entity) else {
            continue;
        };
        // Each wielded hand rings its own item (a sword-and-board draw = ring + shield thunk).
        for hand in [wielded.main, wielded.off] {
            let Some((class, subclass)) = hand else {
                continue; // empty hand — nothing to ring
            };
            let (class, subclass) = (u32::from(class), u32::from(subclass));
            let material = if class == 2 && WOODEN.contains(&subclass) {
                2
            } else if class == 2 {
                1
            } else {
                0
            };
            let Some(pair) = sounds.0.get(class, subclass, material) else {
                continue;
            };
            let kit = if swap.drawing {
                pair.unsheathe
            } else {
                pair.sheathe
            };
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
                warn!("sheathe (kit {kit}): {e:#}");
            }
        }
    }
}

/// Registration hook for [`super::SoundPlugin`].
pub(super) fn plugin(app: &mut App) {
    app.add_systems(Startup, load_sheathe_sounds.after(AssetSet::Open))
        .add_systems(Update, sheathe_sounds.in_set(WorldStage::Present));
}
