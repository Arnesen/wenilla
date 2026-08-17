//! The **autocast shine** — the flaming rim that runs round an autocasting pet button — drawn
//! natively on the UI quad APPEND lane (decisions 1383, 1386; ledger B282, B228).
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
//! aspect and resolution; `scale=` on the ref's `<Model>` is SetModelScale, and the model ORIGIN
//! is the frame rect's BOTTOM-LEFT, which is why a square authored 0..0.02 lands across the whole
//! rect instead of in one quadrant.
//!
//! **The `<Model>`'s rect and scale are per-TEMPLATE, and the reference does not repeat itself**
//! (decision 1391) — which is why they ride the [`ShineSite`] and not a constant:
//!
//! - **pet button** — `setAllPoints` on 30×30 at `scale="1.2"`: the path is **30.72 units**,
//!   1.024× the button, overhanging its top and right by 2.4% and clipped there.
//! - **spell book button** — its own `<Size>36×36</Size>` at `CENTER (1,1)` on a 37×37 button, at
//!   `scale="1.22"`: the path is **31.232 units** inside a 36-unit rect that itself sits 1.5 units
//!   in from the button's bottom-left.
//!
//! **The stars on that path do NOT take the same projection** ([`STAR`], decisions 1386/1390):
//! their half-extent is an EYE-SPACE length added after the transform, so it misses `modelScale`
//! — the square is scaled by 1.2 and the stars standing on it are not — and, unlike the square,
//! it keeps the ortho leg's aspect term: `2 · half · √(W² + H²)` **window px**, free of UI scale
//! and frame scale. 12.80 px at 1024×768; 14.69 at 1280×720. 1386 froze the 4:3 case into a
//! constant, which is why our 16:9 stars were 22 % small until 1390.
//!
//! **And the widget is its own scissor** ([`site_quads`], decision 1387): a `<Model>` installs its
//! frame rect as the VIEWPORT for the duration of its draw and restores it after, so the model
//! cannot paint one pixel outside the button. With the path running on the rim and the stars
//! straddling it, that clip is half the silhouette — the reference's flat outer edge with the
//! flames licking inward, against the soft halo an unclipped draw hangs round the whole button.
//!
//! **We draw the authored population, not a sample of it** (decision 1386). `rate × life` is
//! 300 live particles per emitter, 1200 for the model, and that number IS the look: the bone
//! lays one down every 0.205 units of rim while a newborn star is 12.8 units across at 4:3 (more
//! on a widescreen — [`STAR`]), so ~62 of them pile onto any point of the band and ADD saturates
//! it into one continuous flaming rim —
//! unevenly by channel (the ramp is gold, so R and G clip where B does not, which is why it
//! reads gold and not white), and what travels is the brightness envelope, not any one star.
//! 1383 shipped a 23-sample stand-in for that population, chosen to keep the *coverage*
//! unbroken, and coverage was the wrong invariant: the samples tile the same path at 1/13th the
//! intensity, so the band came apart into legible marching sparkles (B228, the director's A/B
//! against the reference). Sampling saved cost the script layer was charging us; native, the
//! authored count is affordable, so there is nothing left to approximate.
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

/// The script layer's registration token: a `Texture` region whose path starts with this converts
/// to **no quads** — the conversion records a [`ShineSite`] at its rect/z instead, and
/// [`emit_shine`] draws there. Ours, not the install's — the string never reaches the resolver.
///
/// An optional `:<f32>` suffix carries the `<Model>`'s **`scale=`** ([`token_model_scale`]), which
/// is `SetModelScale` and is not the same on every button that wears this shine (1391).
pub(crate) const SHINE_TOKEN: &str = "benilla:autocast-shine";

/// Read a token's model scale, or `None` if `path` is not a shine token at all — the single
/// definition of what the token's grammar is, shared by the conversion (which records the site)
/// and the shipped-XML sweep (which must exempt exactly this and nothing that merely resembles
/// it, so a typo still fails the archive check).
pub(crate) fn token_model_scale(path: &str) -> Option<f32> {
    match path.strip_prefix(SHINE_TOKEN)? {
        "" => Some(1.2),
        rest => rest.strip_prefix(':')?.parse().ok(),
    }
}

/// The spark art — the M2's own emitter texture, resolved once per site at conversion time
/// through the same resolver every UI texture uses.
pub(crate) const STAR_TEXTURE: &str = "Interface\\Buttons\\GlowStar";

/// FrameXML units per model unit — `768 · 5/3`, **aspect- and resolution-independent** (wow-re
/// `modelframe-clip-and-scale.md` C2, confirmed exactly as 1321 recorded it): 1 model unit is
/// *not* 1 layout unit, because `0x76d1a0` always folds in `G48 · 5/3` (`76d1c6 0x41ae70(1.0)` =
/// G48 · `76d1cb fmul [0x80655c]` = f32(5/3)), and that `G48` cancels the `768·√(a²+1)` in the
/// FrameXML-unit definition. The star's half-extent ([`STAR`]) escapes that fold, which is the
/// whole reason the two constants no longer live in the same space.
const PER_MODEL_UNIT: f32 = 1280.0;

/// The rim square's side in FrameXML units at a given `SetModelScale`: the M2's authored
/// 0.02-unit bone square through [`PER_MODEL_UNIT`]. **30.72 at the pet bar's `scale="1.2"`,
/// 31.23 at the spell book's `scale="1.22"`** — the ref writes a different number on each
/// template and 1391 stopped averaging them into one constant.
fn side(model_scale: f32) -> f32 {
    0.02 * PER_MODEL_UNIT * model_scale
}
/// The size ramp's authored **half-extents, in MODEL units** (`file+0x104`) — deliberately not
/// pre-projected like [`side`], because they do not take [`side`]'s projection (decision 1390;
/// wow-re `modelframe-clip-and-scale.md` C3, which corrects the FrameXML-unit constant 1386 wrote
/// here and confirms everything else in that record).
///
/// A particle quad is not built in model space and transformed with the model: `0x7b3d20`
/// installs an **identity view** for the whole of the emitter's vertex generation (`7b3e41` save
/// / `7b3e46` install / restored at the tail), so the `±half` that `0x7b2a50` adds from the
/// `±1.0` corner table to the already-transformed position is an **eye-space length**; and file
/// flag `+0x04` bit `0x200` gates off the one multiply by the emitter scale (`0x7b2ba6 je`, taken
/// because `0x7b2ba3 test ah,0x2` finds the bit clear — the two *ungated* readers of
/// `[emitter+0x264]` are the bounding sphere). So `SetModelScale(1.2)` reaches the bone square
/// but **not** the stars standing on it.
///
/// The consequence, and the reason this is a model-unit constant now: an eye-space length is
/// projected by the ortho leg, which maps the frame's *diagonal-normalized* layout rect to NDC
/// ±1 — so a star's size is **aspect-dependent where the path's is not**.
///
/// > full width = `2 · half · 768·√(a²+1)` FrameXML units = **`2 · half · √(W² + H²)` window px**
///
/// The pixel form is the one [`site_quads`] uses: no UI scale, no frame scale, just the screen
/// diagonal. That is 12.80 px at 1024×768 — the number 1386 froze into a constant — but **14.69
/// px at 1280×720**, 22 % larger against a button 4 % *smaller*. The reference's shine really is
/// fatter on a widescreen; the 1280 that 1321 folded back is the 4:3 case of a law, not the law.
const STAR: [f32; 3] = [0.005, 0.0015, 0.001];
/// The in-plane billboard spin, radians per second of age (`file+0x198`, `angle = spin · age`,
/// closed form at `0x7b2ddc`) — 0.1 rad = 5.73° over a particle's whole life. The per-particle
/// sign flip that `0x7b2dea` would apply is gated by runtime bit `0x8000` ← file `+0x04` bit
/// `0x200`, which is CLEAR here, so **every** star turns the same way (wow-re C4: the `test
/// bl,0x20` behind it reads slot parity, not a stored field — there is no RNG in it).
const SPIN: f32 = 0.1;
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
/// A particle's lifespan, seconds (`file+0xc0`).
const LIFE: f32 = 1.0;
/// The authored emission rate, particles per second (`file+0xdc`).
const RATE: f32 = 300.0;
/// One emitter per bone corner.
const EMITTERS: usize = 4;
/// Where the over-life ramps break from key 0→1 to key 1→2 (`file+0x14c`).
const MID: f32 = 0.5;

/// Sample a 3-key over-life ramp at normalized age `u` — the reference's own two-segment law
/// (`age > midPoint·lifespan` picks the segment, `0x7b5aaa`) with the inset it applies to the
/// segment-local time (`t = u·0.99 + 0.005`, `0x7b9b10`), so the authored endpoint values are
/// **never exactly reached** — a newborn star is already 0.5 % of the way toward the mid key.
/// Tiny, and carried because it is free and because a golden that pins the authored endpoints
/// pins something the reference never draws.
fn ramp(keys: [f32; 3], u: f32) -> f32 {
    let (a, b, seg) = if u <= MID {
        (keys[0], keys[1], u / MID)
    } else {
        (keys[1], keys[2], (u - MID) / (1.0 - MID))
    };
    let t = seg * 0.99 + 0.005;
    a + (b - a) * t
}

/// [`ramp`] over the colour keys, all four lanes.
fn ramp_color(age: f32) -> [f32; 4] {
    std::array::from_fn(|i| ramp([COLOR[0][i], COLOR[1][i], COLOR[2][i]], age))
}

/// Where an emitter sits at lap fraction `f` (wrapped into 0..1), as a FrameXML-unit offset from
/// the button's BOTTOM-LEFT (the model's own origin — module doc). The bones walk the square
/// clockwise on screen from that corner, one edge per quarter-lap.
fn point(f: f32, side: f32) -> (f32, f32) {
    let f = f - f.floor();
    let edge = f * 4.0;
    let leg = edge.floor();
    let t = edge - leg;
    match leg as u32 {
        0 => (0.0, t * side),         // bottom-left → top-left
        1 => (t * side, side),        // top-left → top-right
        2 => (side, side - t * side), // top-right → bottom-right
        _ => (side - t * side, 0.0),  // bottom-right → bottom-left
    }
}

/// How many particles one emitter keeps alive — the emitter's own steady state, `rate × life`
/// (decision 1386: the population IS the look, and 1383 shipped a 23-sample stand-in for it).
///
/// **Both of the spawn driver's multipliers are byte-confirmed 1.0 for this widget** (wow-re
/// `modelframe-particle-density.md`): the camera-distance LOD
/// (`clamp(1 − (dist − 50)·0.02, 0.25, 1.0)`) sits at 1.0 *structurally*, because
/// `CSimpleModel::Draw` ZEROES the eye it is measured against (`0x76d3da`), leaving
/// `dist ≈ 0.02` layout units against a 50-unit inner radius — a UI model would have to be 50
/// screen diagonals away to fall off; and `particleDensity` defaults to 1.0 (`0x82e92c`) with no
/// UI exemption. The emitter's pool is `trunc(life·rate·1.15)` = 345 slots, so the 300 never hit
/// a cap either. `WOW_SHINE_N=<n>` stays as the A/B instrument, not as a pending question.
fn live_per_emitter() -> usize {
    static N: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
        std::env::var("WOW_SHINE_N")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or((RATE * LIFE).round() as usize)
    });
    *N
}

/// A multiplier on the authored star width (`WOW_SHINE_SIZE=<f>`, default 1.0) — an A/B
/// instrument, not a pending question: the projection it used to bracket is settled (1390).
///
/// The band's saturated THICKNESS is set by the quad width and its brightness by width × count,
/// so the two trade off and neither can be read off a capture alone — which is exactly why this
/// stays. `WOW_SHINE_SIZE=0.817` reproduces the pre-1390 4:3 sizing on a 16:9 window, so a
/// reference shot captured at 4:3 can be compared against ours without rebuilding.
/// **Never a tuning knob to leave dialled**: the shipped value is the one the bytes say.
fn size_scale() -> f32 {
    static S: std::sync::LazyLock<f32> = std::sync::LazyLock::new(|| {
        std::env::var("WOW_SHINE_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0)
    });
    *S
}

/// The live population, computed once: one entry per particle, oldest last, carrying everything
/// its FIXED age fixes — its age, its **model-unit half-extent** ([`STAR`]; the site's screen
/// diagonal turns it into pixels at emit), and its colour+alpha. Only the position moves per
/// frame, which is what keeps a 1200-quad emitter cheap.
///
/// The ages are the steady state of a `rate`/s emitter: one particle born every `1/rate` seconds,
/// none older than `life`. **Our one departure from the reference's own timeline**: it spawns in
/// per-frame batches (≈5 coincident particles per frame at 60 fps, at whatever position the bone
/// held that frame), where this spreads the same population evenly along the path. At the
/// authored star width against a 0.2-unit spawn pitch the two are the same picture; unlike a
/// sample count, it costs nothing to revisit, because the population is the file's.
static PARTICLES: std::sync::LazyLock<Vec<(f32, f32, [f32; 4])>> = std::sync::LazyLock::new(|| {
    let size_scale = size_scale();
    (0..live_per_emitter())
        .map(|k| {
            let age = k as f32 / RATE;
            let u = (age / LIFE).min(1.0);
            (age, ramp(STAR, u) * size_scale, ramp_color(u))
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
    /// extract's `s`); a resize re-extracts and re-records. Scales the bone PATH ([`side`]).
    pub(crate) scale: f32,
    /// The `<Model>`'s `SetModelScale`, read off the token ([`token_model_scale`]) — the ref
    /// writes `1.2` on the pet button and `1.22` on the spell book's, so it belongs to the SITE
    /// and not to a module constant (1391). Scales the bone path only; never the stars.
    pub(crate) model_scale: f32,
    /// The window's diagonal in the same pixels, `√(W² + H²)` — what scales the STARS, and
    /// nothing else does (decision 1390; [`STAR`]'s law is in screen px, free of UI scale and
    /// frame scale). Recorded beside `scale` so a resize re-answers both at once.
    pub(crate) diag: f32,
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
    // THE WIDGET IS ITS OWN SCISSOR (decision 1387). A `<Model>` does not draw into the UI's
    // shared space: `0x76d240` saves the viewport, installs the frame's OWN device rect as the
    // viewport for the duration of the model's draw, and restores it after
    // (`76d3cd` save · `76d5d7 0x58af60(left·x1, right·x1, bottom·y1, top·y1, 0, 1)` ·
    // `76d66c` restore — wow-re `system/ui/scratch/modelframe-camera-law.md` §4 steps 3/6/9),
    // while the ortho leg maps that same rect to NDC ±1 (§4b). Everything the model draws outside
    // the rect is therefore cut by the clip volume, with a hard flat boundary at the rect edge.
    //
    // That is not cosmetic here, it IS the shine's silhouette: the authored 0.02 square projects
    // to 30.72 units on a 30-unit button, so the bone path runs ON the rim and each 12.8-unit
    // newborn star straddles it — half in, half out. Clipped, only the inward half survives, and
    // the rim reads as the reference's does: a flat outer edge at the button border with ragged
    // flame tongues bleeding inward over the icon. Unclipped, the same stars spill a soft halo a
    // fifth of a button wide on all four sides — the director's A/B against the real client,
    // which shows the stone behind the button completely unlit (B228).
    let viewport = match site.clip {
        Some(inherited) => site.rect.intersect(inherited),
        None => site.rect,
    };
    if viewport.is_empty() {
        return;
    }
    let lap = ms as f32 / PERIOD_MS as f32;
    let s = site.scale;
    let side = side(site.model_scale);
    // The model origin: the button rect's bottom-left, in y-down px.
    let (ox, oy) = (site.rect.min.x, site.rect.max.y);
    for e in 0..EMITTERS {
        // Bone `e` LAGS bone `e-1` by 500 ms — a quarter lap *behind*, not ahead (wow-re's bone
        // table: bone 0 opens at BL, bone 1 at BR, bone 2 at TR, bone 3 at TL, all walking the
        // one clockwise cycle BL→TL→TR→BR). Four identical emitters at four phases draw the same
        // picture whichever way the index runs, which is exactly why the old `+e/4` could be
        // wrong for two of them and look right; the law is worth being right about anyway.
        let corner = -(e as f32) / EMITTERS as f32;
        for &(age, w, c) in PARTICLES.iter() {
            // The particle's fixed place in the loop: its emitter's quarter-lap offset, less how
            // far the bone has moved on since this star was born (it is baked into the model
            // frame at birth and never rides the bone — wow-re §7, flags bit 0x10 clear).
            let (x, y) = point(lap + corner - age / PERIOD, side);
            let (cx, cy) = (ox + x * s, oy - y * s);
            // The path took `s`; the star takes the screen diagonal instead ([`STAR`]) — the two
            // are different projections of the same model, not a shared one.
            let half = w * site.diag;
            out.push(UiQuad {
                rect: Rect::new(cx - half, cy - half, cx + half, cy + half),
                z_key: site.z,
                texture: Some(site.texture.clone()),
                color: [c[0], c[1], c[2], c[3] * site.alpha],
                additive: true,
                // The in-plane billboard spin ([`SPIN`]) — the one field 1321 recorded as live
                // and left unmodelled for want of a rotation to hang it on. `UiQuad::rotation`
                // is that rotation, and it turns the corners with their UVs, which is what the
                // reference's screen-plane rotate does.
                rotation: SPIN * age,
                clip: Some(viewport),
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

    /// The pet button's rim square — the geometry every golden below was taken at
    /// (`scale="1.2"`, the ref's own value on that template).
    fn s12() -> f32 {
        side(1.2)
    }

    /// The token's grammar (1391): it carries the `<Model>`'s `scale=`, because the reference
    /// writes a different one on each template that wears this shine — and it still refuses
    /// anything that merely looks like the token, which is what keeps the shipped-XML archive
    /// sweep honest about a typo.
    #[test]
    fn the_token_carries_the_models_scale_and_nothing_else_parses_as_one() {
        assert_eq!(token_model_scale("benilla:autocast-shine:1.2"), Some(1.2));
        assert_eq!(token_model_scale("benilla:autocast-shine:1.22"), Some(1.22));
        assert_eq!(
            token_model_scale(SHINE_TOKEN),
            Some(1.2),
            "bare = the pet bar's"
        );
        for bad in [
            "benilla:autocast-shin",
            "benilla:autocast-shine2",
            "benilla:autocast-shine:",
            "benilla:autocast-shine:x",
            "benilla:autocast-shine1.22",
            "Interface\\Buttons\\GlowStar",
        ] {
            assert_eq!(token_model_scale(bad), None, "{bad} must not parse");
        }
        // The two rim squares the shipped templates actually ask for.
        assert!((side(1.2) - 30.72).abs() < 1e-4, "pet button");
        assert!((side(1.22) - 31.232).abs() < 1e-4, "spell book");
    }

    /// The population golden (1386): the emitter keeps `rate × life` particles alive, aged one
    /// spawn interval apart, none past its life — the file's own numbers, so a constant edit
    /// cannot quietly re-open the sampling era.
    #[test]
    fn the_population_is_the_files_own_steady_state() {
        assert_eq!(PARTICLES.len(), 300, "300/s × 1.0 s life");
        assert_eq!(PARTICLES[0].0, 0.0, "the newest particle is newborn");
        for w in PARTICLES.windows(2) {
            let step = w[1].0 - w[0].0;
            assert!(
                (step - 1.0 / RATE).abs() < 1e-6,
                "spawn interval {step} ≠ 1/{RATE}"
            );
        }
        assert!(PARTICLES.iter().all(|p| p.0 < LIFE));
        // Density is what the look rests on: the bone lays a particle every 0.20 units of rim
        // while a newborn star is 12.8 units wide at 4:3, so ~60 stars pile onto any point of
        // the band. Additive, that saturates — the reference's thick continuous rim, and exactly
        // what 1383's 23 samples (7.7 units apart, one star deep) could not produce. Both terms
        // are in FrameXML units here, so the star takes the 4:3 case of its own law (1390).
        let pitch = 4.0 * s12() / PERIOD / RATE;
        let depth = 2.0 * ramp(STAR, 0.0) * 1280.0 / pitch;
        assert!(
            depth > 50.0,
            "only {depth:.0} stars deep — that will not saturate"
        );
    }

    /// The newborn particle's quad, generated through the REAL producer body at exact
    /// millisecond clocks: origin at the site rect's bottom-left, the quarter-edge midpoint
    /// exactly half way (LINEAR, `interp == 1`), the ramps' own size and colour, the site's
    /// alpha folded, and `4 × population` quads per site.
    #[test]
    fn a_sites_particles_come_out_where_the_bone_walked() {
        let site = ShineSite {
            rect: Rect::new(72.0, 56.0, 102.0, 86.0), // the pet bar's button 1, y-down px
            z: 42,
            clip: None,
            alpha: 1.0,
            scale: 1.0,
            model_scale: 1.2,
            diag: 1280.0, // 1024×768: s = 1 and the diagonal is 1280 — the law's 4:3 case
            texture: Handle::default(),
        };
        let at = |ms: u32| {
            let mut out = Vec::new();
            site_quads(&site, ms, &mut out);
            assert_eq!(out.len(), EMITTERS * PARTICLES.len());
            // Emitter 0's newest particle is the first quad; its centre is the bone itself.
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
            (0.0, s12() / 2.0),
            "quarter edge in: the LEFT edge's midpoint (LINEAR)",
        );
        near(at(500), (0.0, s12()), "clock 500 ms: top-left");
        near(at(1000), (s12(), s12()), "clock 1000 ms: top-right");
        near(at(1500), (s12(), 0.0), "clock 1500 ms: bottom-right");
        near(at(2000), (0.0, 0.0), "clock 2000 ms wraps onto clock 0");
        // The newborn quad wears the ramp's birth values (the inset included — it is 0.5 % of
        // the way to the mid key already), the additive blend the emitter authors, and the
        // site's own paint order.
        let mut out = Vec::new();
        site_quads(&site, 0, &mut out);
        let head = &out[0];
        let width = head.rect.max.x - head.rect.min.x;
        assert!((width - 2.0 * ramp(STAR, 0.0) * site.diag).abs() < 0.01);
        assert!(width < 2.0 * STAR[0] * site.diag, "the inset bites");
        assert_eq!(head.color, ramp_color(0.0));
        assert!(head.additive);
        assert_eq!(head.z_key, 42);
        // The site's alpha folds into every particle on top of the ramp's own.
        let dim = ShineSite { alpha: 0.5, ..site };
        let mut dimmed = Vec::new();
        site_quads(&dim, 0, &mut dimmed);
        assert_eq!(dimmed[0].color[3], ramp_color(0.0)[3] * 0.5);
    }

    /// The widget's viewport IS the silhouette (1387): every spark carries the site rect as its
    /// clip, an inherited scroll clip intersects with it rather than replacing it, and — the
    /// point of the whole thing — the sparks really do straddle the rim, so the clip is load
    /// bearing and not a decorative no-op.
    #[test]
    fn nothing_the_model_draws_escapes_the_widgets_rect() {
        let site = ShineSite {
            rect: Rect::new(72.0, 56.0, 102.0, 86.0),
            z: 42,
            clip: None,
            alpha: 1.0,
            scale: 1.0,
            model_scale: 1.2,
            diag: 1280.0, // 1024×768: s = 1 and the diagonal is 1280 — the law's 4:3 case
            texture: Handle::default(),
        };
        let mut out = Vec::new();
        site_quads(&site, 0, &mut out);
        assert!(out.iter().all(|q| q.clip == Some(site.rect)));

        // Unclipped, a newborn star hangs a long way past the border — half its 12.8 units plus
        // the 0.36 the 30.72 path already overhangs. That overhang is the halo the reference
        // does not have; the clip above is what removes it.
        let escaping = out
            .iter()
            .filter(|q| !site.rect.contains(q.rect.min) || !site.rect.contains(q.rect.max))
            .count();
        assert!(
            escaping > out.len() / 2,
            "only {escaping}/{} sparks cross the rim — the path is not on the rim any more",
            out.len()
        );
        let worst = out
            .iter()
            .map(|q| site.rect.min.x - q.rect.min.x)
            .fold(f32::MIN, f32::max);
        assert!(
            (worst - ramp(STAR, 0.0) * site.diag).abs() < 0.01,
            "the newborn star should hang exactly half its width past the left rim, not {worst}"
        );

        // A scroll-framed ancestor narrows the viewport; it never widens it.
        let scrolled = ShineSite {
            clip: Some(Rect::new(0.0, 0.0, 90.0, 200.0)),
            ..site.clone()
        };
        let mut out = Vec::new();
        site_quads(&scrolled, 0, &mut out);
        assert!(out
            .iter()
            .all(|q| q.clip == Some(Rect::new(72.0, 56.0, 90.0, 86.0))));

        // Scrolled fully out of view, the site draws nothing at all.
        let hidden = ShineSite {
            clip: Some(Rect::new(0.0, 0.0, 10.0, 10.0)),
            ..site
        };
        let mut out = Vec::new();
        site_quads(&hidden, 0, &mut out);
        assert!(out.is_empty());
    }

    /// The two projections and the spin, pinned where they can't drift back into one another
    /// (1386, corrected by 1390): the bone square is 1280/model-unit × the Model's 1.2 at every
    /// aspect, the star is an eye-space length that takes the screen diagonal and neither scale,
    /// and every particle spins the same way at `spin · age`.
    #[test]
    fn the_star_skips_the_models_scale_and_every_spin_turns_one_way() {
        // The bone square: 1280 per model unit and the Model's 1.2, at EVERY aspect (C2).
        assert!(
            (s12() - 30.72).abs() < 1e-4,
            "the bone square DOES take the 1.2"
        );
        // The star: `2 · half · √(W² + H²)` window px, no 1.2 and no UI scale — so unlike the
        // square it MOVES with the window's shape (C3). 1024×768 is the case 1386 mistook for
        // the whole law; 1280×720 is 22 % wider on a button 4 % narrower.
        let width = |w: f32, h: f32| 2.0 * STAR[0] * (w * w + h * h).sqrt();
        assert!((width(1024.0, 768.0) - 12.80).abs() < 0.01, "4:3");
        assert!((width(1280.0, 720.0) - 14.69).abs() < 0.01, "16:9");
        assert!(
            width(1280.0, 720.0) > 1.14 * width(1024.0, 768.0),
            "the aspect term is what 1386 froze out; it must be live"
        );
        // The square is 2.4 star-widths across at 4:3 — 1321's 2.0 was the 1.2 leaking into the
        // star. It is only 2.1 at 16:9, and that drift IS the law, not a rounding.
        assert!((s12() / width(1024.0, 768.0) - 2.4).abs() < 1e-3);

        let site = ShineSite {
            rect: Rect::new(0.0, 0.0, 30.0, 30.0),
            z: 0,
            clip: None,
            alpha: 1.0,
            scale: 1.0,
            model_scale: 1.2,
            diag: 1280.0, // 1024×768: s = 1 and the diagonal is 1280 — the law's 4:3 case
            texture: Handle::default(),
        };
        let mut out = Vec::new();
        site_quads(&site, 0, &mut out);
        // Rotation is a pure function of age, one direction, reaching 0.1 rad at the end of life.
        assert_eq!(out[0].rotation, 0.0, "a newborn star has not turned yet");
        let oldest = &out[PARTICLES.len() - 1];
        assert!(
            (oldest.rotation - SPIN * (1.0 - 1.0 / RATE)).abs() < 1e-6,
            "the oldest star has turned spin × its age"
        );
        assert!(
            out.iter().all(|q| q.rotation >= 0.0),
            "the sign-flip bit is clear on this asset — nothing counter-rotates"
        );
    }

    /// The corner walk and the four bones' phasing — wow-re's own bone table
    /// (`modelframe-animation-clock.md` §6): one clockwise cycle BL→TL→TR→BR walked by all four,
    /// each successive bone index LAGGING the last by 500 ms, so at clock 0 bone 0 sits at BL,
    /// bone 1 at BR, bone 2 at TR and bone 3 at TL.
    #[test]
    fn the_bones_walk_one_clockwise_cycle_each_lagging_the_last() {
        assert_eq!(point(0.0, s12()), (0.0, 0.0), "bottom-left at 0 s");
        assert_eq!(point(0.25, s12()), (0.0, s12()), "top-left at 0.5 s");
        assert_eq!(point(0.5, s12()), (s12(), s12()), "top-right at 1.0 s");
        assert_eq!(point(0.75, s12()), (s12(), 0.0), "bottom-right at 1.5 s");
        assert_eq!(point(1.0, s12()), (0.0, 0.0), "home at 2.0 s");

        // Where each emitter's newborn particle sits at clock 0, straight off the producer.
        let site = ShineSite {
            rect: Rect::new(0.0, 0.0, 30.0, 30.0),
            z: 0,
            clip: None,
            alpha: 1.0,
            scale: 1.0,
            model_scale: 1.2,
            diag: 1280.0, // 1024×768: s = 1 and the diagonal is 1280 — the law's 4:3 case
            texture: Handle::default(),
        };
        let mut out = Vec::new();
        site_quads(&site, 0, &mut out);
        let born = |e: usize| {
            let r = out[e * PARTICLES.len()].rect;
            (
                (r.min.x + r.max.x) * 0.5,
                site.rect.max.y - (r.min.y + r.max.y) * 0.5,
            )
        };
        let near = |got: (f32, f32), want: (f32, f32), what: &str| {
            assert!(
                (got.0 - want.0).abs() < 0.01 && (got.1 - want.1).abs() < 0.01,
                "{what}: at {got:?}, expected {want:?}"
            );
        };
        near(born(0), (0.0, 0.0), "bone 0 opens at BL");
        near(
            born(1),
            (s12(), 0.0),
            "bone 1 opens at BR — a quarter lap BEHIND bone 0",
        );
        near(born(2), (s12(), s12()), "bone 2 opens at TR");
        near(born(3), (0.0, s12()), "bone 3 opens at TL");
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

    /// The ramps read the M2's keys through the reference's own two-segment sampler — including
    /// its `t = u·0.99 + 0.005` inset, which means the authored endpoints are approached and
    /// never reached. Pinned as inequalities plus the inset's exact arithmetic, so the law is
    /// what is asserted rather than three literals.
    #[test]
    fn the_ramps_read_the_m2s_keys_through_the_references_inset() {
        let near = |got: f32, want: f32| assert!((got - want).abs() < 1e-4, "{got} vs {want}");
        // Segment-local time 0 lands 0.5 % in; segment-local time 1 lands 0.5 % short. The
        // reference's own branch is `age > midPoint·lifespan`, so age == mid is still the FIRST
        // segment — its 0.995 end, not the second segment's 0.005 start.
        near(ramp(STAR, 0.0), STAR[0] + (STAR[1] - STAR[0]) * 0.005);
        near(ramp(STAR, MID), STAR[0] + (STAR[1] - STAR[0]) * 0.995);
        near(ramp(STAR, 1.0), STAR[1] + (STAR[2] - STAR[1]) * 0.995);
        assert!(ramp(STAR, 0.0) < STAR[0], "the newborn star never hits k0");
        assert!(ramp(STAR, 1.0) > STAR[2], "the dying star never reaches k2");
        // The size ramp only ever shrinks, and the alpha only ever fades after the mid key —
        // the two facts the population's brightness envelope rests on.
        let mut prev = f32::INFINITY;
        for k in 0..=100 {
            let w = ramp(STAR, k as f32 / 100.0);
            assert!(w < prev, "the size ramp must shrink monotonically");
            prev = w;
        }
        // Alpha holds at the authored 1.0 across the whole first segment (k0 and k1 are both
        // opaque) and only fades over the second — the reason the band's bright half is its
        // freshest half and the wave reads as travelling.
        near(ramp_color(0.0)[3], 1.0);
        near(ramp_color(MID)[3], 1.0);
        assert!(ramp_color(1.0)[3] < 0.01, "a dying particle is ~invisible");
    }
}
