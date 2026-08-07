//! **Key bindings** (decision 0997) — the one chord→command engine every rebindable input runs
//! through, replacing the per-site hardcoded key reads (and the four independent copies of
//! 0585's bare-binding modifier rule they carried).
//!
//! The split mirrors the CVar table (0954): the **string-domain truth** lives engine-side
//! ([`benilla_ui::script::keybind`] — the table the Key Bindings window's Lua edits
//! synchronously), and this module derives the app's **dispatch view** from it whenever its
//! generation moves: canonical chord strings parsed ([`chord`]) into an exact-match map.
//!
//! Dispatch (the [`latch_and_dispatch`] system, ordered inside [`crate::ui_script::UiInput`]
//! right after the UI key feed):
//! - a press matches a chord only on the **exact** modifier set (0585, engine-wide now; Super
//!   held matches nothing);
//! - [`Kind::Held`] commands **latch** on the matching press and unlatch on the *base key's*
//!   release — the reference's `runOnUp` movement law, which is why tapping Shift mid-run does
//!   not stop you, and why a chat box taking focus stops movement (latches clear on the capture
//!   edge — the reference's own focus handler, `0x514490`) without eating the release;
//! - [`Kind::Edge`]/[`Kind::EdgeUpDown`] run their 1.12 Lua bodies in the VM;
//! - [`Kind::Host`] lands in [`BindingsState::fired`] for engine consumers (chat open, TAB
//!   targeting, nameplates, autorun, camera zoom, …).
//!
//! While the Keybindings page (the Options window's category since 1008) has a capsule
//! selected it arms the **capture seam** (`BenillaBindCapture`): raw input is swallowed here,
//! canonicalized (`ALT-CTRL-SHIFT-<TOKEN>`), and handed back to the page's own capture handler
//! — the 1.12 law (lone modifiers ignored, left/right clicks stay UI clicks, ESC binds like
//! any key).
//!
//! Persistence: `benilla/bindings/account.txt` + `<Realm>-<Char>.txt` ([`store`], through
//! [`crate::local_state`]); the character file's existence is the character-set state, deleted
//! on the confirmed switch back to general — the reference's own semantics.

use bevy::input::keyboard::KeyboardInput;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::input::ButtonState;
use bevy::prelude::*;

use benilla_ui::script::keybind::{KeybindCommand, KeybindRequest};
use benilla_ui::script::UiScript;

use crate::char_select::ClientState;
use crate::ui_script::{PlayerUiHover, PointerOverUi, UiKeyboardCapture};

pub(crate) mod chord;
pub(crate) mod commands;
mod store;

use chord::{BindKey, Chord};
pub(crate) use commands::cmd;
use commands::{Cmd, Kind, SPECS};

/// The derived chord→command map (exact-modifier law) — rebuilt whenever the engine table's
/// generation moves.
#[derive(Resource, Default)]
struct BindingDispatch {
    map: std::collections::HashMap<Chord, Cmd>,
    seen_generation: Option<u64>,
}

/// This frame's binding activity — what the engine-side consumers read instead of raw keys.
#[derive(Resource, Default)]
pub(crate) struct BindingsState {
    /// Live latches: (base key, command) — a [`Kind::Held`] or [`Kind::EdgeUpDown`] press that
    /// has not released yet.
    latched: Vec<(BindKey, Cmd)>,
    /// Commands whose first latch began this frame (the press edge).
    just: Vec<Cmd>,
    /// Host-edge commands fired this frame.
    fired: Vec<Cmd>,
    /// Accumulated analog amount per host command this frame (wheel notches; a key press adds
    /// the reference's own 1.0 step) — the camera zoom's input.
    amounts: Vec<(Cmd, f32)>,
    /// Rising edge of the keyboard-capture gate last frame (internal: clears Held latches once).
    was_typing: bool,
}

impl BindingsState {
    /// Is a held command latched right now? (The reference's held movement bit.)
    pub(crate) fn pressed(&self, c: Cmd) -> bool {
        self.latched.iter().any(|&(_, l)| l == c)
    }
    /// Did this command's latch begin this frame? (The key-DOWN edge — the autorun cancel set.)
    pub(crate) fn just_pressed(&self, c: Cmd) -> bool {
        self.just.contains(&c)
    }
    /// Did this host-edge command fire this frame?
    pub(crate) fn fired(&self, c: Cmd) -> bool {
        self.fired.contains(&c)
    }
    /// Total analog amount for a host command this frame (0.0 when idle).
    pub(crate) fn amount(&self, c: Cmd) -> f32 {
        self.amounts
            .iter()
            .filter(|&&(a, _)| a == c)
            .map(|&(_, v)| v)
            .sum()
    }
    /// Test seam for consumer systems: a state in which these host commands fired this frame.
    #[cfg(test)]
    pub(crate) fn test_fired(cmds: &[Cmd]) -> Self {
        Self {
            fired: cmds.to_vec(),
            ..Default::default()
        }
    }
}

/// Which files this session's bindings live in — the macros-files pattern
/// ([`crate::ui_macro::MacroFiles`]), resolved once the character is known.
#[derive(Resource, Default)]
struct BindingFiles {
    account: Option<std::path::PathBuf>,
    character: Option<std::path::PathBuf>,
    identity: Option<(String, String)>,
}

/// Label for this module's systems inside [`crate::ui_script::UiInput`] — the UI key feed is
/// ordered `.before()` it (a key a focused box consumes must already be reflected in the
/// capture gate when dispatch runs).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BindingSet;

pub(crate) struct BindingsPlugin;

impl Plugin for BindingsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BindingDispatch>()
            .init_resource::<BindingsState>()
            .init_resource::<BindingFiles>()
            // PostStartup like the macro catalog: needs the VM, which lands at the Startup
            // schedule boundary.
            .add_systems(PostStartup, seed_bindings)
            .add_systems(
                Update,
                (
                    (sync_dispatch, latch_and_dispatch)
                        .chain()
                        .in_set(crate::ui_script::UiInput)
                        .in_set(BindingSet)
                        .before(crate::schedule::WorldStage::Input)
                        .run_if(in_state(ClientState::InWorld)),
                    load_character_bindings.run_if(in_state(ClientState::InWorld)),
                    save_bindings.after(load_character_bindings),
                ),
            );
    }
}

/// The registry as the engine table's registration payload — shared by the boot seed and the
/// hermetic capture fixtures (which have no plugin seed to race, the `register_cvars` posture).
pub(crate) fn registry_commands() -> Vec<KeybindCommand> {
    SPECS
        .iter()
        .map(|s| KeybindCommand {
            name: s.name,
            category: s.category,
            run_on_up: s.run_on_up(),
            default1: s.d1,
            default2: s.d2,
        })
        .collect()
}

/// Register the command registry with the engine table and seed the account set from disk —
/// boot-time, before any window opens.
fn seed_bindings(script: Option<NonSendMut<UiScript>>, mut files: ResMut<BindingFiles>) {
    let Some(mut script) = script else { return };
    script.register_bindings(&registry_commands());
    files.account = crate::local_state::bindings_account_path();
    if let Some(overrides) = read_diff(&files.account) {
        script.seed_binding_set(1, Some(store::resolve(&overrides)));
    } else {
        script.seed_binding_set(1, Some(store::resolve(&[])));
    }
    script.load_binding_set(1);
    info!("bindings: {} commands registered", SPECS.len());
}

/// Read + parse one diff file; `None` when absent/unreadable (defaults).
fn read_diff(path: &Option<std::path::PathBuf>) -> Option<Vec<(String, Vec<String>)>> {
    let path = path.as_ref()?;
    match std::fs::read_to_string(path) {
        Ok(text) => Some(store::from_diff(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warn!("bindings: reading {}: {e}", path.display());
            None
        }
    }
}

/// Load the character-specific set once the roster names the character (the macros-load
/// pattern); its file existing makes it the active set, the reference's own rule.
fn load_character_bindings(
    script: Option<NonSendMut<UiScript>>,
    roster: Res<crate::char_select::Roster>,
    mut files: ResMut<BindingFiles>,
) {
    let Some(mut script) = script else { return };
    let Some(id) = crate::ui_macro::identity(&roster) else {
        return;
    };
    if files.identity.as_ref() == Some(&id) {
        return;
    }
    files.character = crate::local_state::bindings_character_path(&id.0, &id.1);
    files.identity = Some(id);
    match read_diff(&files.character) {
        Some(overrides) => {
            script.seed_binding_set(2, Some(store::resolve(&overrides)));
            script.load_binding_set(2);
            info!("bindings: character-specific set loaded");
        }
        None => {
            script.seed_binding_set(2, None);
            script.load_binding_set(1);
        }
    }
}

/// Persist on the window's SaveBindings (Okay): write the set's diff; saving account while a
/// character file exists deletes it — the confirmed permanent delete.
fn save_bindings(script: Option<NonSendMut<UiScript>>, files: Res<BindingFiles>) {
    let Some(mut script) = script else { return };
    for req in script.take_keybind_requests() {
        let KeybindRequest::Save(which) = req;
        let snapshot = script.keybind_snapshot();
        let text = store::to_diff(&snapshot);
        let path = match which {
            1 => &files.account,
            2 => &files.character,
            _ => continue,
        };
        if let Some(path) = path {
            if let Err(e) = crate::local_state::write_atomic(path, &text) {
                warn!("bindings: saving {}: {e}", path.display());
            }
        }
        if which == 1 {
            if let Some(chr) = &files.character {
                match std::fs::remove_file(chr) {
                    Ok(()) => info!("bindings: character-specific set deleted"),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => warn!("bindings: deleting {}: {e}", chr.display()),
                }
            }
        }
    }
}

/// Rebuild the dispatch map when the engine table moved (a rebind, a set switch, the seed),
/// and fire the reference's own `UPDATE_BINDINGS` so the Lua consumers repaint (the action
/// bar's HotKey corners; the ref registers the same event for the same job).
fn sync_dispatch(script: Option<NonSendMut<UiScript>>, mut dispatch: ResMut<BindingDispatch>) {
    let Some(mut script) = script else { return };
    let generation = script.keybinds_generation();
    if dispatch.seen_generation == Some(generation) {
        return;
    }
    dispatch.seen_generation = Some(generation);
    dispatch.map.clear();
    let by_name: std::collections::HashMap<&str, Cmd> = SPECS
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name, Cmd(i as u16)))
        .collect();
    for (name, keys) in script.keybind_snapshot() {
        let Some(&cmd) = by_name.get(name.as_str()) else {
            continue;
        };
        for key in keys {
            match Chord::parse(&key) {
                Some(ch) => {
                    dispatch.map.insert(ch, cmd);
                }
                None => warn!("bindings: {name}: unpressable chord '{key}' (unknown token)"),
            }
        }
    }
    script.fire_event("UPDATE_BINDINGS", vec![]);
}

/// The dispatch pass — see the module doc. Runs right after the UI key feed (same frame's
/// capture gate), before `WorldStage::Input` (a bound key must act this frame, once).
#[allow(clippy::too_many_arguments)]
fn latch_and_dispatch(
    script: Option<NonSendMut<UiScript>>,
    mut keyboard: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    scroll: Res<AccumulatedMouseScroll>,
    capture: Res<UiKeyboardCapture>,
    hover: Res<PlayerUiHover>,
    over_ui: Res<PointerOverUi>,
    dispatch: Res<BindingDispatch>,
    mut state: ResMut<BindingsState>,
) {
    state.just.clear();
    state.fired.clear();
    state.amounts.clear();

    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    let sup = keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight);

    let mut script = script;
    let armed = script.as_ref().is_some_and(|s| s.bind_capture_armed());
    let run_lua = |script: &mut Option<NonSendMut<UiScript>>, lua: &str, tag: &str| {
        if let Some(s) = script.as_mut() {
            if let Err(e) = s.run(lua) {
                warn!("bindings({tag}): {e}");
            }
        }
    };

    // ── The capture seam ── the Keybindings page has a capsule selected: swallow raw input,
    // canonicalize, hand the chord string to the page's Lua (1.12's OnKeyDown law: lone
    // modifiers and unknown keys ignored; Super is not a 1.12 modifier — a Super press is
    // ignored outright; left/right stay UI clicks; the wheel is a chord like any other).
    if armed {
        let mut captured: Option<String> = None;
        for ev in keyboard.read() {
            if ev.state != ButtonState::Pressed || ev.repeat || sup {
                continue;
            }
            if let Some(token) = chord::key_token(ev.key_code) {
                captured = Some(chord::chord_string(alt, ctrl, shift, token));
            }
        }
        for b in [MouseButton::Middle, MouseButton::Forward, MouseButton::Back] {
            if buttons.just_pressed(b) && !sup {
                if let Some(token) = chord::mouse_token(b) {
                    captured = Some(chord::chord_string(alt, ctrl, shift, token));
                }
            }
        }
        if scroll.delta.y != 0.0 && !sup {
            let token = if scroll.delta.y > 0.0 {
                "MOUSEWHEELUP"
            } else {
                "MOUSEWHEELDOWN"
            };
            captured = Some(chord::chord_string(alt, ctrl, shift, token));
        }
        if let Some(chord_str) = captured {
            run_lua(
                &mut script,
                &format!("KeyBindings_OnHostKey(\"{chord_str}\")"),
                "capture",
            );
        }
        // Releases still unlatch below (a key held across the arm must not stick), but no new
        // latches or fires happen while armed.
    }

    // ── The typing edge ── a box taking focus stops movement (the reference's focus handler
    // releases every direction bit, `0x514490`): Held latches clear once on the rising edge;
    // EdgeUpDown latches stay armed — their release half still fires (the reference delivers
    // the up of a pressed binding regardless).
    let typing = capture.0;
    if typing && !state.was_typing {
        state
            .latched
            .retain(|&(_, c)| !matches!(SPECS[c.0 as usize].kind, Kind::Held));
    }
    state.was_typing = typing;

    // ── Keyboard ── press edges latch/fire (exact-modifier chord match, no repeats, gated on
    // typing and the capture arm); release edges unlatch and fire the runOnUp up-half.
    for ev in keyboard.read() {
        let key = chord::normalize_key(ev.key_code);
        match ev.state {
            ButtonState::Pressed => {
                if armed || typing || sup || ev.repeat {
                    continue;
                }
                if state.latched.iter().any(|&(k, _)| k == BindKey::Key(key)) {
                    continue; // already latched (missed release would double-latch)
                }
                let chord = Chord {
                    alt,
                    ctrl,
                    shift,
                    key: BindKey::Key(key),
                };
                if let Some(&cmd) = dispatch.map.get(&chord) {
                    press(&mut state, &mut script, run_lua, cmd, BindKey::Key(key));
                }
            }
            ButtonState::Released => {
                release(&mut state, &mut script, run_lua, BindKey::Key(key));
            }
        }
    }

    // ── Mouse buttons ── same law; presses only while the cursor is over the world (a frame
    // under the cursor owns its clicks), releases always.
    for b in [
        MouseButton::Left,
        MouseButton::Right,
        MouseButton::Middle,
        MouseButton::Forward,
        MouseButton::Back,
    ] {
        if buttons.just_pressed(b)
            && !armed
            && !sup
            && hover.0.is_none()
            && !state.latched.iter().any(|&(k, _)| k == BindKey::Mouse(b))
        {
            let chord = Chord {
                alt,
                ctrl,
                shift,
                key: BindKey::Mouse(b),
            };
            if let Some(&cmd) = dispatch.map.get(&chord) {
                press(&mut state, &mut script, run_lua, cmd, BindKey::Mouse(b));
            }
        }
        if buttons.just_released(b) {
            release(&mut state, &mut script, run_lua, BindKey::Mouse(b));
        }
    }

    // ── Wheel ── a notch is a press with no release (which is exactly why the table refuses
    // wheel chords on runOnUp commands); over UI the wheel belongs to the hovered frame.
    // Trackpads report pixel deltas — normalized to line-equivalents so the zoom consumer's
    // feel is unchanged from when it read the scroll itself.
    let wheel = match scroll.unit {
        bevy::input::mouse::MouseScrollUnit::Line => scroll.delta.y,
        bevy::input::mouse::MouseScrollUnit::Pixel => {
            scroll.delta.y / bevy::input::mouse::MouseScrollUnit::SCROLL_UNIT_CONVERSION_FACTOR
        }
    };
    if wheel != 0.0 && !armed && !sup && !over_ui.0 {
        let (key, amount) = if wheel > 0.0 {
            (BindKey::WheelUp, wheel)
        } else {
            (BindKey::WheelDown, -wheel)
        };
        let chord = Chord {
            alt,
            ctrl,
            shift,
            key,
        };
        if let Some(&cmd) = dispatch.map.get(&chord) {
            match &SPECS[cmd.0 as usize].kind {
                Kind::Edge(lua) => run_lua(&mut script, lua, SPECS[cmd.0 as usize].name),
                Kind::Host => {
                    state.fired.push(cmd);
                    state.amounts.push((cmd, amount));
                }
                // Unreachable by construction (SetBinding refuses these); harmless if a
                // hand-edited file smuggles one in.
                Kind::Held | Kind::EdgeUpDown(..) => {}
            }
        }
    }

    // ── The stuck-latch sweep ── a release the window never saw (focus loss, the macOS
    // modifier eater's cousin): any latch whose base key reads up in the input state unlatches
    // now, firing its up-half so a pushed action button unsticks visibly.
    let mut stuck: Vec<BindKey> = Vec::new();
    for &(k, _) in &state.latched {
        let up = match k {
            BindKey::Key(kc) => {
                !keys.pressed(kc) && !(kc == KeyCode::Enter && keys.pressed(KeyCode::NumpadEnter))
            }
            BindKey::Mouse(b) => !buttons.pressed(b),
            BindKey::WheelUp | BindKey::WheelDown => true,
        };
        if up && !stuck.contains(&k) {
            stuck.push(k);
        }
    }
    for k in stuck {
        release(&mut state, &mut script, run_lua, k);
    }
}

#[cfg(test)]
impl BindingDispatch {
    /// A dispatch seeded straight from the registry defaults — the no-VM test seam.
    fn test_defaults() -> Self {
        let mut map = std::collections::HashMap::new();
        for (i, s) in SPECS.iter().enumerate() {
            for d in [s.d1, s.d2].into_iter().flatten() {
                map.insert(Chord::parse(d).expect("default parses"), Cmd(i as u16));
            }
        }
        Self {
            map,
            seen_generation: None,
        }
    }
}

/// One matching press: latch the held kinds, run/fire by class.
fn press(
    state: &mut BindingsState,
    script: &mut Option<NonSendMut<UiScript>>,
    run_lua: impl Fn(&mut Option<NonSendMut<UiScript>>, &str, &str),
    cmd: Cmd,
    key: BindKey,
) {
    let spec = &SPECS[cmd.0 as usize];
    match &spec.kind {
        Kind::Held => {
            if !state.pressed(cmd) {
                state.just.push(cmd);
            }
            state.latched.push((key, cmd));
        }
        Kind::Edge(lua) => run_lua(script, lua, spec.name),
        Kind::EdgeUpDown(down, _) => {
            run_lua(script, down, spec.name);
            state.latched.push((key, cmd));
        }
        Kind::Host => {
            state.fired.push(cmd);
            if !state.pressed(cmd) {
                state.just.push(cmd);
            }
            state.amounts.push((cmd, 1.0));
        }
    }
}

/// A base key's release: drop its latch; a runOnUp pair fires its up-half (delivered even while
/// typing — the reference completes a pressed binding's release regardless of focus).
fn release(
    state: &mut BindingsState,
    script: &mut Option<NonSendMut<UiScript>>,
    run_lua: impl Fn(&mut Option<NonSendMut<UiScript>>, &str, &str),
    key: BindKey,
) {
    let mut i = 0;
    while i < state.latched.len() {
        if state.latched[i].0 == key {
            let (_, cmd) = state.latched.remove(i);
            if let Kind::EdgeUpDown(_, up) = &SPECS[cmd.0 as usize].kind {
                run_lua(script, up, SPECS[cmd.0 as usize].name);
            }
        } else {
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use bevy::input::keyboard::{Key, KeyboardInput};
    use bevy::input::mouse::{MouseButtonInput, MouseScrollUnit, MouseWheel};

    use super::*;

    /// A minimal app around [`latch_and_dispatch`] with the registry defaults and NO VM — the
    /// keyboard/mouse laws end-to-end from real input events, exactly the way winit feeds them
    /// (InputPlugin turns the events into `ButtonInput`/`AccumulatedMouseScroll` in PreUpdate;
    /// the dispatch runs in Update).
    fn harness() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::input::InputPlugin))
            .init_resource::<UiKeyboardCapture>()
            .init_resource::<PlayerUiHover>()
            .init_resource::<PointerOverUi>()
            .init_resource::<BindingsState>()
            .insert_resource(BindingDispatch::test_defaults())
            .add_systems(Update, latch_and_dispatch);
        app
    }

    fn key(app: &mut App, k: KeyCode, state: bevy::input::ButtonState, repeat: bool) {
        app.world_mut().write_message(KeyboardInput {
            key_code: k,
            logical_key: Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
            state,
            text: None,
            repeat,
            window: Entity::PLACEHOLDER,
        });
    }
    fn press_key(app: &mut App, k: KeyCode) {
        key(app, k, bevy::input::ButtonState::Pressed, false);
    }
    fn release_key(app: &mut App, k: KeyCode) {
        key(app, k, bevy::input::ButtonState::Released, false);
    }
    fn state(app: &App) -> &BindingsState {
        app.world().resource::<BindingsState>()
    }

    #[test]
    fn held_commands_latch_across_frames_and_release_per_base_key() {
        let mut app = harness();
        // W and UP are both MOVEFORWARD: press both, release one — still moving.
        press_key(&mut app, KeyCode::KeyW);
        app.update();
        assert!(state(&app).pressed(cmd::MOVE_FORWARD));
        assert!(state(&app).just_pressed(cmd::MOVE_FORWARD), "press edge");
        press_key(&mut app, KeyCode::ArrowUp);
        app.update();
        assert!(state(&app).pressed(cmd::MOVE_FORWARD));
        assert!(
            !state(&app).just_pressed(cmd::MOVE_FORWARD),
            "second key on an already-held command is no new edge"
        );
        release_key(&mut app, KeyCode::KeyW);
        app.update();
        assert!(state(&app).pressed(cmd::MOVE_FORWARD), "UP still holds it");
        release_key(&mut app, KeyCode::ArrowUp);
        app.update();
        assert!(!state(&app).pressed(cmd::MOVE_FORWARD));
        // A repeat press (held-key auto-repeat) neither re-latches nor re-edges.
        press_key(&mut app, KeyCode::KeyW);
        app.update();
        key(
            &mut app,
            KeyCode::KeyW,
            bevy::input::ButtonState::Pressed,
            true,
        );
        app.update();
        assert!(state(&app).pressed(cmd::MOVE_FORWARD));
        assert!(!state(&app).just_pressed(cmd::MOVE_FORWARD));
    }

    #[test]
    fn the_exact_modifier_law_holds_table_wide() {
        // Bare Z is the sheath toggle; ALT-Z is TOGGLEUI; CTRL-ALT-Z is nobody's.
        let mut app = harness();
        press_key(&mut app, KeyCode::KeyZ);
        app.update();
        assert!(state(&app).fired(cmd::TOGGLE_SHEATH));
        assert!(!state(&app).fired(cmd::TOGGLE_UI));
        release_key(&mut app, KeyCode::KeyZ);
        press_key(&mut app, KeyCode::AltLeft);
        press_key(&mut app, KeyCode::KeyZ);
        app.update();
        assert!(
            state(&app).fired(cmd::TOGGLE_UI),
            "ALT-Z is TOGGLEUI (0870)"
        );
        assert!(!state(&app).fired(cmd::TOGGLE_SHEATH));
        release_key(&mut app, KeyCode::KeyZ);
        press_key(&mut app, KeyCode::ControlLeft);
        press_key(&mut app, KeyCode::KeyZ);
        app.update();
        assert!(
            !state(&app).fired(cmd::TOGGLE_UI) && !state(&app).fired(cmd::TOGGLE_SHEATH),
            "CTRL-ALT-Z matches neither entry (0585)"
        );
        // TAB vs SHIFT-TAB are two different commands now.
        let mut app = harness();
        press_key(&mut app, KeyCode::Tab);
        app.update();
        assert!(state(&app).fired(cmd::TARGET_NEAREST_ENEMY));
        release_key(&mut app, KeyCode::Tab);
        press_key(&mut app, KeyCode::ShiftLeft);
        press_key(&mut app, KeyCode::Tab);
        app.update();
        assert!(state(&app).fired(cmd::TARGET_PREVIOUS_ENEMY));
        assert!(!state(&app).fired(cmd::TARGET_NEAREST_ENEMY));
        // Super/Cmd is never a binding modifier: a super-held press matches nothing.
        let mut app = harness();
        press_key(&mut app, KeyCode::SuperLeft);
        press_key(&mut app, KeyCode::KeyZ);
        app.update();
        assert!(!state(&app).fired(cmd::TOGGLE_SHEATH));
    }

    /// **The pet lane routes on the CTRL digits, and the number row is untouched** (B218,
    /// decision 1052). The two share their base keys, so the only thing keeping them apart is
    /// the exact-modifier law — worth pinning on the pair that actually collides rather than
    /// trusting the law in the abstract. CTRL-0 is slot **10**, the 1.12 cache's own wrap.
    #[test]
    fn the_pet_lane_dispatches_on_the_ctrl_digits() {
        let by_name =
            |n: &str| Cmd(SPECS.iter().position(|s| s.name == n).expect("registered") as u16);
        let mut app = harness();
        press_key(&mut app, KeyCode::ControlLeft);
        press_key(&mut app, KeyCode::Digit1);
        app.update();
        assert!(state(&app).pressed(by_name("BONUSACTIONBUTTON1")));
        assert!(
            !state(&app).pressed(by_name("ACTIONBUTTON1")),
            "the modifier decides: CTRL-1 is not the number row's"
        );
        // The runOnUp half: the latch drops on the BASE key's release (which is what runs
        // BenillaPetActionButtonUp → CastPetAction in the app's VM), Ctrl still held.
        release_key(&mut app, KeyCode::Digit1);
        app.update();
        assert!(!state(&app).pressed(by_name("BONUSACTIONBUTTON1")));
        // CTRL-0 → slot 10.
        press_key(&mut app, KeyCode::Digit0);
        app.update();
        assert!(state(&app).pressed(by_name("BONUSACTIONBUTTON10")));
        // Bare 1 is still the action bar's, with no pet command in sight.
        let mut app = harness();
        press_key(&mut app, KeyCode::Digit1);
        app.update();
        assert!(state(&app).pressed(by_name("ACTIONBUTTON1")));
        assert!(!state(&app).pressed(by_name("BONUSACTIONBUTTON1")));
    }

    #[test]
    fn the_typing_gate_blocks_new_input_and_clears_held_latches_once() {
        let mut app = harness();
        press_key(&mut app, KeyCode::KeyW);
        app.update();
        assert!(state(&app).pressed(cmd::MOVE_FORWARD));
        // A box takes focus: movement stops (the reference's focus handler releases the
        // direction bits), and new presses do nothing.
        app.world_mut().resource_mut::<UiKeyboardCapture>().0 = true;
        app.update();
        assert!(
            !state(&app).pressed(cmd::MOVE_FORWARD),
            "latches clear on the capture edge"
        );
        press_key(&mut app, KeyCode::KeyX);
        app.update();
        assert!(
            !state(&app).fired(cmd::SIT_OR_STAND),
            "typed keys are not bindings"
        );
        // Focus drops; keys work again.
        release_key(&mut app, KeyCode::KeyX);
        app.world_mut().resource_mut::<UiKeyboardCapture>().0 = false;
        app.update();
        press_key(&mut app, KeyCode::KeyX);
        app.update();
        assert!(state(&app).fired(cmd::SIT_OR_STAND));
    }

    #[test]
    fn mouse_buttons_bind_only_over_the_world_and_the_wheel_respects_ui() {
        let mut app = harness();
        // BUTTON4 (winit Forward) is TOGGLEAUTORUN's second default.
        app.world_mut().write_message(MouseButtonInput {
            button: MouseButton::Forward,
            state: bevy::input::ButtonState::Pressed,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert!(state(&app).fired(cmd::TOGGLE_AUTORUN));
        // Over a UI frame the press belongs to the frame.
        app.world_mut().write_message(MouseButtonInput {
            button: MouseButton::Forward,
            state: bevy::input::ButtonState::Released,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        app.world_mut().resource_mut::<PlayerUiHover>().0 = Some(7);
        app.world_mut().write_message(MouseButtonInput {
            button: MouseButton::Forward,
            state: bevy::input::ButtonState::Pressed,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert!(!state(&app).fired(cmd::TOGGLE_AUTORUN));
        // MOVEANDSTEER (BUTTON3) is a HELD command on a mouse button: press latches, release ends.
        let mut app = harness();
        app.world_mut().write_message(MouseButtonInput {
            button: MouseButton::Middle,
            state: bevy::input::ButtonState::Pressed,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert!(state(&app).pressed(cmd::MOVE_AND_STEER));
        app.world_mut().write_message(MouseButtonInput {
            button: MouseButton::Middle,
            state: bevy::input::ButtonState::Released,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert!(!state(&app).pressed(cmd::MOVE_AND_STEER));
        // The wheel: a notch fires CAMERAZOOMIN with its amount; over UI it belongs to the frame.
        let mut app = harness();
        app.world_mut().write_message(MouseWheel {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: 2.0,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert!(state(&app).fired(cmd::CAMERA_ZOOM_IN));
        assert_eq!(state(&app).amount(cmd::CAMERA_ZOOM_IN), 2.0);
        app.world_mut().resource_mut::<PointerOverUi>().0 = true;
        app.world_mut().write_message(MouseWheel {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: 2.0,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert!(
            !state(&app).fired(cmd::CAMERA_ZOOM_IN),
            "a UI wheel is the frame's"
        );
    }
}
