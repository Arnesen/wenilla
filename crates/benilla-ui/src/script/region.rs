//! The **region** side of the object model (RF-0023's distinct-tag leaves): the `Texture`/
//! `FontString` wrapper cache, their shared metatable, and the region method surface. Split from
//! [`super::object`] (which keeps the frame side + `CreateFrame`) so each grows along its own
//! axis — frames grow per-kind method tables ([`super::statusbar`], [`super::button`]), regions
//! grow paint/coords methods here.

use mlua::{Lua, Table, Value};

use super::object::{
    anchor_bits_eq, as_f32, decode_id, draw_layer_from_str, id_to_lud, point_from_str,
};
use super::{
    JustifyH, JustifyV, Model, Outline, TexCoords, REG_REGION_META, REG_REGION_METHODS,
    REG_WRAPPERS, SCREEN,
};
use crate::layout::{Anchor, Point};
use crate::widget::RegionHandle;

/// Resolve `self` (a region wrapper) to its live [`RegionHandle`].
pub(super) fn region_handle_of(lua: &Lua, this: &Table) -> mlua::Result<RegionHandle> {
    let id = decode_id(this)?;
    let model = lua.app_data_ref::<Model>().expect("model app_data");
    model
        .id_to_region
        .get(&id)
        .copied()
        .ok_or_else(|| mlua::Error::runtime("stale or invalid region handle"))
}

/// Get-or-create the wrapper table for a region id (distinct metatable — the region "tag").
pub(super) fn region_wrapper(lua: &Lua, id: u32) -> mlua::Result<Table> {
    let wrappers: Table = lua.named_registry_value(REG_WRAPPERS)?;
    if let Value::Table(t) = wrappers.get::<Value>(id)? {
        return Ok(t);
    }
    let t = lua.create_table()?;
    t.raw_set(0, Value::LightUserData(id_to_lud(id)))?;
    let meta: Table = lua.named_registry_value(REG_REGION_META)?;
    t.set_metatable(Some(meta))?;
    wrappers.set(id, t.clone())?;
    Ok(t)
}

/// Install the region method table + the shared region metatable.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    install_region_methods(lua)?;

    // SetPortraitTexture(textureRegion, unit) — the live API's **global** (ref-UnitFrame.lua:
    // `SetPortraitTexture(this.portrait, this.unit)`), distinct from the region-method
    // `SetPortraitToTexture(path)` icon crop. It binds a Texture region to a unit token; the app
    // renders that unit's model off-screen and feeds the bake back through the region's
    // [`super::QuadContent::Texture::portrait_unit`], with `circular` marking the round stencil
    // (the bake is square with an opaque backdrop; the frame-ring portraits cut the inscribed
    // circle, exactly what the app's quad shader does with the flag). `texture`/`color` drop —
    // the bake replaces them. A later `SetTexture`/`SetPortraitToTexture` clears the binding.
    lua.globals().set(
        "SetPortraitTexture",
        lua.create_function(|lua, (region, unit): (Table, String)| {
            let rh = region_handle_of(lua, &region)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let data = model.region_data.entry(rh).or_default();
            data.portrait_unit = Some(unit);
            data.texture = None;
            data.color = None;
            data.circular = true;
            Ok(())
        })?,
    )?;

    // BenillaSetBoothTexture(textureRegion, slotToken) — the **square** twin of
    // SetPortraitTexture (decision 0208 §5): the same `portrait_unit` booth-image binding
    // WITHOUT the circular mask, for the paper doll's rectangular model pane (its texture region
    // samples the booth's body bake edge to edge — no frame ring to mask for). Benilla-named:
    // the real client's pane is a live 3D `<PlayerModel>`; ours is the doctrine-consistent
    // still (0105/0118), so the binding is ours, not the live API's.
    lua.globals().set(
        "BenillaSetBoothTexture",
        lua.create_function(|lua, (region, token): (Table, String)| {
            let rh = region_handle_of(lua, &region)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let data = model.region_data.entry(rh).or_default();
            data.portrait_unit = Some(token);
            data.texture = None;
            data.color = None;
            data.circular = false;
            Ok(())
        })?,
    )?;

    let region_meta = lua.create_table()?;
    let region_index = lua.create_function(|lua, (_this, key): (Table, Value)| {
        let methods: Table = lua.named_registry_value(REG_REGION_METHODS)?;
        methods.get::<Value>(key)
    })?;
    region_meta.set("__index", region_index)?;
    lua.set_named_registry_value(REG_REGION_META, region_meta)?;
    Ok(())
}

fn install_region_methods(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;

    // GetName() → this region's global name, or nil when it was declared anonymously — the pair
    // the frame side already answers. Real FrameXML round-trips a region through its name wherever
    // it can't hold a reference to itself: `ComboFrame.lua`'s shine chain hands `frame:GetName()`
    // to a fade `finishedFunc` and `getglobal`s it back ("hack since a frame can't have a
    // reference to itself in it" — its own comment).
    //
    // Resolved by scanning the region-name registry rather than storing the name a second time on
    // the region: that registry is the single authority for region names (the widget arena
    // deliberately holds none), and a mirrored copy is one more thing to drift. The scan is linear
    // in NAMED regions, and this is a human-rate call.
    m.set(
        "GetName",
        lua.create_function(|lua, this: Table| {
            let id = decode_id(&this)?;
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let name = model
                .region_names
                .iter()
                .find(|&(_, &v)| v == id)
                .map(|(k, _)| k.clone());
            match name {
                Some(n) => Ok(Value::String(lua.create_string(&n)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // Region-level visibility — the real VisibleRegion Show/Hide on Textures/FontStrings (the
    // ref kit hides tab slices, cooldown swipes, money coins…). A hidden region draws nothing;
    // IsVisible additionally requires the owner frame's effective visibility, mirroring the
    // frame-side pair.
    m.set(
        "Show",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            model.region_data.entry(rh).or_default().hidden = false;
            Ok(())
        })?,
    )?;
    m.set(
        "Hide",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            model.region_data.entry(rh).or_default().hidden = true;
            Ok(())
        })?,
    )?;
    m.set(
        "IsShown",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            let shown = !model.region_data.get(&rh).is_some_and(|d| d.hidden);
            Ok(if shown { Value::Integer(1) } else { Value::Nil })
        })?,
    )?;
    m.set(
        "IsVisible",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            let shown = !model.region_data.get(&rh).is_some_and(|d| d.hidden);
            let owner_visible = model
                .arena
                .region(rh)
                .and_then(|r| model.arena.frame(r.owner))
                .is_some_and(|f| f.effective_visible);
            Ok(if shown && owner_visible {
                Value::Integer(1)
            } else {
                Value::Nil
            })
        })?,
    )?;

    // SetAlpha/GetAlpha — the region's own alpha, distinct from the owner frame's. The ref kit reads
    // it back to ramp a texture (CastingBarFrame's completion flash does
    // `CastingBarFlash:SetAlpha(GetAlpha() + CASTING_BAR_FLASH_STEP)`), so the getter must return the
    // region's value, never the frame's. Draw law + the open question: [`RegionData::alpha`].
    m.set(
        "SetAlpha",
        lua.create_function(|lua, (this, alpha): (Table, f32)| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            model.region_data.entry(rh).or_default().alpha = Some(alpha.clamp(0.0, 1.0));
            Ok(())
        })?,
    )?;
    m.set(
        "GetAlpha",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model
                .region_data
                .get(&rh)
                .and_then(|d| d.alpha)
                .unwrap_or(1.0))
        })?,
    )?;

    m.set(
        "SetVertexColor",
        lua.create_function(
            |lua, (this, r, g, b, a): (Table, f32, f32, f32, Option<f32>)| {
                let rh = region_handle_of(lua, &this)?;
                let mut model = lua.app_data_mut::<Model>().expect("model");
                model.region_data.entry(rh).or_default().color = Some([r, g, b, a.unwrap_or(1.0)]);
                Ok(())
            },
        )?,
    )?;
    m.set(
        "SetTexture",
        lua.create_function(
            |lua, (this, arg, g, b, a): (Table, Value, Option<f32>, Option<f32>, Option<f32>)| {
                let rh = region_handle_of(lua, &this)?;
                let mut model = lua.app_data_mut::<Model>().expect("model");
                let data = model.region_data.entry(rh).or_default();
                // A plain SetTexture makes the region ordinary again — drop any portrait circular mask
                // and any live-unit-portrait binding.
                data.circular = false;
                data.portrait_unit = None;
                match &arg {
                    // SetTexture("") clears, same as SetTexture(nil) — the ref lua blanks state
                    // art with the empty string (QuestLogFrame.lua:165-166).
                    Value::String(s) if s.to_str()?.is_empty() => {
                        data.texture = None;
                        data.color = None;
                    }
                    Value::String(s) => {
                        data.texture = Some(s.to_str()?.to_string());
                    }
                    Value::Number(_) | Value::Integer(_) => {
                        data.color = Some([
                            as_f32(&arg),
                            g.unwrap_or(0.0),
                            b.unwrap_or(0.0),
                            a.unwrap_or(1.0),
                        ]);
                    }
                    // SetTexture(nil) clears (the live API's blank-the-region form); a cleared
                    // texture region draws nothing.
                    Value::Nil => {
                        data.texture = None;
                        data.color = None;
                    }
                    _ => {}
                }
                Ok(())
            },
        )?,
    )?;
    // SetPortraitToTexture(path) — set the texture AND mark the region a portrait (drawn masked to
    // its inscribed circle). The client's portraits are circular (model or icon fallback); the frame
    // ring is a thin band whose transparent corners would otherwise show the square texture's edges.
    // The live API's `SetPortraitToTexture(texture, path)` is a global crop helper; ours is the
    // region-method face of the same intent. This is the *icon/path* portrait — distinct from the
    // `SetPortraitTexture(region, unit)` live model bake, so it drops any live-unit binding.
    // `SetTexture` on the same region clears the flag (a plain texture again).
    m.set(
        "SetPortraitToTexture",
        lua.create_function(|lua, (this, path): (Table, String)| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let data = model.region_data.entry(rh).or_default();
            data.texture = Some(path);
            data.circular = true;
            data.portrait_unit = None;
            Ok(())
        })?,
    )?;
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
    // SetAlphaGradient(start, length) — the per-character write-on reveal (CSimpleFontString;
    // the quest-description machinery, ref QuestFrame.lua:548/558). Returns whether `start` is
    // still inside the text (chars) — the ref's OnUpdate loop advances until this goes false.
    m.set(
        "SetAlphaGradient",
        lua.create_function(|lua, (this, start, length): (Table, f32, f32)| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let data = model.region_data.entry(rh).or_default();
            data.alpha_gradient = Some((start, length));
            let chars = data.text.as_deref().map_or(0, |t| t.chars().count());
            Ok(start < chars as f32)
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
    // SetBlendMode("BLEND"|"ADD"|…) — the shared alphaMode enum (0x811aa8); only ADD changes
    // draw behavior in v1 (DISABLE/ALPHAKEY/MOD are accepted as straight alpha, a stated gap).
    m.set(
        "SetBlendMode",
        lua.create_function(|lua, (this, mode): (Table, String)| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            model.region_data.entry(rh).or_default().additive = mode.eq_ignore_ascii_case("ADD");
            Ok(())
        })?,
    )?;
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
    fn measured_wh(lua: &Lua, this: &Table) -> mlua::Result<(f32, f32)> {
        let rh = region_handle_of(lua, this)?;
        let model = lua.app_data_ref::<Model>().expect("model");
        let d = model.region_data.get(&rh);
        let m = d.and_then(|d| d.measured.filter(|m| m.key == d.measure_key()));
        let size = d.and_then(|d| d.size);
        let w = m.map(|m| m.w).or(size.map(|s| s.0)).unwrap_or(0.0);
        let h = m.map(|m| m.h).or(size.map(|s| s.1)).unwrap_or(0.0);
        Ok((w, h))
    }
    m.set(
        "GetStringWidth",
        lua.create_function(|lua, this: Table| Ok(measured_wh(lua, &this)?.0))?,
    )?;
    m.set(
        "GetStringHeight",
        lua.create_function(|lua, this: Table| Ok(measured_wh(lua, &this)?.1))?,
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
    m.set(
        "SetDrawLayer",
        lua.create_function(|lua, (this, layer, sub): (Table, String, Option<i64>)| {
            let rh = region_handle_of(lua, &this)?;
            let dl = draw_layer_from_str(&layer)
                .ok_or_else(|| mlua::Error::runtime(format!("unknown draw layer '{layer}'")))?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            if let Some(region) = model.arena.region_mut(rh) {
                region.draw_layer = dl;
                if let Some(s) = sub {
                    region.sub_level = s.clamp(i64::from(i8::MIN), i64::from(i8::MAX)) as i8;
                }
            }
            Ok(())
        })?,
    )?;

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
            lua.app_data_mut::<Model>()
                .expect("model")
                .region_data
                .entry(rh)
                .or_default()
                .justify_h = jh;
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
            lua.app_data_mut::<Model>()
                .expect("model")
                .region_data
                .entry(rh)
                .or_default()
                .justify_v = jv;
            Ok(())
        })?,
    )?;

    // SetRotation(radians) — spin the texture about its center, counterclockwise-positive (the
    // later-era Texture API, shipped early: the world-map player arrow's stand-in rotation —
    // see `QuadContent::Texture::rotation`). No-arg/nil resets to 0.
    m.set(
        "SetRotation",
        lua.create_function(|lua, (this, radians): (Table, Option<f32>)| {
            let rh = region_handle_of(lua, &this)?;
            lua.app_data_mut::<Model>()
                .expect("model")
                .region_data
                .entry(rh)
                .or_default()
                .rotation = radians.unwrap_or(0.0);
            Ok(())
        })?,
    )?;

    // SetTexCoord(left, right, top, bottom) — the 4-edge form (XML `<TexCoords>`): a UV sub-rect in
    // 0..1 texture space (top-left origin) the Texture region samples, slicing quadrant/atlas art
    // (decision 0084). SetTexCoord(ULx,ULy, LLx,LLy, URx,URy, LRx,LRy) — the 8-arg affine form: an
    // arbitrary UV quad (rotation/shear — the reference's `DrawRouteLine` route lines), stored per
    // corner in the renderer's screen winding.
    m.set(
        "SetTexCoord",
        lua.create_function(|lua, (this, rest): (Table, mlua::Variadic<f32>)| {
            let rh = region_handle_of(lua, &this)?;
            let coords = match rest.len() {
                4 => Some(TexCoords::Rect([rest[0], rest[1], rest[2], rest[3]])),
                // The live arg order is UL, LL, UR, LR (corner pairs); [`TexCoords::Corners`]
                // stores screen order [TL, TR, BR, BL].
                8 => Some(TexCoords::Corners([
                    [rest[0], rest[1]], // UL → TL
                    [rest[4], rest[5]], // UR → TR
                    [rest[6], rest[7]], // LR → BR
                    [rest[2], rest[3]], // LL → BL
                ])),
                // No args resets to the full texture (the live API's clear form).
                0 => None,
                n => {
                    return Err(mlua::Error::runtime(format!(
                        "SetTexCoord: expected 4 (edges) or 8 (corner pairs) args, got {n}"
                    )))
                }
            };
            lua.app_data_mut::<Model>()
                .expect("model")
                .region_data
                .entry(rh)
                .or_default()
                .tex_coords = coords;
            Ok(())
        })?,
    )?;
    // GetTexCoord() → left, right, top, bottom (the 4-edge form; full texture if never set; an
    // affine mapping reports its bounding edges).
    m.set(
        "GetTexCoord",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            let [l, r, t, b] = model
                .region_data
                .get(&rh)
                .and_then(|d| d.tex_coords)
                .map(|tc| tc.edges())
                .unwrap_or([0.0, 1.0, 0.0, 1.0]);
            Ok((l, r, t, b))
        })?,
    )?;
    // SetFontObject("GameFontNormal") — re-point this FontString at a named virtual Font object: its
    // resolved paint (face/height/color/outline) is copied into the region as the new default. An
    // explicit SetFont/SetTextColor afterward overrides. An unknown name errors (never silently no-op).
    m.set(
        "SetFontObject",
        lua.create_function(|lua, (this, name): (Table, String)| {
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let Some(font) = model.font_objects.get(&name).cloned() else {
                return Err(mlua::Error::runtime(format!(
                    "SetFontObject: no font object named '{name}' is registered"
                )));
            };
            let d = model.region_data.entry(rh).or_default();
            d.font_object = Some(name);
            d.font_path = font.font;
            d.font_height = font.height;
            d.outline = font.outline;
            d.font_shadow = font.shadow;
            if let Some(c) = font.color {
                d.color = Some(c);
            }
            if let Some(j) = font.justify_h {
                d.justify_h = j;
            }
            if let Some(j) = font.justify_v {
                d.justify_v = j;
            }
            Ok(())
        })?,
    )?;
    // GetFontObject() → the name of the last font object this FontString resolved (or nil).
    m.set(
        "GetFontObject",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            match model
                .region_data
                .get(&rh)
                .and_then(|d| d.font_object.clone())
            {
                Some(n) => Ok(Value::String(lua.create_string(&n)?)),
                None => Ok(Value::Nil),
            }
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
                if let Some(p) = path.filter(|p| !p.is_empty()) {
                    d.font_path = Some(p);
                }
                if let Some(h) = height {
                    d.font_height = Some(h);
                }
                if let Some(f) = flags {
                    d.outline = match f.to_ascii_uppercase() {
                        f if f.contains("THICK") => Outline::Thick,
                        f if f.contains("OUTLINE") => Outline::Normal,
                        _ => Outline::None,
                    };
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
    // SetTextColor(r, g, b [, a]) — a FontString's text color (distinct name from SetVertexColor,
    // same storage in v1: the resolved text color the renderer draws with).
    m.set(
        "SetTextColor",
        lua.create_function(
            |lua, (this, r, g, b, a): (Table, f32, f32, f32, Option<f32>)| {
                let rh = region_handle_of(lua, &this)?;
                lua.app_data_mut::<Model>()
                    .expect("model")
                    .region_data
                    .entry(rh)
                    .or_default()
                    .color = Some([r, g, b, a.unwrap_or(1.0)]);
                Ok(())
            },
        )?,
    )?;

    lua.set_named_registry_value(REG_REGION_METHODS, m)?;
    Ok(())
}

/// The layout [`super::layout::Handle`] a region anchors to by default: its **owner frame**'s id
/// (minted if needed), or [`SCREEN`] if the region has somehow lost its owner.
fn region_owner_id(model: &mut Model, rh: RegionHandle) -> u32 {
    match model.arena.region(rh).map(|r| r.owner) {
        Some(owner) => model.frame_id(owner),
        None => SCREEN,
    }
}

/// Resolve a `SetPoint`/`SetAllPoints` `relativeTo` argument (a frame/region wrapper table, a frame
/// name, or nil) to a layout id, defaulting to `owner` when absent/unresolved.
fn resolve_target(model: &mut Model, target: &Value, owner: u32) -> u32 {
    match target {
        Value::Table(t) => decode_id(t)
            .ok()
            .filter(|id| model.id_to_frame.contains_key(id) || model.id_to_region.contains_key(id))
            .unwrap_or(owner),
        Value::String(s) => s
            .to_str()
            .ok()
            .and_then(|n| {
                // Frames first (the client's global namespace is one; frames publish before their
                // regions build), then the region-name registry — the real XML anchors regions to
                // sibling regions by name (merchant label plate → `$parentSlot`).
                model
                    .arena
                    .lookup(n.as_ref())
                    .map(|h| model.frame_id(h))
                    .or_else(|| model.region_names.get(n.as_ref()).copied())
            })
            .unwrap_or_else(|| {
                // The owner fallback matches the frame path, but a *named* target that doesn't
                // resolve is almost always a bug — a typo, or an XML forward reference (anchors
                // resolve at SetPoint time, so a target must be declared before its dependents;
                // ItemTextFrame's scrollbar track landed on the parchment this way). Warn
                // instead of silently misdirecting the anchor.
                let who = model
                    .id_to_frame
                    .get(&owner)
                    .and_then(|&h| model.arena.frame(h))
                    .and_then(|f| f.name.clone())
                    .unwrap_or_else(|| "<anonymous>".into());
                model.warnings.push(format!(
                    "SetPoint(region of {who}): relativeTo '{}' does not resolve — anchored to the owner",
                    s.to_str().ok().as_deref().unwrap_or("<non-utf8>")
                ));
                owner
            }),
        _ => owner,
    }
}

/// Bit-exact equality for a region's explicit size — the layout gate's own lens
/// (`InputFingerprint::input` feeds `f32::to_bits`), so a setter's no-op detection and the gate
/// can never disagree; see [`anchor_bits_eq`].
fn size_bits_eq(a: Option<(f32, f32)>, b: Option<(f32, f32)>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some((aw, ah)), Some((bw, bh))) => {
            aw.to_bits() == bw.to_bits() && ah.to_bits() == bh.to_bits()
        }
        _ => false,
    }
}

/// `Region:SetPoint(point [, relativeTo [, relativePoint]] [, x, y])` — the region twin of
/// [`super::object`]'s frame `SetPoint`, writing [`super::RegionData::anchors`]. The overload is
/// disambiguated by argument *type* exactly as the frame version.
fn region_set_point(lua: &Lua, this: &Table, point: &str, rest: [Value; 4]) -> mlua::Result<()> {
    let point = point_from_str(point)
        .ok_or_else(|| mlua::Error::runtime(format!("SetPoint: unknown point '{point}'")))?;
    let rh = region_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model");
    let owner = region_owner_id(&mut model, rh);

    let mut cursor = 0usize;
    let rel_to_id: u32 = match rest.first() {
        Some(Value::Table(_) | Value::String(_) | Value::Nil) => {
            cursor = 1;
            resolve_target(&mut model, &rest[0], owner)
        }
        // A leading number is the `SetPoint(point, x, y)` overload — cursor stays at 0.
        _ => owner,
    };

    let mut rel_point = point;
    if let Some(Value::String(s)) = rest.get(cursor) {
        if let Some(p) = s.to_str().ok().and_then(|n| point_from_str(n.as_ref())) {
            rel_point = p;
            cursor += 1;
        }
    }

    let x = rest.get(cursor).map(as_f32).unwrap_or(0.0);
    let y = rest.get(cursor + 1).map(as_f32).unwrap_or(0.0);

    let data = model.region_data.entry(rh).or_default();
    let new = Anchor::new(point, rel_to_id, rel_point, x, y);
    // Same no-op law as the frame twin (`layout_methods::set_point`): idempotent only when the
    // bit-identical anchor already holds the tail and no earlier entry carries this point.
    let same_at_tail = data.anchors.last().is_some_and(|a| anchor_bits_eq(a, &new))
        && !data.anchors[..data.anchors.len() - 1]
            .iter()
            .any(|a| a.point == point);
    if !same_at_tail {
        data.anchors.retain(|a| a.point != point);
        data.anchors.push(new);
        model.touch_layout();
    }
    Ok(())
}
