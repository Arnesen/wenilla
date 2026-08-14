//! The **region** side of the object model (RF-0023's distinct-tag leaves): the `Texture`/
//! `FontString` wrapper cache, their shared metatable, and the region method surface. Split from
//! [`super::object`] (which keeps the frame side + `CreateFrame`) so each grows along its own
//! axis — frames grow per-kind method tables ([`super::statusbar`], [`super::button`]), regions
//! grow paint/coords methods here.

use mlua::{Lua, Table, Value};

use super::object::{anchor_bits_eq, as_f32, decode_id, id_to_lud, point_from_str};
use super::{Model, REG_REGION_META, REG_REGION_METHODS, REG_WRAPPERS, SCREEN};
use crate::layout::Anchor;
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

mod layout;
mod paint;
mod text;

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

    // The three clusters this file was split into (0716's budget). Order is immaterial —
    // every one of them only writes into the same method table.
    paint::install(lua, &m)?;
    text::install(lua, &m)?;
    layout::install(lua, &m)?;

    lua.set_named_registry_value(REG_REGION_METHODS, m)?;
    Ok(())
}

/// The layout [`super::layout::Handle`] a region anchors to by default: its **owner frame**'s id
/// (minted if needed), or [`SCREEN`] if the region has somehow lost its owner.
pub(super) fn region_owner_id(model: &mut Model, rh: RegionHandle) -> u32 {
    match model.arena.region(rh).map(|r| r.owner) {
        Some(owner) => model.frame_id(owner),
        None => SCREEN,
    }
}

/// Resolve a `SetPoint`/`SetAllPoints` `relativeTo` argument (a frame/region wrapper table, a frame
/// name, or nil) to a layout id, defaulting to `owner` when absent/unresolved.
pub(super) fn resolve_target(model: &mut Model, target: &Value, owner: u32) -> u32 {
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
pub(super) fn size_bits_eq(a: Option<(f32, f32)>, b: Option<(f32, f32)>) -> bool {
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
pub(super) fn region_set_point(
    lua: &Lua,
    this: &Table,
    point: &str,
    rest: [Value; 4],
) -> mlua::Result<()> {
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

/// The measured extent a FontString reports, falling back to an explicit `SetSize`.
///
/// Hoisted out of the text cluster when this file split (0716): `GetStringWidth`/`GetStringHeight`
/// live in `region::text` and `GetWidth`/`GetHeight` in `region::layout`, and both read it.
pub(super) fn measured_wh(lua: &Lua, this: &Table) -> mlua::Result<(f32, f32)> {
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
