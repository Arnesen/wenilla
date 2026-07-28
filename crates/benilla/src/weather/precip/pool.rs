//! The precip **sim economics** — the drop/patter records, the packet delay-line, the
//! ground oracle cache, and the per-kind frame (`run_kind`). Split from `precip`'s root;
//! byte-cited constants stay in the root's table, ECS wiring stays in the root's systems.

use bevy::platform::collections::HashMap;

use super::*;

/// One falling particle. Positions/velocities are Bevy world space (y up).
pub(super) struct Drop {
    pub(super) pos: Vec3,
    pub(super) vel: Vec3,
    /// Ground height (Bevy y) of the drop's CURRENT grid cell — dies (and may patter) on
    /// reaching it. Re-sampled whenever the drop drifts into a new cell ([`Drop::cell`]): at
    /// grade-1 rain the lateral drift is ~9.5 yd/s over a ~1.2 s fall (~11 yd), so the spawn
    /// column's ground is the WRONG answer by up to a house — a drop seeded outside the inn's
    /// porch drifted through the open front and splashed on the vestibule floor with the
    /// terrain's height (director-caught, 2026-07-13). Whether the reference stores the spawn
    /// answer or re-reads its grid per cell is open (Q-J); under a roof either reading keeps
    /// splashes out of the room.
    pub(super) land_y: f32,
    /// The grid cell `land_y` was sampled for (see [`HeightCache`]).
    pub(super) cell: (i32, i32),
    /// Per-particle size jitter (flake quads; 1.0 for rain streaks).
    pub(super) size: f32,
}

/// A recorded drop waiting in the packet pipeline: replays at `at` (its packet's `baseTime`
/// plus its record offset), and only once its packet has SEALED.
struct Pending {
    at: f32,
    drop: Drop,
}

/// One drop packet — the reference's `Packet<Drop>` (rf-weather-emission-timeline, rounds
/// 2–3). Records accumulate while OPEN; `baseTime` (`packet+0x36010`) is stamped ONLY at open
/// (`0x67598c`: `baseTime = now + 6144/rate`) and the batch REPLAYS from it — a delay line
/// (visible density = true density delayed by `6144/rate`). On shader hardware a packet draws
/// ONLY from its static buffer, baked once at close (`0x6752b0`, sole caller = the flush
/// `0x675a97`) — so an OPEN packet renders NOTHING: visibility needs sealed AND past its
/// replay instant. Sparse upswing cohorts stamp far-future baseTimes and are never seen at
/// record time (the very first packet stamps ~100 s out and genuinely ghost-replays its faint
/// drizzle later — it passes the 60 s duration guard `[0x80ff6c]`; byte-faithful, kept).
struct Packet {
    /// `buildTime` — when the packet opened (record offsets are relative to this).
    opened: f32,
    /// `baseTime` — `opened + 6144/rate_at_open`: the instant replay starts.
    visible_at: f32,
    records: Vec<Pending>,
    /// Total records ever pushed (records drain on activation; the CAP tracks this).
    count: u32,
}

/// One ground splash: a camera-facing triangle animated across the 4×4 splash atlas.
pub(super) struct Patter {
    pub(super) pos: Vec3,
    pub(super) age: f32,
    /// Atlas row (0..4 splash variants).
    pub(super) variant: u8,
}

/// The per-kind pool (falling layer + ground layer) — its geometry rides the shared effect
/// stream (0733), so it owns no meshes.
#[derive(Default)]
pub(super) struct Pool {
    /// ACTIVE drops — falling, rendered, landing.
    pub(super) drops: Vec<Drop>,
    /// The OPEN packet (recording; renders nothing).
    open: Option<Packet>,
    /// SEALED packets — closed cohorts replaying (or waiting for) their baseTime windows.
    sealed: Vec<Packet>,
    pub(super) patters: Vec<Patter>,
}

impl Pool {
    /// Records still in the pipeline (open + sealed) — the instrument's "pipe" count.
    pub(super) fn pending_len(&self) -> usize {
        self.open.as_ref().map_or(0, |p| p.records.len())
            + self.sealed.iter().map(|p| p.records.len()).sum::<usize>()
    }

    /// Close-check + (re)open for this frame's recording; returns the packet's remaining
    /// record space and the replay instant a record made NOW carries. Close conditions
    /// (`0x675a6d` full ≥ 6144 / build age ≥ `P/6144` / past `baseTime`): closing SEALS the
    /// packet (bakes its buffer — it may start drawing) and the next record opens a fresh one
    /// stamped at the live rate.
    fn open_for(&mut self, now: f32, rate: f32, close_age: f32) -> (usize, f32) {
        if let Some(pk) = &self.open {
            if pk.count >= PACKET_CAP as u32 || now - pk.opened >= close_age || now >= pk.visible_at
            {
                self.seal();
            }
        }
        let pk = self.open.get_or_insert_with(|| Packet {
            opened: now,
            visible_at: now + PACKET_CAP as f32 / rate.max(1.0),
            records: Vec::new(),
            count: 0,
        });
        (
            PACKET_CAP - pk.count as usize,
            pk.visible_at + (now - pk.opened),
        )
    }

    fn seal(&mut self) {
        if let Some(pk) = self.open.take() {
            if !pk.records.is_empty() {
                self.sealed.push(pk);
            }
        }
    }

    /// The TYPE-CHANGE cut (Q-D, driver `0x67be40` cross-fade): emission has already stopped
    /// (`0x67585d`); the OPEN packet is retired unbaked (`0x6756b2–da` — it never drew, so it
    /// is discarded), and every sealed packet whose replay hasn't started is unlinked
    /// (`0x67575a`: discard when `curTick ≤ baseTime`) — the straggler tail dies here. Packets
    /// already replaying finish their windows; active drops fall out normally (≤ ~1.3 s).
    pub(super) fn cut(&mut self, now: f32) {
        self.open = None;
        self.sealed.retain(|p| now > p.visible_at);
    }
}

/// Lazy ground-height cache over the reference's weather ground oracle — the fog grid's
/// sampler `0x6b7070` is **WMO/doodad-AWARE** (Q-B round 3, refuting the round-2 "terrain
/// only" read): after the MCVT terrain sample it probes the chunk's static object refs ±200 yd
/// (`CMapObj::IntersectSegment 0x6a37b0`) and MAXes the hit with terrain (`0x6b7237–4a`). So
/// drops LAND ON ROOFS — splashes on the inn roof, never inside, from any camera. Benilla's
/// equivalent: one downward ray per ~1.04-yd cell from the spawn plane against terrain +
/// walk-WMO + doodads/GameObjects. Cleared when the spawn plane moves a story (the ray start
/// decides which roofs it sees) or when it outgrows its bound.
#[derive(Resource, Default)]
pub(super) struct HeightCache {
    cells: HashMap<(i32, i32), f32>,
    cast_y: f32,
}

/// The reference's cell stride (`0x80ff98` = 1.0416666).
pub(super) const CELL: f32 = 1.041_666_6;
/// A probe that hits nothing reports ground 200 below the query (`0x67c812`: `refZ − 200`).
const MISS_DEPTH: f32 = 200.0;

impl HeightCache {
    pub(super) fn ground_y(
        &mut self,
        x: f32,
        z: f32,
        cast_from_y: f32,
        spatial: &SpatialQuery,
        filter: &SpatialQueryFilter,
    ) -> f32 {
        if (self.cast_y - cast_from_y).abs() > 8.0 || self.cells.len() > 20_000 {
            self.cells.clear();
            self.cast_y = cast_from_y;
        }
        let key = Self::key(x, z);
        if let Some(&y) = self.cells.get(&key) {
            return y;
        }
        let cx = (key.0 as f32 + 0.5) * CELL;
        let cz = (key.1 as f32 + 0.5) * CELL;
        let y = spatial
            .cast_ray(
                Vec3::new(cx, cast_from_y, cz),
                Dir3::NEG_Y,
                MISS_DEPTH + 50.0,
                true,
                filter,
            )
            .map_or(cast_from_y - MISS_DEPTH, |hit| cast_from_y - hit.distance);
        self.cells.insert(key, y);
        y
    }

    /// The grid cell owning an XZ position (the reference's 1.0416666-yd stride).
    pub(super) fn key(x: f32, z: f32) -> (i32, i32) {
        ((x / CELL).floor() as i32, (z / CELL).floor() as i32)
    }
}

/// The reference's per-frame record count (`0x6754cc–ef`): `fistp(min(space, quota) − 0.5)`
/// under round-nearest-EVEN — the x87 default. The parity matters: at `space = 1` this is
/// `RNE(0.5) = 0` FOREVER, so a packet that lands on 6143 records can never full-close and
/// seals only at the `P/6144` build age (5.7 s shader rain) — one of the three mechanisms
/// behind the reference's stochastic ~5 s / ~10 s upswing onset (round 3 Q-A).
fn frame_count(space: usize, quota: f32) -> usize {
    ((space as f32).min(quota) - 0.5).round_ties_even().max(0.0) as usize
}

/// One kind's frame: record (RNE-counted, packet-stamped), activate sealed cohorts,
/// integrate, land into the ground layer.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_kind(
    pool: &mut Pool,
    kind: WeatherKind,
    weather: &WeatherState,
    wind: &WeatherWind,
    heights: &mut HeightCache,
    spatial: &SpatialQuery,
    filter: &SpatialQueryFilter,
    rng: &mut u32,
    now: f32,
    dt: f32,
    cam_pos: Vec3,
) {
    let density = weather.density_for(kind);
    let (p, half_xy, z_off) = match kind {
        WeatherKind::Rain => (RAIN_P, RAIN_HALF_XY, RAIN_Z_OFF),
        _ => (SNOW_P, SNOW_HALF_XY, SNOW_Z_OFF),
    };

    // ===== record (the emit gate + the RNE frame count) =====
    // The per-frame emit gate (`0x6754a0`, threshold `[0x8015b8]` = 1.0): a frame records only
    // when `min(dt, 1/60)·rate > 1.0` — below ~60 drops/s NOTHING spawns (no trickle of ones).
    let rate = weather.density_gain() * p * density;
    let quota = rate * dt.min(DT_CAP);
    if quota > 1.0 {
        let close_age = p / PACKET_CAP as f32;
        let (space, replay_at) = pool.open_for(now, rate, close_age);
        let n = frame_count(space, quota)
            .min(POOL.saturating_sub(pool.drops.len() + pool.pending_len()));
        let anchor = cam_pos + wind.vel * WIND_LEAD;
        let w = density;
        for _ in 0..n {
            let (r1, r2, r3, r4, r5) = (
                rand01(rng),
                rand01(rng),
                rand01(rng),
                rand01(rng),
                rand01(rng),
            );
            // R2: the rand scatter lives on a flat plane at EYE height — the SEED point. (R10's
            // plane tilt — `lerp(0, 65°, sat(speed/18))` about the ⊥-heading axis — keys on the
            // ZONE AMBIENT wind speed (manager+0x84 → +0x7c), which benilla doesn't model yet;
            // at zone wind 0 the tilt is identity, so flat IS the byte behaviour here.)
            let scatter = Vec3::new((r1 - 0.5) * 2.0 * half_xy, 0.0, (r2 - 0.5) * 2.0 * half_xy);
            let seed = Vec3::new(anchor.x, cam_pos.y, anchor.z) + scatter;
            // The byte kinematics (w = density): the drift heading centres on the WORLD-FIXED
            // −1.57 azimuth (0x80ffbc) ± a grade-scaled spread — rain stays a coherent sheet
            // (±7.5° at grade 1); calm snow wanders anywhere (spread 2π at grade 0).
            let (vy, drift_mag, spread) = match kind {
                WeatherKind::Rain => (
                    -(RAIN_VZ_BASE + RAIN_VZ_W * w + RAIN_VZ_RNG * w * r3),
                    ((2.0 * r4 - 1.0) + RAIN_DRIFT_BASE) * w + RAIN_DRIFT_EPS,
                    RAIN_SPREAD_W * w + RAIN_SPREAD_BIAS,
                ),
                _ => (
                    -(SNOW_VZ_BASE + SNOW_VZ_W * w + w * r3),
                    ((r4 - 0.5) + SNOW_DRIFT_OFF) * w + SNOW_DRIFT_EPS,
                    std::f32::consts::TAU - SNOW_SPREAD_W * w,
                ),
            };
            let heading = DRIFT_AZ_CENTER + (r5 - 0.5) * spread;
            let vel = wow_azimuth_to_bevy(heading) * drift_mag + Vec3::Y * vy;
            // Back-project the drop up its own velocity to the box top (`0x674df6–e53`):
            // `T = −z_off/Vz`, spawn = seed − T·V — it starts z_off above the eye and passes
            // its eye-plane scatter point at t = T. `vy` is strictly negative for both kinds.
            let spawn = seed - vel * (z_off / -vy);
            let land_y = heights.ground_y(spawn.x, spawn.z, spawn.y, spatial, filter);
            // Ground above the whole spawn column (a mountainside or roof over the box top) —
            // the oracle reports ground ≥ spawn z and the drop dies at birth (`0x675051`).
            if land_y >= spawn.y {
                continue;
            }
            let drop = Drop {
                pos: spawn,
                vel,
                land_y,
                cell: HeightCache::key(spawn.x, spawn.z),
                size: 0.7 + 0.6 * rand01(rng),
            };
            if let Some(pk) = &mut pool.open {
                pk.count += 1;
                pk.records.push(Pending {
                    at: replay_at,
                    drop,
                });
            }
        }
    }

    // ===== activate: sealed cohorts replay their records from baseTime onward =====
    // A record whose replay instant passed while its packet was still baking appears mid-
    // window: fast-forward its fall (drift included — re-sample the ground at the cell it
    // drifted to) and skip it if it already landed (no splash — an unseen landing never drew).
    let cast_plane = cam_pos.y + z_off;
    let drops = &mut pool.drops;
    for pk in &mut pool.sealed {
        let mut i = 0;
        while i < pk.records.len() {
            if pk.records[i].at <= now {
                let rec = pk.records.swap_remove(i);
                let lag = now - rec.at;
                let mut drop = rec.drop;
                drop.pos += drop.vel * lag;
                let cell = HeightCache::key(drop.pos.x, drop.pos.z);
                if cell != drop.cell {
                    drop.cell = cell;
                    drop.land_y =
                        heights.ground_y(drop.pos.x, drop.pos.z, cast_plane, spatial, filter);
                }
                if drop.pos.y > drop.land_y {
                    drops.push(drop);
                }
            } else {
                i += 1;
            }
        }
    }
    pool.sealed.retain(|pk| !pk.records.is_empty());

    // Ground layer. Rain: a patter is created **1:1 with each landing drop** (rf-weather-render
    // Q5). The `|v|² ≤ 2` gate (0x6755c2, manager+0x5c) tests the RIDDEN-TRANSPORT velocity
    // (object 0x903) — ≈0 whenever the player isn't on a moving transport — NOT the wind:
    // running never kills splashes. Benilla has no ridden transports yet, so the gate always
    // passes; when transports land, test the transport's planar speed² ≤ 2.0 here. Snow: every
    // landing flake settles and fades over the `+0.25 s` window. Patter pool cap = 0x1800
    // (byte-cited; at full shader rain the landing rate saturates it — Q-A round 3).
    let ground_life = match kind {
        WeatherKind::Rain => PATTER_LIFE,
        _ => SNOW_SETTLE_LIFE,
    };
    let ground_gate = true;

    // ===== integrate + land =====
    // The landing test runs against the ground of the drop's CURRENT cell (re-sampled on cell
    // change — see [`Drop::land_y`]): a drop drifting under cover "lands" the moment its cell's
    // ground (the roof top, the grid is a max-from-above) rises above it, and its patter sits ON
    // that ground — the drip line splashes on the porch roof, never the room floor.
    let mut landed: Vec<Patter> = Vec::new();
    {
        pool.drops.retain_mut(|d| {
            d.pos += d.vel * dt;
            let cell = HeightCache::key(d.pos.x, d.pos.z);
            if cell != d.cell {
                d.cell = cell;
                d.land_y = heights.ground_y(d.pos.x, d.pos.z, cast_plane, spatial, filter);
            }
            if d.pos.y > d.land_y {
                return true;
            }
            if ground_gate {
                landed.push(Patter {
                    pos: Vec3::new(d.pos.x, d.land_y + 0.02, d.pos.z),
                    age: 0.0,
                    variant: 0, // assigned below (needs the rng)
                });
            }
            false
        });
    }
    for l in &mut landed {
        l.variant = (rand01(rng) * 4.0) as u8 & 3;
    }
    landed.truncate(GROUND_CAP.saturating_sub(pool.patters.len()));
    pool.patters.append(&mut landed);
    pool.patters.retain_mut(|l| {
        l.age += dt;
        l.age < ground_life
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The RNE frame count (`0x6754cc–ef`, x87 round-nearest-even): normal quotas round to
    /// the nearest count, and `space = 1` yields `RNE(0.5) = 0` — the parity stick that keeps
    /// a 6143-record packet from ever full-closing.
    #[test]
    fn frame_count_rounds_nearest_even() {
        assert_eq!(frame_count(6144, 583.3), 583); // RNE(582.8)
        assert_eq!(frame_count(300, 583.3), 300); // space-clamped: RNE(299.5) → 300 (even)
        assert_eq!(frame_count(1, 583.3), 0); // the parity stick: RNE(0.5) → 0
        assert_eq!(frame_count(2, 583.3), 2); // RNE(1.5) → 2 (even)
        assert_eq!(frame_count(6144, 1.2), 1); // RNE(0.7) → 1
    }

    /// The packet clock: baseTime is stamped at OPEN alone (`0x67598c`); a packet re-opens
    /// (re-stamping at the live rate) once past its own baseTime or the `P/6144` build age.
    /// Records replay ONLY after their packet seals (shader legs draw from the close-baked
    /// buffer, `0x6752b0` ← `0x675a97` sole caller).
    #[test]
    fn packet_seals_before_replaying() {
        let mut pool = Pool::default();
        let close_age = RAIN_P / PACKET_CAP as f32; // 35000/6144 ≈ 5.7 s
        let (_, at0) = pool.open_for(0.0, RAIN_P, close_age);
        assert!((at0 - PACKET_CAP as f32 / RAIN_P).abs() < 1e-3);
        // 0.1 s later, same packet — the stamp is unchanged; the record offset advances.
        let (_, at1) = pool.open_for(0.1, RAIN_P * 2.0, close_age);
        assert!((at1 - (at0 + 0.1)).abs() < 1e-3);
        // Past the baseTime the packet re-opens at the live rate.
        let (_, at2) = pool.open_for(1.0, RAIN_P * 2.0, close_age);
        assert!((at2 - (1.0 + PACKET_CAP as f32 / (RAIN_P * 2.0))).abs() < 1e-3);
    }

    /// The Q-D type-change cut: the open packet is discarded unbaked, sealed-but-not-yet-
    /// replaying packets are unlinked (`0x67575a`), replaying ones keep their records.
    #[test]
    fn cut_discards_the_unreplayed_pipeline() {
        let d = || Drop {
            pos: Vec3::ZERO,
            vel: Vec3::NEG_Y,
            land_y: -100.0,
            cell: (0, 0),
            size: 1.0,
        };
        let mut pool = Pool {
            open: Some(Packet {
                opened: 9.0,
                visible_at: 12.0,
                records: vec![Pending {
                    at: 12.0,
                    drop: d(),
                }],
                count: 1,
            }),
            ..Default::default()
        };
        pool.sealed.push(Packet {
            opened: 0.0,
            visible_at: 1.0, // replaying since t=1
            records: vec![Pending { at: 4.0, drop: d() }],
            count: 1,
        });
        pool.sealed.push(Packet {
            opened: 8.0,
            visible_at: 60.0, // never started replaying — the straggler
            records: vec![Pending {
                at: 60.5,
                drop: d(),
            }],
            count: 1,
        });
        pool.cut(10.0);
        assert!(pool.open.is_none());
        assert_eq!(pool.sealed.len(), 1, "only the replaying packet survives");
        assert!((pool.sealed[0].visible_at - 1.0).abs() < 1e-6);
    }

    /// GT#1 under the round-3 shader-leg model: on a full upswing the earliest possible
    /// replay is ~4.9 s (packet-1 is always the ~100 s forecast orphan) and the deterministic
    /// fixed-dt path lands in the reference's observed 4.5–15 s onset band — never the
    /// pre-pipeline 2.5 s.
    #[test]
    fn upswing_first_visible_rain_is_gated() {
        let mut pool = Pool::default();
        let close_age = RAIN_P / PACKET_CAP as f32;
        let dt = 1.0 / 60.0;
        let mut first_visible = f32::MAX;
        let mut t = 0.0f32;
        while t < 30.0 {
            let a = (t / 10.0).min(1.0);
            let intensity = ((a - 0.25) * (4.0 / 3.0)).max(0.0);
            let rate = RAIN_P * intensity; // K = 1 (weatherDensity 3)
            let quota = rate * dt;
            if quota > 1.0 {
                let (space, replay_at) = pool.open_for(t, rate, close_age);
                let n = frame_count(space, quota);
                if let Some(pk) = &mut pool.open {
                    pk.count += n as u32;
                    for _ in 0..n {
                        pk.records.push(Pending {
                            at: replay_at,
                            drop: Drop {
                                pos: Vec3::ZERO,
                                vel: Vec3::NEG_Y,
                                land_y: -100.0,
                                cell: (0, 0),
                                size: 1.0,
                            },
                        });
                    }
                }
            }
            // Replay eligibility: sealed AND past the record instant.
            for pk in &pool.sealed {
                for r in &pk.records {
                    if r.at <= t {
                        first_visible = first_visible.min(r.at.max(pk.visible_at));
                    }
                }
            }
            t += dt;
        }
        assert!(
            (4.5..15.0).contains(&first_visible),
            "first visible rain at {first_visible:.2} s — the reference band is ~5/8.5–14 s"
        );
    }
}
