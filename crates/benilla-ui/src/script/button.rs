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
//! no blend modes yet), and `PushedTextOffset`/per-state fonts are not modeled.
//!
//! Method resolution: CheckButton's table is consulted first, then Button's, then the shared frame
//! table — mirroring the client's class chain, and keeping duck-typing honest (`frame.SetChecked`
//! is nil on a plain Button; `frame.GetText` is nil on a plain Frame).

use mlua::{Lua, MultiValue, Table, Value};

use super::object::{as_f32, frame_handle_of};
use super::region::region_wrapper;
use super::{event, Model, RegionData};
use crate::order::DrawLayer;
use crate::widget::{ButtonState, FrameHandle, FrameKind, KindState, RegionKind};

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
            data.color = None;
        }
        Some(Value::String(s)) => data.texture = Some(s.to_str()?.to_string()),
        Some(v @ (Value::Number(_) | Value::Integer(_))) => {
            let arg = |i: usize| args.get(i).map(as_f32);
            data.color = Some([
                as_f32(v),
                arg(1).unwrap_or(0.0),
                arg(2).unwrap_or(0.0),
                arg(3).unwrap_or(1.0),
            ]);
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

    // The per-state label fonts (the 1.12 API trio; XML `<NormalFont>/<HighlightFont>/
    // <DisabledFont>` route here through the loader). Stored as font-object NAMES — extract
    // re-points the ButtonText to the current state's object each frame, so Enable/Disable and
    // hover swap the label's paint with no Lua involvement (the client's own behavior:
    // UIPanelButtonTemplate's gold/white/gray label states).
    m.set(
        "SetTextFontObject",
        lua.create_function(|lua, (this, name): (Table, String)| {
            with_button(lua, &this, |bs| bs.normal_font = Some(name))
        })?,
    )?;
    m.set(
        "SetHighlightFontObject",
        lua.create_function(|lua, (this, name): (Table, String)| {
            with_button(lua, &this, |bs| bs.highlight_font = Some(name))
        })?,
    )?;
    m.set(
        "SetDisabledFontObject",
        lua.create_function(|lua, (this, name): (Table, String)| {
            with_button(lua, &this, |bs| bs.disabled_font = Some(name))
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
    m.set(
        "IsEnabled",
        lua.create_function(|lua, this: Table| with_button(lua, &this, |bs| bs.enabled))?,
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
            // The LIVE mouse-held press counts too (hovered + LeftButton down on this frame —
            // the same predicate the extract pass renders the PushedTexture by), closing the
            // documented INTERIM where a mouse-held button answered "NORMAL": the chat scroll
            // buttons' hold-repeat (ref MessageFrameScrollButton_OnUpdate) polls exactly this
            // mid-press (decision 0288 P3).
            let held = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model.mouseover == Some(h)
                    && model.mouse_down_on.get("LeftButton").copied() == Some(h)
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
            click_button(lua, id, &btn, false);
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
        "GetChecked",
        lua.create_function(|lua, this: Table| with_button(lua, &this, |bs| bs.checked))?,
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

/// The click behavior shared by the input path and `Click()`: gated on `enabled`; a CheckButton
/// **toggles before OnClick fires** (the documented widget contract — a handler reading
/// `self:GetChecked()` sees the new state); then `OnClick(self, button, down)` — `down` is `true`
/// only for a press that fired via a `"<Button>ButtonDown"` registration, `false` for every
/// release-fired or programmatic click (the Era signature: `down` mirrors which transition fired).
pub(super) fn click_button(lua: &Lua, id: u32, button: &str, down: bool) {
    let fire = {
        let mut model = lua.app_data_mut::<Model>().expect("model app_data");
        let Some(&h) = model.id_to_frame.get(&id) else {
            return;
        };
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
}
