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
            data.fill = None;
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
            data.fill = None;
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

    // SetParent(frame) — **a Texture/FontString really does have this**, and we were the ones
    // missing it. `SetParent` lives in the REGION method table (`0x7a1550`); Texture's class lookup
    // falls back to Region's at `0x79c650` and FontString's at `0x79ee50`, so both reach it (wow-re
    // `system/ui/scratch/widget-api-batch-benilla.md` Q7, §5-verified). `FuBar_FuXPFu.lua:210`'s
    // `self.Spark:SetParent(self.XPBar)` is not a broken addon.
    //
    // Four contract traps, each spelled out because each is a plausible implementation's silent
    // divergence:
    //
    //  · **The argument must be a FRAME.** `0x7a16ea` runs `IsA(FrameTag)` on it, and a Texture or
    //    Region argument raises `"…Wrong parent object type, expected frame"` (`0x87cb78`). Ours
    //    raises too — a re-parent that quietly did nothing is the silent-drop class of 1203/1205.
    //  · **A missing argument is NOT the nil form.** TNONE fails the `== LUA_TNIL` test and falls
    //    through to `"…Couldn't find region named '%s'"` (`0x87cb48`), so `tex:SetParent()` raises
    //    while `tex:SetParent(nil)` detaches. That is why this reads a `MultiValue`: mlua cannot
    //    tell an absent argument from an explicit nil any other way.
    //  · **Anchors are untouched.** The re-link moves draw-layer/region-list membership only; a
    //    re-parented Texture still anchors to whatever `SetPoint` named — which is why FuXPFu
    //    re-points its spark afterwards, and why our anchors (which store the resolved target id)
    //    need no fixing up at all.
    //  · **Zero return values**, on every path.
    //
    // The mechanism half — full re-link, layer and sub-level preserved, `nil` = orphaned but not
    // destroyed — is [`crate::widget::WidgetArena::set_region_owner`]'s doc.
    m.set(
        "SetParent",
        lua.create_function(|lua, args: mlua::MultiValue| {
            let mut it = args.into_iter();
            let Some(Value::Table(this)) = it.next() else {
                return Err(mlua::Error::runtime("SetParent: expected a region"));
            };
            let rh = region_handle_of(lua, &this)?;
            let Some(parent) = it.next() else {
                return Err(mlua::Error::runtime(
                    "SetParent(): Couldn't find region named '' (no argument)",
                ));
            };
            let wrong_type =
                || mlua::Error::runtime("SetParent(): Wrong parent object type, expected frame");
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let new_owner = match &parent {
                Value::Nil => None,
                // A region/font-object wrapper decodes to an id that is not a frame's (or to no id
                // at all) — both land on the same "expected frame" the reference raises.
                Value::Table(t) => Some(
                    decode_id(t)
                        .ok()
                        .and_then(|id| model.id_to_frame.get(&id).copied())
                        .ok_or_else(wrong_type)?,
                ),
                // A name resolves through the frame registry, as every other frame-target argument
                // does (`SetPoint`'s relativeTo, `SetParent` on the frame side).
                Value::String(s) => {
                    let name = s.to_str()?;
                    Some(model.arena.lookup(name.as_ref()).ok_or_else(|| {
                        mlua::Error::runtime(format!(
                            "SetParent(): Couldn't find region named '{}'",
                            name.as_ref()
                        ))
                    })?)
                }
                _ => return Err(wrong_type()),
            };
            if model.arena.set_region_owner(rh, new_owner) {
                // An un-anchored region draws relative to its owner, and an anchored one resolves
                // against the owner's rect and effective scale (`layout.rs`'s region sweep) — the
                // owner is a layout input, so a re-link is a layout change.
                model.touch_layout();
            }
            Ok(())
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
                let d = model.region_data.entry(rh).or_default();
                d.vertex_color = Some([r, g, b, a.unwrap_or(1.0)]);
                // The same slot `SetTextColor` writes on a FontString, so it is the same explicit
                // colour set as far as font-object inheritance is concerned.
                d.font_explicit.color = true;
                Ok(())
            },
        )?,
    )?;
    // GetVertexColor — the setter's own pair, a real 5875 binding (`0x79aa50`, wow-re
    // `system/ui/ledger.tsv`; it sits directly above `SetVertexColor 0x79abd0` in the same region
    // method family). Never set = the untinted white every region draws at by default.
    m.set(
        "GetVertexColor",
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
    // SetGradientAlpha(orientation, r1,g1,b1,a1, r2,g2,b2,a2) and its alpha-less twin
    // SetGradient(orientation, r1,g1,b1, r2,g2,b2) — the two-stop linear gradient the client
    // generates into the same `+0xcc` texture slot the colour form of SetTexture fills.
    //
    // These were missing, and they were the single wall in front of the corpus's largest family:
    // `FuBar\FuBar_Panel.lua:144` calls SetGradientAlpha while building the bar, so all 20 FuBar
    // plugins died there — after the debugstack/chunk-name fix got them that far.
    //
    // The orientation token is matched case-insensitively and anything that is not "VERTICAL" is
    // horizontal, which is the client's own leniency and matters because addons spell it both ways.
    // The gradient is stored WHOLE (see `RegionData::gradient`); the paint folds it to its midpoint
    // because a quad carries one tint today. That approximation is visible and is recorded at the
    // field, not hidden here.
    for (name, with_alpha) in [("SetGradientAlpha", true), ("SetGradient", false)] {
        m.set(
            name,
            lua.create_function(move |lua, args: mlua::MultiValue| {
                let mut it = args.into_iter();
                let this: Table = match it.next() {
                    Some(Value::Table(t)) => t,
                    _ => return Err(mlua::Error::runtime("expected a region")),
                };
                let orientation = match it.next() {
                    Some(Value::String(s)) => s.to_str()?.to_string(),
                    // A missing/!string orientation is horizontal, like any non-"VERTICAL" token.
                    _ => String::new(),
                };
                let n = if with_alpha { 8 } else { 6 };
                let mut c = [0.0f32; 8];
                for slot in c.iter_mut().take(n) {
                    *slot = it.next().as_ref().map(as_f32).unwrap_or(0.0);
                }
                let (start, end) = if with_alpha {
                    ([c[0], c[1], c[2], c[3]], [c[4], c[5], c[6], c[7]])
                } else {
                    // SetGradient has no alpha stops: both ends are opaque.
                    ([c[0], c[1], c[2], 1.0], [c[3], c[4], c[5], 1.0])
                };
                let rh = region_handle_of(lua, &this)?;
                let mut model = lua.app_data_mut::<Model>().expect("model");
                let d = model.region_data.entry(rh).or_default();
                d.gradient = Some(super::Gradient {
                    vertical: orientation.eq_ignore_ascii_case("VERTICAL"),
                    start,
                    end,
                });
                Ok(())
            })?,
        )?;
    }
    m.set(
        "SetTexture",
        // The trailing three are `Value`, not `Option<f32>`, and that is a fidelity fix rather than
        // laxity. The path form reads ONE argument (`0x770200`); only the colour form
        // (`0x770360`) reads up to four. A C function takes what it wants off the Lua stack and
        // ignores the rest, so `SetTexture(path, true)` is fine on the real client — and typing
        // these as `Option<f32>` made us raise on it, `bad argument #3: error converting Lua
        // boolean to f32`, in a call the client accepts silently.
        //
        // Found by `_LazyPig/LazyPigMenu.lua:182`
        // (`texture_title:SetTexture("Interface\DialogFrame\UI-DialogBox-Header", true)`), which
        // reached us only once the survey started seating the addon registry — the whole point of
        // that instrument fix. The stray `true` is meaningless in 1.12 and the addon author
        // presumably meant a later client's second parameter; either way the client shrugs.
        lua.create_function(
            |lua, (this, arg, g, b, a): (Table, Value, Value, Value, Value)| {
                let rh = region_handle_of(lua, &this)?;
                let mut model = lua.app_data_mut::<Model>().expect("model");
                let data = model.region_data.entry(rh).or_default();
                // A plain SetTexture makes the region ordinary again — drop any portrait circular mask
                // and any live-unit-portrait binding.
                data.circular = false;
                data.portrait_unit = None;
                // Both forms write the SAME `+0xcc` texture slot — the path form loads a file
                // there (`0x770200`), the colour form generates an 8×8 solid into it
                // (`0x770360`) — so each clears the other. NEITHER touches the vertex colour at
                // `+0xb8`: a tint outlives the art it was tinting.
                match &arg {
                    // SetTexture("") clears, same as SetTexture(nil) — the ref lua blanks state
                    // art with the empty string (QuestLogFrame.lua:165-166).
                    Value::String(s) if s.to_str()?.is_empty() => {
                        data.texture = None;
                        data.fill = None;
                    }
                    Value::String(s) => {
                        data.texture = Some(s.to_str()?.to_string());
                        data.fill = None;
                    }
                    // The colour form, and the ONLY branch that looks at the trailing three. A
                    // non-numeric there takes the same default a missing one does, which is what
                    // reading off a C stack does: `lua_tonumber` on a non-number yields 0.
                    Value::Number(_) | Value::Integer(_) => {
                        let chan = |v: &Value, dflt: f32| match v {
                            Value::Number(_) | Value::Integer(_) => as_f32(v),
                            Value::String(s) => s
                                .to_str()
                                .ok()
                                .and_then(|s| s.parse::<f32>().ok())
                                .unwrap_or(dflt),
                            _ => dflt,
                        };
                        data.fill =
                            Some([as_f32(&arg), chan(&g, 0.0), chan(&b, 0.0), chan(&a, 1.0)]);
                        data.texture = None;
                    }
                    // SetTexture(nil) clears (the live API's blank-the-region form); a cleared
                    // texture region draws nothing.
                    Value::Nil => {
                        data.texture = None;
                        data.fill = None;
                    }
                    _ => {}
                }
                Ok(())
            },
        )?,
    )?;
    // SetDesaturated(flag) -> shaderSupported — Texture only (`0x79c1e0`, wow-re ledger; the
    // reference's own `ItemButtonTemplate.lua:69` is `local shaderSupported =
    // icon:SetDesaturated(desaturated)`).
    //
    // **The RETURN is the whole design, and it is the half a plausible implementation drops.**
    // 1.12 ran on cards that could not do the shader, so the verb reports whether it took effect
    // and FrameXML falls back by hand:
    //
    //     if ( not desaturated ) then r,g,b = 1,1,1
    //     elseif ( not r or not shaderSupported ) then r,g,b = 0.5,0.5,0.5 end
    //     icon:SetVertexColor(r, g, b)
    //
    // We have no desaturation in the renderer, so we answer **nil — unsupported**, which is a real
    // 1.12 machine's answer and not a lie. Claiming support would suppress that grey fallback and
    // leave disabled icons drawn at full colour: strictly worse-looking than saying no. When a
    // desaturating shader lands, flip the return and this comment with it.
    //
    // Why it matters far past one verb: **98 of the 109 addons that draw and then raise on being
    // used, raise here** — `FuBar_Panel.lua:43`'s right-click reaches Dewdrop's `AddLine`
    // (`Dewdrop-2.0.lua:2172`), which calls `button.arrow:SetDesaturated(true)` unguarded. A
    // static scan costed this at 61 addons and it was declined; the use-probe costed it at 98 the
    // moment anyone right-clicks.
    m.set(
        "SetDesaturated",
        lua.create_function(|lua, (this, flag): (Table, Value)| {
            let on = !matches!(flag, Value::Nil | Value::Boolean(false));
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            model.region_data.entry(rh).or_default().desaturated = on;
            // nil, not false: the reference's `shaderSupported` is a 1|nil C answer and callers
            // write `not shaderSupported`.
            Ok(Value::Nil)
        })?,
    )?;
    // GetTexture() — Texture only (`0x79ba70`), verified in wow-re's widget-method batch
    // (`system/ui/scratch/widget-api-batch-benilla.md`). Three contract details are each the kind a
    // plausible implementation gets silently wrong, so each is spelled out:
    //
    //  · **Exactly ONE return value**, never a multi-return.
    //  · **The colour form returns the literal string `"Solid Texture"`** (`0x835708`), NOT nil.
    //    `SetTexture(r,g,b)` synthesizes an 8x8 solid, and the getter reports that name — so an
    //    addon's `if not tex then` passes straight through on a colour-filled region. Returning nil
    //    here would look tidier and would be wrong in the one direction callers test for.
    //  · **The path is stripped at the LAST `.`** (`0x79baf0`): the loader appends `.blp`/`.tga` to
    //    what was set and the getter strips an extension back off. Taken verbatim rather than
    //    "strip only a real extension" — a directory containing a dot is mangled by the real client
    //    too, and this surface is a transcription, not an improvement.
    //
    // Four corpus addons: `AtlasQuest.lua:228` (`AQATLASMAP = AtlasMap:GetTexture()`) and
    // `FuBarPlugin-2.0.lua:343` (`return self.iconFrame:GetTexture()`), each reached by two addons.
    m.set(
        "GetTexture",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_mut::<Model>().expect("model");
            let Some(data) = model.region_data.get(&rh) else {
                return Ok(None);
            };
            if data.fill.is_some() {
                return Ok(Some("Solid Texture".to_string()));
            }
            Ok(data.texture.as_ref().map(|t| match t.rfind('.') {
                Some(i) => t[..i].to_string(),
                None => t.clone(),
            }))
        })?,
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
        // The key carries the owner's effective_scale ([`RegionData::measure_key`]) — the same
        // recipe the request loop stamps, or every read under a SetScale'd owner reports stale.
        let scale = model
            .arena
            .region(rh)
            .and_then(|r| model.arena.frame(r.owner))
            .map(|f| f.effective_scale)
            .unwrap_or(1.0);
        let m = d.and_then(|d| d.measured.filter(|m| m.key == d.measure_key(scale)));
        let size = d.and_then(|d| d.size);
        let w = m.map(|m| m.w).or(size.map(|s| s.0)).unwrap_or(0.0);
        let h = m.map(|m| m.h).or(size.map(|s| s.1)).unwrap_or(0.0);
        Ok((w, h))
    }
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
            let name = super::font::resolve("SetFontObject", &font)?;
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
            super::font::repaint(d, &fo);
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
                Some(n) => Ok(Value::Table(super::font::wrapper(lua, &n)?)),
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
