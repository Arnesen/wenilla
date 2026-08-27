//! Difftest **`RenderSubmesh::section`** against real art — the fact the consolidator gate in
//! `terrain_stream::spawn::assemble` rests on: two batches naming one M2 skin section draw the
//! SAME triangles, so they are coplanar and must never be split across vertex-transform lanes.
//!
//! `Ballista.m2` is the positive, and it is the model the defect was reported on: its bolt head
//! (section 7) and its shields (section 10) each carry a base batch **plus** an
//! `ARMORREFLECT3` shine batch — additive, render flag `0x10` (no depth write), env-mapped. Every
//! one of those three facts is a consolidator exclusion, so before the gate the base rode the
//! retained/merged lane while the shine stayed on the entity path, and the pair's depths agreed
//! only to a few ULPs: a per-pixel coin flip that re-rolled with the camera.
//!
//! `HumanTentMedium.m2` is the discriminator — a doodad whose every section is drawn once, which a
//! parse that answered "shared" everywhere would fail.
//!
//! Skips (passes) when the client isn't present at `<repo>/WoW/Data`.

use benilla_formats::{load_m2_mesh, open_chain, RenderSubmesh};

/// The union of the facts the consolidators exclude on: `static_gx::divert` refuses env-mapped and
/// depth-flagged batches, the merge lane refuses additive ones. A batch answering `true` can never
/// leave the entity path, whatever its siblings do.
fn refusable(s: &RenderSubmesh) -> bool {
    s.env_map || s.no_depth_write || s.no_depth_test || s.additive
}

#[test]
fn the_ballista_authors_a_coplanar_shine_over_its_bolt_head_and_shields() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = open_chain(&data).expect("open vanilla patch chain");
    let subs = load_m2_mesh(
        &mut chain,
        "World\\Azeroth\\Elwynn\\PassiveDoodads\\Ballista\\Ballista.m2",
    )
    .expect("load Ballista");

    // Every M2 batch names its section; the field exists to be compared, so `None` here would make
    // the gate a silent no-op on the whole M2 population.
    assert!(
        subs.iter().all(|s| s.section.is_some()),
        "every M2 batch carries its skin-section index"
    );

    // The two shared sections, and the split inside each: one plain base, one refusable shine.
    let mut shared = 0;
    for section in [7u16, 10] {
        let batches: Vec<&RenderSubmesh> =
            subs.iter().filter(|s| s.section == Some(section)).collect();
        assert_eq!(
            batches.len(),
            2,
            "Ballista section {section} is drawn by a base batch and a shine batch"
        );
        assert_eq!(
            batches.iter().filter(|s| refusable(s)).count(),
            1,
            "exactly one of section {section}'s two batches is a consolidator exclusion — that \
             asymmetry is the defect the assemble gate closes"
        );
        // The shine layer's own three facts, so a parse regression on any one of them red-bars
        // here rather than silently re-opening the split.
        let shine = batches.iter().find(|s| refusable(s)).unwrap();
        assert!(shine.env_map, "the shine batch generates its texcoords");
        assert!(shine.no_depth_write, "…carries render flag 0x10");
        assert!(shine.additive, "…and blends additively (blend mode 4)");
        shared += 1;
    }
    assert_eq!(shared, 2);
}

#[test]
fn a_plain_doodad_draws_every_section_once() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = open_chain(&data).expect("open vanilla patch chain");
    let subs = load_m2_mesh(
        &mut chain,
        "World\\Generic\\Buildings\\HumanTentMedium\\HumanTentMedium.m2",
    )
    .expect("load HumanTentMedium");

    let mut seen: Vec<Option<u16>> = subs.iter().map(|s| s.section).collect();
    let before = seen.len();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen.len(),
        before,
        "the tent draws each of its sections exactly once — nothing here is coplanar with itself"
    );
}
