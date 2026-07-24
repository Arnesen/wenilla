//! Frame method-table cluster: event registration + script handlers (`RegisterEvent`/
//! `UnregisterEvent`/`SetScript`/`GetScript`), the drag-gesture registration (`RegisterForDrag`,
//! decision 0216 §3), and region creation (`CreateTexture`/`CreateFontString`). Split out of
//! [`super`] purely for size — see its module doc for the shared id/handle plumbing and
//! method-table wiring.

use std::collections::HashSet;

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::script::region::region_wrapper;
use crate::script::{Model, RegionData, REG_SCRIPTS, SCRIPT_KINDS};
use crate::widget::RegionKind;

use super::{decode_id, draw_layer_from_str, frame_handle_of, publish_global};

/// Populate `m`'s event/script and region-creation methods (see the module doc).
pub(super) fn install(lua: &Lua, m: &Table) -> mlua::Result<()> {
    // Events + scripts
    m.set(
        "RegisterEvent",
        lua.create_function(|lua, (this, event): (Table, String)| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            // Ordered listener list (the client's SignalEvent walks a LIST — order is law);
            // re-register keeps the original position (no duplicate fire).
            let list = model.event_to_frames.entry(event.clone()).or_default();
            if !list.contains(&h) {
                list.push(h);
            }
            model.frame_events.entry(h).or_default().insert(event);
            Ok(())
        })?,
    )?;
    m.set(
        "UnregisterEvent",
        lua.create_function(|lua, (this, event): (Table, String)| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            if let Some(list) = model.event_to_frames.get_mut(&event) {
                list.retain(|x| x != &h);
            }
            if let Some(set) = model.frame_events.get_mut(&h) {
                set.remove(&event);
            }
            Ok(())
        })?,
    )?;
    m.set(
        "SetScript",
        lua.create_function(
            |lua, (this, name, func): (Table, String, Option<Function>)| {
                set_script(lua, &this, &name, func)
            },
        )?,
    )?;
    m.set(
        "GetScript",
        lua.create_function(|lua, (this, name): (Table, String)| get_script(lua, &this, &name))?,
    )?;
    // RegisterForDrag(...varargs of button names) — the drag-gesture twin of `RegisterForClicks`
    // (decision 0216 §3), but on the SHARED table: any Frame can be a drag source, not just a
    // Button. Replace-the-set semantics (empty varargs clears); `crate::script::cursor`'s
    // arm/start/release path consults the set case-insensitively (the `RegisterForClicks`
    // precedent). Pruning on frame destroy: see [`Model::drag_registered`]'s doc — nothing in
    // this engine destroys a frame yet, so this map is in the same boat as `scripts`/
    // `frame_events`.
    m.set(
        "RegisterForDrag",
        lua.create_function(|lua, (this, args): (Table, MultiValue)| {
            let h = frame_handle_of(lua, &this)?;
            let mut set = HashSet::new();
            for v in args.iter() {
                if let Value::String(s) = v {
                    set.insert(s.to_str()?.to_string());
                }
            }
            let mut model = lua.app_data_mut::<Model>().expect("model");
            model.drag_registered.insert(h, set);
            Ok(())
        })?,
    )?;
    // Regions
    m.set(
        "CreateTexture",
        lua.create_function(
            |lua, (this, name, layer): (Table, Option<String>, Option<String>)| {
                create_region(lua, &this, RegionKind::Texture, name, layer)
            },
        )?,
    )?;
    m.set(
        "CreateFontString",
        lua.create_function(
            |lua, (this, name, layer): (Table, Option<String>, Option<String>)| {
                create_region(lua, &this, RegionKind::FontString, name, layer)
            },
        )?,
    )?;

    Ok(())
}

fn set_script(lua: &Lua, this: &Table, name: &str, func: Option<Function>) -> mlua::Result<()> {
    let kind = SCRIPT_KINDS
        .iter()
        .copied()
        .find(|&k| k.eq_ignore_ascii_case(name))
        .ok_or_else(|| mlua::Error::runtime(format!("SetScript: unsupported script '{name}'")))?;
    let h = frame_handle_of(lua, this)?;
    let id = lua.app_data_mut::<Model>().expect("model").frame_id(h);

    // Store the closure Lua-side (REG_SCRIPTS[id][kind]); update the Rust presence mirror.
    let scripts: Table = lua.named_registry_value(REG_SCRIPTS)?;
    let per: Table = match scripts.get::<Value>(id)? {
        Value::Table(t) => t,
        _ => {
            let t = lua.create_table()?;
            scripts.set(id, t.clone())?;
            t
        }
    };
    match func {
        Some(f) => {
            per.set(kind, f)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            model.scripts.entry(h).or_default().insert(kind);
        }
        None => {
            per.set(kind, Value::Nil)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            if let Some(set) = model.scripts.get_mut(&h) {
                set.remove(&kind);
            }
        }
    }
    Ok(())
}

fn get_script(lua: &Lua, this: &Table, name: &str) -> mlua::Result<Value> {
    let kind = match SCRIPT_KINDS
        .iter()
        .copied()
        .find(|&k| k.eq_ignore_ascii_case(name))
    {
        Some(k) => k,
        None => return Ok(Value::Nil),
    };
    let id = decode_id(this)?;
    let scripts: Table = lua.named_registry_value(REG_SCRIPTS)?;
    match scripts.get::<Value>(id)? {
        Value::Table(t) => t.get::<Value>(kind),
        _ => Ok(Value::Nil),
    }
}

fn create_region(
    lua: &Lua,
    this: &Table,
    kind: RegionKind,
    name: Option<String>,
    layer: Option<String>,
) -> mlua::Result<Table> {
    let owner = frame_handle_of(lua, this)?;
    let dl = layer
        .as_deref()
        .and_then(draw_layer_from_str)
        .unwrap_or_default();

    let id = {
        let mut model = lua.app_data_mut::<Model>().expect("model");
        let rh = model
            .arena
            .create_region(owner, kind, dl, 0)
            .ok_or_else(|| mlua::Error::runtime("CreateTexture/FontString: dead owner frame"))?;
        model.region_data.insert(rh, RegionData::default());
        model.region_id(rh)
    };

    let wrapper = region_wrapper(lua, id)?;
    if let Some(name) = name {
        publish_global(lua, &name, &wrapper)?;
        // Publish into the region-name registry too (first-wins, the frame rule) — this is what
        // lets a sibling region's SetPoint name us as its `relativeTo` (see `resolve_target`).
        let mut model = lua.app_data_mut::<Model>().expect("model");
        model.region_names.entry(name).or_insert(id);
    }
    Ok(wrapper)
}
