//! The `Button`/`CheckButton` method surface — per-kind behavior over the frame arena
//! (`CSimpleButton` `0x6eeab0` / `CSimpleCheckbox` `0x6eeb30`).
//!
//! Grounded in wow-re's byte-verified LoadXML tables (RF-28): the four state textures
//! (`Normal/Pushed/Disabled/Highlight`), the `ButtonText` fontstring + `text` attribute, the
//! `OnClick` script slot (`+0x4cc`); CheckButton runs Button's loader first and adds
//! `CheckedTexture`/`DisabledCheckedTexture` + the `checked` bool (`+0x4dc`). Which texture *shows*
//! is interaction state ([`ButtonState::region_visible`], applied at extract) — faithful to the
//! documented widget model (texture array + current pointer `+0x4c4`), not byte-pinned. Two stated
//! v1 gaps: the highlight draws with normal blending (the client ADD-blends it; the quad pass has
//! no blend modes yet), and `PushedTextOffset` is not modeled. Per-state label fonts *are*: the
//! `*FontObject` trio picks which object each state inherits and `extract` re-resolves it every
//! frame, while `SetFont` writes the button's own face/size/flags over all of them
//! ([`crate::widget::ButtonFont`]).
//!
//! Method resolution: CheckButton's table is consulted first, then Button's, then the shared frame
//! table — mirroring the client's class chain, and keeping duck-typing honest (`frame.SetChecked`
//! is nil on a plain Button; `frame.GetText` is nil on a plain Frame).

use mlua::{Lua, MultiValue, ObjectLike, Table, Value};

use super::object::{as_f32, frame_handle_of};
use super::region::region_wrapper;
use super::{event, Model, RegionData};
use crate::order::DrawLayer;
use crate::widget::{
    ButtonFont, ButtonState, FrameHandle, FrameKind, KindState, RegionHandle, RegionKind,
};

pub(super) const REG_BUTTON_METHODS: &str = "__benilla_button_methods";
pub(super) const REG_CHECKBUTTON_METHODS: &str = "__benilla_checkbutton_methods";

/// The Button state-texture/text slots (each an arena region created on first set).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Normal,
    Pushed,
    Disabled,
    Highlight,
    Checked,
    DisabledChecked,
    Text,
}

impl Slot {
    fn get(self, bs: &ButtonState) -> Option<crate::widget::RegionHandle> {
        match self {
            Slot::Normal => bs.normal,
            Slot::Pushed => bs.pushed,
            Slot::Disabled => bs.disabled,
            Slot::Highlight => bs.highlight,
            Slot::Checked => bs.checked_tex,
            Slot::DisabledChecked => bs.disabled_checked,
            Slot::Text => bs.text,
        }
    }

    fn set(self, bs: &mut ButtonState, rh: crate::widget::RegionHandle) {
        match self {
            Slot::Normal => bs.normal = Some(rh),
            Slot::Pushed => bs.pushed = Some(rh),
            Slot::Disabled => bs.disabled = Some(rh),
            Slot::Highlight => bs.highlight = Some(rh),
            Slot::Checked => bs.checked_tex = Some(rh),
            Slot::DisabledChecked => bs.disabled_checked = Some(rh),
            Slot::Text => bs.text = Some(rh),
        }
    }

    /// The slot's region kind + default draw layer: state textures under the text, the highlight
    /// in its own top layer (drawLayer table `0x811a84`), the checked marks above the state.
    fn shape(self) -> (RegionKind, DrawLayer) {
        match self {
            Slot::Normal | Slot::Pushed | Slot::Disabled => {
                (RegionKind::Texture, DrawLayer::Artwork)
            }
            Slot::Checked | Slot::DisabledChecked => (RegionKind::Texture, DrawLayer::Overlay),
            Slot::Highlight => (RegionKind::Texture, DrawLayer::Highlight),
            Slot::Text => (RegionKind::FontString, DrawLayer::Overlay),
        }
    }
}

/// Point the ButtonText at the button's NORMAL font object, so the label's *query* surface answers
/// what its paint already shows.
///
/// Before this, the two disagreed. `extract` resolves the per-state font object every frame and
/// overlays it onto a CLONE of the region's data (`extract.rs` l.114-122), so the label painted
/// correctly while `region_data.font_object` stayed `None` — and a dropdown row's
/// `GetFontObject()` answered **nil**, `GetFont()` answered **nil, nil**. Nothing errored: a
/// FontString with no font of its own is a legal state, so this was invisible in exactly the way
/// 1205's silent-drop class predicts.
///
/// The measured consequence is smaller than it first looks, and that is stated rather than implied.
/// The corpus's 65 `GetFontObject` sites are on FontStrings the addon created and linked itself
/// (Dewdrop-2.0 calls `text:SetFontObject(GameFontHighlightSmall)` one line before it reads the
/// object back), so none of them were reaching this. What WAS reaching it is the reference's own
/// `DropDownList1` OnLoad, which derives `UIDROPDOWNMENU_DEFAULT_TEXT_HEIGHT` from
/// `DropDownList1Button1NormalText:GetFont()` and got nil.
///
/// **Normal only, deliberately.** The highlight and disabled objects are transient states that
/// extract overlays at paint time; linking either here would make a resting button report its hover
/// font. The remaining divergence, stated: while a button is disabled or hovered, the reference's
/// label reports that state's font and ours still reports the normal one. The paint is unaffected
/// either way, and no corpus site reads a label's font mid-state.
///
/// `font::repaint` honours the severance mask, so a `<FontHeight>` or `SetTextColor` the label set
/// for itself survives the link (the rule wow-re pinned in `font-object-lua-surface.md`).
fn link_label_to_font_object(lua: &Lua, text: Option<RegionHandle>, name: Option<&str>) {
    let Some(rh) = text else { return };
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let Some(name) = name else {
        model.region_data.entry(rh).or_default().font_object = None;
        return;
    };
    // An unregistered name is not an error here: `SetTextFontObject` already accepted it, and the
    // loader's own log-and-continue rule (0068) owns the reporting.
    let Some(fo) = model.font_object(name).cloned() else {
        return;
    };
    let d = model.region_data.entry(rh).or_default();
    d.font_object = Some(name.to_string());
    super::font::repaint(d, &fo);
    model.touch_measure(rh);
}

/// Run `f` over a frame's Button state under one short write borrow.
fn with_button<T>(
    lua: &Lua,
    this: &Table,
    f: impl FnOnce(&mut ButtonState) -> T,
) -> mlua::Result<T> {
    let h = frame_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let frame = model
        .arena
        .frame_mut(h)
        .ok_or_else(|| mlua::Error::runtime("stale frame handle"))?;
    match &mut frame.kind_state {
        KindState::Button(bs) => Ok(f(bs)),
        _ => Err(mlua::Error::runtime("not a Button")),
    }
}

/// Write [`ButtonState::loot_slot`] — `LootButton:SetSlot`'s whole body, kept here beside the
/// other `ButtonState` writers rather than reaching into the arena from `script::loot`.
pub(super) fn set_loot_slot(lua: &Lua, this: &Table, slot: Option<u32>) -> mlua::Result<()> {
    let h = frame_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let frame = model
        .arena
        .frame_mut(h)
        .ok_or_else(|| mlua::Error::runtime("stale frame handle"))?;
    match (&frame.kind, &mut frame.kind_state) {
        (crate::widget::FrameKind::LootButton, KindState::Button(bs)) => {
            bs.loot_slot = slot;
            Ok(())
        }
        // The reference's own guard: `SetSlot` type-checks `this` against the LootButton tag via
        // vtable slot 4 (`0x4c18ee`), so calling it on a plain Button raises rather than writing a
        // field nothing would ever read.
        _ => Err(mlua::Error::runtime(
            "SetSlot: 'this' is not a LootButton widget",
        )),
    }
}

/// Get-or-create the region behind `slot`; returns its id.
fn ensure_slot(lua: &Lua, this: &Table, slot: Slot) -> mlua::Result<u32> {
    let h = frame_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let existing = match &model
        .arena
        .frame(h)
        .ok_or_else(|| mlua::Error::runtime("stale frame handle"))?
        .kind_state
    {
        KindState::Button(bs) => slot.get(bs),
        _ => return Err(mlua::Error::runtime("not a Button")),
    };
    let rh = match existing {
        Some(rh) => rh,
        None => {
            let (kind, layer) = slot.shape();
            let rh = model
                .arena
                .create_region(h, kind, layer, 0)
                .ok_or_else(|| mlua::Error::runtime("stale frame handle"))?;
            // The highlight draws ADD by contract (the live API's SetHighlightTexture default
            // blend; highlight art is authored for it — under straight alpha it reads as a
            // dark box over the icon).
            model.region_data.insert(
                rh,
                RegionData {
                    additive: slot == Slot::Highlight,
                    ..Default::default()
                },
            );
            model.touch_layout(); // a region entered the layout gate's read set (decision 0740)
                                  // A freshly built slot region gets the creation-path implicit anchor (decision 1310):
                                  // the reference's C++ string setters SetAllPoints a fresh state texture outright
                                  // (`0x778f9d`/`0x7790db` — fresh means zero anchors, so the conditional form is
                                  // equivalent), and ButtonText creation runs the FontString post-step (`0x778b96` →
                                  // `0x771480`), which seats a fresh label CENTER. The XML loader re-derives after
                                  // applying authored `<Anchors>` (see `loader/widgets.rs`); an EXISTING slot region is
                                  // never touched here — the get half of get-or-create changes no geometry.
            super::region::implicit_creation_anchor(&mut model, rh);
            if let Some(frame) = model.arena.frame_mut(h) {
                if let KindState::Button(bs) = &mut frame.kind_state {
                    slot.set(bs, rh);
                }
            }
            rh
        }
    };
    Ok(model.region_id(rh))
}

/// The shared body of `Set<State>Texture(path)` / `(r, g, b [, a])`.
fn set_slot_texture(lua: &Lua, this: &Table, slot: Slot, args: &MultiValue) -> mlua::Result<()> {
    let id = ensure_slot(lua, this, slot)?;
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let rh = *model.id_to_region.get(&id).expect("slot region id");
    let data = model.region_data.entry(rh).or_default();
    match args.front() {
        // `Set<State>Texture("")` CLEARS — the live API's blank form (ref QuestLogFrame.lua:165
        // clears the quest rows' +/- slot with exactly this), same as SetTexture(nil)/("") on a
        // plain region.
        Some(Value::String(s)) if s.to_str()?.is_empty() => {
            data.texture = None;
            data.fill = None;
        }
        Some(Value::String(s)) => {
            data.texture = Some(s.to_str()?.to_string());
            data.fill = None;
        }
        // The colour form generates a solid texture into the same slot the path form loads into
        // ([`RegionData::fill`]) — never a tint, so each clears the other.
        Some(v @ (Value::Number(_) | Value::Integer(_))) => {
            let arg = |i: usize| args.get(i).map(as_f32);
            data.fill = Some([
                as_f32(v),
                arg(1).unwrap_or(0.0),
                arg(2).unwrap_or(0.0),
                arg(3).unwrap_or(1.0),
            ]);
            data.texture = None;
        }
        _ => {}
    }
    Ok(())
}

/// The shared body of `Get<State>Texture` — the region wrapper, or nil while unset.
fn get_slot_texture(lua: &Lua, this: &Table, slot: Slot) -> mlua::Result<Value> {
    let rh = with_button(lua, this, |bs| slot.get(bs))?;
    let id = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        rh.map(|rh| model.region_id(rh))
    };
    match id {
        Some(id) => Ok(Value::Table(region_wrapper(lua, id)?)),
        None => Ok(Value::Nil),
    }
}

/// Register one `Set<X>Texture`/`Get<X>Texture` pair on `m`.
fn texture_pair(lua: &Lua, m: &Table, name: &str, slot: Slot) -> mlua::Result<()> {
    m.set(
        format!("Set{name}Texture"),
        lua.create_function(move |lua, (this, args): (Table, MultiValue)| {
            set_slot_texture(lua, &this, slot, &args)
        })?,
    )?;
    m.set(
        format!("Get{name}Texture"),
        lua.create_function(move |lua, this: Table| get_slot_texture(lua, &this, slot))?,
    )?;
    Ok(())
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;

    texture_pair(lua, &m, "Normal", Slot::Normal)?;
    texture_pair(lua, &m, "Pushed", Slot::Pushed)?;
    texture_pair(lua, &m, "Disabled", Slot::Disabled)?;
    texture_pair(lua, &m, "Highlight", Slot::Highlight)?;

    // SetText/GetText target the ButtonText fontstring (RF-28: the `text` attr routes there).
    m.set(
        "SetText",
        lua.create_function(|lua, (this, text): (Table, Option<String>)| {
            let id = ensure_slot(lua, &this, Slot::Text)?;
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let rh = *model.id_to_region.get(&id).expect("text region id");
            model.region_data.entry(rh).or_default().text = text;
            model.touch_measure(rh);
            Ok(())
        })?,
    )?;
    m.set(
        "GetText",
        lua.create_function(|lua, this: Table| {
            let rh = with_button(lua, &this, |bs| bs.text)?;
            let text = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                rh.and_then(|rh| model.region_data.get(&rh))
                    .and_then(|d| d.text.clone())
            };
            match text {
                Some(t) => Ok(Value::String(lua.create_string(&t)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    m.set(
        "GetFontString",
        lua.create_function(|lua, this: Table| get_slot_texture(lua, &this, Slot::Text))?,
    )?;

    // SetFontString(fontString) — ADOPT a caller-made FontString as this Button's label.
    //
    // Byte-carved end to end by wow-re (decision 1505,
    // `system/ui/scratch/resize-bounds-and-button-fontstring.md`
    // §5): the binding `0x780a60` is gates + a delegate to `CSimpleButton::SetFontString
    // 0x778d20`, which is the SAME function `SetText`'s lazy creation path funnels through — so
    // adopting and creating share their whole tail, and only the allocation differs.
    //
    // The idiom it exists for: build the label yourself so you can style and place it, then hand
    // it to the button so `SetText` and the per-state font machinery drive it. Quiver's
    // `Component/Select.wow.lua` builds every dropdown option row that way, and with the method
    // nil the row died on its first line — the second of B267's three walls.
    //
    // **It raises, in three distinct ways, and never silently no-ops on a bad argument** — the
    // first shape I built here was a lenient no-op and the bytes say otherwise:
    //
    //  · a **missing** argument, `nil`, a number, a string or a boolean → `lua_type(L,2) != TABLE`
    //    → `Usage: %s:SetFontString(fontstring)` (`0x87a100`). Missing and nil take the identical
    //    leg (`index2adr` answers NULL → type −1), so `SetFontString(nil)` **cannot clear the
    //    label from Lua**; the C++ clear exists but no binding reaches it.
    //  · a **table that is not a widget** → `%s:SetFontString(): Couldn't find 'this' in
    //    fontstring` (`0x87a160`).
    //  · a **Frame or a Texture** — anything that is not a FontString → `%s:SetFontString():
    //    Wrong object type, expected fontstring` (`0x87a124`), gated by an `IsA` against the
    //    FontString token, which a Texture fails.
    //
    // `%s` is the BUTTON's name (or `<unnamed>`), never the argument's. Zero return values.
    //
    // What `0x778d20` then does, all four clauses VERIFIED and all four here:
    //
    //  · **`new == old` is a total no-op** — the first compare, before anything is touched.
    //  · **the previous label is DESTROYED**, not orphaned (`old->vtable[0](1)`, the scalar
    //    deleting destructor, returning the storage to the FontString pool). So a button whose
    //    `SetText` already lazily made one does not leak a stray string behind the new label.
    //  · **the string is RE-PARENTED to the button and its draw layer forced to ARTWORK**
    //    (`0x77fd10(parent, 2, 1)` — layer id 2 in the client's own `.rdata 0x811a80` name table).
    //    Always, whatever it was parented to or drawn in before.
    //  · **it is anchored only if it has NO anchors of its own** (a scan of all nine
    //    `anchorPoints` slots): LEFT/RIGHT/CENTER by the justify bits, to the matching point on
    //    the button. That is exactly [`super::region::implicit_creation_anchor`]'s FontString arm,
    //    which already transcribes the same `& 7` → LEFT(1)/RIGHT(4)/else-CENTER chain, so it is
    //    reused rather than re-written. One stated difference: the reference reads the *button's*
    //    Normal `CSimpleFont` justify word and ours reads the string's own — they agree in the
    //    ordinary case, because our extract resolves the button's per-state font onto the label
    //    every frame anyway.
    //
    // The fifth clause — apply the button's per-state font immediately — needs no code: our
    // extract re-points the label at the current state's font object every frame
    // ([`ButtonState::normal_font`]), so binding the label IS applying it.
    m.set(
        "SetFontString",
        lua.create_function(|lua, args: MultiValue| {
            let mut it = args.into_iter();
            let this = match it.next() {
                Some(Value::Table(t)) => t,
                _ => return Err(mlua::Error::runtime("expected a button")),
            };
            let who = {
                let h = frame_handle_of(lua, &this)?;
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model
                    .arena
                    .frame(h)
                    .and_then(|f| f.name.clone())
                    .unwrap_or_else(|| "<unnamed>".to_string())
            };
            let Some(Value::Table(fs)) = it.next() else {
                return Err(mlua::Error::runtime(format!(
                    "Usage: {who}:SetFontString(fontstring)"
                )));
            };
            // The two remaining legs are DIFFERENT questions and the reference asks them in this
            // order: first "is this a framescript object at all" (`0x780b27`), then "is it a
            // FontString" (`0x780b90`'s `IsA` against the FontString token). A Frame or a Texture
            // passes the first and fails the second — so resolving straight to a region handle
            // would collapse both onto the first message and mislabel every widget argument.
            let rh = {
                let not_an_object = || {
                    mlua::Error::runtime(format!(
                        "{who}:SetFontString(): Couldn't find 'this' in fontstring"
                    ))
                };
                let id = super::object::decode_id(&fs).map_err(|_| not_an_object())?;
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                let known_widget =
                    model.id_to_frame.contains_key(&id) || model.id_to_region.contains_key(&id);
                if !known_widget {
                    return Err(not_an_object());
                }
                match model.id_to_region.get(&id).copied() {
                    Some(rh)
                        if model.arena.region(rh).map(|r| r.kind)
                            == Some(RegionKind::FontString) =>
                    {
                        rh
                    }
                    _ => {
                        return Err(mlua::Error::runtime(format!(
                            "{who}:SetFontString(): Wrong object type, expected fontstring"
                        )))
                    }
                }
            };
            let old = with_button(lua, &this, |bs| bs.text)?;
            if old == Some(rh) {
                return Ok(());
            }
            let owner = frame_handle_of(lua, &this)?;
            with_button(lua, &this, |bs| bs.text = Some(rh))?;
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            if let Some(dead) = old {
                // DESTROYED, not orphaned — the shared region-lifetime law, whose doc carries the
                // reason (`0x778d3c`'s scalar deleting destructor).
                super::region::free_region(&mut model, dead);
            }
            model.arena.set_region_owner(rh, Some(owner));
            if let Some(r) = model.arena.region_mut(rh) {
                r.draw_layer = DrawLayer::Artwork;
            }
            super::region::implicit_creation_anchor(&mut model, rh);
            // A new label, a new owner and a possible new anchor — the layout's read set moved,
            // and the string's extents are what the button's own `GetTextWidth` reports.
            model.touch_layout();
            model.touch_measure(rh);
            Ok(())
        })?,
    )?;

    // GetTextWidth / GetTextHeight — the Button's OWN text-extent readers (`0x782290` / `0x782390`,
    // wow-re `system/ui/scratch/item9-firing34-merge.md` l.36 and the Button method carve in
    // `widget-api-batch-benilla.md` Q8, which lists both present on Button and `GetStringWidth`
    // absent). Both are thin forwards onto the label FontString's own extent vtable slots
    // (`0x1c` / `0x20`), which is exactly what this delegation is.
    //
    // **Who asks.** `Bagnon_Forever/database/ui.lua:61` sizes its character-switch dropdown from
    // `button:GetTextWidth() + 40` over every saved character — with the method nil the whole
    // dropdown died on the first row, so the director could not switch characters in Bagnon at all.
    // The idiom generalises: it is how a 1.12 kit fits a button to its label, and the reference's
    // own `MoneyFrame.lua` l.202 (`SetWidth(GetTextWidth() + iconWidth)`) is the same shape.
    //
    // **Which measurement**, since a FontString carries two. The label's `GetStringWidth` — the
    // NATURAL, unwrapped extent — never the laid-out `GetWidth`. A button label does not wrap, so
    // the two agree in the ordinary case; where they differ, serving the laid-out width would hand
    // every "size the button to its text" caller its own previous output as its next input, which
    // is decision 0997's measured feedback loop (the macro window's tab that changed width every
    // frame). The unwrapped extent is the one that settles.
    //
    // `0` before the host has measured the string (a frame's latency, as everywhere else), and `0`
    // for a Button with no label at all — the reference dereferences its FontString pointer `+0x338`
    // here where `SetFont`/`GetFont` deliberately do not, and what a null one does is NOT byte-read,
    // so this answers the harmless number rather than raising on a guess.
    for (name, region_getter) in [
        ("GetTextWidth", "GetStringWidth"),
        // `GetHeight`, not a `GetStringHeight` — 1.12 has no such method (byte-verified absent
        // in every encoding), and the reference's own `Button:GetTextHeight 0x782390` is this
        // same call on the embedded FontString rather than a separate API.
        ("GetTextHeight", "GetHeight"),
    ] {
        m.set(
            name,
            lua.create_function(move |lua, this: Table| {
                let label = get_slot_texture(lua, &this, Slot::Text)?;
                match label {
                    Value::Table(t) => t.call_method::<f32>(region_getter, ()),
                    _ => Ok(0.0),
                }
            })?,
        )?;
    }

    // The per-state label fonts (the 1.12 API trio; XML `<NormalFont>/<HighlightFont>/
    // <DisabledFont>` route here through the loader). Stored as font-object NAMES — extract
    // re-points the ButtonText to the current state's object each frame, so Enable/Disable and
    // hover swap the label's paint with no Lua involvement (the client's own behavior:
    // UIPanelButtonTemplate's gold/white/gray label states).
    //
    // Each takes the font OBJECT, its name, or nil, like `FontString:SetFontObject` — across the
    // trio the corpus splits 5 object-form to 4 string-form, so accepting one shape only would
    // break about half the callers, and nil clears the state back to the default. Because the
    // state is stored as a NAME and re-resolved at every `extract`, a later mutation of that font
    // object reaches these labels for free.
    m.set(
        "SetTextFontObject",
        lua.create_function(|lua, (this, font): (Table, Value)| {
            let name = super::font::resolve("SetTextFontObject", &font)?;
            let text = with_button(lua, &this, |bs| {
                bs.normal_font.clone_from(&name);
                bs.text
            })?;
            link_label_to_font_object(lua, text, name.as_deref());
            Ok(())
        })?,
    )?;
    m.set(
        "SetHighlightFontObject",
        lua.create_function(|lua, (this, font): (Table, Value)| {
            let name = super::font::resolve("SetHighlightFontObject", &font)?;
            with_button(lua, &this, |bs| bs.highlight_font = name)
        })?,
    )?;
    m.set(
        "SetDisabledFontObject",
        lua.create_function(|lua, (this, font): (Table, Value)| {
            let name = super::font::resolve("SetDisabledFontObject", &font)?;
            with_button(lua, &this, |bs| bs.disabled_font = name)
        })?,
    )?;

    // SetFont(file, height [, flags]) / GetFont() — the Button's own font, `0x780880`/`0x79f3b0`
    // (wow-re `system/ui/scratch/widget-api-batch-benilla.md` Q8, §5-verified).
    // `_LazyPig/LazyPigMenu.lua:214` calls it straight on a `CreateFrame("Button", …)`, and it is
    // that addon's only blocker.
    //
    // Three contract details, each a plausible implementation's silent divergence:
    //
    //  · **It returns ZERO values.** The shared impl `0x79f210` pushes `1`/nil (a real font-load
    //    probe on a Font object — `!OmniCC` uses it), but `Button:SetFont` DISCARDS it
    //    (`xor eax,eax`), so a failed font load is undetectable from a Button. Returning `true`
    //    here would look harmless and would hand an addon a probe the client does not have.
    //  · **It never touches the label.** `SetFont`/`GetFont` do not dereference the FontString
    //    pointer `+0x338` at all, so styling a bare `CreateFrame("Button")` with no `<ButtonText>`
    //    is no error, no crash and **no lazy creation** — which is why the style is stored on the
    //    button ([`ButtonState::font`]) and `extract` applies it to whatever label exists. A later
    //    `SetText` (which *does* lazily create one, `0x778dc0`) then picks it up, so either call
    //    order works, exactly as the reference's does.
    //  · **It retunes all three embedded fonts** (normal `+0x33c`, disabled `+0x434`, highlight
    //    `+0x3b8`) and `GetFont` reads back only the normal one — see [`ButtonFont`] for why one
    //    record is the faithful shape for three identical writes.
    //
    // The argument gate is the shared impl's: arg2 `lua_isstring` and arg3 `lua_isnumber`, else
    // `luaL_error("Usage: %s:SetFont(\"font\", fontHeight [, flags])")` (`0x87c69c`). Both
    // predicates are the *coercing* ones in 5.0 — `lua_isstring` takes a number, `lua_isnumber`
    // takes a numeric string — so the leniency is transcribed rather than tightened.
    m.set(
        "SetFont",
        lua.create_function(
            |lua, (this, file, height, flags): (Table, Value, Value, Option<String>)| {
                let usage = || {
                    mlua::Error::runtime(
                        "Usage: <Button>:SetFont(\"font\", fontHeight [, flags])".to_string(),
                    )
                };
                let path = match &file {
                    Value::String(s) => s.to_str()?.to_string(),
                    Value::Number(_) | Value::Integer(_) => as_f32(&file).to_string(),
                    _ => return Err(usage()),
                };
                let height = match &height {
                    Value::Number(_) | Value::Integer(_) => as_f32(&height),
                    Value::String(s) => s.to_str()?.parse::<f32>().map_err(|_| usage())?,
                    _ => return Err(usage()),
                };
                let flags = super::Outline::flags(flags.as_deref().unwrap_or(""))
                    .as_str()
                    .to_string();
                with_button(lua, &this, |bs| {
                    bs.font = Some(ButtonFont {
                        path,
                        height,
                        flags,
                    })
                })
                // …and nothing is returned: `with_button` yields `()`.
            },
        )?,
    )?;
    // GetFont() → file, height, flags — **3 values**, read off the NORMAL embedded font. Unset
    // locally, that font still resolves through what the normal state inherits
    // (`<NormalFont>`/`SetTextFontObject`), which is how a `GameMenuButtonTemplate` button answers
    // GameFontNormal's face before anything calls `SetFont`. Nil path / nil height when neither
    // exists, still 3 values.
    m.set(
        "GetFont",
        lua.create_function(|lua, this: Table| {
            let (own, inherits) =
                with_button(lua, &this, |bs| (bs.font.clone(), bs.normal_font.clone()))?;
            let (path, height, flags) = match own {
                Some(f) => (Some(f.path), Some(f.height), f.flags),
                None => {
                    let model = lua.app_data_ref::<Model>().expect("model app_data");
                    let fo = inherits.and_then(|n| model.font_object(&n));
                    (
                        fo.and_then(|f| f.font.clone()),
                        fo.and_then(|f| f.height),
                        fo.map(|f| f.outline)
                            .unwrap_or_default()
                            .as_str()
                            .to_string(),
                    )
                }
            };
            let path = match path {
                Some(p) => Value::String(lua.create_string(&p)?),
                None => Value::Nil,
            };
            Ok((path, height, flags))
        })?,
    )?;

    // The per-state label COLORS (Button:SetTextColor + the Highlight/Disabled pair): the state's
    // color, when set, repaints the ButtonText over the state font object's paint at extract —
    // the dropdown kit's rows use all three (info.textR/G/B rows, isTitle's NORMAL-yellow and
    // notClickable's HIGHLIGHT-white recolors of a disabled row).
    m.set(
        "SetTextColor",
        lua.create_function(
            |lua, (this, r, g, b, a): (Table, f32, f32, f32, Option<f32>)| {
                with_button(lua, &this, |bs| {
                    bs.normal_color = Some([r, g, b, a.unwrap_or(1.0)])
                })
            },
        )?,
    )?;
    m.set(
        "SetHighlightTextColor",
        lua.create_function(
            |lua, (this, r, g, b, a): (Table, f32, f32, f32, Option<f32>)| {
                with_button(lua, &this, |bs| {
                    bs.highlight_color = Some([r, g, b, a.unwrap_or(1.0)])
                })
            },
        )?,
    )?;
    m.set(
        "SetDisabledTextColor",
        lua.create_function(
            |lua, (this, r, g, b, a): (Table, f32, f32, f32, Option<f32>)| {
                with_button(lua, &this, |bs| {
                    bs.disabled_color = Some([r, g, b, a.unwrap_or(1.0)])
                })
            },
        )?,
    )?;

    // LockHighlight/UnlockHighlight — pin the HighlightTexture on regardless of hover (ref
    // CButton::LockHighlight; the dropdown kit's checked rows stay lit).
    m.set(
        "LockHighlight",
        lua.create_function(|lua, this: Table| {
            with_button(lua, &this, |bs| bs.locked_highlight = true)
        })?,
    )?;
    m.set(
        "UnlockHighlight",
        lua.create_function(|lua, this: Table| {
            with_button(lua, &this, |bs| bs.locked_highlight = false)
        })?,
    )?;

    m.set(
        "Enable",
        lua.create_function(|lua, this: Table| with_button(lua, &this, |bs| bs.enabled = true))?,
    )?;
    m.set(
        "Disable",
        lua.create_function(|lua, this: Table| with_button(lua, &this, |bs| bs.enabled = false))?,
    )?;
    // IsEnabled() → the NUMBER 1 or the NUMBER 0 — never a boolean, and never nil.
    //
    // wow-re `ui/scratch/button-enabled-state.md`, VERIFIED off the Button method table at
    // `0x879d10`: `IsEnabled 0x7800b0` reads the three-valued STATE at `[obj+0x328]`
    // (0 DISABLED / 1 NORMAL / 2 PUSHED), does `setne cl` for "not disabled", and pushes that as a
    // NUMBER. So the reference's own `IsEnabled() == 0` and `== 1` tests (FriendsFrame.lua l.404,
    // StaticPopup.lua l.713) are live code.
    //
    // **This answered a Lua boolean until now, and the difference is not cosmetic, because 0 is
    // TRUTHY in Lua.** A caller writing the reference's `== 0` got false forever; a caller writing
    // `IsEnabled() > 0` got "attempt to compare number with boolean" — which is where pfUI's
    // `api/ui-widgets.lua:812` icon-fade died, taking the widget module with it. The divergence was
    // known and worked around by hand in three of our own files (FriendsFrame.xml's
    // `guildButtonDisabled` read BOTH spellings and said so); those workarounds go with this
    // change, which is 1719's bar — a prior call revisited because a new measurement arrived.
    //
    // Slider carries its own `IsEnabled` (slider.rs) and is deliberately NOT changed here: the
    // note above is the BUTTON method table, and whether the slider widget shares the contract is
    // unverified. Assuming they match is exactly the guess this comment exists to have avoided.
    m.set(
        "IsEnabled",
        lua.create_function(|lua, this: Table| {
            with_button(lua, &this, |bs| i64::from(bs.enabled))
        })?,
    )?;

    // RegisterForClicks(...): replaces the button's registered-click set outright (the live API's
    // contract — not additive) with its varargs, verbatim strings (`"LeftButtonUp"`,
    // `"RightButtonDown"`, …). [`UiScript::mouse_button`] consults the set via [`wants_click`] to
    // decide whether a press or a release reaches `OnClick`.
    m.set(
        "RegisterForClicks",
        lua.create_function(|lua, (this, args): (Table, MultiValue)| {
            let mut set = std::collections::HashSet::new();
            for v in args.iter() {
                if let Value::String(s) = v {
                    set.insert(s.to_str()?.to_string());
                }
            }
            with_button(lua, &this, |bs| bs.registered_clicks = set)
        })?,
    )?;

    // SetButtonState("PUSHED"/"NORMAL") / GetButtonState — the scripted press state
    // (`0x780270`/`0x780180`; ref `ActionButtonDown/Up`, ActionButton.lua:15-28: DOWN pushes
    // only from NORMAL, UP fires only from PUSHED — the state doubles as the keybind debounce).
    // Case-insensitive like the API's other string enums; an unknown state is a runtime error.
    // DISABLED is Enable/Disable's to set, not this method's (the 1.12 FrameXML never passes it).
    m.set(
        "SetButtonState",
        lua.create_function(|lua, (this, state): (Table, String)| {
            let pushed = if state.eq_ignore_ascii_case("PUSHED") {
                true
            } else if state.eq_ignore_ascii_case("NORMAL") {
                false
            } else {
                return Err(mlua::Error::runtime(format!(
                    "SetButtonState: unknown state '{state}'"
                )));
            };
            with_button(lua, &this, |bs| bs.pushed_state = pushed)
        })?,
    )?;
    m.set(
        "GetButtonState",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            // The LIVE mouse-held press counts too (hovered + a registered button down on this
            // frame — the same predicate the extract pass renders the PushedTexture by), closing
            // the documented INTERIM where a mouse-held button answered "NORMAL": the chat scroll
            // buttons' hold-repeat (ref MessageFrameScrollButton_OnUpdate) polls exactly this
            // mid-press (decision 0288 P3). It reads the same in the reference for the same
            // reason — `0x7792ad` writes ONE state variable and `0x780180` reads it back, so a
            // right-held button answers "PUSHED" as surely as a left-held one.
            let held = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model.mouseover == Some(h) && press_held(&model, h)
            };
            with_button(lua, &this, move |bs| {
                if !bs.enabled {
                    "DISABLED"
                } else if bs.pushed_state || held {
                    "PUSHED"
                } else {
                    "NORMAL"
                }
            })
        })?,
    )?;

    // Click([button]) — the programmatic click: same path as a physical one (toggle-then-OnClick).
    m.set(
        "Click",
        lua.create_function(|lua, (this, button): (Table, Option<String>)| {
            let h = frame_handle_of(lua, &this)?;
            let id = {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                model.frame_id(h)
            };
            let btn = button.unwrap_or_else(|| "LeftButton".to_string());
            // A programmatic Click() always emulates a completed (released) click.
            // A programmatic Click() — `scripted = true`, which only a LootButton reads.
            click_button(lua, id, &btn, false, true);
            Ok(())
        })?,
    )?;

    lua.set_named_registry_value(REG_BUTTON_METHODS, m)?;

    // CheckButton's own table (consulted before Button's — the class chain).
    let c = lua.create_table()?;
    texture_pair(lua, &c, "Checked", Slot::Checked)?;
    texture_pair(lua, &c, "DisabledChecked", Slot::DisabledChecked)?;
    c.set(
        "SetChecked",
        lua.create_function(|lua, (this, v): (Table, Value)| {
            // Numeric coercion, NOT Lua truthiness — byte-verified (decision 0227; wow-re
            // `system/ui/scratch/button-check-and-state-texture.md`, `SetChecked 0x799bf0` →
            // `0x6f1c10`): a number goes through `lua_tonumber` then a truncate-to-int (`fistp`,
            // round-toward-zero, `0x40a2b0`) and the C++ setter tests `!= 0`. So `SetChecked(0)`
            // UNchecks (0 is Lua-truthy — only a numeric read gets this right) and `SetChecked(1)`
            // checks; the type-dispatched helper also honors the keyword strings the reference
            // passes ("true"/"false" — SpellBookFrame.lua l.296-303), while a non-keyword string
            // falls to tonumber (→ 0 → unchecked). nil/other → false; booleans honored as-is for
            // this codebase's own Era-style callers.
            let checked = match v {
                Value::Boolean(b) => b,
                Value::Integer(i) => i != 0,
                Value::Number(n) => n.trunc() != 0.0,
                Value::String(s) => s.to_str().ok().is_some_and(|s| {
                    let t = s.trim();
                    t.eq_ignore_ascii_case("true")
                        || t.parse::<f64>().is_ok_and(|n| n.trunc() != 0.0)
                }),
                _ => false,
            };
            with_button(lua, &this, |bs| bs.checked = checked)
        })?,
    )?;
    c.set(
        // GetChecked() -> the NUMBER 1 or nil, never a boolean (1830). Its neighbour
        // `IsEnabled` two entries up was corrected for the same reason under 1719, whose comment
        // records that a boolean here killed pfUI's widget module outright; this is that call
        // finished. The reference proves the shape in its own source rather than only at the
        // bytes: stock `UIOptionsFrame.xml:310` saves a checkbox as
        // `tostring(this:GetChecked())` and stock `BuffFrame.lua:71` reads it back as `== "1"`,
        // a round-trip that only closes on the number.
        "GetChecked",
        lua.create_function(|lua, this: Table| {
            let checked = with_button(lua, &this, |bs| bs.checked)?;
            Ok(crate::script::binding_abi::predicate(checked))
        })?,
    )?;
    lua.set_named_registry_value(REG_CHECKBUTTON_METHODS, c)?;

    Ok(())
}

/// Whether frame `h` should fire `OnClick` for the input transition `name`
/// (`"<Button>ButtonUp"`/`"<Button>ButtonDown"`, e.g. `"RightButtonDown"`) — the
/// [`UiScript::mouse_button`](super::UiScript::mouse_button) gate. A Button/CheckButton consults
/// its `RegisterForClicks` set (case-insensitively — the live API is forgiving of case); any
/// other kind with an `OnClick` fires exactly as it always has, before `RegisterForClicks`
/// existed: on release only (`name` ending `"Up"`), for any button, never on press.
pub(super) fn wants_click(model: &Model, h: FrameHandle, name: &str) -> bool {
    match model.arena.frame(h).map(|f| &f.kind_state) {
        Some(KindState::Button(bs)) => bs
            .registered_clicks
            .iter()
            .any(|s| s.eq_ignore_ascii_case(name)),
        _ => name.ends_with("Up"),
    }
}

/// Whether frame `h` is registered for `button` (`"LeftButton"`, `"RightButton"`, …) in **either**
/// variant — the press-visual gate, which is a strictly weaker test than [`wants_click`]'s.
///
/// `CButton::OnMouseDown 0x779210` runs the two as separate tests in one pass, and the difference
/// between them is the whole of this:
///
/// ```text
/// 0x779238  eax = event.buttonMask
/// 0x779243  ecx = eax << 8 | eax
/// 0x77924b  test [this+0x330], ecx   ; registered EITHER WAY -> keep going, else nothing at all
/// 0x779256  hit-test the point       ; else nothing
/// 0x77926b  test event.buttonMask, [this+0x330]  ; the DOWN bits alone -> fire OnClick
/// 0x7792ad  push 2 ; SetState(PUSHED)            ; unconditional past the gates above
/// ```
///
/// So `[this+0x330]` is one dword holding the `"…ButtonDown"` registrations in byte 0 and the
/// `"…ButtonUp"` registrations in byte 1, and the mask `m | m << 8` asks "this button, either
/// variant". **The pushed art is not conditional on the click firing, or on the handler doing
/// anything** — it is conditional only on the button being registered for that mouse button at
/// all, which is why a right-click on an action, spellbook or pet slot lights up even when the
/// right-click does nothing (they all register `RightButtonUp`), and why a right-click on a plain
/// `LeftButtonUp` button does not.
pub(super) fn wants_press_visual(model: &Model, h: FrameHandle, button: &str) -> bool {
    match model.arena.frame(h).map(|f| &f.kind_state) {
        Some(KindState::Button(bs)) => bs.registered_clicks.iter().any(|s| {
            s.strip_suffix("Up")
                .or_else(|| s.strip_suffix("Down"))
                .is_some_and(|b| b.eq_ignore_ascii_case(button))
        }),
        _ => false,
    }
}

/// Is a press currently holding this button down — the `held` half of the PushedTexture rule.
///
/// Any captured mouse button counts, gated by [`wants_press_visual`]. Callers pair it with
/// `hovered`, which is `0x779256`'s hit test at press time plus `0x7793f0`'s restore-to-NORMAL
/// when the pointer leaves mid-press.
pub(super) fn press_held(model: &Model, h: FrameHandle) -> bool {
    model
        .mouse_down_on
        .iter()
        .any(|(button, &pressed)| pressed == h && wants_press_visual(model, h, button))
}

/// The click behavior shared by the input path and `Click()`: gated on `enabled`; a CheckButton
/// **toggles before OnClick fires** (the documented widget contract — a handler reading
/// `self:GetChecked()` sees the new state); then `OnClick(self, button, down)` — `down` is `true`
/// only for a press that fired via a `"<Button>ButtonDown"` registration, `false` for every
/// release-fired or programmatic click (the Era signature: `down` mirrors which transition fired).
///
/// `scripted` says the click came from Lua `:Click()` rather than from hardware — the reference's
/// second `OnClick` argument (`0` from `0x779280`/`0x7793a4`, `1` from the `Button:Click` binding
/// `0x7826c0`). Only [`crate::widget::FrameKind::LootButton`] reads it, and it reads it as a hard
/// gate: `0x4c182b` returns before the base call, so **a scripted click on a loot row does
/// nothing at all — not even run the row's own Lua `OnClick`**. Every other kind ignores it, which
/// is the reference's shape too (the base `0x779540` never looks at the flag).
pub(super) fn click_button(lua: &Lua, id: u32, button: &str, down: bool, scripted: bool) {
    let mut take_loot = None;
    let fire = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        let Some(&h) = model.id_to_frame.get(&id) else {
            return;
        };
        let is_loot = model
            .arena
            .frame(h)
            .is_some_and(|f| f.kind == crate::widget::FrameKind::LootButton);
        if is_loot {
            if scripted {
                return;
            }
            // Read the take BEFORE the handler runs, but queue it after: the reference reads
            // `[esi+0x4dc]` after the base call, and a handler that re-slots its own row mid-click
            // is not a case the shipped UI produces — reading first keeps the borrow simple and
            // the two cannot disagree. The modifier gate is `0x4c183a`/`0x4c1848`/`0x4c1856`:
            // shift, ctrl OR alt suppresses, and it is the whole reason the shipped
            // `LootFrameItem_OnClick` never calls a take itself.
            let (shift, ctrl, alt) = model.modifiers;
            if !shift && !ctrl && !alt {
                take_loot = model.arena.frame(h).and_then(|f| match &f.kind_state {
                    KindState::Button(bs) => bs.loot_slot,
                    _ => None,
                });
            }
        }
        let Some(frame) = model.arena.frame_mut(h) else {
            return;
        };
        let is_check = frame.kind == FrameKind::CheckButton;
        match &mut frame.kind_state {
            // A Button/CheckButton click is gated on its enabled flag…
            KindState::Button(bs) => {
                if bs.enabled {
                    if is_check {
                        bs.checked = !bs.checked;
                    }
                    true
                } else {
                    false
                }
            }
            // …every other kind with an OnClick just fires (plain frames can carry one too).
            _ => true,
        }
    };
    if !fire {
        return;
    }
    let btn = match lua.create_string(button) {
        Ok(s) => Value::String(s),
        Err(_) => return,
    };
    if let Err(e) = event::fire_widget_handler(lua, id, "OnClick", vec![btn, Value::Boolean(down)])
    {
        lua.app_data_mut::<Model>()
            .expect("model app_data")
            .errors
            .push(e.to_string());
    }
    // …and then the take, unconditionally on the handler's outcome. `0x4c1867` is not guarded by
    // anything the Lua side did: the base call at `0x4c1833` returns void and its result is never
    // tested. A row whose `OnClick` errored still loots, which is the reference's behaviour and
    // the reason a broken addon hook cannot silently eat your loot.
    if let Some(slot) = take_loot {
        // The 0-based store, back to the 1-based display row the take queue speaks.
        lua.app_data_mut::<Model>()
            .expect("model app_data")
            .loot_picks
            .push(slot + 1);
    }
}
