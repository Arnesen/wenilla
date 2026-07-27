//! **TOGGLEUI** — the hide-the-interface binding (`CTRL-Z`, and `Cmd-Z` as its Mac twin).
//!
//! The state ([`UiHidden`]) and the binding that flips it; the two things that *obey* it are the
//! quad pass's mesh rebuild (`ui_pass::rebuild_ui_mesh` draws nothing) and the UI's pointer feed
//! (`ui_script::input::feed_ui_input` stops hit-testing). Kept out of `ui_pass` deliberately: this
//! is a *game binding*, not part of the render substrate.

use bevy::input::keyboard::KeyboardInput;
use bevy::input::ButtonState;
use bevy::prelude::*;

use crate::char_select::ClientState;
use crate::ui_script::{UiInput, UiKeyboardCapture};

/// Is the player UI hidden right now? (`CTRL-Z`/`Cmd-Z` — [`toggle_ui_hidden`].)
///
/// Hidden means *"draw nothing"*, not *"stop producing"*: the widget arena keeps ticking and both
/// quad lanes keep filling, so open panels, tooltip state, cooldown sweeps and chat scrollback are
/// exactly where they were the moment the UI comes back — only the pass's mesh batches go away.
/// **Everything that lands in `ui_pass::UiQuads` goes dark together**: the FrameXML layer, the
/// minimap, the V-plates, chat bubbles and floating combat text — the world and nothing else,
/// which is the point of the binding. What stays up: the dev overlays (their own camera and their
/// own `Ctrl`+`Cmd` chords — an instrument, not the player's UI), the glue/loading screens (Bevy UI
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

/// `CTRL-Z` / `Cmd-Z` — the reference's TOGGLEUI binding (`BINDING_NAME_TOGGLEUI` in
/// `GlobalStrings.lua`; the 1.12.1 install's own `bindings-cache.wtf` reads `bind CTRL-Z TOGGLEUI`
/// on all three accounts in it), plus the `Cmd` twin — the Mac keyboard the director plays the
/// reference on presses `Cmd` for that binding, and nothing in game binds the Cmd plane.
///
/// Exactly one of the two, and no `Alt`/`Shift`: the reference names a binding
/// `[ALT-][CTRL-][SHIFT-]<key>` and matches it by string equality, so `CTRL-ALT-Z` is a *different*
/// entry and a modified press must never fall through to another binding (decision 0585's
/// bare-binding rule — the discipline `vplates::toggle_vplates` spends `SHIFT` under), while
/// `Ctrl`+`Cmd` together is the dev-overlay plane. Inert while an EditBox holds the keyboard: a
/// `Z` in a chat line is text.
///
/// Driven off the key **message stream** rather than `ButtonInput`'s edges, because AppKit swallows
/// `keyUp` for a key released while `Cmd` is held: the release never reaches winit, `ButtonInput`
/// goes on believing `Z` is down, and the *second* `Cmd-Z` would never produce a `just_pressed`
/// edge — the UI would hide and refuse to come back. `repeat` presses are dropped so a held chord
/// doesn't strobe.
fn toggle_ui_hidden(
    mut keyboard: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    ui_capture: Res<UiKeyboardCapture>,
    mut hidden: ResMut<UiHidden>,
) {
    let pressed = keyboard
        .read()
        .any(|ev| ev.key_code == KeyCode::KeyZ && ev.state == ButtonState::Pressed && !ev.repeat);
    if !pressed || ui_capture.0 || !toggle_chord(&keys) {
        return;
    }
    hidden.0 = !hidden.0;
    info!(
        "ui: {} (Ctrl/Cmd-Z)",
        if hidden.0 { "HIDDEN" } else { "shown" }
    );
}

/// The modifier half of the chord: exactly one of `Ctrl`/`Cmd` held, and neither `Alt` nor `Shift`.
/// See [`toggle_ui_hidden`] for why each arm is what it is.
fn toggle_chord(keys: &ButtonInput<KeyCode>) -> bool {
    let ctrl = keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]);
    let cmd = keys.any_pressed([KeyCode::SuperLeft, KeyCode::SuperRight]);
    let blocked = keys.any_pressed([
        KeyCode::AltLeft,
        KeyCode::AltRight,
        KeyCode::ShiftLeft,
        KeyCode::ShiftRight,
    ]);
    (ctrl ^ cmd) && !blocked
}

/// Leaving the world clears the hide — safety, not fidelity: the binding is `InWorld`-only, so a UI
/// left hidden at logout would come back invisible with no on-screen affordance to explain it.
fn show_ui(mut hidden: ResMut<UiHidden>) {
    hidden.0 = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plugin's own wiring, driven the way winit drives it: a real [`KeyboardInput`] message
    /// plus the modifier mirror. Covers what the pure-chord tests can't — the message path (the
    /// `Cmd`-swallowed-`keyUp` insurance), the repeat filter, and the typing gate.
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

    fn press_z(app: &mut App, repeat: bool) {
        let window = Entity::PLACEHOLDER;
        app.world_mut().write_message(KeyboardInput {
            key_code: KeyCode::KeyZ,
            logical_key: bevy::input::keyboard::Key::Character("z".into()),
            state: ButtonState::Pressed,
            text: Some("z".into()),
            repeat,
            window,
        });
        app.update();
    }

    fn hidden(app: &App) -> bool {
        app.world().resource::<UiHidden>().0
    }

    /// The binding toggles both ways off the message stream — the second press is the one an
    /// edge-driven read would lose on macOS (AppKit eats the `keyUp` under `Cmd`).
    #[test]
    fn cmd_z_hides_and_shows_again() {
        let mut app = app(&[KeyCode::SuperLeft]);
        press_z(&mut app, false);
        assert!(hidden(&app), "Cmd-Z hides");
        press_z(&mut app, false);
        assert!(!hidden(&app), "Cmd-Z again shows");
    }

    /// A held chord auto-repeats; only the real press counts, or the UI would strobe.
    #[test]
    fn repeats_do_not_strobe() {
        let mut app = app(&[KeyCode::ControlLeft]);
        press_z(&mut app, false);
        press_z(&mut app, true);
        press_z(&mut app, true);
        assert!(hidden(&app), "the repeats after the press changed nothing");
    }

    /// While an EditBox owns the keyboard the chord is inert — `Ctrl-Z` in a chat line is the
    /// box's business, not a binding.
    #[test]
    fn inert_while_typing() {
        let mut app = app(&[KeyCode::ControlLeft]);
        app.world_mut().resource_mut::<UiKeyboardCapture>().0 = true;
        press_z(&mut app, false);
        assert!(!hidden(&app), "a captured keyboard eats the chord");
    }

    /// The bare-binding rule at the wiring level: an unmodified `Z` is the sheath toggle
    /// (`player::control`), and it must not also hide the UI.
    #[test]
    fn bare_z_is_not_the_binding() {
        let mut app = app(&[]);
        press_z(&mut app, false);
        assert!(!hidden(&app), "bare Z belongs to the sheath toggle");
    }

    fn keys(held: &[KeyCode]) -> ButtonInput<KeyCode> {
        let mut k = ButtonInput::default();
        for &c in held {
            k.press(c);
        }
        k
    }

    /// The chord accepts either command modifier alone — `CTRL-Z` is the reference's own entry,
    /// `Cmd-Z` the Mac twin — and both sides of each family.
    #[test]
    fn either_command_modifier_alone_fires() {
        for held in [
            KeyCode::ControlLeft,
            KeyCode::ControlRight,
            KeyCode::SuperLeft,
            KeyCode::SuperRight,
        ] {
            assert!(toggle_chord(&keys(&[held])), "{held:?} alone fires");
        }
    }

    /// Decision 0585's bare-binding rule, both directions: an unmodified `Z` is the *sheath*
    /// binding and must not hide the UI, and a press carrying an extra modifier names a different
    /// binding entry (`CTRL-ALT-Z`, `CTRL-SHIFT-Z`) that this one must not answer for. `Ctrl`+`Cmd`
    /// is the dev-overlay plane and is likewise not ours.
    #[test]
    fn bare_and_over_modified_presses_are_not_the_binding() {
        assert!(!toggle_chord(&keys(&[])), "bare Z is the sheath binding");
        for held in [
            vec![KeyCode::ControlLeft, KeyCode::AltLeft],
            vec![KeyCode::ControlLeft, KeyCode::ShiftLeft],
            vec![KeyCode::SuperLeft, KeyCode::AltRight],
            vec![KeyCode::SuperLeft, KeyCode::ShiftRight],
            vec![KeyCode::ControlLeft, KeyCode::SuperLeft],
        ] {
            assert!(!toggle_chord(&keys(&held)), "{held:?} is another binding");
        }
    }
}
