//! Asset-gated fixture: the ItemDisplayInfo **icon** column against the real 5875 data — pins
//! col 5 (see `src/items.rs`) to display ids the local vmangos DB actually pairs with known
//! items, so a schema drift or column slip fails loudly. The model/geoset columns have their own
//! synthetic tests in the adapter. Skips (passes) without `<repo>/WoW/Data`.

use std::path::PathBuf;

use benilla_formats::{load_item_display_catalog, open_chain};

fn vanilla_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
}

#[test]
fn item_icons_resolve_known_display_ids() {
    let data = vanilla_data_dir();
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = open_chain(&data).expect("open vanilla patch chain");
    let catalog = load_item_display_catalog(&mut chain).expect("load item display catalog");
    assert!(
        catalog.len() > 29_000,
        "5875 ships 29604 display rows, got {}",
        catalog.len()
    );

    // The icon-column derivation anchors, now as regressions — display ids straight from the
    // server's item_template (the values the live wire answers for these items).
    for (item, display_id, icon) in [
        ("Worn Shortsword", 1542, "Interface\\Icons\\INV_Sword_04"),
        ("Tough Jerky", 2473, "Interface\\Icons\\INV_Misc_Food_16"),
        (
            "Worn Wooden Shield",
            18730,
            "Interface\\Icons\\INV_Shield_09",
        ),
        ("Hearthstone", 6418, "Interface\\Icons\\INV_Misc_Rune_01"),
    ] {
        assert_eq!(
            catalog.get(display_id).and_then(|d| d.icon.as_deref()),
            Some(icon),
            "{item} ({display_id})"
        );
    }
}
