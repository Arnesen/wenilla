//! The **CVar table** — the client's named-console-variable store, engine side (decision 0954).
//!
//! In the reference every player setting is a CVar: a case-insensitively named string value with
//! a registered default, read and written from Lua through `GetCVar`/`SetCVar`/`GetCVarDefault`
//! (the 1.12 options panels are thin UI over this table). benilla keeps the same shape at the
//! same seam: the **host registers** the vars it implements
//! ([`super::UiScript::register_cvars`] — most backed by an engine knob, a few consumed by Lua
//! alone), Lua reads/writes them synchronously, and each Lua write lands on the **change queue** the host
//! drains per frame ([`super::UiScript::take_cvar_changes`]) to sync its resources and mark the
//! config file dirty. Host-side writes ([`super::UiScript::set_cvar_host`] — the loaded config,
//! an env override) do NOT ride the queue: the host already knows, and an echo would re-dirty
//! the file it just loaded.
//!
//! Values are **strings**, like the client's (`GetCVar` returns a string; consumers parse and
//! clamp at their own edge). An unknown name warns once and no-ops — a benilla CVar is either
//! host-registered or addon-declared through `RegisterCVar` (decision 1195), so an unknown key
//! is a typo or an unshipped feature, never a storage slot.
//!
//! **The table dies with the VM, so persistence bridges it** (decision 1291): in the reference
//! this store is engine memory and survives every `ReloadUI`; ours is per-VM state, replaced at
//! every login and reload (1290/1291). The host hands each fresh VM the config file's values
//! ([`super::UiScript::set_cvar_saved_base`]) before anything registers, and registration —
//! either kind — starts a key at its saved value. The host folds the dying VM's table back into
//! its persist state on the session edge, so the two halves meet.

use mlua::{Lua, Value};

use super::{Model, ScriptValue};

/// One registered CVar: the host's spelling (for change events and the config file), the live
/// value, and the registered default (`GetCVarDefault`, and the saver's "only write what moved").
#[derive(Clone, Debug)]
pub(crate) struct CvarSlot {
    /// The registered spelling (`MasterVolume`), reported on change events and in snapshots;
    /// the table key is the lowercase form (the client's lookups are case-insensitive).
    pub name: String,
    pub value: String,
    pub default: String,
}

impl super::UiScript {
    /// Hand registration the config file's persisted values (decision 1291): name → value, keys
    /// lowercased here. Set **before** any `register_cvars` / addon `RegisterCVar` runs in this
    /// VM; a name registered while present here starts at the saved value instead of its default.
    ///
    /// This is the bridge that makes the per-VM table behave like the reference's engine-side
    /// one (where the store outlives every `ReloadUI`): a CVar with no host knob
    /// (`statusBarText`) and a CVar only an addon declares would otherwise revert to default on
    /// every VM replacement — and the saver, seeing value == default, would then *strip the
    /// player's setting from the file*.
    pub fn set_cvar_saved_base(&mut self, entries: impl IntoIterator<Item = (String, String)>) {
        self.model_mut().cvars_saved_base = entries
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect();
    }

    /// Register the host-backed CVars (name, default) — boot-time, idempotent. A re-register of
    /// a live table refreshes defaults but never clobbers a value someone already set. A fresh
    /// registration starts at the saved-base value when the config file carries one
    /// ([`Self::set_cvar_saved_base`]), else at the default.
    pub fn register_cvars<'a>(&mut self, vars: impl IntoIterator<Item = (&'a str, &'a str)>) {
        let mut model = self.model_mut();
        for (name, default) in vars {
            let key = name.to_ascii_lowercase();
            match model.cvars.get_mut(&key) {
                Some(slot) => slot.default = default.to_string(),
                None => {
                    let value = model
                        .cvars_saved_base
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| default.to_string());
                    model.cvars.insert(
                        key,
                        CvarSlot {
                            name: name.to_string(),
                            value,
                            default: default.to_string(),
                        },
                    );
                }
            }
        }
    }

    /// Host-side write (loaded config, env override): sets the value WITHOUT queueing a change
    /// event — the host is the caller, an echo would re-dirty the file it just read. Unknown
    /// names warn once, same posture as the Lua side.
    pub fn set_cvar_host(&mut self, name: &str, value: &str) {
        let mut model = self.model_mut();
        let key = name.to_ascii_lowercase();
        match model.cvars.get_mut(&key) {
            Some(slot) => slot.value = value.to_string(),
            None => warn_unknown(&mut model, name),
        }
    }

    /// Read one CVar's live value (host side).
    pub fn cvar(&self, name: &str) -> Option<String> {
        self.model_mut()
            .cvars
            .get(&name.to_ascii_lowercase())
            .map(|s| s.value.clone())
    }

    /// Snapshot the whole table as `(name, value, default)` — the saver writes the entries whose
    /// value moved off the default, and nothing else (the config file stays a diff, not a dump).
    pub fn cvars_snapshot(&self) -> Vec<(String, String, String)> {
        self.model_mut()
            .cvars
            .values()
            .map(|s| (s.name.clone(), s.value.clone(), s.default.clone()))
            .collect()
    }

    /// Drain the `(name, new_value)` changes Lua `SetCVar` queued since the last call — the
    /// host's cue to sync its resources and mark the config dirty. Names are the registered
    /// spelling regardless of the caller's casing.
    pub fn take_cvar_changes(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.model_mut().cvar_changes)
    }
}

/// An **engine-side** write: a value the engine owns moved, and the CVar table mirrors it. The
/// minimap's zoom is the case that needs it — the client's `set_zoom` (`0x6da8e0`) writes the live
/// index *and* `CVar::Set`s `minimapZoom`/`minimapInsideZoom` in the same breath, which is exactly
/// why the level survives a restart. So this queues the change like a Lua `SetCVar` would (the host
/// must hear it and dirty the config file), unlike [`super::UiScript::set_cvar_host`], whose whole
/// point is *not* to echo the value it just loaded.
///
/// A name the host never registered is a **silent** no-op here, not a warning: engine writes are
/// code, not UI content, so a miss means this build's host doesn't back the var (a bare test VM) —
/// and the registered set is welded to the code truths by `crate::cvars`'s own test.
pub(super) fn set_from_engine(model: &mut Model, name: &str, value: String) {
    let Some(slot) = model.cvars.get_mut(&name.to_ascii_lowercase()) else {
        return;
    };
    if slot.value == value {
        return;
    }
    slot.value = value.clone();
    let registered = slot.name.clone();
    model.cvar_changes.push((registered, value));
}

/// Push the warn-once for an unknown CVar name into the model's warning stream.
fn warn_unknown(model: &mut Model, name: &str) {
    let key = name.to_ascii_lowercase();
    if model.cvars_warned.insert(key) {
        model.warnings.push(format!(
            "unknown CVar '{name}' (not host-registered) — ignored"
        ));
    }
}

/// Coerce the Lua argument to the string the table stores: the client stringifies numbers and
/// booleans the same way (`SetCVar("MusicVolume", 0.4)` is the common call shape).
fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => s.to_str().ok().map(|s| s.to_owned()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Boolean(b) => Some(if *b { "1".into() } else { "0".into() }),
        _ => None,
    }
}

/// Register the `GetCVar`/`SetCVar`/`GetCVarDefault`/`RegisterCVar` globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    // `RegisterCVar(name, value)` — declare a CVar the client does not ship (decision 1195).
    //
    // This is how an addon gets a persisted setting without a saved-variables file, and it is why
    // `GetCVar` on an unknown name is a *warning* rather than an error: an addon that calls
    // `RegisterCVar` at load and `GetCVar` after expects the second to answer. Registering an
    // existing name is a **no-op, not an overwrite** — the reference will not let an addon reset a
    // client CVar's live value by re-declaring it, and a re-run of the addon's own load must not
    // wipe the player's setting either.
    lua.globals().set(
        "RegisterCVar",
        lua.create_function(|lua, (name, value): (String, Option<Value>)| {
            let value = value.as_ref().and_then(value_to_string).unwrap_or_default();
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let key = name.to_ascii_lowercase();
            // The saved base outranks the declared default (decision 1291): an addon-declared
            // CVar the player has set persists in the config file, and this lookup is how the
            // saved value survives the VM being replaced — the declared value stays the DEFAULT,
            // so the saver still knows what "moved off default" means for this key.
            let saved = model.cvars_saved_base.get(&key).cloned();
            model.cvars.entry(key).or_insert(CvarSlot {
                name,
                value: saved.unwrap_or_else(|| value.clone()),
                default: value,
            });
            Ok(())
        })?,
    )?;

    lua.globals().set(
        "GetCVar",
        lua.create_function(|lua, name: String| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            match model.cvars.get(&name.to_ascii_lowercase()) {
                Some(slot) => Ok(Value::String(lua.create_string(&slot.value)?)),
                None => {
                    warn_unknown(&mut model, &name);
                    Ok(Value::Nil)
                }
            }
        })?,
    )?;
    lua.globals().set(
        "GetCVarDefault",
        lua.create_function(|lua, name: String| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            match model.cvars.get(&name.to_ascii_lowercase()) {
                Some(slot) => Ok(Value::String(lua.create_string(&slot.default)?)),
                None => {
                    warn_unknown(&mut model, &name);
                    Ok(Value::Nil)
                }
            }
        })?,
    )?;
    lua.globals().set(
        "SetCVar",
        lua.create_function(|lua, args: mlua::MultiValue| {
            let mut it = args.iter();
            let (Some(Value::String(name)), Some(v)) = (it.next(), it.next()) else {
                return Err(mlua::Error::runtime("Usage: SetCVar(\"name\", value)"));
            };
            let name = name.to_str()?.to_owned();
            let Some(value) = value_to_string(v) else {
                return Err(mlua::Error::runtime("Usage: SetCVar(\"name\", value)"));
            };
            // The THIRD argument is the `CVAR_UPDATE` token, and it is the whole mechanism behind
            // that event (decision 1140). 1.12's own callers are its two options panels —
            // `SetCVar(value.cvar, value.value, index)` (UIOptionsFrame.lua l.335/343/345,
            // OptionsFrame.lua l.192) — where `index` is the CheckButtons table's KEY, an
            // uppercase display name like "STATUS_BAR_TEXT". The engine passes it straight through
            // as arg1, which is why `UIOptionsFrame_OnEvent` can look the row up with
            // `UIOptionsFrameCheckButtons[arg1]`, and why `TextStatusBar_OnEvent` compares against
            // "STATUS_BAR_TEXT" rather than "statusBarText".
            //
            // A caller that omits it fires nothing — the event is opt-in per write, not a
            // property of the variable. (Which means `ReputationFrame.xml`'s
            // `arg1 == "statusBarText"` arm is dead code in shipped 1.12: nothing ever passes the
            // CVar's own name as the token. Transcribed as found, not repaired.)
            let token = it.next().and_then(value_to_string);
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let key = name.to_ascii_lowercase();
            match model.cvars.get_mut(&key) {
                Some(slot) => {
                    if slot.value != value {
                        slot.value = value.clone();
                        let reg_name = slot.name.clone();
                        model.cvar_changes.push((reg_name, value.clone()));
                        if let Some(token) = token {
                            model.pending_events.push((
                                "CVAR_UPDATE".to_string(),
                                vec![ScriptValue::Str(token), ScriptValue::Str(value)],
                            ));
                        }
                    }
                }
                None => warn_unknown(&mut model, &name),
            }
            Ok(())
        })?,
    )
}

#[cfg(test)]
mod tests {
    use crate::script::UiScript;

    fn script_with_volume() -> UiScript {
        let mut s = UiScript::new().unwrap();
        s.register_cvars([("MusicVolume", "0.4"), ("MasterVolume", "1.0")]);
        s
    }

    #[test]
    fn the_table_round_trips_and_queues_changes_case_insensitively() {
        let mut s = script_with_volume();
        // Registration seeds value = default; reads are case-insensitive.
        assert_eq!(
            s.eval::<String>(r#"return GetCVar("musicvolume")"#)
                .unwrap(),
            "0.4"
        );
        // A Lua write queues ONE change under the REGISTERED spelling; the number stringifies.
        s.run(r#"SetCVar("MUSICVOLUME", 0.7)"#).unwrap();
        assert_eq!(
            s.take_cvar_changes(),
            vec![("MusicVolume".to_string(), "0.7".to_string())]
        );
        assert!(s.take_cvar_changes().is_empty(), "drained");
        // The default is unmoved and separately readable.
        assert_eq!(
            s.eval::<String>(r#"return GetCVarDefault("MusicVolume")"#)
                .unwrap(),
            "0.4"
        );
        assert_eq!(s.cvar("MusicVolume").as_deref(), Some("0.7"));
        // A write to the same value queues nothing (quiet frames stay quiet).
        s.run(r#"SetCVar("MusicVolume", "0.7")"#).unwrap();
        assert!(s.take_cvar_changes().is_empty());
    }

    /// **`SetCVar`'s third argument is the `CVAR_UPDATE` token** (decision 1140) — the whole
    /// mechanism behind that event. 1.12's options panels pass their CheckButtons table KEY
    /// (`SetCVar(value.cvar, value.value, index)`), an uppercase display name, and the engine
    /// hands it back verbatim as arg1 — which is what lets `UIOptionsFrame_OnEvent` do
    /// `UIOptionsFrameCheckButtons[arg1]`. Omit it and nothing fires: the event is opt-in per
    /// write. A write that changes nothing fires nothing either, like the change queue.
    #[test]
    fn the_third_argument_is_the_cvar_update_token() {
        let mut s = script_with_volume();
        s.run(
            "SEEN = {} \
             f = CreateFrame(\"Frame\") \
             f:RegisterEvent(\"CVAR_UPDATE\") \
             f:SetScript(\"OnEvent\", function() table.insert(SEEN, arg1 .. \"=\" .. arg2) end)",
        )
        .unwrap();

        // No token: the value moves, the change queues, the event does not fire.
        s.run(r#"SetCVar("MusicVolume", "0.5")"#).unwrap();
        s.tick(0.0);
        assert_eq!(s.eval::<f64>("return getn(SEEN)").unwrap(), 0.0);

        // With one: arg1 is the token verbatim (the display name, NOT the CVar's own name),
        // arg2 the new value.
        s.run(r#"SetCVar("MusicVolume", "0.6", "MUSIC_VOLUME")"#)
            .unwrap();
        s.tick(0.0);
        assert_eq!(
            s.eval::<String>("return SEEN[1]").unwrap(),
            "MUSIC_VOLUME=0.6"
        );

        // A no-op write is silent on both channels.
        s.run(r#"SetCVar("MusicVolume", "0.6", "MUSIC_VOLUME")"#)
            .unwrap();
        s.tick(0.0);
        assert_eq!(s.eval::<f64>("return getn(SEEN)").unwrap(), 1.0);
        assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    }

    #[test]
    fn host_writes_do_not_echo_and_snapshots_carry_defaults() {
        let mut s = script_with_volume();
        s.set_cvar_host("MasterVolume", "0.25");
        assert!(
            s.take_cvar_changes().is_empty(),
            "a host write must not re-dirty the config it just loaded"
        );
        assert_eq!(
            s.eval::<String>(r#"return GetCVar("MasterVolume")"#)
                .unwrap(),
            "0.25"
        );
        let snap = s.cvars_snapshot();
        let master = snap.iter().find(|(n, _, _)| n == "MasterVolume").unwrap();
        assert_eq!((master.1.as_str(), master.2.as_str()), ("0.25", "1.0"));
    }

    #[test]
    fn unknown_names_warn_once_and_no_op() {
        let mut s = script_with_volume();
        assert!(s
            .eval::<bool>(r#"return GetCVar("bogusKnob") == nil"#)
            .unwrap());
        s.run(r#"SetCVar("bogusKnob", 1)"#).unwrap();
        s.run(r#"SetCVar("bogusKnob", 2)"#).unwrap();
        assert!(s.take_cvar_changes().is_empty());
        let warns: Vec<String> = s.take_warnings();
        assert_eq!(warns.len(), 1, "warn-once: {warns:?}");
        assert!(warns[0].contains("bogusKnob"));
        // Registration is idempotent and never clobbers a live value.
        s.set_cvar_host("MusicVolume", "0.9");
        s.register_cvars([("MusicVolume", "0.4")]);
        assert_eq!(s.cvar("MusicVolume").as_deref(), Some("0.9"));
    }
    /// **`GetCVar("realmName")` answers the session's realm, set by the one seam that owns it.**
    ///
    /// It is a real 1.12 CVar (`0x83f2d0`, persisted — the client builds its SavedVariables path
    /// from it) and it had no value here at all. `Ace/AceState.lua:27` is
    /// **Registration honors the saved base** (decision 1291) — the bridge that makes the
    /// per-VM table behave like the reference's engine-side store: a knobless CVar keeps the
    /// player's persisted value across a VM replacement, and the default stays the DEFAULT so
    /// "moved off default" still means something to the saver.
    #[test]
    fn registration_starts_at_the_saved_value_not_the_default() {
        let mut s = UiScript::new().unwrap();
        s.set_cvar_saved_base([("StatusBarText".to_string(), "1".to_string())]);
        s.register_cvars([("statusBarText", "0"), ("farclip", "500")]);

        assert_eq!(
            s.eval::<String>(r#"return GetCVar("statusBarText")"#)
                .unwrap(),
            "1",
            "a saved value outranks the registered default (any key case)"
        );
        assert_eq!(
            s.eval::<String>(r#"return GetCVarDefault("statusBarText")"#)
                .unwrap(),
            "0",
            "…while the default stays the default"
        );
        assert_eq!(
            s.eval::<String>(r#"return GetCVar("farclip")"#).unwrap(),
            "500",
            "a key the file never carried starts at its default"
        );
    }

    /// The same law through the Lua half: an addon's `RegisterCVar` (decision 1195) starts at
    /// the persisted value, so its setting survives the VM being replaced (1290/1291).
    #[test]
    fn an_addon_registered_cvar_starts_at_its_saved_value() {
        let mut s = UiScript::new().unwrap();
        s.set_cvar_saved_base([("myaddon_scale".to_string(), "2.5".to_string())]);
        s.run(r#"RegisterCVar("MyAddon_Scale", "1.0")"#).unwrap();
        assert_eq!(
            s.eval::<String>(r#"return GetCVar("MyAddon_Scale")"#)
                .unwrap(),
            "2.5"
        );
        assert_eq!(
            s.eval::<String>(r#"return GetCVarDefault("MyAddon_Scale")"#)
                .unwrap(),
            "1.0",
            "the declared value is the DEFAULT, not the live value"
        );
    }

    /// `ace.trim(GetCVar("realmName"))` inside `SetGameState`, which EVERY Ace addon runs at
    /// PLAYER_ENTERING_WORLD, so nil became `gsub(nil, ...)` and took the whole family down.
    ///
    /// Asserted through `set_realm_name` rather than by writing the CVar directly, because that is
    /// the point: the CVar and `GetRealmName()` are the SAME fact and must not be settable apart.
    #[test]
    fn the_realm_name_cvar_and_get_realm_name_are_one_fact() {
        let mut s = UiScript::new().unwrap();
        s.register_cvars([("realmName", "")]);

        // Before a session: empty, never nil — `ace.trim` must have a string to gsub.
        assert_eq!(
            s.eval::<String>(r#"return GetCVar("realmName")"#).unwrap(),
            ""
        );

        s.set_realm_name("Archimonde");
        assert_eq!(
            s.eval::<String>(r#"return GetCVar("realmName")"#).unwrap(),
            "Archimonde",
            "the realm seam must write the CVar too, or the two facts drift"
        );

        // Ace's own line, run for real.
        let trimmed: String = s
            .eval(r#"return string.gsub(GetCVar("realmName"), "^%s*", "")"#)
            .unwrap();
        assert_eq!(trimmed, "Archimonde");
    }
}
