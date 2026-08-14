//! **The AddOn API** (1188 phase 2) — the eleven globals an addon manager and a demand-loader use,
//! over a registry the host fills at discovery.
//!
//! Every name here is in the 1.12.1 client's own `_G` with origin `engine`
//! (`reference/1.12-globals.tsv`), so all eleven are ours to implement in Rust and the set is
//! measured rather than remembered. `SaveAddOns`/`ResetAddOns`/`GetAddOnEnableState` are
//! deliberately **not** here: they exist only in the *glue* namespace (`Interface\GlueXML\
//! AddonList.lua`), which is a different `_G` from the in-world one this VM is.
//!
//! ## The shapes are the reference's, read off its own call sites
//!
//! Not from memory — from `Interface\GlueXML\AddonList.lua` and `FrameXML\UIParent.lua` in the
//! 1.12 MPQs:
//!
//! ```lua
//! name, title, notes, url, loadable, reason, security, newVersion = GetAddOnInfo(index)
//! local loaded, reason = LoadAddOn(name)          -- UIParent.lua's UIParentLoadAddOn
//! ```
//!
//! `reason` is a **token**, spliced by the reference into `getglobal("ADDON_"..reason)` — so its
//! spelling is load-bearing and its full set is enumerable from `GlobalStrings.lua`: `MISSING`,
//! `DISABLED`, `CORRUPT`, `BANNED`, `INSECURE`, `INTERFACE_VERSION`, `NOT_DEMAND_LOADED`, the
//! `DEP_*` mirrors of each, and `UNKNOWN_ERROR`. `security` is likewise a token, matched against
//! `"SECURE"`/`"INSECURE"`/`"BANNED"` by the glue's icon picker.
//!
//! **Index or name, everywhere.** Every verb takes either, 1-based, because the reference's do.
//!
//! ## The load law lives next door
//!
//! Every "can this load, and why not" answer comes from [`super::addon_gate`] — the reference's
//! `AddOn_CanLoad 0x51e780` as one pure function (decision 1292). The RE answer 1191 §6 was
//! missing has landed (wow-re `addon-version-gate.md`, §5-verified): the version gate is an
//! exact `== 11200` whose refusal the `checkAddonVersion` CVar suppresses by **actively
//! resetting** the reason — so `INTERFACE_VERSION` is now enforced here exactly as the client
//! enforces it, with the *Load out of date AddOns* toggle as the player's escape, instead of
//! 1191's report-but-never-act interim. The CVar is read live per query (§2.2), which is why a
//! checkbox click needs nothing but a list repaint.

use std::path::PathBuf;

use mlua::{Lua, MultiValue, Value};

use super::addon_gate::{can_load, GateRow, Verdict};
use super::Model;

/// One addon, as the AddOn API sees it — the host fills this at discovery
/// ([`super::UiScript::register_addons`]).
#[derive(Clone, Debug, Default)]
pub struct AddOnInfo {
    /// The folder name. `GetAddOnInfo`'s first return, `ADDON_LOADED`'s `arg1`, and the key every
    /// other verb resolves a name against (case-insensitively, as the reference does).
    pub name: String,
    /// `## Title`. `None` when the manifest declares none — and the reference really does return
    /// nil there, which is why the glue writes `if title then … else SetText(name) end`.
    pub title: Option<String>,
    /// `## Notes`.
    pub notes: Option<String>,
    /// `## URL`.
    pub url: Option<String>,
    /// `## Secure: 1` — Blizzard's own addons carry it. Drives the `security` token.
    pub secure: bool,
    /// `## LoadOnDemand: 1`.
    pub load_on_demand: bool,
    /// `## Dependencies` / `## RequiredDeps`.
    pub dependencies: Vec<String>,
    /// Every `## Key: Value` in manifest order, for `GetAddOnMetadata`.
    pub directives: Vec<(String, String)>,
    /// The `.toc`'s ordered file list — what `LoadAddOn` runs.
    pub files: Vec<String>,
    /// `## SavedVariables` — globals restored from, and written to, the account-scoped file.
    pub saved_variables: Vec<String>,
    /// `## SavedVariablesPerCharacter` — the same, per character. Loaded **second**, so a
    /// per-character value wins over the account one (`0x51f4b5` then `0x51f53b`).
    pub saved_variables_per_character: Vec<String>,
    /// Enable state, from `AddOns.txt`. An addon nobody has ever disabled is enabled.
    pub enabled: bool,
    /// `enabled` as registration found it — the last-SAVED state, which `ResetDisabledAddOns`
    /// reverts to. Stamped by [`super::UiScript::register_addons`]; callers need not set it.
    pub saved_enabled: bool,
    /// Has it loaded this session?
    pub loaded: bool,
    /// `## Interface` as the client parses it (`Toc::interface_version` — the leading integer,
    /// `0` when the line is absent). What the version gate compares (decision 1292).
    pub interface: u32,
}

/// Resolve a Lua index-or-name argument to a position in the registry.
///
/// The reference's verbs all take either, and an addon in the wild passes whichever it has —
/// `IsAddOnLoaded("Bagnon")` from a dependant, `GetAddOnInfo(i)` from a list walker. Names compare
/// case-insensitively for the same reason dependency lookup does: a `.toc` may spell a name any way.
fn resolve(model: &Model, key: &Value) -> Option<usize> {
    let by_index = |n: i64| usize::try_from(n).ok()?.checked_sub(1);
    match key {
        // 1-based, like every indexed API in the tree. A Lua number literal arrives as either
        // Integer or Number depending on how it was written, so both arms are real.
        Value::Integer(i) => by_index(*i),
        Value::Number(n) => by_index(*n as i64),
        Value::String(s) => {
            let want = s.to_str().ok()?;
            model
                .addons
                .iter()
                .position(|a| a.name.eq_ignore_ascii_case(&want))
        }
        _ => None,
    }
    .filter(|i| *i < model.addons.len())
}

/// Lower the registry into the gate's rows — ONE adapter, so every verb consults the same law
/// ([`super::addon_gate`], decision 1292) over the same facts.
fn gate_rows(model: &Model) -> Vec<GateRow<'_>> {
    model
        .addons
        .iter()
        .map(|a| GateRow {
            name: &a.name,
            enabled: a.enabled,
            interface: a.interface,
            load_on_demand: a.load_on_demand,
            loaded: a.loaded,
            dependencies: a.dependencies.iter().map(String::as_str).collect(),
        })
        .collect()
}

/// The live `checkAddonVersion` read — the gate's CVar half, re-read per query like the
/// reference's (`IsAddonVersionCheckEnabled 0x51f180` inside `AddOn_CanLoad`, §2.2). An absent
/// table (a bare test VM, or a query before the host's seed) answers the registrar default:
/// check ON, `"1"`.
fn version_check(model: &Model) -> bool {
    model
        .cvars
        .get("checkaddonversion")
        .is_none_or(|s| s.value != "0")
}

/// [`can_load`] over the registry, in the in-game flavour (`dl=1` — the surface these verbs
/// are): the addon's own verdict, with the loaded short-circuit the in-game `GetAddOnInfo`
/// applies (`0x48e390`: an already-loaded addon reports loadable/nil before the gate runs, so
/// flipping its toggle mid-session cannot retroactively mark it unloadable).
fn verdict(model: &Model, i: usize) -> Verdict {
    if model.addons[i].loaded {
        return Verdict::Loadable;
    }
    can_load(&gate_rows(model), i, true, version_check(model))
}

/// `1`/`nil` — the client's boolean shape, which every addon tests with a bare `if`.
fn flag(b: bool) -> Value {
    if b {
        Value::Integer(1)
    } else {
        Value::Nil
    }
}

fn lua_str(lua: &Lua, s: &str) -> mlua::Result<Value> {
    Ok(Value::String(lua.create_string(s)?))
}

fn opt_str(lua: &Lua, s: &Option<String>) -> mlua::Result<Value> {
    match s {
        Some(s) => lua_str(lua, s),
        None => Ok(Value::Nil),
    }
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    g.set(
        "GetNumAddOns",
        lua.create_function(|lua, ()| {
            Ok(lua.app_data_ref::<Model>().expect("model").addons.len())
        })?,
    )?;

    // `name, title, notes, enabled, loadable, reason, security` — SEVEN values, and slot 4 is
    // **`enabled`**, not `url`.
    //
    // **The client registers `GetAddOnInfo` TWICE, as two different functions**, and this is the
    // in-game one. wow-re `system/ui/scratch/addon-version-gate.md`: glue `0x46d460` returns EIGHT
    // with `url` at slot 4 and `newVersion` at 8; in-game `0x48e390` returns SEVEN with `enabled`
    // at slot 4 and no `url` at all. They differ in behaviour too — the in-game one passes `dl=1`,
    // so `NOT_DEMAND_LOADED` is reachable here and never from glue, and it short-circuits an
    // already-loaded addon to `1, nil` before the gate.
    //
    // We shipped the GLUE shape in the IN-GAME VM. Slots 5/6/7 aligned by luck; slot 4 did not, and
    // it is the one the ecosystem reads:
    //
    //     local name, _, _, enabled, loadable = GetAddOnInfo(major)   -- AceLibrary-1.0:400
    //
    // `## URL` is rare, so `enabled` came back nil for nearly every addon and Ace's
    // `if enabled and loadable` refused to load its own dependency. 70 corpus folders reach it
    // (AceLibrary/AceAddon replicated — one library, which is what makes it wide rather than
    // narrow). Nothing errored: a nil where a flag belongs is a legal Lua value.
    //
    // `enabled` is the number 1 or nil, never boolean `true` — every push in these functions is
    // `lua_pushnumber(1.0)` via `0x6f3810`, with no `lua_pushboolean` anywhere. Same for
    // `loadable`. `flag` already answers in that shape.
    //
    // `url` is not lost, it moves to where the client keeps it in-game: `GetAddOnMetadata(name,
    // "URL")`.
    g.set(
        "GetAddOnInfo",
        lua.create_function(|lua, key: Value| {
            let model = lua.app_data_ref::<Model>().expect("model");
            let Some(i) = resolve(&model, &key) else {
                return Ok(MultiValue::new());
            };
            // The one arbiter (decision 1292): loaded short-circuit, then `AddOn_CanLoad` in the
            // in-game flavour — so NOT_DEMAND_LOADED and INTERFACE_VERSION are reachable here,
            // exactly as `0x48e390` reports them.
            let reason = verdict(&model, i).token();
            let a = &model.addons[i];
            Ok(MultiValue::from_vec(vec![
                lua_str(lua, &a.name)?,
                opt_str(lua, &a.title)?,
                opt_str(lua, &a.notes)?,
                flag(a.enabled),
                flag(reason.is_none()),
                match &reason {
                    Some(r) => lua_str(lua, r)?,
                    None => Value::Nil,
                },
                lua_str(lua, if a.secure { "SECURE" } else { "INSECURE" })?,
            ]))
        })?,
    )?;

    g.set(
        "IsAddOnLoaded",
        lua.create_function(|lua, key: Value| {
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(flag(
                resolve(&model, &key).is_some_and(|i| model.addons[i].loaded),
            ))
        })?,
    )?;

    g.set(
        "IsAddOnLoadOnDemand",
        lua.create_function(|lua, key: Value| {
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(flag(
                resolve(&model, &key).is_some_and(|i| model.addons[i].load_on_demand),
            ))
        })?,
    )?;

    // Varargs of dependency names — the glue folds them into a tooltip line with `AddonTooltip_
    // BuildDeps(GetAddOnDependencies(id))`, so "none" is zero returns, not an empty string.
    g.set(
        "GetAddOnDependencies",
        lua.create_function(|lua, key: Value| {
            let model = lua.app_data_ref::<Model>().expect("model");
            let Some(i) = resolve(&model, &key) else {
                return Ok(MultiValue::new());
            };
            let mut out = Vec::new();
            for dep in &model.addons[i].dependencies {
                out.push(lua_str(lua, dep)?);
            }
            Ok(MultiValue::from_iter(out))
        })?,
    )?;

    // The raw `## Key: Value`, by key — how an addon reads its own `## Version`.
    g.set(
        "GetAddOnMetadata",
        lua.create_function(|lua, (key, field): (Value, String)| {
            let model = lua.app_data_ref::<Model>().expect("model");
            let Some(i) = resolve(&model, &key) else {
                return Ok(Value::Nil);
            };
            match model.addons[i]
                .directives
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(&field))
            {
                Some((_, v)) => lua_str(lua, v),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // **One argument, index or name — the `(character, index)` form is GLUE-ONLY** (byte-carved,
    // wow-re `addon-enable-store.md`: in-game `0x48e690`/`0x48e760` read arg1 alone and key the
    // store on `0x5abdc0()`, the logged-in character; the two-argument shape belongs to the glue
    // registrars `0x46d7b0`/`0x46d8a0`, and our glue screen is native — no caller exists).
    // A numeric index out of range is a **Lua error** in the reference (`luaL_error` via
    // `0x51df00`); an unknown NAME is where we diverge, disclosed: the reference creates a
    // phantom enable-hash entry for the typo, we no-op — the safer direction, and `resolve`'s
    // established semantics (1191).
    for (name, on) in [("EnableAddOn", true), ("DisableAddOn", false)] {
        g.set(
            name,
            lua.create_function(move |lua, key: Value| {
                let mut model = lua.app_data_mut::<Model>().expect("model");
                if let Value::Integer(_) | Value::Number(_) = key {
                    if resolve(&model, &key).is_none() {
                        return Err(mlua::Error::runtime(format!(
                            "{}: addon index out of range",
                            if on { "EnableAddOn" } else { "DisableAddOn" }
                        )));
                    }
                }
                if let Some(i) = resolve(&model, &key) {
                    model.addons[i].enabled = on;
                }
                Ok(())
            })?,
        )?;
    }

    // No arguments read at all (`0x48e720`/`0x48e7f0` — the loop re-evaluates the bound and the
    // current character each iteration; ours is one pass over the same store).
    for (name, on) in [("EnableAllAddOns", true), ("DisableAllAddOns", false)] {
        g.set(
            name,
            lua.create_function(move |lua, _: MultiValue| {
                let mut model = lua.app_data_mut::<Model>().expect("model");
                for a in &mut model.addons {
                    a.enabled = on;
                }
                Ok(())
            })?,
        )?;
    }

    // `ResetDisabledAddOns 0x48e830` — **revert-to-last-saved**, byte-carved (wow-re
    // `addon-enable-store.md`): the reference destroys the current character's enable hash and
    // reloads it from the on-disk `AddOns.txt` — so unsaved toggles of BOTH polarities revert,
    // and a disable already on disk stays disabled. Within one session the current character's
    // file is immutable until the shutdown tail writes it (the glue can only edit it with no
    // session up), so the registration-time `enabled` — read from that very file — IS the
    // last-saved state, and reverting to it is the same operation without re-reading disk.
    g.set(
        "ResetDisabledAddOns",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model");
            for a in &mut model.addons {
                a.enabled = a.saved_enabled;
            }
            Ok(())
        })?,
    )?;

    // loaded, reason — see `load_addon`.
    g.set(
        "LoadAddOn",
        lua.create_function(|lua, key: Value| {
            let index = {
                let model = lua.app_data_ref::<Model>().expect("model");
                resolve(&model, &key)
            };
            let Some(i) = index else {
                return Ok(MultiValue::from_vec(vec![
                    Value::Nil,
                    lua_str(lua, "MISSING")?,
                ]));
            };
            match load_addon(lua, i) {
                Ok(()) => Ok(MultiValue::from_vec(vec![Value::Integer(1), Value::Nil])),
                Err(reason) => Ok(MultiValue::from_vec(vec![
                    Value::Nil,
                    lua_str(lua, &reason)?,
                ])),
            }
        })?,
    )?;

    Ok(())
}

/// Load one addon on demand, dependencies first — `AddOn_Load 0x51f240` reached from `LoadAddOn`
/// rather than from the startup sweep.
///
/// **This runs inside a Lua binding, synchronously**, which is the whole reason the loader takes
/// `&Lua` (see `crate::loader::load_into`): the reference's `UIParentLoadAddOn` does
/// `local loaded = LoadAddOn(name)` and touches the addon's frames on the next line, so a deferred
/// load would be a different function wearing the same name.
///
/// `Err` carries the reference's own reason token.
fn load_addon(lua: &Lua, i: usize) -> Result<(), String> {
    let (name, root, files, deps, already) = {
        let model = lua.app_data_ref::<Model>().expect("model");
        let a = &model.addons[i];
        (
            a.name.clone(),
            model.addons_root.clone(),
            a.files.clone(),
            a.dependencies.clone(),
            a.loaded,
        )
    };
    if already {
        return Ok(()); // the reference answers a redundant load with success, not an error
    }
    // The one arbiter (decision 1292): the reference's own shape — `LoadAddOn 0x48e980` refuses
    // through `AddOn_CanLoad` (via `AddOn_Load`'s step 3) and re-derives the reason from the
    // same gate on failure. The version gate is in here now: an out-of-date addon demand-loads
    // only under force-load, exactly as `0x48ea8c`'s carve records.
    {
        let model = lua.app_data_ref::<Model>().expect("model");
        if let refused @ Verdict::Refused { .. } = verdict(&model, i) {
            return Err(refused.token().expect("a refusal always carries a token"));
        }
    }
    let Some(root) = root else {
        return Err("MISSING".into()); // no addon root: a hermetic capture has nothing to load
    };

    // The gate said yes, so a dependency failing HERE is a load-time failure (its files errored,
    // it was uninstalled mid-session) — still mapped to the DEP_ mirror, applied once.
    for dep in &deps {
        let d = {
            let model = lua.app_data_ref::<Model>().expect("model");
            model
                .addons
                .iter()
                .position(|a| a.name.eq_ignore_ascii_case(dep))
        };
        match d {
            None => return Err("DEP_MISSING".into()),
            Some(d) => {
                let loaded = {
                    let model = lua.app_data_ref::<Model>().expect("model");
                    model.addons[d].loaded
                };
                if !loaded {
                    load_addon(lua, d).map_err(|r| {
                        if r.starts_with("DEP_") {
                            r // §2.3: the prefix applies exactly once at any nesting depth
                        } else {
                            format!("DEP_{r}")
                        }
                    })?;
                }
            }
        }
    }

    run_files(lua, &name, &root, &files);
    // `Bindings.xml` (1188 phase 4) attaches here, between the files and the saved variables.
    load_bindings(lua, &name, &root);
    load_saved_variables(lua, i);

    {
        let mut model = lua.app_data_mut::<Model>().expect("model");
        model.addons[i].loaded = true;
    }
    // The verified position: after the files, at the end of this addon's load (`0x51f5ad`).
    super::event::fire_global(lua, "ADDON_LOADED", &[super::ScriptValue::Str(name)]);
    Ok(())
}

/// Register this addon's `Bindings.xml` into the key-binding table (decision 1188 phase 4) — the
/// demand-load twin of what the host's startup walk does at the same position.
///
/// **Position is the mechanism here too**: the reference loads it at `0x51f400`, after the
/// addon's own `.toc` files and before its saved variables. After the files, because a binding's
/// body calls the functions those files define; before the saved variables, because they are the
/// next step of the same verified sequence and `ADDON_LOADED` closes it.
///
/// A missing file is the normal case and silent — most addons declare no bindings. `Bindings.xml`
/// is the reference's own spelling and the only one probed; the sandbox is [`read_under`]'s, so
/// this reads exactly what the addon's other files may read and nothing else.
fn load_bindings(lua: &Lua, name: &str, root: &std::path::Path) {
    let path = crate::loader::join_ref(name, "Bindings.xml");
    let Some(bytes) = read_under(root, &path) else {
        return;
    };
    match crate::bindings_xml::parse(&crate::source::decode(&bytes)) {
        Ok(bindings) => super::keybind::register_addon_bindings(lua, name, &bindings),
        // Reported rather than swallowed, like a failed saved-variables chunk: a binding file that
        // stopped parsing means keys silently stop working, which is exactly what a player cannot
        // diagnose on their own.
        Err(e) => log_error(lua, &format!("{name}/Bindings.xml: {e}")),
    }
}

/// Execute this addon's saved-variables files — **account first, per-character second**, which is
/// the reference's order (`0x51f4b5` then `0x51f53b`) and therefore why a per-character value wins.
///
/// Each file is *executed as a Lua chunk in the shared global state*, not parsed: the reference
/// does exactly that (`0x704bc0` names the chunk, `0x704ae0` runs `luaL_loadbuffer` +
/// `lua_pcall`), with no `setfenv` and no restricted environment. A missing file is a silent
/// no-op (`0x51f4a9`, `0x51f530`) — it is the normal first-run case.
///
/// **Position is the mechanism**: this runs after the addon's own files (which assign the
/// file-scope defaults) and before `ADDON_LOADED` (whose handlers are specified to see the
/// restored value). Reverse either and the saved value can never win.
fn load_saved_variables(lua: &Lua, i: usize) {
    let (name, account, character, has_account, has_character) = {
        let model = lua.app_data_ref::<Model>().expect("model");
        let a = &model.addons[i];
        (
            a.name.clone(),
            model.addons_saved_account.clone(),
            model.addons_saved_character.clone(),
            !a.saved_variables.is_empty(),
            !a.saved_variables_per_character.is_empty(),
        )
    };
    for (dir, declared) in [(account, has_account), (character, has_character)] {
        if !declared {
            continue; // an addon that declares nothing has no file, even if one is lying there
        }
        let Some(dir) = dir else { continue };
        let path = dir.join(format!("{name}.lua"));
        let Ok(bytes) = std::fs::read(&path) else {
            continue; // absent is the first-run case
        };
        if let Err(e) = run_chunk(
            lua,
            &bytes,
            &crate::script::addon_chunk_name(&name.to_string(), &format!("{name}.lua")),
        ) {
            // The reference fails this silently. We do not: a settings file that stopped parsing
            // is exactly the thing a player needs told, and the file is left on disk untouched.
            log_error(lua, &format!("{}: {e}", path.display()));
        }
    }
}

/// Run one addon's `.toc`-listed files, in listed order — the demand-load twin of the host's
/// `Addon::load_files`, and deliberately the same rules: `.lua` is a chunk, anything else is
/// FrameXML, and every reference resolves against the *including file's* directory (1186) inside
/// the AddOns root as the sandbox.
fn run_files(lua: &Lua, name: &str, root: &std::path::Path, files: &[String]) {
    let provider = |req: &str| -> Option<Vec<u8>> { read_under(root, req) };
    for file in files {
        let path = crate::loader::join_ref(name, file);
        let Some(bytes) = read_under(root, &path) else {
            log_error(lua, &format!("{name}/{file}: not found"));
            continue;
        };
        if std::path::Path::new(file)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("lua"))
        {
            if let Err(e) = run_chunk(lua, &bytes, &crate::script::addon_chunk_name(name, file)) {
                log_error(lua, &format!("{name}/{file}: {e}"));
            }
            continue;
        }
        let doc = match crate::framexml::parse(&crate::source::decode(&bytes)) {
            Ok(d) => d,
            Err(e) => {
                log_error(lua, &format!("{name}/{file}: {e}"));
                continue;
            }
        };
        let report = crate::loader::load_into(lua, &doc, &path, &provider);
        for e in report.errors {
            log_error(lua, &format!("{name}/{file}: {e}"));
        }
    }
}

/// `root/rel`, refusing to escape `root` — the AddOns-root sandbox (1186), lexical and applied
/// before any filesystem call. `join_ref` has already resolved the path, so an escape survives as
/// a leading `..` and is what the `Normal`-component test rejects.
fn read_under(root: &std::path::Path, rel: &str) -> Option<Vec<u8>> {
    let rel = std::path::Path::new(rel);
    if rel
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    std::fs::read(root.join(rel)).ok()
}

/// Run a chunk that came off disk, from `&Lua` — [`crate::script::UiScript::run_chunk`]'s body for
/// the demand-load path, which never holds a `UiScript` (1191 §4). Bytes and a BOM strip, for the
/// reasons in [`crate::source`].
fn run_chunk(lua: &Lua, bytes: &[u8], name: &str) -> mlua::Result<()> {
    lua.load(crate::source::chunk(bytes))
        .set_name(name)
        .set_mode(mlua::ChunkMode::Text)
        .exec()
}

/// Record a load error where the host drains it — the same channel a handler error uses, so a
/// demand-load failure surfaces the way every other script error does rather than vanishing.
fn log_error(lua: &Lua, msg: &str) {
    lua.app_data_mut::<Model>()
        .expect("model")
        .errors
        .push(format!("LoadAddOn: {msg}"));
}

/// The host's push: the discovered registry and the root its files live under.
impl super::UiScript {
    /// Replace the AddOn registry — called once at world entry, after discovery
    /// ([`crate::script`]'s `register_bindings` is the same shape: the host owns the facts, the
    /// engine owns the verbs).
    pub fn register_addons(
        &mut self,
        mut addons: Vec<AddOnInfo>,
        root: Option<PathBuf>,
        saved_account: Option<PathBuf>,
        saved_character: Option<PathBuf>,
    ) {
        // The registration-time enable state IS the last-saved state (the host read it off the
        // enable file moments ago) — snapshot it here so `ResetDisabledAddOns` has its revert
        // point and no caller has to remember to provide one.
        for a in &mut addons {
            a.saved_enabled = a.enabled;
        }
        let mut model = self.model_mut();
        model.addons = addons;
        model.addons_root = root;
        model.addons_saved_account = saved_account;
        model.addons_saved_character = saved_character;
    }

    /// Execute one addon's saved-variables files, at the startup walk's verified position — the
    /// twin of what [`load_saved_variables`] does inside `LoadAddOn`, so both halves restore state
    /// the same way.
    pub fn load_addon_saved_variables(&mut self, name: &str) {
        let i = self
            .model_ref()
            .addons
            .iter()
            .position(|a| a.name.eq_ignore_ascii_case(name));
        if let Some(i) = i {
            load_saved_variables(self.lua(), i);
        }
    }

    /// Every **loaded** addon that declares saved variables, as
    /// `(name, account-scoped names, per-character names)` — what the host writes at shutdown.
    ///
    /// The gate is *loaded*, not a dirty bit: the reference gates its write on the record's loaded
    /// byte (`0x51f711`) and has no dirty tracking at all. An addon that never loaded this session
    /// has no globals to write, and writing it would blank the file it never read.
    pub fn addon_saved_variable_sets(&self) -> Vec<(String, Vec<String>, Vec<String>)> {
        self.model_ref()
            .addons
            .iter()
            .filter(|a| a.loaded)
            .filter(|a| {
                !a.saved_variables.is_empty() || !a.saved_variables_per_character.is_empty()
            })
            .map(|a| {
                (
                    a.name.clone(),
                    a.saved_variables.clone(),
                    a.saved_variables_per_character.clone(),
                )
            })
            .collect()
    }

    /// Mark an addon loaded — the host's startup walk reporting what it ran, so `IsAddOnLoaded`
    /// answers for the startup half exactly as it does for the `LoadAddOn` half.
    pub fn mark_addon_loaded(&mut self, name: &str) {
        if let Some(a) = self
            .model_mut()
            .addons
            .iter_mut()
            .find(|a| a.name.eq_ignore_ascii_case(name))
        {
            a.loaded = true;
        }
    }

    /// `(name, enabled)` for every registered addon, in registry order — what the host writes back
    /// to `AddOns.txt`, so a `DisableAddOn` from Lua survives the session.
    pub fn addon_enable_states(&self) -> Vec<(String, bool)> {
        self.model_ref()
            .addons
            .iter()
            .map(|a| (a.name.clone(), a.enabled))
            .collect()
    }
}
