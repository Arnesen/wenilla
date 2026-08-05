//! The **key-binding table** — the client's chord→command store, engine side (decision 0997).
//!
//! In the reference every rebindable action is a *binding*: a command name (`MOVEFORWARD`)
//! carrying a category header and up to two bound key chords, stored key→command and saved to
//! `bindings-cache.wtf`. 1.12 exposes the whole system to Lua (`Blizzard_BindingUI` is plain
//! FrameXML over it): `GetNumBindings`/`GetBinding(i)`, `SetBinding(key[, command])` with its
//! steal-the-key law, `GetBindingKey`/`GetBindingAction`, and the three-set model —
//! `LoadBindings(0 default | 1 account | 2 character)`, `SaveBindings(which)`,
//! `GetCurrentBindingSet()`.
//!
//! benilla keeps the same shape at the same seam, split like the CVar table (0954): the **host
//! registers** the commands it actually implements (honest-tree — no row without a real engine
//! action) with their 1.12 default chords, **this table is the string-domain truth** the window's
//! Lua reads and writes synchronously, and the app derives its dispatch view (parsed chords →
//! latched commands) whenever [`super::UiScript::keybinds_generation`] moves, persisting on the
//! queued [`KeybindRequest::Save`]. Chord strings are the reference's own canon —
//! `[ALT-][CTRL-][SHIFT-]<TOKEN>` (`W`, `SPACE`, `NUMPAD0`, `BUTTON4`, `MOUSEWHEELUP`) — matched
//! by string equality exactly as the client does (decision 0585's law, now engine-wide).
//!
//! The stored account/character sets live here too (seeded by the app from
//! `benilla/bindings/…`, re-snapshotted on save), so `LoadBindings` is **synchronous** like the
//! reference's — the window calls it and repaints in the same tick, no host round-trip.

use std::collections::HashMap;

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// One host-registered command: the 1.12 name, its category header's global-string key
/// (`BINDING_HEADER_MOVEMENT`), whether it has press+release semantics (`runOnUp` — the
/// mousewheel-refusal law rides this), and the 1.12 default chords.
#[derive(Clone, Copy, Debug)]
pub struct KeybindCommand {
    pub name: &'static str,
    pub category: &'static str,
    pub run_on_up: bool,
    pub default1: Option<&'static str>,
    pub default2: Option<&'static str>,
}

/// A host request queued by Lua: persist the live table as set `1`/`2` (`Save`), on which the
/// app writes `benilla/bindings/…` — and, for `Save(1)` issued while the character set was
/// active, deletes the character file (the reference's confirmed delete-on-switch).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeybindRequest {
    Save(u32),
}

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    category: String,
    run_on_up: bool,
    defaults: [Option<String>; 2],
    /// The live bound chords, in bind order (the reference's per-command key list; the first two
    /// are what `GetBindingKey` and the window's Key 1/Key 2 columns show).
    keys: Vec<String>,
}

/// The table + the stored sets + the Lua→host seams. Lives in [`Model`].
#[derive(Default)]
pub(crate) struct KeybindState {
    entries: Vec<Entry>,
    by_name: HashMap<String, usize>,
    /// Saved snapshots: `stored[0]` = account (set 1), `stored[1]` = character (set 2, `None`
    /// when no character-specific bindings exist). Each is per-entry key lists, entry order.
    stored: [Option<Vec<Vec<String>>>; 2],
    /// `1` account, `2` character — which set the live table edits (`GetCurrentBindingSet`).
    current_set: u32,
    /// Bumped by every live-table mutation and every seed — the app's re-derive-dispatch signal.
    generation: u64,
    requests: Vec<KeybindRequest>,
    /// The Keybindings page's capture arm (`BenillaBindCapture(armed)`): while set, the host
    /// swallows raw input and calls the page back with the canonical chord string.
    capture_armed: bool,
}

impl KeybindState {
    /// Strip the reference's modifier prefixes; what remains is the base token (which may itself
    /// be `-` — `CTRL--` is Ctrl+minus, so the strip walks prefixes, never splits on `-`).
    fn base_token(chord: &str) -> &str {
        let mut rest = chord;
        loop {
            let Some(next) = ["ALT-", "CTRL-", "SHIFT-"]
                .iter()
                .find_map(|p| rest.strip_prefix(p))
            else {
                return rest;
            };
            rest = next;
        }
    }

    /// The reference's `SetBinding(key[, command])`: unbind `key` wherever it is, then (with a
    /// command) append it to that command's key list. Returns `false` for the one refusal the
    /// reference has — a mousewheel chord on a press+release (`runOnUp`) command
    /// (`KEYBINDINGFRAME_MOUSEWHEEL_ERROR`: a wheel tick cannot be released).
    fn set_binding(&mut self, key: &str, command: Option<&str>) -> bool {
        let key = key.to_ascii_uppercase();
        let cmd_idx = match command {
            Some(name) => match self.by_name.get(&name.to_ascii_uppercase()) {
                Some(&i) => Some(i),
                None => return false, // unknown command — refuse, the window never offers one
            },
            None => None,
        };
        if let Some(i) = cmd_idx {
            let wheel = matches!(Self::base_token(&key), "MOUSEWHEELUP" | "MOUSEWHEELDOWN");
            if wheel && self.entries[i].run_on_up {
                return false;
            }
        }
        for e in &mut self.entries {
            e.keys.retain(|k| k != &key);
        }
        if let Some(i) = cmd_idx {
            self.entries[i].keys.push(key);
        }
        self.generation += 1;
        true
    }

    /// Snapshot the live key lists (entry order) — the stored-set payload.
    fn snapshot(&self) -> Vec<Vec<String>> {
        self.entries.iter().map(|e| e.keys.clone()).collect()
    }

    /// Replace the live key lists from a snapshot (entry-count mismatch pads/truncates — a
    /// stored set from before a command was registered simply leaves the new command on its
    /// live value).
    fn apply(&mut self, snap: &[Vec<String>]) {
        for (i, e) in self.entries.iter_mut().enumerate() {
            if let Some(keys) = snap.get(i) {
                e.keys = keys.clone();
            }
        }
        self.generation += 1;
    }

    fn defaults_snapshot(&self) -> Vec<Vec<String>> {
        self.entries
            .iter()
            .map(|e| e.defaults.iter().flatten().cloned().collect())
            .collect()
    }

    /// The reference's `LoadBindings(set)`: 0 = defaults, 1 = account, 2 = character (falling
    /// back to the account snapshot when no character set exists — the reference's character
    /// set starts as a copy). Loading 1/2 also moves `current_set`; loading defaults does not
    /// (you are still *editing* the set you had, as in the window's Reset To Default).
    fn load(&mut self, set: u32) {
        match set {
            0 => {
                let d = self.defaults_snapshot();
                self.apply(&d);
            }
            1 => {
                let s = self.stored[0]
                    .clone()
                    .unwrap_or_else(|| self.defaults_snapshot());
                self.apply(&s);
                self.current_set = 1;
            }
            2 => {
                let s = self.stored[1]
                    .clone()
                    .or_else(|| self.stored[0].clone())
                    .unwrap_or_else(|| self.defaults_snapshot());
                self.apply(&s);
                self.current_set = 2;
            }
            _ => {}
        }
    }

    /// The reference's `SaveBindings(which)`: snapshot live into set `which`, make it current,
    /// and — saving account while character bindings exist — drop the character set (the
    /// window's confirmed permanent delete). Queues the host persist.
    fn save(&mut self, which: u32) {
        if which != 1 && which != 2 {
            return;
        }
        self.stored[(which - 1) as usize] = Some(self.snapshot());
        if which == 1 {
            self.stored[1] = None;
        }
        self.current_set = which;
        self.requests.push(KeybindRequest::Save(which));
    }
}

impl super::UiScript {
    /// Register the host-implemented commands (boot-time, order = the 1.12 `Bindings.xml`
    /// order). Idempotent per name; live keys seed from the defaults.
    pub fn register_bindings(&mut self, commands: &[KeybindCommand]) {
        let mut model = self.model_mut();
        for c in commands {
            let key = c.name.to_ascii_uppercase();
            if model.keybinds.by_name.contains_key(&key) {
                continue;
            }
            let defaults = [c.default1.map(str::to_owned), c.default2.map(str::to_owned)];
            let idx = model.keybinds.entries.len();
            model.keybinds.by_name.insert(key, idx);
            model.keybinds.entries.push(Entry {
                name: c.name.to_owned(),
                category: c.category.to_owned(),
                run_on_up: c.run_on_up,
                keys: defaults.iter().flatten().cloned().collect(),
                defaults,
            });
        }
        model.keybinds.current_set = 1;
        model.keybinds.generation += 1;
    }

    /// Host-side seed of a stored set (`1` account / `2` character) — the loaded
    /// `benilla/bindings/…` state, already resolved to full per-command key lists by the app's
    /// diff layer. Passing the character set marks it existing (the window's checkbox state);
    /// `seed_binding_set(2, None)` clears it. Does not touch the live table — call
    /// [`Self::load_binding_set`] after seeding to activate one.
    pub fn seed_binding_set(&mut self, set: u32, keys: Option<Vec<(String, Vec<String>)>>) {
        let mut model = self.model_mut();
        let kb = &mut model.keybinds;
        let snap = keys.map(|pairs| {
            let by_name: HashMap<_, _> = pairs.into_iter().collect();
            kb.entries
                .iter()
                .map(|e| {
                    by_name
                        .get(&e.name)
                        .cloned()
                        .unwrap_or_else(|| e.keys.clone())
                })
                .collect()
        });
        match set {
            1 => kb.stored[0] = snap,
            2 => kb.stored[1] = snap,
            _ => {}
        }
    }

    /// Host-side `LoadBindings` (world entry: character set if it exists, else account).
    pub fn load_binding_set(&mut self, set: u32) {
        self.model_mut().keybinds.load(set);
    }

    /// The live table as `(command, bound chords)` in registration order — the app's dispatch
    /// derivation and the save diff read this.
    pub fn keybind_snapshot(&self) -> Vec<(String, Vec<String>)> {
        self.model_mut()
            .keybinds
            .entries
            .iter()
            .map(|e| (e.name.clone(), e.keys.clone()))
            .collect()
    }

    /// Bumped by every table mutation (Lua or host) — cheap to poll; re-derive dispatch when it
    /// moves (the macros-generation pattern).
    pub fn keybinds_generation(&self) -> u64 {
        self.model_mut().keybinds.generation
    }

    /// Drain the queued host requests (persist-on-save).
    pub fn take_keybind_requests(&mut self) -> Vec<KeybindRequest> {
        std::mem::take(&mut self.model_mut().keybinds.requests)
    }

    /// Which set the live table edits: 1 account, 2 character.
    pub fn current_binding_set(&self) -> u32 {
        self.model_mut().keybinds.current_set
    }

    /// Whether a character-specific stored set exists (the checkbox's persisted truth).
    pub fn character_bindings_exist(&self) -> bool {
        self.model_mut().keybinds.stored[1].is_some()
    }

    /// The Keybindings page's capture arm — while true, the host swallows raw input and
    /// calls `KeyBindings_OnHostKey("<chord>")` instead of dispatching it.
    pub fn bind_capture_armed(&self) -> bool {
        self.model_mut().keybinds.capture_armed
    }
}

/// Register the binding globals (1.12 names; `GetBinding` returns the era 4-tuple —
/// command, category token, key1, key2 — the categorized window's shape).
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    lua.globals().set(
        "GetNumBindings",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_mut::<Model>().expect("model app_data");
            Ok(model.keybinds.entries.len())
        })?,
    )?;
    lua.globals().set(
        "GetBinding",
        lua.create_function(|lua, i: usize| {
            let model = lua.app_data_mut::<Model>().expect("model app_data");
            let Some(e) = i.checked_sub(1).and_then(|i| model.keybinds.entries.get(i)) else {
                return Ok(MultiValue::new());
            };
            let mut out = vec![
                Value::String(lua.create_string(&e.name)?),
                Value::String(lua.create_string(&e.category)?),
            ];
            for k in e.keys.iter().take(2) {
                out.push(Value::String(lua.create_string(k)?));
            }
            Ok(MultiValue::from_iter(out))
        })?,
    )?;
    lua.globals().set(
        "SetBinding",
        lua.create_function(|lua, (key, command): (String, Option<String>)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let ok = model.keybinds.set_binding(&key, command.as_deref());
            Ok(if ok { Some(1) } else { None })
        })?,
    )?;
    lua.globals().set(
        "GetBindingKey",
        lua.create_function(|lua, command: String| {
            let model = lua.app_data_mut::<Model>().expect("model app_data");
            let kb = &model.keybinds;
            let mut out = Vec::new();
            if let Some(&i) = kb.by_name.get(&command.to_ascii_uppercase()) {
                for k in kb.entries[i].keys.iter().take(2) {
                    out.push(Value::String(lua.create_string(k)?));
                }
            }
            Ok(MultiValue::from_iter(out))
        })?,
    )?;
    lua.globals().set(
        "GetBindingAction",
        lua.create_function(|lua, key: String| {
            let model = lua.app_data_mut::<Model>().expect("model app_data");
            let key = key.to_ascii_uppercase();
            let name = model
                .keybinds
                .entries
                .iter()
                .find(|e| e.keys.iter().any(|k| k == &key))
                .map(|e| e.name.as_str())
                .unwrap_or("");
            lua.create_string(name)
        })?,
    )?;
    lua.globals().set(
        "LoadBindings",
        lua.create_function(|lua, set: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.keybinds.load(set);
            Ok(())
        })?,
    )?;
    lua.globals().set(
        "SaveBindings",
        lua.create_function(|lua, which: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.keybinds.save(which);
            Ok(())
        })?,
    )?;
    lua.globals().set(
        "GetCurrentBindingSet",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_mut::<Model>().expect("model app_data");
            Ok(model.keybinds.current_set)
        })?,
    )?;
    lua.globals().set(
        "BenillaCharacterBindingsExist",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_mut::<Model>().expect("model app_data");
            Ok(model.keybinds.stored[1].is_some())
        })?,
    )?;
    lua.globals().set(
        "BenillaBindCapture",
        lua.create_function(|lua, armed: Option<bool>| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.keybinds.capture_armed = armed.unwrap_or(false);
            Ok(())
        })?,
    )
}

#[cfg(test)]
mod tests {
    use super::KeybindCommand;
    use crate::script::keybind::KeybindRequest;
    use crate::script::UiScript;

    fn script() -> UiScript {
        let mut s = UiScript::new().unwrap();
        s.register_bindings(&[
            KeybindCommand {
                name: "MOVEFORWARD",
                category: "BINDING_HEADER_MOVEMENT",
                run_on_up: true,
                default1: Some("W"),
                default2: Some("UP"),
            },
            KeybindCommand {
                name: "JUMP",
                category: "BINDING_HEADER_MOVEMENT",
                run_on_up: false,
                default1: Some("SPACE"),
                default2: Some("NUMPAD0"),
            },
            KeybindCommand {
                name: "CAMERAZOOMIN",
                category: "BINDING_HEADER_CAMERA",
                run_on_up: false,
                default1: Some("MOUSEWHEELUP"),
                default2: None,
            },
        ]);
        s
    }

    #[test]
    fn the_table_reads_like_the_reference() {
        let s = script();
        assert_eq!(s.eval::<usize>("return GetNumBindings()").unwrap(), 3);
        // Era 4-tuple: command, category token, key1, key2 — defaults seeded live.
        assert!(s
            .eval::<bool>(
                r#"local c, cat, k1, k2 = GetBinding(1)
                   return c == "MOVEFORWARD" and cat == "BINDING_HEADER_MOVEMENT"
                      and k1 == "W" and k2 == "UP""#
            )
            .unwrap());
        assert_eq!(
            s.eval::<String>(r#"return GetBindingAction("NUMPAD0")"#)
                .unwrap(),
            "JUMP"
        );
        assert_eq!(
            s.eval::<String>(r#"return GetBindingAction("F")"#).unwrap(),
            "",
            "an unbound key reads as the empty command, like the client"
        );
    }

    #[test]
    fn set_binding_steals_and_the_wheel_refusal_holds() {
        let s = script();
        let g0 = s.keybinds_generation();
        // Bind W to JUMP: stolen from MOVEFORWARD (its slot 1 empties, UP slides up), appended
        // to JUMP's list after its defaults.
        assert!(s
            .eval::<bool>(r#"return SetBinding("W", "JUMP") == 1"#)
            .unwrap());
        assert!(s
            .eval::<bool>(
                r#"local k1, k2 = GetBindingKey("MOVEFORWARD"); return k1 == "UP" and k2 == nil"#
            )
            .unwrap());
        assert_eq!(
            s.eval::<String>(r#"return GetBindingAction("W")"#).unwrap(),
            "JUMP"
        );
        // Unbind by key: SetBinding(key) with no command.
        s.run(r#"SetBinding("SPACE")"#).unwrap();
        assert_eq!(
            s.eval::<String>(r#"return GetBindingAction("SPACE")"#)
                .unwrap(),
            ""
        );
        // The one refusal: a wheel chord on a press+release command (1.12's
        // KEYBINDINGFRAME_MOUSEWHEEL_ERROR); a wheel on a click command binds fine.
        assert!(s
            .eval::<bool>(r#"return SetBinding("SHIFT-MOUSEWHEELUP", "MOVEFORWARD") == nil"#)
            .unwrap());
        assert!(s
            .eval::<bool>(r#"return SetBinding("SHIFT-MOUSEWHEELUP", "CAMERAZOOMIN") == 1"#)
            .unwrap());
        assert!(
            s.keybinds_generation() > g0,
            "mutations bump the generation"
        );
    }

    #[test]
    fn the_three_set_model_loads_saves_and_deletes_like_the_reference() {
        let mut s = script();
        assert_eq!(s.current_binding_set(), 1);
        assert!(!s.character_bindings_exist());
        // Rebind, save as character: stored set 2 exists, current set moves.
        s.run(r#"SetBinding("F", "JUMP")"#).unwrap();
        s.run("SaveBindings(2)").unwrap();
        assert_eq!(s.current_binding_set(), 2);
        assert!(s.character_bindings_exist());
        assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(2)]);
        // Reset To Default: LoadBindings(0) restores defaults but stays on the character set.
        s.run("LoadBindings(0)").unwrap();
        assert_eq!(
            s.eval::<String>(r#"return GetBindingAction("F")"#).unwrap(),
            ""
        );
        assert_eq!(s.current_binding_set(), 2);
        // Cancel's revert: LoadBindings(current) restores the saved character set.
        s.run("LoadBindings(2)").unwrap();
        assert_eq!(
            s.eval::<String>(r#"return GetBindingAction("F")"#).unwrap(),
            "JUMP"
        );
        // Okay back to general: SaveBindings(1) drops the character set — the confirmed
        // permanent delete — and the app is asked to persist (and unlink the character file).
        s.run("SaveBindings(1)").unwrap();
        assert_eq!(s.current_binding_set(), 1);
        assert!(!s.character_bindings_exist());
        assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(1)]);
    }

    #[test]
    fn host_seeding_feeds_load_and_the_capture_arm_reads_back() {
        let mut s = script();
        s.seed_binding_set(1, Some(vec![("JUMP".into(), vec!["F".into()])]));
        s.load_binding_set(1);
        assert_eq!(
            s.eval::<String>(r#"return GetBindingAction("F")"#).unwrap(),
            "JUMP"
        );
        // Commands absent from the seed keep their live keys (a stored set from before a
        // command was registered leaves the new command alone).
        assert_eq!(
            s.eval::<String>(r#"return GetBindingAction("W")"#).unwrap(),
            "MOVEFORWARD"
        );
        assert!(!s.bind_capture_armed());
        s.run("BenillaBindCapture(true)").unwrap();
        assert!(s.bind_capture_armed());
        s.run("BenillaBindCapture(false)").unwrap();
        assert!(!s.bind_capture_armed());
        let snap = s.keybind_snapshot();
        assert_eq!(snap[1].0, "JUMP");
        assert_eq!(snap[1].1, vec!["F".to_string()]);
    }
}
