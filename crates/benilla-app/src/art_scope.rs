//! **Within-map art residency** — the distance half of decision 0729's map-scope teardown.
//!
//! 0729 gave the world-art dedup caches a *reset point*: a cross-map transition clears them, and the
//! loading screen pays for the rebuild. B131 is the case that argument does not reach — a long flight
//! **inside** one map never fires that transition, so nothing ever evicts and `mats`/`images` ratchet
//! for as long as you stay on a continent (decision 0785's open half: 26.8 k materials / 2.8 k images
//! after ten minutes here; 3.2 → 4.9 GiB of VRAM on the reporter's tour).
//!
//! This module turns clear-on-*transition* into clear-on-*distance*: every cache entry remembers the
//! view focus it was last used from, and a sweep drops what was last wanted more than
//! [`DEFAULT_RADIUS_YD`] away. So residency is bounded by *where you are*, not by how long the process
//! has run, and the map-change clear stays exactly as it was — the hard reset on top of the soft one.
//!
//! **Distance decides what expires; a dwell floor decides how soon.** [`MIN_DWELL_SECS`] exists
//! because the radius alone is sized for gameplay speed — the detached free-fly camera crosses it in
//! five seconds and would lose a city's dedup only to rebuild it on the way back (found on the
//! director's first real run; see the constant).
//!
//! **Distance, not a TTL**, and the difference is safety rather than taste: a time-expired entry can
//! be one the streamer is about to ask for again from ten yards away, and dropping it costs a
//! duplicate; a distance-expired entry is one whose tiles unloaded long ago. The radius is therefore
//! floored at the streamer's own reach ([`radius_floor`]) so eviction can never outrun the thing that
//! placed the art.
//!
//! **Two properties worth knowing before reading the code**, both of which buy the design its
//! smallness:
//!
//! 1. **Stamping needs no plumbing.** A *use* (a [`SpatialCache::fetch`] hit or an insert) clears the
//!    stamp to `None` — "used since the last sweep" — and the *sweep* is what writes today's focus
//!    onto every `None`. So no call site has to know where the camera is, and a cache nothing sweeps
//!    (the `Local<MaterialCache>`s in the glue booth and the portrait bake) simply never expires.
//! 2. **Nothing needs ordering.** The stamp is at worst one sweep interval stale (≈25 yd at flight
//!    speed, against a 2.6 km radius) and `due` being read a frame before it is set only moves the
//!    sweep one frame. There are no `.before`/`.after` constraints anywhere in this module.
//!
//! The failure mode if the radius were ever too small: evicting the dedup for art that is still
//! drawn. That is not a visual bug and not a leak — the drawn entity holds its own handles, and the
//! next spawn of that art re-creates one duplicate material which becomes the new cache entry. It
//! costs a lost batch, and it self-heals. That asymmetry (too-eager = mild and transient, too-lazy =
//! the bug we are fixing) is why the floor is a clamp and not a warning.

use std::collections::HashMap;
use std::hash::Hash;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::time::Real;

use benilla_formats::TILE_SIZE;

/// Wall seconds between sweeps ([`Real`], not the virtual clock — this is a housekeeping cadence,
/// and decision 0789's reading applies: "what time is it" is not "how much world time to advance").
/// One `retain` over ~30 k entries a second is not measurable; the interval exists so it is not run
/// per frame, and it doubles as the stamp's resolution.
const SWEEP_SECS: f32 = 1.0;

/// Default eviction radius in yards — five ADT tiles (2667 yd). Chosen as the smallest round
/// multiple of the tile grid comfortably clear of [`radius_floor`] at the default
/// `$WOW_TILE_RADIUS` (2 ⇒ 2419 yd): far enough that art still on screen is never evicted, close
/// enough that leaving one city for another recovers it (Stormwind → Booty Bay is ~5500 yd,
/// Ironforge → Stormwind ~4250).
const DEFAULT_RADIUS_YD: f32 = 5.0 * TILE_SIZE;

/// The **dwell floor**: nothing is dropped within this many wall seconds of its last use, however far
/// away you have gone. Distance decides *what* expires; this decides how soon it is allowed to.
///
/// Found by the director's first real run, and it is the case the distance rule alone gets wrong. The
/// detached free-fly camera (`F`, `FlyCam::speed` 40–100 yd/s, ×5 on Ctrl) travels at up to 500 yd/s —
/// so it crosses the 2667 yd radius in **five seconds**, and a sweep then drops a whole city's dedup
/// that the same camera re-creates on the way back. Their 48-second leg evicted 25 570 entries and
/// rebuilt `pmat` 3086 → 15354 doing exactly that: the eviction was *correct* by the radius and
/// useless in effect, because nothing had finished being wanted.
///
/// 30 s covers a 15 000 yd round trip at boosted fly speed. Under gameplay speed it never binds at
/// all (a gryphon needs ~107 s to cover the radius), so B131's own case is unchanged — this only
/// stops the thrash at speeds the radius was never sized for. Residency stays bounded either way:
/// what you hold is at most the art of the last 30 seconds of travel, which does not grow with how
/// long the process has run.
const MIN_DWELL_SECS: f32 = 30.0;

/// The smallest radius that cannot evict art the streamer is still holding: the far **corner** of
/// the `(2r+1)²` tile block it keeps resident, plus a tile of margin.
///
/// A stamp records where *the viewer* was when the art was last wanted, not where the art is — so an
/// entry can be up to a streaming radius "behind" the art it belongs to. Flooring the radius at the
/// block's circumscribed radius means you have to leave the art's neighbourhood entirely before its
/// dedup expires.
fn radius_floor(tile_radius: u32) -> f32 {
    (tile_radius as f32 + 0.5) * TILE_SIZE * std::f32::consts::SQRT_2 + TILE_SIZE
}

/// Resolve `$WOW_ART_RADIUS` (yards; `0` or negative ⇒ eviction off, the A/B leg that reproduces the
/// unbounded behaviour on a fixed build) against the streamer's reach. An explicitly-requested radius
/// below [`radius_floor`] is raised **and** warned about, rather than silently obeyed: a knob that
/// quietly means something else than it says is how decision 0789 happened.
fn radius_from_env(tile_radius: u32) -> f32 {
    let floor = radius_floor(tile_radius);
    match std::env::var("WOW_ART_RADIUS")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
    {
        Some(r) if r <= 0.0 => 0.0,
        Some(r) if r < floor => {
            warn!(
                "art-scope: WOW_ART_RADIUS={r} is inside the streamer's own reach ({floor:.0} yd \
                 at tile radius {tile_radius}) — using {floor:.0}"
            );
            floor
        }
        Some(r) => r,
        None => DEFAULT_RADIUS_YD.max(floor),
    }
}

/// Horizontal (tile-plane) distance between two wow-space points. Height is deliberately ignored:
/// residency is a 2D question — the streamer's window is a tile square, and art 200 yd below a
/// gryphon is in front of you, not away from you.
fn plan_dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (dx, dy) = (a[0] - b[0], a[1] - b[1]);
    (dx * dx + dy * dy).sqrt()
}

/// A dedup cache whose entries remember **where they were last used from**, so a sweep can drop the
/// art of a place you have left. A drop-in for the `HashMap<K, Handle<_>>` these caches used to be:
/// the only difference at a call site is that a lookup is [`Self::fetch`] (it counts as a use).
///
/// `None` in the stamp means "used since the last sweep" — see the module docs for why that inverted
/// spelling is what removes all the plumbing.
pub(crate) struct SpatialCache<K, V> {
    map: HashMap<K, (V, Option<Stamp>)>,
}

/// Where and when an entry was last wanted: the view focus at the first sweep after its last use.
/// Both halves are load-bearing — `at` decides whether it is far enough to expire, `t` whether it has
/// been idle long enough to be allowed to ([`MIN_DWELL_SECS`]).
#[derive(Clone, Copy)]
struct Stamp {
    at: [f32; 3],
    t: f32,
}

impl<K, V> Default for SpatialCache<K, V> {
    fn default() -> Self {
        Self {
            map: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash, V> SpatialCache<K, V> {
    /// Live entry count — what the census reports and the map-change evictors log.
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    /// Drop everything (the map-scope teardown, decision 0729 — still the hard reset).
    pub(crate) fn clear(&mut self) {
        self.map.clear();
    }

    /// Fetch a deduped value, **counting the hit as a use** so it restarts its distance grace.
    /// Clones out rather than lending a reference: every caller wants an owned `Handle` anyway, and
    /// ending the borrow at the call is what lets the miss path insert in the same expression.
    pub(crate) fn fetch(&mut self, key: &K) -> Option<V>
    where
        V: Clone,
    {
        let slot = self.map.get_mut(key)?;
        slot.1 = None;
        Some(slot.0.clone())
    }

    /// Install a freshly-built value, counted as a use.
    pub(crate) fn insert(&mut self, key: K, value: V) {
        self.map.insert(key, (value, None));
    }

    /// Get-or-build in place, counted as a use — for the caches whose value is too big to clone out
    /// ([`crate::clutter::ClutterGeometry`]'s decoded submeshes).
    pub(crate) fn or_insert_with(&mut self, key: K, build: impl FnOnce() -> V) -> &mut V {
        let slot = self.map.entry(key).or_insert_with(|| (build(), None));
        slot.1 = None;
        &mut slot.0
    }

    /// Stamp everything used since the last sweep at `focus`/`now`, then drop every entry that is
    /// **both** more than `radius` yards away **and** idle for at least [`MIN_DWELL_SECS`]. Returns
    /// how many were dropped.
    ///
    /// The two conditions are not redundant. Distance alone expires art the moment a fast camera
    /// pulls away from it, which is how the director's free-fly leg lost a city's dedup and rebuilt
    /// it seconds later; the dwell floor is what makes "not immediate" true at any speed, not just at
    /// gameplay speed.
    pub(crate) fn scope(&mut self, focus: [f32; 3], radius: f32, now: f32) -> usize {
        let before = self.map.len();
        self.map.retain(|_, (_, stamp)| match *stamp {
            None => {
                *stamp = Some(Stamp { at: focus, t: now });
                true
            }
            Some(s) => plan_dist(s.at, focus) <= radius || now - s.t < MIN_DWELL_SECS,
        });
        before - self.map.len()
    }
}

/// Which cache a census row is about. Fixed set, in journal-column order — the CSV's columns only
/// ever grow at the end, so this enum's order is part of the file format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ArtSlot {
    /// `Placements::materials` — streamed doodad/WMO submesh materials.
    PlaceMats,
    /// `EntityMaterials` — creature/player/GameObject model materials.
    EntityMats,
    /// `SkinComposites` — composited character body atlases (decision 0044).
    Skins,
    /// `WorldAssets::model_materials` — the ground-clutter material dedup.
    ClutterMats,
    /// `WorldAssets::textures` — decoded world BLPs by (path, wrap).
    Textures,
    /// `ClutterGeometry` — CPU submesh copies of every detail M2 decoded.
    ClutterGeo,
}

impl ArtSlot {
    /// Every slot, in journal-column order.
    pub(crate) const ALL: [ArtSlot; 6] = [
        ArtSlot::PlaceMats,
        ArtSlot::EntityMats,
        ArtSlot::Skins,
        ArtSlot::ClutterMats,
        ArtSlot::Textures,
        ArtSlot::ClutterGeo,
    ];

    /// The journal's column name for this slot.
    pub(crate) fn column(self) -> &'static str {
        match self {
            ArtSlot::PlaceMats => "pmat",
            ArtSlot::EntityMats => "emat",
            ArtSlot::Skins => "skin",
            ArtSlot::ClutterMats => "cmat",
            ArtSlot::Textures => "tex",
            ArtSlot::ClutterGeo => "cgeo",
        }
    }

    fn idx(self) -> usize {
        match self {
            ArtSlot::PlaceMats => 0,
            ArtSlot::EntityMats => 1,
            ArtSlot::Skins => 2,
            ArtSlot::ClutterMats => 3,
            ArtSlot::Textures => 4,
            ArtSlot::ClutterGeo => 5,
        }
    }
}

/// Per-cache residency, refreshed by every [`ArtScope::apply`] call — the instrument that localizes
/// a residency ratchet to **one cache** in a single journal row, instead of the run-length probe that
/// found B131. `Assets<T>::len()` says "materials are growing"; this says which map is holding them.
#[derive(Resource, Default)]
pub(crate) struct ArtCensus {
    live: [usize; ArtSlot::ALL.len()],
    dropped: [usize; ArtSlot::ALL.len()],
}

impl ArtCensus {
    /// Live entries in one cache as of its last sweep.
    pub(crate) fn live(&self, slot: ArtSlot) -> usize {
        self.live[slot.idx()]
    }

    /// Entries dropped by distance across every cache since the run began.
    pub(crate) fn dropped_total(&self) -> usize {
        self.dropped.iter().sum()
    }
}

/// The sweep's shared state: the radius, this tick's focus, and whether this frame is a sweep frame.
#[derive(Resource, Default)]
pub(crate) struct ArtScopeState {
    /// Eviction radius in yards; `0` ⇒ never evict. Zero until [`configure_art_scope`] runs, so
    /// nothing can be dropped before the streamer's reach is known.
    radius: f32,
    /// The view focus, wow coords ([`crate::terrain_stream::view_focus`] — the same ladder the
    /// streamer and the WDL ring use). `None` when there is no focus at all (no avatar, no camera),
    /// which suspends both stamping and sweeping.
    focus: Option<[f32; 3]>,
    /// Set on the one frame per [`SWEEP_SECS`] that sweeps.
    due: bool,
    last_sweep: f32,
    /// Wall seconds since app start, this frame — the clock the dwell floor is measured on.
    now: f32,
}

impl ArtScopeState {
    /// The view focus this frame, wow coords — what the sweep measures from. Exposed for the FPS
    /// journal: its `x,y,z` are the *avatar*, which stands still through a whole detached free-fly, so
    /// on that leg the position columns say nothing about where the art was being asked for. This is
    /// the column that does.
    pub(crate) fn focus(&self) -> Option<[f32; 3]> {
        self.focus
    }
}

/// What an owning module needs to scope its caches: the policy, plus the census to report into.
#[derive(SystemParam)]
pub(crate) struct ArtScope<'w> {
    state: Res<'w, ArtScopeState>,
    census: ResMut<'w, ArtCensus>,
}

impl ArtScope<'_> {
    /// Sweep one cache (on sweep frames) and record its residency. Safe to call for a cache that is
    /// empty, unconfigured, or in a run with eviction switched off — it degrades to a census read.
    pub(crate) fn apply<K: Eq + Hash, V>(&mut self, cache: &mut SpatialCache<K, V>, slot: ArtSlot) {
        let dropped = match self.state.focus {
            Some(focus) if self.state.due && self.state.radius > 0.0 => {
                cache.scope(focus, self.state.radius, self.state.now)
            }
            _ => 0,
        };
        let i = slot.idx();
        self.census.live[i] = cache.len();
        self.census.dropped[i] += dropped;
        if dropped > 0 {
            debug!(
                "art-scope: dropped {dropped} {} entries beyond {:.0} yd ({} live)",
                slot.column(),
                self.state.radius,
                cache.len()
            );
        }
    }
}

/// Resolve the radius once the streamer's configuration exists (it decides the floor).
fn configure_art_scope(
    mut state: ResMut<ArtScopeState>,
    cfg: Option<Res<crate::assets::RenderConfig>>,
) {
    let tile_radius = cfg.map(|c| c.tile_radius).unwrap_or(2);
    state.radius = radius_from_env(tile_radius);
    if state.radius > 0.0 {
        info!(
            "art-scope: within-map art evicts beyond {:.0} yd of the view focus",
            state.radius
        );
    } else {
        warn!("art-scope: eviction OFF ($WOW_ART_RADIUS=0) — within-map residency is unbounded");
    }
}

/// Publish this frame's focus and decide whether it is a sweep frame. Deliberately unordered against
/// every consumer — see the module docs.
fn track_art_scope(
    mut state: ResMut<ArtScopeState>,
    time: Res<Time<Real>>,
    player: Res<crate::player::Player>,
    camera: Query<&Transform, With<crate::player::WorldCamera>>,
    roster: Option<Res<crate::char_select::Roster>>,
) {
    let cam = camera.single().ok().map(|c| c.translation);
    state.focus = (player.active || cam.is_some()).then(|| {
        crate::terrain_stream::view_focus(
            &player,
            cam,
            roster
                .as_deref()
                .and_then(crate::char_select::Roster::pending_entry),
        )
    });
    let now = time.elapsed_secs();
    state.now = now;
    state.due = now - state.last_sweep >= SWEEP_SECS;
    if state.due {
        state.last_sweep = now;
    }
}

/// Owns the sweep policy + the census. The caches themselves stay private to their modules; each
/// registers its own three-line system that hands its caches to [`ArtScope::apply`].
pub(crate) struct ArtScopePlugin;

impl Plugin for ArtScopePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ArtCensus>()
            .init_resource::<ArtScopeState>()
            .add_systems(
                Startup,
                configure_art_scope.after(crate::assets::AssetSet::Open),
            )
            .add_systems(Update, track_art_scope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// When the art is used and stamped.
    const NOW: f32 = 1000.0;
    /// A sweep past the dwell floor — the earliest moment distance is *allowed* to expire anything.
    /// Every "…is dropped" assertion sweeps here, which is itself the dwell floor's regression test:
    /// swap this back to `NOW` and three of them fail (verified).
    const LATER: f32 = NOW + MIN_DWELL_SECS + 1.0;
    const HOME: [f32; 3] = [0.0, 0.0, 0.0];
    const FAR: [f32; 3] = [4000.0, 0.0, 0.0];
    const R: f32 = DEFAULT_RADIUS_YD;

    /// A fresh entry survives its first sweep — that is the sweep that *stamps* it. Without this the
    /// grace period would depend on where in the second the art happened to load.
    #[test]
    fn the_first_sweep_stamps_rather_than_judges() {
        let mut c: SpatialCache<u32, u32> = SpatialCache::default();
        c.insert(1, 10);
        // Swept from the far side immediately: the entry has no stamp yet, so it cannot be too far.
        assert_eq!(c.scope(FAR, R, NOW), 0);
        assert_eq!(c.len(), 1);
        // Now it is stamped at FAR, so a later sweep from HOME drops it.
        assert_eq!(c.scope(HOME, R, LATER), 1);
        assert!(c.fetch(&1).is_none());
    }

    #[test]
    fn art_you_walked_away_from_is_dropped() {
        let mut c: SpatialCache<u32, u32> = SpatialCache::default();
        c.insert(1, 10);
        c.scope(HOME, R, NOW); // stamp at home
        assert_eq!(
            c.scope([R * 0.9, 0.0, 0.0], R, LATER),
            0,
            "inside the radius"
        );
        assert_eq!(c.scope(FAR, R, LATER), 1, "beyond it");
    }

    /// A hit restarts the grace, which is what keeps art in the direction of travel resident: the
    /// same tree material is re-fetched by every new placement that streams in ahead of you.
    #[test]
    fn a_use_restarts_the_grace() {
        let mut c: SpatialCache<u32, u32> = SpatialCache::default();
        c.insert(1, 10);
        c.scope(HOME, R, NOW);
        for step in 1..20 {
            let at = [step as f32 * R * 0.5, 0.0, 0.0];
            assert_eq!(c.fetch(&1), Some(10));
            // Swept well past the dwell floor every step, so what keeps it alive is the use.
            assert_eq!(
                c.scope(at, R, NOW + step as f32 * (MIN_DWELL_SECS + 1.0)),
                0,
                "re-used at step {step}"
            );
        }
    }

    /// `or_insert_with` builds once and counts as a use, like `fetch`.
    #[test]
    fn or_insert_with_builds_once_and_counts_as_a_use() {
        let mut c: SpatialCache<u32, Vec<u32>> = SpatialCache::default();
        let mut builds = 0;
        for _ in 0..3 {
            let v = c.or_insert_with(7, || {
                builds += 1;
                vec![1, 2, 3]
            });
            assert_eq!(v.len(), 3);
        }
        assert_eq!(builds, 1);
        assert_eq!(
            c.scope(FAR, R, LATER),
            0,
            "the last use restarted the grace"
        );
    }

    /// **The director's free-fly case.** A camera at boosted fly speed crosses the radius in ~5 s; if
    /// distance alone decided, the art it just streamed would be dropped and then rebuilt as the same
    /// camera came back. Nothing expires inside the dwell floor however far away the focus goes.
    #[test]
    fn a_fast_camera_cannot_outrun_the_dwell_floor() {
        let mut c: SpatialCache<u32, u32> = SpatialCache::default();
        c.insert(1, 10);
        c.scope(HOME, R, NOW);
        // 500 yd/s (FlyCam 100 × the Ctrl boost 5) for five seconds: far past the radius already.
        for s in 1..=5 {
            let at = [500.0 * s as f32, 0.0, 0.0];
            assert_eq!(
                c.scope(at, R, NOW + s as f32),
                0,
                "dropped {s} s out — the free-fly thrash"
            );
        }
        // Fly all the way back inside the radius and the entry is still the same one: no rebuild.
        assert_eq!(c.scope(HOME, R, NOW + 6.0), 0);
        assert_eq!(c.fetch(&1), Some(10));
        // Stay away past the floor and it does expire — the bound is delayed, not removed.
        c.scope(HOME, R, NOW + 7.0);
        assert_eq!(c.scope(FAR, R, NOW + 7.0 + MIN_DWELL_SECS), 1);
    }

    /// Height is not distance: flying over a city must not evict it.
    #[test]
    fn altitude_is_not_distance() {
        let mut c: SpatialCache<u32, u32> = SpatialCache::default();
        c.insert(1, 10);
        c.scope(HOME, R, NOW);
        assert_eq!(c.scope([0.0, 0.0, 10_000.0], R, LATER), 0);
    }

    /// Eviction must never outrun the streamer: the default radius clears the far corner of the
    /// resident tile block at the default `$WOW_TILE_RADIUS`, and the floor rises with it.
    #[test]
    fn the_radius_clears_the_streamers_own_reach() {
        assert!(
            DEFAULT_RADIUS_YD > radius_floor(2),
            "default {DEFAULT_RADIUS_YD} must clear the floor {}",
            radius_floor(2)
        );
        // The corner of the 5×5 block at tile radius 2 is ~1886 yd; the floor adds a tile.
        assert!(
            (radius_floor(2) - 2418.7).abs() < 1.0,
            "{}",
            radius_floor(2)
        );
        assert!(
            radius_floor(4) > DEFAULT_RADIUS_YD,
            "a wider window raises it"
        );
    }

    /// With `$WOW_ART_RADIUS` unset (every ordinary run, and the test binary) the radius is the
    /// default, floored — never zero, so a shipped build always bounds itself.
    #[test]
    fn an_unconfigured_run_still_bounds_itself() {
        assert_eq!(radius_from_env(2), DEFAULT_RADIUS_YD.max(radius_floor(2)));
        assert!(radius_from_env(8) >= radius_floor(8));
    }

    /// **The mechanism the whole design rests on**: dropping the cache's handle really does free the
    /// asset. If `Assets<T>` kept the value alive past its last strong handle, eviction would cost us
    /// the dedup and recover no memory — a failure indistinguishable, from the outside, from "the fix
    /// did nothing". Checked here rather than assumed, because the *reason* B131's `images` column
    /// never fell is precisely that these caches held the last handle.
    #[test]
    fn dropping_the_last_handle_frees_the_asset() {
        let mut app = App::new();
        app.add_plugins((
            bevy::app::TaskPoolPlugin::default(),
            bevy::asset::AssetPlugin::default(),
        ))
        .init_asset::<Image>();
        let mut cache: SpatialCache<u32, Handle<Image>> = SpatialCache::default();
        let id = {
            let mut images = app.world_mut().resource_mut::<Assets<Image>>();
            let handle = images.add(Image::default());
            let id = handle.id();
            cache.insert(1, handle);
            id
        };
        app.update();
        assert!(
            app.world().resource::<Assets<Image>>().contains(id),
            "held by the cache"
        );
        cache.scope(HOME, R, NOW); // stamp it here…
        assert_eq!(cache.scope(FAR, R, LATER), 1, "…then walk away");
        // The drop is processed by `Assets::track_assets` on a later frame, not at the drop itself.
        for _ in 0..3 {
            app.update();
        }
        assert!(
            !app.world().resource::<Assets<Image>>().contains(id),
            "the image outlived its last handle — eviction would free nothing"
        );
    }

    /// A cache nobody sweeps (the `Local<MaterialCache>`s in the glue booth / portrait bake) expires
    /// nothing: without a `scope` call the stamps stay `None` forever.
    #[test]
    fn an_unswept_cache_never_expires() {
        let mut c: SpatialCache<u32, u32> = SpatialCache::default();
        c.insert(1, 10);
        assert_eq!(c.fetch(&1), Some(10));
        assert_eq!(c.len(), 1);
    }
}
