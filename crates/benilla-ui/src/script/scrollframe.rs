//! The `ScrollFrame` method surface (`CSimpleScrollFrame`) — the ScrollFrame mechanism (decision
//! 0112, the engine's last structural gap): `SetScrollChild`/`GetScrollChild`, the vertical scroll
//! offset (`SetVerticalScroll`/`GetVerticalScroll`), and the live range
//! (`GetVerticalScrollRange`/`UpdateScrollChildRect`). Spec-faithful to the documented widget
//! contract (the Era `ScrollFrameTemplate` Lua drives its scrollbar off exactly these), not
//! byte-pinned — same posture as StatusBar's fill. Horizontal scroll (`SetHorizontalScroll`/…) is
//! out of scope: no 1.12 template drives it.
//!
//! The actual geometry — the scroll child's anchors pinned to the frame + the scroll offset, and
//! the clip every descendant of the child draws/hits within — lives in [`super::mod@super`]'s
//! `resolve`/`extract`/`hit_test` (the layout-graph override + the ancestor-walk clip helper); this
//! module is the thin Lua binding over the per-frame [`ScrollFrameState`], plus the two script
//! hooks (`OnVerticalScroll`/`OnScrollRangeChanged`) a scrollbar's `OnValueChanged`/`OnLoad` wires to.
//!
//! The methods live in their own registry table, consulted by the frame `__index` dispatcher only
//! for ScrollFrame frames — so duck-typing addons (`if frame.SetScrollChild then …`) see `nil` on
//! every other kind, exactly as against the client's per-class method sets.

use mlua::{Lua, Table, Value};

use super::object::{decode_id, frame_handle_of, frame_wrapper};
use super::{event, Model};
use crate::widget::{FrameHandle, KindState, ScrollFrameState};

/// Registry key of the ScrollFrame method table (the MAXCSTACK discipline: Lua-side root, named key).
pub(super) const REG_SCROLLFRAME_METHODS: &str = "__benilla_scrollframe_methods";

/// Run `f` over a frame's ScrollFrame state under one short write borrow. Errors if `this` is not a
/// live ScrollFrame (unreachable through the kind dispatcher, but the method table is a plain Lua
/// value — a caller could fish it out and misapply it).
fn with_scroll<T>(
    lua: &Lua,
    this: &Table,
    f: impl FnOnce(&mut ScrollFrameState) -> T,
) -> mlua::Result<T> {
    let h = frame_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let frame = model
        .arena
        .frame_mut(h)
        .ok_or_else(|| mlua::Error::runtime("stale frame handle"))?;
    match &mut frame.kind_state {
        KindState::Scroll(s) => Ok(f(s)),
        _ => Err(mlua::Error::runtime("not a ScrollFrame")),
    }
}

/// `GetVerticalScrollRange()`'s live computation: `max(0, child_height − frame_height)` from the
/// **resolved** rects — `0.0` when either is unresolved or there is no child. Never cached: the
/// scroll offset always clamps against the current layout, not a stale snapshot.
fn scroll_range(model: &Model, h: FrameHandle) -> f32 {
    let child = match model.arena.frame(h).map(|f| &f.kind_state) {
        Some(KindState::Scroll(s)) => s.child,
        _ => None,
    };
    let Some(child) = child else { return 0.0 };
    let frame_h = model.resolved.get(&h).map(|r| r.height());
    let child_h = model.resolved.get(&child).map(|r| r.height());
    match (frame_h, child_h) {
        (Some(fh), Some(ch)) => (ch - fh).max(0.0),
        _ => 0.0,
    }
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;

    // SetScrollChild(frame|name|nil) — a wrapper table or a name string resolves like every other
    // frame-target arg (SetParent, SetPoint's relativeTo); nil (or an unresolvable target) clears.
    m.set(
        "SetScrollChild",
        lua.create_function(|lua, (this, target): (Table, Value)| {
            let new_child: Option<FrameHandle> = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                match &target {
                    Value::Table(t) => decode_id(t)
                        .ok()
                        .and_then(|id| model.id_to_frame.get(&id).copied()),
                    Value::String(s) => {
                        s.to_str().ok().and_then(|n| model.arena.lookup(n.as_ref()))
                    }
                    _ => None,
                }
            };
            with_scroll(lua, &this, |s| s.child = new_child)
        })?,
    )?;
    m.set(
        "GetScrollChild",
        lua.create_function(|lua, this: Table| {
            let child = with_scroll(lua, &this, |s| s.child)?;
            let id = {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                child
                    .filter(|&h| model.arena.frame(h).is_some())
                    .map(|h| model.frame_id(h))
            };
            match id {
                Some(id) => Ok(Value::Table(frame_wrapper(lua, id)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // SetVerticalScroll(px) — clamps into [0, GetVerticalScrollRange()] (computed live), stores, and
    // fires OnVerticalScroll(self, offset) — the scrollbar's OnValueChanged wiring's other half.
    m.set(
        "SetVerticalScroll",
        lua.create_function(|lua, (this, px): (Table, f32)| {
            let h = frame_handle_of(lua, &this)?;
            let range = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                scroll_range(&model, h)
            };
            let clamped = px.clamp(0.0, range);
            with_scroll(lua, &this, |s| s.vertical = clamped)?;
            fire_vertical_scroll(lua, &this, clamped)
        })?,
    )?;
    m.set(
        "GetVerticalScroll",
        lua.create_function(|lua, this: Table| with_scroll(lua, &this, |s| s.vertical))?,
    )?;
    m.set(
        "GetVerticalScrollRange",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(scroll_range(&model, h))
        })?,
    )?;
    // UpdateScrollChildRect() — the Era template calls this after resizing its content to re-drive
    // its scrollbar. Our range is always live (never cached) against the RESOLVED rects — but a
    // script resize (`child:SetHeight(...)`) lands in `LayoutInput` immediately, while `resolved`
    // only catches up on the app's own next `resolve()` call (Lua runs before that each tick,
    // `ui_script/mod.rs`'s drive loop). This is precisely the Era idiom this method exists to
    // serve — resize the content, then immediately ask for the new range — so a stale `resolved`
    // would make its very own primary caller see a one-tick-old (often wrong-by-a-lot, e.g. still
    // "nothing to scroll") answer. Force a fresh resolve first: cheap on a quiet graph (the
    // fixpoint's own doc — an unrelated resize elsewhere converges in one round), and correctness
    // here matters more than dodging a resolve pass a caller who wants nothing stale already asked
    // for by name.
    m.set(
        "UpdateScrollChildRect",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let (id, range) = {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                super::UiScript::resolve_layout(&mut model);
                let range = scroll_range(&model, h);
                (model.frame_id(h), range)
            };
            if let Err(e) = event::fire_widget_handler(
                lua,
                id,
                "OnScrollRangeChanged",
                vec![Value::Number(0.0), Value::Number(f64::from(range))],
            ) {
                lua.app_data_mut::<Model>()
                    .expect("model app_data")
                    .errors
                    .push(e.to_string());
            }
            Ok(())
        })?,
    )?;

    lua.set_named_registry_value(REG_SCROLLFRAME_METHODS, m)?;
    Ok(())
}

/// Fire `OnVerticalScroll(self, offset)` (the ScrollFrame's own script slot). Fired outside any
/// model borrow; errors go to [`Model::errors`] like every other widget handler.
fn fire_vertical_scroll(lua: &Lua, this: &Table, value: f32) -> mlua::Result<()> {
    let id = {
        let h = frame_handle_of(lua, this)?;
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        model.frame_id(h)
    };
    if let Err(e) = event::fire_widget_handler(
        lua,
        id,
        "OnVerticalScroll",
        vec![Value::Number(f64::from(value))],
    ) {
        lua.app_data_mut::<Model>()
            .expect("model app_data")
            .errors
            .push(e.to_string());
    }
    Ok(())
}
