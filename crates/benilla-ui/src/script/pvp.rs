//! The PvP-flag **Era API surface** (decision 0646) — one global, no state.
//!
//! `TogglePVP()` is the whole outbound half of the PvP family: everything the UI *reads* is unit
//! state that already arrives on the [`super::unit`] snapshot (`UnitIsPVP`, `UnitIsPVPFreeForAll`,
//! `UnitFactionGroup`). The call queues a toggle the app drains
//! ([`super::UiScript::take_pvp_toggles`]) and turns into its `CMSG_TOGGLE_PVP` send, keeping the
//! engine free of ECS/net reach (decision 0068 §3) — [`super::duel`]'s shape, one verb smaller.
//!
//! The reference registers the binding at `0x48d700` and calls it from exactly one place in the
//! whole shipped 1.12 UI: `SlashCmdList["PVP"]` (ChatFrame.lua). benilla's popup row is a
//! deliberate second caller — decision 0646 §3.

use mlua::Lua;

use super::Model;

impl super::UiScript {
    /// Drain the PvP-flag toggles queued since the last call — each one is a `CMSG_TOGGLE_PVP`.
    /// A count rather than a payload: the packet is empty, so two toggles in a frame are two
    /// sends, not one collapsed intent.
    pub fn take_pvp_toggles(&mut self) -> u32 {
        std::mem::take(&mut self.model_mut().pvp_toggles)
    }

    /// Queue a toggle from the app side — the `/pvp` slash command. In the reference that slash
    /// handler *is* Lua (a one-liner over `TogglePVP`); benilla parses slash lines in Rust, so the
    /// same intent enters the same queue here rather than through the global ([`super::duel`]'s
    /// `queue_duel_request` reasoning, verbatim).
    pub fn queue_pvp_toggle(&mut self) {
        self.model_mut().pvp_toggles += 1;
    }
}

/// Register the PvP globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    // TogglePVP() — /pvp and the unit popup's PvP row. Takes no argument in 1.12: the *state*
    // form of the opcode (a one-byte body) has no binding, so there is nothing to pass.
    lua.globals().set(
        "TogglePVP",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.pvp_toggles += 1;
            Ok(())
        })?,
    )
}
