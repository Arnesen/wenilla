//! **TOGGLEUI** — the hide-the-interface binding (`ALT-Z`).
//!
//! The state ([`UiHidden`]) and the binding that flips it; the two things that *obey* it are the
//! quad pass's mesh rebuild (`ui_pass::rebuild_ui_mesh` draws nothing) and the UI's pointer feed
//! (`ui_script::input::feed_ui_input` stops hit-testing). Kept out of `ui_pass` deliberately: this
//! is a *game binding*, not part of the render substrate.

use bevy::prelude::*;

use crate::char_select::ClientState;
use crate::ui_script::{UiInput, UiKeyboardCapture};

/// Is the player UI hidden right now? (`ALT-Z` — [`toggle_ui_hidden`].)
///
/// Hidden means *"draw nothing"*, not *"stop producing"*: the widget arena keeps ticking and both
/// quad lanes keep filling, so open panels, tooltip state, cooldown sweeps and chat scrollback are
/// exactly where they were the moment the UI comes back — only the pass's mesh batches go away.
/// **Everything that lands in `ui_pass::UiQuads` goes dark together**: the FrameXML layer, the
/// minimap, the V-plates, chat bubbles and floating combat text — the world and nothing else,
/// which is the point of the binding. What stays up: the dev overlays (their own camera and their
/// own dev chords — an instrument, not the player's UI), the glue/loading screens (Bevy UI
/// nodes, not quads), and the cursor.
///
/// The UI also stops taking the **mouse** while hidden: an invisible action bar must not eat a
/// click or arm a tooltip. The keyboard feed stays live — a hidden UI is still the client you're
/// playing, and `ENTER`/`ESCAPE`/the bar keys keep working exactly as the reference's do.
#[derive(Resource, Default)]
pub(crate) struct UiHidden(pub bool);

/// The TOGGLEUI binding + its state.
pub(crate) struct UiHidePlugin;

impl Plugin for UiHidePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiHidden>()
            .add_systems(
                Update,
                // After `UiInput`, like every other gameplay key reader: an EditBox that consumed
                // this frame's keys has already published its capture flag by then.
                toggle_ui_hidden
                    .after(UiInput)
                    .run_if(in_state(ClientState::InWorld)),
            )
            .add_systems(OnExit(ClientState::InWorld), show_ui);
    }
}

/// `ALT-Z` — the reference's own TOGGLEUI default (`BINDING_NAME_TOGGLEUI` in `GlobalStrings.lua`),
/// on every platform.
///
/// **This binding was `CTRL-Z` here until 0870, and that was wrong** — a fidelity error worth naming
/// because of *how* it got in. The source was this install's `bindings-cache.wtf`, which reads
/// `bind CTRL-Z TOGGLEUI` on all three of its accounts; that file is what the client last *wrote*,
/// not what it ships with, and all three accounts descend from one profile whose TOGGLEUI had been
/// rebound. `ALT-Z` is the shipped default (director's correction; vanilla binding lists agree, and
/// `ALT-Z` is unbound in that same cache — exactly the hole a rebind leaves). A saved-state file is
/// evidence about a *player*, never about the client.
///
/// `ALT` and nothing else: the reference names a binding `[ALT-][CTRL-][SHIFT-]<key>` and matches it
/// by string equality, so `CTRL-ALT-Z` is a *different* entry and a modified press must never fall
/// through to another binding (decision 0585's bare-binding rule — the discipline
/// `vplates::toggle_vplates` spends `SHIFT` under). That block also keeps this clear of the
/// dev-overlay plane (`Ctrl`+`Shift` — [`crate::debug_panel::dev_chord`]). Inert while an EditBox
/// holds the keyboard: an `Alt-Z` in a chat line is the box's business.
///
/// Read off `ButtonInput`'s edge. It used to be driven off the raw key message stream because AppKit
/// swallows `keyUp` for a key released while **Cmd** is held, which would have latched the old
/// `Cmd-Z` twin after one use; with no Cmd anywhere in the binding that hazard doesn't exist, and
/// `just_pressed` doesn't repeat, so the repeat filter went with it (0870).
fn toggle_ui_hidden(
    keys: Res<ButtonInput<KeyCode>>,
    ui_capture: Res<UiKeyboardCapture>,
    mut hidden: ResMut<UiHidden>,
) {
    if !keys.just_pressed(KeyCode::KeyZ) || ui_capture.0 || !toggle_chord(&keys) {
        return;
    }
    hidden.0 = !hidden.0;
    info!("ui: {} (Alt-Z)", if hidden.0 { "HIDDEN" } else { "shown" });
}

/// The modifier half of the binding: `ALT` held, and nothing else. See [`toggle_ui_hidden`].
fn toggle_chord(keys: &ButtonInput<KeyCode>) -> bool {
    let alt = keys.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]);
    let blocked = keys.any_pressed([
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::ShiftLeft,
        KeyCode::ShiftRight,
        KeyCode::SuperLeft,
        KeyCode::SuperRight,
    ]);
    alt && !blocked
}

/// Leaving the world clears the hide — safety, not fidelity: the binding is `InWorld`-only, so a UI
/// left hidden at logout would come back invisible with no on-screen affordance to explain it.
fn show_ui(mut hidden: ResMut<UiHidden>) {
    hidden.0 = false;
}

#[cfg(test)]
mod tests {
    use bevy::input::keyboard::KeyboardInput;
    use bevy::input::ButtonState;

    use super::*;

    /// The plugin's own wiring, driven the way winit drives it: the modifier mirror plus a real
    /// `Z` edge. Covers what the pure-chord tests can't — the typing gate and the toggle itself.
    fn app(held: &[KeyCode]) -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            bevy::input::InputPlugin,
            bevy::state::app::StatesPlugin,
        ))
        .insert_state(ClientState::InWorld)
        .init_resource::<UiKeyboardCapture>()
        .add_plugins(UiHidePlugin);
        for &k in held {
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(k);
        }
        app
    }

    /// One `Z` keystroke, down then up, fed the way winit feeds it — as [`KeyboardInput`] messages
    /// that Bevy's own input system turns into the `just_pressed` edge this binding reads. The
    /// release matters: `ButtonInput::press` only raises the edge for a key that wasn't already
    /// down, so a second tap without one is not a second press (which is also why a held key's
    /// auto-repeat can't strobe the toggle).
    fn tap_z(app: &mut App) {
        for state in [ButtonState::Pressed, ButtonState::Released] {
            app.world_mut().write_message(KeyboardInput {
                key_code: KeyCode::KeyZ,
                logical_key: bevy::input::keyboard::Key::Character("z".into()),
                state,
                text: Some("z".into()),
                repeat: false,
                window: Entity::PLACEHOLDER,
            });
            app.update();
        }
    }

    fn hidden(app: &App) -> bool {
        app.world().resource::<UiHidden>().0
    }

    /// `ALT-Z` toggles both ways — the reference's own TOGGLEUI default (0870).
    #[test]
    fn alt_z_hides_and_shows_again() {
        let mut app = app(&[KeyCode::AltLeft]);
        tap_z(&mut app);
        assert!(hidden(&app), "Alt-Z hides");
        tap_z(&mut app);
        assert!(!hidden(&app), "Alt-Z again shows");
    }

    /// While an EditBox owns the keyboard the binding is inert — an `Alt-Z` in a chat line is the
    /// box's business, not a binding.
    #[test]
    fn inert_while_typing() {
        let mut app = app(&[KeyCode::AltLeft]);
        app.world_mut().resource_mut::<UiKeyboardCapture>().0 = true;
        tap_z(&mut app);
        assert!(!hidden(&app), "a captured keyboard eats the binding");
    }

    /// The bare-binding rule at the wiring level: an unmodified `Z` is the sheath toggle
    /// (`player::control`, and `Z TOGGLESHEATH` in the reference), and it must not also hide the UI.
    #[test]
    fn bare_z_is_not_the_binding() {
        let mut app = app(&[]);
        tap_z(&mut app);
        assert!(!hidden(&app), "bare Z belongs to the sheath toggle");
    }

    fn keys(held: &[KeyCode]) -> ButtonInput<KeyCode> {
        let mut k = ButtonInput::default();
        for &c in held {
            k.press(c);
        }
        k
    }

    /// `ALT` alone is the binding, either side of the family.
    #[test]
    fn either_alt_alone_fires() {
        for held in [KeyCode::AltLeft, KeyCode::AltRight] {
            assert!(toggle_chord(&keys(&[held])), "{held:?} alone fires");
        }
    }

    /// Decision 0585's bare-binding rule, both directions: an unmodified `Z` is the *sheath*
    /// binding, and a press carrying an extra modifier names a different entry (`CTRL-ALT-Z`,
    /// `ALT-SHIFT-Z`) that this one must not answer for. The old `CTRL-Z`/`Cmd-Z` arms are gone
    /// (0870) — neither is the reference's default, and `Win+Z` is Snap Layouts.
    #[test]
    fn bare_and_over_modified_presses_are_not_the_binding() {
        assert!(!toggle_chord(&keys(&[])), "bare Z is the sheath binding");
        for held in [
            vec![KeyCode::ControlLeft],
            vec![KeyCode::SuperLeft],
            vec![KeyCode::AltLeft, KeyCode::ControlRight],
            vec![KeyCode::AltLeft, KeyCode::ShiftRight],
            vec![KeyCode::AltLeft, KeyCode::SuperLeft],
        ] {
            assert!(!toggle_chord(&keys(&held)), "{held:?} is another binding");
        }
    }
}
