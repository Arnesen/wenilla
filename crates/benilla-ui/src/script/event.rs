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

use std::borrow::Cow;

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

/// Drain [`Model::pending_size_changed`] and fire `OnSizeChanged(self, width, height)` for each.
///
/// The resolve pass ([`super::UiScript::resolve_layout`]) only ever holds a `&mut Model` — three of
/// its callers reach it that way — so it *queues* the frames whose resolved size moved and this
/// runs at the next `&Lua` seam. Errors are recorded, never propagated: a handler blowing up must
/// not abort a layout pass.
///
/// **Drains exactly once, deliberately.** A handler is free to resize things, `Show()` something,
/// or call `UpdateScrollChildRect` — each of which can re-enter the resolver and queue *more*
/// entries. Looping until the queue empties would let a handler that grows its own frame spin the
/// engine forever inside one call; taking one batch instead leaves the next batch for the next
/// resolve, which is exactly the reference's shape (its `ApplyRect` fires, the handler dirties the
/// layout, and the *next* layout pass fires again). A genuinely oscillating handler oscillates at
/// one fire per frame, visibly, instead of hanging the client.
pub(super) fn fire_size_changes(lua: &Lua) {
    let pending = std::mem::take(
        &mut lua
            .app_data_mut::<Model>()
            .expect("model")
            .pending_size_changed,
    );
    for (id, w, h) in pending {
        if let Err(e) = fire(
            lua,
            id,
            "OnSizeChanged",
            None,
            vec![Value::Number(f64::from(w)), Value::Number(f64::from(h))],
        ) {
            lua.app_data_mut::<Model>()
                .expect("model")
                .errors
                .push(e.to_string());
        }
    }
}

/// Fire `OnShow`/`OnHide` for a set of frames whose effective visibility just changed (from
/// [`crate::widget::WidgetArena::set_shown`]/`set_parent`'s changed-list). Errors are recorded in
/// [`Model::errors`] rather than propagated — a handler error must not abort the `Show()` call.
///
/// **This is also where the `toplevel` raise fires** (`effective_visible_show 0x76ae10` @`0x76aee0`,
/// wow-re `ui/scratch/toplevel-raise.md`): the binary tests the toplevel bit and raises *after* the
/// subtree's visibility has propagated and *before* that node's OnShow notify — which is exactly
/// this seam, since the arena has finished propagating by the time it hands back the changed list.
/// Per node and in list order, so a handler reading `GetFrameLevel()` sees the raised value the way
/// it would in the reference. It lives here rather than at the Lua `Show` binding because *every*
/// visibility transition this engine performs runs `0x76ae10` — an arena-level show from the
/// tooltip path (or a future host one) must raise too, and a second seam is how that gets forgotten.
pub(super) fn fire_visibility_changes(lua: &Lua, changed: Vec<FrameHandle>) {
    // Resolve (handle, id, now-visible?) under one short borrow, then fire with no borrow held.
    let items: Vec<(FrameHandle, u32, bool)> = {
        let mut model = lua.app_data_mut::<Model>().expect("model");
        changed
            .into_iter()
            .filter_map(|h| {
                let vis = model.arena.frame(h)?.effective_visible;
                Some((h, model.frame_id(h), vis))
            })
            .collect()
    };
    for (h, id, visible) in items {
        if visible {
            let mut model = lua.app_data_mut::<Model>().expect("model");
            super::object::toplevel::raise_on_show(&mut model, h);
        }
        let name = if visible { "OnShow" } else { "OnHide" };
        if let Err(e) = fire(lua, id, name, None, Vec::new()) {
            lua.app_data_mut::<Model>()
                .expect("model")
                .record_script_error(e.to_string());
        }
    }
}

/// Is a handler bound under `script` for this frame id? — the **existence** question, asked
/// without firing anything.
///
/// It exists for the keyboard walk ([`super::keyboard`]), where the reference's consumption gate is
/// the presence of the slot rather than anything the handler does: `0x76b7d0` reads `[+0x188]`/
/// `[+0x190]`, consumes on either, and fires only the first. A lookup failure reads as "absent" —
/// the gate must never be able to raise out of the middle of a walk.
pub(super) fn has_widget_handler(lua: &Lua, id: u32, script: &str) -> bool {
    let Ok(scripts) = lua.named_registry_value::<Table>(REG_SCRIPTS) else {
        return false;
    };
    match scripts.get::<Value>(id) {
        Ok(Value::Table(per)) => matches!(per.get::<Value>(script), Ok(Value::Function(_))),
        _ => false,
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

/// The legacy `argN` global names, pre-spelled through the arities that occur in practice so the
/// per-arg save/set/restore below allocates nothing.
const ARG_NAMES: [&str; 16] = [
    "arg1", "arg2", "arg3", "arg4", "arg5", "arg6", "arg7", "arg8", "arg9", "arg10", "arg11",
    "arg12", "arg13", "arg14", "arg15", "arg16",
];

/// The global name for arg `i` — **1-based**, matching the globals themselves (`arg1..`). Past
/// [`ARG_NAMES`] it falls back to allocating.
fn arg_name(i: usize) -> Cow<'static, str> {
    match ARG_NAMES.get(i - 1) {
        Some(&name) => Cow::Borrowed(name),
        None => Cow::Owned(format!("arg{i}")),
    }
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
        saved_args.push(g.get::<Value>(arg_name(i).as_ref())?);
    }

    g.set("this", wrapper.clone())?;
    if let Some(ev) = event_name {
        g.set("event", lua.create_string(ev)?)?;
    }
    for (i, v) in extra.iter().enumerate() {
        g.set(arg_name(i + 1).as_ref(), v.clone())?;
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
        g.set(arg_name(i + 1).as_ref(), v)?;
    }

    outcome
}
