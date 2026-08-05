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
    if let Some(cursor) = window.cursor_position().filter(|_| !ui_hidden) {
        // Window cursor is logical px, y-down from top-left; the UI is y-up 768-virtual units
        // (decisions 0582 + 0584's uiScale dial) — flip through the window height, then ÷s into
        // the VM's space (the inverse of the extract seam's ×s).
        let s = super::seam_scale(window.height(), ui_scale.0);
        let (x, y) = (cursor.x / s, (window.height() - cursor.y) / s);
        hover.0 = script.mouse_move(x, y);
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
    } else {
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
    // A *bare*-key binding is exactly that: bare. The reference names a binding
    // `[ALT-][CTRL-][SHIFT-]<key>` and matches it by string equality — its own FrameXML rebuilds the
    // name that way to recognise a press (`WorldMapFrame.xml:615-627`, the three prefixes prepended
    // around `arg1` and compared to `GetBindingKey("TOGGLEWORLDMAP")`), and `ALT-`/`CTRL-`/`SHIFT-`
    // are three separate strings in `WoW.exe`. So `CTRL-P` is a *different* entry from `P` and a
    // modified press never falls through to the unmodified binding. Ours must not either — otherwise
    // the dev-overlay chords would ALSO fire the letter's game action (`Ctrl`+`Shift`+key —
    // decisions 0585/0867/0870). `sup` is our one addition to the reference's three: Cmd is not a
    // binding modifier in any client's scheme, so a bare binding has no business firing under it
    // either. (A binding that *wants* a modifier declares it — see `vplates::toggle_vplates`, which
    // spends SHIFT itself for the V / SHIFT-V pair.)
    let modified = shift || ctrl || alt || sup;
    for ev in keyboard.read() {
        // ACTIONBUTTON1..12 (the number row 1..9,0,-,= — the bar's own HotKey labels): the ref's
        // two-edge binding (Bindings.xml:121-127 `runOnUp` → ActionButton.lua:15-45) — DOWN shows
        // the pushed visual, UP fires the action; the PUSHED/NORMAL state gate in the Lua makes a
        // held key's auto-repeat DOWNs no-ops. Gated on no EditBox holding the keyboard — else
        // "1" is a typed character — plus the bare-binding rule (`modified`), and consumed either
        // edge so a bar key never also lands as text.
        if let Some(i) = action_button_index(ev.key_code) {
            if !capture.0 && !modified {
                let edge = if ev.state == ButtonState::Pressed {
                    "Down"
                } else {
                    "Up"
                };
                if let Err(e) = script.run(&format!("BenillaActionButton{edge}({i})")) {
                    warn!("ui_script(actionbutton): {e}");
                }
                continue;
            }
        }
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
        if let Some(name) = named {
            let consumed = script.key_input(name);
            // ESCAPE with no EditBox focused runs the escape binding: the reference's own
            // TOGGLEGAMEMENU action (UiPanels.xml `ToggleGameMenu`) — one eater per press down the
            // close/cancel ladder, and the game menu when nothing is left to eat. No argument: the
            // `clicked` form is the micro button's plain toggle. A focused box consumes ESCAPE
            // first (`key_input` returns true), so this never fires while typing — the real
            // client's ESC precedence.
            if name == "ESCAPE" && !consumed {
                if let Err(e) = script.run("ToggleGameMenu()") {
                    warn!("ui_script(escape): {e}");
                }
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
        } else if let Some((lua, tag)) =
            bare_key_binding(ev.key_code).filter(|_| !capture.0 && !modified)
        {
            // A default bare-key binding ([`bare_key_binding`]) — the key-feed seam ESCAPE uses; the
            // action itself lives in Lua. Both gates are in the *branch condition*, not the body, so a
            // gated-out key still falls through to plain character input below ('b' typed into a chat
            // line is text, not the bag toggle).
            if let Err(e) = script.run(lua) {
                warn!("ui_script({tag}): {e}");
            }
        } else if !(sup || (ctrl && !alt)) {
            // Plain character input. Command-modified chars never insert (an unbound Cmd/Ctrl
            // chord like Cmd+L must not type "l") — but Ctrl+Alt passes: that's AltGr, the char
            // plane European layouts type real text with (and macOS Option+letter comes through
            // with only `alt`, composing its special characters).
            if let Some(text) = &ev.text {
                script.char_input(text);
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

/// The main-bar button (1-based) a key drives — the vanilla default ACTIONBUTTON1..12 row
/// (`1..9,0,-,=`), the exact labels the bar's HotKey corners render (`BENILLA_ACTIONBAR_HOTKEYS`).
fn action_button_index(key: KeyCode) -> Option<u8> {
    Some(match key {
        KeyCode::Digit1 => 1,
        KeyCode::Digit2 => 2,
        KeyCode::Digit3 => 3,
        KeyCode::Digit4 => 4,
        KeyCode::Digit5 => 5,
        KeyCode::Digit6 => 6,
        KeyCode::Digit7 => 7,
        KeyCode::Digit8 => 8,
        KeyCode::Digit9 => 9,
        KeyCode::Digit0 => 10,
        KeyCode::Minus => 11,
        KeyCode::Equal => 12,
        _ => return None,
    })
}

/// The default **bare-key** game bindings we've implemented: key → (the Lua the binding runs, its
/// `warn!` tag). One table so the two gates every one of them needs — no EditBox holding the keyboard,
/// no command modifier held (decision 0585) — are written once at the single call site instead of
/// re-derived per branch, which is how a seventh binding would quietly have shipped without them.
///
/// Vanilla names, for the ref cross-check:
/// - **B** OPENALLBAGS — open/close ALL bags at once, the real client's behavior, shared with the
///   backpack button (`BagFrame.xml` `BenillaBagToggle_OnClick` → `BenillaOpenAllBags`/`…Close…`).
/// - **L** TOGGLEQUESTLOG — ref `Bindings.xml:603-605` → `QuestLogFrame.lua:24-30`.
/// - **C** TOGGLECHARACTER0 — ref `CharacterFrame.lua:3-23`'s `ToggleCharacter`, tab-aware (hides the
///   window if the paper doll is already the visible tab, else shows/switches to it).
/// - **M** TOGGLEWORLDMAP — the fullscreen map (decision 0203 phase 2).
/// - **P** TOGGLESPELLBOOK — ref `Bindings.xml:582-583` (decision 0216 §8).
/// - **N** TOGGLETALENTS — ref `Bindings.xml`: `ToggleTalentFrame()` (decision 0304).
/// - **O** TOGGLEFRIENDSTAB — ref `Bindings.xml:618-620`: `ToggleFriendsFrame(1)`, the social
///   window on its Friends tab (decision 0668). The bare TOGGLESOCIAL binding is the same window
///   without a tab argument; 'O' is the tab-1 one in the client's default set.
fn bare_key_binding(key: KeyCode) -> Option<(&'static str, &'static str)> {
    Some(match key {
        KeyCode::KeyB => ("BenillaBagToggle_OnClick()", "togglebackpack"),
        KeyCode::KeyL => ("ToggleQuestLog()", "togglequestlog"),
        KeyCode::KeyC => (
            "ToggleCharacter(\"BenillaPaperDollFrame\")",
            "togglecharacter",
        ),
        KeyCode::KeyM => ("ToggleWorldMap()", "toggleworldmap"),
        KeyCode::KeyP => ("ToggleSpellBook(BOOKTYPE_SPELL)", "togglespellbook"),
        KeyCode::KeyN => ("ToggleTalentFrame()", "toggletalents"),
        KeyCode::KeyO => ("ToggleFriendsFrame(1)", "togglesocial"),
        _ => return None,
    })
}
