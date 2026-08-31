//! Region method-table cluster: **paint** — what a Texture shows and how it is tinted,
//! blended, cropped and layered. Split out of `region.rs` at the 0716 file-size budget.

use mlua::{Lua, MultiValue, Table, Value};

use crate::script::object::{as_f32, draw_layer_from_str, draw_layer_name};
use crate::script::{Model, TexCoords};

/// Resolve `self` (a region wrapper) to its live [`RegionHandle`].
use super::region_handle_of;

/// Populate `m`'s paint methods (see the module doc).
pub(super) fn install(lua: &Lua, m: &Table) -> mlua::Result<()> {
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
                d.gradient = Some(crate::script::Gradient {
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
        // The RETURN is part of the contract (wow-re `widget-api-batch-benilla.md` Q1, VERIFIED):
        // the path form answers **1 | nil — nil meaning the file did not load** — and the colour
        // and clear forms answer 1. Atlas is the load-bearing caller: `Atlas_Refresh` does
        // `local builtIn = AtlasMap:SetTexture("…\Images\Maps\"..zoneID)` and walks its plugin
        // fallback chain on nil, so a binding that returns nothing draws no map at all whatever
        // the file resolver does. The load verdict comes from the host's [`Model::texture_probe`]
        // (existence across patch chain + loose addon folder — the same candidate walk the
        // renderer resolves with); a VM with no probe installed has no backend, so its path form
        // stays nil, which is both literally true and what every engine-less test always observed.
        lua.create_function(
            |lua, (this, arg, g, b, a): (Table, Value, Value, Value, Value)| {
                let rh = region_handle_of(lua, &this)?;
                // Scoped, so the model borrow is provably gone before the layout touch below —
                // the path arm hands it back mid-way (to call the host probe without holding a
                // mutable borrow across a host callback) and the others do not.
                let (loaded, derived) = {
                    let mut model = lua.app_data_mut::<Model>().expect("model");
                    // A plain SetTexture makes the region ordinary again — drop any portrait circular mask
                    // and any live-unit-portrait binding.
                    let data = model.region_data.entry(rh).or_default();
                    data.circular = false;
                    data.portrait_unit = None;
                    // **`SetTexture` CLEARS the desaturation** (wow-re `texture-desaturate-law.md` §2.3,
                    // VERIFIED): `+0x128` is a `CGxShader*`, and `CSimpleTexture::SetTexture`
                    // (`0x770200`) writes it from its 4th stack arg, for which the Lua binding
                    // (`0x79bb40`) pushes slot 0 — permanently NULL — on both of its legs. A
                    // re-implementation that keeps a desaturate boolean independent of the texture
                    // handle diverges on every `icon:SetDesaturated(1)` followed by `icon:SetTexture(t)`.
                    //
                    // Scoped exactly as the binary scopes it:
                    //  · the **same path** is inert — `0x770225` returns before ever reaching the write,
                    //    so a repaint that re-sets the icon it already shows keeps its grey;
                    //  · `nil`/`""` DO clear — the `test esi,esi` leg falls through to the write;
                    //  · the **colour form** does NOT — that is `0x770360`, which is not among the four
                    //    writers of the field.
                    let same_path = matches!((&arg, &data.texture),
                    (Value::String(s), Some(cur)) if s.to_str().is_ok_and(|s| *s == **cur));
                    let colour_form = matches!(&arg, Value::Number(_) | Value::Integer(_));
                    if !same_path && !colour_form {
                        data.desaturated = false;
                    }
                    // Does this region's rect come from its ART? An axis authored `0` takes its span
                    // from the content (decision 1349, `script::layout::content_span`), so on that
                    // shape — and only there — swapping the texture MOVES the region and the resolve
                    // has to hear about it. Read before the match, which writes the art and never the
                    // size.
                    //
                    // **An ANCHOR-LESS region is not that shape**, whatever its size: with no
                    // pinned edge and no center there is nothing for a span to be added to
                    // (`combine_edge` needs one), so every edge stays unset — which is why the
                    // resolve sweep skips such regions outright. Painting one is a paint, not a
                    // layout change, and saying otherwise would re-open the change gate on every
                    // `CreateTexture(…):SetTexture(…)` in the UI (decisions 0740/1385/1388).
                    let derived = !data.anchors.is_empty()
                        && data.size.is_none_or(|(w, h)| w == 0.0 || h == 0.0);
                    // Both forms write the SAME `+0xcc` texture slot — the path form loads a file
                    // there (`0x770200`), the colour form generates an 8×8 solid into it
                    // (`0x770360`) — so each clears the other. NEITHER touches the vertex colour at
                    // `+0xb8`: a tint outlives the art it was tinting.
                    let loaded = match &arg {
                        // SetTexture("") clears, same as SetTexture(nil) — the ref lua blanks state
                        // art with the empty string (QuestLogFrame.lua:165-166). The return for ""
                        // is read as the path form's failure (nothing loads from an empty name);
                        // INFERRED — no corpus caller reads it.
                        Value::String(s) if s.to_str()?.is_empty() => {
                            data.texture = None;
                            data.fill = None;
                            false
                        }
                        Value::String(s) => {
                            let path = s.to_str()?.to_string();
                            data.texture = Some(path.clone());
                            data.fill = None;
                            drop(model);
                            let model = lua.app_data_ref::<Model>().expect("model");
                            model
                                .texture_probe
                                .as_ref()
                                .is_some_and(|probe| probe(&path))
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
                            true
                        }
                        // SetTexture(nil) clears (the live API's blank-the-region form); a cleared
                        // texture region draws nothing. Returns 1 (Q1's `SetTexture(nil) / ()` row).
                        Value::Nil => {
                            data.texture = None;
                            data.fill = None;
                            true
                        }
                        _ => false,
                    };
                    (loaded, derived)
                };
                // Named precisely, and only on the content-derived shape: an ordinary sized icon's
                // rect cannot move here, and touching the layout on every icon repaint would
                // re-open the resolve's change gate every frame for a rect nobody moved
                // (decisions 0740/1385/1388).
                if derived {
                    lua.app_data_mut::<Model>()
                        .expect("model")
                        .touch_layout_region(rh);
                }
                Ok(if loaded {
                    Value::Number(1.0)
                } else {
                    Value::Nil
                })
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
    // The renderer now greys the texel (decision 1327 — `ui_quad.wgsl`'s luminance fold), so we
    // answer **1 — supported**, and the branch above takes its shader arm: the icon goes greyscale
    // AND wears the caller's own dim tint. Until 1327 this answered nil, which is a real 1.12
    // card's answer and was the honest one while nothing greyed — but it costs the *look*: a 0.65
    // grey multiply on colourful art is a dimmer, still-colourful icon, which is precisely what
    // B162 reported against the talent tree.
    //
    // Why it matters far past one verb: **98 of the 109 addons that draw and then raise on being
    // used, raise here** — `FuBar_Panel.lua:43`'s right-click reaches Dewdrop's `AddLine`
    // (`Dewdrop-2.0.lua:2172`), which calls `button.arrow:SetDesaturated(true)` unguarded. A
    // static scan costed this at 61 addons and it was declined; the use-probe costed it at 98 the
    // moment anyone right-clicks.
    //
    // **The argument's truth table is not `if flag then`** (wow-re `texture-desaturate-law.md` §1.1,
    // VERIFIED — `0x6f1c10(L, 2, default=1)` dispatched through the jump table at `0x6f1ce8`). Two
    // of its arms are the opposite of the obvious reading, and both are reachable:
    //  · **no argument at all is ON**, not off — `LUA_TNONE` takes the `ja` default arm, so
    //    `tex:SetDesaturated()` greys. This is why the flag arrives as a `MultiValue`: mlua hands a
    //    missing `Value` parameter through as `Nil`, which is the one arm that means OFF.
    //  · **a number truncating to 0 is OFF** (`0x6f3620`+`0x40a2b0`), so `SetDesaturated(0)` clears.
    //  · a table/function/userdata is ON (same default arm as absent).
    // The string arm (`0x6f1c51`, comparing against `0x871460`/`0x853758`) is NOT modelled — no
    // corpus caller passes one, and the comparands were not read; a string takes the ON arm here.
    m.set(
        "SetDesaturated",
        lua.create_function(|lua, args: MultiValue| {
            let mut args = args.into_iter();
            let this: Table = match args.next() {
                Some(Value::Table(t)) => t,
                _ => return Ok(Value::Nil),
            };
            let on = match args.next() {
                None => true,
                Some(Value::Nil) => false,
                Some(Value::Boolean(b)) => b,
                Some(Value::Integer(i)) => i != 0,
                Some(Value::Number(n)) => n.trunc() != 0.0,
                Some(_) => true,
            };
            let rh = region_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            model.region_data.entry(rh).or_default().desaturated = on;
            // 1, not true: the reference's `shaderSupported` is a 1|nil C answer (byte-verified —
            // `0x6f3810 lua_pushnumber(L, 1.0)`, never `false`, never zero values) and callers
            // write `not shaderSupported`.
            Ok(Value::Number(1.0))
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

    // GetDrawLayer() — the setter's pair, present on BOTH region leaves in the client (Texture
    // `0x79a6c0`, FontString `0x79c660`, each its own copy — read off the method-table pair bytes).
    //
    // This module's own note used to end "GetDrawLayer is in the client's pair and absent here;
    // absent is absent" — accurate, and a gap rather than a decision. pfUI's `GetNoNameObject`
    // (api/api.lua:1342) reads it off every child while reskinning a Blizzard frame, and died
    // there; that is the measurement (1719).
    //
    // Returns the layer NAME alone. The sub-level we store beside it is not a second return here:
    // 1.12's pair is layer-only, and `SetDrawLayer`'s optional sub-level argument above is already
    // marked as this engine's extension point.
    m.set(
        "GetDrawLayer",
        lua.create_function(|lua, this: Table| {
            let rh = region_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            let dl = model
                .arena
                .region(rh)
                .map_or(crate::order::DrawLayer::Artwork, |r| r.draw_layer);
            Ok(draw_layer_name(dl))
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
    Ok(())
}

impl crate::script::UiScript {
    /// Install the host's texture-path oracle — the resolver behind the path form of
    /// `SetTexture`'s **1 | nil** return ([`Model::texture_probe`]). The host hands in existence
    /// over its real stores (patch chain + loose addon folder); a VM that never gets one keeps
    /// answering nil for every path, the engine-less truth.
    pub fn set_texture_probe(&mut self, probe: crate::script::TextureProbe) {
        self.model_mut().texture_probe = Some(probe);
    }

    /// Install the host's texture **texel-size** oracle — what lets a region with an authored size
    /// of `0` on an axis take that span from its art, as the client's virtual size getters do
    /// ([`Model::texture_size_probe`], decision 1349 / wow-re `region-size-fallback.md` §2). A VM
    /// that never gets one leaves such a region exactly where it was.
    pub fn set_texture_size_probe(&mut self, probe: crate::script::TextureSizeProbe) {
        self.model_mut().texture_size_probe = Some(probe);
    }
}
