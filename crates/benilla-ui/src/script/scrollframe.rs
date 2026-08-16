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

/// `GetVerticalScrollRange()`'s live computation: `max(0, contentExtent − frameHeight)` from the
/// **resolved** rects — `0.0` when the frame is unresolved or there is no child. Never cached: the
/// scroll offset always clamps against the current layout, not a stale snapshot.
///
/// **The content extent is the scroll child's whole SUBTREE, not the child frame's own height**
/// (wow-re `system/ui/scratch/simplehtml-markup-engine.md` §4.5, byte-identified during the
/// SimpleHTML RE; decision 1338 corrects what 0112 shipped). `0x786e30` seeds a bounding box
/// `{FLT_MAX, FLT_MAX, 0, 0}` and calls the **recursive** `0x786f80(scrollChild, &bbox)`, which
/// unions the frame's REGION list (head `+0x1b4`, link `+0x1b8`, guard `[entry+8]+0xc4`) and
/// re-enters itself for every CHILD FRAME (head `+0x2fc`, link `+0x300`, guard `[entry+8]+0xd0`);
/// the range stored at `[+0x31c]` is `max(0, bboxExtent − viewportExtent)`.
///
/// The distinction is not academic: stock `ItemTextPageScrollChild` is declared **10×10**
/// (`ItemTextFrame.xml` l.198) around a 270×304 SimpleHTML whose blocks extend well past it.
/// Measured as the child's own height the range is zero and a book cannot be scrolled at all;
/// measured as the subtree box it tracks the rendered text. Every ScrollFrame whose child is sized
/// directly — the guild-info EditBox, the options panes, every faux list — is unchanged, because
/// there its own rect already IS its subtree.
///
/// In **LOCAL units**, each height divided by the owning frame's effective scale — i.e. exactly
/// `child:GetHeight() - self:GetHeight()`, the widget contract this method states. The resolved
/// rects are screen px, and the scroll offset is NOT: `SetVerticalScroll` becomes the scroll
/// child's anchor y-offset, which the solver multiplies by that child's scale (`resolve`'s
/// override; `anchor_resolve_y`). Reporting a screen-px range against a local-unit offset makes
/// every SCALED ScrollFrame under-scroll by exactly its scale — the bar's max is the range, so it
/// stops short of the end and the tail of the content is unreachable. Invisible at scale 1.0,
/// which is every 1.12-native window; the era-scaled options window (ERA_WINDOW_SCALE 0.78) is the
/// first real-scroll customer that isn't, and it lost 22% of its travel.
fn scroll_range(model: &Model, h: FrameHandle) -> f32 {
    let child = match model.arena.frame(h).map(|f| &f.kind_state) {
        Some(KindState::Scroll(s)) => s.child,
        _ => None,
    };
    let Some(child) = child else { return 0.0 };
    let scale_of = |f: FrameHandle| {
        model
            .arena
            .frame(f)
            .map(|x| x.effective_scale)
            .filter(|s| s.abs() >= 1e-6)
            .unwrap_or(1.0)
    };
    let Some(frame_h) = model.resolved.get(&h).map(|r| r.height() / scale_of(h)) else {
        return 0.0;
    };
    let Some((top, bottom)) = subtree_span(model, child) else {
        return 0.0;
    };
    ((top - bottom) / scale_of(child) - frame_h).max(0.0)
}

/// The vertical span `(top, bottom)` of `f`'s subtree in SCREEN px — its own resolved rect unioned
/// with every VISIBLE descendant region and child frame, recursively (`0x786f80`). `None` when
/// nothing in the subtree resolved.
///
/// Visibility is the client's own guard on both lists: a hidden block contributes no scroll range,
/// which is what stops a window's parked art from inventing travel nobody can use.
fn subtree_span(model: &Model, f: FrameHandle) -> Option<(f32, f32)> {
    let frame = model.arena.frame(f)?;
    let mut span: Option<(f32, f32)> = model.resolved.get(&f).map(|r| (r.top, r.bottom));
    let union = |s: &mut Option<(f32, f32)>, t: f32, b: f32| {
        *s = Some(match *s {
            Some((ot, ob)) => (ot.max(t), ob.min(b)),
            None => (t, b),
        });
    };
    for &rh in &frame.regions {
        if model.region_data.get(&rh).is_some_and(|d| d.hidden) {
            continue;
        }
        if let Some(r) = model.region_resolved.get(&rh) {
            union(&mut span, r.top, r.bottom);
        }
    }
    for &ch in &frame.children {
        if !model.arena.frame(ch).is_some_and(|c| c.effective_visible) {
            continue;
        }
        if let Some((t, b)) = subtree_span(model, ch) {
            union(&mut span, t, b);
        }
    }
    span
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
            let changed = with_scroll(lua, &this, |s| {
                let changed = s.child != new_child;
                s.child = new_child;
                changed
            })?;
            if changed {
                // The child override is part of the resolve's read set (decision 0112's local
                // anchor map — a fingerprint input), so a real re-target dirties tier 1.
                lua.app_data_mut::<Model>().expect("model").touch_layout();
            }
            Ok(())
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
            let changed = with_scroll(lua, &this, |s| {
                let changed = s.vertical.to_bits() != clamped.to_bits();
                s.vertical = clamped;
                changed
            })?;
            if changed {
                lua.app_data_mut::<Model>().expect("model").touch_layout();
            }
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
            // That forced resolve can have MOVED sizes — this method exists precisely because the
            // caller just resized its content — so drain the queue before the range notify: the
            // reference fires `OnSizeChanged` from `ApplyRect`, i.e. during the layout, not after.
            event::fire_size_changes(lua);
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
