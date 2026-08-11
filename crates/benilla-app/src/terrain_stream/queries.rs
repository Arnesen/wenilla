//! What the streamed world answers about a position: the spawn-time MCSH ground-shade lookup,
//! the ground-effect/height queries the sound + clutter systems make against resident tiles, and
//! the area authority (the `AreaTable.dbc` leaf under the player's feet, decision 0070).

use benilla_assets::coords::bevy_to_wow;
use benilla_assets::AdtTile;
use benilla_formats::{mcsh_shadowed_at, world_to_tile};
use bevy::prelude::*;

use crate::player::Player;

use super::TerrainStreamer;

/// The `AreaTable.dbc` id under the player's feet — the zone/subzone, from the containing
/// resident chunk's MCNK `areaId` (decision 0070: drives zone music/ambience/reverb; later the
/// minimap zone text). `None` until the ground tile is resident (or off-terrain). Written each
/// frame by [`update_current_area`]; consumers change-detect on the inner value.
#[derive(Resource, Default, PartialEq, Eq)]
pub(crate) struct CurrentArea(pub(crate) Option<u32>);

/// Ordering handle on [`update_current_area`] — the leaf-authority write. The zone-text feed
/// (`crate::area`) orders after it (which itself orders after the interior claim), so leaf +
/// indoor bit + names always come from one coherent frame — the client's single-pass resolve.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct AreaAuthoritySet;

/// Outcome of the spawn-time MCSH ground-shade lookup.
pub(crate) enum ShadeResolve {
    /// The shade is known: `true` = the doodad's base sits on MCSH-shadowed terrain (sun ×0.5 in the
    /// lobe), `false` = lit (×2.5, or a deliberate fallback when the ground tile isn't resident).
    Ready(bool),
    /// The doodad's own ground tile is requested but hasn't decoded yet — defer the spawn one frame so a
    /// straddling tree isn't baked lit before its true tile lands.
    Pending,
}

/// Resolve a doodad's terrain ground-shade the reference way: a GLOBAL world-position → tile → chunk
/// MCSH lookup at the doodad's origin (the client's `0x69b350`), NOT a sample of whichever tile happened
/// to register the placement — the latter is load-order/timing dependent and left straddling trees wrongly
/// lit. `bevy_pos` is the doodad's origin (its `Transform.translation`).
///
/// - Ground tile resident + decoded → the true MCSH bit under the footprint.
/// - Ground tile requested but still decoding → [`ShadeResolve::Pending`] (caller defers the spawn).
/// - Ground tile not in the loaded set (a doodad straddling in from just beyond the ring) → `Ready(false)`:
///   show it lit rather than hide it, matching the reference (a null tile reads lit until it streams in).
///   (A doodad whose origin tile is *requested but never decodes* — a missing map-edge ADT — would defer
///   indefinitely; that can't arise for a real placement, whose origin always lies in an existing tile.)
pub(crate) fn doodad_ground_shade(
    streamer: &TerrainStreamer,
    adt_tiles: &Assets<AdtTile>,
    bevy_pos: Vec3,
) -> ShadeResolve {
    let wow = bevy_to_wow(bevy_pos);
    let (tx, ty) = world_to_tile(wow[0], wow[1]);
    match streamer.tiles.get(&(tx as i32, ty as i32)) {
        None => ShadeResolve::Ready(false),
        Some(ts) => match adt_tiles.get(&ts.handle) {
            None => ShadeResolve::Pending,
            Some(adt) => ShadeResolve::Ready(mcsh_shadowed_at(&adt.chunks, wow).unwrap_or(false)),
        },
    }
}

/// The MCNK `areaId` under a **Bevy-space** position on the resident terrain — the OUTDOOR leg of
/// the client's `GetAreaID 0x670250`, for any unit rather than the player.
///
/// [`CurrentArea`] is the player's authority and races a WMO interior claim ahead of this
/// ([`update_current_area`]); there is no per-unit equivalent of that claim, so a caller asking
/// about a *remote* unit gets the terrain answer alone. `None` off-terrain / mid-stream, and an
/// `areaId` of 0 (unassigned in the data) reads as a miss, same as the player's resolver.
pub(crate) fn area_id_under(
    streamer: &TerrainStreamer,
    adt_tiles: &Assets<AdtTile>,
    bevy_pos: Vec3,
) -> Option<u32> {
    let wow = bevy_to_wow(bevy_pos);
    let (tx, ty) = world_to_tile(wow[0], wow[1]);
    let ts = streamer.tiles.get(&(tx as i32, ty as i32))?;
    let adt = adt_tiles.get(&ts.handle)?;
    benilla_formats::area_id_at(&adt.chunks, wow).filter(|&id| id != 0)
}

/// The `GroundEffectTexture` id under a **Bevy-space** position on the resident terrain — the
/// footstep terrain-type source (decision 0070 slice 3; the same global position→tile→chunk
/// lookup shape as [`doodad_ground_shade`]). `None` off-terrain / mid-stream / no-effect cell —
/// the footstep resolver falls back to its Dirt default.
pub(crate) fn ground_effect_under(
    streamer: &TerrainStreamer,
    adt_tiles: &Assets<AdtTile>,
    bevy_pos: Vec3,
) -> Option<u32> {
    let wow = bevy_to_wow(bevy_pos);
    let (tx, ty) = world_to_tile(wow[0], wow[1]);
    let ts = streamer.tiles.get(&(tx as i32, ty as i32))?;
    let adt = adt_tiles.get(&ts.handle)?;
    benilla_formats::ground_effect_at(&adt.chunks, wow)
}

/// The terrain surface height (raw WoW `z`) under a **Bevy-space** position on the resident terrain —
/// the terrain leg of the client's down-ray arbitration (`FUN_006821f0`'s `0x69c320` probe, and the
/// identical one in `GetAreaID 0x670250`), which races the WMO probe and wins the column whenever the
/// ground is strictly nearer under the eye. Same global position→tile→chunk lookup shape as
/// [`ground_effect_under`].
///
/// `None` means **no terrain surface in this column**, which the race must read as "no terrain hit",
/// never as "ground at 0": off the streamed ring, mid-decode, or — the load-bearing case — the column
/// falls in an MCNK hole, the cut-out through which a mine or cave entrance reaches its WMO interior.
pub(crate) fn terrain_height_under(
    streamer: &TerrainStreamer,
    adt_tiles: &Assets<AdtTile>,
    bevy_pos: Vec3,
) -> Option<f32> {
    let wow = bevy_to_wow(bevy_pos);
    let (tx, ty) = world_to_tile(wow[0], wow[1]);
    let ts = streamer.tiles.get(&(tx as i32, ty as i32))?;
    let adt = adt_tiles.get(&ts.handle)?;
    benilla_formats::terrain_height_at(&adt.chunks, wow)
}

/// Track the `AreaTable` id under the player's feet. Faithful to the client's GetAreaID resolver
/// (wow-re 0x670250): a WMO interior takes precedence over the terrain chunk when the down-ray
/// keeps the WMO nearer — [`CurrentAreaInterior`] is exactly that player-position **faces-only**
/// down-ray (wow-re `zonetext-indoor-bit.md`; the portal-legged render seed flipped in the abbey
/// yard), so an indoor city (Ironforge, Undercity) reports its OWN area via
/// `WMOAreaTable.AreaTableID` rather than the enclosing zone's terrain. Outdoors (no interior
/// claim, or a group with no area row) it falls to the containing chunk's MCNK `areaId`. Holds
/// the previous value while the tile is still decoding, so a tile-edge crossing never flickers
/// through `None`.
pub(super) fn update_current_area(
    mut area: ResMut<CurrentArea>,
    player: Res<Player>,
    streamer: Res<TerrainStreamer>,
    adt_tiles: Res<Assets<AdtTile>>,
    interior: Res<crate::wmo_portal::CurrentAreaInterior>,
    wmo_areas: Option<Res<crate::sound::interior::WmoAreas>>,
) {
    let wow = bevy_to_wow(player.pos);
    // WMO interior first: the player's down-ray group resolved to its WMOAreaTable world area.
    let interior_area = interior.0.zip(wmo_areas.as_ref()).and_then(|(k, cat)| {
        cat.0
            .resolve(k.wmo_id, k.name_set, k.group_area_id)
            .map(|a| a.area_table_id)
            .filter(|&id| id != 0)
    });
    let found = interior_area.or_else(|| {
        let (tx, ty) = world_to_tile(wow[0], wow[1]);
        streamer
            .tiles
            .get(&(tx as i32, ty as i32))
            .and_then(|ts| adt_tiles.get(&ts.handle))
            .and_then(|adt| benilla_formats::area_id_at(&adt.chunks, wow))
    });
    // 0 = unassigned in the data; treat like a miss so schedulers keep the last real zone.
    let found = found.filter(|&id| id != 0);
    if found.is_some() && *area != CurrentArea(found) {
        *area = CurrentArea(found);
    }
}
