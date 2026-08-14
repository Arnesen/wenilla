//! Frame method-table cluster: Show/Hide/visibility, identity/hierarchy (`GetName`/`GetParent`/
//! `SetParent`), strata/level/scale/alpha, the Backdrop mechanism (`SetBackdrop`/`SetBackdropColor`/
//! `SetBackdropBorderColor`), and mouse-enable. Split out of [`super`] purely for size — the shared
//! id/handle plumbing, `CreateFrame`, and the method-table wiring stay there; this module's
//! [`install`] just populates its share of the one shared method table.

use mlua::{Lua, Table, Value};

use crate::order::Strata;
use crate::script::{event, Backdrop, Insets, Model};

use super::{decode_id, frame_handle_of, frame_wrapper, strata_from_str};

/// Populate `m`'s visibility/hierarchy/strata/backdrop/mouse methods (see the module doc).
pub(super) fn install(lua: &Lua, m: &Table) -> mlua::Result<()> {
    // Show / Hide / visibility
    m.set(
        "Show",
        lua.create_function(|lua, this: Table| set_shown(lua, &this, true))?,
    )?;
    m.set(
        "Hide",
        lua.create_function(|lua, this: Table| set_shown(lua, &this, false))?,
    )?;
    // SetShown(bool) — the live API's branchless Show/Hide (a consensus call across the 0068
    // target addons; Lua truthiness, so SetShown(nil) hides).
    m.set(
        "SetShown",
        lua.create_function(|lua, (this, shown): (Table, Value)| {
            let show = !matches!(shown, Value::Nil | Value::Boolean(false));
            set_shown(lua, &this, show)
        })?,
    )?;
    m.set(
        "IsShown",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model.arena.frame(h).map(|f| f.shown).unwrap_or(false))
        })?,
    )?;
    m.set(
        "IsVisible",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model
                .arena
                .frame(h)
                .map(|f| f.effective_visible)
                .unwrap_or(false))
        })?,
    )?;
    // Identity / hierarchy
    m.set(
        "GetName",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let name = {
                let model = lua.app_data_ref::<Model>().expect("model");
                model.arena.frame(h).and_then(|f| f.name.clone())
            };
            match name {
                Some(n) => Ok(Value::String(lua.create_string(&n)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    // GetID/SetID — the app-meaning-free numeric label (XML `id=`, the client's `+0xb4`): a
    // dropdown row's list position, a tab index. Default 0 (see `Frame::wow_id`).
    m.set(
        "GetID",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model.arena.frame(h).map(|f| f.wow_id).unwrap_or(0))
        })?,
    )?;
    m.set(
        "SetID",
        lua.create_function(|lua, (this, id): (Table, i64)| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            if let Some(f) = model.arena.frame_mut(h) {
                f.wow_id = id;
            }
            Ok(())
        })?,
    )?;
    m.set(
        "GetParent",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let parent_id = {
                let mut model = lua.app_data_mut::<Model>().expect("model");
                model
                    .arena
                    .frame(h)
                    .and_then(|f| f.parent)
                    .map(|p| model.frame_id(p))
            };
            match parent_id {
                Some(pid) => Ok(Value::Table(frame_wrapper(lua, pid)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    m.set(
        "SetParent",
        lua.create_function(|lua, (this, parent): (Table, Value)| {
            let h = frame_handle_of(lua, &this)?;
            let new_parent = {
                let model = lua.app_data_ref::<Model>().expect("model");
                match &parent {
                    Value::Table(t) => decode_id(t)
                        .ok()
                        .and_then(|id| model.id_to_frame.get(&id).copied()),
                    Value::String(s) => {
                        s.to_str().ok().and_then(|n| model.arena.lookup(n.as_ref()))
                    }
                    _ => None,
                }
            };
            let changed = {
                let mut model = lua.app_data_mut::<Model>().expect("model");
                let before = model.arena.frame(h).and_then(|f| f.parent);
                let changed = model.arena.set_parent(h, new_parent);
                // A real reparent can move the subtree's effective scale — a layout-gate input
                // (the arena rejects same-parent/cycle calls, so `before` moving is the test).
                if model.arena.frame(h).and_then(|f| f.parent) != before {
                    model.touch_layout();
                }
                changed
            };
            event::fire_visibility_changes(lua, changed);
            Ok(())
        })?,
    )?;
    // Strata / level / scale / alpha
    m.set(
        "SetFrameStrata",
        lua.create_function(|lua, (this, strata): (Table, String)| {
            let h = frame_handle_of(lua, &this)?;
            let s = strata_from_str(&strata)
                .ok_or_else(|| mlua::Error::runtime(format!("unknown frameStrata '{strata}'")))?;
            lua.app_data_mut::<Model>()
                .expect("model")
                .arena
                .set_frame_strata(h, s);
            Ok(())
        })?,
    )?;
    m.set(
        "GetFrameStrata",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            let s = model.arena.frame(h).map(|f| f.strata).unwrap_or_default();
            Ok(strata_name(s).to_string())
        })?,
    )?;
    m.set(
        "SetFrameLevel",
        lua.create_function(|lua, (this, level): (Table, i64)| {
            let h = frame_handle_of(lua, &this)?;
            let lvl = level.clamp(0, i64::from(u16::MAX)) as u16;
            lua.app_data_mut::<Model>()
                .expect("model")
                .arena
                .set_frame_level(h, lvl, true);
            Ok(())
        })?,
    )?;
    m.set(
        "GetFrameLevel",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(i64::from(
                model.arena.frame(h).map(|f| f.level).unwrap_or(0),
            ))
        })?,
    )?;
    m.set(
        "SetScale",
        lua.create_function(|lua, (this, scale): (Table, f32)| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            // Effective-scale changes ride the propagation's own eps gate; the own-scale compare
            // is the cheap superset (same own scale => no effective change is possible).
            let changed = model.arena.frame(h).is_some_and(|f| f.scale != scale);
            model.arena.set_scale(h, scale);
            if changed {
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;
    m.set(
        "GetScale",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model.arena.frame(h).map(|f| f.scale).unwrap_or(1.0))
        })?,
    )?;
    m.set(
        "SetAlpha",
        lua.create_function(|lua, (this, alpha): (Table, f32)| {
            let h = frame_handle_of(lua, &this)?;
            // The 1.12 API clamps to 0..1 — ref Lua leans on it (fade code that passes a 0..255
            // alpha stays fully opaque until the value falls below 1/255).
            lua.app_data_mut::<Model>()
                .expect("model")
                .arena
                .set_alpha(h, alpha.clamp(0.0, 1.0));
            Ok(())
        })?,
    )?;
    m.set(
        "GetAlpha",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model.arena.frame(h).map(|f| f.alpha).unwrap_or(1.0))
        })?,
    )?;
    // Backdrop (backdrop-mechanism.md): SetBackdrop(table|nil) installs (or, with nil, tears down)
    // the frame's tiled bg + 8-piece border plate. The two color setters tint the bg / all 8 border
    // pieces (never the reverse — spec §4). The Lua-table SetBackdrop defaults both colors to WHITE
    // (the ctor), so a caller must SetBackdropColor after to tint (the tooltip's OnLoad does).
    m.set(
        "SetBackdrop",
        lua.create_function(|lua, (this, arg): (Table, Value)| {
            let h = frame_handle_of(lua, &this)?;
            let bd = match arg {
                Value::Nil => None,
                Value::Table(t) => Some(backdrop_from_table(&t)?),
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "SetBackdrop: expected a table or nil, got {}",
                        other.type_name()
                    )))
                }
            };
            let mut model = lua.app_data_mut::<Model>().expect("model");
            match bd {
                Some(b) => {
                    model.backdrops.insert(h, b);
                }
                None => {
                    model.backdrops.remove(&h);
                }
            }
            Ok(())
        })?,
    )?;
    // `GetBackdrop()` (`0x777370`; wow-re `ui/scratch/widget-api-batch-benilla.md` Q5) — five things
    // a plausible implementation gets wrong, so each is spelled out with its reason:
    //
    // 1. It is a **reconstruction from the struct**, never the caller's table. The reference stores
    //    no Lua reference anywhere: `SetBackdrop` reads six keys into a fresh 0x68-byte struct at
    //    `frame+0x1ac` and drops the table, so `0x777426`–`0x7776b9` re-push every key from those
    //    fields. Handing back a stored clone would leak keys the reader never accepted and would
    //    keep stale values a later `SetBackdropColor` changed.
    // 2. **No backdrop ⇒ ZERO Lua values, not `nil`** — the early bail is `xor eax,eax; ret`, which
    //    for a *return* path really is "no values" (contrast `binding_abi`'s note: the same two
    //    bytes after a `luaL_error` are unreachable boilerplate). Observable through `select('#')`,
    //    and it is the shape our `GetTitleRegion` will *not* have when it lands — that one pushes
    //    nil, i.e. one value. The client cannot distinguish "never set" from `SetBackdrop(nil)`.
    // 3. **A partial `SetBackdrop` omits nothing on the way out.** Every `SetBackdrop` allocates a
    //    fresh struct (`0x777801`, ctor `0x77e5f0`), so a key the caller left out is a *ctor
    //    default* here, not an absent key and not the previous backdrop's value: `bgFile`/`edgeFile`
    //    `""`, `tileSize` 0, `edgeSize` **32**. Our `backdrop_from_table` already builds on
    //    `Backdrop::default()`, so this falls out — but only because `None` maps to `""` below
    //    rather than to nil.
    // 4. **`tile` is the NUMBER `1`, or the key is ABSENT — never `true`/`false`.** The push is
    //    `0x3ff00000` (the double 1.0) on true and `lua_pushnil` on false, and `lua_settable` with a
    //    nil value creates no key (and *erases* one from a recycled table, which is why the nil is
    //    written rather than skipped). An addon reading `if backdrop.tile then` sees the same truth
    //    either way; one that round-trips the table into `SetBackdrop` is why the number matters,
    //    since `tile` there goes through a coercer that takes numbers.
    // 5. The undocumented **in-place form** (`0x77740e`): `lua_type(L,2) == LUA_TTABLE` skips
    //    `lua_newtable` and fills arg 2, reusing an existing `insets` subtable rather than replacing
    //    it. Implemented — it is four lines, and an addon caching one table across frames would
    //    otherwise silently get a new one each call. A non-table arg 2 is ignored, not an error.
    //
    // No `bgColor`/`edgeColor`/`alpha` key exists: the two colors live in the struct but the reader
    // never pushes them (`GetBackdropColor`/`GetBackdropBorderColor` are their only accessors).
    m.set(
        "GetBackdrop",
        lua.create_function(|lua, (this, target): (Table, Value)| {
            let h = frame_handle_of(lua, &this)?;
            // Copied out before a single Lua write: filling a *caller-supplied* table can run a
            // `__newindex` metamethod, which can re-enter us and would panic on the app-data borrow.
            let bd = {
                let model = lua.app_data_ref::<Model>().expect("model");
                model.backdrops.get(&h).cloned()
            };
            let Some(bd) = bd else {
                return Ok(mlua::MultiValue::new()); // trap 2 — zero values, not nil
            };
            let t = match target {
                Value::Table(t) => t,
                _ => lua.create_table()?,
            };
            t.set("bgFile", bd.bg_file.as_deref().unwrap_or(""))?;
            t.set("edgeFile", bd.edge_file.as_deref().unwrap_or(""))?;
            t.set(
                "tile",
                if bd.tile {
                    Value::Number(1.0) // trap 4
                } else {
                    Value::Nil // trap 4 — erases the key from a recycled table
                },
            )?;
            t.set("tileSize", f64::from(bd.tile_size))?;
            t.set("edgeSize", f64::from(bd.edge_size))?;
            let insets = match t.get::<Value>("insets") {
                Ok(Value::Table(existing)) => existing, // trap 5 — reuse, don't replace
                _ => {
                    let fresh = lua.create_table()?;
                    t.set("insets", &fresh)?;
                    fresh
                }
            };
            insets.set("left", f64::from(bd.insets.left))?;
            insets.set("right", f64::from(bd.insets.right))?;
            insets.set("top", f64::from(bd.insets.top))?;
            insets.set("bottom", f64::from(bd.insets.bottom))?;
            Ok(mlua::MultiValue::from_vec(vec![Value::Table(t)]))
        })?,
    )?;
    m.set(
        "SetBackdropColor",
        lua.create_function(
            |lua, (this, r, g, b, a): (Table, f32, f32, f32, Option<f32>)| {
                let h = frame_handle_of(lua, &this)?;
                let mut model = lua.app_data_mut::<Model>().expect("model");
                if let Some(bd) = model.backdrops.get_mut(&h) {
                    bd.bg_color = [r, g, b, a.unwrap_or(1.0)];
                }
                Ok(())
            },
        )?,
    )?;
    m.set(
        "SetBackdropBorderColor",
        lua.create_function(
            |lua, (this, r, g, b, a): (Table, f32, f32, f32, Option<f32>)| {
                let h = frame_handle_of(lua, &this)?;
                let mut model = lua.app_data_mut::<Model>().expect("model");
                if let Some(bd) = model.backdrops.get_mut(&h) {
                    bd.border_color = [r, g, b, a.unwrap_or(1.0)];
                }
                Ok(())
            },
        )?,
    )?;
    m.set(
        "GetBackdropColor",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            match model.backdrops.get(&h) {
                Some(bd) => Ok((
                    bd.bg_color[0],
                    bd.bg_color[1],
                    bd.bg_color[2],
                    bd.bg_color[3],
                )),
                None => Ok((1.0, 1.0, 1.0, 1.0)),
            }
        })?,
    )?;
    m.set(
        "GetBackdropBorderColor",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            match model.backdrops.get(&h) {
                Some(bd) => Ok((
                    bd.border_color[0],
                    bd.border_color[1],
                    bd.border_color[2],
                    bd.border_color[3],
                )),
                None => Ok((1.0, 1.0, 1.0, 1.0)),
            }
        })?,
    )?;
    // Mouse interaction (EnableMouse gates hit-testing; keyboard focus is out of scope)
    m.set(
        "EnableMouse",
        lua.create_function(|lua, (this, enable): (Table, bool)| {
            let h = frame_handle_of(lua, &this)?;
            lua.app_data_mut::<Model>()
                .expect("model")
                .arena
                .set_mouse_enabled(h, enable);
            Ok(())
        })?,
    )?;
    m.set(
        "IsMouseEnabled",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model.arena.is_mouse_enabled(h))
        })?,
    )?;
    // EnableKeyboard(flag) / IsKeyboardEnabled() — `0x776f90` / `0x776ff0`, real Frame entries in
    // wow-re's frame-API carve, beside the mouse pair above.
    //
    // 8 corpus addons call this and were RAISING on it (`ColorPickerPlus:258`,
    // `Dewdrop-2.0.lua:2021-2022` — which is in ~65 addons — `AckisRecipeList/ARLFrame.lua:1650`).
    // The flag round-trips; **key delivery is not gated on it**, exactly as the wheel's flag
    // shipped in 1198, because the machinery it would gate does not exist here yet.
    //
    // Why that is the right half rather than a silent stub, which this codebase has been bitten by
    // three times (1203/1205/1211): the majority of the corpus calls pass **false**, and for those
    // our behaviour is already the reference's — we deliver no keys to arbitrary frames either. The
    // `true` callers want delivery we do not do, but they did not get it before this landed; they
    // got a raise that killed the enclosing function. And per
    // `frame-key-script-delivery.md` §3.2 being enabled genuinely is separable from having a
    // handler, so the flag is a real piece of the model, not a placeholder for one.
    m.set(
        "EnableKeyboard",
        lua.create_function(|lua, (this, enable): (Table, bool)| {
            let h = frame_handle_of(lua, &this)?;
            lua.app_data_mut::<Model>()
                .expect("model")
                .arena
                .set_keyboard_enabled(h, enable);
            Ok(())
        })?,
    )?;
    m.set(
        "IsKeyboardEnabled",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model.arena.is_keyboard_enabled(h))
        })?,
    )?;
    // `EnableMouseWheel(flag)` / `IsMouseWheelEnabled()` — the wheel's own gate, a separate flag
    // from `EnableMouse` in the reference and separate here (decision 1198).
    //
    // The flag is real and round-trips. **The dispatch is NOT gated on it yet, deliberately.**
    // Our wheel dispatch keys off "does this frame carry an `OnMouseWheel` handler", walking up to
    // the nearest ancestor that does — more permissive than the reference, which also requires the
    // frame to be wheel-enabled so a scroll region can hand the wheel to the window behind it
    // without tearing its handler out.
    //
    // Gating it today would break our own UI: 44 `OnMouseWheel` sites across 14 shipped files and
    // **not one of them declares `enableMouseWheel`**, because the loader has never read that
    // attribute. The condition to flip it is concrete rather than someday — teach the loader the
    // attribute, declare it on those 44 sites, then gate. Until then this is a disclosed superset
    // (1189's argument, pointed the other way), and the two corpus addons that stopped on the
    // missing *method* are unblocked either way.
    m.set(
        "EnableMouseWheel",
        lua.create_function(|lua, (this, enable): (Table, bool)| {
            let h = frame_handle_of(lua, &this)?;
            lua.app_data_mut::<Model>()
                .expect("model")
                .arena
                .set_mouse_wheel_enabled(h, enable);
            Ok(())
        })?,
    )?;
    m.set(
        "IsMouseWheelEnabled",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model.arena.is_mouse_wheel_enabled(h))
        })?,
    )?;
    // Clamp-to-screen (`0x776c00`/`0x776cb0`, geometry flags bit4 — layout.md): the layout resolve
    // keeps the frame's assembled rect inside the window, size preserved. GameTooltip frames
    // default true by construction (widget::Frame::clamped_to_screen — decision 0352).
    m.set(
        "SetClampedToScreen",
        lua.create_function(|lua, (this, clamp): (Table, bool)| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let changed = model.arena.is_clamped_to_screen(h) != clamp;
            model.arena.set_clamped_to_screen(h, clamp);
            if changed {
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;
    m.set(
        "IsClampedToScreen",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model.arena.is_clamped_to_screen(h))
        })?,
    )?;
    // Hit-rect insets — the MOUSE rect only (widget::Frame::hit_rect_insets): the hit test shrinks
    // the resolved rect by these four before testing the cursor, and nothing else reads them, so a
    // frame's geometry/draw/anchor answers are unchanged. The ref sets them wherever a button's
    // frame is larger than its art (the micro buttons' 18 px empty header).
    m.set(
        "SetHitRectInsets",
        lua.create_function(
            |lua, (this, left, right, top, bottom): (Table, f32, f32, f32, f32)| {
                let h = frame_handle_of(lua, &this)?;
                lua.app_data_mut::<Model>()
                    .expect("model")
                    .arena
                    .set_hit_rect_insets(h, [left, right, top, bottom]);
                Ok(())
            },
        )?,
    )?;
    m.set(
        "GetHitRectInsets",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            let i = model.arena.hit_rect_insets(h);
            Ok((i[0], i[1], i[2], i[3]))
        })?,
    )?;
    Ok(())
}

/// Parse a Lua `SetBackdrop` table into a [`Backdrop`] (backdrop-mechanism.md §1). The keys read —
/// exactly the compiled reader's set — are `bgFile`/`edgeFile` (strings), `tile` (boolean, default
/// false), `tileSize`/`edgeSize` (numbers; a missing number leaves the ctor default: tileSize 0,
/// edgeSize 32), and `insets{left,right,top,bottom}` (numbers, each default 0). A non-string file or
/// non-number size is treated as absent (the client's per-key type gate). Colors stay the ctor
/// white — SetBackdrop never reads a color key; the color setters do that.
fn backdrop_from_table(t: &Table) -> mlua::Result<Backdrop> {
    let mut bd = Backdrop::default();
    if let Ok(Value::String(s)) = t.get::<Value>("bgFile") {
        bd.bg_file = Some(s.to_str()?.to_string());
    }
    if let Ok(Value::String(s)) = t.get::<Value>("edgeFile") {
        bd.edge_file = Some(s.to_str()?.to_string());
    }
    // `tile` — Lua truthiness (nil/false ⇒ false; a missing key reads as Nil).
    bd.tile = !matches!(
        t.get::<Value>("tile").unwrap_or(Value::Nil),
        Value::Nil | Value::Boolean(false)
    );
    if let Ok(Value::Number(n)) = t.get::<Value>("tileSize") {
        bd.tile_size = n as f32;
    } else if let Ok(Value::Integer(n)) = t.get::<Value>("tileSize") {
        bd.tile_size = n as f32;
    }
    if let Ok(Value::Number(n)) = t.get::<Value>("edgeSize") {
        bd.edge_size = n as f32;
    } else if let Ok(Value::Integer(n)) = t.get::<Value>("edgeSize") {
        bd.edge_size = n as f32;
    }
    if let Ok(Value::Table(ins)) = t.get::<Value>("insets") {
        let num = |k: &str| -> f32 {
            match ins.get::<Value>(k) {
                Ok(Value::Number(n)) => n as f32,
                Ok(Value::Integer(n)) => n as f32,
                _ => 0.0,
            }
        };
        bd.insets = Insets {
            left: num("left"),
            right: num("right"),
            top: num("top"),
            bottom: num("bottom"),
        };
    }
    Ok(bd)
}

fn strata_name(s: Strata) -> &'static str {
    match s {
        Strata::World => "WORLD",
        Strata::Background => "BACKGROUND",
        Strata::Low => "LOW",
        Strata::Medium => "MEDIUM",
        Strata::High => "HIGH",
        Strata::Dialog => "DIALOG",
        Strata::Fullscreen => "FULLSCREEN",
        Strata::FullscreenDialog => "FULLSCREEN_DIALOG",
        Strata::Tooltip => "TOOLTIP",
        Strata::Blizzard => "BLIZZARD",
    }
}

fn set_shown(lua: &Lua, this: &Table, shown: bool) -> mlua::Result<()> {
    let h = frame_handle_of(lua, this)?;
    let changed = {
        let mut model = lua.app_data_mut::<Model>().expect("model");
        model.arena.set_shown(h, shown)
    };
    event::fire_visibility_changes(lua, changed);
    Ok(())
}
