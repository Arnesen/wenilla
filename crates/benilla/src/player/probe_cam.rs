//! `WOW_PROBE_CAM` — the scripted **camera park**: hold the third-person rig at an absolute yaw,
//! pitch and zoom so an unattended probe can *look at* a named thing (decision 0653).
//!
//! [`super::probe_look`] turns the avatar's **aim**; that is the right lever for the facing stream,
//! and the wrong one for framing a subject — it never touches `FlyCam::pitch`, which world entry
//! pins at `-0.45` rad (26° **down**). Everything above the horizon was therefore unreachable
//! headlessly: a `.go` to a pinned coordinate lands with the camera staring at the dirt, so the
//! tower / crystal / rock the report is *about* is off the top of the frame. That is why the first
//! C4 flicker burst came back clean — the subject was never in it.
//!
//! Format: `WOW_PROBE_CAM="<yaw_deg>,<pitch_deg>[,<dist_yd>]@<start_s>[:<pan_deg_per_s>][;…]"`,
//! e.g. `"180,12,25@25"` — from 25 s on, face south-ish, 12° **up**, 25 yd back. Yaw and pitch are
//! absolute (world yaw as the rig spells it, `+pitch` = up); the distance is optional and left alone
//! when omitted. Entries are held **every frame** once armed, not set once: a teleport, a settle, or
//! a wire-in reset would otherwise knock the pose off between the arm and the shot. The last armed
//! entry wins, so a `;` list is a sequence of poses.
//!
//! The optional **pan** turns the parked yaw at a constant rate from that pose's own start
//! (`"140,16,22@20:8"` — park, then sweep 8°/s). It exists because a parked camera is blind to a
//! whole class of defect: the director's read of the Far Watch Post tower is that it "mostly only
//! flickers while moving the cam, not while still" — z-fighting's signature, and precisely the
//! condition decision 0653's still burst cannot see (decision 0656). Pair it with `benilla-visual
//! flicker`'s **toggle map**, the reading that survives a moving view.
//!
//! It writes the rig fields directly for the same reason `probe_look` writes `face_yaw` directly —
//! faking mouse motion needs the OS cursor inside the viewport, which an unfocused probe window
//! cannot promise, and the camera *plumbing* is not what is under test. From these fields onward
//! this is the identical path a mouse-drag takes.

use bevy::prelude::*;

use super::camera::{CameraControl, FlyCam};

/// One parked pose: absolute yaw/pitch (radians), an optional orbit distance (yards), when it takes
/// over, and an optional constant yaw pan (rad/s) applied from that moment.
struct Pose {
    yaw: f32,
    pitch: f32,
    distance: Option<f32>,
    at: f32,
    pan: f32,
}

#[derive(Resource)]
pub(super) struct ProbeCam {
    poses: Vec<Pose>,
}

/// Parse the env script. Absent or unparseable entries yield no poses (with a warning), so the probe
/// is inert unless asked for.
pub(super) fn from_env() -> Option<ProbeCam> {
    let spec = std::env::var("WOW_PROBE_CAM").ok()?;
    let poses: Vec<Pose> = spec
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            let (pose, at) = s.split_once('@')?;
            let mut parts = pose.split(',').map(str::trim);
            let yaw = parts.next()?.parse::<f32>().ok()?;
            let pitch = parts.next()?.parse::<f32>().ok()?;
            // A third field is the orbit distance; anything past it is a typo worth refusing.
            let distance = match (parts.next(), parts.next()) {
                (None, _) => Some(None),
                (Some(d), None) => d.parse::<f32>().ok().map(Some),
                (Some(_), Some(_)) => None,
            }?;
            // `@<start>` or `@<start>:<pan>` — the same `at:rate` shape `WOW_PROBE_LOOK` uses.
            let (at, pan) = match at.trim().split_once(':') {
                Some((at, pan)) => (at.trim(), pan.trim().parse::<f32>().ok()?),
                None => (at.trim(), 0.0),
            };
            Some(Pose {
                yaw: yaw.to_radians(),
                pitch: pitch.to_radians(),
                distance,
                at: at.parse::<f32>().ok()?,
                pan: pan.to_radians(),
            })
        })
        .collect();
    if poses.is_empty() {
        warn!("probe-cam: no usable pose in {spec:?} (want e.g. 180,12,25@25) — inert");
    }
    (!poses.is_empty()).then_some(ProbeCam { poses })
}

/// Hold the camera at the latest armed pose. Runs in `WorldStage::Input` **before** `control`, so
/// the frame that sees the pose is the frame that renders it.
pub(super) fn drive_probe_cam(
    probe: Res<ProbeCam>,
    time: Res<Time>,
    mut rig: ResMut<CameraControl>,
    mut cam: Query<&mut FlyCam, With<super::camera::WorldCamera>>,
    self_player: Query<(), With<crate::net::SelfPlayer>>,
) {
    if self_player.is_empty() {
        return; // in-world only, like the other probes
    }
    let now = time.elapsed_secs();
    // Latest armed wins, so a `;` list reads as a timeline.
    let Some(pose) = probe.poses.iter().rfind(|p| now >= p.at) else {
        return;
    };
    let Ok(mut cam) = cam.single_mut() else {
        return;
    };
    // Absolute, from the pose's own start — so the sweep is reproducible frame-for-frame across
    // runs rather than accumulating whatever frame times this run happened to get.
    cam.yaw = pose.yaw + pose.pan * (now - pose.at);
    cam.pitch = pose.pitch;
    if let Some(d) = pose.distance {
        // Both, or the wheel glide eases `distance` back toward the old target every frame and the
        // parked shot drifts through the whole zoom while the burst is running.
        rig.distance = d;
        rig.target_distance = d;
    }
}
