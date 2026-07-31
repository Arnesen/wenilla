//! The mouse cursor — the real client's `Interface\Cursor\*.blp` set, shown as a crisp **hardware
//! (OS-composited) cursor** so it has zero lag, matching the reference 1.12 client (whose cursor is
//! lag-free on this same machine). Which cursor shows is the targeting classifier's call
//! ([`crate::target::WorldCursor`], the wow-re cursor RE's decision tree): Point by default, the sword over an
//! attackable unit, the speech bubble / pouch / trainer / taxi over service NPCs, loot/skin over
//! corpses — each with its grayed `Unable*` twin out of range. The whole set preloads at startup
//! (18 tiny BLPs); a stem missing from the archives falls back toward the base cursor, then Point.
//!
//! macOS needs native AppKit. winit drives the cursor through the legacy cursor-rect API
//! (`addCursorRect`/`resetCursorRects`), which a continuously-redrawing Metal view doesn't honor on
//! mouse-move — so winit's `CursorIcon::Custom` and `CursorOptions.visible = false` both revert to the
//! arrow the instant you move (verified against winit's source). We bypass that: build an `NSCursor`
//! per mode from its BLP, call `NSWindow::disableCursorRects` to stop AppKit's cursor-rect
//! reconciliation, then `[cursor set]` the current mode directly (re-asserted each non-look frame).
//! During mouselook we `NSCursor::hide()`/`unhide()` (the reliable app-global hide counter). Other
//! platforms use winit's `CursorIcon::Custom` (which works there), swapped on mode change; their
//! hide-on-look goes through `CursorOptions.visible`, handled in `player::control`.
//!
//! **The held cursor payload** (decision 0216 §5, wow-re cursor-system.md §1 VERIFIED): while
//! `UiScript::cursor_payload()` holds a payload with a resolved icon, the HARDWARE cursor becomes
//! that icon instead of the classified mode — the real client composites the item's `Interface\
//! Icons\…` art into its drag bitmap and uploads it via the same `SetHardwareCursor` path the mode
//! art uses. Both platform `drive` fns below check the held payload FIRST, each frame, and fall
//! back to the mode cursor when nothing is held or its icon fails to decode; each caches its built
//! cursor per icon path ([`other::PayloadCursorImages`]/[`macos::PayloadCursors`]) — repeated
//! pickups of the same icon never re-decode. The icons are 64×64 (unlike the mode BLPs, already
//! 32×32); [`box_downsample_32`] shrinks one to the cursor's fixed 32×32 with hotspot `(0, 0)` —
//! the pointer sits at the icon's top-left corner, the icon hangs down-right, matching the
//! reference look (and `ui_script::extract::cursor_icon_quad`'s capture-only stand-in).

use crate::assets::AssetSet;
use bevy::prelude::*;

/// The held cursor payload's icon path (`Interface\Icons\…`, extensionless — the DBC/FrameXML
/// convention), any arm — `None` if nothing is held or that arm's icon hasn't resolved yet
/// (either way, the caller falls back to the mode cursor).
fn payload_icon(script: &benilla_ui::script::UiScript) -> Option<String> {
    use benilla_ui::script::CursorPayload;
    match script.cursor_payload()? {
        CursorPayload::Item(i) => i.texture,
        CursorPayload::Spell(s) => s.texture,
        CursorPayload::Action(a) => a.texture,
    }
}

/// Box-downsample a raw RGBA8 image (top-to-bottom raster order —
/// [`crate::assets::WorldAssets::decode_rgba`]'s layout) to a fixed 32×32 buffer: each output
/// texel averages a `(w/32)×(h/32)` block of the source. Vanilla item icons are 64×64 — a clean
/// 2×2 average per output texel (decision 0216 §5) — but this degrades gracefully for any other
/// source size.
fn box_downsample_32(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    const OUT: u32 = 32;
    let mut out = vec![0u8; (OUT * OUT * 4) as usize];
    if w == 0 || h == 0 {
        return out;
    }
    for oy in 0..OUT {
        let y0 = oy * h / OUT;
        let y1 = ((oy + 1) * h / OUT).max(y0 + 1).min(h);
        for ox in 0..OUT {
            let x0 = ox * w / OUT;
            let x1 = ((ox + 1) * w / OUT).max(x0 + 1).min(w);
            let mut sum = [0u32; 4];
            let mut n = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * w + x) * 4) as usize;
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s += u32::from(rgba[i + c]);
                    }
                    n += 1;
                }
            }
            let o = ((oy * OUT + ox) * 4) as usize;
            for (c, byte) in out[o..o + 4].iter_mut().enumerate() {
                *byte = (sum[c] / n) as u8;
            }
        }
    }
    out
}

/// Decode a payload icon and box-downsample it to a 32×32 hardware-cursor-ready RGBA8 buffer
/// ([`box_downsample_32`]), alpha forced fully opaque — the client's own drag-bitmap composite
/// (byte-verified, wow-re cursor-dragdrop-payload.md: 64×64 → 2×2 box filter → 32×32, alpha
/// written 0xFF; folded back by 0218). `None` on a missing/undecodable icon.
fn decode_payload_cursor_rgba(
    assets: &mut crate::assets::WorldAssets,
    path: &str,
) -> Option<Vec<u8>> {
    let (w, h, rgba) = assets.decode_rgba(path)?;
    let mut out = box_downsample_32(w, h, &rgba);
    for a in out.iter_mut().skip(3).step_by(4) {
        *a = 0xFF;
    }
    Some(out)
}

/// Every cursor BLP stem benilla can currently show ([`crate::target::WorldCursor::stem`]): the
/// classifier's modes + their grayed twins. Preloaded once at startup.
const CURSOR_STEMS: &[&str] = &[
    "Point",
    "Attack",
    "UnableAttack",
    "Speak",
    "UnableSpeak",
    "Pickup",
    "UnablePickup",
    "Interact",
    "UnableInteract",
    "Buy",
    "UnableBuy",
    "Inspect", // the Ctrl-hover magnifier (ShowInspectCursor) AND the TEXT-GameObject plaque cursor
    "UnableInspect",
    "Trainer",
    "UnableTrainer",
    "Taxi",
    "UnableTaxi",
    "Skin",
    "UnableSkin",
    "Repair", // the repair-mode base cursor (never grayed — the shipped UnableRepair is unreachable)
    // The data-driven GameObject cursors (decision 0236, wow-re cursor-system §4): a mailbox's Mail,
    // a lock's Mine / GatherHerbs (grayed out of reach), a picked lock's PickLock (never grayed).
    "Mail",
    "UnableMail",
    "Mine",
    "UnableMine",
    "GatherHerbs",
    "UnableGatherHerbs",
    "PickLock",
    // The spell-targeting pair (wow-re cursor-system.md §5): Cast(2) / UnableCast(22). Cast is
    // the armed-enchant-pick overlay's mode; the grayed twin preloads with it (the overlay's
    // valid/invalid split is its named refinement).
    "Cast",
    "UnableCast",
];

/// A cursor stem's archive path.
fn cursor_path(stem: &str) -> String {
    format!("Interface\\Cursor\\{stem}.blp")
}

/// The **displayed** cursor — the world classifier's base [`crate::target::WorldCursor`] with the
/// UI overlays applied, mirroring the client's displayed-vs-base mode pair (`0xbe2c2c`/`0xbe2c4c`,
/// wow-re cursor-system.md §7): a world classification always wins; over plain UI, the repair-mode
/// latch shows Repair(17) (the locked base `ShowRepairCursor` sets), else the bag hover's sell
/// latch shows Buy(3) (armed only while the base is Point — `ShowContainerSellCursor 0x4fa460`).
/// Read by the platform drivers instead of `WorldCursor` directly.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct DisplayedCursor(pub(crate) crate::target::WorldCursor);

fn overlay_ui_cursor(
    base: Res<crate::target::WorldCursor>,
    script: Option<bevy::ecs::system::NonSend<benilla_ui::script::UiScript>>,
    pending_pick: Option<Res<crate::ui_craft::PendingItemCast>>,
    mut displayed: ResMut<DisplayedCursor>,
) {
    use crate::target::{CursorKind, WorldCursor};
    use benilla_ui::script::UiCursorMode;
    let mut out = *base;
    // Spell-targeting pre-empts the WHOLE classifier — the real dispatcher's step 2 runs before
    // any object resolve (wow-re cursor-system.md §5, VERIFIED; the 0446/0452 named deferral).
    // benilla's one targeting state is the armed enchant pick. INTERIM: plain Cast everywhere
    // while armed — the ref's valid/invalid split (Cast in range / UnableCast out, computed by
    // `0x6e6810`/`0x6e6350`) needs the item-target validity law derived before it's honest to
    // gray anything.
    if pending_pick.is_some_and(|p| p.spell_id != 0) {
        displayed.0 = WorldCursor {
            kind: CursorKind::Cast,
            unable: false,
        };
        return;
    }
    if out == WorldCursor::default() {
        if let Some(script) = script.as_ref() {
            // Repair is the locked base-mode override (`ShowRepairCursor`) and wins over the
            // displayed-cursor family — the real client parks the base at Repair, so the sell/
            // inspect gate (base == Point) bails while it holds (wow-re cursor-system.md §7).
            if script.repair_mode() {
                out.kind = CursorKind::Repair;
            } else {
                match script.ui_cursor() {
                    Some(UiCursorMode::Buy) => out.kind = CursorKind::Buy,
                    Some(UiCursorMode::UnableBuy) => {
                        out = WorldCursor {
                            kind: CursorKind::Buy,
                            unable: true,
                        }
                    }
                    Some(UiCursorMode::Inspect) => out.kind = CursorKind::Inspect,
                    None => {}
                }
            }
        }
    }
    displayed.0 = out;
}

pub(crate) struct CursorPlugin;

impl Plugin for CursorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DisplayedCursor>();
        #[cfg(target_os = "macos")]
        app.add_systems(Startup, macos::setup.after(AssetSet::Open))
            .add_systems(Update, (overlay_ui_cursor, macos::drive).chain());
        #[cfg(not(target_os = "macos"))]
        app.init_resource::<other::PayloadCursorImages>()
            .add_systems(Startup, other::setup.after(AssetSet::Open))
            .add_systems(Update, (overlay_ui_cursor, other::drive).chain());
    }
}

/// Non-macOS: winit's custom OS cursor works fine — preload every mode's image and swap the
/// window's `CursorIcon` when the classified mode changes. Hide-on-look is `CursorOptions.visible`,
/// driven by `player::control`.
#[cfg(not(target_os = "macos"))]
mod other {
    use super::{cursor_path, payload_icon, CURSOR_STEMS};
    use crate::assets::{cursor_texture, WorldAssets};
    use bevy::platform::collections::{HashMap, HashSet};
    use bevy::prelude::*;
    use bevy::window::{CursorIcon, CustomCursor, CustomCursorImage, PrimaryWindow};

    /// stem → decoded cursor image, preloaded at startup.
    #[derive(Resource, Default)]
    pub(super) struct CursorImages(HashMap<String, Handle<Image>>);

    /// Held-payload icon path → its decoded 32×32 hardware cursor (decision 0216 §5), built
    /// lazily on first use (unlike [`CursorImages`]'s fixed startup preload — the icon set is far
    /// too large to preload) and cached so a repeated pickup never re-decodes.
    #[derive(Resource, Default)]
    pub(super) struct PayloadCursorImages(HashMap<String, Handle<Image>>);

    pub(super) fn setup(
        mut commands: Commands,
        world_assets: Option<ResMut<WorldAssets>>,
        mut images: ResMut<Assets<Image>>,
    ) {
        let Some(mut world_assets) = world_assets else {
            return;
        };
        let mut map = HashMap::default();
        for stem in CURSOR_STEMS {
            match world_assets.decode_cursor(&cursor_path(stem), &mut images) {
                Some(handle) => {
                    map.insert(stem.to_string(), handle);
                }
                None => warn!("cursor {stem}.blp missing/undecodable — mode falls back"),
            }
        }
        commands.insert_resource(CursorImages(map));
    }

    /// Each frame: while a cursor payload with a resolved icon is held, show ITS 32×32 hardware
    /// cursor (decoded/downsampled on first use, then cached by path); otherwise swap to the
    /// classified mode (base-stem fallback, then Point, then OS) — unchanged from before 0216 §5.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn drive(
        mut commands: Commands,
        cursor: Res<super::DisplayedCursor>,
        cursors: Option<Res<CursorImages>>,
        script: Option<NonSend<benilla_ui::script::UiScript>>,
        world_assets: Option<ResMut<WorldAssets>>,
        mut images: ResMut<Assets<Image>>,
        mut payload_cursors: ResMut<PayloadCursorImages>,
        window: Option<Single<Entity, With<PrimaryWindow>>>,
        mut current: Local<Option<String>>,
        mut decode_failed: Local<HashSet<String>>,
    ) {
        let Some(window) = window else {
            return;
        };
        let held_icon = script.as_ref().and_then(|s| payload_icon(s));
        if let Some(icon) = held_icon {
            if current.as_deref() == Some(icon.as_str()) {
                return; // already showing this icon
            }
            if !payload_cursors.0.contains_key(&icon) && !decode_failed.contains(&icon) {
                let built = world_assets.and_then(|mut a| {
                    let rgba = super::decode_payload_cursor_rgba(&mut a, &icon)?;
                    Some(images.add(cursor_texture(32, 32, rgba)))
                });
                match built {
                    Some(handle) => {
                        payload_cursors.0.insert(icon.clone(), handle);
                    }
                    None => {
                        warn!("cursor payload icon {icon} missing/undecodable — mode cursor kept");
                        decode_failed.insert(icon.clone());
                    }
                }
            }
            if let Some(handle) = payload_cursors.0.get(&icon) {
                set_custom_cursor(&mut commands, *window, handle.clone());
                *current = Some(icon);
                return;
            }
            // Decode failed (or still pending catalogs): fall through to the mode cursor below.
        }

        let Some(cursors) = cursors else {
            return;
        };
        let stem = cursor.0.stem();
        if current.as_deref() == Some(stem.as_str()) {
            return;
        }
        let handle = cursors
            .0
            .get(&stem)
            .or_else(|| cursors.0.get(stem.trim_start_matches("Unable")))
            .or_else(|| cursors.0.get("Point"));
        if let Some(handle) = handle {
            set_custom_cursor(&mut commands, *window, handle.clone());
        }
        *current = Some(stem);
    }

    /// The shared `CustomCursor::Image` insert — hotspot `(0, 0)`: the vanilla mode cursors' active
    /// tip is their top-left pixel, and the held-payload icon is downsampled to match that same
    /// top-left hotspot convention (decision 0216 §5).
    fn set_custom_cursor(commands: &mut Commands, window: Entity, handle: Handle<Image>) {
        commands
            .entity(window)
            .insert(CursorIcon::Custom(CustomCursor::Image(CustomCursorImage {
                handle,
                texture_atlas: None,
                flip_x: false,
                flip_y: false,
                rect: None,
                hotspot: (0, 0),
            })));
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::collections::{HashMap, HashSet};

    use crate::assets::LockRecover;
    use crate::assets::WorldAssets;
    use crate::player::CameraControl;
    use benilla_formats::read_texture_rgba;
    use bevy::prelude::*;
    use objc2::rc::Retained;
    use objc2::ClassType; // brings `alloc()` into scope
    use objc2_app_kit::{NSApplication, NSCursor, NSImage};
    use objc2_foundation::{MainThreadMarker, NSData, NSPoint};

    use super::{cursor_path, payload_icon, CURSOR_STEMS};

    /// The built `NSCursor` per mode stem. Held as a non-send resource because AppKit types aren't
    /// `Send`/`Sync` and must only be touched on the main thread. `pub(super)` only so it can appear
    /// in `drive`'s param list (the plugin registers `drive` from the parent module).
    pub(super) struct NativeCursors(HashMap<&'static str, Retained<NSCursor>>);

    /// Held-payload icon path → its built 32×32 `NSCursor` (decision 0216 §5) — the mac twin of
    /// [`super::other::PayloadCursorImages`], built lazily on first use and cached by path.
    pub(super) struct PayloadCursors(HashMap<String, Retained<NSCursor>>);

    /// Build every mode's `NSCursor` from its BLP and store them. An **exclusive** system so it runs
    /// on the main thread (AppKit's requirement) and can insert the non-send resource. The
    /// window-level `disableCursorRects` + first `set` happen in [`drive`], once the window
    /// definitely exists.
    pub(super) fn setup(world: &mut World) {
        // Inserted unconditionally (ahead of the native-cursor early-return below) — `drive` reads
        // it every frame regardless of whether any MODE cursor decoded.
        world.insert_non_send_resource(PayloadCursors(HashMap::new()));
        let mut cursors = HashMap::new();
        for stem in CURSOR_STEMS {
            let Some((w, h, rgba)) = world.get_resource_mut::<WorldAssets>().and_then(|wa| {
                read_texture_rgba(&mut wa.chain.lock_recover(), &cursor_path(stem)).ok()
            }) else {
                warn!("cursor {stem}.blp missing/undecodable — mode falls back");
                continue;
            };
            match build_cursor(w, h, &rgba) {
                Some(cursor) => {
                    cursors.insert(*stem, cursor);
                }
                None => warn!("failed to build NSCursor for {stem}.blp — mode falls back"),
            }
        }
        if cursors.is_empty() {
            warn!("no cursor BLPs available — using the OS cursor");
            return;
        }
        world.insert_non_send_resource(NativeCursors(cursors));
    }

    /// Each frame (main thread, forced by the `NonSend` params): the first frame, stop AppKit's
    /// cursor-rect reconciliation on our window (the thing that reverts winit's cursor on move);
    /// then, while not looking, assert the cursor to show — the held payload's icon (decision 0216
    /// §5, decoded/downsampled to 32×32 on first use and cached by path) if one is held and
    /// resolved, else the classified mode (base-stem fallback, then Point); on entering/leaving
    /// mouselook, hide/show it via the app-global hide counter.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn drive(
        cursors: Option<NonSend<NativeCursors>>,
        mut payload_cursors: NonSendMut<PayloadCursors>,
        script: Option<NonSend<benilla_ui::script::UiScript>>,
        world_assets: Option<ResMut<WorldAssets>>,
        mode: Res<super::DisplayedCursor>,
        rig: Res<CameraControl>,
        mut focus: MessageReader<bevy::window::WindowFocused>,
        mut was_looking: Local<bool>,
        mut rects_disabled: Local<bool>,
        mut decode_failed: Local<HashSet<String>>,
        mut last_set: Local<Option<String>>,
        // The raw pointer of the NSCursor we last `set` — the drift detector's baseline
        // (`WOW_CURSOR_TRACE`). Kept alive by the `cursors`/`payload_cursors` caches, and only
        // ever compared, never dereferenced.
        mut last_ptr: Local<usize>,
    ) {
        let Some(cursors) = cursors else {
            return;
        };
        // macOS resets the app's cursor to the arrow on every activation — a set that happened
        // while we weren't key (the very first frames run before the window shows; any cmd-tab
        // away) is simply lost. Re-assert on each focus gain, the moments AppKit forgets us.
        // (Before this, the glue screens showed the OS arrow: their cursor never changes off
        // Point, so nothing ever re-asserted after activation; in-world the constant mouselook
        // transitions masked it.)
        for ev in focus.read() {
            if ev.focused {
                *last_set = None;
            }
        }
        let held_icon = script.as_ref().and_then(|s| payload_icon(s));
        if let Some(icon) = &held_icon {
            if !payload_cursors.0.contains_key(icon) && !decode_failed.contains(icon) {
                let built = world_assets.and_then(|mut a| {
                    let rgba = super::decode_payload_cursor_rgba(&mut a, icon)?;
                    build_cursor(32, 32, &rgba)
                });
                match built {
                    Some(cursor) => {
                        payload_cursors.0.insert(icon.clone(), cursor);
                    }
                    None => {
                        warn!("cursor payload icon {icon} missing/undecodable — mode cursor kept");
                        decode_failed.insert(icon.clone());
                    }
                }
            }
        }
        let looking = rig.is_looking();
        let stem = mode.0.stem();
        // The held payload's cursor wins over the classified mode (falls back to the mode cursor
        // when nothing is held, or its icon hasn't resolved/decoded this frame). The KEY names what
        // we chose, so the set below fires only on a real cursor change or a detected drift —
        // `NSCursor::set` is a WindowServer round-trip that intermittently stalls the main thread
        // for milliseconds, and asserting it unconditionally every frame was the single biggest
        // line of the 0366 frame-time tail. `disableCursorRects` (below, once) stops AppKit's
        // cursor-RECT reconciliation (the every-mouse-move revert); the drift check in the set
        // block below catches what that can't — the other in-process `set` callers.
        let key = held_icon
            .as_ref()
            .filter(|icon| payload_cursors.0.contains_key(icon.as_str()))
            .map(|icon| format!("payload:{icon}"))
            .unwrap_or_else(|| format!("mode:{stem}"));
        let cursor = held_icon
            .as_ref()
            .and_then(|icon| payload_cursors.0.get(icon.as_str()))
            .or_else(|| cursors.0.get(stem.as_str()))
            .or_else(|| cursors.0.get(stem.trim_start_matches("Unable")))
            .or_else(|| cursors.0.get("Point"));
        // SAFETY: main thread (guaranteed by the `NonSend` params). hide/unhide are balanced across
        // the look transition so the app-global hide counter never drifts.
        unsafe {
            if !*rects_disabled {
                if let Some(mtm) = MainThreadMarker::new() {
                    if let Some(window) = NSApplication::sharedApplication(mtm)
                        .windows()
                        .firstObject()
                    {
                        window.disableCursorRects();
                        *rects_disabled = true;
                    }
                }
            }
            if looking && !*was_looking {
                NSCursor::hide();
                // The next un-look must re-assert even an unchanged cursor (the hidden period is
                // AppKit's to fiddle with).
                *last_set = None;
            } else if !looking && *was_looking {
                NSCursor::unhide();
            }
            if !looking {
                // Assert on a key change OR on detected **drift**. The set is not fire-and-forget:
                // other in-process actors — winit's AppKit view (tracking-area `cursorUpdate:` /
                // `mouseEntered:` handlers) and AppKit's own activation resets — call `[NSCursor
                // set]` with THEIR cursor (the arrow) at unpredictable moments, replacing ours.
                // The mode cursors self-healed by accident (their `key` churns as the hover
                // classification changes), but a held payload's key is CONSTANT for the whole
                // carry, so one usurped set left the icon gone for good while the payload was
                // still held — the "spell disappears from the cursor but is still stuck to it"
                // bug (reproduced via WOW_CURSOR_TRACE; the drift fired with no input at all).
                // `currentCursor` is the app-local top of the cursor stack — a plain ObjC read,
                // no WindowServer round-trip — so checking it every frame is free, and `set`
                // still fires only on change-or-drift, preserving the 0366 frame-time rule.
                let drifted = *last_ptr != 0
                    && Retained::as_ptr(&NSCursor::currentCursor()) as usize != *last_ptr;
                if drifted || last_set.as_deref() != Some(key.as_str()) {
                    if let Some(cursor) = cursor {
                        cursor.set();
                        *last_ptr = Retained::as_ptr(cursor) as usize;
                        // `WOW_CURSOR_TRACE=1` — the assert/usurp timeline instrument.
                        if std::env::var_os("WOW_CURSOR_TRACE").is_some() {
                            eprintln!(
                                "[cursor-trace] set {key} ({:#x}){}",
                                *last_ptr,
                                if drifted { " [healed drift]" } else { "" }
                            );
                        }
                        *last_set = Some(key);
                    }
                }
            }
        }
        *was_looking = looking;
    }

    /// RGBA → `NSCursor`. Goes via PNG + `NSImage::initWithData` (standard image data) rather than
    /// hand-rolling an `NSBitmapImageRep`.
    fn build_cursor(width: u32, height: u32, rgba: &[u8]) -> Option<Retained<NSCursor>> {
        let buf = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .ok()?;
        let data = NSData::with_bytes(&png);
        let image = NSImage::initWithData(NSImage::alloc(), &data)?;
        // Hotspot = the vanilla cursors' top-left active tip.
        Some(NSCursor::initWithImage_hotSpot(
            NSCursor::alloc(),
            &image,
            NSPoint { x: 0.0, y: 0.0 },
        ))
    }
}
