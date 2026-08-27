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
    // GetNumPoints() → how many anchors this frame carries. On the Region map (`0x87c9b8`), so
    // every widget answers it — the region twin shipped first and noted this side was missing;
    // collapsing the map to one implementation each (decision 1501) is what made the gap fatal
    // rather than merely absent, and this is the arm it was missing.
    m.set(
        "GetNumPoints",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(model
                .layout_inputs
                .get(&h)
                .map_or(0, |i| i.anchors.len() as i64))
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
                // A size write moves no edge and no roster membership (decision 1388).
                model.touch_layout_frame(h);
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
                model.touch_layout_frame(h);
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
                model.touch_layout_frame(h);
            }
            Ok(())
        })?,
    )?;
    m.set(
        "GetWidth",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            settle(lua);
            let model = lua.app_data_ref::<Model>().expect("model");
            Ok(size_read(&model, h, true))
        })?,
    )?;
    m.set(
        "GetHeight",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            settle(lua);
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
            settle(lua);
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
                settle(lua);
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
/// **Resolve the layout graph NOW if anything has moved since the last pass.**
///
/// The client answers a geometry query against current layout; ours cached rects in a per-frame
/// `resolve()` pass, so a query made EARLIER IN THE SAME CALL STACK than that pass read nil. That
/// is not an edge case — it is what every menu does. `Dewdrop-2.0.lua` (embedded in ~65 corpus
/// addons) creates its menu frame, anchors it, shows it and then measures it inside one `OnClick`:
///
/// ```lua
/// local left = frame:GetLeft()                                              -- l.1942, nil for us
/// frame:SetPoint(point, parent, relativePoint, curX - left - width / 2, 0)  -- l.1960, dies
/// ```
///
/// **97 of the 108 addons that drew and then raised on being touched died on that one line.** No
/// missing verb: `GetLeft` was always there, and always answered nil.
///
/// Cheap on a SETTLED tree: `resolve_layout`'s tier-1 gate is one epoch comparison that returns
/// immediately when nothing has been touched, so a run of getters pays a compare each.
///
/// **The interleaved case is not, and this is measured rather than asserted.** A write bumps the
/// epoch, so `SetPoint; GetLeft; SetPoint; GetLeft; …` resolves the whole graph once per iteration.
/// Timed on a 200-frame tree: 30 alternating pairs cost **1.41 ms** against **54 µs** for the same
/// writes with a single read at the end — ~26x, about 47 µs per settle, and the per-settle half
/// scales with the GRAPH, not the loop.
///
/// That is exactly the shape Dewdrop's menu builder has, so opening a menu pays it once. It buys a
/// menu that works at all, which is the trade taken here. If it ever reads as a hitch, the fix is a
/// narrower resolve (the queried frame's subtree), not a return to the stale cache.
///
/// **It deliberately does NOT fire `OnSizeChanged`.** That drain runs Lua handlers, and re-entering
/// Lua from inside a binding is how a borrow panic or an unbounded recursion happens. The
/// size-change queue is drained by the next real `UiScript::resolve`, one tick later than the
/// reference's per-rect-application fire (`ApplyRect 0x76b580`). Stated rather than hidden.
fn settle(lua: &Lua) {
    let mut model = lua.app_data_mut::<Model>().expect("model");
    crate::script::UiScript::resolve_layout(&mut model);
}

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
        // Value-only unless the target moved (decision 1388) — the per-frame `SetPoint` idiom
        // (a dragged window, a moving spark) re-points the SAME anchor at the SAME target with new
        // offsets, so it names its node and the cached graph survives the frame.
        //
        // A retarget names its node too now (decision 1625): the edge set is not derivable from a
        // per-node hash, but it IS derivable from the anchors, and both lists are right here. The
        // target lists are collected only on the structural path — the value-only one is the hot
        // idiom and must stay allocation-free.
        let structural = anchor_retarget_is_structural(&input.anchors, &new);
        let old_targets: Option<Vec<u32>> =
            structural.then(|| input.anchors.iter().map(|a| a.relative_to).collect());
        input.anchors.retain(|a| a.point != point);
        input.anchors.push(new);
        match old_targets {
            None => model.touch_layout_frame(h),
            Some(old) => {
                let new_targets: Vec<u32> = model.layout_inputs[&h]
                    .anchors
                    .iter()
                    .map(|a| a.relative_to)
                    .collect();
                model.touch_layout_retarget_frame(h, &old, &new_targets);
            }
        }
    }
    Ok(())
}

/// Would this `SetPoint` change the node's set of anchor TARGETS — i.e. is it structural?
///
/// The layout scope's reverse edges are built from `Anchor::relative_to` (decision 1350): "this
/// node reads that node's rect". An anchor whose OFFSETS moved is a value change — the node
/// re-solves and nothing else does, which is exactly what a precise touch claims. An anchor whose
/// TARGET moved is not: an edge has to disappear and another to appear, and no per-node hash can
/// say so.
///
/// 1388 answered that by throwing the cached graph away. **Since decision 1625 it does not have
/// to be**: the write site holds both target lists, so the node's edges are re-pointed in place
/// (`Model::touch_layout_retarget_frame`) and the roster and every other node's hash survive.
/// What this predicate decides is therefore no longer "value change or catastrophe" but which of
/// two precise touches to use — and it still earns its place, because the value-only answer is
/// the hot per-frame idiom and must stay allocation-free.
///
/// Mirrors the `retain(point) + push` the setters do. It is value-only in exactly one shape — one
/// existing anchor carries this point and keeps the same target. Zero (an edge appears) and two or
/// more (edges disappear) are both structural, and so is any change of target.
pub(crate) fn anchor_retarget_is_structural(anchors: &[Anchor], new: &Anchor) -> bool {
    let mut same_point = anchors.iter().filter(|a| a.point == new.point);
    match (same_point.next(), same_point.next()) {
        (Some(old), None) => old.relative_to != new.relative_to,
        _ => true,
    }
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
