//! Whole-directory guards over the shipped `assets/ui/*.xml` — the coverage the per-window test
//! modules structurally cannot give, since each of those loads only the file it is about.

/// EVERY shipped `assets/ui/*.xml` parses — not just the ones a window test happens to load.
///
/// The per-window tests each load their own file, which left real holes: `CraftFrame.xml` and
/// `TradeSkillFrame.xml` ship in [`super::load_default_ui`]'s manifest and had NO parse coverage at
/// all, because the manifest is an inline array no test walks. A malformed comment in either (XML
/// forbids `--` inside `<!-- -->`, which a prose edit hits easily) would have reached a real run
/// untouched by a green suite.
///
/// Deliberately parse-only, not `loader::load`: loading one file out of manifest order reports
/// legitimate errors for templates its predecessors define, so a load-sweep would have to duplicate
/// the load order to say anything. Parsing is order-free and catches the whole well-formedness
/// class on its own.
#[test]
fn every_shipped_ui_xml_parses() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("assets/ui").flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "xml") {
            let text = std::fs::read_to_string(&path).expect("read");
            if let Err(e) = benilla_ui::framexml::parse(&text) {
                panic!("{}: {e}", path.display());
            }
            checked += 1;
        }
    }
    // Never let the sweep pass by finding nothing — a moved assets dir would otherwise turn this
    // into a test that guards zero files while staying green.
    assert!(
        checked >= 40,
        "only {checked} xml files swept — sweep broke"
    );
}
