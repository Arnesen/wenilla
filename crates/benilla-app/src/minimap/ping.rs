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

/// **The reference's ping does not fade; it pops.** `Minimap_OnUpdate` writes
/// `SetAlpha(255 * (t / MINIMAPPING_FADE_TIMER))` — a 0..255 alpha handed to an API that clamps to
/// 0..1, so the marker stays fully opaque until `t` falls below `0.5/255` ≈ 2 ms and then vanishes
/// in one frame. That is what the real client looks like, so it is what we draw. It is a
/// *behaviour* of theirs, not a quirk of ours to hide: flip this to `false` for a real linear fade
/// and nothing else changes.
const PING_ALPHA_IS_A_POP: bool = true;

/// The marker's on-screen size, as a fraction of the widget side on the client's byte-pinned
/// 140.8-px minimap basis — the same basis every other blip constant is frozen against
/// ([`super::blips::BLIP_BASIS_PX`]).
///
/// **INTERIM — an eyeball, not a measurement.** What the reference declares is verified (ref
/// `Minimap.xml` l.396): `MiniMapPing` is a `<Model>` frame on
/// `Interface\MiniMap\Ping\MinimapPing.mdx`, XML `scale="0.4"`, viewport 50×50. What that
/// *renders* at is not: the on-screen size is the model's own quad extent times the frame's
/// px-per-model-unit, the way [`super::blips::ARROW_QUAD_PX`] is 0.0500 · 768. Nobody has measured
/// this model's quad, so 40 px is the paused first version's eyeball carried forward unchanged —
/// deliberately, so the pin can be judged without the art also moving. Out for RE with the model's
/// animation; the rim arrows' precedent says a model like this is a plain textured quad, in which
/// case the flat stand-in becomes geometry-exact at the model's own size.
const PING_PX: f32 = 40.0;

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
    art: &[Handle<Image>],
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
    let rect = Rect::from_center_size(
        ctx.center + ctx.offset([px, py, 0.0]),
        Vec2::splat(ctx.side * (PING_PX / super::blips::BLIP_BASIS_PX)),
    );
    for layer in art {
        quads.overlays.push(UiQuad {
            rect,
            z_key: ctx.z,
            texture: Some(layer.clone()),
            color: [1.0, 1.0, 1.0, alpha * ctx.alpha],
            ..default()
        });
    }
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
    }
    // A map change drops it: the point it marks is not on this map.
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
    // Ours goes on the wire — raw world floats; the server relays them to the group and does
    // nothing at all when we are solo (which is why the marker was drawn locally, not awaited).
    if live.sender == 0 {
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

    /// **It draws, and where.** The pin's whole claim is that the marker's rect comes out of
    /// [`BlipCtx::offset`] like every other blip's — so this drives the real emitter and checks
    /// the rect, one quad per art layer, rather than trusting the caller.
    #[test]
    fn the_emitter_puts_a_quad_per_layer_at_the_pinned_point() {
        let c = ctx();
        let mut ping = MinimapPing::default();
        let mut quads = UiQuads::default();
        let art = vec![Handle::<Image>::default(), Handle::<Image>::default()];

        // No ping: nothing drawn.
        emit_ping(&c, &mut ping, None, 0, &art, &mut quads);
        assert!(quads.overlays.is_empty());

        // A click 30 UI units up (north) at seam 1 seats a ping 30/px_per_yd yards north...
        emit_ping(&c, &mut ping, Some((0.0, 30.0)), 0, &art, &mut quads);
        assert_eq!(quads.overlays.len(), 2, "one quad per layer");
        let want = c.center + c.offset([30.0 / c.px_per_yd, 0.0, 0.0]);
        for q in &quads.overlays {
            let mid = (q.rect.min + q.rect.max) * 0.5;
            assert!((mid - want).length() < 1e-3, "{mid:?} vs {want:?}");
            let side = c.side * (PING_PX / super::super::blips::BLIP_BASIS_PX);
            assert!((q.rect.width() - side).abs() < 1e-3);
        }

        // ...and once it is out of range it stops drawing WITHOUT dying: walk 200 yd away, then
        // back. (The reference's Lua hides the marker off-disc; it does not clear the ping.)
        quads.overlays.clear();
        let mut far = ctx();
        far.wx = -200.0;
        emit_ping(&far, &mut ping, None, 0, &art, &mut quads);
        assert!(quads.overlays.is_empty(), "off the disc: hidden");
        emit_ping(&c, &mut ping, None, 0, &art, &mut quads);
        assert_eq!(quads.overlays.len(), 2, "back in range: visible again");
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
