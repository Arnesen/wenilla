//! Region method-table cluster: **layout** — size, anchors and the resolved-rect readers.
//! Split out of `region.rs` at the 0716 file-size budget.

use mlua::{Lua, Table, Value};

use crate::layout::{Anchor, Point};
use crate::script::object::{anchor_bits_eq, frame_wrapper, point_name};
use crate::script::{Model, SCREEN};

/// Resolve `self` (a region wrapper) to its live [`RegionHandle`].
use super::{
    measured_wh, region_handle_of, region_owner_id, region_set_point, resolve_target, size_bits_eq,
};

/// Populate `m`'s layout methods (see the module doc).
pub(super) fn install(lua: &Lua, m: &Table) -> mlua::Result<()> {
    // Region explicit size — fills the axes the region's anchors don't pin (unread under an
    // implicit SetAllPoints's two corners; decision 1310).
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
    // in object.rs). Every drawable region carries anchors (authored or the creation-path
    // implicit anchor, decision 1310) and reads its resolved rect; a templateless Lua region
    // nobody anchored never resolves → nil, same as pre-resolve.
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

    // ── The rest of the Region map `0xcf54b4` (wow-re `font-object-lua-surface.md`) ──────────────
    //
    // These four landed together because the MAP is the unit, not the name. `SetParent` shipped
    // alone when one addon line named it, and its own getter stayed missing for months — which is
    // how `TheoryCraft\TheoryCraftUI.lua:720` (`buttontext:GetParent()`, a FontString) died every
    // session, and how the per-kind census came to read `115 GetParent (missing on Texture,
    // FontString)`. The set is closed and byte-verified, so it is asserted as a set in
    // `tests/reference_surface.rs` rather than grown a name at a time.

    // GetParent() → the OWNER frame's wrapper. A region always has one (`region_owner_id` falls
    // back to the owner for every unresolved case), so unlike the frame twin this never answers nil.
    m.set(
        "GetParent",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let owner = {
                let mut model = lua.app_data_mut::<Model>().expect("model");
                region_owner_id(&mut model, rh)
            };
            frame_wrapper(lua, owner)
        })?,
    )?;

    // GetCenter() → the resolved rect's midpoint, or a nil PAIR before the first resolve — the same
    // contract, and the same source, as the GetLeft/GetRight/GetTop/GetBottom readers above.
    //
    // **Deliberately unscaled, where the frame twin divides by GetEffectiveScale.** The region edge
    // readers report raw resolved units; scaling only the centre would make `GetCenter()` disagree
    // with `(GetLeft() + GetRight()) / 2` on any scaled subtree — a contradiction inside one method
    // table is worse than a missing division, and regions have no scale of their own to divide by.
    m.set(
        "GetCenter",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(match model.region_resolved.get(&rh) {
                Some(r) => (
                    Value::Number(f64::from((r.left + r.right) * 0.5)),
                    Value::Number(f64::from((r.bottom + r.top) * 0.5)),
                ),
                None => (Value::Nil, Value::Nil),
            })
        })?,
    )?;

    // GetNumPoints() → how many anchors this region carries. Absent on our FRAMES too, which is the
    // same drift one table up; this side is what the corpus named.
    m.set(
        "GetNumPoints",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model
                .region_data
                .get(&rh)
                .map_or(0, |d| d.anchors.len() as i64))
        })?,
    )?;

    // GetPoint([n]) → point, relativeTo, relativePoint, xOfs, yOfs — the n-th (1-based, default
    // first) anchor, mirroring the frame twin including its out-of-range answer (five nils).
    //
    // **The relativeTo dispatch is the part a frame-shaped copy gets wrong.** Region anchors live in
    // ONE id space with frames and may target a sibling REGION — the real XML does it everywhere
    // (`region.rs`'s `resolve_target`: frames first, then the region-name registry). So the id is
    // matched against both tables and answered with the matching wrapper kind; handing back a frame
    // wrapper for a region id would be a working-looking handle onto the wrong object.
    m.set(
        "GetPoint",
        lua.create_function(|lua, (this, n): (Table, Option<i64>)| {
            let rh = region_handle_of(lua, &this)?;
            let anchor = {
                let model = lua.app_data_ref::<Model>().expect("model");
                let idx = (n.unwrap_or(1).max(1) - 1) as usize;
                model
                    .region_data
                    .get(&rh)
                    .and_then(|d| d.anchors.get(idx))
                    .cloned()
            };
            let Some(a) = anchor else {
                return Ok((Value::Nil, Value::Nil, Value::Nil, Value::Nil, Value::Nil));
            };
            let rel = {
                let is_frame = {
                    let model = lua.app_data_ref::<Model>().expect("model");
                    model.id_to_frame.contains_key(&a.relative_to)
                };
                if a.relative_to == SCREEN {
                    Value::Nil
                } else if is_frame {
                    Value::Table(frame_wrapper(lua, a.relative_to)?)
                } else {
                    Value::Table(super::region_wrapper(lua, a.relative_to)?)
                }
            };
            Ok((
                Value::String(lua.create_string(point_name(a.point))?),
                rel,
                Value::String(lua.create_string(point_name(a.relative_point))?),
                Value::Number(f64::from(a.x_off)),
                Value::Number(f64::from(a.y_off)),
            ))
        })?,
    )?;
    Ok(())
}
