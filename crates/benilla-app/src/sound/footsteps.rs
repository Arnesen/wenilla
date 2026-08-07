//! Footsteps (decision 0070 slice 3): the `$FSD` anim tag resolved through the terrain-type
//! chain — `ground effect under the unit → TerrainType → × the unit's footstep class →
//! FootstepTerrainLookup → SoundEntries`.
//!
//! **Trigger: `$FSD` and nothing else** (decision 1080). A footfall is two disjoint channels in
//! the client's event dispatcher `0x5ffbd0` — `$FSD → 0x623390` is the *sound*, the per-foot side
//! tags (`$FL/$FR/$RL/$RR/$SL/$SR/$BL/$BR/$WL/$WR`) `→ 0x5fbf70` are the *visual* footfall (the
//! decal + spray, [`crate::footprints`]) and play nothing (wow-re `footprint-decals.md` §1, §5
//! byte-arbitrated). Reading both as steps rang **every gait at double rate** — HumanMale's Walk
//! keys `$FR0 · $FSD · $FL0 · $FSD`, a horse's gallop four of each per 0.8 s cycle — and made the
//! turn-in-place shuffle, whose *only* keys are `$SL0 $SR0` at `t = 0.000`, clatter where the
//! reference is silent ([`crate::creature_anim::is_footstep_sound`]).
//!
//! The unit's class comes from its voice row (`CreatureSoundData.FootstepID`) through the
//! generic display→sound chain incl. the model fallback (`benilla_formats::creature_sound` —
//! characters reach class 7 as *data*). **Class 0, or no row at all, = no footstep sounds**:
//! the client's `$FSD` handler bails on a zero class before any lookup (`0x6233ec`, wow-re
//! `benilla-pins.md` B11, byte-confirmed); the lookup's class-0 rows are the Ancient
//! Protector's stomps (kit 661), reached only by that model's own nonzero class.
//!
//! Ahead of the class, `0x623390` opens on the same three state gates the visual handler does,
//! read on the ROOT unit (the rider of a mounted composite): **hover** (`0x6233aa`, move flag
//! `0x4000_0000`), **stealth/CREEP** (`0x62339c`, `BYTES_1` byte 3 bit `0x2` — not death) and
//! **player ghost** (`0x6233d3`, `PLAYER_FLAGS & 0x10`) — a stealthed unit is silent-footed.
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
use crate::creature_anim::{is_footstep_sound, move_flags, AnimSoundEvent, MovementState};
use crate::entities::CollisionHeight;
use crate::liquid::{unit_claim, water_surface_at, WaterChunkInfo};
use crate::net::{NetEntity, ObjectStore};
use crate::player::swim_enter_depth;
use crate::schedule::WorldStage;
use crate::terrain_stream::{ground_effect_under, TerrainStreamer};
use crate::wmo_portal::UnitWmoRoom;

use super::creature::CreatureVoices;
use super::kit::{play_kit, KitRef, SoundCategory, SoundKits};
use super::{AudioListener, SoundConfig, SoundOutput};

/// The loaded terrain-chain catalog. `pub(crate)`: [`crate::footprints`] reads the same chain's
/// `TerrainType.Flags` gate (`leaves_footprints`) — one load, two footfall consumers.
#[derive(Resource)]
pub(crate) struct Footsteps(pub(crate) FootstepCatalog);

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
    // The handler's state gates read the ROOT unit — a mount child's `$FSD` is the rider's
    // footfall, and it is the RIDER's stealth/hover/ghost the client tests (`0x623390`'s `this`
    // is the unit the mount model's event stream is registered against, wow-re
    // `footprint-decals.md` §1 "Mounted"). Same walk-to-root the footprint spawner does.
    parents: Query<&ChildOf>,
    root_state: Query<(Option<&ObjectStore>, Option<&MovementState>)>,
    footsteps: Option<Res<Footsteps>>,
    voices: Option<Res<CreatureVoices>>,
    streamer: Res<TerrainStreamer>,
    adt_tiles: Res<Assets<AdtTile>>,
    water: Query<&WaterChunkInfo>,
    placements: crate::liquid::RoomPlacements,
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
        if !is_footstep_sound(&ev.ident) {
            continue;
        }
        let Ok((net, transform, collision, room)) = units.get(ev.entity) else {
            continue;
        };
        // The three state gates, on the root unit (module docs): hover · stealth · player ghost.
        let mut root = ev.entity;
        while let Ok(child_of) = parents.get(root) {
            root = child_of.parent();
        }
        let Ok((store, movement)) = root_state.get(root) else {
            continue;
        };
        if movement.is_some_and(|m| m.flags & move_flags::HOVER != 0) {
            continue;
        }
        if store.is_some_and(|s| s.0.unit_is_stealthed() || s.0.player_is_ghost()) {
            continue;
        }
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
        let depth = water_surface_at(water.iter(), wow, unit_claim(room, &placements))
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
