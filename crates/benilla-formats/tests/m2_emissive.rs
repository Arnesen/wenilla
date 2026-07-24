//! Difftest the M2 UNLIT (0x01) → `emissive` detection against the real GeneralLantern03: its glass
//! batch (ElwynnLantern01.blp, material flags 0x05) is self-lit; its body batch (flags 0x00) is not.
//! Skips (passes) when the client isn't present at `<repo>/WoW/Data`.

use std::path::PathBuf;

use benilla_formats::{load_m2_mesh, open_chain};

fn vanilla_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
}

#[test]
fn lantern_glass_batch_is_emissive() {
    let data = vanilla_data_dir();
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = open_chain(&data).expect("open vanilla patch chain");
    let subs = load_m2_mesh(
        &mut chain,
        "World\\Generic\\Human\\Passive Doodads\\Lanterns\\GeneralLantern03.m2",
    )
    .expect("load GeneralLantern03");

    assert!(
        subs.iter().any(|s| s.emissive),
        "the lantern has a self-lit (UNLIT 0x01) glass batch"
    );
    assert!(
        subs.iter().any(|s| !s.emissive),
        "the lantern body batch is lit normally (not emissive)"
    );
}
