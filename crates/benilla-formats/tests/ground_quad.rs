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
    let data = benilla_formats::wow_data_or_skip!();
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

/// The HOVER population (the `groundscan` hover census, 2026-07-31): Consecration authors its
/// 23-yard burn disc at z = 0.097 and its center glow at 0.207 — flat, uniform-z, just above the
/// ground plane to dodge terrain z-fighting. Both must detect (the widened ceiling,
/// `GROUND_HOVER_MAX`), with the authored hover preserved on the corners; and Flamestrike's model
/// must decal exactly its two flat discs while its 3-D flame column, ribbons, and billboard glow
/// stay off the lane.
#[test]
fn hovering_discs_detect_as_ground_quads() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = open_chain(&data).expect("open chain");

    let bytes = chain
        .read_file("spells\\consecration_impact_base.m2")
        .expect("Consecration impact-base model");
    let subs = parse_m2_render_submeshes(&bytes, "", &[]).expect("submeshes");
    assert_eq!(subs.len(), 2, "glow + burn disc");
    let mut hovers: Vec<f32> = subs
        .iter()
        .map(|s| {
            let quad = s.ground_quad().expect("both Consecration discs detect");
            let z = quad.corners[0][2];
            assert!(
                quad.corners.iter().all(|c| c[2] == z),
                "uniform authored plane"
            );
            z
        })
        .collect();
    hovers.sort_by(f32::total_cmp);
    assert!((hovers[0] - 0.097).abs() < 1e-3, "burn disc hover");
    assert!((hovers[1] - 0.207).abs() < 1e-3, "center glow hover");

    let bytes = chain
        .read_file("Spells\\Flamestrike_Impact_Base.m2")
        .expect("Flamestrike impact-base model");
    let subs = parse_m2_render_submeshes(&bytes, "", &[]).expect("submeshes");
    let quads = subs.iter().filter(|s| s.ground_quad().is_some()).count();
    assert_eq!(
        quads, 2,
        "exactly the burn disc + center glow — flames/ribbons/billboard stay meshes"
    );
}

#[test]
fn character_model_detects_no_ground_quads() {
    let data = benilla_formats::wow_data_or_skip!();
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

/// The quad's **static M2Color tint** ([`benilla_formats::GroundQuad::tint`]) — the constant the
/// mesh path draws through its vertex-colour bake, which a decal consumer has no vertex buffer to
/// carry. The Flare's ground wash is the case that named it: two 13.89-yd quads on the NEUTRAL
/// `GENERICGLOW*` radials, whose entire colour is the constant `(0.992, 0.467, 0.0)` — drawn
/// untinted, an additive pool of them blows white instead of laying down dim orange. Battle
/// Shout's crescents are the other side of the same gate: their colour VARIES (white→red over the
/// clip), so it rides `rgb_anim`, the vertex bake is cleared, and this must read white — that is
/// what keeps the two from double-applying.
#[test]
fn ground_quads_carry_their_static_m2color_tint() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = open_chain(&data).expect("open chain");

    let bytes = chain
        .read_file("SPELLS\\Flare_State_Base.m2")
        .expect("Flare state-base model");
    let subs = parse_m2_render_submeshes(&bytes, "", &[]).expect("submeshes");
    let quads: Vec<_> = subs.iter().filter_map(|s| s.ground_quad()).collect();
    assert_eq!(quads.len(), 2, "both washes are ground quads");
    for q in &quads {
        assert!((q.tint[0] - 0.992).abs() < 1e-3, "warm red: {:?}", q.tint);
        assert!((q.tint[1] - 0.467).abs() < 1e-3, "warm green: {:?}", q.tint);
        assert!(q.tint[2] < 1e-3, "no blue at all: {:?}", q.tint);
        // 13.89 yd across — the wash the tint has to colour.
        let span = q.corners[3][0] - q.corners[0][0];
        assert!((span - 13.89).abs() < 0.02, "wash span {span}");
    }

    // The animated twin: colour varies, so it rides `rgb_anim` and the vertex bake is cleared.
    let bytes = chain
        .read_file("Spells\\BattleShout_Cast_Base.m2")
        .expect("Battle Shout cast-base model");
    let subs = parse_m2_render_submeshes(&bytes, "", &[]).expect("submeshes");
    for sub in &subs {
        let q = sub.ground_quad().expect("crescent");
        assert!(sub.rgb_anim.is_some(), "the crescent's colour is a loop");
        assert_eq!(q.tint, [1.0; 3], "…so the static tint stays white");
    }
}
