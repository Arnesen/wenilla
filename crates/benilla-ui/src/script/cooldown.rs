//! The `Cooldown` widget's method surface (decision 0137 phase 4) — the Era API's
//! `Cooldown:SetCooldown(start, duration)` over the engine-modeled sweep/flash machine
//! ([`crate::widget::CooldownState`]).
//!
//! The 1.12 reference has no such method: its cooldown is a `Model` playing
//! `UI-Cooldown-Indicator.mdx`, driven by `Cooldown.lua`'s authored machine
//! (`CooldownFrame_SetTimer` → `SetSequence(0)` + `Show`; `OnUpdateModel` scrubs sequence 0 to
//! `(GetTime()-start)/duration`, flips to sequence 1 realtime at 1.0, `OnAnimFinished` → `Hide`).
//! benilla keeps `CooldownFrame_SetTimer`'s show/hide gate ref-verbatim in the authored XML and
//! models the sequence machine engine-side: `SetCooldown` stores the pair, the extraction derives
//! the sweep fraction / flash phase from the engine clock, and `tick` hides a finished widget
//! (the `OnAnimFinished` edge). The pixels — the byte-pinned 60 %-black pie and the `star4`
//! flash — are the app renderer's ([`super::types::QuadContent::Cooldown`]).
//!
//! The methods live in their own registry table, consulted by the frame `__index` dispatcher only
//! for Cooldown frames — the same duck-typing posture as StatusBar's.

use mlua::{Lua, Table};

use super::object::frame_handle_of;
use super::Model;
use crate::widget::{CooldownState, KindState};

/// Registry key of the Cooldown method table (the MAXCSTACK discipline: Lua-side root, named key).
pub(super) const REG_COOLDOWN_METHODS: &str = "__benilla_cooldown_methods";

/// Run `f` over a frame's Cooldown state under one short write borrow.
fn with_cooldown<T>(
    lua: &Lua,
    this: &Table,
    f: impl FnOnce(&mut CooldownState) -> T,
) -> mlua::Result<T> {
    let h = frame_handle_of(lua, this)?;
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let frame = model
        .arena
        .frame_mut(h)
        .ok_or_else(|| mlua::Error::runtime("stale frame handle"))?;
    match &mut frame.kind_state {
        KindState::Cooldown(c) => Ok(f(c)),
        _ => Err(mlua::Error::runtime("not a Cooldown")),
    }
}

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let m = lua.create_table()?;

    m.set(
        "SetCooldown",
        // `SetCooldown(start, duration)` — GetTime-clock seconds, the pair
        // `CooldownFrame_SetTimer` stores (`this.start`/`this.duration`). Show/hide stays the
        // authored helper's job (ref-verbatim), so the method only (re)arms the timer.
        lua.create_function(|lua, (this, start, duration): (Table, f64, f64)| {
            with_cooldown(lua, &this, |c| {
                c.start = start;
                c.duration = duration;
            })
        })?,
    )?;
    m.set(
        "GetCooldownTimes",
        // The Era read-back (milliseconds, like the live API) — no reference consumer; kept for
        // parity with SetCooldown so a transcription can round-trip.
        lua.create_function(|lua, this: Table| {
            with_cooldown(lua, &this, |c| (c.start * 1000.0, c.duration * 1000.0))
        })?,
    )?;

    lua.set_named_registry_value(REG_COOLDOWN_METHODS, m)?;
    Ok(())
}
