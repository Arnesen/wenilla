//! The **under-floor census** (`WOW_GROUND_CENSUS=<secs>[,<every>]`) — the instrument that closes
//! B197's symptom: *"±half the NPCs in one building sit below the floor"*.
//!
//! Every reported site of that bug was a picture. A picture cannot say whether a unit is below the
//! floor **because the server put it there** (then it is the server's ledger) or because *we* pulled
//! it down off the pose the server sent (then it is ours), and it cannot say whether the floor the
//! unit belongs on is even collidable from where it stands. Both questions are one line of numbers
//! per unit:
//!
//! ```text
//! UGD 0x…  z=5.02  seat=9.14  drop=+4.12  terrain=5.02  above=9.07 (+4.05)  spline=0 swim=0
//! ```
//!
//! - **`seat`** is the server's own Z for this unit — the pose the create block / a move packet /
//!   its spline last wrote ([`GroundClamped::seat_y`]), before the ground clamp had its say.
//! - **`drop`** is `seat − z`: how far the clamp pulled the unit *below* the server's pose. This is
//!   the number that names the bug; on a healthy scene it is within a yard either way (the clamp's
//!   whole job is a small correction), and `sunk` counts the ones past [`SUNK_YD`].
//! - **`above`** is the lowest walkable surface strictly above the unit's feet, found by walking a
//!   one-sided down-ray through the stack. A unit with a floor right there and a big `drop` is
//!   standing under a floor it could be standing on — the exact shape of the report.
//! - **`terrain`** is the MCNK height under it, which is what the clamp finds when a building's own
//!   collider has not attached yet.
//!
//! Pair it with the slot-keyed probe identity and a `.go` to the reported spot:
//!
//! ```text
//! WOW_USER=probeN WOW_PASS=pprobeN WOW_CHAR=Probe<n> WOW_NOSOUND=1 \
//!   WOW_PROBE_CHAT=".go xyz 6558.80 457.23 9.07 1" \
//!   WOW_GROUND_CENSUS="30,15" WOW_PROBE_EXIT_AT=75 cargo run -q -p benilla
//! ```
//!
//! The repeat form is the load-race half: a census at 30 s and again at 45 s says whether a sunk
//! unit *stays* sunk once the world has finished arriving, which is what separates a transient from
//! the ratchet decision 1384 removed.

use benilla_assets::coords::bevy_to_wow;
use benilla_protocol::EntityKind;
use bevy::prelude::*;

use super::ProbeClock;
use crate::net::{CreatureSwimming, GroundClamped, Guid, NetEntity, SelfPlayer, Spline};

/// A drop past this (yd) counts as **sunk** — the headline count. The clamp's honest corrections are
/// the small float off a slightly-high wire Z and the little hill a straight spline cuts through;
/// half a yard is well clear of both and well under a storey.
const SUNK_YD: f32 = 0.5;

/// How far above a unit's feet the stack walk starts looking for the floor it should be on. A
/// storey and a bit: high enough to clear any building floor a unit could be sunk beneath, low
/// enough that it doesn't report the roof of an unrelated structure.
const CEILING_YD: f32 = 12.0;

/// Feet-clearance for "strictly above" in the stack walk — below this the surface *is* the one the
/// unit is standing on, not one above it.
const ABOVE_EPS: f32 = 0.05;

pub(crate) struct GroundCensusPlugin;

impl Plugin for GroundCensusPlugin {
    fn build(&self, app: &mut App) {
        let raw = std::env::var("WOW_GROUND_CENSUS").unwrap_or_default();
        let mut parts = raw
            .split(',')
            .map(|s| s.trim().parse::<f32>().unwrap_or(0.0));
        let at = parts.next().filter(|v| *v > 0.0).unwrap_or(30.0);
        let every = parts.next().unwrap_or(0.0);
        let radius = std::env::var("WOW_GROUND_CENSUS_RADIUS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(80.0);
        app.insert_resource(GroundCensus {
            next: at,
            every,
            radius,
        })
        .add_systems(Update, fire_ground_census);
    }
}

/// [`GroundCensusPlugin`] state: when the next census fires, how often after that (`0` = once), and
/// how far from the body to look.
#[derive(Resource)]
struct GroundCensus {
    next: f32,
    every: f32,
    radius: f32,
}

/// What the census reads per unit: identity, kind, pose, the clamp's memo (the seat), whether a
/// path is driving it, and whether it is swimming (which exempts it from the clamp).
type CensusQuery = (
    &'static Guid,
    &'static NetEntity,
    &'static Transform,
    Option<&'static GroundClamped>,
    Option<&'static Spline>,
    Has<CreatureSwimming>,
);

/// One census line per streamed `Unit` within [`GroundCensus::radius`] of the body, worst drop
/// first, under a summary line naming the count that matters.
fn fire_ground_census(
    mut probe: ResMut<GroundCensus>,
    time: ProbeClock,
    world: benilla_world::collision::WorldCollision,
    point: benilla_world::world_point::WorldPoint,
    body: Query<&Transform, With<SelfPlayer>>,
    units: Query<CensusQuery>,
) {
    let now = time.elapsed_secs();
    if probe.next <= 0.0 || now < probe.next {
        return;
    }
    probe.next = if probe.every > 0.0 {
        now + probe.every
    } else {
        -1.0
    };
    let Ok(body) = body.single() else {
        println!("GROUND_CENSUS t={now:.1} NO BODY — not in world, nothing measured");
        return;
    };
    let radius2 = probe.radius * probe.radius;

    let mut rows: Vec<(f32, bool, String)> = Vec::new();
    for (guid, net, t, clamped, spline, swimming) in &units {
        if net.kind != EntityKind::Unit
            || t.translation.distance_squared(body.translation) > radius2
        {
            continue;
        }
        let z = t.translation.y;
        let seat = clamped.map_or(z, |c| c.seat_y);
        let drop = seat - z;
        let terrain = point.terrain_height_under(t.translation);
        let above = lowest_surface_above(&world, t.translation);
        let wow = bevy_to_wow(t.translation);
        rows.push((
            drop,
            swimming,
            format!(
                "UGD {:#018x} pos=({:.2},{:.2},{:.2}) z={z:.2} seat={seat:.2} drop={drop:+.2} \
                 terrain={} above={} spline={} swim={}",
                guid.0,
                wow[0],
                wow[1],
                wow[2],
                terrain.map_or_else(|| "none".into(), |v| format!("{v:.2}")),
                above.map_or_else(|| "none".into(), |v| format!("{v:.2} ({:+.2})", v - z)),
                u8::from(spline.is_some()),
                u8::from(swimming),
            ),
        ));
    }
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    // A swimmer is EXEMPT from the clamp by design (its wire Z is its swim depth), so its seat is
    // simply the last Z the clamp saw before it entered the water — a stale memo, not a drop. It
    // stays in the listing and out of the count.
    let sunk = rows
        .iter()
        .filter(|(d, swim, _)| *d > SUNK_YD && !swim)
        .count();
    let worst = rows
        .iter()
        .find(|(_, swim, _)| !swim)
        .map_or(0.0, |(d, _, _)| *d);
    let at = bevy_to_wow(body.translation);
    println!(
        "GROUND_CENSUS t={now:.1} units={} sunk={sunk} worst={worst:+.2} radius={:.0} \
         body=({:.2},{:.2},{:.2})",
        rows.len(),
        probe.radius,
        at[0],
        at[1],
        at[2],
    );
    for (_, _, line) in &rows {
        println!("{line}");
    }
}

/// The lowest walkable surface **strictly above** `feet` — the floor a sunk unit should be standing
/// on. Walks the one-sided down-ray stack from [`CEILING_YD`] overhead: each hit above the feet
/// becomes the new candidate and the next cast starts just under it, so a unit beneath three
/// storeys reports the one immediately over its head rather than the roof. `None` when nothing is
/// overhead — the healthy outdoor case.
fn lowest_surface_above(
    world: &benilla_world::collision::WorldCollision,
    feet: Vec3,
) -> Option<f32> {
    let mut from = feet.y + CEILING_YD;
    let mut found = None;
    for _ in 0..8 {
        let reach = from - feet.y;
        if reach <= ABOVE_EPS {
            break;
        }
        let origin = Vec3::new(feet.x, from, feet.z);
        // A miss ENDS the walk with what it already has — it never discards it. `?` here read as
        // "no floor above this unit" for three of B197's four sunk NPCs, whose second cast (from
        // just under the floor it had already found, down to feet sitting on the terrain) missed
        // that terrain by a float hair. The instrument's own first finding was its own bug.
        let Some(hit) = world.ray_body(origin, Dir3::NEG_Y, reach) else {
            break;
        };
        let y = origin.y - hit.distance;
        if y <= feet.y + ABOVE_EPS {
            break;
        }
        found = Some(y);
        from = y - 0.02;
    }
    found
}
