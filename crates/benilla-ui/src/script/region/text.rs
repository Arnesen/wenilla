//! Region method-table cluster: **text** — a FontString's string, its faces, colour, shadow
//! and justification. Split out of `region.rs` at the 0716 file-size budget.
//!
//! **These duplicate names the `<Font>` object table also carries, deliberately for now**: a region
//! may override its inherited font object per-property, and the severance mask (`FontExplicit`) is
//! what keeps the two apart. Collapsing them onto one shared block is the follow-on 1231 named.

use mlua::{Lua, Table, Value};

use crate::script::{JustifyH, JustifyV, Model, Outline};

/// Resolve `self` (a region wrapper) to its live [`RegionHandle`].
use super::{measured_wh, region_handle_of};

/// Populate `m`'s text methods (see the module doc).
pub(super) fn install(lua: &Lua, m: &Table) -> mlua::Result<()> {
    m.set(
        "SetText",
        lua.create_function(|lua, (this, text): (Table, Option<String>)| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let data = model.region_data.entry(rh).or_default();
            data.text = text;
            // Fresh text draws whole — an armed write-on gradient belongs to the old string.
            data.alpha_gradient = None;
            Ok(())
        })?,
    )?;

    // SetFormattedText(fmt, ...) = SetText(format(fmt, ...)) — routed through the stdlib's
    // positional-aware `format` so `%N$s` specs behave (a consensus call across the 0068 targets).
    m.set(
        "SetFormattedText",
        lua.create_function(|lua, (this, args): (Table, mlua::MultiValue)| {
            let format: mlua::Function = lua
                .globals()
                .get::<Table>("string")?
                .get::<mlua::Function>("format")?;
            let text: String = format.call(args)?;
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let data = model.region_data.entry(rh).or_default();
            data.text = Some(text);
            data.alpha_gradient = None;
            Ok(())
        })?,
    )?;

    m.set(
        "GetText",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let text = {
                let model = lua.app_data_ref::<Model>().expect("model");
                model.region_data.get(&rh).and_then(|d| d.text.clone())
            };
            match text {
                Some(t) => Ok(Value::String(lua.create_string(&t)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // GetStringWidth/GetStringHeight (FontString): the host-measured text extent from the measure
    // round-trip ([`super::UiScript::set_measured_text`]) — the client asks its font engine for the
    // laid-out string's metrics exactly here (`fontstring.md`), and the tooltip's auto-size sums
    // these to fit its lines. `0` until the string has been measured (a frame's latency; converges).
    // The stored measure only counts while its key matches the CURRENT text/font/wrap
    // ([`RegionData::measure_key`]): after a SetText the old string's width is not this string's
    // metric — serving it is how the whisper header's `GetWidth()` latched the edit-box insets on
    // the previous header's width. A poll-until-nonzero caller (the chat header machine) now
    // converges on the RIGHT measure instead of settling on a stale one.
    // `GetWidth`/`GetHeight` prefer the measured extent, falling back to an explicit `SetSize` — the
    // real client's `SmallTextTooltipText:GetWidth()` idiom (ref-GameTooltip.xml l.63).
    // GetStringWidth is the **natural, unwrapped** extent — never the declared box, and never the
    // wrapped one (wow-re `fontstring-overflow.md`, "The measurement echo": the reference's getter
    // re-measures the raw text with NO wrap constraint). Unlike `GetWidth` below it deliberately
    // does NOT fall back to an explicit `SetSize`: the declared width is the very thing a caller
    // asks this to be independent of. A kit that sizes a box from this number and then sets a width
    // on the string — which is what the reference's own `PanelTemplates_TabResize` does — would
    // otherwise read its own output back as its next input and never settle (decision 0997, the
    // macro window's character tab changing width every frame). `0` until measured, as ever.
    fn natural_w(lua: &Lua, this: &Table) -> mlua::Result<f32> {
        let rh = region_handle_of(lua, this)?;
        let model = lua.app_data_ref::<Model>().expect("model");
        let Some(d) = model.region_data.get(&rh) else {
            return Ok(0.0);
        };
        let scale = model
            .arena
            .region(rh)
            .and_then(|r| model.arena.frame(r.owner))
            .map(|f| f.effective_scale)
            .unwrap_or(1.0);
        Ok(d.measured
            .filter(|m| m.key == d.measure_key(scale))
            .map(|m| m.natural_w)
            .unwrap_or(0.0))
    }
    m.set(
        "GetStringWidth",
        lua.create_function(|lua, this: Table| natural_w(lua, &this))?,
    )?;

    m.set(
        "GetStringHeight",
        lua.create_function(|lua, this: Table| Ok(measured_wh(lua, &this)?.1))?,
    )?;

    // SetJustifyH("LEFT"|"CENTER"|"RIGHT") — a FontString's horizontal justification (XML `justifyH`).
    m.set(
        "SetJustifyH",
        lua.create_function(|lua, (this, j): (Table, String)| {
            let rh = region_handle_of(lua, &this)?;
            let jh = match j.to_ascii_uppercase().as_str() {
                "LEFT" => JustifyH::Left,
                "RIGHT" => JustifyH::Right,
                _ => JustifyH::Center,
            };
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let d = model.region_data.entry(rh).or_default();
            d.justify_h = jh;
            d.font_explicit.justify_h = true;
            Ok(())
        })?,
    )?;

    // SetJustifyV("TOP"|"MIDDLE"|"BOTTOM") — a FontString's vertical justification (XML `justifyV`).
    m.set(
        "SetJustifyV",
        lua.create_function(|lua, (this, j): (Table, String)| {
            let rh = region_handle_of(lua, &this)?;
            let jv = match j.to_ascii_uppercase().as_str() {
                "TOP" => JustifyV::Top,
                "BOTTOM" => JustifyV::Bottom,
                _ => JustifyV::Middle,
            };
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let d = model.region_data.entry(rh).or_default();
            d.justify_v = jv;
            d.font_explicit.justify_v = true;
            Ok(())
        })?,
    )?;

    // SetFontObject(GameFontNormal) — re-point this FontString at a Font object: its resolved paint
    // (face/height/color/outline/shadow) becomes the region's, and the link is kept live, so a later
    // `GameFontNormal:SetFont(…)` re-paints this region too ([`super::font`]'s module doc).
    //
    // All three argument forms the reference's own usage string names (`.rdata 0x87c5cc`:
    // `SetFontObject(font or "font" or nil)`) — the **object**, which is what 3,180 of the corpus's
    // 3,186 call sites pass (`Gratuity-2.0.lua:57`, every FuBar/Ace label); a **name string**, for
    // our own shipped XML and the 6 sites that use it; and **nil**, which severs the link. A frame,
    // a number, or an unknown name is an error — never a silent no-op (1203/1205/1211's class).
    m.set(
        "SetFontObject",
        lua.create_function(|lua, (this, font): (Table, Value)| {
            let name = crate::script::font::resolve("SetFontObject", &font)?;
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            // The nil form: unlink, and leave the paint standing (the reference stores a null
            // parent; nothing re-reads and nothing is cleared).
            let Some(name) = name else {
                model.region_data.entry(rh).or_default().font_object = None;
                return Ok(());
            };
            let Some(fo) = model.font_objects.get(&name).cloned() else {
                return Err(mlua::Error::runtime(format!(
                    "SetFontObject: no font object named '{name}' is registered"
                )));
            };
            let d = model.region_data.entry(rh).or_default();
            d.font_object = Some(name);
            // The severance mask is deliberately NOT reset here. §5-verified: the real "stop
            // inheriting this property" signal is a CLEARED bit in the inheritMask at
            // `FONTINSTANCE+0x2c` (FontString `+0xd4`, per-axis justify at `+0x124`), cleared by
            // each local setter and never restored — "a FontString that set its own colour stays
            // severed even across a later SetFontObject" (wow-re
            // `system/ui/scratch/font-object-lua-surface.md`). This corrects our first cut, which
            // reset it.
            crate::script::font::repaint(d, &fo);
            Ok(())
        })?,
    )?;

    // GetFontObject() → the font OBJECT this FontString last resolved (or nil).
    //
    // The object, not its name: `Dewdrop-2.0.lua:2181` is
    // `button.text:SetTextColor(button.text:GetFontObject():GetTextColor())` — 65 sites across 62
    // corpus addons that index the result immediately. A name string there raises.
    m.set(
        "GetFontObject",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let name = {
                let model = lua.app_data_ref::<Model>().expect("model");
                model
                    .region_data
                    .get(&rh)
                    .and_then(|d| d.font_object.clone())
                    .filter(|n| model.font_objects.contains_key(n))
            };
            match name {
                Some(n) => Ok(Value::Table(crate::script::font::wrapper(lua, &n)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // SetNonSpaceWrap(enable) / CanNonSpaceWrap() — FontString only (`0x79e9f0` / `0x79ead0`).
    //
    // Two contract details from wow-re's batch, both easy to get wrong:
    //  · the getter is **`CanNonSpaceWrap`**, not `GetNonSpaceWrap`, and it answers **`1` or nil**,
    //    not a boolean — 1.12 predates that convention and an addon may compare against 1.
    //  · a **no-argument call ENABLES** it (the default is on), rather than being a query.
    //
    // `oRA2/Leader/Item.lua:561` is `f.textname:SetNonSpaceWrap(false)`, reached by two addons.
    m.set(
        "SetNonSpaceWrap",
        lua.create_function(|lua, (this, enable): (Table, Value)| {
            let on = match &enable {
                Value::Nil => true,
                Value::Boolean(b) => *b,
                _ => true,
            };
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            model.region_data.entry(rh).or_default().non_space_wrap = Some(on);
            Ok(())
        })?,
    )?;

    m.set(
        "CanNonSpaceWrap",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_mut::<Model>().expect("model");
            let on = model
                .region_data
                .get(&rh)
                .and_then(|d| d.non_space_wrap)
                .unwrap_or(true);
            Ok(if on { Some(1i64) } else { None })
        })?,
    )?;

    // ── shadow, on the REGION ────────────────────────────────────────────────────────────────
    // These already existed on the font-object table; a FontString made by `CreateFontString` had
    // none of them, and that is where the corpus calls them:
    // `FuBar_NavigatorFu/NavigatorFu.lua:31` does
    // `coordText:SetShadowColor(GameFontNormal:GetShadowColor())` — the GETTER on a font object,
    // the SETTER on a fresh region — and `KLHThreatMeter/.../KTM_Gui.lua:404` is
    // `fontstring:SetShadowColor(0,0,0,0.3)`.
    //
    // **`GetShadowColor` returns FOUR values, not three** (`0x79dd2f`, `mov eax,0x4` — wow-re's
    // widget-method batch). Three is the plausible wrong answer and it silently drops the alpha
    // that NavigatorFu is round-tripping. `GetShadowOffset` returns two, in UI units.
    //
    // Either half may be set before the other, so each starts from whatever is there — the same
    // rule the font-object versions already follow.
    m.set(
        "SetShadowColor",
        lua.create_function(
            |lua, (this, r, g, b, a): (Table, f32, f32, f32, Option<f32>)| {
                let rh = region_handle_of(lua, &this)?;
                let mut model = lua.app_data_mut::<Model>().expect("model");
                let d = model.region_data.entry(rh).or_default();
                let offset = d.font_shadow.map_or([0.0, 0.0], |s| s.offset);
                d.font_shadow = Some(crate::script::FontShadow {
                    offset,
                    color: [r, g, b, a.unwrap_or(1.0)],
                });
                d.font_explicit.shadow = true;
                Ok(())
            },
        )?,
    )?;

    m.set(
        "GetShadowColor",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_mut::<Model>().expect("model");
            let c = model
                .region_data
                .get(&rh)
                .and_then(|d| d.font_shadow)
                .map_or([0.0, 0.0, 0.0, 1.0], |s| s.color);
            Ok((c[0], c[1], c[2], c[3]))
        })?,
    )?;

    m.set(
        "SetShadowOffset",
        lua.create_function(|lua, (this, x, y): (Table, f32, f32)| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let d = model.region_data.entry(rh).or_default();
            let color = d.font_shadow.map_or([0.0, 0.0, 0.0, 1.0], |s| s.color);
            d.font_shadow = Some(crate::script::FontShadow {
                offset: [x, y],
                color,
            });
            d.font_explicit.shadow = true;
            Ok(())
        })?,
    )?;

    m.set(
        "GetShadowOffset",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_mut::<Model>().expect("model");
            let o = model
                .region_data
                .get(&rh)
                .and_then(|d| d.font_shadow)
                .map_or([0.0, 0.0], |s| s.offset);
            Ok((o[0], o[1]))
        })?,
    )?;

    // SetFont(path, height [, flags]) — the direct face/size/outline setter (the real region API and
    // the XML `font=`/`<FontHeight>`/`outline=` join). `flags` is an OUTLINETYPE-ish string
    // ("OUTLINE"/"THICKOUTLINE"/…"); anything else clears the outline. A nil/empty `path` keeps the
    // current face (so a FontString with only `<FontHeight>` retains its inherited object's font).
    // Returns true (the live API returns whether the font loaded; we always accept — face
    // availability is the renderer's concern).
    m.set(
        "SetFont",
        lua.create_function(
            |lua, (this, path, height, flags): (Table, Option<String>, Option<f32>, Option<String>)| {
                let rh = region_handle_of(lua, &this)?;
                let mut model = lua.app_data_mut::<Model>().expect("model");
                let d = model.region_data.entry(rh).or_default();
                // Each argument actually supplied is an EXPLICIT set: it must survive a later
                // mutation of the font object this region inherits (`FontExplicit`).
                if let Some(p) = path.filter(|p| !p.is_empty()) {
                    d.font_path = Some(p);
                    d.font_explicit.face = true;
                }
                if let Some(h) = height {
                    d.font_height = Some(h);
                    d.font_explicit.height = true;
                }
                if let Some(f) = flags {
                    d.outline = Outline::flags(&f);
                    d.font_explicit.outline = true;
                }
                Ok(true)
            },
        )?,
    )?;

    // SetTextHeight(height) — switch the FontString to the scaled-string regime (§5-verified,
    // wow-re `fontstring-overflow.md`: `0x771600` is the ONLY clearer of the one-to-one bit
    // `0x200`; the literal size then flows through UNCAPPED, magnified from the raster). Stored
    // as the distinct [`RegionData::text_height`] — the font object is untouched, so GetFont
    // keeps reporting the face's own height like the real API.
    m.set(
        "SetTextHeight",
        lua.create_function(|lua, (this, height): (Table, f32)| {
            let rh = region_handle_of(lua, &this)?;
            lua.app_data_mut::<Model>()
                .expect("model")
                .region_data
                .entry(rh)
                .or_default()
                .text_height = Some(height);
            Ok(())
        })?,
    )?;

    // GetFont() → path, height, flags — the resolved face/size/outline (nil path if never set).
    m.set(
        "GetFont",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            let d = model.region_data.get(&rh);
            let path = d.and_then(|d| d.font_path.clone());
            let height = d.and_then(|d| d.font_height);
            let flags = d.map(|d| d.outline).unwrap_or_default().as_str();
            let path = match path {
                Some(p) => Value::String(lua.create_string(&p)?),
                None => Value::Nil,
            };
            Ok((path, height, flags))
        })?,
    )?;

    // SetTextColor(r, g, b [, a]) — a FontString's text color. A different binding name for the
    // same `+0xb8` vertex-colour slot `SetVertexColor` writes: a FontString has no texel of its own
    // to multiply against, so its vertex colour IS the colour it draws.
    m.set(
        "SetTextColor",
        lua.create_function(
            |lua, (this, r, g, b, a): (Table, f32, f32, f32, Option<f32>)| {
                let rh = region_handle_of(lua, &this)?;
                let mut model = lua.app_data_mut::<Model>().expect("model");
                let d = model.region_data.entry(rh).or_default();
                d.vertex_color = Some([r, g, b, a.unwrap_or(1.0)]);
                d.font_explicit.color = true;
                Ok(())
            },
        )?,
    )?;

    // GetTextColor() → r, g, b, a — `SetTextColor`'s missing pair, and a real binding in the same
    // FontInstance family. 11 corpus sites read it off a FontString directly (`CustomNameplates`
    // re-tints a level tag from the name's colour; `TipBuddy` snapshots every tooltip line), on top
    // of the 65 that reach it through `GetFontObject()`. Never set = the white every region draws
    // at, same convention as `GetVertexColor` (the same slot).
    m.set(
        "GetTextColor",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            let c = model
                .region_data
                .get(&rh)
                .and_then(|d| d.vertex_color)
                .unwrap_or([1.0, 1.0, 1.0, 1.0]);
            Ok((c[0], c[1], c[2], c[3]))
        })?,
    )?;
    Ok(())
}
