//! WMO MOLT light parse — byte check against the Goldshire blacksmith's 3 warm forge lights. Pins the
//! chunk walk *past* zero-size chunks (`MOVV(0)`/`MOVB(0)` sit right before MOLT — a walk that stops
//! there finds nothing) and the SMOLight field offsets. Skips when the client isn't present.

use std::path::PathBuf;

use benilla_formats::{parse_wmo_lights, Chain};

fn vanilla_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
}

#[test]
fn goldshire_blacksmith_has_three_warm_omni_lights() {
    let data = vanilla_data_dir();
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let reader = Chain::open(&data).expect("open vanilla patch chain");
    let bytes = reader
        .read("World\\wmo\\Azeroth\\Buildings\\GoldshireBlacksmith\\GoldshireBlacksmith.wmo")
        .expect("read GoldshireBlacksmith.wmo");

    let lights = parse_wmo_lights(&bytes);
    // 3 MOLT lights (the forge). A chunk walk that breaks at MOVV(0)/MOVB(0) — which precede MOLT —
    // would find 0; this is the regression guard for that bug.
    assert_eq!(lights.len(), 3, "blacksmith should have 3 MOLT lights");
    for l in &lights {
        assert!(
            l.is_omni(),
            "forge MOLT lights are type 0 (omni), got {}",
            l.light_type
        );
        // The forge glow is warm orange — RGB(255,140,37) ≈ (1.0, 0.55, 0.145). Pins the BGRA decode
        // and the 0x30 stride (a garbled offset would not land on a warm colour).
        let c = l.color;
        assert!(
            c[0] > 0.9 && c[1] > 0.4 && c[1] < 0.7 && c[2] < 0.25,
            "forge light should be warm-orange, got {c:?}"
        );
    }
}

#[test]
fn no_molt_yields_empty() {
    assert!(parse_wmo_lights(&[0u8; 8]).is_empty());
}
