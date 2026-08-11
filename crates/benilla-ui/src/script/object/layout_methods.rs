//! Frame method-table cluster: anchoring and size — `SetPoint`/`ClearAllPoints`/`GetPoint`/
//! `SetAllPoints`/`SetWidth`/`SetHeight`/`SetSize`/`GetWidth`/`GetHeight` and the resolved-edge
//! readers (`GetLeft`/`GetRight`/`GetTop`/`GetBottom`). Split out of [`super`] purely for size — see
//! its module doc for the shared id/handle plumbing and method-table wiring.

use mlua::{Lua, Table, Value};

use crate::layout::{Anchor, Point};
use crate::script::{Model, SCREEN};
use crate::widget::FrameHandle;

use super::{as_f32, decode_id, frame_handle_of, frame_wrapper, point_from_str, point_name};

/// Populate `m`'s layout (anchor/size) methods (see the module doc).
pub(super) fn install(lua: &Lua, m: &Table) -> mlua::Result<()> {
    // Layout: SetPoint / ClearAllPoints / SetSize / SetWidth / SetHeight / GetWidth / GetHeight
    m.set(
        "SetPoint",
        lua.create_function(
            |lua, (this, p, a2, a3, a4, a5): (Table, String, Value, Value, Value, Value)| {
                set_point(lua, &this, &p, [a2, a3, a4, a5])
            },
        )?,
    )?;
    m.set(
        "ClearAllPoints",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            // Every layout setter here follows one law: mutate ONLY on an actual value change,
            // and report the change to the tier-1 epoch (`touch_layout`). The compare is what
            // keeps an idempotent per-frame caller (the classic OnUpdate re-SetPoint idiom) from
            // pinning the gate open — the same absorption the fingerprint gives, paid once at
            // the write instead of per-frame over the whole model.
            let changed = match model.layout_inputs.get_mut(&h) {
                Some(input) if !input.anchors.is_empty() => {
                    input.anchors.clear();
                    true
                }
                _ => false,
            };
            if changed {
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;
    // GetPoint([n]) → point, relativeTo, relativePoint, xOfs, yOfs — the n-th (1-based, default
    // first) anchor. relativeTo is nil when the target is the screen root (the client returns
    // UIParent there; benilla has no UIParent wrapper yet — stated, a consensus-list call).
    m.set(
        "GetPoint",
        lua.create_function(|lua, (this, n): (Table, Option<i64>)| {
            let h = frame_handle_of(lua, &this)?;
            let anchor = {
                let model = lua.app_data_ref::<Model>().expect("model");
                let idx = (n.unwrap_or(1).max(1) - 1) as usize;
                model
                    .layout_inputs
                    .get(&h)
                    .and_then(|i| i.anchors.get(idx))
                    .cloned()
            };
            let Some(a) = anchor else {
                return Ok((Value::Nil, Value::Nil, Value::Nil, Value::Nil, Value::Nil));
            };
            let rel = if a.relative_to == SCREEN {
                Value::Nil
            } else {
                Value::Table(frame_wrapper(lua, a.relative_to)?)
            };
            Ok((
                Value::String(lua.create_string(point_name(a.point))?),
                rel,
                Value::String(lua.create_string(point_name(a.relative_point))?),
                Value::Number(f64::from(a.x_off)),
                Value::Number(f64::from(a.y_off)),
            ))
        })?,
    )?;
    // SetAllPoints([relativeTo]) — pin TOPLEFT+BOTTOMRIGHT to the target (default: the parent),
    // the XML `setAllPoints="true"` behavior as a method (rf24 `0x767800`'s SetAllPoints path).
    m.set(
        "SetAllPoints",
        lua.create_function(|lua, (this, target): (Table, Value)| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let rel_id: u32 = match &target {
                // Same id_to_region fallback as `set_point` above (and `region.rs`'s own
                // `resolve_target`, which already checked both) — a table wrapper may name a region.
                Value::Table(t) => decode_id(t)
                    .ok()
                    .filter(|id| {
                        model.id_to_frame.contains_key(id) || model.id_to_region.contains_key(id)
                    })
                    .unwrap_or_else(|| default_parent_id(&mut model, h)),
                Value::String(s) => {
                    // Frames first, then the region-name registry: the real XML anchors frames to
                    // REGIONS too (gossip option rows → the greeting FontString) — resolve() binds
                    // region targets in its second round.
                    let named = s.to_str().ok().and_then(|n| {
                        model
                            .arena
                            .lookup(n.as_ref())
                            .map(|hh| model.frame_id(hh))
                            .or_else(|| model.region_names.get(n.as_ref()).copied())
                    });
                    named.unwrap_or_else(|| default_parent_id(&mut model, h))
                }
                _ => default_parent_id(&mut model, h),
            };
            let pair = [
                Anchor::new(Point::TopLeft, rel_id, Point::TopLeft, 0.0, 0.0),
                Anchor::new(Point::BottomRight, rel_id, Point::BottomRight, 0.0, 0.0),
            ];
            let input = model.layout_inputs.entry(h).or_default();
            let same = input.anchors.len() == 2
                && input
                    .anchors
                    .iter()
                    .zip(&pair)
                    .all(|(a, b)| anchor_bits_eq(a, b));
            if !same {
                input.anchors.clear();
                input.anchors.extend_from_slice(&pair);
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;
    m.set(
        "SetWidth",
        lua.create_function(|lua, (this, w): (Table, f32)| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let input = model.layout_inputs.entry(h).or_default();
            let changed = input.width.to_bits() != w.to_bits();
            input.width = w;
            if changed {
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;
    m.set(
        "SetHeight",
        lua.create_function(|lua, (this, ht): (Table, f32)| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let input = model.layout_inputs.entry(h).or_default();
            let changed = input.height.to_bits() != ht.to_bits();
            input.height = ht;
            if changed {
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;
    m.set(
        "SetSize",
        lua.create_function(|lua, (this, w, ht): (Table, f32, f32)| {
            let h = frame_handle_of(lua, &this)?;
            let mut model = lua.app_data_mut::<Model>().expect("model");
            let input = model.layout_inputs.entry(h).or_default();
            let changed =
                input.width.to_bits() != w.to_bits() || input.height.to_bits() != ht.to_bits();
            input.width = w;
            input.height = ht;
            if changed {
                model.touch_layout();
            }
            Ok(())
        })?,
    )?;
    m.set(
        "GetWidth",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(size_read(&model, h, true))
        })?,
    )?;
    m.set(
        "GetHeight",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(size_read(&model, h, false))
        })?,
    )?;

    // GetCenter() → centerX, centerY — the resolved rect's center in LOCAL units (y-up; screen ÷
    // the frame's effective scale — the client's convention: coordinate getters report the frame's
    // own scaled space, and callers divide GetCursorPosition (screen px) by GetEffectiveScale to
    // meet them there; the ref world map's hover math does exactly that). nil pair before the
    // first resolve, like the edge readers.
    m.set(
        "GetCenter",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            let inv = 1.0 / eff_scale(&model, h);
            Ok(match model.resolved.get(&h) {
                Some(r) => (
                    Value::Number(f64::from((r.left + r.right) * 0.5 * inv)),
                    Value::Number(f64::from((r.bottom + r.top) * 0.5 * inv)),
                ),
                None => (Value::Nil, Value::Nil),
            })
        })?,
    )?;

    // GetEffectiveScale() — the frame's real effective scale (parentScale · ownScale, the arena's
    // propagated product). benilla has no uiScale CVar, so the root factor is 1; a SetScale'd
    // subtree (the windowed world map) reports its true factor, and the reference's
    // `GetCursorPosition()/GetEffectiveScale()` transcriptions convert screen→local correctly.
    m.set(
        "GetEffectiveScale",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(f64::from(eff_scale(&model, h)))
        })?,
    )?;

    // GetLeft/GetRight/GetTop/GetBottom — the frame's RESOLVED edges in LOCAL units (y-up, so
    // GetBottom is height-from-screen-bottom, the era semantics; `GetRect 0x768320`'s Lua faces,
    // layout.rs's module doc; screen ÷ effective scale like GetCenter). `nil` before the first
    // resolve — callers treat that as "not laid out yet" (the ref's own FauxScrollFrame code
    // nil-checks these too).
    for (name, pick) in [
        ("GetLeft", 0u8),
        ("GetRight", 1u8),
        ("GetTop", 2u8),
        ("GetBottom", 3u8),
    ] {
        m.set(
            name,
            lua.create_function(move |lua, this: Table| {
                let h = frame_handle_of(lua, &this)?;
                let model = lua.app_data_mut::<Model>().expect("model");
                let inv = 1.0 / eff_scale(&model, h);
                Ok(model.resolved.get(&h).map(|r| {
                    inv * match pick {
                        0 => r.left,
                        1 => r.right,
                        2 => r.top,
                        _ => r.bottom,
                    }
                }))
            })?,
        )?;
    }
    Ok(())
}

/// The frame's effective scale, ε-guarded (a zero would poison the local-unit division).
pub(super) fn eff_scale(model: &Model, h: FrameHandle) -> f32 {
    let s = model
        .arena
        .frame(h)
        .map(|f| f.effective_scale)
        .unwrap_or(1.0);
    if s.abs() < 1e-6 {
        1.0
    } else {
        s
    }
}

/// `GetWidth`/`GetHeight`: the resolved rect's span in LOCAL units (screen ÷ effective scale — the
/// client returns the value as authored, and `SetWidth(w)` on a scaled frame resolves to `w·scale`
/// screen px) if `resolve` has produced one, else the explicit size the frame was given
/// (`SetWidth`/`SetHeight`/`SetSize`, already local) — matching the client's "0 = derive".
fn size_read(model: &Model, h: FrameHandle, width: bool) -> f32 {
    if let Some(r) = model.resolved.get(&h) {
        let span = if width { r.width() } else { r.height() };
        return span / eff_scale(model, h);
    }
    model
        .layout_inputs
        .get(&h)
        .map(|i| if width { i.width } else { i.height })
        .unwrap_or(0.0)
}

/// `SetPoint(point [, relativeTo [, relativePoint]] [, x, y])` (RF-0023/RF-0024). Defaults per the
/// client: `relativeTo` = the frame's parent (the screen root if top-level), `relativePoint` =
/// `point`. Overload disambiguated by argument *type* (a string/table after `point` is `relativeTo`;
/// a number is the first offset).
fn set_point(lua: &Lua, this: &Table, point: &str, rest: [Value; 4]) -> mlua::Result<()> {
    let point = point_from_str(point)
        .ok_or_else(|| mlua::Error::runtime(format!("SetPoint: unknown point '{point}'")))?;
    let h = frame_handle_of(lua, this)?;

    let mut model = lua.app_data_mut::<Model>().expect("model");

    // relativeTo: a table (wrapper) or a string (name), else default to the parent/screen. A table
    // wrapper may name a FRAME or a REGION (a Button anchored to a FontString by direct reference —
    // e.g. `button:SetPoint("TOPLEFT", someFontString, "BOTTOMLEFT", ...)` — is exactly as legal in
    // the real client as anchoring to a frame, and the string-branch below already accepts both), so
    // this must check id_to_region too, not just id_to_frame: an id that only lives in id_to_region
    // used to fail the frame-only filter and silently fall back to the default parent, misdirecting
    // the anchor with no error (caught via the quest-log reward rows, which anchor Buttons to
    // FontString regions by table reference — QuestLogFrame.xml's own comment on the port).
    let mut cursor = 0usize;
    let rel_to_id: u32 = match rest.first() {
        Some(Value::Table(t)) => {
            cursor = 1;
            decode_id(t)
                .ok()
                .filter(|id| {
                    model.id_to_frame.contains_key(id) || model.id_to_region.contains_key(id)
                })
                .unwrap_or_else(|| default_parent_id(&mut model, h))
        }
        Some(Value::String(s)) => {
            cursor = 1;
            // Frames first, then regions by name (see SetAllPoints above — same rationale).
            let named = s.to_str().ok().and_then(|n| {
                model
                    .arena
                    .lookup(n.as_ref())
                    .map(|hh| model.frame_id(hh))
                    .or_else(|| model.region_names.get(n.as_ref()).copied())
            });
            named.unwrap_or_else(|| {
                // The parent fallback is the client's behavior, but a *named* target that
                // doesn't resolve is almost always a bug (a typo, or an XML forward reference —
                // anchors resolve at SetPoint time, so a target must be declared before its
                // dependents). Say so instead of silently misdirecting the anchor: this exact
                // silence cost a hunt twice (QuestLogFrame's reward rows, ItemTextFrame's
                // scrollbar track).
                let who = model
                    .arena
                    .frame(h)
                    .and_then(|f| f.name.clone())
                    .unwrap_or_else(|| "<anonymous>".into());
                model.warnings.push(format!(
                    "SetPoint({who}): relativeTo '{}' does not resolve — anchored to the parent",
                    s.to_str().ok().as_deref().unwrap_or("<non-utf8>")
                ));
                default_parent_id(&mut model, h)
            })
        }
        // An *explicit* `nil` relativeTo (`SetPoint(point, nil, relPoint, x, y)`) means "default
        // target" but still occupies its argument slot — consume it so the relativePoint/offsets
        // that follow line up. Only a leading *number* (the `SetPoint(point, x, y)` overload) or a
        // truly-absent arg leaves the cursor at 0.
        Some(Value::Nil) => {
            cursor = 1;
            default_parent_id(&mut model, h)
        }
        _ => default_parent_id(&mut model, h),
    };

    // relativePoint: a string in the next slot, else defaults to `point`.
    let mut rel_point = point;
    if let Some(Value::String(s)) = rest.get(cursor) {
        if let Some(p) = s.to_str().ok().and_then(|n| point_from_str(n.as_ref())) {
            rel_point = p;
            cursor += 1;
        }
    }

    let x = rest.get(cursor).map(as_f32).unwrap_or(0.0);
    let y = rest.get(cursor + 1).map(as_f32).unwrap_or(0.0);

    let input = model.layout_inputs.entry(h).or_default();
    let new = Anchor::new(point, rel_to_id, rel_point, x, y);
    // No-op detection must mirror the retain+push below EXACTLY (the fingerprint hashes the vec
    // in order, so a same-anchor call that would still REORDER the vec is a real change): a call
    // is idempotent only when the identical anchor already sits at the tail and no earlier entry
    // carries this point. Anchor's derived PartialEq compares f32 by value, which calls -0.0 ==
    // 0.0 where the fingerprint's bit-compare would not — compare bits, or the verify assert
    // trips on that (authored-XML-real) edge.
    let same_at_tail = input
        .anchors
        .last()
        .is_some_and(|a| anchor_bits_eq(a, &new))
        && !input.anchors[..input.anchors.len() - 1]
            .iter()
            .any(|a| a.point == point);
    if !same_at_tail {
        input.anchors.retain(|a| a.point != point);
        input.anchors.push(new);
        model.touch_layout();
    }
    Ok(())
}

/// Bit-exact anchor equality — the same lens the layout gate's fingerprint reads anchors through
/// (`InputFingerprint::anchors` feeds `f32::to_bits`), so the setters' no-op detection and the
/// gate can never disagree about whether a write "changed" something.
pub(crate) fn anchor_bits_eq(a: &Anchor, b: &Anchor) -> bool {
    a.point == b.point
        && a.relative_to == b.relative_to
        && a.relative_point == b.relative_point
        && a.x_off.to_bits() == b.x_off.to_bits()
        && a.y_off.to_bits() == b.y_off.to_bits()
}

/// The default `relativeTo` id for a `SetPoint` with no explicit target: the frame's parent id, or
/// [`SCREEN`] if it is top-level (the client anchors top-level frames to `UIParent`/the screen root).
fn default_parent_id(model: &mut Model, h: FrameHandle) -> u32 {
    match model.arena.frame(h).and_then(|f| f.parent) {
        Some(p) => model.frame_id(p),
        None => SCREEN,
    }
}
