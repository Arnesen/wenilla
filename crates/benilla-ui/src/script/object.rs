//! The FrameScript object model, frame side (RF-0023) — `CreateFrame`, the frame metatable +
//! wrapper cache, and the shared frame method surface (decision 0068 v1). The region side lives in
//! [`super::region`]; per-kind frame methods in their own modules ([`super::statusbar`], …),
//! dispatched by [`kind_method_registries`] before the shared table.
//!
//! A frame's Lua value is a table `T` with `T[0] = lightuserdata(id)` (the id encoding stands in for
//! RF-0023's `CScriptObject*`; see the module docs on ids) and one **shared** metatable whose
//! `__index` is a Rust function that dispatches a method name to its implementation. Wrappers are
//! cached in a Lua-side registry table keyed by id, so `GetParent()` (etc.) returns the *same* table
//! every time (stable identity). Named frames auto-publish to `_G`, non-overwriting (RF-0023). Region
//! leaves (`Texture`/`FontString`) use the same `T[0]` scheme but a *distinct* metatable (the "tag"
//! that separates their method surface from frames').
//!
//! The whole surface honors the MAXCSTACK discipline: the two metatables, the two method tables, and
//! the wrapper cache live in the Lua registry (named keys); Rust holds none of them across calls.

use std::ffi::c_void;

use mlua::{LightUserData, Lua, Table, Value};

use super::{Model, REG_FRAME_META, REG_FRAME_METHODS, REG_SCRIPTS, REG_WRAPPERS};
use crate::layout::Point;
use crate::order::{DrawLayer, Strata};
use crate::widget::{FrameHandle, FrameKind};

// The frame method-table clusters — split out purely for size (each module's own doc says what
// lives there); this file keeps the shared id/handle plumbing, `install`, and `CreateFrame`.
mod events_regions;
mod frame_state;
mod layout_methods;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// id ↔ lightuserdata
// ─────────────────────────────────────────────────────────────────────────────────────────────

pub(super) fn id_to_lud(id: u32) -> LightUserData {
    LightUserData(id as usize as *mut c_void)
}

/// Read the id out of a wrapper table's `T[0]` lightuserdata (RF-0023).
pub(super) fn decode_id(this: &Table) -> mlua::Result<u32> {
    match this.raw_get::<Value>(0)? {
        Value::LightUserData(l) => Ok(l.0 as usize as u32),
        _ => Err(mlua::Error::runtime(
            "not a benilla frame/region object (missing T[0] identity)",
        )),
    }
}

/// Resolve `self` (a frame wrapper) to its live [`FrameHandle`].
pub(super) fn frame_handle_of(lua: &Lua, this: &Table) -> mlua::Result<FrameHandle> {
    let id = decode_id(this)?;
    let model = lua.app_data_ref::<Model>().expect("model app_data");
    model
        .id_to_frame
        .get(&id)
        .copied()
        .ok_or_else(|| mlua::Error::runtime("stale or invalid frame handle"))
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Wrapper cache (RF-0023: lazy bind + named _G publish, non-overwriting)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Get-or-create the wrapper table for a frame id, caching it and (if the frame is named and `_G`
/// has no such global yet) publishing it to `_G`. Callers must hold **no** model borrow (this takes
/// a short one to read the frame's name).
pub(super) fn frame_wrapper(lua: &Lua, id: u32) -> mlua::Result<Table> {
    let wrappers: Table = lua.named_registry_value(REG_WRAPPERS)?;
    if let Value::Table(t) = wrappers.get::<Value>(id)? {
        return Ok(t);
    }
    let t = lua.create_table()?;
    t.raw_set(0, Value::LightUserData(id_to_lud(id)))?;
    let meta: Table = lua.named_registry_value(REG_FRAME_META)?;
    t.set_metatable(Some(meta))?;
    wrappers.set(id, t.clone())?;

    let name: Option<String> = {
        let model = lua.app_data_ref::<Model>().expect("model app_data");
        model
            .id_to_frame
            .get(&id)
            .and_then(|h| model.arena.frame(*h))
            .and_then(|f| f.name.clone())
    };
    if let Some(name) = name {
        publish_global(lua, &name, &t)?;
    }
    Ok(t)
}

/// Publish a wrapper to `_G[name]`, non-overwriting (RF-0023).
pub(super) fn publish_global(lua: &Lua, name: &str, wrapper: &Table) -> mlua::Result<()> {
    let g = lua.globals();
    if g.get::<Value>(name)?.is_nil() {
        g.set(name, wrapper.clone())?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// String → enum parsing (the client's tables, transcribed in order.rs / layout.rs)
// ─────────────────────────────────────────────────────────────────────────────────────────────

pub(super) fn point_from_str(s: &str) -> Option<Point> {
    Some(match s.to_ascii_uppercase().as_str() {
        "TOPLEFT" => Point::TopLeft,
        "TOP" => Point::Top,
        "TOPRIGHT" => Point::TopRight,
        "LEFT" => Point::Left,
        "CENTER" => Point::Center,
        "RIGHT" => Point::Right,
        "BOTTOMLEFT" => Point::BottomLeft,
        "BOTTOM" => Point::Bottom,
        "BOTTOMRIGHT" => Point::BottomRight,
        _ => return None,
    })
}

/// The inverse of [`point_from_str`] (the client's point-id table `0x811a38`, names back out).
fn point_name(p: Point) -> &'static str {
    match p {
        Point::TopLeft => "TOPLEFT",
        Point::Top => "TOP",
        Point::TopRight => "TOPRIGHT",
        Point::Left => "LEFT",
        Point::Center => "CENTER",
        Point::Right => "RIGHT",
        Point::BottomLeft => "BOTTOMLEFT",
        Point::Bottom => "BOTTOM",
        Point::BottomRight => "BOTTOMRIGHT",
    }
}

fn strata_from_str(s: &str) -> Option<Strata> {
    Some(match s.to_ascii_uppercase().as_str() {
        "WORLD" => Strata::World,
        "BACKGROUND" => Strata::Background,
        "LOW" => Strata::Low,
        "MEDIUM" => Strata::Medium,
        "HIGH" => Strata::High,
        "DIALOG" => Strata::Dialog,
        "FULLSCREEN" => Strata::Fullscreen,
        "FULLSCREEN_DIALOG" => Strata::FullscreenDialog,
        "TOOLTIP" => Strata::Tooltip,
        "BLIZZARD" => Strata::Blizzard,
        _ => return None,
    })
}

pub(super) fn draw_layer_from_str(s: &str) -> Option<DrawLayer> {
    Some(match s.to_ascii_uppercase().as_str() {
        "BACKGROUND" => DrawLayer::Background,
        "BORDER" => DrawLayer::Border,
        "ARTWORK" => DrawLayer::Artwork,
        "OVERLAY" => DrawLayer::Overlay,
        "HIGHLIGHT" => DrawLayer::Highlight,
        _ => return None,
    })
}

fn frame_kind_from_str(s: &str) -> Option<FrameKind> {
    Some(match s.to_ascii_uppercase().as_str() {
        "FRAME" => FrameKind::Frame,
        "BUTTON" => FrameKind::Button,
        "CHECKBUTTON" => FrameKind::CheckButton,
        "EDITBOX" => FrameKind::EditBox,
        "STATUSBAR" => FrameKind::StatusBar,
        "SLIDER" => FrameKind::Slider,
        "SCROLLFRAME" => FrameKind::ScrollFrame,
        "MODEL" => FrameKind::Model,
        "MESSAGEFRAME" => FrameKind::MessageFrame,
        "SCROLLINGMESSAGEFRAME" => FrameKind::ScrollingMessageFrame,
        "COLORSELECT" => FrameKind::ColorSelect,
        "SIMPLEHTML" => FrameKind::SimpleHtml,
        "MOVIEFRAME" => FrameKind::MovieFrame,
        "GAMETOOLTIP" => FrameKind::GameTooltip,
        "MINIMAP" => FrameKind::Minimap,
        "COOLDOWN" => FrameKind::Cooldown,
        _ => return None,
    })
}

/// A Lua number-ish → f32 (nil/other → 0.0), for offset/color args.
pub(super) fn as_f32(v: &Value) -> f32 {
    match v {
        Value::Number(n) => *n as f32,
        Value::Integer(i) => *i as f32,
        _ => 0.0,
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// install — build the method tables, metatables, CreateFrame, and registry roots
// ─────────────────────────────────────────────────────────────────────────────────────────────

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    // The wrapper cache + the per-frame script tables (Lua-side roots — the MAXCSTACK discipline).
    lua.set_named_registry_value(REG_WRAPPERS, lua.create_table()?)?;
    lua.set_named_registry_value(REG_SCRIPTS, lua.create_table()?)?;

    install_frame_methods(lua)?;
    super::region::install(lua)?;
    super::statusbar::install(lua)?;
    super::button::install(lua)?;
    super::editbox::install(lua)?;
    super::messageframe::install(lua)?;

    // Shared frame metatable: __index is a Rust dispatcher over the frame method table (RF-0023),
    // checking the frame's *kind-specific* method table first. Per-kind resolution matters beyond
    // correctness: addons duck-type widgets (`if frame.SetValue then …`), so a plain frame must
    // resolve `SetValue` to nil — one shared table would make every frame quack like every widget.
    let frame_meta = lua.create_table()?;
    let frame_index = lua.create_function(|lua, (this, key): (Table, Value)| {
        for reg in kind_method_registries(lua, &this) {
            let methods: Table = lua.named_registry_value(reg)?;
            let v = methods.get::<Value>(key.clone())?;
            if !v.is_nil() {
                return Ok(v);
            }
        }
        let methods: Table = lua.named_registry_value(REG_FRAME_METHODS)?;
        methods.get::<Value>(key)
    })?;
    frame_meta.set("__index", frame_index)?;
    lua.set_named_registry_value(REG_FRAME_META, frame_meta.clone())?;
    // RF-0023 publishes the shared metatable as _G["__framescript_meta"].
    lua.globals().set("__framescript_meta", frame_meta)?;

    // CreateFrame(kind, name?, parent?, inherits?) — a global (RF-0024's factory is the loader's;
    // this is the runtime one). `inherits` (4th) is the loader's template job — accepted + warned.
    let create_frame = lua.create_function(create_frame)?;
    lua.globals().set("CreateFrame", create_frame)?;

    // GetScreenWidth/Height — the screen-root rect in UI units (the ref's tooltip side-pick reads
    // them: ContainerFrameItemButton_OnEnter anchors LEFT when the slot sits in the right half).
    lua.globals().set(
        "GetScreenWidth",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model.screen.width())
        })?,
    )?;
    lua.globals().set(
        "GetScreenHeight",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model.screen.height())
        })?,
    )?;

    // GetCursorPosition() → x, y — the last cursor position the host fed (`mouse_move`/
    // `mouse_button`), UI units y-up like every other coordinate read. The real client scales by
    // the UI scale; ours is the constant 1 (`GetEffectiveScale`), so the ref's `/scale` dance is
    // an identity. The world map polls this every OnUpdate for hover/click math.
    lua.globals().set(
        "GetCursorPosition",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model.cursor_pos)
        })?,
    )?;

    // The modifier-key mirror ([`UiScript::set_modifiers`], fed by the app's input pass before
    // any mouse event each frame) — the reference handlers fork on these at click time
    // (ContainerFrame.lua's shift-split / ctrl-dressup, ActionBarFrame.xml's shift-pickup,
    // SpellBookFrame.lua's shift-pickup). Era booleans, not 1.12's 1/nil — the house API target
    // (decision 0068); every transcribed `if IsShiftKeyDown()` reads both identically.
    for (name, pick) in [
        ("IsShiftKeyDown", 0usize),
        ("IsControlKeyDown", 1),
        ("IsAltKeyDown", 2),
    ] {
        lua.globals().set(
            name,
            lua.create_function(move |lua, ()| {
                let model = lua.app_data_ref::<Model>().expect("model");
                let m = [model.modifiers.0, model.modifiers.1, model.modifiers.2];
                Ok(m[pick])
            })?,
        )?;
    }

    Ok(())
}

/// The registry keys of a frame's kind-specific method tables, in resolution order — the `__index`
/// dispatcher walks them before the shared table, mirroring the client's class chain (CheckButton
/// resolves through Button's map, RF-28). Empty for plain kinds (and for anything that isn't a live
/// frame, e.g. a region wrapper passing through).
fn kind_method_registries(lua: &Lua, this: &Table) -> &'static [&'static str] {
    let Ok(id) = decode_id(this) else { return &[] };
    let model = lua.app_data_ref::<Model>().expect("model app_data");
    let Some(h) = model.id_to_frame.get(&id) else {
        return &[];
    };
    match model.arena.frame(*h).map(|f| f.kind) {
        Some(FrameKind::StatusBar) => &[super::statusbar::REG_STATUSBAR_METHODS],
        Some(FrameKind::EditBox) => &[super::editbox::REG_EDITBOX_METHODS],
        Some(FrameKind::ScrollingMessageFrame) => &[super::messageframe::REG_MESSAGEFRAME_METHODS],
        Some(FrameKind::ScrollFrame) => &[super::scrollframe::REG_SCROLLFRAME_METHODS],
        Some(FrameKind::Slider) => &[super::slider::REG_SLIDER_METHODS],
        Some(FrameKind::Button) => &[super::button::REG_BUTTON_METHODS],
        Some(FrameKind::CheckButton) => &[
            super::button::REG_CHECKBUTTON_METHODS,
            super::button::REG_BUTTON_METHODS,
        ],
        Some(FrameKind::Minimap) => &[super::minimap::REG_MINIMAP_METHODS],
        Some(FrameKind::Cooldown) => &[super::cooldown::REG_COOLDOWN_METHODS],
        Some(FrameKind::GameTooltip) => &[super::tooltip::REG_TOOLTIP_METHODS],
        _ => &[],
    }
}

fn create_frame(
    lua: &Lua,
    (kind, name, parent, inherits): (String, Option<Value>, Option<Value>, Option<Value>),
) -> mlua::Result<Table> {
    let kind = frame_kind_from_str(&kind)
        .ok_or_else(|| mlua::Error::runtime(format!("CreateFrame: unknown frame type '{kind}'")))?;
    let name: Option<String> = match &name {
        Some(Value::String(s)) => Some(s.to_str()?.to_string()),
        _ => None,
    };

    // Resolve the parent handle (a wrapper table, or a name string) under a short read borrow.
    let parent_handle: Option<FrameHandle> = {
        let model = lua.app_data_ref::<Model>().expect("model app_data");
        match &parent {
            Some(Value::Table(t)) => decode_id(t)
                .ok()
                .and_then(|id| model.id_to_frame.get(&id).copied()),
            Some(Value::String(s)) => s.to_str().ok().and_then(|n| model.arena.lookup(n.as_ref())),
            _ => None,
        }
    };

    if matches!(&inherits, Some(v) if !v.is_nil()) {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        model.warnings.push(format!(
            "CreateFrame: `inherits`/template expansion is the XML loader's job — ignored for '{}'",
            name.as_deref().unwrap_or("<unnamed>")
        ));
    }

    // Create in the arena, mint the id, seed a default layout input. All under one write borrow.
    let id = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        let h = model.arena.create(kind, name, parent_handle);
        // The client's CreateFrame inheritance (the ctor doc's "loader/CreateFrame concern" —
        // widget/mod.rs `create`): a child enters its PARENT's stratum at the parent's level + 1.
        // The ctor's bare MEDIUM/0 left a DIALOG-strata popup drawing its own translucent
        // backdrop OVER its buttons (the children stayed in MEDIUM — the abandon dialog's dark
        // buttons, pixel-diffed against the raw art). An explicit frameStrata/frameLevel attr
        // still overrides right after (apply_attrs).
        if let Some(p) = parent_handle {
            if let Some((pstrata, plevel)) = model.arena.frame(p).map(|f| (f.strata, f.level)) {
                model.arena.set_frame_strata(h, pstrata);
                model.arena.set_frame_level(h, plevel + 1, false);
            }
        }
        let id = model.frame_id(h);
        model.layout_inputs.entry(h).or_default();
        id
    };

    frame_wrapper(lua, id)
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Frame method surface — the table itself is built by the three clusters above
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Build the shared frame method table from the three clusters ([`frame_state`], [`layout_methods`],
/// [`events_regions`]) and publish it to the registry.
fn install_frame_methods(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;
    frame_state::install(lua, &m)?;
    layout_methods::install(lua, &m)?;
    events_regions::install(lua, &m)?;
    lua.set_named_registry_value(REG_FRAME_METHODS, m)?;
    Ok(())
}
