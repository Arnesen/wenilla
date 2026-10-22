//! wenilla carry: the browser build's log stack without spans.
//!
//! On wasm32 `bevy_log` puts `tracing_wasm::WASMLayer` on top of the subscriber, and that layer
//! calls `performance.mark` on every span enter and `performance.measure` on every exit, with a
//! `format!`ed name each time. Bevy and the client open several INFO spans every frame
//! (`present_frames`, the `gpu_preprocessing` buffer writes, `ui_script: tick`, …): measured on the
//! login screen at ~5,000 entries a second. That costs JS calls and string formatting per frame,
//! and the browser keeps every entry in its timeline for the life of the tab (1.3 M after five
//! minutes), so the tab's memory grows without bound.
//!
//! [`custom_layer`] installs the same [`LogRing`] as native, plus [`NoSpans`], a layer whose
//! callsite interest is `never` for spans. A plain (unfiltered) layer's `never` disables a
//! callsite for the whole subscriber, so `info_span!` becomes the macro's cached-interest check and
//! nothing reaches `WASMLayer`. Events are untouched: the console and the crash ring see the same
//! lines as before. Nothing in the browser consumes spans (no `trace`, chrome or tracy layer).

use bevy::app::App;
use bevy::log::tracing::subscriber::Interest;
use bevy::log::tracing::{Metadata, Subscriber};
use bevy::log::tracing_subscriber::layer::{Context, Layer};
use bevy::log::BoxedLayer;

use crate::log_ring::LogRing;

/// `LogPlugin::custom_layer` for the browser build: the crash ring, with spans switched off.
pub fn custom_layer(_: &mut App) -> Option<BoxedLayer> {
    Some(Box::new(LogRing.and_then(NoSpans)))
}

/// Disables every span callsite; passes events through.
pub struct NoSpans;

impl<S: Subscriber> Layer<S> for NoSpans {
    fn register_callsite(&self, meta: &'static Metadata<'static>) -> Interest {
        if meta.is_span() {
            Interest::never()
        } else {
            Interest::always()
        }
    }

    fn enabled(&self, meta: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        !meta.is_span()
    }
}
