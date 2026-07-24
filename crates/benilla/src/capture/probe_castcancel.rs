//! The cast-cancel live probe (`WOW_PROBE=castcancel`) — the end-to-end instrument for the cast
//! bar's LOCAL self-cancel (decision 0256 open item 2), inert without the env: once in-world, use
//! the Hearthstone (a 10 s cast that never arms the send guard, so it exercises the
//! started-`Casting` half of the cancel's inflight union), then 2 s into the bar inject a real
//! `W` press into `ButtonInput<KeyCode>` — the ACTUAL controller path, so the probe drives the
//! same move-start edge a player's key does (controller → `LocalMoveStart` →
//! `ui_cast::local_self_cancel` → `CMSG_CANCEL_CAST`).
//!
//! The verdict is read off the run log with `WOW_CAST_TRACE=1`: the `LOCAL self-cancel` line
//! must land beside the `SEND move StartForward` line — frames, not a server round trip, after
//! it — and the server's echo (`RECV CAST_RESULT failure`) must follow without repainting
//! anything (the reap already emptied the `Casting` key it tests). The probe also logs every
//! cast-bar phase transition ([`bar_timeline`]) so the red bar's hold/burst/fade durations are
//! measured off the timestamps, not eyeballed. Non-combat, safe unattended
//! (method.md's rule bans unattended *combat* probes); a hearth that completes because the
//! cancel FAILED just ports the probe character home — visible in the log as the missing cancel line. Pair with
//! the slot-keyed probe identity (`WOW_USER=probeN WOW_PASS=pprobeN WOW_CHAR=Probe<N-spelled>`,
//! method.md) + `WOW_CAST_TRACE=1 WOW_PROBE_EXIT_AT=<s>`.

use bevy::prelude::*;

use crate::net::SelfPlayer;

pub(crate) struct ProbeCastCancelPlugin;

impl Plugin for ProbeCastCancelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ProbeCastCancel>()
            .add_systems(Update, (cast_cancel_probe, bar_timeline));
    }
}

/// The bar's phase, read out of the live VM (the transcription's own state fields).
const BAR_PHASE_CHUNK: &str = "return (function()\n\
    local f = CastingBarFrame\n\
    if not f then return \"noui\" end\n\
    if not f:IsVisible() then return \"hidden\" end\n\
    local txt = CastingBarText:GetText() or \"?\"\n\
    if f.casting then return \"casting|\" .. txt end\n\
    if f.channeling then return \"channel|\" .. txt end\n\
    if GetTime() < (f.holdTime or 0) then return \"hold|\" .. txt end\n\
    if f.flash then return \"burst|\" .. txt end\n\
    if f.fadeOut then return \"fade|\" .. txt end\n\
    return \"shown|\" .. txt\n\
 end)()";

/// Log every cast-bar phase transition with a timestamp — the measured timeline of the
/// transcription's hold/burst/fade (decision 0454's pin: `holdTime` 1 s wall-clock, then
/// ~5 flash + ~20 fade ticks normalized to `CASTING_BAR_REF_TICK` 30 Hz ≈ 0.83 s). The deltas
/// between the logged `hold` → `burst` → `fade` → `hidden` lines ARE the verdict; no eyeballing.
fn bar_timeline(
    time: Res<Time>,
    script: Option<NonSend<benilla_ui::script::UiScript>>,
    self_player: Query<(), With<SelfPlayer>>,
    mut last: Local<String>,
) {
    if self_player.is_empty() {
        return;
    }
    let Some(script) = script else {
        return;
    };
    let Ok(state) = script.eval::<String>(BAR_PHASE_CHUNK) else {
        return;
    };
    if state != *last {
        info!(
            "castcancel probe: bar {:.3}s {}",
            time.elapsed_secs(),
            state
        );
        *last = state;
    }
}

/// Probe state: when we entered the world, and the next phase to run.
#[derive(Resource, Default)]
struct ProbeCastCancel {
    entered: Option<f32>,
    phase: u8,
    used_at: Option<f32>,
}

/// Scan the backpack for the Hearthstone by item link (a restock can't silently move the
/// probe's target) and use it. `false` = not resolved yet — a fresh character's first login
/// still has the item-query round trip in flight, so the caller retries per frame.
const USE_HEARTH_CHUNK: &str = "return (function()\n\
    for s = 1, C_Container.GetContainerNumSlots(0) do\n\
      local link = C_Container.GetContainerItemLink(0, s)\n\
      if link and string.find(link, \"item:6948\", 1, true) then\n\
        C_Container.UseContainerItem(0, s)\n\
        return true\n\
      end\n\
    end\n\
    return false\n\
 end)()";

/// The timeline: from **3.0 s** after world-enter, scan-until-resolved for the Hearthstone and
/// use it (deadline 10 s — the link needs the server's item-query echo on a cold cache); **2.0 s
/// into the 10 s cast** press `W`; **+0.3 s** release it. Every phase logs, so a stalled run
/// shows where it stopped.
fn cast_cancel_probe(
    time: Res<Time>,
    mut probe: ResMut<ProbeCastCancel>,
    self_player: Query<(), With<SelfPlayer>>,
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
) {
    if self_player.is_empty() {
        return;
    }
    let entered = *probe.entered.get_or_insert(time.elapsed_secs());
    let t = time.elapsed_secs() - entered;
    match probe.phase {
        0 if t >= 3.0 => {
            let Some(script) = script else {
                probe.phase = 99;
                error!("castcancel probe: no UI VM — cannot use the Hearthstone");
                return;
            };
            match script.eval::<bool>(USE_HEARTH_CHUNK) {
                Ok(true) => {
                    probe.phase = 1;
                    probe.used_at = Some(t);
                    info!("castcancel probe: using the Hearthstone (10 s cast opens the bar)");
                }
                Ok(false) if t >= 10.0 => {
                    probe.phase = 99;
                    error!("castcancel probe: no Hearthstone link resolved by t+10 s");
                }
                Ok(false) => {} // still resolving — retry next frame
                Err(e) => {
                    probe.phase = 99;
                    error!("castcancel probe: {e}");
                }
            }
        }
        1 if t >= probe.used_at.unwrap_or(f32::MAX) + 2.0 => {
            probe.phase = 2;
            info!("castcancel probe: pressing W mid-cast — the local cancel should fire NOW");
            keys.press(KeyCode::KeyW);
        }
        2 if t >= probe.used_at.unwrap_or(f32::MAX) + 2.3 => {
            probe.phase = 3;
            info!("castcancel probe: releasing W — read the cast-trace for the verdict");
            keys.release(KeyCode::KeyW);
        }
        _ => {}
    }
}
