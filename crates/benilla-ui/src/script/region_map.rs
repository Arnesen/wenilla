//! **The Region method map, reached by frames and regions alike** — the 19 names of the client's
//! `0x87c9b8` table, each ONE callable that works on a Frame, a Texture or a FontString.
//!
//! ## The fact this exists for (byte-verified in `WoW.exe`, decision 1501)
//!
//! 1.12.1's widget inheritance is not a Lua metatable chain: every class owns a flat
//! `{name, lua_CFunction}` `.data` table, and its lookup probes that table and, on a miss,
//! **tail-calls exactly one base class's lookup** (wow-re
//! `system/ui/scratch/widget-api-batch-benilla.md`):
//!
//! ```text
//! Region   0x87c9b8 (19)  lookup 0x7a2ea0  → TERMINAL (root)
//! ├─ Frame        0x878ec0 (68)  lookup 0x778590 → 0x7a2ea0
//! ├─ Texture      0x87c128 (22)  lookup 0x79c620 → 0x7a2ea0
//! └─ FontString   0x87c1d8 (32)  lookup 0x79ee20 → 0x7a2ea0
//! ```
//!
//! Parsing the two tables out of the image directly, **`Frame ∩ Region = ∅`**: not one of the 19
//! is re-registered by Frame, so every Frame-derived class reaches the *same* function a Texture
//! does. `WorldFrame.GetHeight` **is** `0x7a2030`, byte-for-byte the function
//! `someTexture:GetHeight()` resolves to.
//!
//! That is why this is not a curiosity. The idiom
//!
//! ```lua
//! local _Height = WorldFrame.GetHeight     -- pull the method off ANY widget, once
//! …
//! local h = _Height(someTexture)           -- and apply it to ANY other
//! ```
//!
//! is ordinary, correct 1.12 Lua. Quiver's `Api/Index.wow.lua` is built on it
//! (`_Height = WorldFrame.GetHeight`, `_Width = WorldFrame.GetWidth`), and benilla raised
//! `stale or invalid frame handle` on it, from inside the addon's `VARIABLES_LOADED` handler,
//! **before** the handler's last line got to publish `Quiver.CastPetAction` — which is the whole
//! of bug B267. One split method surface, one nil field, one dead addon.
//!
//! ## Why it is a bridge rather than a merge
//!
//! Our two implementations of each name are not redundant: a frame's size reads `layout_inputs`
//! and divides its edges by `GetEffectiveScale`, a region's reads `region_data` and does not; a
//! frame's `GetParent` can answer nil, a region's never can; a region's `GetPoint` resolves a
//! `relativeTo` that may be a sibling *region*. Those differences are correct, and they are the
//! same shape the reference has: `Region:GetHeight 0x7a2030` reads the receiver's layout
//! sub-object and calls `[vtable+0x20]`, which `CSimpleFontString` **overrides**. One name, one
//! entry point, per-kind behaviour behind it.
//!
//! So each of the 19 becomes one function that resolves the receiver *first* and then hands the
//! whole argument list to the arm that owns that kind — and that single function is written into
//! every method table the reference's chain would reach: the frame table, the region table, the
//! Texture and FontString leaf tables, and the title region's narrower copy. Identity holds the
//! way the binary's does — `WorldFrame.GetHeight == someTexture.GetHeight` is true here too.
//!
//! **Exactly the 19, and no more.** `Show`/`Hide`/`IsShown`/`IsVisible`/`SetAlpha`/`GetAlpha` look
//! like they belong and do not: Frame and Texture each register their *own*, at different
//! addresses (`texture-fontstring-method-split.md` §3), so `WorldFrame.Show(someTexture)` fails on
//! the real client and must keep failing here. `SetSize` is in neither table — it is 1.12-absent
//! and ours, so it stays where it is. The map is the unit.

use mlua::{Function, Lua, MultiValue, Table, Value};

use super::object::decode_id;
use super::{
    Model, REGION_MAP_METHODS, REG_FONTSTRING_METHODS, REG_FRAME_METHODS, REG_REGION_METHODS,
    REG_TEXTURE_METHODS, REG_TITLE_METHODS,
};

/// Which side of the object model a wrapper's `T[0]` id names. Ids come from one counter
/// ([`Model::next_id`]), so a region id can never be mistaken for a frame's.
enum Side {
    Frame,
    Region,
}

/// `Err` when the table is not a widget wrapper at all (that error names the real problem and is
/// worth keeping); `Ok(None)` when it *is* one whose widget no longer exists.
fn side_of(lua: &Lua, this: &Table) -> mlua::Result<Option<Side>> {
    let id = decode_id(this)?;
    let model = lua.app_data_ref::<Model>().expect("model app_data");
    Ok(if model.id_to_frame.contains_key(&id) {
        Some(Side::Frame)
    } else if model.id_to_region.contains_key(&id) {
        Some(Side::Region)
    } else {
        None
    })
}

/// Replace each of the 19 Region-map entries in every table the chain reaches with one shared
/// function that dispatches on the receiver.
///
/// Runs **after** `region::install` (which is what builds the region, leaf and title tables) and
/// after `install_frame_methods`; every table it names is already populated by then.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let frame: Table = lua.named_registry_value(REG_FRAME_METHODS)?;
    let region: Table = lua.named_registry_value(REG_REGION_METHODS)?;
    let leaves: Vec<Table> = [
        REG_TEXTURE_METHODS,
        REG_FONTSTRING_METHODS,
        REG_TITLE_METHODS,
    ]
    .into_iter()
    .map(|k| lua.named_registry_value::<Table>(k))
    .collect::<mlua::Result<_>>()?;

    for name in REGION_MAP_METHODS {
        // Both arms must already exist. A missing one is not a thing to paper over with a
        // one-sided shared function: the reference gives every widget all 19, so a gap on either
        // side is a hole in the surface and says so here rather than at some addon's call site.
        // (`GetNumPoints` was exactly that hole on the frame side until this module went in.)
        let on_frame: Function = frame.get(name).map_err(|_| {
            mlua::Error::runtime(format!("Region map: the FRAME table has no {name}"))
        })?;
        let on_region: Function = region.get(name).map_err(|_| {
            mlua::Error::runtime(format!("Region map: the REGION table has no {name}"))
        })?;
        let shared = lua.create_function(move |lua, args: MultiValue| {
            // The receiver is argument 1 on every one of the 19 — a method call always passes it,
            // and a pulled-off `Api._Height(x)` call passes it as the only argument.
            let Some(Value::Table(this)) = args.iter().next().cloned() else {
                return Err(mlua::Error::runtime(
                    "expected a frame or region as the first argument",
                ));
            };
            match side_of(lua, &this)? {
                Some(Side::Frame) => on_frame.call::<MultiValue>(args),
                Some(Side::Region) => on_region.call::<MultiValue>(args),
                // A wrapper whose widget is gone. Deliberately ONE message for both sides — at
                // this point the receiver's kind is exactly what could not be established, and
                // each side's old wording claimed it.
                None => Err(mlua::Error::runtime("stale or invalid widget handle")),
            }
        })?;
        frame.set(name, shared.clone())?;
        region.set(name, shared.clone())?;
        for leaf in &leaves {
            leaf.set(name, shared.clone())?;
        }
    }
    Ok(())
}
