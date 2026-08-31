//! Frame method-table cluster: event registration + script handlers (`RegisterEvent`/
//! `UnregisterEvent`/`SetScript`/`GetScript`), the drag-gesture registration (`RegisterForDrag`,
//! decision 0216 §3), and region creation (`CreateTexture`/`CreateFontString`). Split out of
//! [`super`] purely for size — see its module doc for the shared id/handle plumbing and
//! method-table wiring.

use std::collections::HashSet;

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::script::binding_abi::optional_string;
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
    // `CreateTexture(name, layer, inheritsFrom)` — the third argument accepted for the same reason
    // and with the same resolver, though the demand is a rounding error beside FontString's: of
    // 1329 corpus call sites exactly TWO pass a third argument, and one of those passes `nil`
    // (CustomNameplates:437, a four-argument later-client form). Accepted rather than skipped
    // because dropping it silently is the very class this fixes, and one real site is still a site.
    m.set(
        // **The three argument positions are NOT one rule, and the fourth is the odd one.** Verified
        // together in wow-re `ui/scratch/xml-template-name-lookup.md` §5.2:
        //
        //  · `name` and `layer` go through `0x6f3510` (is-number-**or**-string) then `0x6f3690`, and
        //    **the result is never tested** — so a table is simply absent (unnamed / the pre-staged
        //    default layer), and a NUMBER is accepted and stringified.
        //  · `inherits` is gated by a raw `lua_type(L, 4) == LUA_TSTRING` (`0x773d28` / `0x773b06`)
        //    asked BEFORE anything reads the slot — so a table AND a number are both ignored, and the
        //    non-string leg jumps to the very block the two template-HIT legs jump to, with both
        //    template handles NULL. It is the success path, not a bolted-on case.
        //
        // **Nothing here raises.** This mattered: benilla typed all three `Option<String>`, so mlua
        // raised `bad argument #4: error converting Lua table to String` — and pfUI, the most-installed
        // 1.12 addon, builds every buff-stack label as
        // `f.buffs[i]:CreateFontString(nil, "OVERLAY", f.buffs[i])`, passing the BUTTON as the third
        // argument. The real client constructs it, ignores the argument, and logs nothing.
        "CreateTexture",
        lua.create_function(
            |lua, (this, name, layer, inherits): (Table, Value, Value, Value)| {
                let name = optional_string(lua, &name);
                let layer = optional_string(lua, &layer);
                let wrapper = create_region(lua, &this, RegionKind::Texture, name, layer)?;
                if let Some(from) = strict_string_arg(&inherits).filter(|s| !s.is_empty()) {
                    apply_region_inherits(lua, &wrapper, &from)?;
                }
                Ok(wrapper)
            },
        )?,
    )?;
    // ── The title region: Frame:CreateTitleRegion() / GetTitleRegion() ──────────────────────────
    //
    // wow-re `system/ui/scratch/widget-api-batch-benilla.md` Q6 (`0x773910` / `0x773820`). Four
    // details, each one a coin-flip a reimplementation loses:
    //
    //  · **It reads NO argument at all** — nothing in `0x773910`-`0x773a1f` touches Lua index 2. So
    //    `CustomNameplates/options.lua:73`'s `CreateTitleRegion(optionsFrame)` is harmless and
    //    identical to the no-arg call, rather than an error or a differently-parented region.
    //  · **It is IDEMPOTENT, destructively.** One region per frame (`CSimpleFrame+0xA8`): a second
    //    call runs ClearAllPoints on the existing one and returns THE SAME OBJECT — so calling it
    //    on an XML-declared `<TitleRegion>` silently wipes that region's anchors. Returning a
    //    fresh region instead would leave the first one hit-testing forever.
    //  · **A fresh one has NO anchors** and does nothing until `SetPoint`/`SetAllPoints`. Both
    //    corpus consumers immediately call `SetAllPoints`, which is the whole-window drag idiom.
    //  · **`GetTitleRegion` answers 1 value (nil)** when there is none — note the asymmetry with
    //    `GetBackdrop`, which answers 0 values. Q6 flags it explicitly; both converged
    //    independently there, so both are reproduced here.
    // **ONE NAMED DIVERGENCE, because it is a superset and 1189 is what a superset costs.** Q6 says
    // the object answers *exactly* the 19 Region methods — no Show/Hide, no scripts, no textures.
    // Ours answers the whole shared region table, because Texture/FontString/Title use one
    // metatable here, so `titleRegion:SetTexture(…)` is accepted where the reference would raise
    // `attempt to call method`. It is inert rather than wrong — a title region never draws (the
    // extract skips its kind outright), so a texture or a Hide set on one changes nothing — but an
    // addon that FEATURE-DETECTS would see a method the real client does not have. Splitting the
    // metatable is the fix; it is not free, and nothing in the corpus asks, so this is recorded
    // rather than hidden.
    m.set(
        "CreateTitleRegion",
        lua.create_function(|lua, this: Table| {
            let owner = frame_handle_of(lua, &this)?;
            let existing = {
                let model = lua.app_data_ref::<Model>().expect("model");
                model.arena.frame(owner).and_then(|f| f.title_region)
            };
            if let Some(rh) = existing {
                let id = {
                    let mut model = lua.app_data_mut::<Model>().expect("model");
                    let d = model.region_data.entry(rh).or_default();
                    let changed = !d.anchors.is_empty();
                    d.anchors.clear();
                    if changed {
                        model.touch_layout();
                    }
                    model.region_id(rh)
                };
                return region_wrapper(lua, id);
            }
            let wrapper = create_region(lua, &this, RegionKind::Title, None, None)?;
            let id = decode_id(&wrapper)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let rh = *model
                .id_to_region
                .get(&id)
                .expect("the region we just created is registered");
            if let Some(f) = model.arena.frame_mut(owner) {
                f.title_region = Some(rh);
            }
            Ok(wrapper)
        })?,
    )?;
    m.set(
        "GetTitleRegion",
        lua.create_function(|lua, this: Table| {
            let owner = frame_handle_of(lua, &this)?;
            let found = {
                let mut model = lua.app_data_mut::<Model>().expect("model");
                model
                    .arena
                    .frame(owner)
                    .and_then(|f| f.title_region)
                    .map(|rh| model.region_id(rh))
            };
            match found {
                Some(id) => Ok(Value::Table(region_wrapper(lua, id)?)),
                // ONE value, and it is nil — not zero values. See the `GetBackdrop` asymmetry above.
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // The THIRD argument is `inheritsFrom`, and we were dropping it on the floor.
    //
    // 49 corpus call sites across 5 distinct addons pass one — AckisRecipeList (28),
    // CustomNameplates (10), _LazyPig (6), LibAboutPanel (4), ColorPickerPlus (1) — and **every one
    // of them names a FONT OBJECT**: GameFontNormalSmall (15), GameFontNormal (11),
    // GameFontHighlightSmall (10), GameFontHighlight (9), GameFontNormalLarge (3), GameFontDisable.
    // Five separate addons, so this is not one library file replicated (1207 checked, not assumed).
    // Ignoring it is 1203's class exactly: the addon asks for a font, we accept the call, and the
    // string comes out in whatever the default is with no failure anywhere.
    //
    // The resolution order is carved (`0x773c30`): the FONT-object registry FIRST (`0x773d39`,
    // create=0), and only on a font miss the template registry (`0x773d47`). This routes through
    // the `SetFontObject` binding rather than reaching into the model, which is the same call the
    // XML `<FontString inherits=>` path makes — one implementation of "apply a font object", not
    // two that can drift.
    m.set(
        "CreateFontString",
        lua.create_function(
            |lua, (this, name, layer, inherits): (Table, Value, Value, Value)| {
                let name = optional_string(lua, &name);
                let layer = optional_string(lua, &layer);
                let wrapper = create_region(lua, &this, RegionKind::FontString, name, layer)?;
                if let Some(from) = strict_string_arg(&inherits).filter(|s| !s.is_empty()) {
                    apply_region_inherits(lua, &wrapper, &from)?;
                }
                Ok(wrapper)
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
///   describes — `EnableKeyboard`/`IsKeyboardEnabled` now exist and the flag round-trips, but there
///   is still no keyboard index and no strata walk, and keys are routed straight to the focused
///   EditBox — so the names stay out until that exists, which is the whole rule above. What has to
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
            // `insert` answering true = this frame did not have the kind before — the tick's
            // OnUpdate list rides that edge (decision 1446; `scripts` has no other writer).
            if model.scripts.entry(h).or_default().insert(kind) {
                match kind {
                    "OnUpdate" => model.on_update_frames.push(h),
                    "OnSizeChanged" => model.on_size_changed_frames.push(h),
                    _ => {}
                }
            }
        }
        None => {
            per.set(kind, Value::Nil)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            if let Some(set) = model.scripts.get_mut(&h) {
                if set.remove(&kind) {
                    match kind {
                        "OnUpdate" => model.on_update_frames.retain(|&x| x != h),
                        "OnSizeChanged" => {
                            model.on_size_changed_frames.retain(|&x| x != h);
                        }
                        _ => {}
                    }
                }
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

/// `CreateTexture`/`CreateFontString`'s third argument — `inheritsFrom` — in the carved order.
///
/// **Font-object registry first, template registry second** (`0x773d39` then `0x773d47`). That
/// order is not cosmetic: `inherits=` is one attribute over two namespaces, and every corpus caller
/// of this argument names a font object, so a template-first resolver would miss all 49 of them.
///
/// A name in NEITHER registry raises, which is the same contract 1253 established for `CreateFrame`
/// and the same bytes (`luaL_error`, `0x87957c` / `0x879544`, which never returns).
///
/// A name that IS a registered template but not a font object is a deliberate gap rather than a
/// silent one: the lookup succeeded, so the reference would not raise, and re-interpreting a
/// template node onto an existing region is a different mechanism from applying a font object. It
/// warns and leaves the region usable — the distinction 1253 drew between *the lookup missing* and
/// *the result being unusable*. Zero corpus callers reach it.
fn apply_region_inherits(lua: &Lua, wrapper: &Table, from: &str) -> mlua::Result<()> {
    use mlua::ObjectLike;
    // The font object is applied through the binding the XML path uses, so there is one
    // implementation of "apply a font object" rather than two that can drift apart.
    if wrapper.call_method::<()>("SetFontObject", from).is_ok() {
        return Ok(());
    }
    let is_template = {
        let model = lua.app_data_ref::<Model>().expect("model");
        let templates = model.framexml_templates.borrow();
        templates.contains_key(from) || templates.keys().any(|k| k.eq_ignore_ascii_case(from))
    };
    if is_template {
        let mut model = lua.app_data_mut::<Model>().expect("model");
        model.warnings.push(format!(
            "CreateTexture/CreateFontString: '{from}' is a registered TEMPLATE, not a font object;              the region is created but the template's content is not applied (no corpus caller              does this)"
        ));
        return Ok(());
    }
    Err(mlua::Error::runtime(format!(
        "Couldn't find inherited node \"{from}\""
    )))
}

/// **The `lua_type(L, idx) == LUA_TSTRING` gate, asked before anything reads the slot** — the
/// region constructors' `inherits` argument and nowhere else (`0x773d28` for `CreateFontString`,
/// `0x773b06` for `CreateTexture`).
///
/// Strictly narrower than [`optional_string`]: a **number is refused here**, because the tag is
/// tested raw with no coercion. That one-argument difference is the whole reason
/// `f:CreateTexture(nil, nil, 5)` silently ignores the 5 while `CreateFrame("Frame", nil, nil, 5)`
/// **raises** `Couldn't find inherited node "5"` — `CreateFrame` runs `lua_tostring(L, 4)` BEFORE
/// its own `cmp 4`, and that retags the number's stack slot in place, so it arrives as a string.
/// Same value, opposite outcome, from argument order alone.
fn strict_string_arg(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => s.to_str().ok().map(|s| s.to_owned()),
        _ => None,
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
