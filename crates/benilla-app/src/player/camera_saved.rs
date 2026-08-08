//! The **camera pose** the client remembers (decision 1131) — orbit distance and pitch, per
//! character. The fourth resident of 0954's folder to be character-scoped, and the smallest: two
//! floats.
//!
//! The reference keeps exactly these two, in `WTF/Account/<ACC>/<REALM>/<CHAR>/camera-settings.txt`
//! — read out of a real 26-character tree (decision 1128's persistence-surface map):
//!
//! ```text
//! cameraDistance 16.068569
//! cameraPitch 13.449968
//! ```
//!
//! Two keys, LF, six decimals, trailing newline; **no yaw** and none of the `SaveView(2..5)` custom
//! views. benilla writes the same two keys in the same order to
//! `benilla/camera/<realm>-<character>.txt` ([`crate::local_state::camera_character_path`]) — a file
//! that stays readable beside its ancestor.
//!
//! **Character-scoped, deliberately.** It is where the reference puts it, and it is what the setting
//! means: a gnome and a tauren want different zooms, and a tank and a healer want different pitches.
//! (The minimap's zoom, the other half of 1131, is install-scoped for the same reason the reference
//! makes it a CVar: it is about the map widget, not the character in front of it.)
//!
//! **Written at the session edges** — `OnExit(InWorld)` (a `/logout`, a disconnect) and `AppExit`
//! (quit) — the same two edges the saved variables use ([`crate::ui_saved`]), and the same posture
//! the reference has for its caches: no autosave, no dirty bit. A camera pose is a thing you settle
//! into once and leave; there is nothing an intermediate write would preserve.
//!
//! **Read once**, when the roster names the character we are entering the world as — the macro/
//! binding load's own seam ([`crate::ui_macro::identity`]). Absent file = the shipped defaults, which
//! is the normal first run.

use std::path::PathBuf;

use bevy::prelude::*;

use crate::char_select::ClientState;

use super::camera::{
    CameraControl, FlyCam, WorldCamera, CAM_DIST_MAX, CAM_DIST_MIN, CAM_PITCH_LIMIT,
};

/// The persisted pose's file keys — the reference's own spellings, in the reference's own order.
const KEY_DISTANCE: &str = "cameraDistance";
const KEY_PITCH: &str = "cameraPitch";

/// Which character's file we are on, and where it lives. `identity` doubles as the once-per-
/// character latch (the macro/binding load's pattern), and it is what the *save* keys off — by the
/// time `OnExit(InWorld)` fires, the roster may already have moved on.
#[derive(Resource, Default)]
pub(super) struct CameraPoseFile {
    identity: Option<(String, String)>,
    path: Option<PathBuf>,
}

/// The persisted `cameraPitch` → the live [`FlyCam::pitch`].
///
/// The file's unit is **degrees**, and its sign is the camera's *elevation above the character*:
/// positive = the camera sits high and looks down, which is the ordinary over-the-shoulder view.
/// [`FlyCam::pitch`] is radians on the client's *internal* convention, where **negative = looking
/// down** (wow-re `follow-camera`, VERIFIED at the pitch integrate `0x510120` and the ±89.00° clamp
/// `0x8089d8`) — so the two are negatives of each other.
///
/// **Unit and sign are INFERRED** (decision 1131), from the reference's own 26-character `WTF`
/// tree: its saved pitches run ≈ −4.7 … +27.6, overwhelmingly positive, and the view those players
/// left behind is the normal one — slightly *above* the character, looking *down*. Under the
/// internal sign that distribution would mean two dozen players all left the camera pointing at the
/// sky. A wow-re cross-check of the file's reader/writer is dispatched; if it corrects this, these
/// two functions are the whole of the change.
fn pitch_from_file(degrees: f32) -> f32 {
    (-degrees.to_radians()).clamp(-CAM_PITCH_LIMIT, CAM_PITCH_LIMIT)
}

/// The live [`FlyCam::pitch`] → the persisted `cameraPitch` (see [`pitch_from_file`]).
fn pitch_to_file(radians: f32) -> f32 {
    -radians.to_degrees()
}

/// Render the pose exactly as the reference's writer does: two keys, one per line, `%f`'s six
/// decimals, LF, trailing newline.
fn render(distance: f32, pitch_radians: f32) -> String {
    format!(
        "{KEY_DISTANCE} {distance:.6}\n{KEY_PITCH} {:.6}\n",
        pitch_to_file(pitch_radians)
    )
}

/// Parse the two keys out of the file, each independently optional — a file with only one line
/// restores only that half, and an unknown key is skipped rather than failing the parse (a later
/// build's third key must not cost this build its zoom). Returns `(distance, pitch_radians)`, both
/// already clamped into the ranges the live camera enforces, so a hand edit lands somewhere legal.
fn parse(text: &str) -> (Option<f32>, Option<f32>) {
    let (mut distance, mut pitch) = (None, None);
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(key), Some(value)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(v) = value.parse::<f32>() else {
            warn!("camera pose: unparseable {key} '{value}' ignored");
            continue;
        };
        if !v.is_finite() {
            continue;
        }
        match key {
            KEY_DISTANCE => distance = Some(v.clamp(CAM_DIST_MIN, CAM_DIST_MAX)),
            KEY_PITCH => pitch = Some(pitch_from_file(v)),
            _ => {}
        }
    }
    (distance, pitch)
}

/// Restore the pose once the roster names the character — the same identity the macro and binding
/// loads key off, so all three land on the same character on the same frame it becomes knowable.
///
/// Sets the rig's three distances together (`distance`, `target_distance`, `collision_distance`):
/// seeding only the target would make every login open with a visible glide out from the default 15,
/// which is precisely the "we forgot" the file exists to end.
fn load_camera_pose(
    roster: Res<crate::char_select::Roster>,
    mut file: ResMut<CameraPoseFile>,
    mut rig: ResMut<CameraControl>,
    mut cam: Query<&mut FlyCam, With<WorldCamera>>,
) {
    let Some(id) = crate::ui_macro::identity(&roster) else {
        return;
    };
    if file.identity.as_ref() == Some(&id) {
        return; // already restored for this character
    }
    let Ok(mut cam) = cam.single_mut() else {
        return; // the camera entity is not up yet — try again next frame, identity unlatched
    };
    file.path = crate::local_state::camera_character_path(&id.0, &id.1);
    file.identity = Some(id);

    let Some(path) = file.path.clone() else {
        return; // hermetic capture, or no install — session-only, defaults stand
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        // Absent is the normal first-run case; anything else is worth a line.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            warn!("camera pose: cannot read {}: {e}", path.display());
            return;
        }
    };
    let (distance, pitch) = parse(&text);
    if let Some(d) = distance {
        rig.distance = d;
        rig.target_distance = d;
        rig.collision_distance = d;
    }
    if let Some(p) = pitch {
        cam.pitch = p;
    }
    info!("camera pose: restored from {}", path.display());
}

/// Write the pose from the live rig. Keyed off the path the *load* resolved, not off the roster:
/// on `OnExit(InWorld)` the roster is already unwinding, and the character whose pose this is is the
/// one we logged in as.
fn save(file: &CameraPoseFile, rig: &CameraControl, pitch: f32) {
    let Some(path) = &file.path else {
        return; // never loaded (glue-only run, hermetic capture) — nothing to write back
    };
    // `target_distance`, never `distance`: the live one is the collision-pulled arm, so quitting
    // with your back to a wall would otherwise save the wall's zoom instead of your own.
    let body = render(rig.target_distance, pitch);
    if let Err(e) = crate::local_state::write_atomic(path, &body) {
        warn!("camera pose: cannot write {}: {e}", path.display());
    }
}

/// `OnExit(InWorld)`: `/logout` back to the glue, or a disconnect.
fn save_on_session_end(
    file: Res<CameraPoseFile>,
    rig: Res<CameraControl>,
    cam: Query<&FlyCam, With<WorldCamera>>,
) {
    if let Ok(cam) = cam.single() {
        save(&file, &rig, cam.pitch);
    }
}

/// `AppExit`: quitting the client. Reads the message rather than a state edge because a quit from
/// in-world never leaves `InWorld` ([`crate::ui_saved`]'s same reason).
fn save_on_exit(
    file: Res<CameraPoseFile>,
    rig: Res<CameraControl>,
    cam: Query<&FlyCam, With<WorldCamera>>,
    mut exits: MessageReader<AppExit>,
) {
    if exits.read().next().is_none() {
        return;
    }
    if let Ok(cam) = cam.single() {
        save(&file, &rig, cam.pitch);
    }
}

pub(super) fn plugin(app: &mut App) {
    app.init_resource::<CameraPoseFile>()
        .add_systems(
            Update,
            // Before the controller reads the rig, so the restored pose is what the first in-world
            // frame renders rather than something the player watches glide into place. Not gated on
            // capture mode: a capture resolves no state path at all (0954's hermetic rule), so this
            // is already inert there.
            load_camera_pose
                .before(super::control)
                .run_if(in_state(ClientState::InWorld)),
        )
        .add_systems(OnExit(ClientState::InWorld), save_on_session_end)
        .add_systems(Update, save_on_exit);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file we write is byte-for-byte the reference's shape: its two keys, its order, its six
    /// decimals, LF, trailing newline — checked against a real `camera-settings.txt` from the RE
    /// tree (`Account/ONE/VMaNGOS/One`, `16.068569` / `13.449968`).
    #[test]
    fn the_rendered_file_matches_the_references_shape() {
        let text = render(16.068_57, pitch_from_file(13.449_968));
        assert_eq!(text, "cameraDistance 16.068569\ncameraPitch 13.449968\n");
    }

    /// Round trip: what we write parses back to what we had, through the sign flip and both clamps.
    #[test]
    fn the_pose_round_trips() {
        let pitch = pitch_from_file(24.2);
        assert!(pitch < 0.0, "a positive saved pitch looks DOWN internally");
        let (d, p) = parse(&render(17.509_666, pitch));
        assert_eq!(d, Some(17.509_666));
        assert!((p.unwrap() - pitch).abs() < 1e-4, "{p:?} vs {pitch}");
        // The reference's one negative sample survives too (camera slightly below, looking up).
        let (_, up) = parse("cameraPitch -4.749999\n");
        assert!(up.unwrap() > 0.0);
    }

    /// A hand-edited or truncated file never lands an illegal pose: each key is independent, the
    /// distance clamps to the zoom range, the pitch to ±89°, and junk is skipped rather than fatal.
    #[test]
    fn a_hand_edited_file_cannot_land_an_illegal_pose() {
        let (d, p) = parse("cameraDistance 999\ncameraPitch 400\n");
        assert_eq!(d, Some(CAM_DIST_MAX));
        assert_eq!(p, Some(-CAM_PITCH_LIMIT));
        let (d, p) = parse("cameraDistance -3\n");
        assert_eq!(
            (d, p),
            (Some(CAM_DIST_MIN), None),
            "one key restores one half"
        );
        let (d, p) = parse("cameraDistance banana\nfutureKey 1\n\ncameraPitch nan\n");
        assert_eq!((d, p), (None, None), "junk and unknown keys are skipped");
    }
}
