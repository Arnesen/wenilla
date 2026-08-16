//! **V-key nameplates** — `CGNamePlateFrame` (wow-re `object-layer/scratch/nameplate-vkey.md`,
//! §5-verified 2026-07-06): the toggled health-bar plates over units' heads, a **2-D overlay**
//! distinct from the world-pass overhead *names* (`crate::nameplates`) they replace.
//!
//! The pinned law, transcribed:
//! - **Master toggles** (`[0xc4da34]` bit 0 enemy / bit 3 friendly; real-client boot default
//!   **OFF** — ours boots enemy ON, friendly OFF (faithful): director calls, 0167 + 0599,
//!   [`VPlateMode::default`]): the `ShowNameplates`/`ShowFriendNameplates` script pairs —
//!   bound to **V / Shift-V** by FrameXML `Bindings.xml` (asset-sourced default, like TAB).
//! - **Gate**: never the own unit; never `NOT_SELECTABLE` (UNIT_FIELD_FLAGS bit 25); enemy bit
//!   covers reaction ≤ neutral, friendly bit ≥ friendly; **max 20 yd**, hardcoded; **no
//!   occlusion** (a 2-D overlay — plates draw through walls); snap, no smoothing. Anti-overlap
//!   there *is*, since 0367 found the shared solver ([`crate::smart_rect`]) — this file's older
//!   "no anti-overlap" was the census that missed it. And the sphere is bounded by the frustum:
//!   the unit must be **in view** ([`in_view`]), or its plate is not seated at all.
//!   (The friendly-totem exclusion waits on totems existing.)
//! - **Anchor**: the same overhead head point (`0x608640`, [`overhead_anchor`]) **+ 2/3 yd**
//!   (`[0x80abfc]`), projected per frame (`0x483ee0`); the plate's **TOP-CENTER** lands on the
//!   point (it hangs below — §8 Q5) and is edge-clamped half a plate inside the screen;
//!   **constant screen size**: the geometry globals `[0x87d9cc]=0.1` × `[0x87d9d0]=0.025` in gx
//!   screencoord units, where one unit = the screen **diagonal** `√(W²+H²)` (§8 Q4 — and never
//!   uiScale: plates live outside the UIParent cascade). **Our basis is growth-damped** past
//!   [`PLATE_DIAG_KNEE`] — a director-pinned deviation (0185/0186): the real client grows
//!   plates diagonal-linear without limit (§9, byte-closed) and the director rejected that
//!   look; past 1024×768 the plate grows at half the real rate (midway between faithful and
//!   the native size).
//! - **Anatomy — the §7 draw list, byte-verified** (frame offsets/sizes in gx screencoord
//!   units; TEXT heights resolve through the same damped basis, [`text_px`];
//!   back → front): the `Nameplate-Border` frame filling the 0.1 × 0.025 rect — its 128 × 32 art
//!   **sharp-resampled** to the plate's exact size ([`border`], 0188) so it reads crisp, not
//!   bilinear-magnified; the
//!   `UI-TargetingFrame-BarFill` health bar (0.0804 × 0.007025 at BOTTOMLEFT + (0.0031,
//!   0.003125)) — fill = HEALTH/MAXHEALTH as a **left-anchored crop** (u1 = fraction), instant,
//!   REACTION-tinted (hostile red / neutral yellow / friendly green / player pure blue — the
//!   confirmed dwords), **no backing behind missing health** (the border alone), the BORDER
//!   drawn over the fill (its rounded bevels cap the fill's ends — the reference look, a
//!   director-pinned correction to the §7 order); the name (`NAMEPLATE_FONT` = Friz, h 0.01,
//!   BOTTOM at plate CENTER, black 1 px drop shadow — director-tuned from the recorded ±0.001 gx,
//!   no truncation) in WHITE; the level (h 0.0086 — director-pinned one em under the byte 0.009;
//!   CENTER at BOTTOMRIGHT + (−0.0092, +0.0071)) in
//!   the client's exact con dwords over its own grayband table; the skull
//!   (`UI-TargetingFrame-Skull`, 0.01², OVERLAY on the level's anchor) replacing the number for
//!   a world boss (creature rank 3, unconditional) or a hostile ≥ 10 levels up. **No cast bar** exists on 1.12 plates
//!   (ctor-verified: exactly 7 children — the ctor's `Nameplate-Glow` child renders as the
//!   bar brighten below, not as a drawn overlay).
//! - **Highlight = the mouseover unit ∪ the target** (the watcher `0x606f20 → 0x607080` reads
//!   both globals): the bar's own colour brightens — a uniform [`LIT_BOOST`] lift of the fill
//!   tint, gradient untouched, nothing else changes (director-pinned form, 0184; the recorded
//!   ADD `Nameplate-Glow` rim read as hard edge lines on our linear-blending pipeline and is
//!   not drawn). The plate rect is itself mouse-enabled UI: hovering it makes its unit the
//!   mouseover (OnEnter `0x7cb850` → `[0xb4e2c8]`; [`PlateRects`] feeds the shared [`Hovered`]
//!   pick), which also lifts the model emissive and lets clicks select through the plate;
//!   plate-rect hover additionally turns the name yellow `0xFFFFFF00` (OnEnter, name-only).
//!   All decoupled from the target dim below.
//! - **Target highlight**: relative alpha — with a target, the target's plate is opaque and
//!   every other plate drops to `0x7F`; with no target all are opaque.
//! - **Mutually exclusive with the overhead name** (ShouldShowName): a unit with a live plate
//!   never also draws its floating name — [`VPlates`] is that verdict, read by the name driver.
//!   (The questgiver marker raises for a live plate too — a director-pinned DEVIATION: the
//!   reference really does sit low under a plate (byte-verified, wow-re `questgiver-marker.md`
//!   Q4a) and the director rejected that overlap; rationale on `quest_markers::pose_markers`,
//!   0408/0409.)
//!
//! (The skull's trivial-gray leg is the shared grey check, [`benilla_ui::script::unit_is_grey`]
//! — `0x5f0700`, §5-VERIFIED 2026-07-17, the same one the tooltip/quest-range APIs read; it is
//! vacuous on a ≥ +10 hostile and kept for transcription fidelity.)

use bevy::ecs::entity::EntityHashSet;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use benilla_protocol::EntityKind;
use benilla_ui::script::{unit_is_grey, JustifyH, JustifyV, Outline};

use crate::entities::{overhead_anchor, BoneAttach, OverheadFallback};
use crate::names::NameCache;
use crate::net::{Guid, NetCommands, NetEntity, ObjectStore, Reputations, SelfPlayer};
use crate::target::{ring_reaction, Factions, Hovered, Selection, TargetUpdate};
use crate::ui_pass::{UiQuad, UiQuadAppend, UiQuads, UvRect};
use crate::ui_text::{layout_text_quads, FontSpec, Justify, UiFontAtlas};
use benilla_assets::{AssetSet, WorldAssets};
use benilla_world::view::WorldCamera;

/// Sharp-resampling the frame border ([`border::resample_sharp`]) so the 128×32 art reads crisp at
/// the plate's larger size instead of bilinear-magnified soft (0188).
mod border;

/// The master bitmask (`[0xc4da34]`): bit 0 enemy plates, bit 3 friendly. The real client boots
/// with both OFF. Ours boots **enemy ON** (the 0167 director call — combat plates from the first
/// frame, a deliberate deviation) and **friendly OFF** (faithful — restored by the 0599 director
/// call, which also gives friendly speech bubbles room: the faithful plate-blocks-bubble gate
/// would otherwise suppress them). Both stay V / Shift-V togglable. `pub(crate)` so the capture
/// harness can force plates on for the `vplates` scenario.
#[derive(Resource)]
pub(crate) struct VPlateMode {
    pub(crate) enemies: bool,
    pub(crate) friends: bool,
}

impl Default for VPlateMode {
    fn default() -> Self {
        Self {
            enemies: true,
            friends: false,
        }
    }
}

/// The units carrying a live plate this frame — the ShouldShowName exclusivity verdict the
/// overhead-name driver reads (a plated unit never also draws its floating name).
#[derive(Resource, Default)]
pub(crate) struct VPlates(pub(crate) EntityHashSet);

/// The plates' screen rects this frame, in push (= draw) order. The plate is mouse-enabled UI on
/// the reference (`RegisterForClicks` in the ctor; OnEnter `0x7cb850` sets the mouseover-unit
/// global), so the hover pick (`crate::target::hover`) consults these — last frame's layout, the
/// reference's own input-vs-layout latency — before ray-testing the world.
#[derive(Resource, Default)]
pub(crate) struct PlateRects(pub(crate) Vec<(Rect, Entity)>);

/// The plate's textures — its own art, chain-verified present (§7 draw list). The name
/// `NAMEPLATE_FONT` resolves to Friz Quadrata, our atlas default.
const BAR_TEXTURE: &str = "Interface\\TargetingFrame\\UI-TargetingFrame-BarFill";
const BORDER_TEXTURE: &str = "Interface\\Tooltips\\Nameplate-Border";
const SKULL_TEXTURE: &str = "Interface\\TargetingFrame\\UI-TargetingFrame-Skull";
const RAID_TEXTURE: &str = "Interface\\TargetingFrame\\UI-RaidTargetingIcons";

/// The plate frame, gx screen-height units (`[0x87d9cc]`/`[0x87d9d0]`): 0.1 × 0.025. The border
/// SetAllPoints-fills it; everything else anchors inside it (§7, byte-verified offsets).
const PLATE_W: f32 = 0.1;
const PLATE_H: f32 = 0.025;
/// The health bar: BOTTOMLEFT ← plate BOTTOMLEFT + (0.0031, 0.003125), sized 0.0804 × 0.007025.
const BAR_OFF_X: f32 = 0.0031;
const BAR_OFF_Y: f32 = 0.003125;
const BAR_W: f32 = 0.0804;
const BAR_H: f32 = 0.007025;
/// Name text height 0.01, its BOTTOM at the plate CENTER; level height 0.0086, its CENTER at
/// plate BOTTOMRIGHT + (−0.0092, +0.0071); both carry the FontString's black drop shadow at
/// ±0.001. The skull (0.01 × 0.01) overlays the level's anchor when it replaces the number.
///
/// `LEVEL_H` is a **director-pinned deviation** (2026-07-07) from the byte 0.009: one em smaller
/// across the window range (em 11 vs 12 at the reference, −1 up through 1440p). TEXT ems compose
/// through the same damped gx basis as the frame geometry ([`text_px`] — the client-side chain is
/// byte-closed end to end, wow-re §9 Q3: live re-raster on resize, no fixed UI space, no staleness).
const NAME_H: f32 = 0.01;
const LEVEL_H: f32 = 0.0086;
const LEVEL_OFF_X: f32 = 0.0092;
const LEVEL_OFF_Y: f32 = 0.0071;
const SKULL_SIZE: f32 = 0.01;
/// The raid-target icon (vkey §7, VERIFIED): 0.02 × 0.02, RIGHT ← border.LEFT (0,0) — it hangs
/// off the plate's left edge, vertically centered; the 4-column atlas cell is 0.25.
const RAID_ICON_SIZE: f32 = 0.02;
/// The plate's world lift above the overhead anchor: 2/3 yd (`[0x80abfc]`).
const PLATE_LIFT: f32 = 2.0 / 3.0;
/// Max plate distance, hardcoded (no cvar): 20 yd.
const MAX_DIST_SQ: f32 = 20.0 * 20.0;
/// The non-target relative alpha while something is targeted — a **director-pinned deviation**
/// (2026-07-07) from the faithful `0x7F` (0.5), which faded the other plates too hard; raised so
/// non-target plates stay readable. Tunable: 0.5 = the byte law, 1.0 = no dim.
const DIM_ALPHA: f32 = 178.0 / 255.0;
/// The LIT (mouseover ∪ target) bar brighten — a uniform multiplicative lift of the fill tint
/// (director-pinned form: brighter colour, gradient untouched). Quad colors are client-space
/// sRGB (`srgb_quad_color` linearizes them), so this multiplies ENCODED texels ~1:1 — the
/// gamma-space modulate the client's FFP would do. 255/215 is the largest clean value: the
/// fill texture's encoded peak (215) lands exactly on white, no row clips, every row scales
/// by the same visible factor. (A first cut of 1.45 assumed a linear-space multiply and
/// flattened six rows against white — the very gradient change the director banned.)
const LIT_BOOST: f32 = 255.0 / 215.0;
/// The plate quads' z keys — back→front: bar fill, then the border OVER it (the border art's
/// rounded inner bevels cap the fill's square ends and crop the gradient's soft edges — the
/// reference look, director-pinned from a ref crop 2026-07-07; corrects the §7 transcription's
/// border-first order), then texts → skull; all above the floating combat text (z 0, a
/// world-scene draw) and far below every scripted-UI packed key.
const Z_FILL: u64 = 4;
const Z_BORDER: u64 = 5;
const Z_TEXT: u64 = 6;
const Z_RAID: u64 = 7;
const Z_SKULL: u64 = 8;

/// The bar-fill palette — the byte-confirmed dwords (`0xcf60d0/e8/c8/dc`): pure
/// red/blue/yellow/green (NOT the ring's pale player-blue). Client-space sRGB, the [`UiQuads`]
/// convention.
const PLATE_HOSTILE: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const PLATE_NEUTRAL: [f32; 4] = [1.0, 1.0, 0.0, 1.0];
const PLATE_FRIENDLY: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const PLATE_PLAYER: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// The plate palette selector — `0x7cbaa0`'s exact test order (§7, VERIFIED dwords): hostile
/// (reaction ≤ 1) red, else a player pure blue, else friendly (≥ 4) green, else — reaction 2–3,
/// unfriendly AND neutral — yellow. (Rank 2 is yellow, not red: an earlier transcription here
/// painted it hostile.) The bar fill (and its lit brighten) tints through this.
fn plate_tint(rank: u8, is_player: bool) -> [f32; 4] {
    if rank <= 1 {
        PLATE_HOSTILE
    } else if is_player {
        PLATE_PLAYER
    } else if rank >= 4 {
        PLATE_FRIENDLY
    } else {
        PLATE_NEUTRAL
    }
}

/// The level con palette — the client's EXACT dwords (§7; the softened WoW colors, not the
/// FrameXML QuestDifficultyColor approximations we shipped first): red `0xFFFF1919`, orange
/// `0xFFFF7F3F`, yellow `0xFFFFFF00`, green `0xFF3FB23F`, gray `0xFF7F7F7F`.
const CON_RED: [f32; 4] = [1.0, 25.0 / 255.0, 25.0 / 255.0, 1.0];
const CON_ORANGE: [f32; 4] = [1.0, 127.0 / 255.0, 63.0 / 255.0, 1.0];
const CON_YELLOW: [f32; 4] = [1.0, 1.0, 0.0, 1.0];
const CON_GREEN: [f32; 4] = [63.0 / 255.0, 178.0 / 255.0, 63.0 / 255.0, 1.0];
const CON_GRAY: [f32; 4] = [127.0 / 255.0, 127.0 / 255.0, 127.0 / 255.0, 1.0];

/// The client's own con-color law (§7, byte-verified — thresholds AND the grayband table
/// `[0x81dda8]`, indexed `playerLevel/5`; it agrees with vmangos' formula at every probed level
/// but the table is the client's own): `diff = unit − player`; ≥ +5 red, +3/+4 orange,
/// −2..+2 yellow, lower → green while within `grayband` levels below, gray past it. The
/// grayband/grey test lives in ONE home ([`benilla_ui::script::unit_is_grey`], the byte-identical
/// `0x80ae98`/`0x81dda8` twins) shared with the engine tooltip's "??"/CIVILIAN gates and the
/// `GetQuestGreenRange` binding — so the plate, the tooltip, and the target frame can never
/// disagree on where grey starts.
fn con_color(pl_level: u32, unit_level: u32) -> [f32; 4] {
    let diff = i64::from(unit_level) - i64::from(pl_level);
    if diff >= 5 {
        CON_RED
    } else if diff >= 3 {
        CON_ORANGE
    } else if diff >= -2 {
        CON_YELLOW
    } else if !unit_is_grey(pl_level, unit_level) {
        CON_GREEN
    } else {
        CON_GRAY
    }
}

/// Snap a logical-pixel coordinate onto the **device** pixel grid — the plate's texel alignment
/// (a benilla divergence: the reference draws at fractional device coords).
///
/// The plate rect is snapped so the sharp-resampled border art blits 1:1 and reads crisp instead
/// of bilinear-smeared (0188). Alignment is a property of the **framebuffer**, though, and the
/// quad lane is *logical* px ([`crate::ui_pass`]: 1 world unit = 1 logical px) — so the plain
/// `round()` this shipped with quantized the plate to `scale_factor` PHYSICAL pixels: two pixels
/// of stepping on the 2× display we develop and play on, against a world that slides continuously
/// underneath. At a fractional scale (1.25/1.5, the Windows norm) it did not land on a texel
/// boundary at all — the crispness it was bought for was an accident of an integer scale factor.
///
/// Snapping on the device grid keeps the 1:1 blit, is correct at any scale factor, and halves the
/// stepping at 2×: measured over a Goldshire walk at 1440×810 (the `vpl` trace), the extra
/// displacement the snap adds to a plate's glide fell from a median 0.33 / max 0.99 logical px per
/// frame to 0.21 / 0.49. What is left is the ±½ device pixel every crisp UI pays.
fn device_snap(v: f32, scale: f32) -> f32 {
    (v * scale).round() / scale
}

/// **Is the unit in view?** — the gate between the 20-yd sphere and the seat.
///
/// The plate gate is a 20-yard *sphere*; the view is a 45° frustum, so most of what the sphere
/// admits is beside or behind you. That would be harmless if an off-screen desire simply drew
/// off-screen — but the seat's on-screen clamp ([`crate::smart_rect`]'s `normalize`, the ref's
/// `0x509e20`) does not merely tidy a plate straddling an edge: it **translates a rect from
/// anywhere bodily onto the screen**. Without this gate a unit two screens off to the side got a
/// phantom plate pinned to the border — measured walking Goldshire at 1440×810 (the `vpl` trace,
/// [`drive_vplates`]): **30% of every drawn plate while moving, two thirds while standing still**.
/// They are not passive either: each claims bucket 0, so the anti-overlap solve pushed the plates
/// of units you *can* see off phantoms you cannot, and they were live hover targets through
/// [`PlateRects`].
///
/// That was the whole of the reported jitter — 26% of frame-steps moved a plate more than 4 px
/// past its own projected motion, up to 300 px in a single frame, and 90% of those involved a
/// phantom (after: 3.5% and 51 px, which is the solver's own documented snap-back). The projection
/// is smooth throughout — median frame-to-frame acceleration 0.05 px — so none of it was ever a
/// camera or an anchor problem.
///
/// The clamp keeps its job: a plate whose unit *is* in view still gets pulled fully inside the
/// border, which is what it is for.
fn in_view(screen: Vec2, viewport: Vec2) -> bool {
    (0.0..=viewport.x).contains(&screen.x) && (0.0..=viewport.y).contains(&screen.y)
}

/// The knee of the plate's gx basis — **a director-pinned DEVIATION from the byte law**
/// (0185/0186). The real client's plates grow diagonal-linear without limit (wow-re §9,
/// byte-closed: ~294 px plate + em-29 name at 2560×1440, bilinear-softened) — the director
/// rejected that look as too big/thick/soft, and a hard cap at native (0185) as too small.
/// 1280 is the diagonal where the 0.1 × 0.025 frame is EXACTLY the border art's native
/// 128 × 32 px; past it the basis grows at [`PLATE_GROWTH_DAMP`] of the real rate — at every
/// window the plate lands midway between the faithful size and the native pin.
const PLATE_DIAG_KNEE: f32 = 1280.0;
/// The growth rate past the knee: ½ = the midpoint between the byte law and the 0185 native
/// pin (the director's "between what it was and what it is now"). 0 restores the 0185 pin,
/// 1 the faithful law.
const PLATE_GROWTH_DAMP: f32 = 0.5;

/// One gx screencoord unit = the screen **DIAGONAL** `√(W²+H²)` (§8 Q4 — the device space spans
/// `[0,G44]×[0,G48]`, the live aspect basis; uiScale never enters: plates anchor WorldFrame-side,
/// outside the UIParent cascade) — growth damped past [`PLATE_DIAG_KNEE`] (the director's pin).
/// Shared with the chat bubble ([`crate::chat_bubble`]) — the sibling outside-UIParent overlay,
/// same basis law so bubble text and plate text hold the same em at every window.
pub(crate) fn plate_basis(viewport: Vec2) -> f32 {
    let d = viewport.x.hypot(viewport.y);
    if d <= PLATE_DIAG_KNEE {
        d
    } else {
        PLATE_DIAG_KNEE + (d - PLATE_DIAG_KNEE) * PLATE_GROWTH_DAMP
    }
}

/// gx screencoord units → pixels over the plate basis. Frame geometry only — text takes
/// [`text_px`]. Shared with the chat bubble, like [`plate_basis`].
pub(crate) fn gx_px(v: f32, basis: f32) -> f32 {
    (v * basis).round()
}

/// Plate FontString height → the FreeType em: `min(32, round(h · basis))`. The real client's
/// law is **byte-closed end to end** (wow-re `25cfa33e` + §9 Q3: the FontObject bridge
/// `0x44d040` divides the gx height by G48 before `GxuFontCreate`, the raster `0x5ca030`
/// multiplies by the viewport height — netting `h·√(W²+H²)`, re-rasterized live on resize,
/// 32-capped at the atlas cell); our `basis` is that diagonal under the director's
/// [`PLATE_DIAG_KNEE`] damping. Reference-pinned at its 1152×648 window: name em 13, level 12.
/// Shared with the chat bubble (`NAMEPLATE_FONT` at the same 0.01 gx), like [`plate_basis`].
pub(crate) fn text_px(h: f32, basis: f32) -> f32 {
    (h * basis).round().min(32.0)
}

/// NAMEPLATES / FRIENDNAMEPLATES / ALLNAMEPLATES through the binding table (0997; defaults V /
/// SHIFT-V / CTRL-V). The typing gate and 0585's modifier law live in the dispatch now.
///
/// The V and SHIFT-V halves stay benilla's **independent** toggles (the shipped behavior) rather
/// than 1.12's exclusive dance (whose NAMEPLATES body also force-hides friendly plates — a
/// recorded divergence, 0997 residue). ALLNAMEPLATES arrives with the table and takes the 1.12
/// body's semantics as-is: both on unless both already on, else both off.
fn toggle_vplates(binds: Res<crate::bindings::BindingsState>, mut mode: ResMut<VPlateMode>) {
    use crate::bindings::cmd;
    if binds.fired(cmd::NAMEPLATES) {
        mode.enemies = !mode.enemies;
        info!(
            "nameplates: enemy {}",
            if mode.enemies { "ON" } else { "OFF" }
        );
    }
    if binds.fired(cmd::FRIEND_NAMEPLATES) {
        mode.friends = !mode.friends;
        info!(
            "nameplates: friendly {}",
            if mode.friends { "ON" } else { "OFF" }
        );
    }
    if binds.fired(cmd::ALL_NAMEPLATES) {
        let both = mode.enemies && mode.friends;
        mode.enemies = !both;
        mode.friends = !both;
        info!("nameplates: all {}", if !both { "ON" } else { "OFF" });
    }
}

/// The world/reaction inputs of the plate gate, bundled (the 16-param ceiling).
#[derive(SystemParam)]
#[allow(clippy::type_complexity)] // one bundled system param — the app's convention
struct PlateWorld<'w, 's> {
    units: Query<
        'w,
        's,
        (
            Entity,
            &'static NetEntity,
            &'static Guid,
            &'static Transform,
            Option<&'static ObjectStore>,
        ),
        Without<SelfPlayer>,
    >,
    self_q: Query<'w, 's, (&'static Transform, Option<&'static ObjectStore>), With<SelfPlayer>>,
    factions: Option<Res<'w, Factions>>,
    reputations: Res<'w, Reputations>,
    selection: Res<'w, Selection>,
    // The mouseover unit (the 3-D pick ∪ last frame's plate rects) — with `selection`, the
    // highlight's trigger pair (the watcher's two globals `[0xb4e2c8]`/`[0xb4e2d8]`).
    hovered: Res<'w, Hovered>,
    camera: Query<'w, 's, (&'static Camera, &'static Transform), With<WorldCamera>>,
    // The cursor, for the plate-rect hover (OnEnter — the yellow name, this frame's rects).
    window: Query<'w, 's, &'static Window, With<bevy::window::PrimaryWindow>>,
}

/// The plate bitmap art, loaded once at STARTUP (not lazily on the first shown frame — the
/// director's first-toggle screenshot caught plates drawing before their textures had decoded).
/// The frame border is NOT here — it lives in [`PlateBorder`], resampled per size.
#[derive(Resource)]
struct PlateArt {
    fill: Handle<Image>,
    skull: Handle<Image>,
    raid_icons: Handle<Image>,
}

/// The frame border: the decoded `Nameplate-Border` BLP (`src`, the exact 128×32 art) plus the
/// sharpened texture cached at the plate's current physical size. Blitted at the plate's larger
/// size the GPU bilinear-magnifies the BLP into a soft blur, so instead we resample the SAME pixels
/// to the display size with sharp bilinear ([`border::resample_sharp`]) — crisp edges, same art
/// (0188, the director's "sharpen the same frame"). Redrawn only when the size changes.
#[derive(Resource)]
struct PlateBorder {
    src_w: u32,
    src_h: u32,
    src: Vec<u8>,
    size: Option<(u32, u32)>,
    handle: Option<Handle<Image>>,
}

/// Warm the plate textures at boot so the first V press draws complete plates. Through
/// [`WorldAssets::sprite_texture`] — the player-UI decode (**sRGB**, clamp, mip 0), the format the
/// UI pass's linearize-multiply-reencode contract requires. The raw `asset_server.load` this
/// shipped with took the `WorldArt` default (gamma-space `Unorm`, repeat): every plate texel got
/// sampled as linear and re-encoded brighter — the director's washed-out lime-instead-of-gold.
fn load_plate_art(
    mut commands: Commands,
    assets: Option<ResMut<WorldAssets>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(mut assets) = assets else {
        return; // no game data (bare test app) — drive_vplates tolerates the missing resource
    };
    let fill = assets.sprite_texture(BAR_TEXTURE, &mut images);
    let skull = assets.sprite_texture(SKULL_TEXTURE, &mut images);
    let raid_icons = assets.sprite_texture(RAID_TEXTURE, &mut images);
    let (Some(fill), Some(skull), Some(raid_icons)) = (fill, skull, raid_icons) else {
        warn!("vplates: plate art missing from the patch chain — plates will not draw");
        return;
    };
    // Decode the border BLP's raw pixels once; drive_vplates resamples them per plate size.
    let Some((src_w, src_h, src)) = assets.decode_rgba(BORDER_TEXTURE) else {
        warn!("vplates: border art missing from the patch chain — plates will not draw");
        return;
    };
    commands.insert_resource(PlateArt {
        fill,
        skull,
        raid_icons,
    });
    commands.insert_resource(PlateBorder {
        src_w,
        src_h,
        src,
        size: None,
        handle: None,
    });
}

/// Gate + draw, every frame: decide which units carry a plate (into [`VPlates`], the
/// name-exclusivity verdict) and append the §7 draw list — border, left-cropped reaction fill,
/// shadowed name (yellow under the mouse), con-colored level or the skull — at constant
/// screen size over the projected anchor + 2/3 yd. Runs in the [`UiQuadAppend`] window (after
/// the script extract), after the targeting chain (it reads the frame's selection verdict).
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // one Bevy system's full input set
fn drive_vplates(
    mode: Res<VPlateMode>,
    mut plates: ResMut<VPlates>,
    mut rects: ResMut<PlateRects>,
    world: PlateWorld,
    mut names: ResMut<NameCache>,
    net_commands: Res<NetCommands>,
    mut atlas: Option<ResMut<UiFontAtlas>>,
    mut quads: ResMut<UiQuads>,
    art: Option<Res<PlateArt>>,
    // The border source + size-keyed cache, and the image store it resamples into.
    plate_border: Option<ResMut<PlateBorder>>,
    mut images: ResMut<Assets<Image>>,
    // The overhead-anchor inputs ([`overhead_anchor`]).
    anchor_q: (
        Query<&BoneAttach>,
        Query<&OverheadFallback>,
        Query<&GlobalTransform>,
        Query<(), With<crate::entities::mount::MountChild>>,
    ),
    // The frame's claim bucket 0 (nameplates) — cleared and rebuilt every pass; worldtext owns
    // bucket 1 in `combat_text` and the two never interact (a plate never pushes a number).
    mut bucket: Local<crate::smart_rect::SmartBucket>,
    // The raid-target board (decision 0434 §6) — the plate's raid-icon child reads it.
    group: Res<crate::ui_party::GroupState>,
) {
    plates.0.clear();
    rects.0.clear();
    bucket.clear();
    if !mode.enemies && !mode.friends {
        return;
    }
    let (Ok((cam, cam_pose)), Ok((self_tf, self_store)), Some(atlas), Some(art), Some(mut border)) = (
        world.camera.single(),
        world.self_q.single(),
        atlas.as_mut(),
        art.as_deref(),
        plate_border,
    ) else {
        return;
    };
    let cam_tf = GlobalTransform::from(*cam_pose);
    let Some(viewport) = cam.logical_viewport_size() else {
        return;
    };
    let basis = plate_basis(viewport);
    let gx = |v: f32| gx_px(v, basis);
    let window = world.window.single().ok();
    let cursor = window.and_then(|w| w.cursor_position());
    let my_level = self_store.and_then(|s| s.0.unit_level()).unwrap_or(1);
    let has_target = world.selection.target.is_some();

    // The plate frame is constant-size this frame; resample the 128×32 border BLP to its exact
    // PHYSICAL pixel size (logical × scale_factor — like the glyph atlas) with sharp bilinear, and
    // cache it. Redrawn only when that size changes; drawn 1:1 so the SAME art reads crisp instead
    // of GPU-bilinear-magnified soft (0188 — the director's "sharpen the same frame").
    let (pw, ph) = (gx(PLATE_W), gx(PLATE_H));
    let scale = window.map_or(1.0, |w| w.scale_factor());
    let tex = (
        (pw * scale).round().max(1.0) as u32,
        (ph * scale).round().max(1.0) as u32,
    );
    if border.size != Some(tex) {
        let rgba = border::resample_sharp(&border.src, border.src_w, border.src_h, tex.0, tex.1);
        border.handle = Some(images.add(benilla_assets::sprite_image(tex.0, tex.1, rgba)));
        border.size = Some(tex);
    }
    let Some(border_tex) = border.handle.clone() else {
        return;
    };

    // The seat PRIORITY (`0x608870`/`0x6089a0` + the per-frame walk `0x608ce0`): plates seat in
    // squared-distance order from the fixed gx point (0.4, 0.3) — the 4:3 screen center, a
    // literal constant in the ref, converted here over the diagonal basis with the Y mirrored
    // out of the ref's Y-up device space. The first-seated plate never moves; each later one is
    // solved off the plates already claimed this frame (bucket 0) — the reference's bouncing,
    // never-overlapping plates. Gate + project first, then seat in sorted order. The TRUE
    // diagonal, not the 0185-damped `basis`: the point is positional law, not plate size.
    let diag = viewport.length();
    let sort_pt = Vec2::new(0.4 * diag, viewport.y - 0.3 * diag);
    let mut cands = Vec::new();
    for (entity, net, guid, tf, store) in &world.units {
        if !matches!(net.kind, EntityKind::Unit | EntityKind::Player) {
            continue;
        }
        // NOT_SELECTABLE never gets a plate (bit 25 — part of the byte gate).
        if store.is_some_and(|s| s.0.unit_flags() & (1 << 25) != 0) {
            continue;
        }
        // A DEAD unit shows no plate — the per-tick gate `0x60f600`'s FIRST, unconditional
        // test (§8 Q2, byte-confirmed): signed `UNIT_FIELD_HEALTH ≤ 0`, absent = 0 (the
        // zero-init descriptor). A poll, not a death callback; the lootable bit is never
        // consulted. Feign death keeps health > 0, so it clears THIS leg — the gate that takes a
        // feigning body's plate away is the hostile-only one below (decision 1022).
        if store.is_none_or(|s| s.0.unit_health().unwrap_or(0) == 0) {
            continue;
        }
        // …and the SAME per-tick gate's second leg (`0x60f62e`, byte-VERIFIED wow-re
        // ghost-death-visuals.md — corrects this file's older "ghost stays plated" note):
        // `bytes_1 byte3 & 3` (ghost | creep) denies the plate — a released ghost (health 1)
        // carries none.
        if store.is_some_and(|s| s.0.unit_is_ghost_visual()) {
            continue;
        }
        // 20 yd, hardcoded, from the player.
        if (tf.translation - self_tf.translation).length_squared() > MAX_DIST_SQ {
            continue;
        }
        // The enemy/friendly split over the shared reaction rank: ≤ neutral is the enemy bit's
        // population, ≥ friendly the friend bit's.
        let rank = ring_reaction(
            world.factions.as_deref(),
            &world.reputations,
            store,
            self_store,
        );
        let is_player = net.kind == EntityKind::Player;
        let friendly = rank >= 4;
        if friendly && !mode.friends || !friendly && !mode.enemies {
            continue;
        }
        // The gate's dead-looking leg, which sits on the HOSTILE side of that same split
        // (`0x60f72f test al,al; jne` → friendly goes to the creature-type check `0x60f750`;
        // hostile falls into `0x60f733 shr eax,5; test al,1` → hide). So a feigning hunter's
        // plate vanishes for the enemies he is hiding from, which is the point of the spell,
        // while a friendly one keeps his (decision 1022).
        if !friendly && store.is_some_and(|s| s.0.unit_reads_dead()) {
            continue;
        }
        // Project the plate point: the overhead anchor + 2/3 yd, per frame, no smoothing. Behind
        // the camera the projection itself fails and the plate is gone. (This comment used to
        // claim "the plate has no clamp — offscreen is gone", which had not been true since the
        // seat gained the solver's on-screen normalize: offscreen was dragged to the border. The
        // gate below is what makes the sentence true again.)
        let anchor = overhead_anchor(
            entity,
            tf,
            &anchor_q.0,
            &anchor_q.1,
            &anchor_q.2,
            &anchor_q.3,
        );
        let Ok(screen) = cam.world_to_viewport(&cam_tf, anchor + Vec3::Y * PLATE_LIFT) else {
            continue;
        };
        // …and the unit has to be in view ([`in_view`]).
        if !in_view(screen, viewport) {
            continue;
        }
        cands.push((
            screen.distance_squared(sort_pt),
            screen,
            anchor,
            rank,
            is_player,
            entity,
            guid,
            store,
        ));
    }
    cands.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (_, screen, anchor, rank, is_player, entity, guid, store) in cands {
        plates.0.insert(entity);

        // The target-highlight relative alpha: target (or nobody targeted) opaque, others 0x7F.
        let alpha = if !has_target || world.selection.target == Some(entity) {
            1.0
        } else {
            DIM_ALPHA
        };
        // The bar tint: the NAMEPLATE palette in `0x7cbaa0`'s exact order.
        let tint = plate_tint(rank, is_player);
        let frac = store
            .and_then(|s| Some(s.0.unit_health()? as f32 / s.0.unit_max_health()?.max(1) as f32))
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        // The frame SEAT (§8 Q5, byte-confirmed): the plate's TOP-CENTER lands on the projected
        // point — the plate HANGS BELOW head + 2/3 yd — and the point is edge-clamped half a
        // plate inside every screen border (`SetPoint(TOP ← root.BOTTOMLEFT, clampedX/Y)`).
        // The highlight is a 2-D hover over THIS rect (the frame's OnEnter — yellow name). `pw`/`ph`
        // (the plate's logical size) are hoisted above the loop — the border resample keys off them.
        // Geometry trace for the vplates capture (`WOW_VPLATE_TRACE=1`): the exact quad rects
        // pushed this frame, in logical px — the machine-side check the capture PNG can't give
        // (fill/border/text hues overlap under zoom).
        let trace = std::env::var("WOW_VPLATE_TRACE").as_deref() == Ok("1");
        // The full seat (`0x509ec0`): the desired rect TOP-anchored on the raw projected point
        // (the plate hangs below head + 2/3 yd), then the bucket-0 seat law — normalize, SOLVE
        // off the plates already claimed this frame, clamp the resolved center-X/top half a
        // plate inside every border, rebuild ([`crate::smart_rect`]). The old bare edge-clamp
        // was this law minus the solve (nameplate-vkey §8.5, now corrected by the solver note).
        let desired = Rect::new(
            screen.x - pw * 0.5,
            screen.y,
            screen.x + pw * 0.5,
            screen.y + ph,
        );
        let solved = bucket.resolve(desired, viewport);
        // The rect snaps to the device pixel grid ([`device_snap`]) — the LEFT edge, not the
        // centre: an odd width would put a snapped centre's edges back between texels. The CLAIM
        // takes the snapped rect, so later plates dodge exactly what is drawn.
        let top = device_snap(solved.min.y, scale);
        let left = device_snap(solved.min.x, scale);
        let plate = Rect::new(left, top, left + pw, top + ph);
        // **The jitter decomposition** (`WOW_MOVE_TRACE` tag `vpl`, one line per plate per frame):
        // the world anchor, the camera pose that projected it, the raw projected point, the
        // solver's answer, and the snapped rect — so "the plates are jittery when moving" is
        // attributed to one of the four (a moving anchor, a noisy camera, a flipping solve, the
        // pixel snap) from numbers rather than from the eye. It rides the shared trace — tag-
        // filtered, one line per plate — and not the `WOW_VPLATE_TRACE` eprintlns beside it, whose
        // four unbuffered writes per plate per frame would distort the frame pacing the question
        // is about (the 0880 lesson).
        if benilla_assets::trace::enabled_for("vpl") {
            let (cp, cf) = (cam_pose.translation, cam_pose.forward());
            benilla_assets::trace::line(
                "vpl",
                &format!(
                    "e={} vp=({:.0},{:.0}) anchor=[{:.4},{:.4},{:.4}] cam=[{:.4},{:.4},{:.4}] \
                     fwd=[{:.4},{:.4},{:.4}] scr=({:.3},{:.3}) solved=({:.3},{:.3}) plate=({:.1},{:.1})",
                    entity.index(),
                    viewport.x,
                    viewport.y,
                    anchor.x,
                    anchor.y,
                    anchor.z,
                    cp.x,
                    cp.y,
                    cp.z,
                    cf.x,
                    cf.y,
                    cf.z,
                    screen.x,
                    screen.y,
                    solved.min.x,
                    solved.min.y,
                    plate.min.x,
                    plate.min.y,
                ),
            );
        }
        bucket.claim(plate);
        rects.0.push((plate, entity));
        let hover = cursor.is_some_and(|c| plate.contains(c));
        // The highlight trigger — the watcher's OR over the two globals: this unit is the
        // MOUSEOVER (the 3-D body pick, or a plate hover routed through [`PlateRects`] last
        // frame) or the current TARGET. `hover` (this frame's rect) joins in so the bar
        // brighten never lags the yellow name.
        let lit =
            hover || world.hovered.target == Some(entity) || world.selection.target == Some(entity);
        if trace {
            eprintln!(
                "vplate-trace: viewport={viewport:?} screen=({:.2},{:.2}) plate=({:.1},{:.1})..({:.1},{:.1}) lit={lit}",
                screen.x, screen.y, plate.min.x, plate.min.y, plate.max.x, plate.max.y
            );
        }
        // The border, drawn OVER the fill (z 5 > z 4): its rounded inner bevels cap the fill's
        // square ends and crop the gradient's soft top/bottom rows — the reference look
        // (director-pinned from a ref crop; corrects the §7 transcription's border-first order).
        quads.overlays.push(UiQuad {
            rect: plate,
            z_key: Z_BORDER,
            texture: Some(border_tex.clone()),
            uv: UvRect::FULL,
            color: [1.0, 1.0, 1.0, alpha],
            ..default()
        });
        // The health fill, bottommost — a left-anchored CROP (quad right = left + frac·width,
        // u1 = frac; byte-verified `SetValue → 0x770410`), reaction-tinted, instant. No backing
        // quad — the border alone sits behind missing health.
        let bar_left = plate.min.x + gx(BAR_OFF_X);
        let bar_bottom = plate.max.y - gx(BAR_OFF_Y);
        let bar = Rect::new(
            bar_left,
            bar_bottom - gx(BAR_H),
            bar_left + gx(BAR_W),
            bar_bottom,
        );
        if trace {
            eprintln!(
                "vplate-trace: fill=({:.1},{:.1})..({:.1},{:.1}) frac={frac:.3}",
                bar.min.x, bar.min.y, bar.max.x, bar.max.y
            );
        }
        // The highlight: LIT (mouseover ∪ target), the bar's own colour simply brightens — a
        // uniform [`LIT_BOOST`] lift of the fill tint, nothing else changes (director-pinned
        // 2026-07-07: no rim, no gradient change — the additive Nameplate-Glow rim we tried
        // first read as hard edge lines, see 0184).
        let boost = if lit { LIT_BOOST } else { 1.0 };
        quads.overlays.push(UiQuad {
            rect: Rect::new(
                bar.min.x,
                bar.min.y,
                bar.min.x + bar.width() * frac,
                bar.max.y,
            ),
            z_key: Z_FILL,
            texture: Some(art.fill.clone()),
            uv: UvRect {
                corners: [[0.0, 0.0], [frac, 0.0], [frac, 1.0], [0.0, 1.0]],
            },
            color: [tint[0] * boost, tint[1] * boost, tint[2] * boost, alpha],
            ..default()
        });
        // Director-tuned (2026-07-07): a constant 1 logical px. The recorded ±0.001 gx rounds
        // to 1 px at the ref's 1152×648 but 2 px at larger windows — which read chunky.
        let shadow = 1.0;
        // The name — its BOTTOM at the plate CENTER, white (yellow while hovered, never
        // reaction-colored), with the FontString's black drop shadow.
        if let Some(name) = names.resolve(guid.0, &net_commands).map(str::to_owned) {
            let color = if hover {
                [1.0, 1.0, 0.0, alpha]
            } else {
                [1.0, 1.0, 1.0, alpha]
            };
            plate_text(
                atlas,
                &mut quads,
                &name,
                Vec2::new(
                    (plate.min.x + plate.max.x) * 0.5,
                    (plate.min.y + plate.max.y) * 0.5,
                ),
                true,
                text_px(NAME_H, basis),
                shadow,
                color,
                trace,
            );
        }
        // The level element — CENTER at plate BOTTOMRIGHT + (−0.0092, +0.0071): the skull
        // replaces the number for a hostile ≥ 10 levels up that isn't trivial-gray (the
        // world-boss rank leg waits on caching the query's rank field; the exact trivial
        // predicate `0x5f0700` is INFERRED — we use the con gray).
        let lvl_at = Vec2::new(plate.max.x - gx(LEVEL_OFF_X), plate.max.y - gx(LEVEL_OFF_Y));
        if let Some(level) = store.and_then(|s| s.0.unit_level()) {
            let con = con_color(my_level, level);
            // The skull's two legs (`0x7cbb40`, §7-VERIFIED): a WORLD BOSS — creature-
            // classification rank 3, from the ask-once creature record (it lands with the
            // name query; a miss just means no skull yet) — unconditionally; or a hostile
            // ≥ 10 levels up that isn't trivial-grey (`0x5f0700`, the shared grey check).
            // The rank goes through the client's own getter (`gated_rank`, decision 0782), which is
            // what makes a MIND-CONTROLLED world boss show its NUMBER here rather than a skull:
            // `0x7cbb40` reads rank via `0x605620`, pet gate and all.
            let world_boss = crate::names::gated_rank(
                benilla_protocol::guid::entry(guid.0).and_then(|e| names.creature_record(e)),
                store,
            ) == 3;
            if world_boss || (rank <= 1 && level >= my_level + 10 && !unit_is_grey(my_level, level))
            {
                let half = gx(SKULL_SIZE) * 0.5;
                quads.overlays.push(UiQuad {
                    rect: Rect::new(
                        lvl_at.x - half,
                        lvl_at.y - half,
                        lvl_at.x + half,
                        lvl_at.y + half,
                    ),
                    z_key: Z_SKULL,
                    texture: Some(art.skull.clone()),
                    uv: UvRect::FULL,
                    color: [1.0, 1.0, 1.0, alpha],
                    ..default()
                });
            } else {
                let mut c = con;
                c[3] *= alpha;
                plate_text(
                    atlas,
                    &mut quads,
                    &level.to_string(),
                    lvl_at,
                    false,
                    text_px(LEVEL_H, basis),
                    shadow,
                    c,
                    trace,
                );
            }
        }
        // The raid icon (vkey §7 + "Raid icon", VERIFIED — the 0434 §6 board finally streams):
        // shown iff the unit's guid holds a mark; RIGHT ← border.LEFT (0,0) so it hangs off the
        // plate's left edge, 0.02×0.02, the 4-column atlas cell (col=idx&3, row=idx>>2, cell
        // 0.25). Drawn between the level and the skull (the §7 draw order).
        let mark = group.raid_target_index(guid.0);
        if mark >= 1 {
            let i = u32::from(mark - 1);
            let (u0, v0) = ((i & 3) as f32 * 0.25, (i >> 2) as f32 * 0.25);
            let size = gx(RAID_ICON_SIZE);
            let cy = (plate.min.y + plate.max.y) * 0.5;
            quads.overlays.push(UiQuad {
                rect: Rect::new(
                    plate.min.x - size,
                    cy - size * 0.5,
                    plate.min.x,
                    cy + size * 0.5,
                ),
                z_key: Z_RAID,
                texture: Some(art.raid_icons.clone()),
                uv: UvRect {
                    corners: [
                        [u0, v0],
                        [u0 + 0.25, v0],
                        [u0 + 0.25, v0 + 0.25],
                        [u0, v0 + 0.25],
                    ],
                },
                color: [1.0, 1.0, 1.0, alpha],
                ..default()
            });
        }
    }
}

/// One line of plate text at `anchor` — ink centered on `anchor.x`; `bottom_seated` puts the
/// ink's bottom edge on `anchor.y` (the name's BOTTOM anchor), else its ink middle (the level's
/// CENTER) — with the plate FontString's black drop shadow one [`SHADOW_OFF`] right+down.
/// Both seats are computed from the **measured ink**, never the layout's line box: cosmic-text's
/// `Middle` centers the em box (ascent-heavy for Friz), which sat the level digit ~8 px below its
/// anchor — half outside the plate, the director's "level is not aligned".
/// `NAMEPLATE_FONT` resolves to Friz, the atlas default.
#[allow(clippy::too_many_arguments)]
fn plate_text(
    atlas: &mut UiFontAtlas,
    quads: &mut UiQuads,
    text: &str,
    anchor: Vec2,
    bottom_seated: bool,
    px: f32,
    shadow_px: f32,
    color: [f32; 4],
    trace: bool,
) {
    // Shape at the nearest baked atlas size, then rescale the quads to the exact target em
    // around the anchor (the combat_text unbaked-size recipe) — plate ems are window-derived
    // (8 at 768, 11 at 1080, [`text_px`]) and mostly absent from the baked set.
    let shaped = atlas.snap_size(px);
    let spec = FontSpec {
        path: None,
        height: Some(shaped),
        outline: Outline::None,
        paint_halo: true,
        alpha_gradient: None,
    };
    let justify = Justify {
        h: JustifyH::Center,
        v: JustifyV::Middle,
    };
    let mut main = layout_text_quads(
        atlas,
        text,
        Rect::from_center_size(anchor, Vec2::ZERO),
        color,
        justify,
        Z_TEXT,
        spec,
    );
    if main.is_empty() {
        return;
    }
    let s = px / shaped;
    for q in main.iter_mut() {
        q.rect = Rect::new(
            anchor.x + (q.rect.min.x - anchor.x) * s,
            anchor.y + (q.rect.min.y - anchor.y) * s,
            anchor.x + (q.rect.max.x - anchor.x) * s,
            anchor.y + (q.rect.max.y - anchor.y) * s,
        );
    }
    // Seat by measured INK, then the shadow is the same run offset one step right+down.
    let dy = {
        let bottom = main.iter().map(|q| q.rect.max.y).fold(f32::MIN, f32::max);
        if bottom_seated {
            anchor.y - bottom
        } else {
            let top = main.iter().map(|q| q.rect.min.y).fold(f32::MAX, f32::min);
            anchor.y - (top + bottom) * 0.5
        }
    };
    for q in main.iter_mut() {
        q.rect = Rect::new(
            q.rect.min.x,
            q.rect.min.y + dy,
            q.rect.max.x,
            q.rect.max.y + dy,
        );
    }
    let mut shadow: Vec<UiQuad> = main
        .iter()
        .map(|q| UiQuad {
            rect: Rect::new(
                q.rect.min.x + shadow_px,
                q.rect.min.y + shadow_px,
                q.rect.max.x + shadow_px,
                q.rect.max.y + shadow_px,
            ),
            color: [0.0, 0.0, 0.0, color[3]],
            ..q.clone()
        })
        .collect();
    if trace {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for q in main.iter() {
            x0 = x0.min(q.rect.min.x);
            y0 = y0.min(q.rect.min.y);
            x1 = x1.max(q.rect.max.x);
            y1 = y1.max(q.rect.max.y);
        }
        eprintln!(
            "vplate-trace: text {text:?} px={px:.1} shaped={shaped:.1} ink=({x0:.1},{y0:.1})..({x1:.1},{y1:.1}) anchor=({:.1},{:.1})",
            anchor.x, anchor.y
        );
    }
    // Shadow first, main over it (equal z resolves by push order — the stable sort).
    quads.overlays.append(&mut shadow);
    quads.overlays.append(&mut main);
}

/// The set the plate drive runs in — the overhead-name driver orders after it (the
/// ShouldShowName exclusivity: a plated unit draws no floating name).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct VPlateSet;

/// V-key nameplates: the toggles + the per-frame gate/draw.
pub(crate) struct VPlatesPlugin;

impl Plugin for VPlatesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VPlateMode>()
            .init_resource::<VPlates>()
            .init_resource::<PlateRects>()
            .add_systems(Startup, load_plate_art.after(AssetSet::Open))
            .add_systems(
                Update,
                (
                    toggle_vplates,
                    // After the targeting chain (selection/hover verdicts), inside the UI-quad
                    // append window (the mesh rebuild waits on it).
                    drive_vplates.after(TargetUpdate).in_set(UiQuadAppend),
                )
                    .chain()
                    .in_set(VPlateSet),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client's own con law (§7): thresholds at level 30 (grayband[6] = 7 → green down to
    /// 23, gray at 22), the exact softened dwords, and the low-level edge (grayband[0] = 4, so
    /// a level-1 never grays anything).
    #[test]
    fn con_color_matches_the_byte_table() {
        assert_eq!(con_color(30, 35), CON_RED);
        assert_eq!(con_color(30, 33), CON_ORANGE);
        assert_eq!(con_color(30, 28), CON_YELLOW);
        assert_eq!(
            con_color(30, 23),
            CON_GREEN,
            "gap 7 = grayband[6], still green"
        );
        assert_eq!(con_color(30, 22), CON_GRAY, "gap 8 crosses the band");
        assert_eq!(con_color(1, 1), CON_YELLOW);
        assert_ne!(
            con_color(5, 1),
            CON_GRAY,
            "grayband[1] = 4 ≥ the max gap at 5"
        );
        // The exact dwords (0xFFFF1919 / 0xFFFF7F3F / 0xFF3FB23F): byte-pinned, not the
        // FrameXML QuestDifficultyColor approximations.
        assert_eq!(CON_RED[1], 25.0 / 255.0);
        assert_eq!(CON_ORANGE[2], 63.0 / 255.0);
        assert_eq!(CON_GREEN[1], 178.0 / 255.0);
    }

    /// The in-view gate: the 20-yd sphere reaches far outside the frustum, and everything it
    /// admits from out there used to be dragged onto a screen border by the seat's clamp. The
    /// bounds are inclusive — a unit exactly on the edge is still in view, and the clamp (not this)
    /// is what keeps its plate fully inside.
    #[test]
    fn only_a_unit_in_view_gets_a_plate() {
        let vp = Vec2::new(1440.0, 810.0);
        assert!(in_view(Vec2::new(720.0, 405.0), vp));
        assert!(in_view(Vec2::ZERO, vp), "the corner is in view");
        assert!(in_view(vp, vp), "so is the far corner");
        for out in [
            Vec2::new(-0.5, 405.0),    // off left
            Vec2::new(1440.5, 405.0),  // off right
            Vec2::new(720.0, -0.5),    // above
            Vec2::new(720.0, 810.5),   // below
            Vec2::new(1409.0, 1332.0), // the measured phantom: two thirds of a screen below
        ] {
            assert!(!in_view(out, vp), "{out:?} is not on screen");
        }
    }

    /// The snap is on the DEVICE grid, not the logical one. At the 2× scale we develop on, a
    /// logical `round()` moved the plate two physical pixels at a time; this moves it one, which
    /// is the smallest step that still lands the border blit on a texel boundary. Pinned so the
    /// `scale` argument can't be "simplified" away back into `round()`.
    #[test]
    fn the_plate_snaps_on_the_device_grid_not_the_logical_one() {
        // 2× display: the grid is every half logical pixel, and every snapped value is a whole
        // number of PHYSICAL pixels.
        for (v, want) in [(10.0, 10.0), (10.2, 10.0), (10.3, 10.5), (10.6, 10.5)] {
            let got = device_snap(v, 2.0);
            assert_eq!(got, want, "{v} at 2×");
            assert_eq!((got * 2.0).fract(), 0.0, "{got} is a whole physical pixel");
        }
        // 1×: unchanged from the old law.
        assert_eq!(device_snap(10.4, 1.0), 10.0);
        assert_eq!(device_snap(10.6, 1.0), 11.0);
        // 1.5× (the Windows norm), where a logical round() was never texel-aligned at all.
        assert_eq!((device_snap(10.4, 1.5) * 1.5).fract(), 0.0);
        assert_eq!((device_snap(10.9, 1.5) * 1.5).fract(), 0.0);
    }

    /// Plate text ems ride the gx DIAGONAL like the frame geometry, growth-damped past the
    /// director's [`PLATE_DIAG_KNEE`] — name em 13 at the reference's 1152×648 window (measured),
    /// level em 11 (director-pinned one under the byte 0.009); at larger windows the em lands midway
    /// between the byte law and the 0185 native pin (2560×1440: byte law 29, pin 13 → ours 21). The
    /// raw em still carries the client's 32 atlas-cell cap.
    #[test]
    fn plate_text_sizes_take_the_damped_diagonal_basis() {
        let ref43 = plate_basis(Vec2::new(1024.0, 768.0));
        let refwin = plate_basis(Vec2::new(1152.0, 648.0));
        assert_eq!(text_px(NAME_H, ref43), 13.0);
        assert_eq!(text_px(LEVEL_H, ref43), 11.0);
        assert_eq!(text_px(NAME_H, refwin), 13.0);
        assert_eq!(text_px(LEVEL_H, refwin), 11.0);
        let big = plate_basis(Vec2::new(2560.0, 1440.0));
        assert_eq!(text_px(NAME_H, big), 21.0, "midway between 29 and 13");
        assert_eq!(text_px(NAME_H, 10_000.0), 32.0, "atlas-cell cap, raw law");
    }

    /// The palette selector follows `0x7cbaa0`'s exact test order — notably reaction 2
    /// (unfriendly) is YELLOW with 3, not hostile red, and a hostile player is red before the
    /// player-blue test.
    #[test]
    fn plate_tint_matches_the_byte_order() {
        assert_eq!(plate_tint(1, false), PLATE_HOSTILE);
        assert_eq!(plate_tint(1, true), PLATE_HOSTILE, "hostile beats player");
        assert_eq!(plate_tint(2, false), PLATE_NEUTRAL, "unfriendly is yellow");
        assert_eq!(plate_tint(3, false), PLATE_NEUTRAL);
        assert_eq!(plate_tint(3, true), PLATE_PLAYER, "player beats neutral");
        assert_eq!(plate_tint(4, false), PLATE_FRIENDLY);
        assert_eq!(plate_tint(6, true), PLATE_PLAYER, "player beats friendly");
    }

    /// The plate geometry law through the gx DIAGONAL unit, growth-damped past
    /// [`PLATE_DIAG_KNEE`]: at 1024×768 (diag exactly 1280) the 0.1 × 0.025 frame = 128 × 32 px
    /// — the border texture's native size, the bar ≈ 103 × 9 px. Past the knee the frame lands
    /// midway between the byte law and native (1080p: byte law 220, native 128 → ours 174).
    /// Below the knee the diagonal rules unchanged (a smaller window shrinks the plate, §8 Q4).
    #[test]
    fn plate_geometry_damps_past_the_native_knee() {
        let ref43 = plate_basis(Vec2::new(1024.0, 768.0));
        assert_eq!(gx_px(PLATE_W, ref43), 128.0);
        assert_eq!(gx_px(PLATE_H, ref43), 32.0);
        assert_eq!(gx_px(BAR_W, ref43), 103.0);
        assert_eq!(gx_px(BAR_H, ref43), 9.0);
        assert_eq!(
            gx_px(PLATE_W, plate_basis(Vec2::new(1920.0, 1080.0))),
            174.0,
            "midway between 220 and 128"
        );
        assert_eq!(gx_px(PLATE_W, plate_basis(Vec2::new(800.0, 600.0))), 100.0);
    }
}
