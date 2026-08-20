//! The standing dev HUD: the cost pill, and nothing else.
//!
//! **The pill is two small numbers** (1448 pared it back; 1454 made it the whole HUD; 1455
//! dropped the spike arrow). fps dim — the familiar anchor, and by construction the number that
//! cannot see cost (0717) — then process-CPU cost per frame, the meter vsync cannot rail; both
//! under-size, because the pill sits over the game all session. Anything deeper is an
//! instrument's job, not a panel's: the journal (`WOW_FPS_JOURNAL`), the probes, Tracy, the
//! `frame hitch` log line and the stall self-sampler. The expanded egui readout that used to
//! live behind a click is gone (1454) — it duplicated the instruments at a standing cost — and
//! the spike-latch arrow went the same way (1455, the director's call).
//!
//! **Drawn from a 4 Hz snapshot, not the live meters.** The pill renders from [`PerfHud::snap`];
//! the meters keep sampling every frame — only the *view* is quantized, and 250 ms is inside a
//! human read of a number.
//!
//! **The pill does not touch egui at all** (1453). Drawing anything through egui wakes the whole
//! lane — bevy_egui's per-frame pipeline plus a full-screen compositing camera — which the
//! director's live toggle priced at ~1 ms for a 20-glyph pill. So [`pill_quads`] lays the pill
//! onto the player-UI quad pass, whose append lane is rebuilt every frame anyway; since 1454 the
//! HUD has no egui surface left and [`crate::debug_panel`]'s 1445 gate no longer consults it.
//!
//! **The HUD does not own settings.** It briefly carried a VSync checkbox and an MSAA readout;
//! VSync is a player video option now ([`crate::video`], the Graphics page's `gxVSync` row) and
//! MSAA is a startup knob (`$WOW_MSAA`). An instrument reports — it does not double as the control
//! panel, and this one is `#[cfg(feature = "dev")]`, so anything a player must reach cannot live
//! here at all. The measurement knob the checkbox actually existed for is `$WOW_NOVSYNC=1`.

use benilla_ui::script::{JustifyH, JustifyV, Outline};
use bevy::prelude::*;
use bevy::time::Real;
use bevy::window::{PrimaryWindow, Window};

use super::stats::FrameStats;
use crate::ui_pass::{UiQuad, UiQuads};
use crate::ui_text::{layout_text_quads, FontSpec, Justify, UiFontAtlas};

// ---- The quad pill (1453): the standing readout, drawn on the player-UI pass. --------------
// The old egui overlay palette, restated as client-space sRGB floats for [`UiQuad::color`]
// (black alpha 224 fill; gray 235 text; gray 180 dim).
const Q_FILL: [f32; 4] = [0.0, 0.0, 0.0, 224.0 / 255.0];
const Q_TEXT: [f32; 4] = [0.92, 0.92, 0.92, 1.0];
/// `|cAARRGGBB` markup for the dim fps run (gray 180).
const Q_DIM_MARKUP: &str = "|cffb4b4b4";
/// Paint order: the pill background one under the glyphs, both above every packed WoW z_key
/// (frame tuples never fill the top bits).
const Z_PILL_BG: u64 = u64::MAX - 1;
const Z_PILL: u64 = u64::MAX;
/// The quad pill's font height (logical px) and box padding.
const PILL_QUAD_PX: f32 = 12.0;
const PILL_PAD: Vec2 = Vec2::new(9.0, 4.0);
/// Top-center offset.
const PILL_TOP: f32 = 8.0;

/// How often the drawn snapshot advances. Fast enough that the numbers read as live and a latched
/// badge appears within a perceptual beat; slow enough that between refreshes the pill's cached
/// quads are reused byte-identical (see [`pill_quads`]).
const HUD_REFRESH_SECS: f32 = 0.25;

/// HUD state. The **dev chord + `P`** toggles `visible` (default on — it's a standing dev
/// surface). `visible` is `pub(crate)` so the capture harness ([`crate::capture`]) can force the
/// overlay off for pristine, UI-free screenshots.
///
/// **`WOW_PERF_HUD=0` starts it hidden**, which is how the HUD gets priced. 1370 records the open
/// gap: every campaign anchor is measured on a binary that is drawing this overlay, at a cost
/// booked as "est 0.4–1.2 ms CPU + unquantified GPU" — an estimate, never a measurement, because
/// nothing could turn the fixture off without also changing the binary. One env var makes it an
/// interleaved A/B on *one* binary instead (`scripts/leg.sh`), so the constant baked into every
/// anchor becomes a number. The meters keep sampling either way: only the drawing stops, which is
/// the half being priced.
#[derive(Resource)]
pub(crate) struct PerfHud {
    pub(crate) visible: bool,
    /// The snapshot the pill draws from — a copy of the meters taken every [`HUD_REFRESH_SECS`],
    /// so the strings it formats hold still between refreshes (the module doc's cost argument).
    /// The live [`FrameStats`] keeps sampling every frame; this is only the view.
    snap: FrameStats,
    /// The clock `snap` was taken at. Spike ages are computed against this, not the live clock,
    /// so they hold still with the rest of the view — and it doubles as the refresh timer.
    snap_at: f32,
}

impl Default for PerfHud {
    fn default() -> Self {
        Self {
            visible: std::env::var("WOW_PERF_HUD").as_deref() != Ok("0"),
            snap: FrameStats::default(),
            // −∞, so the very first frame refreshes rather than drawing an empty snapshot.
            snap_at: f32::NEG_INFINITY,
        }
    }
}

impl PerfHud {
    /// Advance the snapshot if the current one is older than [`HUD_REFRESH_SECS`].
    fn maybe_refresh(&mut self, stats: &FrameStats, now: f32) {
        if now - self.snap_at >= HUD_REFRESH_SECS {
            self.snap = stats.clone();
            self.snap_at = now;
        }
    }
}

pub(super) fn toggle_hud(keys: Res<ButtonInput<KeyCode>>, mut hud: ResMut<PerfHud>) {
    // The dev chord + `P`, not a bare `p` — `P` is the reference's TOGGLESPELLBOOK, and a dev
    // doesn't get to squat on a game binding (decision 0585). The chord can't be mistaken for typed
    // text, so unlike the old bare key it needs no chat-bar/EditBox gate.
    if benilla_world::modkeys::dev_chord(&keys, KeyCode::KeyP) {
        hud.visible = !hud.visible;
    }
}

/// Advance the HUD's 4 Hz snapshot — its own system (1453), kept separate so the sampling cadence
/// never depends on whether anything drew this frame.
pub(super) fn refresh_hud_snapshot(
    mut hud: ResMut<PerfHud>,
    stats: Res<FrameStats>,
    time: Res<Time<Real>>,
) {
    if hud.visible {
        let now = time.elapsed_secs();
        hud.maybe_refresh(&stats, now);
    }
}

/// The pill as ~20 quads on the player-UI pass (decision 1453). The append lane is rebuilt every
/// frame anyway, so the marginal cost is the clone of a cached Vec — where the old egui pill woke
/// bevy_egui's whole pipeline plus a full-screen compositing camera (~1 ms on the director's live
/// toggle). Glyphs are laid out only when the snapshot ticks or the window resizes.
pub(super) fn pill_quads(
    hud: Res<PerfHud>,
    atlas: Option<Res<UiFontAtlas>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    mut quads: ResMut<UiQuads>,
    mut cache: Local<Option<PillCache>>,
) {
    if !hud.visible {
        return;
    }
    let (Some(atlas), Ok(win)) = (atlas, windows.single()) else {
        return; // headless: nothing draws, nothing to price
    };
    let win_w = win.width();
    let stale = !matches!(&*cache, Some(c) if c.snap_at == hud.snap_at && c.win_w == win_w);
    if stale {
        let cpu = hud.snap.cpu.mean();
        let fps = hud.snap.fps();
        // One string, the dim run via markup: "59 fps  8.5 ms" — fps dim, cost in full text.
        let text = match cpu {
            Some(cpu) => format!("{Q_DIM_MARKUP}{fps:.0} fps|r  {cpu:.1} ms"),
            None => format!("{Q_DIM_MARKUP}-- ms"),
        };
        let center = Vec2::new(win_w * 0.5, 0.0); // measured first, then shifted under PILL_TOP
        let mut e = atlas.lock();
        let glyphs = layout_text_quads(
            &mut e,
            &text,
            Rect::from_center_size(center, Vec2::ZERO),
            Q_TEXT,
            Justify {
                h: JustifyH::Center,
                v: JustifyV::Middle,
            },
            Z_PILL,
            FontSpec {
                path: None,
                height: Some(PILL_QUAD_PX),
                outline: Outline::None,
                alpha_gradient: None,
            },
        );
        drop(e);
        let bounds = glyphs
            .iter()
            .map(|q| q.rect)
            .reduce(|a, b| a.union(b))
            .unwrap_or(Rect::from_center_size(center, Vec2::ZERO));
        let strip = Rect::new(
            bounds.min.x - PILL_PAD.x,
            bounds.min.y - PILL_PAD.y,
            bounds.max.x + PILL_PAD.x,
            bounds.max.y + PILL_PAD.y,
        );
        // Measured about y=0; seat the whole strip at PILL_TOP.
        let dy = PILL_TOP - strip.min.y;
        let shift = |r: Rect| Rect::new(r.min.x, r.min.y + dy, r.max.x, r.max.y + dy);
        let strip = shift(strip);
        let mut out = vec![UiQuad {
            rect: strip,
            z_key: Z_PILL_BG,
            color: Q_FILL,
            ..Default::default()
        }];
        for mut q in glyphs {
            q.rect = shift(q.rect);
            out.push(q);
        }
        *cache = Some(PillCache {
            snap_at: hud.snap_at,
            win_w,
            quads: out,
        });
    }
    let Some(c) = &*cache else { return };
    quads.overlays.extend(c.quads.iter().cloned());
}

/// [`pill_quads`]' cache: the laid-out pill, valid for one snapshot tick at one window width.
pub(super) struct PillCache {
    snap_at: f32,
    win_w: f32,
    quads: Vec<UiQuad>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The view the HUD draws only advances on the refresh interval — between ticks it holds
    /// still, which is the entire cost argument (identical snapshots are what the quad cache can
    /// reuse) — and a refresh adopts the live meters wholesale.
    #[test]
    fn the_snapshot_advances_on_the_interval_not_per_frame() {
        let mut live = FrameStats::default();
        let frames: Vec<_> = (0..60).map(|_| (16.6, 20.0)).collect();
        let t = live.feed_frames(&frames, 0.0, 1.0 / 60.0);

        let mut hud = PerfHud::default();
        hud.maybe_refresh(&live, t);
        assert_eq!(
            hud.snap.wall.len(),
            60,
            "the first refresh adopts the meters"
        );
        assert_eq!(hud.snap_at, t);

        let t2 = live.feed_frames(&[(40.0, 20.0)], t, 1.0 / 60.0);
        hud.maybe_refresh(&live, t2);
        assert_eq!(
            hud.snap.wall.len(),
            60,
            "one frame later — inside the interval — the view holds still"
        );

        hud.maybe_refresh(&live, t + HUD_REFRESH_SECS);
        assert_eq!(
            hud.snap.wall.len(),
            61,
            "past the interval the view catches up"
        );
    }
}
