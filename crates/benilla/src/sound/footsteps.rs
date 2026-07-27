//! Footsteps (decision 0070 slice 3): the anim footstep tags resolved through the terrain-type
//! chain — `ground effect under the unit → TerrainType → × the unit's footstep class →
//! FootstepTerrainLookup → SoundEntries`.
//!
//! Triggers: the per-foot tags (`$FL0..3`/`$FR`/`$RL`/`$RR`/`$SL`/`$SR`/`$BL`/`$BR`) and the
//! dispatch tag `$FSD` (the client's `HandleFootfallAnimEvent`; sequences use either style —
//! DireWolf's walk plants four paws with `$FL/$FR` keys, its run fires `$FSD`). Each tag = one
//! step.
//!
//! The unit's class comes from its voice row (`CreatureSoundData.FootstepID`) through the
//! generic display→sound chain incl. the model fallback (`benilla_formats::creature_sound` —
//! characters reach class 7 as *data*). **Class 0, or no row at all, = no footstep sounds**:
//! the client's `$FSD` handler bails on a zero class before any lookup (`0x6233ec`, wow-re
//! `benilla-pins.md` B11, byte-confirmed); the lookup's class-0 rows are the Ancient
//! Protector's stomps (kit 661), reached only by that model's own nonzero class.
//!
//! Wading (feet below a water surface, down to the unit's own swim boundary `0.75·h`) picks the
//! lookup's **splash** slot instead of dry (falling back to dry when the class has no splash kit);
//! deeper the unit swims and footfalls go silent. The flat 2.0-yd stand-in that boundary used to be
//! is retired now 0464's `collisionHeight` plumb has landed (decision 0645) — a murloc's footfalls
//! now go quiet in water that only reaches a human's knees. Still open: this reads a *depth* even
//! for the local player rather than its real mode flag — the named follow-up in decision 0530.

use bevy::prelude::*;

use benilla_assets::coords::bevy_to_wow;
use benilla_assets::AdtTile;
use benilla_formats::FootstepCatalog;

use crate::assets::{AssetSet, LockRecover, WorldAssets};
use crate::creature_anim::{is_footstep, AnimSoundEvent};
use crate::entities::CollisionHeight;
use crate::liquid::{unit_claim, water_surface_at, WaterChunkInfo};
use crate::net::NetEntity;
use crate::player::swim_enter_depth;
use crate::schedule::WorldStage;
use crate::terrain_stream::{ground_effect_under, TerrainStreamer};
use crate::wmo_portal::UnitWmoRoom;

use super::creature::CreatureVoices;
use super::kit::{play_kit, KitRef, SoundCategory, SoundKits};
use super::{AudioListener, SoundConfig, SoundOutput};

#[derive(Resource)]
struct Footsteps(FootstepCatalog);

fn load_footsteps(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_footstep_catalog(&mut chain)
    };
    match loaded {
        Ok(cat) => {
            info!("sound: {} footstep lookup rows", cat.len());
            commands.insert_resource(Footsteps(cat));
        }
        Err(e) => warn!("sound: footstep catalog failed to load: {e:#}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn footstep_sounds(
    mut events: MessageReader<AnimSoundEvent>,
    // GlobalTransform: a mounted unit's steps are the MOUNT model's own tags, fired by the
    // mount CHILD entity — whose local Transform is the seat-relative ~origin. World position
    // is the only correct read for both parented and top-level sources (0441 fold-back).
    units: Query<(
        &NetEntity,
        &GlobalTransform,
        Option<&CollisionHeight>,
        Option<&UnitWmoRoom>,
    )>,
    footsteps: Option<Res<Footsteps>>,
    voices: Option<Res<CreatureVoices>>,
    streamer: Res<TerrainStreamer>,
    adt_tiles: Res<Assets<AdtTile>>,
    water: Query<&WaterChunkInfo>,
    kits: Option<ResMut<SoundKits>>,
    assets: Option<Res<WorldAssets>>,
    mut out: NonSendMut<SoundOutput>,
    config: Res<SoundConfig>,
    listener: Res<AudioListener>,
) {
    if events.is_empty() {
        return;
    }
    let (Some(footsteps), Some(voices), Some(mut kits), Some(assets)) =
        (footsteps, voices, kits, assets)
    else {
        return;
    };
    let listener = listener.pos;
    for ev in events.read() {
        if !is_footstep(&ev.ident) {
            continue;
        }
        let Ok((net, transform, collision, room)) = units.get(ev.entity) else {
            continue;
        };
        // The unit's footstep class (module docs): the voice row's class verbatim; zero or no
        // row = silent (the client's class-0 gate — no code default exists, B11).
        let class = match net.display_id.and_then(|d| voices.0.for_display(d)) {
            Some(v) if v.footstep_class != 0 => v.footstep_class,
            _ => continue,
        };
        // Wading picks the splash slot; swimming (deeper than the wade ceiling) is silent.
        let wow = bevy_to_wow(transform.translation());
        // The unit's own room claim (0696) — it used to pass "no claim", so every unit walking
        // under an ADT lake picked the splash slot on dry indoor stone.
        let depth = water_surface_at(water.iter(), wow, unit_claim(room))
            .map(|s| s - wow[2])
            .filter(|d| *d > 0.0);
        let wade_max = swim_enter_depth(collision.copied().unwrap_or_default().0);
        if depth.is_some_and(|d| d > wade_max) {
            continue;
        }
        let effect = ground_effect_under(&streamer, &adt_tiles, transform.translation());
        let Some((dry, splash)) = footsteps.0.resolve(class, effect) else {
            continue; // no row for this class/terrain: silent (ethereal classes)
        };
        let kit = match depth {
            Some(_) if splash != 0 => splash,
            _ => dry,
        };
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
            Some(transform.translation()),
            SoundCategory::Sfx,
        ) {
            warn!("footstep (kit {kit}): {e:#}");
        }
    }
}

/// Registration hook for [`super::SoundPlugin`].
pub(super) fn plugin(app: &mut App) {
    app.add_systems(Startup, load_footsteps.after(AssetSet::Open))
        .add_systems(Update, footstep_sounds.in_set(WorldStage::Present));
}
