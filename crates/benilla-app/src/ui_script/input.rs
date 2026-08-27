//! The player-UI input pass: [`feed_ui_input`] hit-tests the cursor and dispatches mouse/keyboard
//! events into the UI engine (after [`super::extract::drive_script`] has resolved the frame's
//! rects), plus the action-bar key map. The OS pasteboard itself lives in [`crate::textinput`].
//! Split out of [`super`] purely for size — the plugin wiring and the extraction pass live there
//! and in [`super::extract`] respectively.

use bevy::input::keyboard::KeyboardInput;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::input::ButtonState;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use benilla_ui::script::UiScript;

use super::{CursorPayloadHeld, PlayerUiClickConsumed, PlayerUiHover, UiKeyboardCapture};
use crate::textinput::{self, keymap, HostClipboard};

/// The pointer-side state [`feed_ui_input`] reads and writes, as one
/// [`bevy::ecs::system::SystemParam`] (the argument ceiling): the UI hover + click-consumed
/// outputs, the world pick inputs (LAST frame's hovered unit/GameObject + the occlusion ray —
/// the target chain runs after this pass; a frame's staleness is within the pick's own
/// tolerance) that route the world-click payload legs (decisions 0571 + 0574), and the
/// payload-held mirror.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct PointerFeed<'w> {
    hover: ResMut<'w, PlayerUiHover>,
    click_consumed: ResMut<'w, PlayerUiClickConsumed>,
    hovered: Res<'w, crate::target::Hovered>,
    hovered_object: Res<'w, crate::target::HoveredObject>,
    occlusion: Res<'w, crate::target::PickOcclusion>,
    payload_held: ResMut<'w, CursorPayloadHeld>,
    /// TOGGLEUI ([`crate::ui_hide::UiHidden`]): a hidden UI takes no mouse at all — it rides here
    /// because that is precisely the pointer half of this pass.
    hidden: Res<'w, crate::ui_hide::UiHidden>,
    /// A headless probe is driving the pointer itself ([`super::SyntheticPointer`]) — the real
    /// cursor must not fight it. Same arm as `hidden` below, for the same reason: the mouse half
    /// of this pass is not ours this frame.
    synthetic: Res<'w, super::SyntheticPointer>,
    /// A capture owns the pointer for its whole run ([`super::CapturePointerPinned`]) — the OS
    /// cursor belongs to whoever is at the keyboard and must not reach the shot.
    capture_pinned: Res<'w, super::CapturePointerPinned>,
}

impl PointerFeed<'_> {
    /// The reference's click-time pick state, derived from what benilla already tracks: a
    /// hovered unit/GameObject is an `Object` pick; else a finite occlusion-ray hit (terrain/
    /// WMO/doodad under the cursor) is `Terrain`; else `Nothing` (sky). Decision 0574's
    /// terrain-vs-nothing split rides the occlusion ray the unit pick already casts.
    fn world_pick(&self) -> benilla_ui::script::WorldPick {
        use benilla_ui::script::WorldPick;
        if self.hovered.target.is_some() || self.hovered_object.target.is_some() {
            WorldPick::Object
        } else if self.occlusion.distance.is_finite() {
            WorldPick::Terrain
        } else {
            WorldPick::Nothing
        }
    }
}

/// Feed the window's cursor + buttons + wheel + keyboard into the UI engine (after
/// [`super::extract::drive_script`] has resolved this frame's rects), firing
/// OnEnter/OnLeave/OnClick/OnMouseWheel and the EditBox
/// char/key dispatch, publishing [`PlayerUiHover`] (so the pointer arbiter yields world-pick/camera to
/// the UI) and [`UiKeyboardCapture`] (so gameplay/dev keyboard readers yield to a focused box).
///
/// Runs in [`UiInput`], before `WorldStage::Input` and every other keyboard reader — a key a focused
/// box consumes must never also reach the world in the same frame.
pub(super) fn feed_ui_input(
    script: Option<NonSendMut<UiScript>>,
    // The window's raw handle rides along with it: on Wayland it carries the `wl_display` the
    // clipboard backend is built from (decision 0702). `Option`, because it only appears once
    // winit has actually created the surface.
    window: Query<(&Window, Option<&bevy::window::RawHandleWrapper>), With<PrimaryWindow>>,
    buttons: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    // One [`PointerFeed`] (clippy's argument ceiling): the hover + click-consumed outputs this
    // pass writes, the world pick that routes the world-click payload legs (decision 0571), and
    // the payload-held mirror written for the Send-side world-click consumers.
    mut pointer: PointerFeed,
    // Bundled into one param (clippy's argument ceiling): this frame's key messages, the modifier
    // mirror, the capture gate the keyboard feed writes, and the held OS pasteboard the three
    // clipboard chords resolve against (decision 0702).
    mut kbd: (
        MessageReader<KeyboardInput>,
        Res<ButtonInput<KeyCode>>,
        ResMut<UiKeyboardCapture>,
        NonSendMut<HostClipboard>,
    ),
    // The uiScale dial folded into the seam scale (decision 0584).
    ui_scale: Res<super::UiScaleCvar>,
) {
    let (keyboard, keys, capture, clipboard) = (&mut kbd.0, &kbd.1, &mut kbd.2, &mut kbd.3);
    let world_pick = pointer.world_pick();
    let ui_hidden = pointer.hidden.0;
    // A headless probe is driving the pointer itself this frame (the drag probe). It takes the
    // WHOLE mouse half out — including the else-arm below, which is the half that matters: a
    // `pointer_left_window` between the synthetic press and the synthetic release would disarm
    // the gesture the probe is in the middle of.
    // The two reasons the OS pointer is not ours this frame: a probe is driving a gesture through
    // the real pointer path, or this is a capture (where it is never ours). Same treatment — skip
    // the mouse half whole, touch nothing — so they read as one condition here.
    let synthetic = pointer.synthetic.0 || pointer.capture_pinned.0;
    let (hover, click_consumed, payload_held) = (
        &mut pointer.hover,
        &mut pointer.click_consumed,
        &mut pointer.payload_held,
    );
    click_consumed.0 = false;
    let Some(mut script) = script else {
        capture.0 = false;
        payload_held.0 = false;
        return;
    };
    let Ok((window, raw_handle)) = window.single() else {
        capture.0 = false;
        payload_held.0 = false;
        return;
    };
    // `Some` only on a Wayland session — the signal the clipboard backend picks itself by.
    let wl_display = textinput::wayland_display(raw_handle);
    // The engine's world-drop routing (decisions 0571 + 0574): an object pick keeps every
    // payload (the reference's object leg dispatches SELECT with the item still held), terrain
    // drops items only, nothing drops any arm.
    script.set_world_pick(world_pick);
    // ── Modifiers ── pushed BEFORE the mouse feed: a click handler's modifier fork
    // (`IsShiftKeyDown` — the reference's shift-split/ctrl-dressup/shift-pickup) reads the state
    // as of the click, not last frame's.
    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    script.set_modifiers(shift, ctrl, alt);
    // ── Mouse ── (cursor-off-window only skips the mouse feed; keyboard still flows to a focused box)
    // A UI hidden by TOGGLEUI takes the same route as a cursor outside the window: an action bar
    // nobody can see must not eat the click or arm a tooltip, and the else-arm below is exactly the
    // "no pointer here" bookkeeping (leave the hovered frame once, disarm any press/drag every
    // frame) that keeps a stale gesture from firing when the UI comes back.
    if let Some(cursor) = window
        .cursor_position()
        .filter(|_| !ui_hidden && !synthetic)
    {
        // Window cursor is logical px, y-down from top-left; the UI is y-up 768-virtual units
        // (decisions 0582 + 0584's uiScale dial) — flip through the window height, then ÷s into
        // the VM's space (the inverse of the extract seam's ×s).
        let s = super::seam_scale(window.height(), ui_scale.0);
        let (x, y) = (cursor.x / s, (window.height() - cursor.y) / s);
        // `WOW_HIT_COST=1` — the hit-test lane's own meter (the resolve-lap report's W9): this
        // call rebuilds `order::traversal` + sort + the scroll-clip map EVERY frame the cursor
        // is inside the window, moving or not — a real-play cost every parked probe leg is
        // structurally blind to (probes park the cursor outside). One aggregate line per
        // second, census-meter posture; the number decides whether the epoch-gated skip is
        // worth building, before it is built.
        static HIT_COST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let metering =
            *HIT_COST.get_or_init(|| std::env::var("WOW_HIT_COST").as_deref() == Ok("1"));
        let t0 = metering.then(std::time::Instant::now);
        hover.0 = script.mouse_move(x, y);
        if let Some(t0) = t0 {
            use std::sync::atomic::{AtomicU64, Ordering};
            static ACC_US: AtomicU64 = AtomicU64::new(0);
            static CALLS: AtomicU64 = AtomicU64::new(0);
            static LAST: AtomicU64 = AtomicU64::new(0);
            ACC_US.fetch_add(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
            let calls = CALLS.fetch_add(1, Ordering::Relaxed) + 1;
            let now_s = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if LAST.swap(now_s, Ordering::Relaxed) != now_s && calls > 1 {
                let us = ACC_US.swap(0, Ordering::Relaxed);
                let n = CALLS.swap(0, Ordering::Relaxed);
                eprintln!(
                    "[hit-cost] mouse_move_us/frame={:.1} frames={n}",
                    us as f64 / n.max(1) as f64
                );
            }
        }
        for (btn, name) in [
            (MouseButton::Left, "LeftButton"),
            (MouseButton::Right, "RightButton"),
            (MouseButton::Middle, "MiddleButton"),
        ] {
            if buttons.just_pressed(btn) {
                // A LEFT press that would complete as a world DROP belongs to the drop flow
                // (the drop itself fires on the completed click's RELEASE — 0218's
                // byte-verified trigger — but the press is when the world click-pick and
                // camera orbit-start would act, so they must yield now, exactly as for a
                // hovered click). "Would drop" mirrors `world_drop_click`'s pick routing
                // (decisions 0571 + 0574, amended by 0843): ANY payload over terrain/nothing
                // drops (the item's popup, the spell/action's silent dismiss). Only a payload
                // over an OBJECT is not consumed — the reference runs SELECT with the payload
                // still held, so that click must reach the world.
                if btn == MouseButton::Left && hover.0.is_none() {
                    use benilla_ui::script::WorldPick;
                    let would_drop =
                        script.cursor_payload().is_some() && world_pick != WorldPick::Object;
                    if would_drop {
                        click_consumed.0 = true;
                    }
                }
                script.mouse_button(x, y, name, true);
            }
            if buttons.just_released(btn) {
                script.mouse_button(x, y, name, false);
            }
        }
        if scroll.delta.y != 0.0 {
            script.mouse_wheel(x, y, scroll.delta.y);
        }
    } else if !synthetic {
        // The OS pointer left the window: leave whatever frame was hovered (once, on the
        // Some→None transition) and — every frame it stays outside — clear any armed press/drag
        // (`UiScript::pointer_left_window`), since no release is ever fed to end it and a stale
        // gesture would fire a spurious `OnDragStart`/`OnClick` on re-entry.
        if hover.0.take().is_some() {
            script.mouse_move(f32::MIN, f32::MIN);
        }
        script.pointer_left_window();
    }

    // ── Keyboard capture gate ── read AFTER the mouse feed (a LeftButton click may have just focused a
    // box) but BEFORE feeding keys (an Escape that clears focus is still "captured" this frame, so the
    // world doesn't also act on it). Gameplay/dev readers run after `UiInput` and see this value.
    capture.0 = script.has_keyboard_focus();

    // ── Keyboard → VM ── the three box-event keys route to `key_input` by name (and never also
    // as text, so Enter isn't a stray newline); every *editing* key goes through the per-OS
    // chord table ([`keymap::chord`]) and reaches the box as a semantic `EditAction` — or as
    // one of the clipboard operations, handled here (this is a NonSend, main-thread system —
    // required for the macOS NSPasteboard). What's left goes to `char_input`, minus
    // command-modified chars (Cmd/Ctrl+letter must never type the letter — except Ctrl+Alt, the
    // AltGr plane European layouts type real characters with). Repeats carry `state == Pressed`,
    // so held keys repeat. Modifiers reuse the mirror read above (the message carries none).
    let sup = keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight);
    let mods = keymap::Mods {
        shift,
        ctrl,
        alt,
        sup,
    };
    let mac = cfg!(target_os = "macos");
    for ev in keyboard.read() {
        if ev.state != ButtonState::Pressed {
            continue;
        }
        let named = match ev.key_code {
            KeyCode::Enter | KeyCode::NumpadEnter => Some("ENTER"),
            KeyCode::Escape => Some("ESCAPE"),
            KeyCode::Tab => Some("TAB"),
            _ => None,
        };
        // The editing keys/chords — dispatched unconditionally like the named keys (the engine's
        // route law decides; unfocused they return unconsumed, and the camera/turn keys that
        // read arrows after UiInput see them exactly as before).
        let chord = keymap::chord(ev.key_code, mods, mac);
        // A KEYBOARD FRAME gets these by NAME before their chord runs (decision 1319). They are
        // the keys a focused EditBox receives as a semantic `EditAction` instead of a name, so
        // they never reached `key_input` at all — and a dialog you type into needs its BACKSPACE.
        // `frame_key_input` walks in the engine's own order and declines at a focused box, so the
        // box's editing keys can never be stolen by a frame below it; a `true` here means a frame
        // consumed the key, which suppresses BOTH the chord below and the key's binding (the
        // reference's existence gate, wow-re `frame-key-script-delivery.md` §3).
        let frame_named = match ev.key_code {
            KeyCode::Backspace => Some("BACKSPACE"),
            KeyCode::Delete => Some("DELETE"),
            KeyCode::ArrowLeft => Some("LEFT"),
            KeyCode::ArrowRight => Some("RIGHT"),
            KeyCode::ArrowUp => Some("UP"),
            KeyCode::ArrowDown => Some("DOWN"),
            KeyCode::Home => Some("HOME"),
            KeyCode::End => Some("END"),
            _ => None,
        };
        if let Some(name) = frame_named {
            if script.frame_key_input(name) {
                capture.0 = true; // consumed by a frame: its binding must not also fire
                continue;
            }
        }
        if let Some(name) = named {
            // The three box-event keys. An unconsumed press is not acted on here: every GAME
            // action of a key — ESCAPE's close/cancel ladder (TOGGLEGAMEMENU), TAB's targeting,
            // ENTER's chat open, the number row's action buttons — now lives in the binding
            // dispatch (decision 0997, `crate::bindings`), which runs right after this pass and
            // reads the capture gate written above, so a key a focused box consumed this frame
            // never also fires its binding — the real client's ESC precedence, table-wide.
            // Consumption suppresses the binding — a keyboard FRAME that took this key counts
            // exactly as a focused box does (decision 1319; `capture.0` above already covers the
            // box case, and this adds the frame one).
            if script.key_input(name) {
                capture.0 = true;
            }
        } else if let Some(chord) = chord {
            match chord {
                keymap::Chord::Edit(action) => {
                    script.editbox_action(action);
                }
                // The clipboard trio needs the OS pasteboard, so it resolves here against the held
                // [`HostClipboard`]: copy/cut pull the selection out of the box (RF-0082 §4
                // `0x77e1d0` — selection required; a password box yields its mask run, never the
                // real text), paste sanitizes+inserts.
                keymap::Chord::Copy => {
                    if let Some(text) = script.editbox_copy() {
                        clipboard.write(wl_display, &text);
                    }
                }
                keymap::Chord::Cut => {
                    if let Some(text) = script.editbox_cut() {
                        clipboard.write(wl_display, &text);
                    }
                }
                keymap::Chord::Paste => {
                    if let Some(text) = clipboard.read(wl_display) {
                        script.paste(&text);
                    }
                }
            }
        } else if !(sup || (ctrl && !alt)) {
            // Plain character input. Command-modified chars never insert (an unbound Cmd/Ctrl
            // chord like Cmd+L must not type "l") — but Ctrl+Alt passes: that's AltGr, the char
            // plane European layouts type real text with (and macOS Option+letter comes through
            // with only `alt`, composing its special characters).
            if let Some(text) = &ev.text {
                // Same suppression as the named keys, and this is the case that bites: the number
                // row IS a binding (the action buttons), so a digit typed into a keyboard frame —
                // the stack-split spinner — must not also fire action button 3 (decision 1319).
                if script.char_input(text) {
                    capture.0 = true;
                }
            }
        }
    }

    // The payload-held mirror, written LAST (after the mouse + keyboard feeds, so a same-frame
    // pickup or an ESC clear is already reflected) — the Send-side view the world-click
    // consumers read (decision 0571's no-payload-gated deselect).
    payload_held.0 = script.cursor_payload().is_some();

    for err in script.take_errors() {
        warn!("ui_script(input): {err}");
    }
}

// The two hardcoded key tables that used to live here — the ACTIONBUTTON number row and the
// bare-key window toggles — moved into the command registry (`crate::bindings::commands`,
// decision 0997): one chord→command table, rebindable, with 0585's modifier law enforced once
// in the dispatch instead of per branch here.
