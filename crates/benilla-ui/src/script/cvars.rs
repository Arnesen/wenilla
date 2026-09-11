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

/// One device-supported multisample format, as the Video options dropdown shows it.
///
/// The reference's distilled triple (`[0xb4b444]`, count `[0xb4b440]`, built by `0x48c3e0`), and
/// the shape `MULTISAMPLING_FORMAT_STRING` formats: `"%d-bit color %d-bit depth %dx multisample"`.
///
/// `samples == 1` is the encoding of **no multisampling**, not "one sample of it" — the convention
/// both of the reference's enumerators normalise to (`0x58b8b1`'s jump table maps
/// `D3DMULTISAMPLE_NONE → 1`; the GL path at `0x58d5fa`/`0x58d5fe`/`0x58d605` turns a failed
/// `WGL_SAMPLES_ARB` query or a value of 0 into 1), and the same convention the device paths test
/// with `> 1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MultisampleFormat {
    pub color_bits: u32,
    pub depth_bits: u32,
    pub samples: u32,
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
    ///
    /// Takes `&self` rather than `&mut self` because the table lives behind the VM's app-data
    /// (interior mutability), and the interface loader has to be able to guarantee it from a
    /// `&UiScript` — the stock `UIOptionsFrame.xml` reads CVars in its own `OnLoad`, so a VM that
    /// loads the client's interface without the client's CVar table is not the client
    /// (decision 2115).
    pub fn register_cvars<'a>(&self, vars: impl IntoIterator<Item = (&'a str, &'a str)>) {
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

    /// A native surface's write — [`set_from_engine`]'s public face: sets the value AND queues
    /// the change like a Lua `SetCVar`, so the host's sync dirties the config file. The glue
    /// AddOns screen's *Load out of date AddOns* checkbox is the caller (decision 1293): a
    /// native widget editing a CVar is the minimap-zoom pattern, reached from outside the crate.
    pub fn set_cvar_engine(&mut self, name: &str, value: &str) {
        set_from_engine(&mut self.model_mut(), name, value.to_string());
    }

    /// Publish the multisample formats this run's device actually accepts — what the Video
    /// options dropdown offers, in the order it offers them.
    ///
    /// The host's half of the reference's `0x48c3e0`: there, a D3D `CheckDeviceMultiSampleType`
    /// sweep over the nine-entry candidate table `0x85a83c` or a GL `wglGetPixelFormatAttribivARB`
    /// sweep over every pixel format; here, `benilla_world::view::supported_sample_counts` asking
    /// wgpu the same question. Pushed rather than pulled because the VM lives in `benilla-ui`,
    /// which has no render adapter and should not grow one.
    pub fn set_multisample_formats(&mut self, formats: Vec<MultisampleFormat>) {
        self.model_mut().multisample_formats = formats;
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
/// The `SetCVar` write, shared with `ConsoleExec` (which is `/console name value`): the slot
/// matched case-insensitively, the change queued for the host only when the value moved, and
/// `CVAR_UPDATE` fired only when a token was passed. `false` = no such CVar (warned once).
pub(super) fn write_cvar(
    model: &mut Model,
    name: &str,
    value: String,
    token: Option<String>,
) -> bool {
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
            true
        }
        None => {
            warn_unknown(model, name);
            false
        }
    }
}

fn warn_unknown(model: &mut Model, name: &str) {
    let key = name.to_ascii_lowercase();
    if model.cvars_warned.insert(key) {
        model.record_warning(format!(
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
/// The two nameplate toggles' registered CVar names — **defined here, in the crate that publishes
/// the four verbs that write them**, so the binding and the name cannot drift apart. The app reads
/// these consts rather than re-spelling the strings, and welds them to its registered table with
/// its own test.
///
/// They are the LATER era's names: 1.12 registers no nameplate CVar at all (censused — see
/// [`install_nameplate_verbs`]), so benilla's persistence takes the era spelling rather than
/// inventing one, the same posture as `autoLootDefault`.
pub const CVAR_NAMEPLATE_ENEMIES: &str = "nameplateShowEnemies";
/// The friendly-plate half of [`CVAR_NAMEPLATE_ENEMIES`].
pub const CVAR_NAMEPLATE_FRIENDS: &str = "nameplateShowFriends";

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
    // ── The Video options Multisampling dropdown ──────────────────────────────────────────────
    // Three bindings over one list, exactly as `OptionsFrame.lua` consumes them: `_OnLoad` seeds
    // the selection with `GetCurrentMultisampleFormat()`, `_Initialize` walks
    // `GetMultisampleFormats()` three varargs at a time building the menu, and the Okay handler
    // (l.240) calls `SetMultisampleFormat(UIDropDownMenu_GetSelectedID(...))`. Identities and
    // behaviour from wow-re `system/console/scratch/gxmultisample-default.md` §7 — registration
    // table `0x83de68`, records `0x83e2c0`/`0x83e2c8`/`0x83e2d0`.
    lua.globals().set(
        "GetMultisampleFormats",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            // Flat, three per entry — `0x48c360` "pushes all three fields of every entry", and the
            // Lua walks `for i=1, arg.n, 3`. A device with nothing to offer returns nothing, and
            // the loop runs zero times: an empty dropdown, not a fabricated one.
            let mut out = mlua::MultiValue::new();
            for f in &model.multisample_formats {
                out.push_back(Value::Number(f.color_bits as f64));
                out.push_back(Value::Number(f.depth_bits as f64));
                out.push_back(Value::Number(f.samples as f64));
            }
            Ok(out)
        })?,
    )?;
    lua.globals().set(
        "GetCurrentMultisampleFormat",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let cvar = |n: &str| model.cvars.get(n).and_then(|s| s.value.parse::<u32>().ok());
            let (Some(color), Some(depth), Some(samples)) = (
                cvar("gxcolorbits"),
                cvar("gxdepthbits"),
                cvar("gxmultisample"),
            ) else {
                return Ok(1.0f64);
            };
            // **1-based, and 1.0 on no match** — `0x48c580` "returns the 1-based index of the
            // matching triple, or 1.0 on no match". Not 0 and not nil: `_OnLoad` feeds this
            // straight to `UIDropDownMenu_SetSelectedID`, so a miss has to name a real row.
            let idx = model
                .multisample_formats
                .iter()
                .position(|f| {
                    f.color_bits == color && f.depth_bits == depth && f.samples == samples
                })
                .map_or(1.0, |i| (i + 1) as f64);
            Ok(idx)
        })?,
    )?;
    lua.globals().set(
        "SetMultisampleFormat",
        lua.create_function(|lua, id: Option<f64>| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            // 1-based from `UIDropDownMenu_GetSelectedID`; anything off the end is ignored rather
            // than clamped, because a clamp would silently apply a format the player did not pick.
            let Some(f) = id
                .filter(|v| *v >= 1.0)
                .and_then(|v| model.multisample_formats.get(v as usize - 1).copied())
            else {
                return Ok(());
            };
            // All three, from the chosen entry — `0x48c640` "writes those three CVars from the
            // chosen entry". Through `set_from_engine` so each ride the host's change queue and
            // the config file is marked dirty, the same route a Lua `SetCVar` takes.
            set_from_engine(&mut model, "gxColorBits", f.color_bits.to_string());
            set_from_engine(&mut model, "gxDepthBits", f.depth_bits.to_string());
            set_from_engine(&mut model, "gxMultisample", f.samples.to_string());
            // The reference then sets `gxRestart` (`0x842978`, `0x63ce00`). We deliberately do not
            // register that CVar: `gxMultisample` is latched here too (decision 1629 — the camera
            // reads its sample count once, at spawn), so "applies at next launch" is already the
            // behaviour and a second flag saying so would be a flag nothing reads.
            Ok(())
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
            write_cvar(&mut model, &name, value, token);
            Ok(())
        })?,
    )?;

    install_nameplate_verbs(lua)?;
    install_world_detail_verbs(lua)
}

/// The `WorldDetail` slider's **stop**, benilla's spelling — the panel position `0`/`1`/`2`, held as
/// a CVar because 1.12 has no such CVar and our own Graphics page needs somewhere to keep it (the
/// `autoLootDefault` posture; the app's table carries the reasoning and the `Same` row).
///
/// Defined here, in the crate that publishes the two verbs that read and write it, so the binding
/// and the name cannot drift apart — as for [`CVAR_NAMEPLATE_ENEMIES`]. The app welds both with its
/// own test.
pub const CVAR_WORLD_DETAIL: &str = "WorldDetail";

/// The reference's own Environment Detail CVar — cells visited per chunk by the detail-doodad
/// scatter. [`WORLD_DETAIL_STOPS`] is what the slider writes into it.
pub const CVAR_FRILL_DENSITY: &str = "frillDensity";

/// **`SetWorldDetail`'s preset table, verbatim** — the three dwords at `0x804518`, `{0x10, 0x20,
/// 0x30}`. Read out of `WoW.exe` rather than transcribed from a note.
pub const WORLD_DETAIL_STOPS: [u32; 3] = [16, 32, 48];

/// **The Environment Detail pair** — `SetWorldDetail 0x488dd0` and `GetWorldDetail 0x488d70`, the
/// two engine bindings 1.12's own options panel drives that slider through (`OptionsFrame.lua`'s
/// row 3 is `func = "WorldDetail"`, and `OptionsFrame_Save`/`_Load` prefer `getglobal("Set"..func)`
/// / `getglobal("Get"..func)` over `SetCVar`/`GetCVar`). Decision 2163.
///
/// **The setter, carved end to end** (own decode, agreeing with wow-re
/// `cvar/scratch/registered-defaults-census.md` §8):
///
/// ```text
/// 488ddf  call 0x6f34d0             ; lua_isnumber(L,1)? else error 0x8423e8
/// 488e05  call 0x6f3620             ; lua_tonumber(L,1)
/// 488e0a  call 0x40a2b0             ; -> int, RC forced to CHOP (`or ah,0x0c`): TRUNCATE toward zero
/// 488e11  test esi,esi; jl  0x488e9b ; n < 0  -> error 0x8423b8
/// 488e19  cmp  esi,3;   jge 0x488e9b ; n >= 3 -> error 0x8423b8
/// 488e1e  mov eax,[esi*4 + 0x804518] ; {16,32,48}      -> "%d"  -> CVar::Set "frillDensity"
/// 488e56  fld dword [esi*4+0x804524] ; {0.07,0.04,0.01} -> "%f" -> CVar::Set "smallCull"
/// ```
///
/// The two error strings are `0x8423e8` `"Usage: SetWorldDetail(value)"` and `0x8423b8` `"value
/// must be in the range 0, 2"` — both read out of the image, lowercase `v` and all. Every path
/// returns **zero** Lua values.
///
/// Three shapes worth stating because the obvious reading is wrong on each:
///
/// - **The truncation is toward zero, not a floor.** `0x40a2b0` saves the FPU control word, ORs
///   `0x0c` into the high byte (rounding-control = chop) and `fistp`s. So `SetWorldDetail(2.9)`
///   is stop 2 and `SetWorldDetail(-0.5)` is stop **0** — a negative that truncates to zero is
///   accepted, and only `<= -1` raises.
/// - **`lua_isnumber` accepts a numeric string**, so `SetWorldDetail("2")` works there and here.
/// - **The getter is not the setter's inverse.** `0x488d70` reads only `smallCull`, seeds its
///   result at **2**, and runs `for i in 0..3 { if v <= tbl[i] { result = i } }` over the
///   *descending* `{0.07, 0.04, 0.01}` with **no break** — so the last match wins, and any
///   `smallCull` above `0.07` (legal to 2.0) saturates to 2, i.e. the least detail reads back as
///   the most. A shipped defect, and one of two: `OptionsFrame_SetDefaults` tests that same
///   descending table with ascending `elseif`s, so Restore Defaults always parks the slider at 0.
///
/// **Where benilla diverges, and why it is not the smallCull ladder.** `SmallCull` is a dead knob
/// in the reference — `[0x868620]` has one writer and no reader image-wide (wow-re
/// `cvar/scratch/graphics-cost-cvar-census.md` §8) — so its *only* consumer there is this getter,
/// which makes it storage for a stop the client keeps nowhere else. benilla keeps the stop:
/// [`CVAR_WORLD_DETAIL`] is it, and `frillDensity` is the same knob in the reference's unit. So the
/// getter reads the stop directly and `SmallCull` is not registered, because registering a CVar
/// whose only purpose is to hold a number we already hold is the silent pretence 1203 forbids.
///
/// The observable consequence is **nil at boot and nil for every consumer we know**: a fresh
/// reference client has `frillDensity 16` with `smallCull 0.04` — stop 0's frill with stop 1's cull
/// — so its getter answers **1**, and benilla's `WorldDetail` registers at `"1"` for exactly that
/// reason. The one input that separates them is a bare `frillDensity` write with no stop write,
/// which there leaves the getter on the stale stop and here moves it; pfUI's `hdgraphic` is the
/// only known caller and its own replacement short-circuits above 48, so it never reaches the
/// difference.
fn install_world_detail_verbs(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    g.set(
        "SetWorldDetail",
        lua.create_function(|lua, value: Value| {
            // `lua_isnumber`: a number, or a string Lua can convert to one. Anything else is the
            // usage error — NOT a silent no-op.
            let n = match &value {
                Value::Integer(i) => *i as f64,
                Value::Number(n) => *n,
                Value::String(s) => {
                    match s.to_str().ok().and_then(|s| s.trim().parse::<f64>().ok()) {
                        Some(n) => n,
                        None => return Err(mlua::Error::runtime(USAGE_SET_WORLD_DETAIL)),
                    }
                }
                _ => return Err(mlua::Error::runtime(USAGE_SET_WORLD_DETAIL)),
            };
            // Truncate toward zero (`0x40a2b0`'s chop), then the reference's own two bounds. NaN
            // has no truncation — the reference's `fistp` yields the integer indefinite, which is
            // negative and raises, so raising here is the same answer by the same door.
            if n.is_nan() {
                return Err(mlua::Error::runtime(RANGE_SET_WORLD_DETAIL));
            }
            let stop = n.trunc();
            if !(0.0..3.0).contains(&stop) {
                return Err(mlua::Error::runtime(RANGE_SET_WORLD_DETAIL));
            }
            let stop = stop as usize;
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            // TWO CVars per stop, as `0x488dd0` writes two: there it is `frillDensity` + the dead
            // `smallCull`; here it is `frillDensity` + the stop the panel keeps. Both land on one
            // clutter knob in the host, so applying both is idempotent — and writing only one
            // would leave the other stale for a `GetWorldDetail` in the same Lua tick, since the
            // host does not drain the change queue until the end of the frame.
            write_cvar(
                &mut model,
                CVAR_FRILL_DENSITY,
                WORLD_DETAIL_STOPS[stop].to_string(),
                None,
            );
            write_cvar(&mut model, CVAR_WORLD_DETAIL, stop.to_string(), None);
            // Zero return values, not nil (`eax = 0` at every `ret`).
            Ok(mlua::MultiValue::new())
        })?,
    )?;
    g.set(
        "GetWorldDetail",
        // `MultiValue` in: the reference reads argument 1 for the SETTER only; the getter never
        // calls `lua_gettop`, so an argument is accepted and ignored — and pfUI's replacement is
        // declared `function _G.GetWorldDetail(arg)`, so it is passed one in practice.
        lua.create_function(|lua, _: mlua::MultiValue| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let stop = model
                .cvars
                .get(&CVAR_WORLD_DETAIL.to_ascii_lowercase())
                .and_then(|slot| slot.value.parse::<f64>().ok())
                // The reference can only ever answer 0, 1 or 2 (its result is an index into a
                // three-entry table), so an off-grid stop — which ours can hold, because a console
                // `frillDensity 200` moves the same knob — reports the nearest one rather than a
                // number no caller has a branch for.
                .map_or(0, |v| v.round().clamp(0.0, 2.0) as i64);
            Ok(stop)
        })?,
    )?;
    Ok(())
}

/// `0x8423e8`, verbatim.
const USAGE_SET_WORLD_DETAIL: &str = "Usage: SetWorldDetail(value)";

/// `0x8423b8`, verbatim — lowercase `v`, and the odd comma is the reference's own.
const RANGE_SET_WORLD_DETAIL: &str = "value must be in the range 0, 2";

/// **The four nameplate verbs** — `ShowNameplates 0x489450`, `HideNameplates 0x489460`,
/// `ShowFriendNameplates 0x489470`, `HideFriendNameplates 0x489480` (wow-re
/// `ui/scratch/party-leader-and-nameplate-verbs.md`, §5 four-worker round).
///
/// Each is a **10-byte body** over one of two setters, and the reference's shape is worth stating
/// because two obvious readings are wrong:
///
/// - **They read NO argument.** `ShowNameplates(false)` still shows. There is no `lua_gettop`, no
///   `lua_toboolean`, nothing — the verb *is* the value, which is why there are four of them
///   rather than two taking a flag.
/// - **They return ZERO Lua values** — not `nil`, nothing. (`eax` at the `ret` is the return
///   count, read off the VM's own C arm `0x6f61a8`.)
/// - They are **four separate bodies**, not a masked pair like `UnitIsTapped`/`ByPlayer`:
///   `0x489450` and `0x489470` are literally identical byte strings, rel32 included, over setters
///   that differ only in their `or`/`and` masks.
///
/// The engine state is bits `0x1` (enemy) and `0x8` (friend) of one runtime dword `[0xc4da34]`.
/// **There is no getter** — 15 references image-wide, all `.text`, and the 1153 `{name, fn}`
/// records across all 54 registrar tables intersect the readers in the empty set — so an addon can
/// set the state and never read it back, and neither can we.
///
/// **Where benilla diverges, deliberately and now knowingly.** 1.12 registers no nameplate CVar
/// (censused: `0x63db90`'s 214 sites, 207 literal names, zero matching `/plate/i`), and the engine
/// clears **both bits on every `EnterWorld`** — not once at boot. FrameXML replays them from its
/// own `NAMEPLATES_ON`/`FRIENDNAMEPLATES_ON` saved variables. Our two CVars reproduce *FrameXML's
/// replay*, not the engine's state model: a benilla player's plates survive a zone-in because the
/// CVar store outlives it. That is a divergence in favour of the player, recorded here rather than
/// mistaken for fidelity.
fn install_nameplate_verbs(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    for (name, cvar, on) in [
        ("ShowNameplates", CVAR_NAMEPLATE_ENEMIES, true),
        ("HideNameplates", CVAR_NAMEPLATE_ENEMIES, false),
        ("ShowFriendNameplates", CVAR_NAMEPLATE_FRIENDS, true),
        ("HideFriendNameplates", CVAR_NAMEPLATE_FRIENDS, false),
    ] {
        g.set(
            name,
            // `MultiValue` and not `()`: mlua would otherwise reject a call that passes anything,
            // and the reference accepts and ignores every argument. Returning `MultiValue::new()`
            // is the zero-value return, which is NOT the same as pushing nil.
            lua.create_function(move |lua, _: mlua::MultiValue| {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                let key = cvar.to_ascii_lowercase();
                if let Some(slot) = model.cvars.get_mut(&key) {
                    let value = if on { "1" } else { "0" };
                    if slot.value != value {
                        slot.value = value.to_string();
                        let reg_name = slot.name.clone();
                        model.cvar_changes.push((reg_name, value.to_string()));
                    }
                }
                Ok(mlua::MultiValue::new())
            })?,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::MultisampleFormat;
    use crate::script::UiScript;

    fn script_with_volume() -> UiScript {
        let s = UiScript::new().unwrap();
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

    /// The Video options Multisampling dropdown, driven exactly as `OptionsFrame.lua` drives it:
    /// `_Initialize` walks `GetMultisampleFormats()` three varargs at a time, `_OnLoad` seeds the
    /// selection from `GetCurrentMultisampleFormat()`, and the Okay handler (l.240) writes back
    /// through `SetMultisampleFormat(id)`.
    #[test]
    fn the_multisample_dropdown_round_trips_through_the_three_bindings() {
        let mut s = UiScript::new().unwrap();
        s.register_cvars([
            ("gxColorBits", "32"),
            ("gxDepthBits", "32"),
            ("gxMultisample", "1"),
        ]);
        s.set_multisample_formats(vec![
            MultisampleFormat {
                color_bits: 32,
                depth_bits: 32,
                samples: 1,
            },
            MultisampleFormat {
                color_bits: 32,
                depth_bits: 32,
                samples: 2,
            },
            MultisampleFormat {
                color_bits: 32,
                depth_bits: 32,
                samples: 4,
            },
        ]);

        // Flat triples, in order — the shape `for i=1, arg.n, 3` walks.
        let flat: Vec<f64> = s.eval(r#"return { GetMultisampleFormats() }"#).unwrap();
        assert_eq!(
            flat,
            vec![32.0, 32.0, 1.0, 32.0, 32.0, 2.0, 32.0, 32.0, 4.0],
            "three fields per entry, entries in offer order"
        );

        // 1-BASED: the default `gxMultisample "1"` is the FIRST row, not the zeroth.
        assert_eq!(
            s.eval::<f64>("return GetCurrentMultisampleFormat()")
                .unwrap(),
            1.0
        );

        // Pick 4x — the third row — and all three CVars move together, as `0x48c640` writes them.
        s.eval::<()>("SetMultisampleFormat(3)").unwrap();
        assert_eq!(
            s.eval::<String>(r#"return GetCVar("gxMultisample")"#)
                .unwrap(),
            "4"
        );
        assert_eq!(
            s.eval::<String>(r#"return GetCVar("gxColorBits")"#)
                .unwrap(),
            "32"
        );
        assert_eq!(
            s.eval::<f64>("return GetCurrentMultisampleFormat()")
                .unwrap(),
            3.0,
            "the selection round-trips: what Set wrote, GetCurrent finds"
        );

        // The host sees the write, so the config file is dirtied — the same route a Lua SetCVar
        // takes. Without this the player's choice would live only until they quit.
        let changed: Vec<String> = s
            .take_cvar_changes()
            .into_iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect();
        assert!(
            changed.contains(&"gxMultisample=4".to_string()),
            "expected gxMultisample on the change queue, got {changed:?}"
        );

        // Off the end is ignored, never clamped: a clamp would silently apply a format the player
        // did not choose.
        s.eval::<()>("SetMultisampleFormat(99)").unwrap();
        assert_eq!(
            s.eval::<String>(r#"return GetCVar("gxMultisample")"#)
                .unwrap(),
            "4",
            "an out-of-range id must leave the selection alone"
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
