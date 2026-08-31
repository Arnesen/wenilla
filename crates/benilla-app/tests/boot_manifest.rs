//! Sanity for `web/boot-manifest.json` — the boot-prefetch read-set `web/boot.js` warms the
//! HTTP cache with (captured from a traced boot via `?boottrace=1`; regeneration procedure in
//! boot.js's header). The manifest is data, hand-carried between builds, so this is the guard
//! against the hand: shape, non-emptiness, no duplicates, and chain-style names. Staleness is
//! NOT tested — drift degrades gracefully to an uncached read, by design.

use std::path::Path;

fn manifest() -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/boot-manifest.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e} — capture one with ?boottrace=1", path.display()));
    serde_json::from_str(&text).expect("boot-manifest.json parses as JSON")
}

#[test]
fn the_boot_manifest_is_version_1_with_chain_names() {
    let m = manifest();
    assert_eq!(m["version"], 1, "unknown manifest version: {}", m["version"]);
    let names: Vec<&str> = m["names"]
        .as_array()
        .expect("names is an array")
        .iter()
        .map(|v| v.as_str().expect("every name is a string"))
        .collect();
    assert!(
        names.len() >= 50,
        "a real boot reads 100+ files; {} names smells like a truncated capture",
        names.len()
    );
    let mut seen = std::collections::HashSet::new();
    for name in &names {
        assert!(seen.insert(*name), "duplicate manifest entry: {name}");
        // Chain names are internally backslash-separated and never URL-encoded here — boot.js
        // runs encodeURIComponent at fetch time (the exact twin of web.rs::encode_name).
        assert!(!name.contains('/'), "forward slash in {name} — store raw chain names");
        assert!(!name.contains('%'), "pre-encoded name {name} — store raw chain names");
        assert!(!name.is_empty());
    }
    // The one file every boot certainly reads: if this is missing, the capture never reached
    // the catalog pile at all.
    assert!(
        names.iter().any(|n| n.eq_ignore_ascii_case("DBFilesClient\\Map.dbc")),
        "Map.dbc missing — was the trace captured from a real boot?"
    );
}
