//! Stuck-modifier reconciliation against the OS's live flag state (decision 0606).
//!
//! macOS system shortcuts that grab the keyboard *without* de-focusing the window — the ⇧⌘5
//! screenshot overlay is the canonical one — swallow the modifiers' release events: the overlay is
//! a non-activating panel, so the app never receives `Focused(false)` (Bevy's stuck-key reset,
//! `KeyboardFocusLost` → `release_all`, never fires), and winit only synthesizes modifier key
//! events from `flagsChanged`, which the grabbed keyboard never delivers (verified in
//! winit 0.30.13 `platform_impl/macos/view.rs::update_modifiers`). `ButtonInput<KeyCode>` then
//! reports Shift/Cmd held indefinitely, and every bare-key binding — the action bar's number row
//! behind `ui_script::input`'s bare-binding gate — goes dead until the user happens to tap the
//! stuck modifier again.
//!
//! The fix: poll the OS's live modifier state (`+[NSEvent modifierFlags]` — hardware-derived and
//! documented as independent of the event stream) once per frame, right after Bevy's own input
//! collection, and **release** any modifier Bevy believes is down but the OS says is up.
//! Release-only by design: the hardware state leads the event stream, so a legitimately
//! just-pressed modifier is never wrongly released — whereas synthesizing *presses* from the poll
//! could race in-flight release events and manufacture the same bug in reverse. The poll can't
//! tell left from right within a family, so an up family releases both variants (only ever a
//! correction — a genuinely held key keeps its family bit set and is never touched).

use bevy::prelude::*;

pub struct ModKeysPlugin;

impl Plugin for ModKeysPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(target_os = "macos")]
        app.add_systems(
            PreUpdate,
            mac::reconcile_modifiers.after(bevy::input::InputSystems),
        );
        #[cfg(not(target_os = "macos"))]
        let _ = app;
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use bevy::ecs::system::NonSendMarker;
    use bevy::input::keyboard::Key;
    use bevy::prelude::*;
    use objc2_app_kit::{NSEvent, NSEventModifierFlags};

    /// Release any modifier key Bevy holds pressed that the OS's live flag state says is up.
    /// `NonSendMarker` pins the system to the main thread, which AppKit requires.
    pub(super) fn reconcile_modifiers(
        _main_thread: NonSendMarker,
        mut codes: ResMut<ButtonInput<KeyCode>>,
        mut logical: ResMut<ButtonInput<Key>>,
    ) {
        let flags = unsafe { NSEvent::modifierFlags_class() };
        let families = [
            (
                NSEventModifierFlags::NSEventModifierFlagShift,
                [KeyCode::ShiftLeft, KeyCode::ShiftRight],
                Key::Shift,
            ),
            (
                NSEventModifierFlags::NSEventModifierFlagControl,
                [KeyCode::ControlLeft, KeyCode::ControlRight],
                Key::Control,
            ),
            (
                NSEventModifierFlags::NSEventModifierFlagOption,
                [KeyCode::AltLeft, KeyCode::AltRight],
                Key::Alt,
            ),
            (
                NSEventModifierFlags::NSEventModifierFlagCommand,
                [KeyCode::SuperLeft, KeyCode::SuperRight],
                Key::Super,
            ),
        ];
        for (flag, keys, key) in families {
            if flags.contains(flag) {
                continue;
            }
            // Guarded so an in-sync frame (the overwhelmingly common case) never marks the
            // resources changed.
            for code in keys {
                if codes.pressed(code) {
                    warn!("modkeys: releasing stuck {code:?} (OS reports it up)");
                    codes.release(code);
                }
            }
            if logical.pressed(key.clone()) {
                logical.release(key);
            }
        }
    }
}
