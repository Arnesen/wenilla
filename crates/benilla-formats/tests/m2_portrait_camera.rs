//! M2 portrait-camera parse — byte-level check against real character/creature models. Pins the
//! vanilla camera record stride (`0x7c`) + the `cameraLookup[0]` selection (wow-re
//! `system/ui/scratch/portrait-render.md` §4: the unit-frame portrait renders through exactly this
//! authored camera). Skips when the client isn't present.

use std::path::PathBuf;

use benilla_formats::{parse_m2_portrait_camera, Chain};

fn vanilla_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
}

#[test]
fn character_and_creature_portrait_cameras_parse_sane() {
    let data = vanilla_data_dir();
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let reader = Chain::open(&data).expect("open vanilla patch chain");
    for path in [
        "Character\\Human\\Male\\HumanMale.m2",
        "Creature\\Wolf\\Wolf.m2",
        "Creature\\Rabbit\\Rabbit.m2",
    ] {
        let bytes = reader.read(path).expect("read model");
        let cam = parse_m2_portrait_camera(&bytes)
            .unwrap_or_else(|| panic!("{path}: no portrait camera"));
        eprintln!("{path}: {cam:?}");
        // Structural sanity that a garbled stride/offset could not land on: a usable perspective
        // (fov in a plausible authored range, near < far), the camera off the target, and the rig
        // in front of the model (WoW models author facing +X; a portrait camera sits +X of its
        // subject looking back).
        assert!(
            cam.fov > 0.1 && cam.fov < 1.6,
            "{path}: fov {} outside plausible authored range",
            cam.fov
        );
        assert!(
            cam.near_clip > 0.0 && cam.near_clip < cam.far_clip,
            "{path}: bad clip planes {} .. {}",
            cam.near_clip,
            cam.far_clip
        );
        let dx = cam.position[0] - cam.target[0];
        assert!(
            dx > 0.1,
            "{path}: camera not in front of the model (Δx {dx})"
        );
    }
    // Numeric regression pin — HumanMale's authored rig (fov exactly π/4; eye head-height, in
    // front and off to the model's right — why the ref portrait faces viewer-left; target on the
    // head center). A wrong stride or a swapped base/track offset cannot land on all nine.
    let bytes = reader
        .read("Character\\Human\\Male\\HumanMale.m2")
        .expect("read HumanMale.m2");
    let cam = parse_m2_portrait_camera(&bytes).expect("HumanMale portrait camera");
    let close = |a: f32, b: f32| (a - b).abs() < 1e-3;
    assert!(
        close(cam.fov, std::f32::consts::FRAC_PI_4),
        "fov {}",
        cam.fov
    );
    for (got, want) in cam
        .position
        .iter()
        .zip([0.6335, -0.3879, 1.8867])
        .chain(cam.target.iter().zip([0.0627, 0.0343, 1.8636]))
    {
        assert!(close(*got, want), "eye/target drifted: {got} vs {want}");
    }
    assert!(close(cam.roll, 0.0), "roll {}", cam.roll);
}

#[test]
fn too_short_yields_no_camera() {
    // No MD20 camera array header → None, no panic.
    assert!(parse_m2_portrait_camera(&[0u8; 16]).is_none());
}
