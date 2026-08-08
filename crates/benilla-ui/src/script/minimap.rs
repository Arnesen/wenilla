//! The `Minimap` method surface — the zoom API the FrameXML zoom buttons drive
//! (`MinimapZoomIn`/`MinimapZoomOut` → `Minimap:SetZoom(Minimap:GetZoom() ± 1)`).
//!
//! Grounded in wow-re's byte-verified minimap node: `get_zoom_levels` (`0x6da9a0`) returns the
//! constant 6, `set_zoom` clamps at 5 and marks the tile grid dirty, and the zoom index feeds the
//! `zoom_to_scale` tables (`0x6da9b0`). The engine core carries only the index
//! ([`MinimapState`]); the app renderer maps it to a world radius and draws the tiles
//! (decision 0203). The model attrs (`minimapPlayerModel=`/`minimapArrowModel=`) are not
//! modeled yet — deferred with phase 3's blips.
//!
//! **The level persists** (decision 1131). The client keeps two halves: the live indices
//! (`0x86f698` outdoor / `0x86f69c` indoor — our [`MinimapState`]) and the two CVar objects
//! `minimapZoom`/`minimapInsideZoom` (both registered default `"3"`), which are what `Config.wtf`
//! actually stores. `set_zoom` writes *both*; the minimap reset path (`0x6d9008`–`0x6d901f`)
//! re-seeds the live index from the CVar's parsed int. benilla runs the same two halves at the same
//! seam: `SetZoom` below writes the index and the CVar, and the app seeds the widget from the CVar
//! table once, when the in-game UI materializes ([`super::UiScript::set_minimap_zoom`]).
//!
//! The methods live in their own registry table, consulted by the frame `__index` dispatcher only
//! for Minimap frames — the same duck-typing posture as StatusBar's.

use mlua::{Lua, Table};

use super::object::frame_handle_of;
use super::Model;
use crate::widget::{KindState, MinimapState, MINIMAP_ZOOM_LEVELS};

/// Registry key of the Minimap method table (the MAXCSTACK discipline: Lua-side root, named key).
pub(super) const REG_MINIMAP_METHODS: &str = "__benilla_minimap_methods";

/// Run `f` over a frame's Minimap state under one short write borrow. Errors if `this` is not a
/// live Minimap (unreachable through the kind dispatcher, but the method table is a plain Lua
/// value — a caller can fish it out and misapply it).
fn with_minimap<T>(
    lua: &Lua,
    this: &Table,
    f: impl FnOnce(&mut MinimapState) -> T,
) -> mlua::Result<T> {
    let h = frame_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let frame = model
        .arena
        .frame_mut(h)
        .ok_or_else(|| mlua::Error::runtime("stale frame handle"))?;
    match &mut frame.kind_state {
        KindState::Minimap(m) => Ok(f(m)),
        _ => Err(mlua::Error::runtime("not a Minimap")),
    }
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;

    m.set(
        "GetZoom",
        // Reads whichever index is live — the indoor one (`0x86f69c`) while inside a WMO, else the
        // outdoor one (`0x86f698`). The client's `get_zoom_index` routes on the same flag.
        lua.create_function(|lua, this: Table| with_minimap(lua, &this, |m| m.active_zoom()))?,
    )?;
    m.set(
        "SetZoom",
        lua.create_function(|lua, (this, zoom): (Table, f64)| {
            // The client's set_zoom clamps into [0, levels-1] (`0x6daa10`: clamp at 5). A negative
            // or fractional Lua number truncates like lua_tonumber → int. It writes the index the
            // inside flag selects, so the +/- buttons zoom the map you're actually looking at and
            // each mode keeps its own level.
            let clamped = (zoom.max(0.0) as u8).min(MINIMAP_ZOOM_LEVELS - 1);
            let inside = with_minimap(lua, &this, |m| {
                m.set_active_zoom(clamped);
                m.inside
            })?;
            // …and it persists the level in the same breath: `set_zoom` writes the live index AND
            // `CVar::Set`s the matching CVar (`minimapInsideZoom` indoors, `minimapZoom` out), which
            // is the whole reason a zoom level survives a restart. The borrow above is released
            // before this one — both reach the same `Model` app_data.
            super::cvars::set_from_engine(
                &mut lua.app_data_mut::<Model>().expect("model app_data"),
                if inside {
                    "minimapInsideZoom"
                } else {
                    "minimapZoom"
                },
                clamped.to_string(),
            );
            Ok(())
        })?,
    )?;
    m.set(
        "GetZoomLevels",
        lua.create_function(|lua, this: Table| {
            // Validate the receiver like every kind method (the constant is per the client's
            // `get_zoom_levels`, but a non-Minimap receiver is still a caller bug).
            with_minimap(lua, &this, |_| MINIMAP_ZOOM_LEVELS)
        })?,
    )?;
    lua.set_named_registry_value(REG_MINIMAP_METHODS, m)?;
    Ok(())
}
