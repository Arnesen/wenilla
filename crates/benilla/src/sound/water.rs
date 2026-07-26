//! Water audio (decision 0070 slice 4): the water splash. (The wading **footstep** splash
//! lives in [`super::footsteps`]; the submerged ambience swap lives in [`super::zone`] — the
//! client swaps the one background-ambience slot to the underwater loop, B6.)
//!
//! **The splash trigger is a symmetric depth edge** — VERIFIED (the swim §5's TU-D, wow-re
//! `swim-mechanism.md`, resolving benilla-pins B7/B8's trigger half): a dedicated detector inside
//! the per-frame water decision `0x6030c0` (`0x60314a`) compares `depth = surface − feet` against
//! **`0.4·collisionHeight`** and fires a positioned unit sound on crossing in **either direction**
//! — entering fires it, and so does surfacing back past the line (exit is NOT silent). It is
//! *distinct from* the swim boundary (`0.75·h`): the splash fires waist-deep, before the swim
//! mode starts. Walking the shallows below the line makes only per-step wading splashes
//! (director-verified against the ref, 2026-07-03).
//!
//! What stays INTERIM (same dispatch, still open): the **size class** — the client routes the
//! play through the unit's own sound emitter (`[CGUnit+0xb18]`) where Small/Medium/Large
//! resolves; that selector is not pinned, so kit 1096 Medium plays for everyone. The per-unit
//! **collision height** the line scales by is no longer interim: 0464's named
//! `CreatureModelData.collisionHeight` plumb landed in decision 0645, and this reads it off the
//! unit ([`crate::entities::CollisionHeight`]) instead of scaling the player capsule.

use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use benilla_assets::coords::bevy_to_wow;

use crate::assets::WorldAssets;
use crate::entities::CollisionHeight;
use crate::liquid::{water_surface_at, WaterChunkInfo};
use crate::net::NetEntity;
use crate::schedule::WorldStage;

use super::kit::{play_kit_ext, source_kit_playing, KitRef, SoundCategory, SoundKits};
use super::{AudioListener, SoundConfig, SoundOutput};

/// SoundEntries 1096 `CharacterSplashSoundMedium` (byte-verified in the type-21 census).
const SPLASH_KIT: u32 = 1096;

/// The splash line as a fraction of the unit's collision height — **VERIFIED** `0.4`
/// (`0x60314a`'s compare inside `0x6030c0`). Applied to the feet-referenced depth.
const SPLASH_DEPTH_FRAC: f32 = 0.4;

/// Play the water splash on a unit's `0.4·h` depth-line crossing, either direction (module docs).
#[allow(clippy::too_many_arguments)] // the sound-play plumbing, one param per concern
fn water_splashes(
    units: Query<(Entity, &Transform, Option<&CollisionHeight>), With<NetEntity>>,
    water: Query<&WaterChunkInfo>,
    mut wet: Local<EntityHashMap<bool>>,
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
    for (entity, transform, collision) in &units {
        let wow = bevy_to_wow(transform.translation);
        // The unit's own collision height (0645). `None` only on a unit's very first frame, before
        // the stamp runs — the ctor default covers it, as it does in the reference.
        let h = collision.copied().unwrap_or_default().0;
        // `None` — every unit, not just the local player; see `liquid_at`'s named gap.
        let submerged = water_surface_at(water.iter(), wow, None)
            .is_some_and(|s| s - wow[2] > SPLASH_DEPTH_FRAC * h);
        let was = wet.insert(entity, submerged);
        // The symmetric edge: any crossing splashes — wading in past the line AND surfacing back
        // out of it (`was == None`, a unit first seen, arms silently). One splash at a time PER
        // UNIT: the play is source-tagged and a crossing while the unit's previous splash still
        // sounds is dropped — a spam-jump's up-then-down double crossing fires once, like the
        // ref (director-reported, 2026-07-18: "entry and immediate exit" doubled vs the ref's
        // one; decision 0518). INTERIM reading: the client routes the splash through the unit's own sound
        // emitter (`[CGUnit+0xb18]`, the module's standing unpinned selector) — a busy emitter
        // as the natural suppressor is our model of it; slow wade-in/wade-out (both splash,
        // director-verified 2026-07-03) is preserved, the first splash long finished.
        if was.is_some_and(|w| w != submerged) && !source_kit_playing(&out, entity, SPLASH_KIT) {
            if let Err(e) = play_kit_ext(
                &mut kits,
                &assets,
                &mut out,
                &config,
                listener,
                KitRef::Id(SPLASH_KIT),
                Some(transform.translation),
                SoundCategory::Sfx,
                None,
                Some(entity),
                false,
            ) {
                warn!("water splash: {e:#}");
            }
        }
    }
}

/// Registration hook for [`super::SoundPlugin`].
pub(super) fn plugin(app: &mut App) {
    app.add_systems(Update, water_splashes.in_set(WorldStage::Present));
}
