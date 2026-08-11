//! Global-sequence CONSTANT channel regression test (the floating-stowed-sword bug, 2026-07-03).
//!
//! Vanilla art authors the stowed-weapon attach-bone orientations as **global-sequence** tracks with a
//! single key at t=0 (HumanMale bones 29/30 hips, 58–62 back family — the blade-down / shield-on-back
//! quaternions), on global-seq 0, which is itself 0 ms long: a pure constant. The parser used to skip
//! every global-sequence track ("deferred"), leaving those joints at identity, so a stowed sword lay
//! horizontal through the body. The constant must now reach **every** sequence's keyframe set. Skips
//! when the client isn't present.

use benilla_formats::{open_chain, parse_m2_animations};

/// The hip-sheath attach bone (attachment id 32 → bone 29) and its authored constant rotation,
/// verified by direct dump of the real file (decision 0072's attach survey).
const HIP_BONE: u16 = 29;
const HIP_QUAT: [f32; 4] = [0.382, 0.063, -0.922, 0.0];

#[test]
fn stow_attach_bones_carry_their_constant_rotation_in_every_sequence() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = open_chain(&data).expect("open vanilla patch chain");
    let bytes = chain
        .read_file("Character\\Human\\Male\\HumanMale.m2")
        .expect("HumanMale.m2 in the chain");
    let anims = parse_m2_animations(&bytes);
    assert!(!anims.is_empty(), "HumanMale should have sequences");

    for anim in &anims {
        let hip = anim
            .bones
            .iter()
            .find(|bk| bk.bone == HIP_BONE)
            .unwrap_or_else(|| {
                panic!(
                    "anim {}: hip attach bone {HIP_BONE} missing from the keyframe set — the \
                     global-seq constant was dropped again",
                    anim.anim_id
                )
            });
        assert_eq!(
            hip.rotation.len(),
            1,
            "anim {}: expected the single constant rotation key",
            anim.anim_id
        );
        let (t, q) = &hip.rotation[0];
        assert_eq!(*t, 0.0);
        for (a, b) in q.iter().zip(HIP_QUAT) {
            assert!(
                (a - b).abs() < 1e-3,
                "anim {}: hip constant {q:?} != authored {HIP_QUAT:?}",
                anim.anim_id
            );
        }
    }
}
