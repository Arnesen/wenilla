//! The frame-cost meters: one rolling window per series, their tails, and the spike latch.
//!
//! **Why the tail, and why per-series.** The HUD used to carry percentiles on wall frame time and a
//! flat *mean* on cost — so the client had a tail with no cost and a cost meter with no tail, and
//! `cpu p99` existed nowhere. That is exactly the shape a regression hides in: 0366 measured a run
//! whose per-second mean never passed 16.5 ms while nearly every second carried a few 19-29 ms
//! frames, and the tail was what the director read as "30 fps". 0610 adds the shape — a spike is a
//! burst of ~6 consecutive frames, ~250 ms of degraded time, not one bad frame. Six frames inside a
//! 300-sample mean move it by 2 %.
//!
//! **Everything is measured against the window's own baseline, never against a constant.** 1355
//! records the same slot drifting −3.3 cpu_ms between sessions on thermal state alone, and 1353
//! records the pin-to-pin spread; an absolute threshold would be calibrated for one scene on one
//! afternoon. A spike here is "much more than *this* scene has been costing", which is what
//! noticing a regression actually means — and it works unchanged at 3 ms on an empty map and at
//! 16 ms in Stormwind.

use std::collections::VecDeque;

use bevy::ecs::system::NonSendMarker;
use bevy::prelude::*;
use bevy::time::Real;

use super::clock::{main_thread_cpu_secs, process_cpu_secs};

/// Recent frames kept for the percentiles + the graph (~5 s at 60 fps, ~2.5 s at 120).
pub(super) const SAMPLE_WINDOW: usize = 300;

/// Frame duration above which a frame is logged as a hitch (a stall the player feels). Far above
/// any present interval so it only fires on real stalls (load bursts), not normal jitter.
const HITCH_LOG_MS: f32 = 250.0;

/// A frame past `DROPPED_FACTOR` × the **observed** present interval is a missed interval — the
/// metric a synced player actually feels (0717).
///
/// The factor is 0717's, but the thing it multiplies is not. That record set the threshold at
/// 1.5 × a hardcoded 16.7 ms budget, which silently assumes a 60 Hz rail: on the director's
/// ProMotion panel granting ~120, a missed interval is 16.7 ms and lands *under* the 25 ms
/// threshold, so **every 120→60 drop went uncounted**. Multiplying the interval we can see instead
/// keeps 0717's semantics exactly at 60 Hz and restores them at any other refresh — including an
/// adaptive panel, which has no fixed rail at all (0294) and where the only honest answer to "what
/// is one interval" is "what the display has been giving us lately".
pub(super) const DROPPED_FACTOR: f32 = 1.5;

/// A frame must exceed its series' baseline by this ratio to count as a spike.
const SPIKE_FACTOR: f32 = 1.5;

/// ...**and** by at least this fraction of a present interval. Without the floor the ratio test
/// alone latches on noise: a main-thread baseline of 0.4 ms makes any 0.7 ms frame a "2× spike".
/// Half an interval is the point where an excess starts being able to cost a frame.
const SPIKE_FLOOR_OF_RAIL: f32 = 0.5;

/// The floor's own floor, for the first second of a run and any pathological rail estimate.
const SPIKE_FLOOR_MIN_MS: f32 = 2.0;

/// How long a finished burst stays on the pill after it ends. The whole point of the latch: a
/// spike lasts ~250 ms and the director is looking at the game, not at the HUD, so the evidence
/// has to outlive the event by long enough to glance at.
const LATCH_HOLD_SECS: f32 = 10.0;

/// Consecutive frames a [`SpikeKind::Worker`] burst must last before the badge will show it.
///
/// **Because `cpu` is a sum across threads, not a duration.** A frame in which six pool workers
/// happen to overlap carries six threads' milliseconds and trips the ratio test while costing the
/// frame nothing — the first live run of this HUD latched `▲31.3 work ×1` against `missed 0/300`,
/// which is the instrument reporting the scheduler rather than a regression. A real worker
/// regression is *sustained* (0610's burst is ~6 frames), so a few consecutive frames separates
/// the two cleanly.
///
/// [`SpikeKind::Main`] and [`SpikeKind::Stalled`] are deliberately **not** gated: one long
/// main-thread frame is itself the stutter, and `Stalled` already had to exceed the
/// missed-interval threshold to classify at all.
const WORKER_BURST_FRAMES: u32 = 3;

/// Baselines are re-derived this often, not every frame. A median moves slowly by construction, so
/// a one-second-stale baseline changes no verdict — and it keeps the collapsed pill's per-frame
/// path free of sorts (1370: the HUD is itself inside every campaign anchor).
const BASELINE_REFRESH_SECS: f32 = 1.0;

/// Seconds of history the slow trend keeps — the timescale creep lives on.
///
/// The fast window is ~2.5 s at 120 Hz: plenty to catch a burst, useless for noticing that a scene
/// which used to cost 15 ms now costs 19. One sample a second for a minute costs 60 floats and
/// makes a step change visible as a step.
pub(super) const TREND_WINDOW_SECS: usize = 60;

/// A rolling window of one cost series, in milliseconds, capped at its own length.
pub(super) struct Series {
    samples: VecDeque<f32>,
    cap: usize,
}

impl Series {
    fn new(cap: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(cap),
            cap,
        }
    }

    fn push(&mut self, ms: f32) {
        if self.samples.len() == self.cap {
            self.samples.pop_front();
        }
        self.samples.push_back(ms);
    }

    pub(super) fn cap(&self) -> usize {
        self.cap
    }

    pub(super) fn last(&self) -> f32 {
        self.samples.back().copied().unwrap_or(0.0)
    }

    pub(super) fn len(&self) -> usize {
        self.samples.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = f32> + '_ {
        self.samples.iter().copied()
    }

    /// Windowed mean. `None` on an empty window, so a caller can tell "no data" from "zero cost".
    pub(super) fn mean(&self) -> Option<f32> {
        (!self.samples.is_empty())
            .then(|| self.samples.iter().sum::<f32>() / self.samples.len() as f32)
    }

    /// `(p50, p99, max)` over the window. Sorts a copy — fine for a few-hundred-element dev HUD,
    /// and deliberately **not** on the collapsed pill's per-frame path (see [`Baselines`]).
    pub(super) fn percentiles(&self) -> (f32, f32, f32) {
        if self.samples.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        let mut v: Vec<f32> = self.samples.iter().copied().collect();
        v.sort_by(f32::total_cmp);
        let at = |q: f32| v[(((v.len() - 1) as f32) * q).round() as usize];
        (at(0.50), at(0.99), *v.last().unwrap())
    }

    /// The window's `q`-quantile. Sorts a copy, so like [`Self::percentiles`] it belongs off the
    /// per-frame path — [`Baselines`] is where its caller reads the answer from.
    fn quantile(&self, q: f32) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let mut v: Vec<f32> = self.samples.iter().copied().collect();
        v.sort_by(f32::total_cmp);
        v[(((v.len() - 1) as f32) * q).round() as usize]
    }

    /// The window's median alone — the baseline every spike test is relative to.
    fn median(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let mut v: Vec<f32> = self.samples.iter().copied().collect();
        v.sort_by(f32::total_cmp);
        v[((v.len() - 1) / 2).min(v.len() - 1)]
    }

    /// `(count, fraction)` of windowed frames above `threshold_ms`.
    pub(super) fn frames_over(&self, threshold_ms: f32) -> (usize, f32) {
        let n = self.samples.iter().filter(|&&ms| ms > threshold_ms).count();
        let frac = if self.samples.is_empty() {
            0.0
        } else {
            n as f32 / self.samples.len() as f32
        };
        (n, frac)
    }
}

/// What a spike was blamed on. The three are distinguishable **without GPU timestamps**, which we
/// do not have in-client on Apple Silicon (see the module header of [`super`]): the question "were
/// we busy, or were we blocked?" is answered by whether the CPU meters moved with the frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SpikeKind {
    /// Main-thread CPU ran long — our own systems, serialized, and the frame waited on them. The
    /// most actionable kind: it is the one a player feels as a stutter.
    Main,
    /// Process CPU ran long while the main thread did not — a worker-pool burst (asset decode,
    /// collider bake). Real work, but off the frame's critical path.
    Worker,
    /// The frame ran long while **neither** CPU meter moved: we were blocked, not busy. That is
    /// the GPU or the present path. Stated as an inference, because it is one — a blocked
    /// present-wait consumes no CPU (0717), so this is what its absence looks like from here.
    Stalled,
}

impl SpikeKind {
    /// The short tag the pill's badge carries.
    pub(super) fn tag(self) -> &'static str {
        match self {
            SpikeKind::Main => "main",
            SpikeKind::Worker => "work",
            SpikeKind::Stalled => "wait",
        }
    }

    pub(super) fn describe(self) -> &'static str {
        match self {
            SpikeKind::Main => {
                "main-thread CPU ran long — our systems, on the frame's critical path"
            }
            SpikeKind::Worker => {
                "process CPU ran long while the main thread did not — a worker-pool burst \
                 (asset decode, collider bake), off the critical path"
            }
            SpikeKind::Stalled => {
                "the frame ran long while neither CPU meter moved — blocked, not busy: \
                 the GPU or the present path (inferred, we have no GPU timer on Apple Silicon)"
            }
        }
    }
}

/// A burst that has finished and must stay visible after it ended.
#[derive(Clone, Copy)]
pub(super) struct Spike {
    /// Worst frame of the burst, in the tripping series' milliseconds.
    pub(super) peak_ms: f32,
    /// The series' baseline at the time, so the badge can say what "normal" was.
    pub(super) baseline_ms: f32,
    /// Consecutive frames the burst lasted. 0610: a spike is a burst, not a frame — the six-frame
    /// version is what reads as a quarter-second of degraded time.
    pub(super) frames: u32,
    pub(super) kind: SpikeKind,
    /// `Time<Real>` seconds at the burst's last frame.
    pub(super) at: f32,
}

/// Is this burst worth a badge? See [`WORKER_BURST_FRAMES`] — the one kind that needs to prove
/// itself is the one measured by a cross-thread sum.
fn reportable(s: &Spike) -> bool {
    s.kind != SpikeKind::Worker || s.frames >= WORKER_BURST_FRAMES
}

/// Medians re-derived once a second, off the per-frame path.
#[derive(Default)]
struct Baselines {
    wall: f32,
    cpu: f32,
    main: f32,
    /// The trend sparkline's y-scale: the trend window's p90, **not its max**. Derived here rather
    /// than in the painter because it needs a sort and the pill draws every frame — and refreshed
    /// on exactly the tick that appends the sample it summarises, so it can never lag by more
    /// than one point.
    trend_hi: f32,
    at: f32,
}

/// The per-frame cost meters and everything derived from them.
///
/// Three series, because they answer three different questions and 0717's law binds all of them:
/// while synced, **wall frame time is the display's grant, not our cost**. Only the CPU series
/// measure work.
#[derive(Resource)]
pub(super) struct FrameStats {
    /// Wall frame interval. The grant while synced; our real cost only when uncapped.
    pub(super) wall: Series,
    /// Process CPU per frame, user+system across every thread (`getrusage`) — the campaign's
    /// currency (0736) and the meter the rail cannot fool (0717).
    pub(super) cpu: Series,
    /// Main-thread CPU per frame. The serialized half of the above: what a hitch is made of.
    pub(super) main: Series,
    /// One `cpu` median per second, for [`TREND_WINDOW_SECS`]. The creep lane: the fast window
    /// cannot see a regression that is merely *sustained*, because a sustained cost IS its own
    /// baseline there.
    pub(super) trend: Series,
    prev_cpu_secs: Option<f64>,
    prev_main_secs: Option<f64>,
    baselines: Baselines,
    /// The burst currently accumulating, if this frame and its predecessors are tripping.
    run: Option<Spike>,
    /// The worst burst inside the hold window — what the pill actually shows.
    latched: Option<Spike>,
    /// Bursts that have finished inside the hold window, latched or not.
    recent_bursts: u32,
    recent_since: f32,
}

impl Default for FrameStats {
    fn default() -> Self {
        Self {
            wall: Series::new(SAMPLE_WINDOW),
            cpu: Series::new(SAMPLE_WINDOW),
            main: Series::new(SAMPLE_WINDOW),
            trend: Series::new(TREND_WINDOW_SECS),
            prev_cpu_secs: None,
            prev_main_secs: None,
            baselines: Baselines::default(),
            run: None,
            latched: None,
            recent_bursts: 0,
            recent_since: 0.0,
        }
    }
}

impl FrameStats {
    /// The display's present interval as observed, not as assumed. See [`DROPPED_FACTOR`].
    pub(super) fn rail_ms(&self) -> f32 {
        self.baselines.wall
    }

    /// The threshold a frame must pass to count as a missed interval.
    pub(super) fn dropped_above_ms(&self) -> f32 {
        self.rail_ms() * DROPPED_FACTOR
    }

    /// The sparkline's y-scale — see [`Baselines::trend_hi`].
    pub(super) fn trend_hi(&self) -> f32 {
        self.baselines.trend_hi
    }

    /// Windowed mean frames per second. A mean, and labelled as one — it is the number that cannot
    /// see a spike, which is why it is no longer the pill's headline.
    pub(super) fn fps(&self) -> f32 {
        match self.wall.mean() {
            Some(mean) if mean > 0.0 => 1000.0 / mean,
            _ => 0.0,
        }
    }

    /// The spike still inside its hold window, if any.
    pub(super) fn spike(&self, now: f32) -> Option<Spike> {
        self.latched
            .filter(|s| now - s.at < LATCH_HOLD_SECS)
            .or(self.run)
            .filter(reportable)
    }

    /// Bursts finished inside the hold window — "it happened three times" is a different report
    /// from "it happened once", and only the tooltip has room to say so.
    pub(super) fn recent_bursts(&self, now: f32) -> u32 {
        if now - self.recent_since < LATCH_HOLD_SECS {
            self.recent_bursts
        } else {
            0
        }
    }

    /// How far a frame must sit above its baseline to count. Both tests must pass: the ratio, so
    /// the rule travels between a 3 ms scene and a 16 ms one, and the floor, so it does not fire
    /// inside the noise of a series whose baseline is a fraction of a millisecond.
    fn spike_excess_ms(&self, baseline: f32) -> f32 {
        let floor = (self.rail_ms() * SPIKE_FLOOR_OF_RAIL).max(SPIKE_FLOOR_MIN_MS);
        floor.max(baseline * (SPIKE_FACTOR - 1.0))
    }

    /// Classify the frame just pushed. `None` on a normal frame.
    ///
    /// Order is deliberate and is the whole attribution: main-thread cost first (the actionable
    /// kind), then a worker burst, and only if *neither* CPU meter moved is a long frame read as a
    /// stall. A GPU-bound frame is precisely the one that is long while the CPU is flat.
    fn classify(&self) -> Option<(SpikeKind, f32, f32)> {
        let b = &self.baselines;
        let tripped = |v: f32, base: f32| v - base > self.spike_excess_ms(base);

        if !self.main.is_empty() {
            let v = self.main.last();
            if tripped(v, b.main) {
                return Some((SpikeKind::Main, v, b.main));
            }
        }
        if !self.cpu.is_empty() {
            let v = self.cpu.last();
            if tripped(v, b.cpu) {
                return Some((SpikeKind::Worker, v, b.cpu));
            }
        }
        let v = self.wall.last();
        if v > self.dropped_above_ms() && tripped(v, b.wall) {
            return Some((SpikeKind::Stalled, v, b.wall));
        }
        None
    }

    /// Extend or close the current burst. O(1) — this is the whole per-frame cost of the latch.
    fn observe(&mut self, now: f32) {
        match self.classify() {
            Some((kind, peak, baseline)) => {
                let run = self.run.get_or_insert(Spike {
                    peak_ms: peak,
                    baseline_ms: baseline,
                    frames: 0,
                    kind,
                    at: now,
                });
                run.frames += 1;
                run.at = now;
                if peak > run.peak_ms {
                    run.peak_ms = peak;
                    // The worst frame of a burst names it: a burst that starts as a worker spill
                    // and peaks on the main thread is a main-thread event.
                    run.kind = kind;
                    run.baseline_ms = baseline;
                }
            }
            None => {
                // The burst (if any) just ended: commit it, unless it never earned a badge — a
                // filtered burst is not a burst, or the tooltip would count three while the pill
                // shows one. The latch keeps the WORST burst in the hold window rather than the
                // most recent, so a small one cannot bury the big one you were meant to see.
                if let Some(run) = self.run.take().filter(reportable) {
                    if now - self.recent_since >= LATCH_HOLD_SECS {
                        self.recent_bursts = 0;
                        self.recent_since = now;
                    }
                    self.recent_bursts += 1;
                    let expired = self
                        .latched
                        .is_none_or(|l| now - l.at >= LATCH_HOLD_SECS || run.peak_ms >= l.peak_ms);
                    if expired {
                        self.latched = Some(run);
                    }
                }
            }
        }
    }

    /// The trend's oldest and newest samples — "it used to cost this, it costs that now". `None`
    /// until the window has enough history to make the comparison mean anything.
    pub(super) fn trend_ends(&self) -> Option<(f32, f32)> {
        (self.trend.len() >= 8).then(|| {
            let mut it = self.trend.iter();
            let first = it.next().unwrap_or(0.0);
            (first, self.trend.last())
        })
    }

    /// Drive the meters from a sibling module's test, on the same path [`sample_frame_time`] takes
    /// minus the clocks. Each frame is `(wall_ms, cpu_ms, main_ms)`; returns the clock it left off
    /// at. `hud`'s layout test needs a realistically-populated `FrameStats` — including a *latched
    /// spike*, whose description is the longest string the panel can draw.
    #[cfg(test)]
    pub(super) fn feed_frames(&mut self, frames: &[(f32, f32, f32)], start_t: f32, dt: f32) -> f32 {
        let mut t = start_t;
        for &(wall, cpu, main) in frames {
            self.wall.push(wall);
            self.cpu.push(cpu);
            self.main.push(main);
            self.refresh_baselines(t);
            if self.wall.len() > 1 && self.baselines.wall > 0.0 {
                self.observe(t);
            }
            t += dt;
        }
        t
    }

    fn refresh_baselines(&mut self, now: f32) {
        if now - self.baselines.at < BASELINE_REFRESH_SECS && self.baselines.at > 0.0 {
            return;
        }
        self.baselines = Baselines {
            wall: self.wall.median(),
            cpu: self.cpu.median(),
            main: self.main.median(),
            trend_hi: self.baselines.trend_hi,
            at: now,
        };
        // The trend samples the same median the baseline does, so the two lanes cannot disagree
        // about what "normal right now" is — the badge and the sparkline read one truth.
        if !self.cpu.is_empty() {
            self.trend.push(self.baselines.cpu);
            self.baselines.trend_hi = self.trend.quantile(0.90);
        }
    }
}

/// Sample every meter for this frame.
///
/// **`NonSendMarker` is load-bearing, not decoration.** Bevy's executor runs systems across the
/// task pool, and [`main_thread_cpu_secs`] reports *the calling thread*. Without the marker this
/// system would drift onto a worker and the `main` series would silently become "whichever thread
/// ran the sampler" — noise shaped like a measurement, which is the most expensive kind of
/// instrument bug.
pub(super) fn sample_frame_time(
    _pin_to_main_thread: NonSendMarker,
    time: Res<Time<Real>>,
    mut stats: ResMut<FrameStats>,
) {
    let now = time.elapsed_secs();
    let wall_ms = time.delta_secs() * 1000.0;
    stats.wall.push(wall_ms);

    let cpu = process_cpu_secs();
    if let (Some(prev), Some(n)) = (stats.prev_cpu_secs, cpu) {
        stats.cpu.push(((n - prev) * 1000.0) as f32);
    }
    stats.prev_cpu_secs = cpu;

    let main = main_thread_cpu_secs();
    if let (Some(prev), Some(n)) = (stats.prev_main_secs, main) {
        stats.main.push(((n - prev) * 1000.0) as f32);
    }
    stats.prev_main_secs = main;

    stats.refresh_baselines(now);
    // The first frames have no baseline worth testing against — a cold window would latch the
    // startup gap as a spike and hold it for ten seconds on every launch.
    if stats.wall.len() > 1 && stats.baselines.wall > 0.0 {
        stats.observe(now);
    }

    // Log hard hitches so a load freeze is attributable from the log alone (one big stall vs many
    // medium ones, and roughly when). The first frame's delta is the startup gap, not a hitch.
    if wall_ms > HITCH_LOG_MS && stats.wall.len() > 1 {
        warn!("frame hitch: {wall_ms:.0} ms (main thread blocked this long)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the meters directly, the way a run would. Times are seconds; each tuple is one frame
    /// as `(wall_ms, cpu_ms, main_ms)`.
    fn feed(stats: &mut FrameStats, frames: &[(f32, f32, f32)], start_t: f32, dt: f32) -> f32 {
        let mut t = start_t;
        for &(wall, cpu, main) in frames {
            stats.wall.push(wall);
            stats.cpu.push(cpu);
            stats.main.push(main);
            stats.refresh_baselines(t);
            if stats.wall.len() > 1 && stats.baselines.wall > 0.0 {
                stats.observe(t);
            }
            t += dt;
        }
        t
    }

    /// A settled 120 Hz scene must latch nothing at all — the instrument's own quiet frame.
    #[test]
    fn a_settled_scene_latches_no_spike() {
        let mut s = FrameStats::default();
        let calm: Vec<_> = (0..400)
            .map(|i| (8.3 + (i % 3) as f32 * 0.1, 15.0, 4.0))
            .collect();
        let t = feed(&mut s, &calm, 0.0, 1.0 / 120.0);
        assert!(
            s.spike(t).is_none(),
            "a flat scene must not read as a spike"
        );
    }

    /// The headline case: cost doubles on the main thread while the display keeps granting 120,
    /// so fps does not move at all. The pill must still catch it.
    #[test]
    fn a_main_thread_burst_latches_while_fps_never_moves() {
        let mut s = FrameStats::default();
        let calm: Vec<_> = (0..400).map(|_| (8.3, 15.0, 4.0)).collect();
        let mut t = feed(&mut s, &calm, 0.0, 1.0 / 120.0);
        let fps_before = s.fps();

        // Six consecutive frames of main-thread cost — 0610's burst shape — with the display
        // still granting its interval, exactly the case the old pill could not see.
        let burst: Vec<_> = (0..6).map(|_| (8.3, 15.0, 12.0)).collect();
        t = feed(&mut s, &burst, t, 1.0 / 120.0);
        t = feed(&mut s, &[(8.3, 15.0, 4.0)], t, 1.0 / 120.0);

        let spike = s
            .spike(t)
            .expect("a six-frame main-thread burst must latch");
        assert_eq!(spike.kind, SpikeKind::Main);
        assert_eq!(
            spike.frames, 6,
            "the badge reports the burst, not one frame"
        );
        assert!((spike.peak_ms - 12.0).abs() < 0.01);
        // The control: the number the pill used to show is still, by construction, unmoved.
        assert!(
            (s.fps() - fps_before).abs() < 1.0,
            "fps must be blind here — that is the whole reason the latch exists"
        );
    }

    /// A long frame with both CPU meters flat is a stall, not our work — and must be named as one.
    #[test]
    fn a_long_frame_with_flat_cpu_reads_as_stalled_not_busy() {
        let mut s = FrameStats::default();
        let calm: Vec<_> = (0..400).map(|_| (8.3, 15.0, 4.0)).collect();
        let mut t = feed(&mut s, &calm, 0.0, 1.0 / 120.0);
        t = feed(&mut s, &[(40.0, 15.0, 4.0)], t, 1.0 / 120.0);
        t = feed(&mut s, &[(8.3, 15.0, 4.0)], t, 1.0 / 120.0);

        let spike = s.spike(t).expect("a 40 ms frame must latch");
        assert_eq!(spike.kind, SpikeKind::Stalled);
    }

    /// Process CPU jumping while the main thread stays flat is a worker burst, off the critical
    /// path — the distinction the old all-threads-only meter could not draw.
    #[test]
    fn a_worker_burst_is_not_blamed_on_the_main_thread() {
        let mut s = FrameStats::default();
        let calm: Vec<_> = (0..400).map(|_| (8.3, 15.0, 4.0)).collect();
        let mut t = feed(&mut s, &calm, 0.0, 1.0 / 120.0);
        let burst: Vec<_> = (0..WORKER_BURST_FRAMES).map(|_| (8.3, 60.0, 4.0)).collect();
        t = feed(&mut s, &burst, t, 1.0 / 120.0);
        t = feed(&mut s, &[(8.3, 15.0, 4.0)], t, 1.0 / 120.0);

        let spike = s.spike(t).expect("a worker burst must latch");
        assert_eq!(spike.kind, SpikeKind::Worker);
    }

    /// The noise floor the badge kept tripping on: `cpu` sums every thread, so one frame where
    /// several pool workers overlap trips the ratio test while costing the frame nothing. It must
    /// not reach the pill — nor the tooltip's burst count, or the two would disagree.
    #[test]
    fn a_one_frame_worker_blip_does_not_latch() {
        let mut s = FrameStats::default();
        let calm: Vec<_> = (0..400).map(|_| (8.3, 15.0, 4.0)).collect();
        let mut t = feed(&mut s, &calm, 0.0, 1.0 / 120.0);
        t = feed(&mut s, &[(8.3, 60.0, 4.0)], t, 1.0 / 120.0);
        t = feed(&mut s, &[(8.3, 15.0, 4.0)], t, 1.0 / 120.0);

        assert!(
            s.spike(t).is_none(),
            "a single-frame cross-thread CPU blip is the scheduler, not a regression"
        );
        assert_eq!(s.recent_bursts(t), 0, "and it is not counted as a burst");
    }

    /// The gate is on `Worker` alone: a one-frame main-thread spike *is* the stutter and must
    /// still latch immediately.
    #[test]
    fn a_one_frame_main_spike_is_not_gated() {
        let mut s = FrameStats::default();
        let calm: Vec<_> = (0..400).map(|_| (8.3, 15.0, 4.0)).collect();
        let mut t = feed(&mut s, &calm, 0.0, 1.0 / 120.0);
        t = feed(&mut s, &[(8.3, 15.0, 30.0)], t, 1.0 / 120.0);
        t = feed(&mut s, &[(8.3, 15.0, 4.0)], t, 1.0 / 120.0);

        let spike = s.spike(t).expect("one long main-thread frame must latch");
        assert_eq!(spike.kind, SpikeKind::Main);
        assert_eq!(spike.frames, 1);
    }

    /// The 120 Hz calibration bug, pinned. A missed interval on a ProMotion panel is 16.7 ms; the
    /// old fixed 25 ms threshold (1.5 × an assumed 60 Hz budget) could not see one.
    #[test]
    fn a_missed_interval_is_counted_at_120hz() {
        let mut s = FrameStats::default();
        let mut frames: Vec<_> = (0..300).map(|_| (8.3, 15.0, 4.0)).collect();
        frames[150] = (16.7, 15.0, 4.0);
        let _ = feed(&mut s, &frames, 0.0, 1.0 / 120.0);

        assert!(
            (s.rail_ms() - 8.3).abs() < 0.2,
            "the rail must be derived from the panel we are on, not assumed at 60 Hz"
        );
        let (n, _) = s.wall.frames_over(s.dropped_above_ms());
        assert_eq!(
            n, 1,
            "the doubled frame is a missed interval and must count"
        );
        // What the old rule would have said, kept as the contrast this test exists to prevent.
        let (old_n, _) = s
            .wall
            .frames_over(super::super::FRAME_BUDGET_MS * DROPPED_FACTOR);
        assert_eq!(old_n, 0, "the assumed-60 Hz threshold is blind to it");
    }

    /// The sparkline's y-scale must survive the launch spike. A loading second costs ~110 ms
    /// against a settled ~8.5, and scaling to the window's MAX pressed a whole minute of real
    /// signal into the bottom 7 % of the box — the lane read as a flat line on the director's
    /// screen. p90 drops the outlier and keeps the shape.
    #[test]
    fn one_loading_second_does_not_flatten_the_trend_lane() {
        let mut s = FrameStats::default();
        let mut t = 0.0;
        // A loading second, then a settled minute — one trend sample per second, so step the
        // clock a full second between feeds.
        t = feed(&mut s, &[(111.0, 111.0, 90.0)], t, 1.0);
        for _ in 0..40 {
            t = feed(&mut s, &[(16.6, 8.5, 1.7)], t, 1.0);
        }
        assert!(
            s.trend.iter().any(|v| v > 100.0),
            "the loading sample is still IN the window — this is about the scale, not a filter"
        );
        assert!(
            s.trend_hi() < 20.0,
            "p90 must ignore the launch outlier, got {}",
            s.trend_hi()
        );
        // The settled level has to land in a readable part of the box, not squashed at the floor.
        assert!(
            8.5 / (s.trend_hi() * 1.15) > 0.5,
            "the settled level must occupy the upper half of the lane"
        );
    }

    /// The latch must keep the worst burst in the window, not the most recent — a small wobble
    /// after a big spike must not bury it.
    #[test]
    fn the_latch_keeps_the_worst_burst_not_the_last() {
        let mut s = FrameStats::default();
        let calm: Vec<_> = (0..400).map(|_| (8.3, 15.0, 4.0)).collect();
        let mut t = feed(&mut s, &calm, 0.0, 1.0 / 120.0);
        t = feed(
            &mut s,
            &[(8.3, 15.0, 30.0), (8.3, 15.0, 4.0)],
            t,
            1.0 / 120.0,
        );
        t = feed(
            &mut s,
            &[(8.3, 15.0, 11.0), (8.3, 15.0, 4.0)],
            t,
            1.0 / 120.0,
        );

        let spike = s.spike(t).expect("still inside the hold window");
        assert!(
            (spike.peak_ms - 30.0).abs() < 0.01,
            "the 30 ms peak must survive the 11 ms one"
        );
        assert_eq!(
            s.recent_bursts(t),
            2,
            "both bursts are counted even though one is shown"
        );
    }

    /// And it must let go: a spike outside the hold window is over.
    #[test]
    fn the_latch_expires() {
        let mut s = FrameStats::default();
        let calm: Vec<_> = (0..400).map(|_| (8.3, 15.0, 4.0)).collect();
        let mut t = feed(&mut s, &calm, 0.0, 1.0 / 120.0);
        t = feed(
            &mut s,
            &[(8.3, 15.0, 30.0), (8.3, 15.0, 4.0)],
            t,
            1.0 / 120.0,
        );
        assert!(s.spike(t).is_some());
        assert!(s.spike(t + LATCH_HOLD_SECS + 0.1).is_none());
    }

    /// The floor exists so a sub-millisecond series cannot manufacture spikes out of its own
    /// noise: 2× of nearly nothing is still nearly nothing.
    #[test]
    fn a_tiny_baseline_does_not_manufacture_spikes() {
        let mut s = FrameStats::default();
        let calm: Vec<_> = (0..400).map(|_| (8.3, 15.0, 0.4)).collect();
        let mut t = feed(&mut s, &calm, 0.0, 1.0 / 120.0);
        t = feed(
            &mut s,
            &[(8.3, 15.0, 0.9), (8.3, 15.0, 0.4)],
            t,
            1.0 / 120.0,
        );
        assert!(
            s.spike(t).is_none(),
            "0.4 -> 0.9 ms is a 2x ratio and still noise; the floor must hold it"
        );
    }
}
