//! The connection-telemetry feed — the app side of `benilla_ui::script::net_stats`' seam.
//!
//! One number: the latency `GetNetStats()` reports, which is the main bar's performance ("ping")
//! meter's whole input (`ActionBar.xml`'s `BenillaPerformanceBarFrame` polls it every 10 s and
//! colors the bar green/yellow/red). The measurement itself belongs to the net drain — every
//! `SMSG_PONG` lands a round trip in [`NetStatus::rtt_ring`], the reference's 16-deep RTT history —
//! and this feed only carries its average across the engine boundary (decision 0068 §3: the engine
//! never reaches into ECS or the socket).
//!
//! The push is unconditional rather than change-gated, for the reason [`crate::minimap`]'s
//! containment feed documents: `UiScript` is created when the UI loads, which is *after* a
//! connection can already have settled, so a feed that only spoke on change could miss its only
//! edge and leave the meter reading 0 forever. Three words of copy a frame is nothing next to that.

use bevy::prelude::*;

use benilla_ui::script::UiScript;

use crate::net::NetStatus;
use crate::ui_script::UiInput;

pub(crate) struct UiNetPlugin;

impl Plugin for UiNetPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, feed_net_stats.before(UiInput));
    }
}

/// Push the averaged round trip behind `GetNetStats()`.
fn feed_net_stats(script: Option<NonSendMut<UiScript>>, status: Res<NetStatus>) {
    let Some(mut script) = script else { return };
    script.set_latency_ms(status.avg_latency_ms());
}
