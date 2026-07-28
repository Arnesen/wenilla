//! The GameTooltip Lua verb table — [`install`] registers the widget methods (SetOwner /
//! AddLine / AddDoubleLine / SetText / ClearLines / SetMinimumWidth / SetPadding / FadeOut /
//! IsOwned / NumLines / the anchor law) over the parent module's line-pool mechanics, then
//! chains the item/unit content builders' method sets.

use mlua::{Lua, Table, Value, Variadic};

use crate::layout::{Anchor, Point};
use crate::script::object::{frame_handle_of, frame_wrapper};
use crate::script::Model;

use super::{
    append_line, bool_arg, cancel_fade, clear_content, fire_cleared, hide_tooltip, now,
    parse_line_color, parse_line_tail, set_shown, text_of, tip_mut, with_tip, REG_TOOLTIP_METHODS,
};

pub(in crate::script) fn install(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;

    // SetOwner(owner, anchorType [, x, y]) — remember the owner, clear the content (the engine
    // fires OnTooltipCleared: a fresh owner never inherits the last hover's lines or money), and
    // point the plate per the anchor law. The six owner anchors are the documented 1.12 API set
    // (spec-faithful, same posture as StatusBar's contract; the corpus exercises RIGHT/LEFT/
    // BOTTOMRIGHT); ANCHOR_NONE leaves pointing to the caller (GameTooltip_SetDefaultAnchor),
    // ANCHOR_PRESERVE keeps the previous placement.
    m.set(
        "SetOwner",
        lua.create_function(
            |lua,
             (this, owner, anchor, x, y): (
                Table,
                Table,
                Option<String>,
                Option<f32>,
                Option<f32>,
            )| {
                let h = frame_handle_of(lua, &this)?;
                let owner_h = frame_handle_of(lua, &owner)?;
                {
                    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                    clear_content(&mut model, h);
                    cancel_fade(&mut model, h);
                    tip_mut(&mut model, h)?.owner = Some(owner_h);
                    let owner_id = model.frame_id(owner_h);
                    let anchor = anchor.unwrap_or_else(|| "ANCHOR_RIGHT".into());
                    let pts = match anchor.to_ascii_uppercase().as_str() {
                        "ANCHOR_RIGHT" => Some((Point::BottomLeft, Point::TopRight)),
                        "ANCHOR_LEFT" => Some((Point::BottomRight, Point::TopLeft)),
                        "ANCHOR_TOPRIGHT" => Some((Point::BottomRight, Point::TopRight)),
                        "ANCHOR_TOPLEFT" => Some((Point::BottomLeft, Point::TopLeft)),
                        "ANCHOR_BOTTOMRIGHT" => Some((Point::TopLeft, Point::BottomRight)),
                        "ANCHOR_BOTTOMLEFT" => Some((Point::TopRight, Point::BottomLeft)),
                        "ANCHOR_NONE" | "ANCHOR_PRESERVE" => None,
                        other => {
                            model.warnings.push(format!(
                                "SetOwner: unknown anchor '{other}' (kept ANCHOR_RIGHT)"
                            ));
                            Some((Point::BottomLeft, Point::TopRight))
                        }
                    };
                    match pts {
                        Some((own, rel)) => {
                            let new =
                                Anchor::new(own, owner_id, rel, x.unwrap_or(0.0), y.unwrap_or(0.0));
                            let input = model.layout_inputs.entry(h).or_default();
                            // The no-op compare keeps the per-frame SetOwner idiom (the bag
                            // hover's OnUpdate re-enter) from dirtying tier 1 by itself; the
                            // content clear above already reports its own writes.
                            let same = input.anchors.len() == 1
                                && crate::script::object::anchor_bits_eq(&input.anchors[0], &new);
                            if !same {
                                input.anchors = vec![new];
                                model.touch_layout();
                            }
                        }
                        None if anchor.eq_ignore_ascii_case("ANCHOR_NONE") => {
                            // The caller points it (ClearAllPoints+SetPoint) — drop ours now so a
                            // stale owner anchor never wins the frame the caller forgets to.
                            let dropped = match model.layout_inputs.get_mut(&h) {
                                Some(input) if !input.anchors.is_empty() => {
                                    input.anchors.clear();
                                    true
                                }
                                _ => false,
                            };
                            if dropped {
                                model.touch_layout();
                            }
                        }
                        None => {} // ANCHOR_PRESERVE
                    }
                }
                fire_cleared(lua, h);
                Ok(())
            },
        )?,
    )?;
    m.set(
        "GetOwner",
        lua.create_function(|lua, this: Table| {
            let owner = with_tip(lua, &this, |t| t.owner)?;
            let id = {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                owner
                    .filter(|&h| model.arena.frame(h).is_some())
                    .map(|h| model.frame_id(h))
            };
            match id {
                Some(id) => Ok(Value::Table(frame_wrapper(lua, id)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    // IsOwned(frame) — the hover re-enter loop's gate (ref ContainerFrame.lua OnUpdate): true
    // only while `frame` is the live owner; the owner drops on hide, so a hidden tooltip never
    // resurrects from a stale OnUpdate.
    m.set(
        "IsOwned",
        lua.create_function(|lua, (this, frame): (Table, Table)| {
            let owner = with_tip(lua, &this, |t| t.owner)?;
            let fh = frame_handle_of(lua, &frame)?;
            Ok(owner == Some(fh))
        })?,
    )?;

    // SetText(text, r, g, b, alpha, textWrap) — the byte-pinned `0x531b90` signature (alpha is
    // a REAL parsed argument here, its own isnumber gate, default 1.0 — unlike AddLine's forced
    // opaque; GameTooltip_AddNewbieTip passes `(text, r, g, b, 1, 1)`). `text` is REQUIRED — a
    // non-string raises the binding's own 'Usage: SetText("text" [, color])' FrameScript error
    // and adds no line. No colour args → the engine default GOLD. Clears, writes line 1,
    // SHOWS — the corpus never calls Show after SetText (PaperDoll's empty-slot text relies
    // on it).
    m.set(
        "SetText",
        lua.create_function(|lua, (this, args): (Table, Variadic<Value>)| {
            let h = frame_handle_of(lua, &this)?;
            if !matches!(
                args.first(),
                Some(Value::String(_)) | Some(Value::Number(_)) | Some(Value::Integer(_))
            ) {
                return Err(mlua::Error::RuntimeError(
                    "Usage: SetText(\"text\" [, color])".into(),
                ));
            }
            let text = text_of(args.first());
            let [r, g, b, _] = parse_line_color(args.get(1), args.get(2), args.get(3));
            let a = match args.get(4) {
                Some(Value::Number(n)) => *n as f32,
                Some(Value::Integer(i)) => *i as f32,
                _ => 1.0,
            };
            let wrap = bool_arg(args.get(5));
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            append_line(lua, &this, (text, [r, g, b, a]), None, wrap)?;
            set_shown(lua, h, true);
            Ok(())
        })?,
    )?;
    // AddLine(text [, r, g, b [, wrap]]) — positional; the corpus' archaic `(text, "", r, g, b)`
    // shape has a non-number in the r-slot, so the real binding drops the colour tail and the
    // line renders the default GOLD (the ref's zone/taxi/repair tooltips — director-matched).
    // Appends without showing.
    m.set(
        "AddLine",
        lua.create_function(|lua, (this, args): (Table, Variadic<Value>)| {
            let text = text_of(args.first());
            let (color, wrap) = parse_line_tail(&args[1.min(args.len())..]);
            append_line(lua, &this, (text, color), None, wrap)
        })?,
    )?;
    // AddDoubleLine(textL, textR, rL, gL, bL, rR, gR, bR [, wrap]) — the two-column line
    // (slot|type, damage|speed), byte-pinned `0x531840`: each side's colour gates on ITS OWN
    // r-slot isnumber (default gold, g/b ungated → 0.0, alpha forced opaque), and the wrap arg
    // is dead weight — the core forces wrap=0 whenever the right text is non-empty. The right
    // column right-flushes in the layout pass.
    m.set(
        "AddDoubleLine",
        lua.create_function(
            |lua, (this, tl, tr, cl): (Table, Value, Value, Variadic<Value>)| {
                let left_color = parse_line_color(cl.first(), cl.get(1), cl.get(2));
                let right_color = parse_line_color(cl.get(3), cl.get(4), cl.get(5));
                append_line(
                    lua,
                    &this,
                    (text_of(Some(&tl)), left_color),
                    Some((text_of(Some(&tr)), right_color)),
                    false,
                )
            },
        )?,
    )?;
    m.set(
        "ClearLines",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                clear_content(&mut model, h);
            }
            fire_cleared(lua, h);
            Ok(())
        })?,
    )?;
    m.set(
        "NumLines",
        lua.create_function(|lua, this: Table| with_tip(lua, &this, |t| t.num_lines as i64))?,
    )?;
    // SetMinimumWidth(w) — a floor on the auto-sized width (the ref's SetTooltipMoney calls it
    // with the money row's width so coins never overhang the plate).
    m.set(
        "SetMinimumWidth",
        lua.create_function(|lua, (this, w): (Table, f32)| {
            with_tip(lua, &this, |t| t.min_width = w.max(0.0))
        })?,
    )?;
    // SetPadding(w) — extra auto-size width (ref ItemRefTooltip OnLoad: SetPadding(16) keeps the
    // corner close button off the text). A frame property — persists across content clears.
    m.set(
        "SetPadding",
        lua.create_function(|lua, (this, w): (Table, f32)| {
            with_tip(lua, &this, |t| t.padding = w.max(0.0))
        })?,
    )?;
    // FadeOut() — the world-mouseover loss ramp (alpha 1→0 over TOOLTIP_FADE_SECS, INTERIM
    // constant, then a real hide). Fresh content or Show cancels it.
    m.set(
        "FadeOut",
        lua.create_function(|lua, this: Table| {
            let t = now(lua);
            with_tip(lua, &this, |tip| {
                if tip.fade_start.is_none() {
                    tip.fade_start = Some(t);
                }
            })
        })?,
    )?;
    // Show/Hide — shadow the shared pair (kind tables resolve first) to keep the tooltip's
    // state honest: Show cancels a fade at full alpha; Hide drops the owner + content and fires
    // OnTooltipCleared (the ref clears its money row through exactly that script).
    m.set(
        "Show",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            {
                let mut model = lua.app_data_mut::<Model>().expect("model app_data");
                cancel_fade(&mut model, h);
            }
            set_shown(lua, h, true);
            Ok(())
        })?,
    )?;
    m.set(
        "Hide",
        lua.create_function(|lua, this: Table| {
            let h = frame_handle_of(lua, &this)?;
            hide_tooltip(lua, h);
            Ok(())
        })?,
    )?;

    // The item content channels (SetItemById/SetBagItem/SetMerchantItem/SetBuybackItem) live in
    // their own module — the one shared renderer + the red usable law (decision 0274 P1).
    crate::script::tooltip_item::install_methods(lua, &m)?;
    // The spell/aura/action content channels (SetSpell/SetShapeshift/SetPlayerBuff/SetAction) —
    // the verified spell-builder law (decision 0274 P2, grounded by 0276).
    crate::script::tooltip_spell::install_methods(lua, &m)?;
    // The talent channel (SetTalent) — the spell builder with the talent interleave
    // (decision 0304).
    crate::script::talent::install_tooltip_method(lua, &m)?;
    // The unit content channel (SetUnit + the world-mouseover drivers) — the verified unit law
    // (decision 0274 P3, grounded by 0276).
    crate::script::tooltip_unit::install_methods(lua, &m)?;

    lua.set_named_registry_value(REG_TOOLTIP_METHODS, m)?;
    Ok(())
}
