//! Region method-table cluster: **layout** — size, anchors and the resolved-rect readers.
//! Split out of `region.rs` at the 0716 file-size budget.

use mlua::{Lua, Table, Value};

use crate::layout::{Anchor, Point};
use crate::script::object::anchor_bits_eq;
use crate::script::Model;

/// Resolve `self` (a region wrapper) to its live [`RegionHandle`].
use super::{
    measured_wh, region_handle_of, region_owner_id, region_set_point, resolve_target, size_bits_eq,
};

/// Populate `m`'s layout methods (see the module doc).
pub(super) fn install(lua: &Lua, m: &Table) -> mlua::Result<()> {
    // Region explicit size (drawn centered on the owner; region anchors come later).
    m.set(
        "SetWidth",
        lua.create_function(|lua, (this, w): (Table, f32)| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let d = model.region_data.entry(rh).or_default();
            let new = Some((w, d.size.map_or(0.0, |s| s.1)));
            let changed = !size_bits_eq(d.size, new);
            d.size = new;
            if changed {
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;

    m.set(
        "SetHeight",
        lua.create_function(|lua, (this, h): (Table, f32)| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let d = model.region_data.entry(rh).or_default();
            let new = Some((d.size.map_or(0.0, |s| s.0), h));
            let changed = !size_bits_eq(d.size, new);
            d.size = new;
            if changed {
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;

    m.set(
        "SetSize",
        lua.create_function(|lua, (this, w, h): (Table, f32, f32)| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let d = model.region_data.entry(rh).or_default();
            let new = Some((w, h));
            let changed = !size_bits_eq(d.size, new);
            d.size = new;
            if changed {
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;

    m.set(
        "GetWidth",
        lua.create_function(|lua, this: Table| Ok(measured_wh(lua, &this)?.0))?,
    )?;

    m.set(
        "GetHeight",
        lua.create_function(|lua, this: Table| Ok(measured_wh(lua, &this)?.1))?,
    )?;

    // GetLeft/GetRight/GetTop/GetBottom — the region's RESOLVED edges (y-up UI units; frame twin
    // in object.rs). An anchored region reads its own resolved rect; an unanchored one has no
    // rect of its own (it draws relative to its owner at extract) → nil, same as pre-resolve.
    for (name, pick) in [
        ("GetLeft", 0u8),
        ("GetRight", 1u8),
        ("GetTop", 2u8),
        ("GetBottom", 3u8),
    ] {
        m.set(
            name,
            lua.create_function(move |lua, this: Table| {
                let rh = region_handle_of(lua, &this)?;
                let model = lua.app_data_ref::<Model>().expect("model");
                Ok(model.region_resolved.get(&rh).map(|r| match pick {
                    0 => r.left,
                    1 => r.right,
                    2 => r.top,
                    _ => r.bottom,
                }))
            })?,
        )?;
    }

    // Region anchors: SetPoint/ClearAllPoints/SetAllPoints mirror the frame versions
    // ([`super::object`]) but write [`super::RegionData::anchors`]. An unspecified `relativeTo`
    // defaults to the **owner frame**; a named one may be a frame or a sibling region (the real
    // XML anchors regions to sibling regions everywhere — merchant label plate → `$parentSlot`).
    m.set(
        "SetPoint",
        lua.create_function(
            |lua, (this, p, a2, a3, a4, a5): (Table, String, Value, Value, Value, Value)| {
                region_set_point(lua, &this, &p, [a2, a3, a4, a5])
            },
        )?,
    )?;

    m.set(
        "ClearAllPoints",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let d = model.region_data.entry(rh).or_default();
            let changed = !d.anchors.is_empty();
            d.anchors.clear();
            if changed {
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;

    m.set(
        "SetAllPoints",
        lua.create_function(|lua, (this, target): (Table, Value)| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let owner = region_owner_id(&mut model, rh);
            let rel_id = resolve_target(&mut model, &target, owner);
            let pair = [
                Anchor::new(Point::TopLeft, rel_id, Point::TopLeft, 0.0, 0.0),
                Anchor::new(Point::BottomRight, rel_id, Point::BottomRight, 0.0, 0.0),
            ];
            let data = model.region_data.entry(rh).or_default();
            let same = data.anchors.len() == 2
                && data
                    .anchors
                    .iter()
                    .zip(&pair)
                    .all(|(a, b)| anchor_bits_eq(a, b));
            if !same {
                data.anchors.clear();
                data.anchors.extend_from_slice(&pair);
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;
    Ok(())
}
