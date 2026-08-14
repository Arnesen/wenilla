//! The **rigid bone spin** collector (`m2_bone_spins`) and its sampler — the mechanism that turns
//! Caverns of Time's asteroid belts without a skinning palette (decision 1264's deferred half).
//!
//! Two halves, deliberately separate: the sampler is pure arithmetic and is pinned on synthetic
//! keys, and the collector is pinned against the real shipped art, because what it has to get right
//! is not arithmetic — it is *which* bones qualify. Both shipped skyboxes are asserted, including
//! the one that must yield nothing: a predicate that accidentally admitted every bone would still
//! look correct on the model that has motion.

use benilla_formats::{load_m2_bone_spins, m2_bone_spins, BoneSpin, Chain};

/// `[x, y, z, w]` for a rotation of `deg` about +Z — the axis-agnostic half of the sampler tests.
fn about_z(deg: f32) -> [f32; 4] {
    let h = deg.to_radians() * 0.5;
    [0.0, 0.0, h.sin(), h.cos()]
}

/// The angle (degrees) of a quaternion about its axis, sign-normalised — what a test can compare
/// without caring which of `q`/`−q` the slerp produced.
fn angle_deg(q: [f32; 4]) -> f32 {
    let w = q[3].abs().clamp(0.0, 1.0);
    2.0 * w.acos().to_degrees()
}

fn spin(keys: Vec<(f32, [f32; 4])>, duration: f32, interp: bool) -> BoneSpin {
    BoneSpin {
        pivot: [0.0; 3],
        duration,
        interp,
        keys,
    }
}

/// The sampler's four legs: hold before the first key, slerp between a bracket, clamp past the
/// last, and wrap at the duration. The clamp is the one worth stating out loud — an M2 track's keys
/// live inside the sequence band and the client interpolates within it, so a loop whose last key
/// differs from its first *snaps* at the wrap. Inventing a wrap-around segment would be us
/// animating rather than the file.
#[test]
fn the_sampler_holds_slerps_clamps_and_wraps() {
    let s = spin(
        vec![(0.0, about_z(0.0)), (2.0, about_z(90.0))],
        4.0,
        /*interp=*/ true,
    );
    assert!(angle_deg(s.sample(0.0)) < 1e-3, "at the first key");
    assert!(
        (angle_deg(s.sample(-1.0)) - 90.0).abs() < 1e-3,
        "a negative cursor lands 1 s before the loop start, i.e. 3 s into the previous cycle — \
         `rem_euclid`, not `%`, which would mirror it onto the loop's opening pose instead"
    );
    assert!(
        (angle_deg(s.sample(1.0)) - 45.0).abs() < 1e-2,
        "halfway through the bracket: {}",
        angle_deg(s.sample(1.0))
    );
    assert!(
        (angle_deg(s.sample(3.0)) - 90.0).abs() < 1e-3,
        "past the last key: clamped, NOT interpolated back toward key 0"
    );
    assert!(
        (angle_deg(s.sample(5.0)) - 45.0).abs() < 1e-2,
        "one full period on from t=1: the loop wrapped"
    );
}

/// A step track (`interp_type == 0`) holds each key until the next. Our rig lane lerps these — a
/// recorded divergence (`benilla-extract bonescan`) — and there is no reason to reproduce it in new
/// code when the track's own header word is right there.
#[test]
fn a_step_track_holds_its_key() {
    let s = spin(
        vec![(0.0, about_z(0.0)), (2.0, about_z(90.0))],
        4.0,
        /*interp=*/ false,
    );
    assert!(angle_deg(s.sample(1.9)) < 1e-3, "still on key 0 at 1.9s");
    assert!((angle_deg(s.sample(2.0)) - 90.0).abs() < 1e-3);
}

/// The shortest-arc negation in the slerp. Without it a bracket whose keys dot negative takes the
/// LONG way round — which on a belt authored as a full turn reads as a section spinning backwards.
#[test]
fn the_slerp_takes_the_short_way_round() {
    // 350° and 0° are 10° apart, but their quaternions dot negative.
    let s = spin(
        vec![(0.0, about_z(350.0)), (1.0, about_z(360.0))],
        1.0,
        true,
    );
    let mid = angle_deg(s.sample(0.5));
    // 355° about +Z is 5° about −Z — either reading is ≤10° from both ends. The long way round
    // would put the midpoint ~175° away.
    assert!(
        !(15.0..=345.0).contains(&mid),
        "midpoint took the long arc: {mid}°"
    );
}

/// The real asset. `CavernsOfTimeSky.m2` authors exactly three spinning bones — the asteroid belts,
/// one 66.667 s loop, 25° / 90° / 360° of turn — and bone 0, which carries the other 17 batches,
/// must NOT be among them (it has no track at all, and a collector that admitted it would rotate the
/// entire painted sky).
#[test]
fn the_caverns_of_time_sky_spins_exactly_its_three_belt_bones() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = Chain::open(&data).expect("open vanilla patch chain");
    let spins = load_m2_bone_spins(&mut chain, "Environments\\Stars\\CavernsOfTimeSky.m2")
        .expect("read CavernsOfTimeSky.m2");

    let mut bones: Vec<u16> = spins.keys().copied().collect();
    bones.sort_unstable();
    assert_eq!(
        bones,
        vec![1, 2, 3],
        "the three asteroid-belt bones, and only those"
    );

    for (bone, turn) in [(1u16, 25.0f32), (2, 90.0), (3, 360.0)] {
        let s = &spins[&bone];
        assert!(
            (s.duration - 66.667).abs() < 0.01,
            "bone {bone}: one 66.667 s loop, got {}",
            s.duration
        );
        assert!(
            s.interp,
            "bone {bone}: the belts interpolate, they don't step"
        );
        assert!(
            angle_deg(s.keys[0].1) < 1e-2,
            "bone {bone}: the loop opens unrotated"
        );
        // The authored total turn, read at the last key. 360° comes back as 0° through `acos`
        // (the quaternion is back at identity), which is itself the thing to assert.
        let last = angle_deg(s.keys.last().expect("keyed").1);
        let expect = if turn >= 360.0 { 0.0 } else { turn };
        assert!(
            (last - expect).abs() < 0.5,
            "bone {bone}: expected {expect}° at the final key, got {last}°"
        );
        // The pivot is a few yards off the model origin — which is exactly why the placement has to
        // conjugate by it rather than rotate about the origin (the eye sits AT the origin).
        let d = (s.pivot[0].powi(2) + s.pivot[1].powi(2) + s.pivot[2].powi(2)).sqrt();
        assert!(
            (2.0..5.0).contains(&d),
            "bone {bone}: pivot {d:.2} yd from the origin — the conjugation matters"
        );
    }
}

/// The other shipped skybox has no animation at all, and the collector must say so. This is the
/// half that catches a predicate gone loose: `StratholmeSkybox` is three static opaque batches, and
/// anything it returns here is a bone the file never keys.
#[test]
fn the_stratholme_sky_spins_nothing() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = Chain::open(&data).expect("open vanilla patch chain");
    let bytes = chain
        .read_file("Environments\\Stars\\StratholmeSkybox.m2")
        .expect("read StratholmeSkybox.m2");
    assert!(
        m2_bone_spins(&bytes).is_empty(),
        "Stratholme's sky authors no bone animation"
    );
}
