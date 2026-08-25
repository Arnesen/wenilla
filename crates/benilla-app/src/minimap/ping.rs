//! The minimap **ping** (decision 1596; the feature 0471 paused and this brings back).
//!
//! ## The pin
//!
//! A ping marks a **place in the world**, so a world point `(x, y)` is the only thing this module
//! stores. Where it lands on screen is *derived*, every frame, by [`BlipCtx::offset`] — the exact
//! same function the party dots, the quest dots and the corpse blip go through. It therefore
//! cannot drift from the map, cannot lag the pan by a frame, and cannot survive a zoom change at
//! the old scale: there is no second copy of the position to fall out of step with.
//!
//! That is the whole difference from the first attempt (decision 0453 / 0471), which stored the
//! world point in the engine but drew the marker from **Lua** — a `MiniMapPing` frame re-seated by
//! `Minimap_OnUpdate` through `SetPoint` from a normalized offset the app pushed. Four
//! independent ways to be wrong, and it was wrong in three of them; 1596 §2 has the autopsy.
//!
//! ## The three legs
//!
//! - **In** — a click reaches Lua's `Minimap_OnClick` (ours, and hookable: the corpus's
//!   `CleanMinimap` replaces that global outright), which calls `Minimap:PingLocation(dx, dy)`
//!   with centre-relative offsets in **UI units**. [`emit_ping`]'s caller drains it in the *same
//!   frame it draws the map*, converting through that frame's own geometry: UI units × the 0582
//!   seam scale = window px, ÷ `px_per_yd` = yards. (Skipping that seam multiply is what put the
//!   first version's ping ~27 % too far from the player at 1080p.)
//! - **Across** — our own ping sends `MSG_MINIMAP_PING` (raw world floats; the server relays them
//!   verbatim to the rest of the group and nowhere else). A group member's arrives through the
//!   session event and seats the same way. A ping is drawn **locally at click time**, never waited
//!   for off the wire: vanilla pings work solo.
//! - **Out** — `MINIMAP_PING (unitToken, nx, ny)` fires for addons, with the same normalized
//!   offsets the byte-verified relay `0x4ee330` hands Lua (`(−dy·k, dx·k)`, `k = 1/(2·radius)` —
//!   wow-re `party-group-wire.md` §TU-D). `Minimap:GetPingPosition()` reads the live value back.
//!
//! ## Lifetime
//!
//! 5 s hold, then the reference's 0.5 s "fade" — see [`PING_ALPHA_IS_A_POP`]. A map change drops
//! it (the point is not here any more). **Nothing else clears it**, and in particular *not*
//! proximity: the first version applied the client's 10-yd `d² < 100` auto-clear to the party
//! ping, and that clear belongs to the **`SMSG_GOSSIP_POI` marker** — a different feature in a
//! different slot (wow-re `party-group-wire.md` §TU-D corrects it explicitly; `MSG_MINIMAP_PING`
//! has no C-side storage at all). Walking to your own ping used to delete it mid-hold.

use bevy::prelude::*;

use benilla_assets::coords::bevy_to_wow;
use benilla_ui::script::{ScriptValue, UiScript};

use super::blips::BlipCtx;
use crate::net::{ClientCommand, Guid, NetCommands, SelfPlayer};
use crate::player::Player;
use crate::ui_pass::{UiQuad, UiQuads};

/// How long the ping holds at full strength — the reference's `MINIMAPPING_TIMER`.
const PING_HOLD: f32 = 5.0;
/// The tail after the hold — the reference's `MINIMAPPING_FADE_TIMER`.
const PING_FADE: f32 = 0.5;

/// **The reference's ping does not fade; it pops** — now byte-confirmed, not inferred from the Lua
/// (wow-re `minimap-ping-law.md`). `Minimap_OnUpdate` writes `SetAlpha(255 * (t / 0.5))`, and
/// `Frame:SetAlpha` (`0x774e90`) **clamps to [0,1]** before scaling to a 0–255 byte — so the
/// nominal half-second ramp is at full alpha for 498 of its 500 ms and the whole visible fall is
/// ~12 % of one frame at 60 fps. The real client holds bright for ~5 s and vanishes.
///
/// Reproduced as the clamp rather than as a constant 1.0, so the mechanism stays visible and the
/// `false` branch is a real alternative (a smooth fade, which wow-re notes will visibly diverge).
const PING_ALPHA_IS_A_POP: bool = true;

/// **The marker, byte-measured** (wow-re `system/ui/scratch/minimap-ping-law.md` §10, VERIFIED).
///
/// `MiniMapPing` is a `<Model>` on `Interface\MiniMap\Ping\MinimapPing.mdx` at XML `scale="0.4"`,
/// which puts **1 model unit at 512 px** on the client's stock basis. The model is *five coincident
/// full-UV quads* sharing one centre, every one of them `blendMode = 4` — **additive**
/// `SRC_ALPHA/ONE` — with `flags = 0x0011` (unlit, no depth write). Three of them draw:
///
/// | texture | model units | px | behaviour |
/// |---|---|---|---|
/// | `ping5` | `0.069 × 0.350634` | **12.39** | spins CW, one turn per 4833 ms, alpha 1 |
/// | `ping2` | `0.025` | **12.80** | fully static — its bone has no tracks at all |
/// | `ping4` | `0.00625 × (1→10)` | **3.20 → 32.00** | the expanding, fading ring |
///
/// `ping6` (two of the five quads) is **culled, not invisible**: its weight track is a single key
/// of 0, so the batch is skipped before `blendMode` is read. It is not emitted here.
///
/// Sizes are px on the frozen 140.8-px minimap basis, like every other blip constant
/// ([`super::blips::BLIP_BASIS_PX`]) — the model's px-per-unit and the widget's size both scale
/// with the same screen basis, so their ratio is the constant.
///
/// The paused first version drew one flat 40 px stack: **~3× too big, and missing the ring** —
/// which is the motion the eye actually reads.
const PING5_PX: f32 = 0.069 * 0.350_634 * PX_PER_MODEL_UNIT;
const PING2_PX: f32 = 0.025 * PX_PER_MODEL_UNIT;
const PING4_PX: f32 = 0.006_25 * PX_PER_MODEL_UNIT;

/// `scale="0.4"` on a `<Model>` frame ⇒ `(5/3) · 0.4 · 768` px per model unit at the reference's
/// screen basis (aspect- and resolution-independent — the terms cancel).
const PX_PER_MODEL_UNIT: f32 = 512.0;

/// The **sequence** clock: sequence 1 "Stand", looped. `ping4`'s ring rides this.
const PING_SEQ_MS: f32 = 833.0;
/// The **global** clock (`globalSequence = 0`), which never consults the sequence window at all.
/// `ping5`'s spin rides this — the two periods are coprime, so they re-phase only after ~67 min.
const PING_SPIN_MS: f32 = 4833.0;

/// `ping4`'s alpha: linear `0 → 1` over the first 400 ms of the loop, then `1 → 0` over the
/// remaining 433 ms. Peak alpha lands at 400 ms, when the ring is `1 + 9·400/833 = 5.32×`.
const PING4_ALPHA_PEAK_MS: f32 = 400.0;

/// `ping5`'s spin, as the model's own 21 keys — `(ms, degrees clockwise on screen)`, unwrapped and
/// monotonic (wow-re §10.3). The rate is **not** uniform: ~67.5 °/s over the first half-turn,
/// ~83.1 °/s over the second, with a 108 °/s burst across 225°→270° that an average hides.
///
/// Kept as the table rather than collapsed to a constant rate for exactly that reason. wow-re's
/// three transcription warnings do not bite here — they are about interpolating the model's
/// *quaternions* (a shortest-path slerp collapses the revolution to no motion, and a `w ≥ 0`
/// canonicalisation reverses it at 180°). Interpolating the **angle** linearly, as below, is the
/// sanctioned form: it deviates by at most 0.014°.
const PING5_SPIN: [(f32, f32); 21] = [
    (0.0, 0.0),
    (333.0, 22.5),
    (667.0, 45.0),
    (889.0, 60.0),
    (1111.0, 75.0),
    (1333.0, 90.0),
    (1666.0, 112.5),
    (2000.0, 135.0),
    (2222.0, 150.0),
    (2444.0, 165.0),
    (2667.0, 180.0),
    (2847.0, 195.0),
    (3027.0, 210.0),
    (3208.0, 225.0),
    (3416.0, 247.5),
    (3625.0, 270.0),
    (3805.0, 285.0),
    (3986.0, 300.0),
    (4167.0, 315.0),
    (4500.0, 337.5),
    (4833.0, 360.0),
];

/// The spin angle at `ms` into the global clock, in **radians clockwise on screen** — which is our
/// quad `rotation`'s own sense (the player arrow negates a WoW facing for the same reason).
fn spin_radians(ms: f32) -> f32 {
    let t = ms.rem_euclid(PING_SPIN_MS);
    let deg = PING5_SPIN
        .windows(2)
        .find(|w| t <= w[1].0)
        .map_or(0.0, |w| {
            let (t0, a0) = w[0];
            let (t1, a1) = w[1];
            let u = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            a0 + (a1 - a0) * u
        });
    deg.to_radians()
}

/// The **art**, one handle per drawn layer. Named rather than a list: the three do different
/// things, and a `Vec` that silently changed order would change the animation.
#[derive(Default)]
pub(super) struct PingArt {
    /// The spinner (draw order 0 in the model — though with additive and no depth write, order
    /// cannot change the image).
    pub(super) ping5: Option<Handle<Image>>,
    /// The static centre.
    pub(super) ping2: Option<Handle<Image>>,
    /// The expanding ring.
    pub(super) ping4: Option<Handle<Image>>,
}

/// The live ping. One at a time — the reference keeps no list either.
struct LivePing {
    /// **The pin**: the WoW `(x, y)` this ping marks. The only stored position; the screen seat is
    /// re-derived from it every frame.
    world: (f32, f32),
    /// The map it was placed on. A map change drops the ping rather than re-projecting a point
    /// that does not exist here.
    map: u32,
    /// Seconds since it was seated.
    age: f32,
    /// The pinger's guid, `0` = ourselves — resolved to the `MINIMAP_PING` event's unit token, and
    /// the test for "this is ours, put it on the wire".
    sender: u64,
}

/// The engine-owned ping state (decision 1596). Seated by a click (drained in the renderer, with
/// that frame's geometry) or by a group member's `MSG_MINIMAP_PING`; aged, announced and expired
/// by [`drive_minimap_ping`].
#[derive(Resource, Default)]
pub(crate) struct MinimapPing {
    live: Option<LivePing>,
    /// The model's own clock, in **milliseconds, accumulated only while a ping is shown** — which
    /// is the reference's behaviour rather than a simplification of it. `SetSequence(0)` runs once
    /// in the ref's `OnLoad` and stores an *anchor*; the sampler free-runs from it, and a Model
    /// frame's private clock advances only while the frame is shown. The ref then only `Show()`s
    /// per ping. So **ping N resumes where ping N−1 left off**, and no two consecutive pings look
    /// alike (wow-re §10, VERIFIED). One accumulator reproduces that; a per-ping reset would not.
    shown_ms: f32,
    /// A ping seated since the last [`drive_minimap_ping`] — it still owes the world an outbound
    /// `MSG_MINIMAP_PING` (if it is ours) and a `MINIMAP_PING` event (either way).
    fresh: bool,
}

impl MinimapPing {
    /// Seat a ping at a world point. Re-pinging replaces: the reference tolerates the same, and a
    /// group echo of our own click lands on the spot we already drew (`Minimap_SetPing` twice on
    /// one spot just restarts the timer).
    pub(crate) fn seat(&mut self, world: (f32, f32), map: u32, sender: u64) {
        self.live = Some(LivePing {
            world,
            map,
            age: 0.0,
            sender,
        });
        self.fresh = true;
    }

    /// The ping's alpha at its current age, or `None` once it is over.
    fn alpha(&self) -> Option<f32> {
        let age = self.live.as_ref()?.age;
        if age <= PING_HOLD {
            return Some(1.0);
        }
        let tail = (PING_HOLD + PING_FADE - age) / PING_FADE;
        if tail <= 0.0 {
            None
        } else if PING_ALPHA_IS_A_POP {
            // The reference's `255 * tail`, clamped by SetAlpha's own 0..1 — full until the last
            // ~2 ms. Written as the clamp rather than as a constant 1.0 so the mechanism is
            // visible and the `false` branch is a real alternative, not a rewrite.
            Some((255.0 * tail).min(1.0))
        } else {
            Some(tail)
        }
    }
}

/// Convert a `Minimap:PingLocation(x, y)` click into the world point it names, drain-side.
///
/// `ui` is centre-relative in **UI units** (x right, y up — `GetCursorPosition()`'s space);
/// `seam` is window px per UI unit ([`crate::ui_script::seam_scale`]), and `ctx` is the geometry
/// of the map **as drawn this frame**. The mapping is [`BlipCtx::offset`]'s inverse: screen right
/// = −WoW y (west), screen up = +WoW x (north).
///
/// `None` when the click is outside the disc — the reference's `Minimap_OnClick` makes the same
/// test in Lua (`sqrt(x² + y²) < width/2`), stated here in yards because that is the space the
/// answer lives in.
fn click_to_world(ctx: &BlipCtx, ui: (f32, f32), seam: f32) -> Option<(f32, f32)> {
    if ctx.px_per_yd <= 0.0 || seam <= 0.0 {
        return None;
    }
    let right_yd = ui.0 * seam / ctx.px_per_yd;
    let up_yd = ui.1 * seam / ctx.px_per_yd;
    if right_yd.hypot(up_yd) >= ctx.radius_yd {
        return None;
    }
    Some((ctx.wx + up_yd, ctx.wy - right_yd))
}

/// Seat this frame's `Minimap:PingLocation` click and draw the live ping — both inside the
/// renderer, against the geometry the player actually clicked on and the map actually drew at.
///
/// The seat happens here rather than in a system of its own precisely so there is no window in
/// which a click is held against a *stale* view scale: the first version parked the click for a
/// separate system that read the scale the renderer had left behind on the previous frame, and
/// dropped the click outright whenever that leftover was still zero. (The *drain* is the caller's,
/// one step earlier, so the click is spent even on a frame that draws no map — see there.)
pub(super) fn emit_ping(
    ctx: &BlipCtx,
    ping: &mut MinimapPing,
    click: Option<(f32, f32)>,
    map: u32,
    art: &PingArt,
    quads: &mut UiQuads,
) {
    if let Some(world) = click.and_then(|c| click_to_world(ctx, c, ctx.seam)) {
        ping.seat(world, map, 0);
    }

    let Some(alpha) = ping.alpha() else { return };
    let Some(live) = ping.live.as_ref() else {
        return;
    };
    let (px, py) = live.world;

    // The reference's Lua hides the marker outside the disc and keeps the ping alive
    // (`Minimap_SetPing`'s else-branch is `MiniMapPing:Hide()`, not a clear) — so walking back
    // into range brings it back for the rest of its 5 s.
    let d = (px - ctx.wx).hypot(py - ctx.wy);
    if d >= ctx.radius_yd {
        return;
    }
    let at = ctx.center + ctx.offset([px, py, 0.0]);
    // px on the frozen 140.8 basis → this widget's px, the blip layer's own scalar.
    let k = ctx.side / super::blips::BLIP_BASIS_PX;
    let a = alpha * ctx.alpha;
    let mut layer = |art: &Option<Handle<Image>>, side_px: f32, alpha: f32, rotation: f32| {
        let Some(texture) = art.clone() else { return };
        if alpha <= 0.0 {
            return;
        }
        quads.overlays.push(UiQuad {
            rect: Rect::from_center_size(at, Vec2::splat(side_px * k)),
            z_key: ctx.z,
            texture: Some(texture),
            color: [1.0, 1.0, 1.0, alpha],
            // Every one of the model's quads is `blendMode = 4`, SRC_ALPHA/ONE.
            additive: true,
            rotation,
            ..default()
        });
    };

    // The spinner, on the 4833 ms global clock.
    layer(&art.ping5, PING5_PX, a, spin_radians(ping.shown_ms));
    // The static centre.
    layer(&art.ping2, PING2_PX, a, 0.0);
    // The ring, on the 833 ms sequence loop: geometry scale `1 + 9·t/833` about its own centre
    // (the model carries NO texture-transform chunk, so this is a real size change, not a UV one),
    // under a two-leg alpha that peaks 400 ms in.
    let t = ping.shown_ms.rem_euclid(PING_SEQ_MS);
    let ring_alpha = if t <= PING4_ALPHA_PEAK_MS {
        t / PING4_ALPHA_PEAK_MS
    } else {
        (PING_SEQ_MS - t) / (PING_SEQ_MS - PING4_ALPHA_PEAK_MS)
    };
    layer(
        &art.ping4,
        PING4_PX * (1.0 + 9.0 * t / PING_SEQ_MS),
        a * ring_alpha,
        0.0,
    );
}

/// Age the ping, announce a fresh one, and expire it — everything that is *not* geometry.
///
/// Runs before the script tick so the `MINIMAP_PING` event and the position behind
/// `Minimap:GetPingPosition()` land in the same tick, and so an addon's handler sees a ping that
/// is already on screen (the renderer seated and drew it at the end of the previous frame).
#[allow(clippy::too_many_arguments)] // one Bevy system's full input set
pub(super) fn drive_minimap_ping(
    script: Option<bevy::ecs::system::NonSendMut<UiScript>>,
    mut ping: ResMut<MinimapPing>,
    time: Res<Time>,
    player: Res<Player>,
    map: Option<Res<benilla_world::world_map::CurrentMap>>,
    widget: Res<super::MinimapWidget>,
    inside: Res<super::MinimapInside>,
    group: Res<crate::ui_party::GroupState>,
    self_q: Query<&Guid, With<SelfPlayer>>,
    commands: Res<NetCommands>,
) {
    // Ageing and expiry run before the VM guard: a ping seated with no UI up (the wire can land
    // one across a world-enter) must still run out, rather than waiting to start its five seconds
    // whenever a VM next appears.
    if let Some(live) = ping.live.as_mut() {
        live.age += time.delta_secs();
        // The model clock runs only while a ping is up — see `shown_ms`. (The reference stops it
        // on `Hide()`, which also covers the off-disc case; we keep it running there, a difference
        // of a few tenths of a phase on a marker nobody can see.)
        ping.shown_ms += time.delta_secs() * 1000.0;
    }
    // A map change drops it — a DELIBERATE divergence, now that the law is known. wow-re
    // `minimap-ping-law.md` §3 is categorical: **nothing** engine-side ever clears the reference's
    // ping (six instructions touch those cells; the only zeroing is a CRT static initializer before
    // `WinMain`), so a real 1.12 client that teleports mid-ping keeps drawing a marker at a world
    // point that is now somewhere else entirely. That is a consequence of storing the ping in a
    // never-cleared global, not a behaviour anyone chose, and the 5 s timer hides it in practice.
    // We drop it instead: same observable in every case a player can produce, minus the stale
    // marker after a worldport.
    if let (Some(live), Some(map)) = (ping.live.as_ref(), map.as_ref()) {
        if live.map != map.0 {
            ping.live = None;
        }
    }
    if ping.alpha().is_none() {
        ping.live = None;
    }
    if ping.live.is_none() {
        ping.fresh = false;
    }

    let Some(mut script) = script else { return };
    let Some(live) = ping.live.as_ref() else {
        script.set_minimap_ping(None);
        return;
    };

    // The normalized offsets, recomputed from the pin every tick against the live view radius —
    // the byte-verified relay's own `(−dy·k, dx·k)`, `k = 1/(2·radius)`. With the map hidden there
    // is no live index to read (the extract publishes no slot), so the event's numbers fall back
    // to the registered default zoom: an addon still hears the ping, at the scale the map would
    // have if it were up.
    let wow = bevy_to_wow(player.pos);
    let radius = super::view_radius_yd(
        widget
            .0
            .as_ref()
            .map_or(super::MINIMAP_DEFAULT_ZOOM, |s| s.zoom),
        widget
            .0
            .as_ref()
            .map_or(super::MINIMAP_DEFAULT_ZOOM, |s| s.inside_zoom),
        inside.0,
    );
    let k = 1.0 / (2.0 * radius);
    let norm = ((wow[1] - live.world.1) * k, (live.world.0 - wow[0]) * k);
    script.set_minimap_ping(Some(norm));

    if !std::mem::take(&mut ping.fresh) {
        return;
    }
    let Some(live) = ping.live.as_ref() else {
        return;
    };
    // Ours goes on the wire — raw world floats — but only when there is a group to relay them to.
    // The reference gates its send the same way (`PingLocation` `0x4eeca0` sends only when
    // grouped, VERIFIED) while still pinging locally, which is why a solo ping works at all and
    // why the marker is drawn at click time rather than awaited off the wire.
    if live.sender == 0 && group.in_group {
        let _ = commands.0.send(ClientCommand::MinimapPing {
            x: live.world.0,
            y: live.world.1,
        });
    }
    // The event's unit token: ourselves, or the sender's party slot. A sender we cannot resolve
    // (they left the group mid-flight) still pings — the reference's own Lua ignores arg1.
    let self_guid = self_q.iter().next().map(|g| g.0);
    let token = if live.sender == 0 || Some(live.sender) == self_guid {
        "player".to_string()
    } else {
        group
            .party_slots()
            .position(|m| m.guid == live.sender)
            .map_or_else(|| "party1".to_string(), |i| format!("party{}", i + 1))
    };
    script.fire_event(
        "MINIMAP_PING",
        vec![
            ScriptValue::Str(token),
            ScriptValue::Number(f64::from(norm.0)),
            ScriptValue::Number(f64::from(norm.1)),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `BlipCtx` for a 140-px-side map at a 100-yd view radius, player at the origin.
    fn ctx() -> BlipCtx {
        let side = 140.0;
        let radius = 100.0;
        BlipCtx {
            center: Vec2::new(500.0, 200.0),
            side,
            px_per_yd: (side * 0.5) / radius,
            radius_yd: radius,
            z: 0,
            alpha: 1.0,
            wx: 0.0,
            wy: 0.0,
            wz: 0.0,
            cursor: None,
            cursor_ui: None,
            seam: 1.0,
        }
    }

    /// **The first version's ping landed in the wrong place** (decision 1596 §2.1): the click
    /// arrives in UI units and the map's `px_per_yd` is in *window* px, and it divided one by the
    /// other. At the shipped default (0.9 uiScale on a 1080p window) the seam is ≈1.27, so every
    /// ping seated ≈27 % further from the player than the player clicked — worse the further out
    /// you clicked, which is exactly what "it pings somewhere else" looks like.
    #[test]
    fn a_click_converts_through_the_seam_scale() {
        let c = ctx();
        let seam = 1080.0 / 768.0 * 0.9; // the shipped default at 1080p
                                         // 20 UI units right of centre → 20·seam window px → ÷ px_per_yd yards WEST (−y).
        let (x, y) = click_to_world(&c, (20.0, 0.0), seam).expect("inside the disc");
        let expect_yd = 20.0 * seam / c.px_per_yd;
        assert!((x - 0.0).abs() < 1e-3, "no northing from a due-east click");
        assert!(
            (y + expect_yd).abs() < 1e-3,
            "screen right is WoW −y (west): {y} vs {}",
            -expect_yd
        );
        // The bug: dropping the seam multiply shortens every click by the same factor.
        let naive = 20.0 / c.px_per_yd;
        assert!(
            (expect_yd - naive).abs() > 5.0,
            "the seam is load-bearing, not a rounding difference"
        );
    }

    /// Screen up is WoW +x (north) — [`BlipCtx::offset`]'s inverse, so a ping seated from a click
    /// draws back under the cursor.
    #[test]
    fn a_click_round_trips_through_the_blip_mapping() {
        let c = ctx();
        let ui = (18.0, -25.0);
        let world = click_to_world(&c, ui, 1.0).expect("inside the disc");
        let back = c.offset([world.0, world.1, 0.0]);
        // `offset` is y-DOWN screen space; the click was y-up.
        assert!((back.x - ui.0).abs() < 1e-3, "{back:?} vs {ui:?}");
        assert!((back.y + ui.1).abs() < 1e-3, "{back:?} vs {ui:?}");
    }

    /// The reference's own disc test, in yards: a click outside the map's radius is not a ping.
    #[test]
    fn a_click_outside_the_disc_is_no_ping() {
        let c = ctx();
        // The disc is 70 px of the 140-px side; 69 px in is a ping, 71 px out is not.
        assert!(click_to_world(&c, (69.0, 0.0), 1.0).is_some());
        assert!(click_to_world(&c, (71.0, 0.0), 1.0).is_none());
        assert!(
            click_to_world(&c, (50.0, 50.0), 1.0).is_none(),
            "the corner"
        );
    }

    /// **The pin.** The stored form is a world point, so walking moves the marker across the map
    /// by exactly the player's displacement — no re-seating, no second copy to drift.
    #[test]
    fn the_marker_tracks_the_world_as_the_player_walks() {
        let mut c = ctx();
        let mut ping = MinimapPing::default();
        ping.seat((30.0, 0.0), 0, 0); // 30 yd north of the player
        let live = ping.live.as_ref().unwrap();
        let before = c.offset([live.world.0, live.world.1, 0.0]);
        assert!(before.y < 0.0, "north draws UP the screen: {before:?}");
        // Walk 10 yd north. The ping is now 20 yd away, so it draws 10 yd closer to the centre.
        c.wx += 10.0;
        let after = c.offset([live.world.0, live.world.1, 0.0]);
        assert!(
            (after.y - (before.y + 10.0 * c.px_per_yd)).abs() < 1e-3,
            "{before:?} → {after:?}"
        );
    }

    /// **No proximity clear** (decision 1596 §2.2). The first version applied the client's 10-yd
    /// `d² < 100` auto-clear to the party ping; wow-re `party-group-wire.md` §TU-D shows that
    /// clear belongs to the `SMSG_GOSSIP_POI` marker, and that `MSG_MINIMAP_PING` has no C-side
    /// storage to clear at all. Standing on your own ping must not delete it.
    #[test]
    fn reaching_the_ping_does_not_clear_it() {
        let mut ping = MinimapPing::default();
        ping.seat((1.0, 1.0), 0, 0);
        assert!(ping.alpha().is_some());
        // Age it well inside the hold, standing right on top of the point.
        ping.live.as_mut().unwrap().age = 2.0;
        assert_eq!(ping.alpha(), Some(1.0), "a reached ping still holds");
    }

    /// The hold, then the reference's clamped tail, then gone.
    #[test]
    fn the_ping_holds_five_seconds_and_pops() {
        let mut ping = MinimapPing::default();
        ping.seat((0.0, 0.0), 0, 0);
        for (age, want) in [(0.0, Some(1.0)), (4.9, Some(1.0)), (5.4, Some(1.0))] {
            ping.live.as_mut().unwrap().age = age;
            assert_eq!(ping.alpha(), want, "at {age}s");
        }
        // The last ~2 ms is the only part of the "fade" that is below full.
        ping.live.as_mut().unwrap().age = PING_HOLD + PING_FADE - 0.0005;
        let a = ping.alpha().expect("still alive");
        assert!(a > 0.0 && a < 1.0, "the pop's one dim frame: {a}");
        ping.live.as_mut().unwrap().age = PING_HOLD + PING_FADE;
        assert_eq!(ping.alpha(), None, "over");
    }

    fn art() -> PingArt {
        PingArt {
            ping5: Some(Handle::default()),
            ping2: Some(Handle::default()),
            ping4: Some(Handle::default()),
        }
    }

    /// **It draws, and where.** The pin's whole claim is that the marker's rect comes out of
    /// [`BlipCtx::offset`] like every other blip's — so this drives the real emitter and checks
    /// the rect, rather than trusting the caller. It also pins the two layers' byte-measured
    /// sizes, and the fact that the ring contributes **nothing** at phase 0 (its alpha track
    /// starts at zero, so a "three layers ⇒ three quads" assertion would be wrong).
    #[test]
    fn the_emitter_puts_the_measured_layers_at_the_pinned_point() {
        let c = ctx();
        let mut ping = MinimapPing::default();
        let mut quads = UiQuads::default();
        let art = art();

        // No ping: nothing drawn.
        emit_ping(&c, &mut ping, None, 0, &art, &mut quads);
        assert!(quads.overlays.is_empty());

        // A click 30 UI units up (north) at seam 1 seats a ping 30/px_per_yd yards north.
        emit_ping(&c, &mut ping, Some((0.0, 30.0)), 0, &art, &mut quads);
        assert_eq!(
            quads.overlays.len(),
            2,
            "the ring is transparent at phase 0"
        );
        let want = c.center + c.offset([30.0 / c.px_per_yd, 0.0, 0.0]);
        let k = c.side / super::super::blips::BLIP_BASIS_PX;
        for q in &quads.overlays {
            let mid = (q.rect.min + q.rect.max) * 0.5;
            assert!((mid - want).length() < 1e-3, "{mid:?} vs {want:?}");
            assert!(q.additive, "every quad of the model is SRC_ALPHA/ONE");
        }
        // ping5 (12.39 px) draws first, ping2 (12.80 px) second — the model's own order.
        assert!((quads.overlays[0].rect.width() - PING5_PX * k).abs() < 1e-3);
        assert!((quads.overlays[1].rect.width() - PING2_PX * k).abs() < 1e-3);
        assert!(
            (PING5_PX - 12.39).abs() < 0.01 && (PING2_PX - 12.80).abs() < 0.01,
            "the byte-measured sizes: {PING5_PX} / {PING2_PX}"
        );

        // Out of range it stops drawing WITHOUT dying: walk 200 yd away, then back. (The
        // reference's Lua hides the marker off-disc; it does not clear the ping.)
        quads.overlays.clear();
        let mut far = ctx();
        far.wx = -200.0;
        emit_ping(&far, &mut ping, None, 0, &art, &mut quads);
        assert!(quads.overlays.is_empty(), "off the disc: hidden");
        emit_ping(&c, &mut ping, None, 0, &art, &mut quads);
        assert_eq!(quads.overlays.len(), 2, "back in range: visible again");
    }

    /// **The ring is the motion the eye reads** (wow-re §10.4): a geometry scale `1 + 9·t/833`
    /// from 3.2 px to 32 px across the sequence loop, under an alpha that rises over 400 ms and
    /// falls over the remaining 433 — peaking when the ring is 5.32× its base. The paused version
    /// had no ring at all.
    #[test]
    fn the_ring_expands_and_peaks_at_four_hundred_milliseconds() {
        let c = ctx();
        let k = c.side / super::super::blips::BLIP_BASIS_PX;
        let art = art();
        let ring_at = |ms: f32| {
            let mut ping = MinimapPing::default();
            ping.seat((0.0, 0.0), 0, 0);
            ping.shown_ms = ms;
            let mut quads = UiQuads::default();
            emit_ping(&c, &mut ping, None, 0, &art, &mut quads);
            // The ring is the third layer whenever it is visible at all.
            quads
                .overlays
                .get(2)
                .map(|q| (q.rect.width() / k, q.color[3]))
        };
        assert_eq!(ring_at(0.0), None, "alpha 0 at the loop start: not emitted");
        let (w, a) = ring_at(PING4_ALPHA_PEAK_MS).expect("visible at the peak");
        assert!((a - 1.0).abs() < 1e-3, "peak alpha: {a}");
        assert!(
            (w / PING4_PX - 5.32).abs() < 0.01,
            "5.32x its base at the peak: {}",
            w / PING4_PX
        );
        // It keeps growing past the alpha peak, all the way to 10x, while fading out.
        let (w_late, a_late) = ring_at(PING_SEQ_MS - 1.0).expect("still visible near the end");
        assert!(w_late > w, "still expanding: {w_late} vs {w}");
        assert!(a_late < 0.02, "nearly gone: {a_late}");
        assert!(
            (w_late / PING4_PX - 10.0).abs() < 0.02,
            "10x at the loop end: {}",
            w_late / PING4_PX
        );
        assert!(
            (PING4_PX - 3.2).abs() < 0.01 && (PING4_PX * 10.0 - 32.0).abs() < 0.05,
            "3.2 → 32.0 px"
        );
    }

    /// **The spin is not uniform**, and reproducing it as a constant rate would be wrong by up to
    /// 15° (wow-re §10.3's 108 °/s burst across 225°→270°). The table's own keys, and the
    /// direction, are what this pins: one clockwise turn per 4833 ms, monotonic, never reversing.
    #[test]
    fn the_spin_follows_the_models_own_non_uniform_keys() {
        for (ms, deg) in PING5_SPIN {
            let got = spin_radians(ms).to_degrees();
            // The last key is a full turn, which wraps to 0 — the same rotation.
            let want = if ms >= PING_SPIN_MS { 0.0 } else { deg };
            assert!((got - want).abs() < 0.01, "at {ms} ms: {got} vs {want}");
        }
        // Monotonic clockwise across the whole revolution (positive rotation = CW on screen, the
        // sense the player arrow's `-facing` already establishes).
        let mut prev = -1.0;
        for i in 0..480 {
            let t = i as f32 * (PING_SPIN_MS / 480.0);
            let deg = spin_radians(t).to_degrees();
            assert!(deg >= prev - 1e-3, "never reverses: {deg} after {prev}");
            prev = deg;
        }
        // The burst the half-turn average hides: 225°→270° takes 417 ms, not the ~667 a uniform
        // rate would give.
        let quarter = spin_radians(3208.0).to_degrees();
        let burst_end = spin_radians(3625.0).to_degrees();
        assert!((quarter - 225.0).abs() < 0.01 && (burst_end - 270.0).abs() < 0.01);
    }

    /// The model clock is **not** reset per ping: the reference's Model frame free-runs and only
    /// ticks while shown, so consecutive pings resume mid-animation. Seating a second ping must
    /// leave the phase alone.
    #[test]
    fn a_second_ping_resumes_the_animation_rather_than_restarting_it() {
        let mut ping = MinimapPing::default();
        ping.seat((0.0, 0.0), 0, 0);
        ping.shown_ms = 1234.0;
        ping.seat((10.0, 10.0), 0, 0);
        assert!(
            (ping.shown_ms - 1234.0).abs() < f32::EPSILON,
            "the clock is the model's, not the ping's"
        );
    }

    /// A degenerate frame (the widget has not drawn yet) drops the click rather than seating a
    /// ping at a garbage point — and, unlike the first version, that is the *only* case in which
    /// a click is dropped for want of a scale.
    #[test]
    fn a_click_before_the_map_has_drawn_is_dropped() {
        let mut c = ctx();
        c.px_per_yd = 0.0;
        assert!(click_to_world(&c, (10.0, 10.0), 1.0).is_none());
    }
}
