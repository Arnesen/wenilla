//! The `Model` widget's method surface — the 3D pane an addon or a FrameXML frame parks a model in.
//!
//! **The same split as [`super::minimap`] and [`super::cooldown`], and for the same reason.** The
//! engine core holds exactly the scene the Lua API reads and writes ([`ModelState`]); the pixels
//! are the app renderer's job. That is the posture the `<Minimap>` and cooldown widgets already
//! run under — a sized hole the game layer draws into — and this widget is the third of that
//! shape, not a new compromise.
//!
//! ## Why this surface, in this order
//!
//! It is the wall four of the twenty most-installed 1.12 addons hit at once. pfUI's action-bar
//! module builds the pet bar's autocast shine as
//!
//! ```lua
//! f.autocast = CreateFrame("Model", nil, f)
//! f.autocast:SetModel("Interface\\Buttons\\UI-AutoCastButton.mdx")
//! f.autocast:SetSequence(0)
//! ```
//!
//! and pfUI is embedded in pfQuest, pfQuest-turtle and ShaguDPS as well — so one missing verb
//! stopped all four dead, each of them *after* the whole rest of the UI had built.
//!
//! ## The names are read off the binary, not assumed
//!
//! wow-re's registered-binding scan lists the **scene** half of this widget and not the rest —
//! `SetPosition 0x76dc00`, `SetLight 0x76e1e0`, `GetLight 0x76e7d0`, `GetPosition 0x76ea40`,
//! `SetFogColor 0x76ee60`, `GetFogColor 0x76f080`. It has no row for `SetModel` or its siblings,
//! which would ordinarily mean "not a 1.12 verb" and would make publishing one a fidelity error of
//! exactly the kind decision 1189 warns about (a name we have and the reference lacks routes an
//! addon down a path the real client never takes).
//!
//! So it was checked against the shipped image rather than inferred either way: an isolated-string
//! scan of `WoW.exe` finds **one occurrence each** of `SetModel`, `ClearModel`, `GetModel`,
//! `SetSequence`, `SetSequenceTime`, `SetRotation`, `SetFacing`, `SetModelScale`, `SetCamera`,
//! `SetUnit` and `RefreshUnit`. The reference's own FrameXML corroborates the ones it uses —
//! `SetRotation` ×10 (TabardFrame, UIParent), `SetSequence` ×7, `SetSequenceTime` ×5, `SetUnit`
//! ×9, `RefreshUnit` ×3. **These are 1.12 verbs; wow-re's scan is incomplete for this widget**,
//! and that is reported back to wow-re rather than quietly worked around here.
//!
//! ## What is deliberately NOT here
//!
//! `SetFogFar`/`SetFogNear` (nothing in the shipped chain or the corpus calls them) and any
//! interpretation of `SetLight`'s numbers — the engine core has no lighting model, so the tuple is
//! stored verbatim rather than typed into a scene semantics nobody has verified.

use mlua::{Lua, MultiValue, Table, Value};

use super::object::frame_handle_of;
use super::Model;
use crate::widget::{KindState, ModelState};

/// Registry key of the Model method table (the MAXCSTACK discipline: Lua-side root, named key).
pub(super) const REG_MODEL_METHODS: &str = "__benilla_model_methods";

/// Run `f` over a frame's Model state under one short write borrow. Errors if `this` is not a live
/// Model (unreachable through the kind dispatcher, but the method table is a plain Lua value — a
/// caller can fish it out and misapply it).
fn with_model<T>(lua: &Lua, this: &Table, f: impl FnOnce(&mut ModelState) -> T) -> mlua::Result<T> {
    let h = frame_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let frame = model
        .arena
        .frame_mut(h)
        .ok_or_else(|| mlua::Error::runtime("stale frame handle"))?;
    match &mut frame.kind_state {
        KindState::Model(m) => Ok(f(m)),
        _ => Err(mlua::Error::runtime("not a Model")),
    }
}

/// A Lua number-ish → f32, `nil` and non-numbers → 0.0.
///
/// The widget's setters are all coordinates and angles, and the corpus passes them through
/// arithmetic that can produce a nil (`C.bars[...].icon_size / 25` when the config key is absent).
/// The reference marshals through `lua_tonumber`, which is this.
fn num(v: &Value) -> f32 {
    match v {
        Value::Number(n) => *n as f32,
        Value::Integer(i) => *i as f32,
        Value::String(s) => s.to_str().ok().and_then(|t| t.parse().ok()).unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Same, as an integer (sequence and camera indices).
fn int(v: &Value) -> i32 {
    num(v) as i32
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;

    // ── Content: the model, the unit, and clearing ──────────────────────────────────────────
    //
    // `SetModel` and `SetUnit` are the two ways a pane gets content and they are alternatives, not
    // layers: setting one clears the other, so `GetModel` after a `SetUnit` cannot answer a stale
    // path from three frames ago. (The dress-up and paper-doll frames drive the `SetUnit` arm; every
    // addon in the corpus drives the path arm.)
    m.set(
        "SetModel",
        lua.create_function(|lua, (this, path): (Table, Value)| {
            let path = match &path {
                Value::String(s) => Some(s.to_str()?.to_string()),
                // `SetModel(nil)` is the documented clear, and reaching it through the same setter
                // is how the corpus writes "no model" — not everything calls ClearModel.
                _ => None,
            };
            with_model(lua, &this, |m| {
                m.path = path;
                m.unit = None;
            })
        })?,
    )?;
    m.set(
        "GetModel",
        lua.create_function(|lua, this: Table| with_model(lua, &this, |m| m.path.clone()))?,
    )?;
    m.set(
        "ClearModel",
        lua.create_function(|lua, this: Table| {
            with_model(lua, &this, |m| {
                m.path = None;
                m.unit = None;
                m.sequence_time = None;
            })
        })?,
    )?;
    m.set(
        "SetUnit",
        lua.create_function(|lua, (this, unit): (Table, Value)| {
            let unit = match &unit {
                Value::String(s) => Some(s.to_str()?.to_string()),
                _ => None,
            };
            with_model(lua, &this, |m| {
                m.unit = unit;
                m.path = None;
            })
        })?,
    )?;
    // `RefreshUnit()` re-reads the unit the pane is already showing — for us a no-op with a live
    // receiver check, because our pane holds the unit TOKEN and resolves it at render, so there is
    // no cached appearance here to invalidate. Present because the reference's own
    // `DressUpFrame`/`PaperDollFrame` call it (3 sites) and an addon that hooks them will too.
    m.set(
        "RefreshUnit",
        lua.create_function(|lua, this: Table| with_model(lua, &this, |_| ()))?,
    )?;

    // ── Animation ───────────────────────────────────────────────────────────────────────────
    m.set(
        "SetSequence",
        lua.create_function(|lua, (this, seq): (Table, Value)| {
            let seq = int(&seq);
            with_model(lua, &this, |m| {
                m.sequence = seq;
                // A fresh sequence starts unscrubbed: `SetSequenceTime` is a scrub INTO the
                // current sequence, so carrying the old pair across a change would park the new
                // animation at a time that belongs to the previous one. The cooldown indicator
                // drives exactly this pair every frame and is the reason to get it right.
                m.sequence_time = None;
            })
        })?,
    )?;
    m.set(
        "SetSequenceTime",
        lua.create_function(|lua, (this, seq, ms): (Table, Value, Value)| {
            let pair = (int(&seq), int(&ms));
            with_model(lua, &this, |m| {
                m.sequence = pair.0;
                m.sequence_time = Some(pair);
            })
        })?,
    )?;

    // ── The pane's view: yaw, scale, camera, position ───────────────────────────────────────
    //
    // `SetRotation` and `SetFacing` are TWO NAMES FOR ONE SLOT. Both are in the binary; the shipped
    // FrameXML drives the tabard and character panes with `SetRotation` (10 sites) and calls
    // `SetFacing` nowhere, while addons reach for either. A pane with two independent yaws would be
    // a bug nobody could see until a frame used both.
    for name in ["SetRotation", "SetFacing"] {
        m.set(
            name,
            lua.create_function(|lua, (this, rad): (Table, Value)| {
                let rad = num(&rad);
                with_model(lua, &this, |m| m.facing = rad)
            })?,
        )?;
    }
    m.set(
        "GetFacing",
        lua.create_function(|lua, this: Table| with_model(lua, &this, |m| m.facing))?,
    )?;
    m.set(
        "SetModelScale",
        lua.create_function(|lua, (this, s): (Table, Value)| {
            let s = num(&s);
            with_model(lua, &this, |m| m.scale = s)
        })?,
    )?;
    m.set(
        "GetModelScale",
        lua.create_function(|lua, this: Table| with_model(lua, &this, |m| m.scale))?,
    )?;
    m.set(
        "SetCamera",
        lua.create_function(|lua, (this, c): (Table, Value)| {
            let c = int(&c);
            with_model(lua, &this, |m| m.camera = c)
        })?,
    )?;
    m.set(
        "SetPosition",
        lua.create_function(|lua, (this, x, y, z): (Table, Value, Value, Value)| {
            let p = (num(&x), num(&y), num(&z));
            with_model(lua, &this, |m| m.position = p)
        })?,
    )?;
    m.set(
        "GetPosition",
        lua.create_function(|lua, this: Table| {
            let (x, y, z) = with_model(lua, &this, |m| m.position)?;
            Ok((x, y, z))
        })?,
    )?;

    // ── The scene: light and fog ────────────────────────────────────────────────────────────
    //
    // `SetLight`'s numbers are stored VERBATIM and handed back verbatim. The engine core has no
    // lighting model, so typing this tuple would be asserting a scene semantics nobody has
    // verified — and a wrong typing is worse than an opaque one, because it reads as knowledge.
    m.set(
        "SetLight",
        lua.create_function(|lua, args: MultiValue| {
            let mut it = args.into_iter();
            let this = match it.next() {
                Some(Value::Table(t)) => t,
                _ => return Err(mlua::Error::runtime("expected a Model")),
            };
            let nums: Vec<f32> = it.map(|v| num(&v)).collect();
            with_model(lua, &this, |m| m.light = Some(nums))
        })?,
    )?;
    m.set(
        "GetLight",
        lua.create_function(|lua, this: Table| {
            let light = with_model(lua, &this, |m| m.light.clone())?;
            let out = light
                .unwrap_or_default()
                .into_iter()
                .map(|n| Value::Number(f64::from(n)))
                .collect::<Vec<_>>();
            Ok(MultiValue::from_vec(out))
        })?,
    )?;
    m.set(
        "SetFogColor",
        lua.create_function(|lua, (this, r, g, b): (Table, Value, Value, Value)| {
            let c = (num(&r), num(&g), num(&b));
            with_model(lua, &this, |m| m.fog_color = Some(c))
        })?,
    )?;
    m.set(
        "GetFogColor",
        lua.create_function(|lua, this: Table| {
            let c = with_model(lua, &this, |m| m.fog_color)?;
            Ok(match c {
                Some((r, g, b)) => MultiValue::from_vec(vec![
                    Value::Number(f64::from(r)),
                    Value::Number(f64::from(g)),
                    Value::Number(f64::from(b)),
                ]),
                None => MultiValue::new(),
            })
        })?,
    )?;

    lua.set_named_registry_value(REG_MODEL_METHODS, m)
}
