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
//! - a press probes its **exact** chord and then, only on a miss, **once more** with its leftmost
//!   modifier dropped ([`Chord::fallback`], decision 1142) — which is why `Shift`+`W` walks while
//!   `SHIFT-W` is bound to nothing, and why a bound `SHIFT-TAB` still beats bare `TAB` (the exact
//!   probe is always first). Super held matches nothing;
//! - [`Kind::Held`] commands **latch** on the matching press and unlatch on the *base key's*
//!   release — the reference's `runOnUp` movement law, which is why tapping Shift mid-run does
//!   not stop you, and why a chat box taking focus stops movement (latches clear on the capture
//!   edge — the reference's own focus handler, `0x514490`) without eating the release;
//! - [`Kind::Edge`]/[`Kind::EdgeUpDown`] run their 1.12 Lua bodies in the VM;
//! - [`Kind::Host`] lands in [`BindingsState::fired`] for engine consumers (chat open, TAB
//!   targeting, nameplates, autorun, camera zoom, …).
//!
//! **An addon's `Bindings.xml` rows dispatch here too** (decision 1188 phase 4). They are not in
//! [`SPECS`] — they are read off disk at addon load ([`benilla_ui::bindings_xml`]) — so a resolved
//! chord names a [`Bound`], which is either a registry [`Cmd`] or an index into this frame's
//! addon table. That enum is the whole design: an addon row is a *runtime* `String` body run with
//! the reference's `keystate` global, and every `SPECS[cmd]` in this file would be a latent panic
//! if it were instead an index past the end of the static table.
//!
//! While the Keybindings page (the Options window's category since 1008) has a capsule
//! selected it arms the **capture seam** (`BenillaBindCapture`): raw input is swallowed here,
//! canonicalized (`ALT-CTRL-SHIFT-<TOKEN>`), and handed back to the page's own capture handler
//! — the 1.12 law (lone modifiers ignored, left/right clicks stay UI clicks, ESC binds like
//! any key).
//!
//! Persistence: `benilla-config/bindings/account.txt` + `<Realm>-<Char>.txt` ([`store`], through
//! [`crate::local_state`]); the character file's existence is the character-set state, deleted
//! on the confirmed switch back to general — the reference's own semantics.

use bevy::input::keyboard::KeyboardInput;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::input::ButtonState;
use bevy::prelude::*;

use benilla_ui::script::keybind::{AddonBindingBody, KeybindCommand, KeybindRequest};
use benilla_ui::script::UiScript;

use crate::char_select::ClientState;
use crate::ui_script::{PlayerUiHover, PointerOverUi, UiKeyboardCapture};

pub(crate) mod chord;
pub(crate) mod commands;
mod store;

use chord::{BindKey, Chord};
pub(crate) use commands::cmd;
use commands::{Cmd, Kind, SPECS};

/// What a bound chord names — a registry command, or an addon's `Bindings.xml` body
/// (decision 1188 phase 4).
///
/// **An enum rather than one index space with a sentinel in it.** The tempting shape is
/// "`Cmd(u16)`, and anything `>= SPECS.len()` is an addon" — which would turn every one of this
/// file's eleven `SPECS[cmd.0 as usize]` reads into a panic waiting for the first addon binding to
/// be pressed. The compiler is what should be enforcing that split, so it does: `state.fired` /
/// `state.just` / `state.amounts` stay [`Cmd`]-typed (a host command is always a built-in and
/// there is nothing for an addon to fire into), and only the map and the latches carry a `Bound`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bound {
    /// A registry command — indexes [`SPECS`].
    Spec(Cmd),
    /// An addon-declared binding — indexes [`BindingDispatch::addons`].
    Addon(u16),
}

/// The derived chord→binding map — rebuilt whenever the engine table's generation moves. Probed
/// through [`BindingDispatch::resolve`], never directly: the lookup is two probes, not one.
#[derive(Resource, Default)]
struct BindingDispatch {
    map: std::collections::HashMap<Chord, Bound>,
    /// The addon-declared bindings [`Bound::Addon`] indexes, in the engine table's registration
    /// order — re-snapshotted with the map, so the two can never disagree about what an index
    /// means. Empty in every session with no addon bindings, which is every session today.
    addons: Vec<AddonBindingBody>,
    /// The engine table's generation this map was built from. **Session-keyed** (1290): a fresh VM
    /// restarts its counter at 0, so a bare memo could hold a higher number than the live VM will
    /// ever reach and gate the rebuild off for the whole session.
    seen_generation: crate::ui_script::VmMemo<Option<u64>>,
}

impl BindingDispatch {
    /// Resolve a press to its command — the reference's lookup (`CBindings::ExecuteBinding`
    /// `0x4b7990`, decision 1142): the exact chord, then **one** retry with the leftmost modifier
    /// dropped ([`Chord::fallback`]). The exact probe always runs first, so a bound specific chord
    /// always beats the general one.
    ///
    /// `dev_plane` suppresses the **retry only**, and only on the keyboard path. `Ctrl`+`Shift` is
    /// ours ([`benilla_world::modkeys::DEV_CHORD`]), and 0870 picked it on the argument that the plane
    /// was empty — an argument that rested on the exact-match law 1142 corrects. It is
    /// *nearly* empty anyway under the real law, because the single strip drops `CTRL` and leaves
    /// `SHIFT-`*key*, never reaching the bare letter — but "nearly" is not a plane: `SHIFT-P` is a
    /// live 1.12 default (`TOGGLECHARACTER3`, 1057), so `Ctrl`+`Shift`+`P` would open the pet
    /// paper doll under the perf HUD. That is 0585's original bug, and this is the same fix it
    /// made, now correctly scoped: an *exact* `CTRL-SHIFT-` binding still dispatches, so the
    /// reference's own two (`CTRL-SHIFT-TAB`, `CTRL-SHIFT-PAGEDOWN`) and anything a player binds
    /// there are untouched. Only the plane's fallback shadow is ours. Mouse and wheel are not
    /// suppressed — every dev instrument is a letter, so a modified click is nobody's but the
    /// game's.
    fn resolve(&self, chord: Chord, dev_plane: bool) -> Option<Bound> {
        if let Some(&bound) = self.map.get(&chord) {
            return Some(bound);
        }
        if dev_plane {
            return None;
        }
        self.map.get(&chord.fallback()?).copied()
    }
}

/// This frame's binding activity — what the engine-side consumers read instead of raw keys.
#[derive(Resource, Default)]
pub(crate) struct BindingsState {
    /// Live latches: (base key, binding) — a [`Kind::Held`], [`Kind::EdgeUpDown`] or `runOnUp`
    /// addon press that has not released yet.
    latched: Vec<(BindKey, Bound)>,
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
    /// Is a held command latched right now? (The reference's held movement bit.) Registry
    /// commands only — an addon's latch exists to deliver its release half, and no engine system
    /// reads it.
    pub(crate) fn pressed(&self, c: Cmd) -> bool {
        self.latched.iter().any(|&(_, l)| l == Bound::Spec(c))
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

    // ── The synthetic seam ── commands asserted by something that is not a key: the web bridge
    // (`crate::webbridge`). Each is an ordinary latch/fire under [`BindKey::Synth`], so every
    // consumer reads it through the same four accessors above and cannot tell it from a key —
    // the `follow_forward` precedent (`player/input.rs`: "synthesized input is not a keypress",
    // but it IS a latch). The typing edge drops a synthetic Held like any other; the stuck-latch
    // sweep never touches one (there is no physical key to read up); a VM swap clears it with the
    // rest, and the bridge re-asserts next frame.

    /// Assert a [`Kind::Held`] command for this frame. Idempotent: a command already latched
    /// (synthetically or by a key) gains nothing; a fresh latch lands its press edge in `just`,
    /// exactly as a key-DOWN would, so the autorun cancel set sees it. Non-Held kinds are
    /// ignored — an Edge/EdgeUpDown body has to run through the VM ([`Self::synth_fire`] is the
    /// wrong tool too; the bridge runs those bodies itself).
    pub(crate) fn synth_hold(&mut self, cmd: Cmd) {
        if !matches!(SPECS[cmd.0 as usize].kind, Kind::Held) {
            return;
        }
        let key = BindKey::Synth(cmd);
        if self.latched.iter().any(|&(k, _)| k == key) {
            return;
        }
        if !self.pressed(cmd) {
            self.just.push(cmd);
        }
        self.latched.push((key, Bound::Spec(cmd)));
    }

    /// Drop this command's synthetic latch, if any. A key's latch on the same command is untouched.
    pub(crate) fn synth_release(&mut self, cmd: Cmd) {
        self.latched.retain(|&(k, _)| k != BindKey::Synth(cmd));
    }

    /// Drop every synthetic latch — world exit, the typing edge, the bridge letting go.
    pub(crate) fn synth_release_all(&mut self) {
        self.latched
            .retain(|&(k, _)| !matches!(k, BindKey::Synth(_)));
    }

    /// Fire a [`Kind::Host`] command once this frame with an analog `amount` (the wheel's notch
    /// magnitude for the zoom; `1.0` is the reference's key step) — the wheel path's host arm
    /// (`latch_and_dispatch`), minus the chord. Non-Host kinds are ignored, for the reason on
    /// [`Self::synth_hold`].
    pub(crate) fn synth_fire(&mut self, cmd: Cmd, amount: f32) {
        if !matches!(SPECS[cmd.0 as usize].kind, Kind::Host) {
            return;
        }
        self.fired.push(cmd);
        if !self.pressed(cmd) {
            self.just.push(cmd);
        }
        self.amounts.push((cmd, amount));
    }
}

/// Which files this session's bindings live in — the macros-files pattern
/// ([`crate::ui_macro::MacroFiles`]), resolved once the character is known.
#[derive(Resource, Default)]
struct BindingFiles {
    account: Option<std::path::PathBuf>,
    character: Option<std::path::PathBuf>,
    /// Whose set 2 is loaded — **in the VM that is live now**. See [`load_character_bindings`].
    identity: crate::ui_script::VmMemo<Option<(String, String)>>,
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
            .add_systems(
                Update,
                (
                    // Once per **VM**, not once per process (decision 1290): a login builds a
                    // fresh one, and an unseeded VM has no command registry at all —
                    // `sync_dispatch` would build an empty map and every keybind in the session
                    // would be dead. Hence `Update` with a session-keyed claim rather than
                    // `PostStartup`, ordered ahead of the pair that reads what it registers.
                    seed_bindings.before(sync_dispatch),
                    (sync_dispatch, latch_and_dispatch)
                        .chain()
                        .in_set(crate::ui_script::UiInput)
                        .in_set(BindingSet)
                        .before(benilla_world::schedule::WorldStage::Input)
                        .run_if(in_state(ClientState::InWorld)),
                    load_character_bindings.run_if(in_state(ClientState::InWorld)),
                    drain_binding_requests.after(load_character_bindings),
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

/// Register the command registry with the engine table and seed the account set from disk — once
/// per VM, before any window opens.
fn seed_bindings(
    script: Option<NonSendMut<UiScript>>,
    mut files: ResMut<BindingFiles>,
    mut seeded: Local<crate::ui_script::VmMemo<bool>>,
) {
    let Some(mut script) = script else { return };
    if !seeded.claim(&script) {
        return;
    }
    script.register_bindings(&registry_commands());
    files.account = crate::local_state::bindings_account_path();
    if let Some(overrides) = read_diff(&files.account) {
        script.seed_binding_set(1, Some(store::resolve(&overrides)));
    } else {
        script.seed_binding_set(1, Some(store::resolve(&[])));
    }
    script.load_binding_set(1);
    // The pair, not just the first half: `SPECS` ∪ `ABSENT` is the client's whole 1.12 command
    // surface, and a log line that says only how many landed cannot say how much is left
    // (decision 1745).
    info!(
        "bindings: {} of {} 1.12 commands registered ({} recorded absent)",
        SPECS.len(),
        SPECS.len() + commands::ABSENT.len(),
        commands::ABSENT.len()
    );
}

/// Read + parse one diff file; `None` when absent/unreadable (defaults).
fn read_diff(path: &Option<std::path::PathBuf>) -> Option<Vec<(String, Vec<String>)>> {
    let path = path.as_ref()?;
    // Through `local_state` (not `std::fs`): on the web the state folder is `localStorage`, and
    // a direct read there is a silent "not found" — bindings were saved and never read back.
    match crate::local_state::read_to_string(path) {
        Ok(text) => {
            let diff = store::from_diff(&text);
            info!("bindings: {} overrides read from {}", diff.len(), path.display());
            Some(diff)
        }
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
    // Session-keyed (1290): re-entering the world as the SAME character still meets a fresh VM
    // with no set 2 in it, so "same identity" is only a reason to skip within one VM.
    if files.identity.get(&script).as_ref() == Some(&id) {
        return;
    }
    files.character = crate::local_state::bindings_character_path(&id.0, &id.1);
    *files.identity.get(&script) = Some(id);
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

/// Drain the VM's queued binding requests.
///
/// Two of them. `Save` persists on the window's SaveBindings (Okay): write the set's diff; saving
/// account while a character file exists deletes it — the confirmed permanent delete. `Run` fires
/// a named command's action outright, which is `RunBinding(name)` — the reference's own
/// passthrough verb, whose one shipped caller is `CinematicFrame`'s `OnKeyDown` handing the
/// SCREENSHOT chord back to its binding while it swallows every other key (decision 1724).
fn drain_binding_requests(script: Option<NonSendMut<UiScript>>, files: Res<BindingFiles>) {
    let Some(mut script) = script else { return };
    for req in script.take_keybind_requests() {
        // `RunBinding(name)` — fire the named command's action as if its chord had been pressed.
        // Only the two Lua-bodied kinds can be run this way: a `Held`/`Host` command's action is a
        // bit in the movement word, which a Lua call has no frame to assert for. The one shipped
        // caller is `CinematicFrame`'s SCREENSHOT passthrough, and `SCREENSHOT` is `Kind::Edge`.
        let which = match req {
            KeybindRequest::Save(which) => which,
            KeybindRequest::Run(name) => {
                match SPECS.iter().find(|s| s.name == name).map(|s| &s.kind) {
                    Some(Kind::Edge(lua)) | Some(Kind::EdgeUpDown(lua, _)) => {
                        let lua = *lua;
                        if let Err(e) = script.run(lua) {
                            warn!("bindings(RunBinding {name}): {e}");
                        }
                    }
                    Some(_) => warn!("RunBinding({name}): a held/engine action has no body to run"),
                    None => warn!("RunBinding({name}): no such command"),
                }
                continue;
            }
        };
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
                match crate::local_state::remove_file(chr) {
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
    if *dispatch.seen_generation.get(&script) == Some(generation) {
        return;
    }
    *dispatch.seen_generation.get(&script) = Some(generation);
    dispatch.map.clear();
    // The addon table first: it is what the names the registry does not know resolve into. Before
    // 1188 phase 4 an unknown name hit the `continue` below and the binding silently never fired
    // — it registered, listed in the window, saved and loaded, and did nothing.
    let addons = script.addon_binding_bodies();
    let mut by_name: std::collections::HashMap<&str, Bound> = SPECS
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name, Bound::Spec(Cmd(i as u16))))
        .collect();
    for (i, a) in addons.iter().enumerate() {
        // A registry name is never overwritten — the engine table already refuses to register an
        // addon row over one, so this only ever adds. The `try_from` is the honest form of the
        // cast: 65k addon bindings is not a real case, and a wrapped index would dispatch the
        // wrong body rather than none.
        let Ok(i) = u16::try_from(i) else { break };
        by_name.entry(a.name.as_str()).or_insert(Bound::Addon(i));
    }
    for (name, keys) in script.keybind_snapshot() {
        let Some(&bound) = by_name.get(name.as_str()) else {
            // A name with no home: either an uninstalled addon's row (1201 keeps those on
            // purpose) or a 1.12 command this client does not implement. The second case is the
            // one that used to mystify — a player carrying their own bindings over presses the
            // key they have always pressed and nothing happens, with nothing anywhere saying
            // why. `ABSENT` knows why, so say it (decision 1745).
            if let Some(absent) = commands::ABSENT.iter().find(|a| a.name == name) {
                if !keys.is_empty() {
                    warn!(
                        "bindings: {name} is bound to {keys:?} but benilla does not implement it \u{2014} {}",
                        absent.why
                    );
                }
            }
            continue;
        };
        for key in keys {
            match Chord::parse(&key) {
                Some(ch) => {
                    dispatch.map.insert(ch, bound);
                }
                None => warn!("bindings: {name}: unpressable chord '{key}' (unknown token)"),
            }
        }
    }
    dispatch.addons = addons;
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
    mut same_vm: Local<crate::ui_script::VmMemo<bool>>,
) {
    state.just.clear();
    state.fired.clear();
    state.amounts.clear();

    // A latch indexes the dispatch table snapshotted from the VM that latched it. When the VM is
    // replaced mid-hold (a `/reload` with a key down), releasing against the NEW table would run
    // the wrong addon's `keystate="up"` body — or swallow the release and leave a Held latched
    // with no Stop. So latches die with the VM they were made against (decision 1291); a key
    // still physically down re-latches on its next press edge. The reference keeps a held key
    // running through a `ReloadUI` (its dispatch is engine-side) — dropping is the safe
    // divergence, over the moment the key is pressed again.
    if let Some(script) = script.as_ref() {
        if same_vm.claim(script) {
            state.latched.clear();
        }
    }

    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    let sup = keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight);
    // Exactly the dev overlays' plane (`modkeys::dev_chord`, minus the Super arm the `sup` gate
    // below already covers) — it costs the keyboard its fallback probe. See
    // [`BindingDispatch::resolve`].
    //
    // **Only when there is something on the plane** (decision 1179). A player build holds no dev
    // chord at all, so suppressing the reference's fallback there buys nothing and costs fidelity:
    // `CTRL-SHIFT-P` would resolve to `None` instead of falling through to `SHIFT-P`
    // (`TOGGLECHARACTER3`, the pet paper doll) the way the binary does. 1176 gated what the plane
    // *offers* and left what it *costs*; this is the other half.
    let dev_plane = ctrl && shift && !alt && crate::run_mode::dev_affordances();

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
    let typing = capture.typing;
    if typing && !state.was_typing {
        state.latched.retain(|&(_, b)| match b {
            Bound::Spec(c) => !matches!(SPECS[c.0 as usize].kind, Kind::Held),
            // An addon's `runOnUp` latch is an EdgeUpDown pair by another name — its release half
            // is a Lua body that must still run, so it stays armed for the same reason.
            Bound::Addon(_) => true,
        });
    }
    state.was_typing = typing;

    // ── Keyboard ── press edges latch/fire (exact-modifier chord match, no repeats, gated on
    // typing and the capture arm); release edges unlatch and fire the runOnUp up-half.
    for ev in keyboard.read() {
        let key = chord::normalize_key(ev.key_code);
        match ev.state {
            ButtonState::Pressed => {
                // The alt-arrow exemption: a focused box in alt-arrow mode declines the four
                // arrows, so their bindings DO fire even while typing — which is the whole
                // point of the flag (turn the camera with the chat box open). Every other key
                // a focused box still swallows. See `UiKeyboardCapture::arrows_fall_through`.
                let arrow_exempt = capture.arrows_fall_through
                    && matches!(
                        ev.key_code,
                        KeyCode::ArrowLeft
                            | KeyCode::ArrowRight
                            | KeyCode::ArrowUp
                            | KeyCode::ArrowDown
                    );
                if armed || (typing && !arrow_exempt) || sup || ev.repeat {
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
                if let Some(bound) = dispatch.resolve(chord, dev_plane) {
                    press(
                        &mut state,
                        &mut script,
                        run_lua,
                        &dispatch,
                        bound,
                        BindKey::Key(key),
                    );
                }
            }
            ButtonState::Released => {
                release(
                    &mut state,
                    &mut script,
                    run_lua,
                    &dispatch,
                    BindKey::Key(key),
                );
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
            if let Some(bound) = dispatch.resolve(chord, false) {
                press(
                    &mut state,
                    &mut script,
                    run_lua,
                    &dispatch,
                    bound,
                    BindKey::Mouse(b),
                );
            }
        }
        if buttons.just_released(b) {
            release(
                &mut state,
                &mut script,
                run_lua,
                &dispatch,
                BindKey::Mouse(b),
            );
        }
    }

    // ── Wheel ── **a notch is a press AND its release, back to back.** The reference builds one
    // chord and hands it to `CBindings::ExecuteBinding` twice — `isDown=1` at `0x483d6f`, then
    // `isDown=0` at `0x483d82` (wow-re `system/ui/ui.md` §3, VERIFIED) — so a `runOnUp` command
    // runs both halves in the same frame and a plain one runs its single half (the up leg is the
    // `RunCommand 0x4b7b50` no-op: `UP + !runOnUp` returns without running anything). Before this
    // the notch was a press with no release, which quietly made every press+release command a
    // dead wheel binding. Over UI the wheel belongs to the hovered frame.
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
        match dispatch.resolve(chord, false) {
            // A host command is the one thing the press/release pair cannot carry: its input is
            // the notch's ANALOG MAGNITUDE (the camera zoom's), where [`press`] can only spend
            // the reference's 1.0 key step. It has no release half either way.
            Some(Bound::Spec(cmd)) if matches!(SPECS[cmd.0 as usize].kind, Kind::Host) => {
                state.fired.push(cmd);
                state.amounts.push((cmd, amount));
            }
            // Everything else is the reference's own pair. `Kind::Edge` runs its one body and
            // has nothing to release; `Kind::EdgeUpDown` runs BOTH halves now (a wheel-bound
            // action button presses and releases in the notch, which is what makes it cast); an
            // addon's `runOnUp` body runs twice, `keystate` "down" then "up". `Kind::Held`
            // latches and unlatches before any consumer can observe it — which is the
            // reference's behaviour too, its movement bit being set and cleared in the one tick.
            Some(bound) => {
                press(&mut state, &mut script, run_lua, &dispatch, bound, key);
                release(&mut state, &mut script, run_lua, &dispatch, key);
            }
            None => {}
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
            // No physical key to read: the bridge that asserted it is the one that releases it
            // (`BindingsState::synth_release`), every frame it still wants the hold.
            BindKey::Synth(_) => false,
        };
        if up && !stuck.contains(&k) {
            stuck.push(k);
        }
    }
    for k in stuck {
        release(&mut state, &mut script, run_lua, &dispatch, k);
    }
}

#[cfg(test)]
impl BindingDispatch {
    /// A dispatch seeded straight from the registry defaults — the no-VM test seam. No addon
    /// bindings: an addon body is Lua, and a harness with no VM could only assert it vacuously
    /// (the with-VM harness below is where those live).
    fn test_defaults() -> Self {
        let mut map = std::collections::HashMap::new();
        for (i, s) in SPECS.iter().enumerate() {
            for d in [s.d1, s.d2].into_iter().flatten() {
                map.insert(
                    Chord::parse(d).expect("default parses"),
                    Bound::Spec(Cmd(i as u16)),
                );
            }
        }
        Self {
            map,
            addons: Vec::new(),
            seen_generation: crate::ui_script::VmMemo::default(),
        }
    }
}

/// One matching press: latch the held kinds, run/fire by class.
fn press(
    state: &mut BindingsState,
    script: &mut Option<NonSendMut<UiScript>>,
    run_lua: impl Fn(&mut Option<NonSendMut<UiScript>>, &str, &str),
    dispatch: &BindingDispatch,
    bound: Bound,
    key: BindKey,
) {
    let cmd = match bound {
        Bound::Spec(cmd) => cmd,
        // An addon's press: run its one body with `keystate = "down"`, and latch it only if it
        // asked for the release half — the `Kind::Edge` / `Kind::EdgeUpDown` fork, decided by the
        // file's `runOnUp` instead of by our own registry.
        Bound::Addon(i) => {
            if let Some(a) = dispatch.addons.get(i as usize) {
                run_addon(script, a, "down");
                if a.run_on_up {
                    state.latched.push((key, bound));
                }
            }
            return;
        }
    };
    let spec = &SPECS[cmd.0 as usize];
    match &spec.kind {
        Kind::Held => {
            if !state.pressed(cmd) {
                state.just.push(cmd);
            }
            state.latched.push((key, bound));
        }
        Kind::Edge(lua) => run_lua(script, lua, spec.name),
        Kind::EdgeUpDown(down, _) => {
            run_lua(script, down, spec.name);
            state.latched.push((key, bound));
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
    dispatch: &BindingDispatch,
    key: BindKey,
) {
    let mut i = 0;
    while i < state.latched.len() {
        if state.latched[i].0 == key {
            match state.latched.remove(i).1 {
                Bound::Spec(cmd) => {
                    if let Kind::EdgeUpDown(_, up) = &SPECS[cmd.0 as usize].kind {
                        run_lua(script, up, SPECS[cmd.0 as usize].name);
                    }
                }
                // The same body again, this time with `keystate = "up"` — the one law that makes
                // an addon's `runOnUp` binding one chunk instead of two.
                Bound::Addon(a) => {
                    if let Some(a) = dispatch.addons.get(a as usize) {
                        run_addon(script, a, "up");
                    }
                }
            }
        } else {
            i += 1;
        }
    }
}

/// Run one addon binding's body with the reference's `keystate` global set **for the duration of
/// the call, and restored after**.
///
/// A `Bindings.xml` body is **one Lua chunk read twice**: `runOnUp="true"` means it runs on the
/// press and again on the release, and every shipped body forks on the bare global `keystate`
/// (`if ( keystate == "down" ) then MoveForwardStart(); else MoveForwardStop(); end`). So the
/// global is set in `_G` where the chunk will look for it — not prepended to the source, which
/// would shift every line number in the addon's own error messages.
///
/// **Restoring it is not tidiness.** `keystate` is absent from the 1.12.1 client's in-world `_G`
/// (`reference/1.12-globals.tsv`) even though its own `Bindings.xml` bodies read it as a bare
/// global — which is only possible if the reference sets it transiently around the call, exactly
/// as it does `this`/`event`/`arg1` (`invoke_with_globals`, RF-0025). Leaving it set would hand
/// every addon a global the reference does not have, and an addon that feature-detects it would
/// take a path we cannot honour — decision 1189's "a superset is not free", one call deeper.
///
/// Save-and-restore rather than set-and-delete, because these bodies nest: a binding whose Lua
/// fires another binding must not clear the outer one's `keystate` on the way out.
fn run_addon(script: &mut Option<NonSendMut<UiScript>>, bind: &AddonBindingBody, keystate: &str) {
    let Some(s) = script.as_mut() else { return };
    let globals = s.lua().globals();
    // `Option<String>` rather than a raw Lua value: benilla-app does not depend on mlua, and
    // `keystate` is a string or nothing — `None` converts back to nil on the way out.
    let prior: Option<String> = globals.get("keystate").unwrap_or(None);
    if let Err(e) = globals.set("keystate", keystate) {
        warn!("bindings({}): setting keystate: {e}", bind.name);
        return;
    }
    if let Err(e) = s.run(&bind.body) {
        warn!("bindings({}): {e}", bind.name);
    }
    if let Err(e) = s.lua().globals().set("keystate", prior) {
        warn!("bindings({}): restoring keystate: {e}", bind.name);
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

    /// One addon's `Bindings.xml`, in the reference's own shape: a `runOnUp` binding whose single
    /// body forks on `keystate`, and a one-shot beside it. Each half counts itself in a global, so
    /// the assertions below read what the VM actually ran rather than what we told it to run.
    const PROBE_BINDINGS: &str = r#"<Bindings>
        <Binding name="PROBEHOLD" runOnUp="true" header="PROBE">
            if ( keystate == "down" ) then
                PROBE_DOWN = (PROBE_DOWN or 0) + 1;
            else
                PROBE_UP = (PROBE_UP or 0) + 1;
            end
            PROBE_LAST = keystate;
        </Binding>
        <Binding name="PROBEEDGE">
            PROBE_EDGE = (PROBE_EDGE or 0) + 1;
        </Binding>
    </Bindings>"#;

    /// The **with-VM** harness: a real engine table (the whole registry plus one addon's parsed
    /// `Bindings.xml`) with the real [`sync_dispatch`] chained in front of [`latch_and_dispatch`].
    ///
    /// Deliberately not [`BindingDispatch::test_defaults`]: the bug 1188 phase 4 removes lived in
    /// the *derivation* — `sync_dispatch` dropped every name it could not find in `SPECS`, so a
    /// hand-built map would assert past exactly the code that was wrong.
    fn vm_harness(script: UiScript) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::input::InputPlugin))
            .init_resource::<UiKeyboardCapture>()
            .init_resource::<PlayerUiHover>()
            .init_resource::<PointerOverUi>()
            .init_resource::<BindingsState>()
            .init_resource::<BindingDispatch>()
            .add_systems(Update, (sync_dispatch, latch_and_dispatch).chain());
        app.insert_non_send_resource(script);
        app
    }

    /// A counter a binding body left in the VM — `0` when the body never ran.
    fn lua_count(app: &App, global: &str) -> i64 {
        app.world()
            .non_send_resource::<UiScript>()
            .eval::<i64>(&format!("return {global} or 0"))
            .expect("eval")
    }

    /// A string a binding body left in the VM — `""` when it never ran.
    fn lua_str(app: &App, global: &str) -> String {
        app.world()
            .non_send_resource::<UiScript>()
            .eval::<String>(&format!(r#"return {global} or """#))
            .expect("eval")
    }

    /// **An addon's binding actually fires** (decision 1188 phase 4) — the assertion the whole
    /// phase exists for. Before it, an addon's `Bindings.xml` registered, listed in the Key
    /// Bindings window, saved and loaded, and then did *nothing*: `sync_dispatch` silently dropped
    /// every name that was not in [`SPECS`], so no chord ever reached the body.
    ///
    /// The `runOnUp` half is the part that cannot be guessed from the built-in path, because it is
    /// shaped differently: our own registry holds two Lua strings ([`Kind::EdgeUpDown`]), an
    /// addon holds **one chunk run twice** with the global `keystate` set to `"down"` then
    /// `"up"`. A dispatch that ran the body once, or twice with the same `keystate`, leaves the
    /// player's key held down forever — which is what every shipped `runOnUp` body's `else` branch
    /// is there to prevent.
    #[test]
    fn an_addon_binding_fires_its_lua_and_runs_again_on_release_when_it_asked_to() {
        let mut script = UiScript::new().expect("VM");
        script.register_bindings(&registry_commands());
        script.register_addon_bindings(
            "ProbeAddon",
            &benilla_ui::bindings_xml::parse(PROBE_BINDINGS).expect("well-formed"),
        );
        // A `<Binding>` ships no default chord in 1.12 — the shipped defaults live in the engine's
        // own table — so an addon binding starts unbound and the player binds it, which is what
        // the Key Bindings window does through exactly this call.
        script
            .run(r#"SetBinding("J", "PROBEHOLD"); SetBinding("G", "PROBEEDGE")"#)
            .expect("bind");
        let mut app = vm_harness(script);

        // Press: one run, `keystate == "down"`.
        press_key(&mut app, KeyCode::KeyJ);
        app.update();
        assert_eq!(
            lua_count(&app, "PROBE_DOWN"),
            1,
            "the press must reach the addon's body — this is the phase-4 bug"
        );
        assert_eq!(lua_str(&app, "PROBE_LAST"), "down");
        assert_eq!(
            lua_count(&app, "PROBE_UP"),
            0,
            "no release has happened yet"
        );

        // Release: the SAME body again, with `keystate == "up"`.
        release_key(&mut app, KeyCode::KeyJ);
        app.update();
        assert_eq!(lua_count(&app, "PROBE_UP"), 1);
        assert_eq!(lua_str(&app, "PROBE_LAST"), "up");
        assert_eq!(
            lua_count(&app, "PROBE_DOWN"),
            1,
            "the release runs the chunk with keystate=up, not the down half a second time"
        );

        // The one-shot binding: press runs it, release does not — the `Kind::Edge` behaviour,
        // decided here by the file's missing `runOnUp` rather than by our registry.
        press_key(&mut app, KeyCode::KeyG);
        app.update();
        assert_eq!(lua_count(&app, "PROBE_EDGE"), 1);
        release_key(&mut app, KeyCode::KeyG);
        app.update();
        assert_eq!(
            lua_count(&app, "PROBE_EDGE"),
            1,
            "no runOnUp, no second run — an addon that toggled here would toggle back"
        );

        // And the registry dispatches unchanged beside them: the enum routes, it does not divert.
        press_key(&mut app, KeyCode::KeyW);
        app.update();
        assert!(state(&app).pressed(cmd::MOVE_FORWARD));
        assert_eq!(
            lua_count(&app, "PROBE_DOWN"),
            1,
            "a built-in press is nobody else's"
        );

        // **`keystate` does not outlive the call.** It is absent from the 1.12.1 client's in-world
        // `_G` (`reference/1.12-globals.tsv`) even though its own binding bodies read it, so the
        // reference sets it transiently — as it does `this`/`arg1`. Leaving it set would hand
        // every addon a global the reference lacks, and one that feature-detects `if keystate`
        // would take a branch we cannot honour (decision 1189's "a superset is not free").
        assert_eq!(
            lua_str(&app, "tostring(keystate)"),
            "nil",
            "keystate must be restored after a binding body runs, not left standing in _G"
        );
    }

    /// A `runOnUp` addon latch survives a chat box taking focus, and its release half still runs.
    ///
    /// The typing edge clears [`Kind::Held`] latches (the reference's focus handler releasing the
    /// direction bits) and deliberately leaves [`Kind::EdgeUpDown`] armed, because the up half of
    /// a *pressed* binding is delivered regardless of focus. An addon's `runOnUp` binding is that
    /// same pair wearing one chunk, so it must follow the same rule — dropped on the focus edge,
    /// it would leave whatever its down half started running forever, with no key left to press
    /// to stop it.
    /// **The alt-arrow exemption: you can turn while the chat box has focus.**
    ///
    /// A focused EditBox swallows every key — that is the reference's own handler returning 1 on
    /// every path (`0x77b35e`) and our `UiKeyboardCapture::typing` gate. The four arrow codes are
    /// the single exception: with the box in alt-arrow mode (`ignoreArrows` in XML,
    /// `SetAltArrowKeyMode` in Lua) and ALT not held, the handler returns 0 at `0x77b1c4`, the
    /// strata walk carries the key down to `CGWorldFrame`, and `ExecuteBinding` runs `TURNLEFT`.
    /// The reference's own chat box ships the flag, so this is the default experience.
    ///
    /// benilla read the flag as "consumed but inert, unless Ctrl" until the §5
    /// (`ignorearrows-alt-arrow-gate.md`) corrected both halves — the modifier is ALT, and the key
    /// is not consumed at all. Under the old reading, holding LEFT with the chat box open did
    /// nothing whatever.
    #[test]
    fn a_flagged_editbox_lets_the_arrow_keys_through_to_their_bindings() {
        let mut app = harness();

        // Typing with no exemption: the arrow is swallowed, exactly like every other key.
        app.world_mut().resource_mut::<UiKeyboardCapture>().typing = true;
        press_key(&mut app, KeyCode::ArrowLeft);
        app.update();
        assert!(
            !state(&app).pressed(cmd::TURN_LEFT),
            "an unflagged focused box eats the arrow"
        );
        release_key(&mut app, KeyCode::ArrowLeft);
        app.update();

        // Same key, same focus, exemption armed: the binding fires.
        app.world_mut()
            .resource_mut::<UiKeyboardCapture>()
            .arrows_fall_through = true;
        press_key(&mut app, KeyCode::ArrowLeft);
        app.update();
        assert!(
            state(&app).pressed(cmd::TURN_LEFT),
            "a flagged box declines the arrow, so TURNLEFT runs"
        );

        // And the exemption is FOUR KEYS, not a hole in the typing gate: W is still swallowed.
        press_key(&mut app, KeyCode::KeyW);
        app.update();
        assert!(
            !state(&app).pressed(cmd::MOVE_FORWARD),
            "only the arrows are exempt — every other key a focused box still eats"
        );
    }

    #[test]
    fn a_run_on_up_addon_latch_survives_the_typing_edge_and_still_releases() {
        let mut script = UiScript::new().expect("VM");
        script.register_bindings(&registry_commands());
        script.register_addon_bindings(
            "ProbeAddon",
            &benilla_ui::bindings_xml::parse(PROBE_BINDINGS).expect("well-formed"),
        );
        script.run(r#"SetBinding("J", "PROBEHOLD")"#).expect("bind");
        let mut app = vm_harness(script);

        press_key(&mut app, KeyCode::KeyW);
        press_key(&mut app, KeyCode::KeyJ);
        app.update();
        assert!(state(&app).pressed(cmd::MOVE_FORWARD));
        assert_eq!(lua_count(&app, "PROBE_DOWN"), 1);

        // A box takes focus: movement stops, the addon's latch stays.
        app.world_mut().resource_mut::<UiKeyboardCapture>().typing = true;
        app.update();
        assert!(!state(&app).pressed(cmd::MOVE_FORWARD));
        assert_eq!(
            lua_count(&app, "PROBE_UP"),
            0,
            "the focus edge is not a release — nothing has run the up half yet"
        );

        release_key(&mut app, KeyCode::KeyJ);
        app.update();
        assert_eq!(
            lua_count(&app, "PROBE_UP"),
            1,
            "the up half is delivered even while typing, like every other pressed binding's"
        );
    }

    /// **The armed capture seam, driven by real input events** (B265). The page's own tests call
    /// `KeyBindings_OnHostKey` directly, so nothing asserted that a real notch/press ever
    /// produces that call — and the wheel is the one input that reaches this branch through
    /// neither `KeyboardInput` nor `MouseButtonInput`.
    #[test]
    fn an_armed_capture_takes_a_wheel_notch() {
        let mut script = UiScript::new().expect("VM");
        script.register_bindings(&registry_commands());
        script
            .run(
                r#"CAPTURED = nil
                   function KeyBindings_OnHostKey(chord) CAPTURED = chord end
                   BenillaBindCapture(true)"#,
            )
            .expect("arm");
        let mut app = vm_harness(script);

        app.world_mut().write_message(MouseWheel {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: 1.0,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert_eq!(
            lua_str(&app, "tostring(CAPTURED)"),
            "MOUSEWHEELUP",
            "a wheel notch while armed is a binding key"
        );
        assert!(
            !state(&app).fired(cmd::CAMERA_ZOOM_IN),
            "the armed seam swallows the notch — it must not also zoom"
        );

        // Down, and a modified notch: the canonical prefix order rides the same path.
        app.world_mut().write_message(MouseWheel {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: -1.0,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert_eq!(lua_str(&app, "tostring(CAPTURED)"), "MOUSEWHEELDOWN");
        press_key(&mut app, KeyCode::ShiftLeft);
        app.world_mut().write_message(MouseWheel {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: 1.0,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert_eq!(lua_str(&app, "tostring(CAPTURED)"), "SHIFT-MOUSEWHEELUP");
    }

    /// **The whole wheel-bind path in one harness** (B265): the real Keybindings page, a capsule
    /// armed by a real click, a real notch fed the way winit feeds it, and then the bound chord
    /// dispatching. The page's own tests call `KeyBindings_OnHostKey` by hand and this module's
    /// tests carry no page — between them the join was never asserted.
    #[test]
    fn a_wheel_notch_binds_through_the_real_page_and_then_dispatches() {
        let by_name =
            |n: &str| Cmd(SPECS.iter().position(|s| s.name == n).expect("registered") as u16);
        let mut s = crate::ui_script::keybindings_tests::harness();
        crate::ui_script::keybindings_tests::on_page(&mut s);
        const ROW: &str = "OptionsFrameContainerBodyKeybindingsRow";
        // Expand Movement and arm JUMP's first capsule — JUMP is the classic wheel bind, and one
        // of the 1.12 commands that is NOT `runOnUp`, so the reference accepts the wheel on it.
        s.run(&format!("{ROW}1Header:Click()")).expect("expand");
        s.run(&format!("{ROW}9Key1Button:Click()")).expect("select");
        assert_eq!(
            s.eval::<String>(&format!("return {ROW}9Description:GetText()"))
                .unwrap(),
            "JUMP"
        );
        assert!(s.bind_capture_armed());
        let mut app = vm_harness(s);

        app.world_mut().write_message(MouseWheel {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: 1.0,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        {
            let s = app.world().non_send_resource::<UiScript>();
            assert_eq!(
                s.eval::<String>(r#"return GetBindingAction("MOUSEWHEELUP")"#)
                    .unwrap(),
                "JUMP",
                "a notch on an armed capsule is a bind"
            );
            assert!(!s.bind_capture_armed(), "the completed bind disarms");
        }

        // …and the bound chord now dispatches: the next notch jumps rather than binding.
        app.world_mut().write_message(MouseWheel {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: 1.0,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert!(state(&app).fired(by_name("JUMP")));
        assert!(
            !state(&app).fired(cmd::CAMERA_ZOOM_IN),
            "JUMP stole the wheel from the camera, the 1.12 steal law"
        );
    }

    /// **A notch is a press AND its release** — the reference dispatches the one chord twice,
    /// `isDown=1` then `isDown=0` (`0x483d6f` / `0x483d82`). So a `runOnUp` binding on the wheel
    /// runs both halves in the same frame; before this it ran neither, and the binding was dead.
    ///
    /// Seeded through the stored set rather than `SetBinding`, because the table refuses to put a
    /// wheel chord on a press+release command — this is the "hand-edited file smuggles one in"
    /// case, which is exactly the state the old arm silently dropped on the floor.
    #[test]
    fn a_wheel_notch_runs_both_halves_of_a_press_and_release_binding() {
        let mut script = UiScript::new().expect("VM");
        script.register_bindings(&registry_commands());
        script.register_addon_bindings(
            "ProbeAddon",
            &benilla_ui::bindings_xml::parse(PROBE_BINDINGS).expect("well-formed"),
        );
        script.seed_binding_set(
            1,
            Some(vec![(
                "PROBEHOLD".to_string(),
                vec!["MOUSEWHEELUP".to_string()],
            )]),
        );
        script.load_binding_set(1);
        let mut app = vm_harness(script);

        app.world_mut().write_message(MouseWheel {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: 1.0,
            window: Entity::PLACEHOLDER,
        });
        app.update();
        assert_eq!(lua_count(&app, "PROBE_DOWN"), 1, "the notch's press half");
        assert_eq!(
            lua_count(&app, "PROBE_UP"),
            1,
            "…and its release half, in the same frame — a notch has no key left to lift"
        );
        assert_eq!(lua_str(&app, "PROBE_LAST"), "up");
        // Nothing is left latched: a wheel latch that outlived its notch would hold the down
        // state forever, with no key to press to end it.
        assert!(app.world().resource::<BindingsState>().latched.is_empty());
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

    /// **The two-probe lookup, table-wide** (decision 1142): the exact chord, then one retry with
    /// the leftmost modifier dropped, and never a third. The bug that bought this test was a
    /// modifier held over a movement key eating the movement — so the movement case leads.
    #[test]
    fn a_press_probes_its_chord_then_falls_back_once() {
        // The reported bug: Shift held, W pressed. `SHIFT-W` is nobody's, so the retry drops
        // SHIFT and MOVEFORWARD latches — the reference's `strchr` step, `0x4b7990`.
        let mut app = harness();
        press_key(&mut app, KeyCode::ShiftLeft);
        press_key(&mut app, KeyCode::KeyW);
        app.update();
        assert!(
            state(&app).pressed(cmd::MOVE_FORWARD),
            "SHIFT-W falls back to W — the whole point of 1142"
        );
        // And it unlatches on the base key with the modifier still down (the reference replays
        // the press-time chord at key-up, `0x483bd0`; we latch the resolved command instead —
        // same observable).
        release_key(&mut app, KeyCode::KeyW);
        app.update();
        assert!(!state(&app).pressed(cmd::MOVE_FORWARD));
        // Bare Z is the sheath toggle; ALT-Z is TOGGLEUI. The exact probe runs first, so the
        // specific chord wins outright — a fallback never overrides a real entry.
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
        // CTRL-ALT-Z still fires nothing — but for the real reason, not 0585's. The single strip
        // drops the LEFTMOST modifier (`ALT-CTRL-Z` → `CTRL-Z`, unbound) and stops: it never
        // reaches ALT-Z, and never reaches bare Z. This is the assertion that pins "one retry".
        release_key(&mut app, KeyCode::KeyZ);
        press_key(&mut app, KeyCode::ControlLeft);
        press_key(&mut app, KeyCode::KeyZ);
        app.update();
        assert!(
            !state(&app).fired(cmd::TOGGLE_UI) && !state(&app).fired(cmd::TOGGLE_SHEATH),
            "ALT-CTRL-Z probes CTRL-Z and stops — no second strip to ALT-Z or Z"
        );
        // TAB vs SHIFT-TAB: both bound, so both resolve exactly and neither borrows the other.
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
        // Super/Cmd is never a binding modifier: a super-held press builds no chord at all, so it
        // has no fallback either.
        let mut app = harness();
        press_key(&mut app, KeyCode::SuperLeft);
        press_key(&mut app, KeyCode::KeyZ);
        app.update();
        assert!(!state(&app).fired(cmd::TOGGLE_SHEATH));
    }

    /// **The dev plane spends the keyboard's fallback probe, and nothing else** (1142). 0870 chose
    /// `Ctrl`+`Shift` for the overlays because the plane looked empty under 0585's exact-match
    /// law; under the real law it is *nearly* empty — the single strip lands on `SHIFT-`*key*, not
    /// the bare letter — and `SHIFT-P` is exactly the entry that makes "nearly" insufficient.
    ///
    /// Asserted on [`BindingDispatch::resolve`] rather than through the event harness because the
    /// colliding command is `Kind::Edge` — its whole effect is a Lua body, and the harness has no
    /// VM, so an end-to-end assertion here could only ever be vacuously true.
    #[test]
    fn the_dev_plane_keeps_its_letters_without_stealing_bound_chords() {
        let by_name =
            |n: &str| Cmd(SPECS.iter().position(|s| s.name == n).expect("registered") as u16);
        let pet_paper_doll = by_name("TOGGLECHARACTER3"); // SHIFT-P, decision 1057
        let mut dispatch = BindingDispatch::test_defaults();
        let plane_p = Chord::parse("CTRL-SHIFT-P").expect("parses");
        // Ctrl+Shift+P is the perf HUD's. Off the plane it would fall back onto SHIFT-P — which
        // is 0585's original "one key did two things" bug, reborn one strip further along...
        assert_eq!(
            dispatch.resolve(plane_p, false),
            Some(Bound::Spec(pet_paper_doll)),
            "without the plane rule the retry does reach SHIFT-P — this is what is being blocked"
        );
        // ...so on the plane, the retry is what it costs.
        assert_eq!(dispatch.resolve(plane_p, true), None);
        // The suppression is the FALLBACK only — an exact CTRL-SHIFT- entry still resolves. No
        // shipped default proves it (1.12's own two, CTRL-SHIFT-TAB and CTRL-SHIFT-PAGEDOWN,
        // belong to commands the honest tree doesn't carry yet), so the entry is made here: this
        // is the case a player creates the moment they bind anything on the plane.
        dispatch.map.insert(plane_p, Bound::Spec(pet_paper_doll));
        assert_eq!(
            dispatch.resolve(plane_p, true),
            Some(Bound::Spec(pet_paper_doll)),
            "the plane spends the retry, never the exact probe"
        );
        // And SHIFT-P itself is still the pet paper doll: the plane costs nothing outside itself.
        let shift_p = Chord::parse("SHIFT-P").expect("parses");
        assert_eq!(
            dispatch.resolve(shift_p, false),
            Some(Bound::Spec(pet_paper_doll))
        );
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
        // PetActionButtonUp → CastPetAction in the app's VM), Ctrl still held.
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
        app.world_mut().resource_mut::<UiKeyboardCapture>().typing = true;
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
        app.world_mut().resource_mut::<UiKeyboardCapture>().typing = false;
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

    /// **B opens the backpack, SHIFT-B opens all of them** — the director's report, pinned end to
    /// end (1494): a real keyboard event, through the chord table and the registry's shipped
    /// defaults, into the binding body, into the Lua that shows the windows.
    ///
    /// This is the seam the bug lived in and nothing else covers it. `bag_tests` drive the Lua
    /// globals directly, so they could not see that BOTH chords were registered on one command;
    /// the registry tests read the table, so they could not see what the bodies do. Only pressing
    /// the key joins the two.
    ///
    /// **The windows on the far end are the reference's own now** (1751). `BenillaBagFrame`/
    /// `BenillaBagFrame2` were five static frames our `BagFrame.xml` declared, so this test could
    /// name them and load six files by hand; the live windows are `ContainerFrame1..12`, generated
    /// on demand off the player's chain and RECYCLED across containers, so the question is
    /// `IsBagOpen(id)` and never a frame name. Two consequences, both deliberate:
    ///   * the interface is loaded with the app's own [`crate::ui_script::load_default_ui`] rather
    ///     than a hand-listed six, because the chain half of the manifest is what makes the bag
    ///     windows exist at all and a private six-file reader cannot express it — which also makes
    ///     this test the load order the player gets;
    ///   * it needs client data, like every test that touches a chain entry.
    #[test]
    fn b_opens_the_backpack_and_shift_b_opens_every_bag() {
        let _data = benilla_formats::wow_data_or_skip!();
        let mut script = UiScript::new().expect("VM");
        script.register_bindings(&registry_commands());
        script.set_screen_size(1024.0, 768.0);
        // The in-game UI materializes on world entry (1051), so a player always exists by the time
        // the manifest loads — and the stock macro window's character tab formats
        // `UnitName("player")` into its label in its own OnLoad (decision 1848).
        script.set_unit(
            "player",
            Some(benilla_ui::script::UnitState {
                exists: true,
                name: Some("Probefour".into()),
                level: 60,
                ..Default::default()
            }),
        );
        let failures = crate::ui_script::load_default_ui(&script);
        assert!(
            failures.is_empty(),
            "default UI failed to load: {failures:?}"
        );
        script.set_money(0);
        // The backpack, plus one equipped bag in slot 2 — so "all bags" is two windows and
        // "the backpack alone" is visibly different from it. Fed BEFORE any key: the reference
        // builds a window only for a container that exists (`OpenBag`'s `size > 0` gate), where
        // our own `BenillaBagFrame*` were static frames that showed regardless.
        script.set_container(
            0,
            Some(benilla_ui::script::ContainerState {
                name: Some("Backpack".into()),
                num_slots: 16,
                slots: std::collections::HashMap::new(),
            }),
        );
        script.set_container(
            2,
            Some(benilla_ui::script::ContainerState {
                name: Some("Small Pouch".into()),
                num_slots: 6,
                slots: std::collections::HashMap::new(),
            }),
        );
        let mut app = vm_harness(script);
        // `IsBagOpen(id)` — the reference's own scan over `ContainerFrame1..12`, and the only
        // honest way to ask: which window a bag lands in depends on what else is open.
        let open = |app: &App, id: i64| {
            app.world()
                .non_send_resource::<UiScript>()
                .eval::<bool>(&format!("return IsBagOpen({id}) ~= nil"))
                .expect("eval")
        };

        // B — TOGGLEBACKPACK's shipped default. The backpack, and nothing else.
        press_key(&mut app, KeyCode::KeyB);
        app.update();
        release_key(&mut app, KeyCode::KeyB);
        app.update();
        assert!(open(&app, 0), "B opens the backpack");
        assert!(
            !open(&app, 2),
            "B does NOT open the equipped bag — this is the bug 1494 fixes"
        );

        // B again: bag 0 is open, so this is the close arm.
        press_key(&mut app, KeyCode::KeyB);
        app.update();
        release_key(&mut app, KeyCode::KeyB);
        app.update();
        assert!(!open(&app, 0), "B again shuts it");

        // SHIFT-B — OPENALLBAGS' shipped default, a DIFFERENT command with a different body.
        press_key(&mut app, KeyCode::ShiftLeft);
        press_key(&mut app, KeyCode::KeyB);
        app.update();
        release_key(&mut app, KeyCode::KeyB);
        app.update();
        assert!(open(&app, 0) && open(&app, 2), "SHIFT-B opens every bag");

        // …and closes them again, the count now reading all-open.
        press_key(&mut app, KeyCode::KeyB);
        app.update();
        release_key(&mut app, KeyCode::KeyB);
        release_key(&mut app, KeyCode::ShiftLeft);
        app.update();
        assert!(
            !open(&app, 0) && !open(&app, 2),
            "SHIFT-B again shuts them all"
        );
    }

    // ── The synthetic seam (`BindingsState::synth_*`, the web bridge's door) ──

    fn state_mut(app: &mut App) -> Mut<'_, BindingsState> {
        app.world_mut().resource_mut::<BindingsState>()
    }

    #[test]
    fn a_synthetic_hold_is_a_latch_the_sweep_never_releases() {
        let mut app = harness();
        app.update();
        state_mut(&mut app).synth_hold(cmd::MOVE_FORWARD);
        assert!(state(&app).pressed(cmd::MOVE_FORWARD));
        assert!(
            state(&app).just_pressed(cmd::MOVE_FORWARD),
            "the first assertion is the press edge"
        );
        // Idempotent within the frame: a second assert neither double-latches nor re-edges.
        state_mut(&mut app).synth_hold(cmd::MOVE_FORWARD);
        assert_eq!(
            state(&app)
                .latched
                .iter()
                .filter(|&&(k, _)| k == BindKey::Synth(cmd::MOVE_FORWARD))
                .count(),
            1
        );
        // A frame later the dispatch has swept every key that reads up — and left this one,
        // which has no key to read.
        app.update();
        assert!(state(&app).pressed(cmd::MOVE_FORWARD));
        assert!(!state(&app).just_pressed(cmd::MOVE_FORWARD));
        state_mut(&mut app).synth_release(cmd::MOVE_FORWARD);
        assert!(!state(&app).pressed(cmd::MOVE_FORWARD));
    }

    #[test]
    fn a_synthetic_fire_is_a_host_edge_with_an_amount() {
        let mut app = harness();
        app.update();
        state_mut(&mut app).synth_fire(cmd::CAMERA_ZOOM_IN, 2.5);
        assert!(state(&app).fired(cmd::CAMERA_ZOOM_IN));
        assert!(state(&app).just_pressed(cmd::CAMERA_ZOOM_IN));
        assert_eq!(state(&app).amount(cmd::CAMERA_ZOOM_IN), 2.5);
        app.update();
        assert!(
            !state(&app).fired(cmd::CAMERA_ZOOM_IN),
            "an edge lasts one frame"
        );
        assert_eq!(state(&app).amount(cmd::CAMERA_ZOOM_IN), 0.0);
    }

    #[test]
    fn the_typing_edge_drops_a_synthetic_held_like_a_keys() {
        let mut app = harness();
        app.update();
        state_mut(&mut app).synth_hold(cmd::MOVE_FORWARD);
        app.world_mut().resource_mut::<UiKeyboardCapture>().typing = true;
        app.update();
        assert!(
            !state(&app).pressed(cmd::MOVE_FORWARD),
            "a box taking focus stops movement, synthetic or not"
        );
        app.world_mut().resource_mut::<UiKeyboardCapture>().typing = false;
        app.update();
        assert!(
            !state(&app).pressed(cmd::MOVE_FORWARD),
            "and it stays stopped until re-asserted"
        );
        state_mut(&mut app).synth_hold(cmd::MOVE_FORWARD);
        assert!(state(&app).pressed(cmd::MOVE_FORWARD));
    }

    #[test]
    fn a_synthetic_assert_of_the_wrong_kind_is_a_no_op() {
        let mut app = harness();
        app.update();
        state_mut(&mut app).synth_hold(cmd::JUMP);
        assert!(
            !state(&app).pressed(cmd::JUMP),
            "JUMP is a host edge, not a hold"
        );
        state_mut(&mut app).synth_fire(cmd::MOVE_FORWARD, 1.0);
        assert!(
            !state(&app).fired(cmd::MOVE_FORWARD),
            "MOVEFORWARD is a hold, not a fire"
        );
        assert!(state(&app).just.is_empty() && state(&app).latched.is_empty());
    }

    #[test]
    fn a_key_release_never_touches_a_synthetic_latch_on_the_same_command() {
        let mut app = harness();
        app.update();
        state_mut(&mut app).synth_hold(cmd::MOVE_FORWARD);
        press_key(&mut app, KeyCode::KeyW);
        app.update();
        release_key(&mut app, KeyCode::KeyW);
        app.update();
        assert!(
            state(&app).pressed(cmd::MOVE_FORWARD),
            "W's latch came and went; the bridge's is still there"
        );
    }
}
