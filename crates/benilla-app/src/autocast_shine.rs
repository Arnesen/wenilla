//! The **autocast shine** — the four comet trails that run round an autocasting pet button —
//! drawn natively on the UI quad APPEND lane (decision 1383, B282).
//!
//! ## Why native (B282)
//!
//! Until 1383 the shine was ~92 script-layer `Texture` regions per button, `SetPoint`ed every
//! frame from a Lua `OnUpdate`. Moving a region dirties the layout epoch, so the first layout
//! getter later in the same tick forced a real anchor solve — whose cost is the whole-roster
//! preamble + fingerprint walk, NOT the ~92 moved leaves (decision 1350 scopes the *rounds*, not
//! the gate walk) — and the seeds-grown law forced a second full walk before the gate could
//! close. Measured at the SW gates pin on a default roster: **+4.25 ms cpu/frame for ONE
//! autocasting button**, spark-count-independent (23 sparks cost the same as 92), scaling with
//! the whole UI (an addon-laden roster pays multiples — nazriel's ~11 ms, B282). The reference
//! never pays any of this: its shine is a `<Model>` widget playing an M2 *outside* the FrameXML
//! layout tree. This module is that architecture on our substrate: the script layer carries one
//! **token texture** per button ([`SHINE_TOKEN`], shown/hidden on the autocast *edge*), the
//! extract's conversion records where it sits ([`ShineSite`] — the minimap-slot pattern), and
//! [`emit_shine`] appends the spark quads per frame with zero layout traffic: a shine-only frame
//! leaves the script pipeline fully settled.
//!
//! ## What is drawn — the M2's own law
//!
//! The reference's "autocast is on" mark is a `<Model>` playing
//! `Interface\Buttons\UI-AutoCastButton.mdx`; what that model actually is came out of the shipped
//! .m2 with `benilla-extract m2bones`/`m2part` (this block moved whole from `PetActionBar.xml`,
//! which owned the drawing until 1383):
//!
//! - 4 bones, pivoting at the corners of a 0.02 × 0.02 model-unit square, each keyed with the
//!   SAME 5-key 2.000 s translation loop walking the square edge by edge (0.5 s per edge), the
//!   four phase-shifted by one corner each so they chase one another round it. The tracks are
//!   `interp == 1`, LINEAR: constant speed along each edge, no easing into the corners.
//! - 4 particle emitters, one per bone: 300/s, life 1.000 s, Add blend, unlit, texture
//!   `Interface\Buttons\GlowStar.blp`, size half-extent 0.005 → 0.0015 → 0.001 over life, colour
//!   (0.976,0.875,0.192,1) → (0.996,0.945,0.745,1) → (1,1,1,0). The spline's 4 control points are
//!   ALL at the origin — a particle never travels: it is born where the bone is and shrinks away
//!   there. The effect is four comet trails running round the button's rim, not a rotating ring.
//!
//! MODEL UNITS → FrameXML units is the widget's own projection, not a calibration (wow-re
//! `system/ui/scratch/modelframe-animation-clock.md`, decision 1321's fold-back): the ortho leg
//! collapses to **1280 × modelScale × layoutScale** FrameXML units per model unit, independent of
//! aspect and resolution; `scale="1.2"` on the ref's `<Model>` is SetModelScale, so the 0.02 path
//! is **30.72 units** across a 30-unit button — 1.024×, overhanging its top and right by 2.4% —
//! and the model ORIGIN is the frame rect's BOTTOM-LEFT, which is why a square authored 0..0.02
//! lands across the whole button instead of in one quadrant.
//!
//! OUR ONE DEPARTURE is the particle COUNT: 300 live per emitter is 12 000 quads across a full
//! bar. We draw a SAMPLE of the same trail — each sample sits where the bone was `age` ago and
//! reads the same size/colour ramps at that age — so the trail's shape, speed and colour are the
//! file's; only its density is ours. **How many samples is DERIVED, not chosen** (B228/1317):
//! spacing the samples evenly left the tail as separated marching dots; spacing them by the
//! file's own size ramp — each step advances one star-width / [`OVERLAP`] of arc — keeps the
//! streak continuous everywhere for the fewest quads ([`trail_ages`]).
//!
//! ## The clock — truncation and all
//!
//! The reference does NOT play this loop at wall-clock rate (decision 1321, B228's second round):
//! `CSimpleModel` advances its private scene clock by `__ftol(elapsed * 1000.0)` — truncated,
//! no fractional carry (`0x76d846`; the WORLD driver adds 0.5 first, `0x48366b`) — so a 2000 ms
//! band takes `2000·T/floor(T)` ms at frame time T ms: 2083 ms at 60 fps, 2315 at 144, never
//! less than 2000. [`ShineClock`] is that integer-millisecond accumulator, advanced the
//! reference's way. All shine sites share one clock: the ref's per-button Models are all created
//! at UI load with `SetSequence(0)` and never drift, the loop being exact — one shared clock IS
//! that behaviour.

use bevy::prelude::*;

use crate::ui_pass::{UiQuad, UiQuadAppend, UiQuads};

/// The script layer's registration token: a `Texture` region with this path converts to **no
/// quads** — the conversion records a [`ShineSite`] at its rect/z instead, and [`emit_shine`]
/// draws there. Ours, not the install's — the string never reaches the asset resolver.
pub(crate) const SHINE_TOKEN: &str = "benilla:autocast-shine";

/// The spark art — the M2's own emitter texture, resolved once per site at conversion time
/// through the same resolver every UI texture uses.
pub(crate) const STAR_TEXTURE: &str = "Interface\\Buttons\\GlowStar";

/// The rim square's side in FrameXML units: 0.02 model units × 1280 × the ref Model's 1.2.
const SIZE: f32 = 0.02 * 1280.0 * 1.2;
/// The size ramp's FULL widths in FrameXML units (half-extents {0.005, 0.0015, 0.001} × 2,
/// projected like [`SIZE`]). The mid key is 0.0015, not 0.002 — `m2part` used to round its
/// printout (B228/1317).
const STAR: [f32; 3] = [
    0.010 * 1280.0 * 1.2,
    0.003 * 1280.0 * 1.2,
    0.002 * 1280.0 * 1.2,
];
/// The over-life colour ramp's three keys, `{r, g, b, a}` — the emitter's own.
const COLOR: [[f32; 4]; 3] = [
    [0.976, 0.875, 0.192, 1.0],
    [0.996, 0.945, 0.745, 1.0],
    [1.000, 1.000, 1.000, 0.0],
];
/// The bone loop, milliseconds (seq 0's authored 2.000 s band).
const PERIOD_MS: u32 = 2000;
/// The bone loop in seconds, for the phase/age arithmetic.
const PERIOD: f32 = 2.0;
/// A particle's lifespan, seconds.
const LIFE: f32 = 1.0;
/// One emitter per bone corner.
const EMITTERS: usize = 4;
/// Star-widths of cover on every point of the trail. 1 would put neighbours exactly edge to
/// edge, and a GlowStar fades toward its own edge, so the seam would show; 2 (centres half a
/// width apart) is the cheapest value with no seam (1317).
const OVERLAP: f32 = 2.0;

/// Sample a 3-key over-life ramp (the M2's `{k0, k1 at mid, k2}` shape, linear between),
/// one lane.
fn ramp(keys: [f32; 3], age: f32) -> f32 {
    let (a, b, t) = if age <= 0.5 {
        (keys[0], keys[1], age * 2.0)
    } else {
        (keys[1], keys[2], (age - 0.5) * 2.0)
    };
    a + (b - a) * t
}

/// [`ramp`] over the colour keys, all four lanes.
fn ramp_color(age: f32) -> [f32; 4] {
    std::array::from_fn(|i| ramp([COLOR[0][i], COLOR[1][i], COLOR[2][i]], age))
}

/// Where an emitter sits at lap fraction `f` (wrapped into 0..1), as a FrameXML-unit offset from
/// the button's BOTTOM-LEFT (the model's own origin — module doc). The bones walk the square
/// clockwise on screen from that corner, one edge per quarter-lap.
fn point(f: f32) -> (f32, f32) {
    let f = f - f.floor();
    let edge = f * 4.0;
    let leg = edge.floor();
    let t = edge - leg;
    match leg as u32 {
        0 => (0.0, t * SIZE),         // bottom-left → top-left
        1 => (t * SIZE, SIZE),        // top-left → top-right
        2 => (SIZE, SIZE - t * SIZE), // top-right → bottom-right
        _ => (SIZE - t * SIZE, 0.0),  // bottom-right → bottom-left
    }
}

/// The trail's sample ages, walked once off the size ramp (module doc: derived, not chosen —
/// each step advances one local star-width / [`OVERLAP`] of rim at the bone's own rim speed, so
/// the spacing tightens as the stars shrink and the streak stays unbroken end to end).
fn trail_ages() -> Vec<f32> {
    let speed = 4.0 * SIZE / PERIOD; // FrameXML units of rim per second
    let mut ages = Vec::new();
    let mut age = 0.0f32;
    while age < LIFE {
        ages.push(age);
        let w = ramp(STAR, age / LIFE);
        // Only positive while the ramp is — a zero in the size table must not hang the walk
        // (the 1306 failure mode; the Lua original bailed the same way).
        if w <= 0.0 {
            break;
        }
        age += w / OVERLAP / speed;
    }
    ages
}

/// [`trail_ages`], computed once. Each entry also pre-bakes everything a sample's fixed age
/// fixes: its size, colour and alpha (the per-frame pass moves quads and touches nothing else —
/// the same split the Lua pool made at spark creation, 1317).
static SPARKS: std::sync::LazyLock<Vec<(f32, f32, [f32; 4])>> = std::sync::LazyLock::new(|| {
    trail_ages()
        .into_iter()
        .map(|age| {
            let life = age / LIFE;
            (age, ramp(STAR, life), ramp_color(life))
        })
        .collect()
});

/// One autocasting button's shine site, recorded by the extract's conversion when it meets a
/// shown [`SHINE_TOKEN`] texture (the minimap-slot pattern): everything [`emit_shine`] needs to
/// draw there without asking the script layer anything.
#[derive(Clone, PartialEq)]
pub(crate) struct ShineSite {
    /// The token region's resolved rect — the BUTTON's rect (the token is SetAllPoints on it) —
    /// in window pixels, y-down.
    pub(crate) rect: Rect,
    /// The token's paint-order key, verbatim — sparks draw exactly where the old spark textures
    /// drew (the button's OVERLAY layer).
    pub(crate) z: u64,
    /// The token's clip rect, if any (a scroll-framed ancestor), window px y-down.
    pub(crate) clip: Option<Rect>,
    /// The token's effective UI alpha (parent chain folded), multiplied into every spark.
    pub(crate) alpha: f32,
    /// FrameXML-units → window-pixels factor at the conversion that recorded this site (the
    /// extract's `s`); a resize re-extracts and re-records.
    pub(crate) scale: f32,
    /// `Interface\Buttons\GlowStar`, resolved at record time.
    pub(crate) texture: Handle<Image>,
}

/// This frame's shine sites. Refilled by every FULL conversion (and left alone by the settled /
/// spliced paths — the token entries didn't change, so last conversion's sites are still true;
/// the token arm is deliberately not splice-simple, so any token edge reaches the full path).
#[derive(Resource, Default)]
pub(crate) struct ShineSites(pub(crate) Vec<ShineSite>);

/// The shared integer-millisecond loop clock (module doc: the reference's truncating
/// accumulator, decision 1321 — `floor(elapsed_ms)` added per frame, no fractional carry, so the
/// lap tracks the player's frame rate exactly as the reference widget's does).
#[derive(Resource, Default)]
pub(crate) struct ShineClock {
    ms: u32,
}

impl ShineClock {
    /// Advance by one frame's wall delta (seconds) and return the clock, in `0..PERIOD_MS`.
    fn advance(&mut self, elapsed: f64) -> u32 {
        self.ms = (self.ms + (elapsed * 1000.0).floor() as u32) % PERIOD_MS;
        self.ms
    }
}

/// Append every site's sparks to the UI quad overlay lane — [`UiQuadAppend`], beside the minimap
/// and the V-plates: re-emitted each frame, diffed by the mesh rebuild, never touching the
/// script layout. Runs (and advances the clock) unconditionally; with no sites it is two reads
/// and an add.
fn emit_shine(
    mut clock: ResMut<ShineClock>,
    sites: Res<ShineSites>,
    time: Res<Time<Real>>,
    mut quads: ResMut<UiQuads>,
) {
    let ms = clock.advance(time.delta_secs_f64());
    for site in &sites.0 {
        site_quads(site, ms, &mut quads.overlays);
    }
}

/// One site's sparks at clock `ms`, appended to `out` — [`emit_shine`]'s whole body, pure so the
/// tests drive it at exact millisecond clocks the way the old Lua goldens drove the `OnUpdate`.
fn site_quads(site: &ShineSite, ms: u32, out: &mut Vec<UiQuad>) {
    let lap = ms as f32 / PERIOD_MS as f32;
    let s = site.scale;
    // The model origin: the button rect's bottom-left, in y-down px.
    let (ox, oy) = (site.rect.min.x, site.rect.max.y);
    for e in 0..EMITTERS {
        let corner = e as f32 / EMITTERS as f32;
        for &(age, w, c) in SPARKS.iter() {
            // The spark's fixed place in the loop: its emitter's quarter-lap offset, less how
            // far the bone has moved on since this star was born.
            let (x, y) = point(lap + corner - age / PERIOD);
            let (cx, cy) = (ox + x * s, oy - y * s);
            let half = w * s * 0.5;
            out.push(UiQuad {
                rect: Rect::new(cx - half, cy - half, cx + half, cy + half),
                z_key: site.z,
                texture: Some(site.texture.clone()),
                color: [c[0], c[1], c[2], c[3] * site.alpha],
                additive: true,
                clip: site.clip,
                ..Default::default()
            });
        }
    }
}

/// Resource + producer registration (the [`UiQuadAppend`] window — `ui_pass` sorts the lanes
/// together by `z_key`, so the sparks interleave exactly at the button's overlay layer).
pub(crate) struct AutocastShinePlugin;

impl Plugin for AutocastShinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ShineSites>()
            .init_resource::<ShineClock>()
            .add_systems(Update, emit_shine.in_set(UiQuadAppend));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derivation golden: the size-ramp walk lands on 23 samples (1317's number, now pinned
    /// where the walk lives), strictly increasing, all inside a particle's life.
    #[test]
    fn the_trail_walk_derives_the_1317_sample_count() {
        let ages = trail_ages();
        assert_eq!(ages.len(), 23, "one star-width/2 of arc per step ⇒ 23");
        assert!(ages.windows(2).all(|w| w[1] > w[0]));
        assert!(ages.iter().all(|&a| (0.0..LIFE).contains(&a)));
        // The no-seam law (the old Lua golden's own pin, verbatim): no two neighbours sit
        // further apart on the rim than the SMALLER of their two stars is wide — the older
        // sample's, since the size ramp only shrinks. The 8-even-samples version failed exactly
        // this from mid-life back (7.7 px gaps under 4.6 px stars — B228's marching dots).
        let speed = 4.0 * SIZE / PERIOD;
        for w in ages.windows(2) {
            let gap = (w[1] - w[0]) * speed;
            let star = ramp(STAR, w[1] / LIFE).min(ramp(STAR, w[0] / LIFE));
            assert!(
                gap <= star,
                "gap {gap} exceeds star width {star} at age {}",
                w[0]
            );
        }
    }

    /// The head spark's quad, generated through the REAL producer body at exact millisecond
    /// clocks — the old Lua golden's position law on the native lane: origin at the site rect's
    /// bottom-left, the quarter-edge midpoint exactly half way (LINEAR, `interp == 1`), sizes
    /// and colours the ramps' own, the site's alpha folded, and 4 × 23 quads per site.
    #[test]
    fn a_sites_sparks_come_out_where_the_lua_pool_put_them() {
        let site = ShineSite {
            rect: Rect::new(72.0, 56.0, 102.0, 86.0), // the pet bar's button 1, y-down px
            z: 42,
            clip: None,
            alpha: 1.0,
            scale: 1.0,
            texture: Handle::default(),
        };
        let at = |ms: u32| {
            let mut out = Vec::new();
            site_quads(&site, ms, &mut out);
            assert_eq!(out.len(), EMITTERS * SPARKS.len());
            // Emitter 0's head spark is the first quad; its centre is the bone's own position.
            let r = out[0].rect;
            (
                (r.min.x + r.max.x) * 0.5 - site.rect.min.x,
                site.rect.max.y - (r.min.y + r.max.y) * 0.5,
            )
        };
        // The old Lua golden's own 0.01-px tolerance: the centre is reconstructed from the
        // quad's f32 edges, so exact-bit equality is not the claim — the corner is.
        let near = |got: (f32, f32), want: (f32, f32), what: &str| {
            assert!(
                (got.0 - want.0).abs() < 0.01 && (got.1 - want.1).abs() < 0.01,
                "{what}: at {got:?}, expected {want:?}"
            );
        };
        near(at(0), (0.0, 0.0), "clock 0: the bottom-left origin");
        near(
            at(250),
            (0.0, SIZE / 2.0),
            "quarter edge in: the LEFT edge's midpoint (LINEAR)",
        );
        near(at(500), (0.0, SIZE), "clock 500 ms: top-left");
        near(at(1000), (SIZE, SIZE), "clock 1000 ms: top-right");
        near(at(1500), (SIZE, 0.0), "clock 1500 ms: bottom-right");
        near(at(2000), (0.0, 0.0), "clock 2000 ms wraps onto clock 0");
        // The head quad wears the ramp's birth values: full width, the first colour key, and
        // the additive blend the emitter authors; z is the site's own paint order.
        let mut out = Vec::new();
        site_quads(&site, 0, &mut out);
        let head = &out[0];
        assert!((head.rect.max.x - head.rect.min.x - STAR[0]).abs() < 0.01);
        assert_eq!(head.color, COLOR[0]);
        assert!(head.additive);
        assert_eq!(head.z_key, 42);
        // The site's alpha folds into every spark on top of the ramp's own.
        let dim = ShineSite { alpha: 0.5, ..site };
        let mut dimmed = Vec::new();
        site_quads(&dim, 0, &mut dimmed);
        assert_eq!(dimmed[0].color[3], COLOR[0][3] * 0.5);
    }

    /// The corner walk, the old `pet_bar_tests` golden re-pinned on the native producer: the
    /// head spark of emitter 0 lands on each corner at 0.5 / 1.0 / 1.5 / 2.0 s (lap fractions
    /// 0.25/0.50/0.75/1.0), walking clockwise on screen from the bottom-left origin.
    #[test]
    fn the_head_spark_walks_the_corners_clockwise() {
        assert_eq!(point(0.0), (0.0, 0.0), "bottom-left at 0 s");
        assert_eq!(point(0.25), (0.0, SIZE), "top-left at 0.5 s");
        assert_eq!(point(0.5), (SIZE, SIZE), "top-right at 1.0 s");
        assert_eq!(point(0.75), (SIZE, 0.0), "bottom-right at 1.5 s");
        assert_eq!(point(1.0), (0.0, 0.0), "home at 2.0 s");
        // The four emitters chase one corner apart.
        assert_eq!(point(0.0 + 0.25), point(0.25));
    }

    /// Decision 1321's clock law, on the native accumulator: at a exact 60 fps the floor of
    /// every 16.666… ms delta is 16 ms, so the 2000 ms band takes ⌈2000/16⌉ = 125 frames —
    /// 2083 ms of wall time, never less than authored.
    #[test]
    fn the_clock_truncates_like_the_reference_widget() {
        let mut clock = ShineClock::default();
        let dt = 1.0 / 60.0;
        let mut frames = 0;
        loop {
            frames += 1;
            let before = clock.ms;
            let now = clock.advance(dt);
            if now < before {
                break; // wrapped: one full lap
            }
            assert!(frames < 1000, "clock never wrapped");
        }
        assert_eq!(
            frames, 125,
            "a 2000 ms band takes 125 frames at 60 fps (2083 ms)"
        );
        // 144 fps truncates harder — 6.944 ms banked as 6 — so the same lap takes 334 frames
        // (2315 ms of wall clock). A frame-rate-independent clock would land both at 2000 ms.
        let mut clock = ShineClock::default();
        let dt = 1.0 / 144.0;
        let mut frames = 0;
        loop {
            frames += 1;
            let before = clock.ms;
            if clock.advance(dt) < before {
                break;
            }
            assert!(frames < 1000, "clock never wrapped");
        }
        assert_eq!(
            frames, 334,
            "the same band takes 334 frames at 144 fps (2315 ms)"
        );
    }

    /// The size/colour ramps at the keys and midpoints — the M2's own numbers, so a future
    /// constant edit can't drift silently.
    #[test]
    fn the_ramps_read_the_m2s_keys() {
        // The interior key is reached through `a + (b - a) * 1.0`, which rounds once more than
        // the literal — toleranced, not bit-equal.
        let near = |got: f32, want: f32| assert!((got - want).abs() < 1e-4, "{got} vs {want}");
        near(ramp(STAR, 0.0), STAR[0]);
        near(ramp(STAR, 0.5), STAR[1]);
        near(ramp(STAR, 1.0), STAR[2]);
        for (key, age) in [(COLOR[0], 0.0), (COLOR[1], 0.5), (COLOR[2], 1.0)] {
            for (got, want) in ramp_color(age).into_iter().zip(key) {
                near(got, want);
            }
        }
    }
}
