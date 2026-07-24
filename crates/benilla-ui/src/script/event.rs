//! Handler firing — the FrameScript calling convention (RF-0025), for events, ticks, and
//! show/hide transitions.
//!
//! Every handler is invoked through the *same* protected path, which reproduces both calling
//! conventions the transition-era client supported (decision 0068):
//!
//! - **Legacy globals (RF-0025, byte-verified):** `this` = the firing frame's wrapper, `event` = the
//!   event name (OnEvent only), `arg1..argN` = the args — each **set then restored** around the call
//!   (`luaL_ref` saves the prior value; restored after), so nested fires are safe. The handler reads
//!   its inputs as globals; the client `pcall`s with **nargs = 0**.
//! - **Modern args:** the same values are *also* passed positionally as `(self, event, ...)` for
//!   OnEvent (`(self, elapsed)` for OnUpdate, `(self)` for OnShow/OnHide/OnLoad) — the form Era
//!   addons are written against.
//!
//! So inside one OnEvent handler `this == self` and `arg1 == select(1, ...)`. Handler errors are
//! caught (mlua's `Function::call` is a protected call) and returned to the caller, which records
//! them in [`super::Model::errors`] — never a panic, never a print.

use mlua::{Function, Lua, MultiValue, Table, Value};

use super::{Model, ScriptValue, REG_SCRIPTS};
use crate::script::object::frame_wrapper;
use crate::widget::FrameHandle;

/// Fire `event` at every frame registered for it (the engine-internal twin of
/// `UiScript::fire_event`, for engine code holding only the Lua context — the compare drive's
/// `SHOW_COMPARE_TOOLTIP`), in registration order with per-step live re-reads (the FIFO law —
/// see `fire_event`'s doc). Handler errors land in [`Model::errors`].
pub(super) fn fire_global(lua: &Lua, event: &str, args: &[ScriptValue]) {
    let mut i = 0;
    loop {
        let id = {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let Some(&h) = model.event_to_frames.get(event).and_then(|l| l.get(i)) else {
                break;
            };
            model.frame_id(h)
        };
        if let Err(e) = fire_event_handler(lua, id, event, args) {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .errors
                .push(e.to_string());
        }
        i += 1;
    }
}

/// Fire a frame's `OnEvent` (both conventions) with the given args.
pub(super) fn fire_event_handler(
    lua: &Lua,
    id: u32,
    event: &str,
    args: &[ScriptValue],
) -> mlua::Result<()> {
    let extra: Vec<Value> = args
        .iter()
        .cloned()
        .map(|a| a.into_lua(lua))
        .collect::<mlua::Result<_>>()?;
    fire(lua, id, "OnEvent", Some(event), extra)
}

/// Fire a frame's `OnUpdate` (RF-0025: `this` + `arg1 = elapsed`; modern `(self, elapsed)`).
pub(super) fn fire_update_handler(lua: &Lua, id: u32, elapsed: f32) -> mlua::Result<()> {
    fire(
        lua,
        id,
        "OnUpdate",
        None,
        vec![Value::Number(f64::from(elapsed))],
    )
}

/// Fire a plain widget handler — one that carries no `event` name: the mouse set
/// (`OnEnter`/`OnLeave(self, motion)`, `OnMouseDown`/`OnMouseUp(self, button)`,
/// `OnClick(self, button, down)`, `OnMouseWheel(self, delta)`) and the per-kind slots
/// (`OnValueChanged(self, value)`). `extra` is the handler's arguments (the `arg1..argN` legacy
/// globals plus the trailing modern positionals after `self`). Reuses the one [`fire`] path (so the
/// wrapper lookup + set/restore convention stay in a single home); a no-op if no such script is set.
pub(super) fn fire_widget_handler(
    lua: &Lua,
    id: u32,
    script: &str,
    extra: Vec<Value>,
) -> mlua::Result<()> {
    fire(lua, id, script, None, extra)
}

/// Fire `OnShow`/`OnHide` for a set of frames whose effective visibility just changed (from
/// [`crate::widget::WidgetArena::set_shown`]/`set_parent`'s changed-list). Errors are recorded in
/// [`Model::errors`] rather than propagated — a handler error must not abort the `Show()` call.
pub(super) fn fire_visibility_changes(lua: &Lua, changed: Vec<FrameHandle>) {
    // Resolve (id, now-visible?) under one short borrow, then fire with no borrow held.
    let items: Vec<(u32, bool)> = {
        let mut model = lua.app_data_mut::<Model>().expect("model");
        changed
            .into_iter()
            .filter_map(|h| {
                let vis = model.arena.frame(h)?.effective_visible;
                Some((model.frame_id(h), vis))
            })
            .collect()
    };
    for (id, visible) in items {
        let name = if visible { "OnShow" } else { "OnHide" };
        if let Err(e) = fire(lua, id, name, None, Vec::new()) {
            lua.app_data_mut::<Model>()
                .expect("model")
                .errors
                .push(e.to_string());
        }
    }
}

/// The one firing path. `event_name` is the `event` global + first modern positional (OnEvent only);
/// `extra` are the `arg1..argN` globals + the trailing modern positionals. Globals are saved before
/// and restored after (even on error) — nesting-safe. Holds no model borrow across the call.
fn fire(
    lua: &Lua,
    id: u32,
    script: &str,
    event_name: Option<&str>,
    extra: Vec<Value>,
) -> mlua::Result<()> {
    // Look up the handler (Lua-side, transient handle). Absent ⇒ nothing to do.
    let scripts: Table = lua.named_registry_value(REG_SCRIPTS)?;
    let func: Function = match scripts.get::<Value>(id)? {
        Value::Table(per) => match per.get::<Value>(script)? {
            Value::Function(f) => f,
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };

    let wrapper = frame_wrapper(lua, id)?;
    invoke_with_globals(lua, wrapper, &func, event_name, extra)
}

/// The RF-0025 calling convention itself — the single home for it, shared by [`fire`] (registry
/// handlers: events, OnUpdate, OnShow/OnHide) and the loader's bottom-up `OnLoad` (which holds the
/// compiled `Function` directly). Sets the legacy `this`/`event`/`arg1..argN` globals and passes the
/// modern `(self[, event], extra…)` positionals, saving and restoring the globals around the call
/// (even on error) so nested handler firing is safe.
pub(crate) fn invoke_with_globals(
    lua: &Lua,
    wrapper: Table,
    func: &Function,
    event_name: Option<&str>,
    extra: Vec<Value>,
) -> mlua::Result<()> {
    let g = lua.globals();

    let saved_this: Value = g.get("this")?;
    let saved_event: Value = g.get("event")?;
    let n = extra.len();
    let mut saved_args: Vec<Value> = Vec::with_capacity(n);
    for i in 1..=n {
        saved_args.push(g.get::<Value>(format!("arg{i}"))?);
    }

    g.set("this", wrapper.clone())?;
    if let Some(ev) = event_name {
        g.set("event", lua.create_string(ev)?)?;
    }
    for (i, v) in extra.iter().enumerate() {
        g.set(format!("arg{}", i + 1), v.clone())?;
    }

    let mut modern: Vec<Value> = Vec::with_capacity(2 + n);
    modern.push(Value::Table(wrapper));
    if let Some(ev) = event_name {
        modern.push(Value::String(lua.create_string(ev)?));
    }
    modern.extend(extra.iter().cloned());

    // Protected call. Capture the outcome, but restore globals first regardless.
    let outcome = func.call::<()>(MultiValue::from_vec(modern));

    g.set("this", saved_this)?;
    g.set("event", saved_event)?;
    for (i, v) in saved_args.into_iter().enumerate() {
        g.set(format!("arg{}", i + 1), v)?;
    }

    outcome
}
