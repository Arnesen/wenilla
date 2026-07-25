//! `WOW_PROBE_LOOK` — the scripted **mouse-turn**, the one player action the probe harness could not
//! synthesize (decision 0621).
//!
//! `WOW_PROBE_KEY` presses keys, which is enough for the transition opcodes; but since decision 0617
//! the busiest thing on our wire by far is the facing stream a *mouse* turn produces (~one packet per
//! frame). A defect that only shows up under that density — and the runaway hunt looks like exactly
//! that — could not be reproduced headlessly at all: an agent had to ask the director to drive, every
//! iteration. That is the tooling gap this closes.
//!
//! Format: `WOW_PROBE_LOOK="<deg_per_sec>@<start_s>:<duration_s>[;…]"`, e.g. `"90@20:6"` — turn the
//! avatar's aim 90°/s for six seconds starting twenty seconds in. Negative rates turn the other way.
//! It writes [`Player::face_yaw`] directly rather than faking `AccumulatedMouseMotion` + a held right
//! button: the camera's look session gates on the OS cursor being inside the viewport
//! (`cursor_in_viewport`), which is unreliable for an unfocused probe window, and the camera plumbing
//! is not what is under test. From `face_yaw` onward this is the identical path a real mouse-turn
//! takes — the same value `stream_self_movement` diffs to decide on a `MSG_MOVE_SET_FACING`.

use bevy::prelude::*;

use super::Player;

/// One scripted turn: `rate` (rad/s), when it starts, and how long it runs (seconds, in-world clock).
struct Turn {
    rate: f32,
    at: f32,
    until: f32,
}

#[derive(Resource)]
pub(super) struct ProbeLook {
    turns: Vec<Turn>,
}

/// Parse the env script. Absent or unparseable entries yield no turns (with a warning), so the probe
/// is inert unless asked for.
pub(super) fn from_env() -> Option<ProbeLook> {
    let spec = std::env::var("WOW_PROBE_LOOK").ok()?;
    let turns: Vec<Turn> = spec
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            let (rate, rest) = s.split_once('@')?;
            let (at, dur) = rest.split_once(':')?;
            match (
                rate.trim().parse::<f32>(),
                at.trim().parse::<f32>(),
                dur.trim().parse::<f32>(),
            ) {
                (Ok(rate), Ok(at), Ok(dur)) => Some(Turn {
                    rate: rate.to_radians(),
                    at,
                    until: at + dur,
                }),
                _ => {
                    warn!("probe-look: unparseable turn {s:?} (want e.g. 90@20:6) — skipped");
                    None
                }
            }
        })
        .collect();
    (!turns.is_empty()).then_some(ProbeLook { turns })
}

/// Rotate the aim for every turn whose window covers this frame. Runs in `WorldStage::Input`
/// **before** the controller, so the frame that sees the new `face_yaw` is the frame that streams it.
pub(super) fn drive_probe_look(
    probe: Res<ProbeLook>,
    time: Res<Time>,
    mut player: ResMut<Player>,
    self_player: Query<(), With<crate::net::SelfPlayer>>,
) {
    if self_player.is_empty() {
        return; // in-world only, like the other probes
    }
    let now = time.elapsed_secs();
    let dt = time.delta_secs();
    let rate: f32 = probe
        .turns
        .iter()
        .filter(|t| now >= t.at && now < t.until)
        .map(|t| t.rate)
        .sum();
    if rate != 0.0 {
        player.face_yaw += rate * dt;
    }
}
