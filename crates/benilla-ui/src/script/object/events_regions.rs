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
    // `UnregisterAllEvents()` — drop every registration this frame holds, in one call.
    //
    // 10 corpus addons stop on it (decision 1195), and the idiom is why: an addon's "disable me"
    // path is `self.frame:UnregisterAllEvents()`, and a library that pools frames calls it before
    // handing one out. Unregistering them one by one is not equivalent — the caller does not know
    // what it registered, which is the whole point of the verb.
    m.set(
        "UnregisterAllEvents",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            // Take the frame's OWN set first, then walk only the events it actually held — the
            // alternative (sweeping every listener list in the model) is O(all events) per call,
            // and libraries call this in loops over a frame pool.
            let events = model.frame_events.remove(&h).unwrap_or_default();
            for event in events {
                if let Some(list) = model.event_to_frames.get_mut(&event) {
                    list.retain(|x| x != &h);
                }
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

/// `SetScript(name, func)` — store the closure against one of [`SCRIPT_KINDS`], or **raise**.
///
/// ## Why raising is the right answer for a script we cannot fire
///
/// The error is loud and it blocks the addon at load, which looks like the worse outcome and is
/// not: a script name this host *accepts* and never *fires* is a handler that silently never runs,
/// with nothing anywhere saying so. That is the failure mode decisions 1203, 1205 and 1211 each
/// recorded from a different direction, and it is strictly harder to find than a load error naming
/// the exact call. So the rule for this list is one line long: **a name is accepted only once
/// something fires it.**
///
/// The 1.12 script set is fully carved (wow-re `system/ui/scratch/rf28-typed-widget-loadxml.md`
/// l.10-18 for the base map `0x76a0d0`, the per-type sections for each widget's additions;
/// `system/ui/ui.md` l.544-556 summarises it), so what is missing here is never a mystery — it is a
/// deliberate not-yet. What the corpus actually asks for, and why each answer is what it is:
///
/// * **`OnKeyDown` / `OnKeyUp` / `OnChar`** (14 + 1 + 4 corpus sites over 13 addons) — **raising.**
///   They are in the base map, so *every* frame type has them, and the delivery mechanism is
///   `EnableKeyboard` + the hit-test root's **kind-0/kind-1 index** (wow-re
///   `scratch/scripts-auto-enable.md` §1-2: `0x76af00(kind, -1)`, `OnChar` = kind 0,
///   `OnKeyDown`/`OnKeyUp` = kind 1) with the frame-script pre-gate `0x76bba0` consuming the key
///   ahead of the whole binding dispatch (`scratch/keybinding-dispatch-law.md` §1). benilla has
///   none of that: no `EnableKeyboard`, no keyboard index, and its keys go straight to the focused
///   EditBox (whose C++ override replaces these slots anyway — RF-0082 §2). Accepting the names
///   before the index exists is the exact silent-no-op trap above; the ordering/consumption law is
///   dispatched to wow-re.
/// * **`OnCursorChanged`** (4 sites over 3 addons — all of them the Era `ScrollingEdit_OnCursorChanged`
///   auto-scroll idiom) — **raising.** It is the EditBox's own slot (RF-28 `+0x428`), fired by the
///   caret flush `0x77da80` with **four float caret-POSITION args**, and caret geometry is the one
///   thing this engine deliberately does not have: text is measured host-side. Accepting it would
///   hand every caller four zeros, which for its single idiom means a scroll box that silently
///   never follows the caret.
/// * **`OnAttributeChanged`** (1 site, `Roid-Macros`) — **raising, permanently.** It is 2.0's secure
///   frame/attribute system; there is no such slot in any 1.12 resolver. That addon is asking for a
///   later client and should hear so.
/// * **`OnHorizontalScroll` · `OnHyperlinkEnter` · `OnHyperlinkLeave` · `OnMessageScrollChanged` ·
///   `OnUpdateModel` · `OnAnimFinished` · `OnMovieFinished`/`ShowSubtitle`/`HideSubtitle` ·
///   `OnInputLanguageChanged`** — **raising.** Real 1.12 slots that we do not
///   fire, and measured at **zero** call sites across the 218-addon corpus, so there is nothing to
///   weigh against the trap: they land when their mechanism does (horizontal scroll isn't modeled at
///   all — see [`crate::script::scrollframe`]'s module doc).
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

#[cfg(test)]
mod tests {
    use crate::script::UiScript;

    /// `UnregisterAllEvents` drops every registration the frame holds and leaves every other
    /// frame's alone — the "disable me" path 10 corpus addons stop on (decision 1195).
    ///
    /// The second half is the one worth asserting: the cheap implementation (sweep every listener
    /// list) and the correct one differ only when a *second* frame shares an event, which is the
    /// normal case for `PLAYER_ENTERING_WORLD`.
    #[test]
    fn unregister_all_events_clears_one_frame_and_only_that_frame() {
        let mut s = UiScript::new().unwrap();
        s.run(
            r#"
            Mine  = CreateFrame("Frame", "UnregAllMine")
            Yours = CreateFrame("Frame", "UnregAllYours")
            Seen = {}
            for _, f in ipairs({ Mine, Yours }) do
                f:RegisterEvent("PLAYER_ENTERING_WORLD")
                f:RegisterEvent("PLAYER_LOGIN")
                f:SetScript("OnEvent", function() Seen[event] = (Seen[event] or 0) + 1 end)
            end
            "#,
        )
        .unwrap();

        s.fire_event("PLAYER_LOGIN", vec![]);
        assert_eq!(s.eval::<i64>("return Seen.PLAYER_LOGIN").unwrap(), 2);

        s.run("Mine:UnregisterAllEvents()").unwrap();
        s.run("Seen = {}").unwrap();
        s.fire_event("PLAYER_LOGIN", vec![]);
        s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
        assert_eq!(
            s.eval::<i64>("return Seen.PLAYER_LOGIN").unwrap(),
            1,
            "the other frame's registration must survive — they shared the event"
        );
        assert_eq!(
            s.eval::<i64>("return Seen.PLAYER_ENTERING_WORLD").unwrap(),
            1
        );

        // Idempotent, and harmless on a frame that never registered anything.
        s.run("Mine:UnregisterAllEvents() CreateFrame(\"Frame\"):UnregisterAllEvents()")
            .unwrap();
    }
}
