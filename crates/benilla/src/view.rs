//! The faithful **view distance** (`farclip`) — one source of truth for everything that keys off how
//! far the detailed world is drawn. Promoted out of the debug panel (it was a `ModelDebug` field doing
//! double duty) so it's *config*, not a debug knob, and so subsystems read one value instead of
//! several uncoordinated ones.
//!
//! Read by: the hard far-clip **wall** (terrain/model/liquid/WDL shaders, pushed as `fog_params.w` by
//! `lighting::apply_wow_lighting`), the per-object **cull** (`debug_panel::apply_model_visibility`), and
//! — once `terrain_stream` is split — the tile
//! **stream radius** (which should derive its coverage from `farclip`). Live-editable via the debug
//! panel slider (the same A/B lever as before, now writing this resource).

use bevy::prelude::*;

/// View distance in yards. `farclip` = WoW's `farclip` CVar — the projection far plane for the detailed
/// world; geometry beyond it is clipped per-pixel (the wall) and the WDL horizon fills in beyond.
/// Default **777** (the vanilla max-view clamp `[177, 777]`; matches the reference `Config.wtf`).
#[derive(Resource, Clone, Copy)]
pub struct ViewDistance {
    pub farclip: f32,
}

impl Default for ViewDistance {
    fn default() -> Self {
        Self { farclip: 777.0 }
    }
}
