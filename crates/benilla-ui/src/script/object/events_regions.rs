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
    // HasScript(name) — "can this widget carry that script kind at all", NOT "does it have one
    // set". The distinction is the whole point of the verb: every caller in the corpus uses it to
    // decide whether it is safe to hook, then reads the current handler separately.
    //
    //     if parent:HasScript("OnMouseDown") then                    -- Tablet-2.0.lua:2409
    //         local script = parent:GetScript("OnMouseDown")         -- FuBar-compat:2422
    //         parent:SetScript("OnMouseDown", function() … end)
    //     end
    //
    // It was the top session-start blocker: 32 of the 39 `attempt to call method` failures were
    // this one name, across the Tablet/FuBar/oRA2 stack.
    //
    // ANSWERED FROM THE FLAT `SCRIPT_KINDS`, and that is a deliberate, stated approximation rather
    // than the full answer. The reference's table is PER WIDGET TYPE — a plain Frame really does
    // answer false for `OnClick` — so ours is over-permissive for the type-specific kinds. It is
    // exactly right for the base kinds, which is what every corpus caller asks about
    // (`OnMouseDown` here; a Frame genuinely has it). The cost of the over-permission is small and
    // worth naming: an addon that asks about a type-specific kind installs a handler that never
    // fires, where the reference would have had it skip. That is the same latitude `SetScript`
    // already takes — this verb does not widen it, it just stops lying about its existence.
    //
    // Making it exact means per-type SCRIPT_KINDS, which newly RAISES on 30 sites that work today
    // (9 in our own FrameXML). That is behaviour removal and belongs in its own change, with its
    // own measurement; it is not a tail on this one.
    m.set(
        "HasScript",
        lua.create_function(|_, (_this, name): (Table, String)| {
            Ok(SCRIPT_KINDS.iter().any(|k| k.eq_ignore_ascii_case(&name)))
        })?,
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
/// * **`OnKeyDown` / `OnKeyUp` / `OnChar`** (14 + 1 + 4 corpus sites over 13 addons) — **raising,
///   and now unblocked**: the delivery law was the second §5 this work dispatched, and it landed
///   (wow-re `scratch/frame-key-script-delivery.md`). benilla still has none of the machinery it
///   describes — no `EnableKeyboard`, no keyboard index, keys routed straight to the focused
///   EditBox — so the names stay out until it exists, which is the whole rule above. What has to
///   get built, so the next pass does not have to re-derive it:
///
///   The frame must be in the hit-test root's **kind-0 / kind-1 bucket**
///   (`scratch/scripts-auto-enable.md` §1-2: `0x76af00(kind, …)`, `OnChar` = kind 0,
///   `OnKeyDown`/`OnKeyUp` = kind 1; XML `enableKeyboard` enables both, a `<Scripts>` block
///   auto-enables per handler, and Lua `SetScript` auto-enables **nothing** — so a Lua-only frame
///   is not in the bucket at all and its bound handler can never fire). The dispatcher then walks
///   **strata 8 → 0, level high → low, ties oldest-registration-first**, calling each frame's base
///   input virtual (`0x765f10` key-down → vtable `+0x60`; `0x765df0` char → `+0x5c`) and
///   **stopping at the first nonzero return**. `arg1` is a **key-name string** with no modifier
///   prefix, decoded from the *same* table the keybinding chord names use — verified byte-identical
///   over 273 codes, which is the opposite of the mouse case's two-table shape. `OnChar` gets the
///   literal character, UTF-8.
///
///   Two findings that will bite whoever implements it. The consumption gate is **existence, not
///   handling** — a 1.12 handler cannot signal "handled" (the fire's return is discarded at all
///   three sites), so merely having a key script bound suppresses the key's binding; and
///   asymmetrically, a frame with **only** an `OnKeyUp` script consumes every key-down and runs
///   nothing. And `CGWorldFrame` sits at **strata 0 / level 0** — last in the walk — which is
///   precisely why any keyboard-enabled frame pre-empts the entire binding system.
///
///   (The old gloss here called `0x76bba0` a "frame-script pre-gate" walking the index, after
///   `scratch/keybinding-dispatch-law.md` §1. That is refuted: `0x76bba0` is `CSimpleFrame::OnKeyUp`,
///   one frame's base virtual. The walk is one level up, in the dispatcher.)
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
