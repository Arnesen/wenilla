//! The model-pane family's method surfaces — `Model` and `PlayerModel`, the 3D panes an addon or a
//! FrameXML frame parks a model in.
//!
//! **The same split as [`super::minimap`] and [`super::cooldown`], and for the same reason.** The
//! engine core holds exactly the scene the Lua API reads and writes ([`ModelState`]); the pixels
//! are the app renderer's job. That is the posture the `<Minimap>` and cooldown widgets already
//! run under — a sized hole the game layer draws into.
//!
//! ## Two tables, chained — not one table with everything in it
//!
//! 1.12 registers **four** model-pane types, and each has its own Lua method table that **never
//! repeats its base's entries**; a derived pane reaches its base's verbs through the miss leg of
//! `vtable+0x8` (wow-re `ui/scratch/model-pane-method-tables.md`, byte-enumerated 2026-08-30):
//!
//! ```text
//! CSimpleFrame 0x778590
//! └─ CSimpleModel          0x76f870   table 0x878948 (23)   <Model>          ← built here
//!    └─ CGCharacterModelBase 0x506260  table 0x84f1fc (3)   <PlayerModel>    ← built here
//!       ├─ DressUpModelFrame 0x5050d0  table 0x84f190 (3)   <DressUpModel>   ← not built
//!       └─ TabardModel       0x503bd0  table 0x84ee40 (10)  <TabardModel>    ← not built
//! ```
//!
//! Our `__index` dispatcher already walks a *slice* of registry keys per kind, so the chain is
//! `&[REG_PLAYERMODEL_METHODS, REG_MODEL_METHODS]` and neither table duplicates the other. The
//! direction is derived → base **only**: a plain `<Model>` does not acquire `SetUnit`.
//!
//! `DressUpModel` (`Undress`/`Dress`/`TryOn`) and `TabardModel` (10 tabard verbs) are deliberately
//! **not built**: zero callers in the corpus, and our dress-up window already models the intents
//! host-side (`super::dressup`), so wiring them is a design change to that subsystem rather than a
//! missing verb. Named, not stubbed (decision 1134 §4).
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
//! and its unit frames build portraits as `CreateFrame("PlayerModel", …)` driven by `SetUnit` +
//! `SetCamera`. pfUI is embedded in pfQuest, pfQuest-turtle and ShaguDPS as well, so one missing
//! type stopped all four dead, each of them *after* the whole rest of the UI had built.
//!
//! ## Ownership is read off the registrar, never off a string scan
//!
//! This module published `SetUnit`, `RefreshUnit` and `SetRotation` on `Model` until 2026-08-30,
//! on the strength of an isolated-string scan of `WoW.exe` finding one occurrence of each. **A
//! `strings` hit cannot settle ownership** — it answers *whether a name exists*, and `SetUnit`'s
//! single pooled string `0x84f22c` is referenced by **two** method-table entries in two different
//! tables (`PlayerModel 0x84f1fc[0]` and `GameTooltip 0x854290`). All three are `PlayerModel`'s.
//!
//! The question a `strings` hit *does* settle, and the reason it was reached for: wow-re's
//! registrar-dump tool silently missed six of the 23 widget tables — including this whole family —
//! so the "not in wow-re's scan" half of the old header was a tooling defect, not a fidelity fact.
//! The enumeration recipe that replaces both is §5.1 of the note above: census `(call|jmp)
//! 0x701d80`, read the count from the registering `mov edx`, read the pairs at
//! `base + 8*i`, and settle "which table owns method M" by counting image-wide dword references to
//! M's name VA.
//!
//! ## What is deliberately NOT here
//!
//! Seven of `Model`'s own 23 — `AdvanceTime 0x76eca0`, `ReplaceIconTexture 0x76ed70`,
//! `SetFogNear 0x76f1e0`, `GetFogNear 0x76f2d0`, `SetFogFar 0x76f390`, `GetFogFar 0x76f480`,
//! `ClearFog 0x76f540`. No corpus caller, and their bodies are uncarved: `ClearFog`'s exact effect
//! on the colour/near/far triple is a guess until someone reads it, and a guessed clear reads as
//! knowledge. Named, not stubbed.
//!
//! Also absent, and correctly so: `SetCreature` and `SetCustomRace`. Neither string exists in
//! 5875 in any form (substring scan of the whole mapped image returns 0, against a positive
//! control of 27 hits for `Creature`) — they are later-expansion names, and publishing one would
//! be decision 1189's error.
//!
//! And any interpretation of `SetLight`'s numbers — the engine core has no lighting model, so the
//! tuple is stored verbatim rather than typed into a scene semantics nobody has verified.

use mlua::{Lua, MultiValue, Table, Value};

use super::object::frame_handle_of;
use super::Model;
use crate::widget::{KindState, ModelState};

/// Registry key of the `Model` method table (the MAXCSTACK discipline: Lua-side root, named key).
pub(super) const REG_MODEL_METHODS: &str = "__benilla_model_methods";

/// Registry key of the `PlayerModel` method table — its **own three** entries only. The other 23
/// names a `<PlayerModel>` answers come from [`REG_MODEL_METHODS`] through the dispatcher's chain,
/// exactly as the client's `0x506260` reaches `0x76f870` on a miss.
pub(super) const REG_PLAYERMODEL_METHODS: &str = "__benilla_playermodel_methods";

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

/// **Shape A's gate** (decision 1717's taxonomy, for a *string* position): the client's
/// `lua_isstring` accepts tags 3|4 — a string **or a number** — and nothing else. A number is
/// rendered to decimal text and used as the path, which is why this coerces rather than matching
/// `Value::String` alone.
fn string_arg(v: &Value) -> Option<std::borrow::Cow<'_, str>> {
    match v {
        Value::String(s) => s
            .to_str()
            .ok()
            .map(|t| std::borrow::Cow::Owned(t.to_string())),
        Value::Number(n) => Some(std::borrow::Cow::Owned(n.to_string())),
        Value::Integer(i) => Some(std::borrow::Cow::Owned(i.to_string())),
        _ => None,
    }
}

/// The client's own `Usage:` error, with the receiver's name interpolated the way it interpolates
/// it — `GetName()`, or `<unnamed>` when the widget has none (`0x84c7f0`).
fn usage(lua: &Lua, this: &Table, call: &str) -> mlua::Error {
    let name = this
        .get::<mlua::Function>("GetName")
        .and_then(|f| f.call::<Option<String>>(this.clone()))
        .ok()
        .flatten()
        .unwrap_or_else(|| "<unnamed>".to_string());
    let _ = lua;
    mlua::Error::runtime(format!("Usage: {name}:{call}"))
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;

    // ── Content: the model, and clearing ────────────────────────────────────────────────────
    //
    // `SetModel` and `PlayerModel:SetUnit` are the two ways a pane gets content and they are
    // alternatives, not layers: setting one clears the other, so `GetModel` after a `SetUnit`
    // cannot answer a stale path from three frames ago. Only the `SetModel` arm is a `Model` verb —
    // the unit arm is `PlayerModel`'s (`playermodel_install` below), which is why the paper-doll
    // and dress-up panes are `<PlayerModel>`s and every corpus `<Model>` drives the path arm.
    // **`SetModel(nil)` RAISES. It is not a clear** — `ClearModel` is the clear, and it is a
    // separate entry in the same table. This binding said the opposite until 2026-08-30 ("the
    // documented clear, and how the corpus writes 'no model'"), which was invented: the byte read
    // (`0x76d950`, shape A) gates the argument with `lua_isstring` and raises
    // `Usage: %s:SetModel("file")` on nil, absent, boolean, table, function, userdata and thread.
    // Measured before changing it — all 5 corpus call sites pass a string literal and our own
    // FrameXML never calls it — so nothing was relying on the leniency.
    //
    // **What is NOT reproduced, deliberately:** the reference's *second* raise,
    // `Invalid model file: %s` (`0x878b44`), for a path that does not resolve — its model load is
    // synchronous, so it knows. We render no FrameXML models at all, so we cannot evaluate that
    // condition, and a raise whose predicate we have to guess is worse than a named omission
    // (1134 §4). A path that would be invalid is simply stored.
    m.set(
        "SetModel",
        lua.create_function(|lua, (this, path): (Table, Value)| {
            let path = string_arg(&path)
                .ok_or_else(|| usage(lua, &this, "SetModel(\"file\")"))?
                .to_string();
            with_model(lua, &this, |m| {
                m.path = Some(path);
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
    // `SetFacing` is `Model`'s yaw setter (`0x878948[4]` -> `0x76dce0`, writing `+0x39c`).
    // `SetRotation` writes THE SAME FIELD but is `PlayerModel`'s, not a second name here — see
    // `playermodel_install`. A `<Model>` that wants its pane turned calls this one.
    m.set(
        "SetFacing",
        lua.create_function(|lua, (this, rad): (Table, Value)| {
            let rad = num(&rad);
            with_model(lua, &this, |m| m.facing = rad)
        })?,
    )?;
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

    // ── The two verbs that touch no pane state ──────────────────────────────────────────────

    // `AdvanceTime()` — `0x878948[14]` -> `0x76eca0`. **It does nothing, and that is the verified
    // behaviour, not a stub.** The function takes no Lua argument (no `lua_gettop`, no `tonumber`,
    // no `tostring` anywhere in `[0x76eca0, 0x76ed65)`), returns no value, and its one reachable
    // call is `0x76cfb0`, whose entire body is `mov eax,1; ret`. It writes no `CSimpleModel` field
    // and no global. So there is no clock to advance here and no omission to name: a receiver
    // check and zero returns IS the reference.
    //
    // Present because the shipped chain calls it three times — `Cooldown.lua`, and the glue
    // character screens' `<OnUpdateModel>` on `<ModelFFX>`, which inherits this very table — and
    // an addon that hooks one of those will call it too.
    m.set(
        "AdvanceTime",
        lua.create_function(|lua, this: Table| with_model(lua, &this, |_| ()))?,
    )?;

    // `ReplaceIconTexture(path)` — `0x878948[15]` -> `0x76ed70`. **A material swap, not a content
    // setter**: `0x76cfe0(0xe, path)` -> `0x710ec0` replaces the refcounted handle in
    // `[CM2Model+0xa4][i]` for every M2 texture whose `type == 14`, plus the same-typed slots on
    // the ribbon (`0x7b7950`) and particle (`0x7b4d20`) emitters. It sets no pane content, clears
    // no unit, and stores nothing on the `0x3dc`-byte widget — the override lives on the CM2Model
    // and dies with it when `SetModel`/`ClearModel` releases the instance.
    //
    // **Which is why storing nothing here is faithful rather than lazy.** The reference's two
    // "not ready" cases behave OPPOSITELY: with a `CM2Model` present but its data not resident the
    // call is queued on `[cm2+0x3c]` and replayed; with **no CM2Model at all**
    // (`[widget+0x318] == 0`) it is **dropped and never replayed**. This engine renders no
    // FrameXML models, so our panes are permanently the second case — dropping it is what the
    // client does in exactly our state.
    //
    // The argument gate is real and is shape A: `lua_isstring` (tags 3|4) then `lua_tostring`,
    // raising `Usage: %s:ReplaceIconTexture("texture")` on anything else. It is kept because it is
    // the whole observable behaviour left.
    m.set(
        "ReplaceIconTexture",
        lua.create_function(|lua, (this, path): (Table, Value)| {
            if string_arg(&path).is_none() {
                return Err(usage(lua, &this, "ReplaceIconTexture(\"texture\")"));
            }
            with_model(lua, &this, |_| ())
        })?,
    )?;

    lua.set_named_registry_value(REG_MODEL_METHODS, m)?;
    playermodel_install(lua)
}

/// `PlayerModel`'s **own three** verbs (table `0x84f1fc`) — nothing else. Everything a
/// `<PlayerModel>` else answers is `Model`'s, reached through the dispatcher's chain.
fn playermodel_install(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;

    // `SetUnit(unit)` — `0x84f1fc[0]` -> `0x505d70`. The pane's other content arm, and `SetModel`'s
    // alternative: each clears the other, so `GetModel` after a `SetUnit` cannot answer a stale
    // path. Ours stores the unit TOKEN and resolves it at render.
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

    // `RefreshUnit()` — `0x84f1fc[1]` -> `0x505e40`. Re-reads the unit the pane already shows; for
    // us a no-op with a live receiver check, because the pane holds the TOKEN and resolves it at
    // render, so there is no cached appearance here to invalidate. Present because the reference's
    // own `DressUpFrame`/`PaperDollFrame` call it (3 sites) and addons that hook them will too.
    m.set(
        "RefreshUnit",
        lua.create_function(|lua, this: Table| with_model(lua, &this, |_| ()))?,
    )?;

    // `SetRotation(rad)` — `0x84f1fc[2]` -> the Lua glue `0x505f00` -> the worker `0x505bb0`, whose
    // final instruction is `0x505c44 mov [esi+0x39c], eax`: **the same yaw field `SetFacing`
    // writes**, so the Lua-observable effect of the two verbs is identical and `GetFacing` reads
    // either back. That equality is the whole reason this can be one line.
    //
    // What `0x505bb0` does BESIDES the yaw write is deliberately not modeled, and is worth naming
    // because it is real: it picks a turn animation from the sign of the change (`0xc`
    // ShuffleRight when the current facing is **<** the argument, `0xb` ShuffleLeft when **>**,
    // `0` Stand on equality or NaN — the mapping wow-re CORRECTED on 2026-08-23 after publishing
    // it inverted), plays it unless that id is already armed on bone slot 0, and then
    // UNCONDITIONALLY sets `[+0x3e8] = 1` and `[+0x3ec] = now_ms + 100` — a 100 ms turn hold that
    // the per-paint `0x505c50` expires. Every one of those is invisible to Lua (no getter reads
    // them) and lands on a model renderer we have not built, so storing them here would be state
    // nobody writes and nobody reads. The addresses are the pin for the day the renderer exists.
    m.set(
        "SetRotation",
        lua.create_function(|lua, (this, rad): (Table, Value)| {
            let rad = num(&rad);
            with_model(lua, &this, |m| m.facing = rad)
        })?,
    )?;

    lua.set_named_registry_value(REG_PLAYERMODEL_METHODS, m)
}

impl crate::script::UiScript {
    /// A named model pane's scene state, or `None` if no live frame carries that name as a
    /// `Model`/`PlayerModel`.
    ///
    /// **The read side of the pane, for a host that draws it.** The app keeps one off-screen body
    /// bake per window (this module's header: the scene is ours, the pixels are the host's), and
    /// every frame it has to ask the pane the reference drives which way it is turned. Before
    /// decision 1751 that came from benilla-named globals a file of ours called
    /// (`BenillaPaperDollModel_SetFacing` and its four siblings, one scalar per window on
    /// [`Model`]); the stock FrameXML instead calls
    /// `Model_OnLoad`/`Model_RotateLeft`/`Model_RotateRight`/`Model_OnUpdate`, which write
    /// `PlayerModel:SetRotation` — i.e. this state, on the pane itself. So the host reads the pane.
    ///
    /// By NAME rather than by handle because that is what the host knows: the character sheet's
    /// bake belongs to `CharacterModelFrame`, and since 1751 that name is the reference's own. A
    /// linear scan, because a name index would be a second store to keep true and a loaded
    /// interface holds single-digit model panes.
    pub fn model_pane(&self, name: &str) -> Option<ModelState> {
        self.model_ref()
            .arena
            .iter_frames()
            .find_map(|(_, f)| match (&f.name, &f.kind_state) {
                (Some(n), KindState::Model(m)) if n == name => Some(m.clone()),
                _ => None,
            })
    }

    /// A named model pane's yaw in radians — [`Self::model_pane`]'s one hot field, and `0.0` for a
    /// pane that does not exist yet (its window's file not loaded, or its `OnLoad` not run).
    ///
    /// `0.0` rather than an `Option` because every caller is a per-frame booth mirror whose only
    /// other answer would be "keep the last one", and a pane that has not loaded has no last one.
    /// The reference's own default, 0.61, is authored in `Model_OnLoad`, so a real pane only reads
    /// 0.0 before it loads.
    pub fn model_pane_facing(&self, name: &str) -> f32 {
        self.model_pane(name).map_or(0.0, |m| m.facing)
    }
}
