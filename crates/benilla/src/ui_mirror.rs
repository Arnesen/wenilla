//! The mirror-timer feed: breath / fatigue / feign-death off the wire → FrameXML events
//! (decision 0874).
//!
//! The net bridge queues [`MirrorTimerEdge`]s and the drain fires the reference client's
//! FrameScript events into the script VM — `MIRROR_TIMER_START` / `_PAUSE` / `_STOP`, the exact
//! contract `assets/ui/MirrorTimer.xml` (the transcribed 1.12 `MirrorTimer1/2/3`) registers for.
//! The bars themselves are the reference's: the frame stores the value and integrates
//! `value + scale * elapsed` every OnUpdate, so a packet every few seconds is enough to paint a
//! smooth countdown.
//!
//! **The client computes nothing here.** Breath and fatigue are server state — vmangos's
//! `Player::UpdateMirrorTimers` runs them off its own liquid checks (`IsUnderwater`,
//! `IsInHighSea`) and ships a value + a signed rate. That is why there is no local "am I
//! underwater" predicate in this module and should not be one: a second, disagreeing authority
//! is exactly how a bar ends up drifting from the drowning damage that follows it.
//!
//! The `ui_cast::CastBarFeed` pattern throughout — one queue, one drain, no per-frame work.

use benilla_ui::script::{ScriptValue, UiScript};
use bevy::prelude::*;

use benilla_protocol::messages::{MirrorTimerKind, MirrorTimerStart};

use crate::ui_script::UiInput;
use crate::ui_unit::UnitFeed;

/// One mirror-timer edge off the wire, queued by the net bridge for the bars.
#[derive(Debug, Clone, Copy)]
pub(crate) enum MirrorTimerEdge {
    /// `SMSG_START_MIRROR_TIMER` — start, or wholly re-state, one timer. The server re-sends this
    /// on every change (direction, remaining, frozen), so it arrives repeatedly for one bar.
    Start(MirrorTimerStart),
    /// `SMSG_PAUSE_MIRROR_TIMER` — freeze/unfreeze. vmangos never sends it (it substitutes a full
    /// `Start`), but a server that does must not be ignored.
    Pause { kind: u32, paused: bool },
    /// `SMSG_STOP_MIRROR_TIMER` — that timer is over; its bar hides.
    Stop { kind: u32 },
}

/// The net bridge's mirror-timer queue (the [`crate::ui_cast::CastBarFeed`] pattern).
#[derive(Resource, Default)]
pub(crate) struct MirrorTimerFeed(pub(crate) Vec<MirrorTimerEdge>);

/// The FrameScript name the reference passes as **arg1** for each timer type — the key its
/// `MirrorTimerColors` table is indexed by, and the stem of the caption lookup below.
///
/// The client holds these as a 3-entry table indexed by the wire's `timerType`
/// (`WoW.exe`: `"EXHAUSTION"` @`0x460520`, `"BREATH"` @`0x46052c`, `"FEIGNDEATH"` @`0x460534`,
/// contiguous and in the server's `MirrorTimer::Type` order). Note the type-0 name is
/// `EXHAUSTION`, not the server's own word for it (`FATIGUE`) — the two ends disagree on the
/// name of the same timer, and it is the *client's* word that the Lua is keyed by.
fn script_name(kind: MirrorTimerKind) -> &'static str {
    match kind {
        MirrorTimerKind::Fatigue => "EXHAUSTION",
        MirrorTimerKind::Breath => "BREATH",
        MirrorTimerKind::FeignDeath => "FEIGNDEATH",
    }
}

/// The bar's caption — **arg6** of `MIRROR_TIMER_START`.
///
/// The reference builds the global-string name by formatting `"%s_LABEL"` (`WoW.exe` @`0x460540`,
/// sitting immediately after the name table above) with the timer name and looking the result up
/// in the FrameScript globals. The 1.12 `GlobalStrings.lua` defines exactly two of the three —
/// `BREATH_LABEL = "Breath"` and `EXHAUSTION_LABEL = "Fatigue"`, each with the comment "Used as
/// the label for the … status bar" — and **no** `FEIGNDEATH_LABEL`, so that timer's caption comes
/// back empty in the reference too.
///
/// The strings are inlined here rather than looked up through the VM for the same reason
/// `CastingBar.xml` inlines `FAILED`/`INTERRUPTED`: benilla loads no `GlobalStrings.lua` yet. When
/// it does, this becomes the lookup the reference does.
fn caption(kind: MirrorTimerKind) -> &'static str {
    match kind {
        MirrorTimerKind::Fatigue => "Fatigue",
        MirrorTimerKind::Breath => "Breath",
        // No FEIGNDEATH_LABEL exists in the 1.12 GlobalStrings — the reference shows no caption.
        MirrorTimerKind::FeignDeath => "",
    }
}

/// Drain the queue into the script VM, one FrameScript event per edge.
///
/// A `kind` the client has no bar for is dropped: the server's own `NUM_CLIENT_TIMERS` gate means
/// vanilla never sends one (its fourth, `ENVIRONMENTAL`, drives lava damage with no bar), and the
/// reference would index past the end of a 3-entry table if one arrived.
fn feed_mirror_timers(script: Option<NonSendMut<UiScript>>, mut feed: ResMut<MirrorTimerFeed>) {
    let Some(mut script) = script else {
        // No VM (a capture/headless run): drop the edges rather than let them pile up unbounded.
        feed.0.clear();
        return;
    };
    for edge in feed.0.drain(..) {
        let raw = match edge {
            MirrorTimerEdge::Start(start) => start.kind,
            MirrorTimerEdge::Pause { kind, .. } | MirrorTimerEdge::Stop { kind } => kind,
        };
        let Some(kind) = MirrorTimerKind::from_wire(raw) else {
            continue;
        };
        let name = ScriptValue::Str(script_name(kind).into());
        let (event, args): (&str, Vec<ScriptValue>) = match edge {
            MirrorTimerEdge::Start(start) => (
                "MIRROR_TIMER_START",
                vec![
                    name,
                    ScriptValue::Int(i64::from(start.remaining_ms)),
                    ScriptValue::Int(i64::from(start.duration_ms)),
                    ScriptValue::Int(i64::from(start.scale)),
                    ScriptValue::Int(i64::from(start.paused)),
                    ScriptValue::Str(caption(kind).into()),
                ],
            ),
            MirrorTimerEdge::Pause { paused, .. } => (
                "MIRROR_TIMER_PAUSE",
                vec![name, ScriptValue::Int(i64::from(paused))],
            ),
            MirrorTimerEdge::Stop { .. } => ("MIRROR_TIMER_STOP", vec![name]),
        };
        script.fire_event(event, args);
    }
}

/// The mirror-timer UI seam: the queue + its drain, ordered like the cast bar's — before the VM
/// ticks, so an edge and its first OnUpdate land on the same frame.
pub(crate) struct UiMirrorPlugin;

impl Plugin for UiMirrorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MirrorTimerFeed>()
            .add_systems(Update, feed_mirror_timers.in_set(UnitFeed).before(UiInput));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client's arg1 names, in the server's `MirrorTimer::Type` order — and the fact that
    /// type 0 is `EXHAUSTION` on the client but `FATIGUE` on the server. Getting this wrong is
    /// silent: `MirrorTimerColors[timer]` would be nil and the bar's colour read would error.
    #[test]
    fn arg1_is_the_clients_name_not_the_servers() {
        assert_eq!(script_name(MirrorTimerKind::Fatigue), "EXHAUSTION");
        assert_eq!(script_name(MirrorTimerKind::Breath), "BREATH");
        assert_eq!(script_name(MirrorTimerKind::FeignDeath), "FEIGNDEATH");
    }

    /// The captions are the 1.12 `GlobalStrings.lua` values the `%s_LABEL` lookup resolves to —
    /// and the feign-death one is empty because that global does not exist.
    #[test]
    fn captions_are_the_globalstrings_values() {
        assert_eq!(caption(MirrorTimerKind::Fatigue), "Fatigue");
        assert_eq!(caption(MirrorTimerKind::Breath), "Breath");
        assert_eq!(caption(MirrorTimerKind::FeignDeath), "");
    }
}
