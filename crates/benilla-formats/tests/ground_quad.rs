//! Pins the ground-plane-quad detector ([`RenderSubmesh::ground_quad`]) against real build-5875
//! assets — the shape the ground-fx decal lane (`benilla::ground_fx`) re-renders as projected
//! surface decals. Battle Shout's cast-base model is the canonical population member (verified by
//! hand off the raw M2, 2026-07-14: 24 vertices, all at z = 0 exactly, six 4-vert quads each
//! fully weighted to one of bones {1, 2, 3, 5, 6, 7}, rect −0.776..0.212 × ±0.494); a character
//! body model must detect NOTHING (its one incidentally-flat batch, if any, is not this shape at
//! the base anchor — and regressing a body part into a decal would be spectacular). Skips when
//! the gitignored client data isn't present.

use benilla_formats::{open_chain, parse_m2_render_submeshes};

#[test]
fn battle_shout_crescents_detect_as_ground_quads() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = open_chain(&data).expect("open chain");

    let bytes = chain
        .read_file("Spells\\BattleShout_Cast_Base.m2")
        .expect("Battle Shout cast-base model");
    let subs = parse_m2_render_submeshes(&bytes, "", &[]).expect("submeshes");
    assert_eq!(subs.len(), 6, "six crescent batches");
    let mut bones = Vec::new();
    for sub in &subs {
        let quad = sub
            .ground_quad()
            .expect("every crescent batch is a ground quad");
        bones.push(quad.bone);
        // The authored rect (raw M2 vertex table): x ∈ [−0.776, 0.212], y ∈ [−0.494, 0.494].
        assert!((quad.corners[0][0] + 0.776).abs() < 1e-3, "min x");
        assert!((quad.corners[3][0] - 0.212).abs() < 1e-3, "max x");
        assert!((quad.corners[0][1] + 0.494).abs() < 1e-3, "min y");
        assert!((quad.corners[3][1] - 0.494).abs() < 1e-3, "max y");
        assert!(quad.corners.iter().all(|c| c[2] == 0.0), "authored z = 0");
    }
    bones.sort_unstable();
    assert_eq!(bones, [1, 2, 3, 5, 6, 7], "one quad per slide bone");
}

#[test]
fn character_model_detects_no_ground_quads() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = open_chain(&data).expect("open chain");
    let bytes = chain
        .read_file("Character\\Human\\Male\\HumanMale.m2")
        .expect("human male model");
    let subs = parse_m2_render_submeshes(&bytes, "", &[]).expect("submeshes");
    assert!(
        subs.iter().all(|s| s.ground_quad().is_none()),
        "no body batch reads as a ground quad"
    );
}
