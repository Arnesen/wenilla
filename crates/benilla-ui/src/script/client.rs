//! **Client-identity and script-evaluation verbs** — the small engine globals an addon asks for
//! before it does anything else (1188 phase 5, prioritised by the phase 6 corpus).
//!
//! These are not a guess at what addons want. They are the top of the measured demand list: over a
//! 218-addon vanilla corpus, `GetBuildInfo` is called by 69 addons and `RunScript` by 52, and both
//! are called *at file scope* — so a missing one does not degrade an addon, it stops the addon's
//! very first chunk with `attempt to call global … (a nil value)`. `GetBuildInfo` alone was the
//! first error for the whole Ace/Atlas/AtlasLoot family, which is a large slice of the ecosystem.
//!
//! ## The host-fed pair (decision 1195)
//!
//! `GetRealmName` and `GetFramerate` are the next two down that list — 24 addons are stopped dead
//! by `GetRealmName` at file scope, and it is the **top runtime wall in the corpus** after the
//! dialect gap. Both read a slot the app pushes, the same shape as the zone-text family: the
//! engine owns the verb, the host owns the fact.
//!
//! `GetRealmName` returning `""` before a realm is known is deliberate and is what the reference
//! does at the glue screen. An addon keys its saved variables on it (`db[GetRealmName()]`), so
//! answering `nil` would make that a `table index is nil` error one call deeper — the failure mode
//! we have been paying for all arc.

use mlua::{Lua, MultiValue, Value};

use super::Model;

/// The 1.12.1 client's own build identity, read out of `WoW.exe`'s string table rather than
/// remembered: `5875` and `1.12.1` sit adjacent in the binary, and `Sep 19 2006` is its only
/// build-date string.
///
/// **Hardcoding is the faithful answer here, not a shortcut.** benilla targets exactly 1.12.1
/// (decision 1188), so these are constants of the target, not of our build — an addon asking
/// `GetBuildInfo()` is asking "which client am I on", and the honest answer is the one we
/// implement the API of. Our *own* build stamp is a different question with a different verb
/// ([`crate::script`] has no binding for it, and the reference has none either).
const VERSION: &str = "1.12.1";
const BUILD: &str = "5875";
const BUILD_DATE: &str = "Sep 19 2006";
/// The `## Interface` number a 1.12 `.toc` declares — and what `benilla.toc` declares too.
const TOC_VERSION: i64 = 11200;

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // version, build, date, tocversion
    g.set(
        "GetBuildInfo",
        lua.create_function(|lua, ()| {
            Ok(MultiValue::from_vec(vec![
                Value::String(lua.create_string(VERSION)?),
                Value::String(lua.create_string(BUILD)?),
                Value::String(lua.create_string(BUILD_DATE)?),
                Value::Integer(TOC_VERSION),
            ]))
        })?,
    )?;

    // `RunScript(text)` — compile and run a chunk in the shared global state.
    //
    // The reference's is `RunScript 0x7044c0`, and it is how a macro body, a `/script` slash
    // command, and every addon's "evaluate this snippet" helper reach Lua. It is the same
    // `loadstring`+call our own chunk loader does, so it inherits the same sandbox: there is no
    // `setfenv` here and none in the reference either — a script runs with full API access.
    //
    // A compile or runtime error is **raised**, not swallowed. The reference propagates it to the
    // caller's error handler, which is what puts a red line in the chat frame; returning nil would
    // make a broken macro look like a working one that did nothing.
    g.set(
        "RunScript",
        lua.create_function(|lua, text: String| {
            lua.load(&text)
                .set_name("=[RunScript]")
                .set_mode(mlua::ChunkMode::Text)
                .exec()
        })?,
    )?;

    // `GetRealmName()` — the realm this session is on, as the realm list spells it.
    //
    // The corpus's most-wanted engine verb after the dialect fix: 24 addons stop on it, and they
    // stop *at file scope*, because the idiom is `MyAddonDB[GetRealmName()] = …` in a chunk's
    // opening lines. `""` until the app pushes one (the glue screen's own answer), never `nil`.
    g.set(
        "GetRealmName",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            lua.create_string(&model.realm_name)
        })?,
    )?;

    // `GetFramerate()` — frames per second, as a number.
    //
    // 71 addons in the corpus call it; every performance readout on the ecosystem's FuBar/Titan
    // panels is this one verb. The app pushes a smoothed value each tick
    // ([`super::UiScript::set_framerate`]); 0 before the first push, which is what an addon's
    // `format("%.1f", GetFramerate())` needs to not error.
    g.set(
        "GetFramerate",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.framerate)
        })?,
    )?;

    Ok(())
}

impl super::UiScript {
    /// Push the realm name — the host's half of `GetRealmName` (decision 1195).
    ///
    /// Set at world entry from the realm the session actually connected to. Idempotent, and the
    /// empty string is a legitimate value (no realm yet), not a "clear".
    pub fn set_realm_name(&mut self, realm: &str) {
        {
            let mut model = self.model_mut();
            if model.realm_name != realm {
                model.realm_name = realm.to_string();
            }
            // The `realmName` CVar is the SAME fact, and it is set here so it cannot drift from
            // `GetRealmName()`. It is a real 1.12 CVar (`0x83f2d0`, persisted — the client builds
            // its SavedVariables path from it, wow-re `savedvariables-protocol.md`), and it had no
            // value at all here: `Ace/AceState.lua:27` is
            // `ace.trim(GetCVar("realmName"))` inside `SetGameState`, which EVERY Ace addon runs at
            // PLAYER_ENTERING_WORLD, so the nil became `gsub(nil, ...)` and took the family down.
            //
            // Written straight into the slot rather than through `set_cvar_host` because that
            // borrows the model again, and warns on an unknown name — this is the host declaring
            // the value, not looking it up.
            if let Some(slot) = model.cvars.get_mut("realmname") {
                slot.value = realm.to_string();
            }
        }
    }

    /// Push the current framerate — the host's half of `GetFramerate`.
    ///
    /// Pushed per tick from the app's own frame clock rather than computed here: this crate has no
    /// clock, and a number an addon polls every `OnUpdate` should not be re-derived per call.
    pub fn set_framerate(&mut self, fps: f64) {
        self.model_mut().framerate = fps;
    }
}

#[cfg(test)]
mod tests {
    use crate::script::UiScript;

    /// `GetBuildInfo` answers as the 1.12.1 client, in the reference's 4-value shape.
    ///
    /// The shape is what addons destructure (`local version, build, date, toc = GetBuildInfo()`),
    /// and the `tocversion` is the one they compare against their own `## Interface` to decide
    /// whether to run at all — so a wrong or missing fourth value makes an addon disable itself.
    #[test]
    fn get_build_info_answers_as_the_1_12_1_client() {
        let s = UiScript::new().unwrap();
        assert_eq!(
            s.eval::<Vec<String>>(
                "local v, b, d, t = GetBuildInfo() return { v, b, d, tostring(t) }"
            )
            .unwrap(),
            vec!["1.12.1", "5875", "Sep 19 2006", "11200"]
        );
    }

    /// `GetRealmName` answers `""` before the host pushes one — **never `nil`**, because the
    /// corpus idiom is `MyAddonDB[GetRealmName()] = …` at file scope and a nil index errors one
    /// call deeper, far from the cause.
    #[test]
    fn get_realm_name_is_empty_before_it_is_known_and_the_host_pushes_it() {
        let mut s = UiScript::new().unwrap();
        assert_eq!(s.eval::<String>("return GetRealmName()").unwrap(), "");
        assert!(
            s.eval::<bool>("local t = {} t[GetRealmName()] = 1 return t[''] == 1")
                .unwrap(),
            "the empty string is a usable table key; nil is not, and that is the whole point"
        );
        s.set_realm_name("Whitemane");
        assert_eq!(
            s.eval::<String>("return GetRealmName()").unwrap(),
            "Whitemane"
        );
    }

    /// `GetFramerate` is a number from the first call, so `format("%.1f", GetFramerate())` in an
    /// addon's `OnUpdate` cannot error before the app has pushed a frame.
    #[test]
    fn get_framerate_is_a_number_before_the_first_push() {
        let mut s = UiScript::new().unwrap();
        assert_eq!(s.eval::<f64>("return GetFramerate()").unwrap(), 0.0);
        assert_eq!(
            s.eval::<String>("return format(\"%.1f\", GetFramerate())")
                .unwrap(),
            "0.0"
        );
        s.set_framerate(59.94);
        assert_eq!(
            s.eval::<String>("return format(\"%.1f\", GetFramerate())")
                .unwrap(),
            "59.9"
        );
    }

    /// `RunScript` runs its text in the shared global state, and an error in it is raised.
    #[test]
    fn run_script_evaluates_in_the_shared_state_and_raises() {
        let s = UiScript::new().unwrap();
        s.run(r#"RunScript("RunScriptProbe = 41 + 1")"#).unwrap();
        assert_eq!(s.eval::<i64>("return RunScriptProbe").unwrap(), 42);
        // A script that fails must fail loudly — a silent nil is a broken macro that looks fine.
        assert!(
            s.run(r#"RunScript("this is not lua")"#).is_err(),
            "a malformed script must raise, not vanish"
        );
    }
}
