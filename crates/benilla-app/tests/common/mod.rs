//! **The integration tests' interface loader — one copy, both stores** (decision 1751).
//!
//! The in-crate sibling of `ui_script::test_ui::load_ui`, which integration tests cannot reach:
//! they link this crate as a library, so its `#[cfg(test)]` items are not compiled for them. Each
//! `tests/*.rs` therefore grew its own reader off `assets/ui`, and every one of them broke the
//! first time a file it names became the reference's own.
//!
//! The rule is the manifest's, verbatim: **a bare filename is a file we ship, a path is the
//! reference's own off the player's installed chain.** Our shipped tree is flat, so a separator
//! decides, and nothing here needs to know which windows have migrated.
//!
//! The provider half matters as much as the loop. A sourced document pulls its Lua through its own
//! `<Script file="X.lua"/>`, which the loader resolves against the *including document's*
//! directory — `Interface\FrameXML\X.lua`, a chain path. A disk-only provider leaves every one of
//! those globals nil and the failures land nowhere near the cause.
//!
//! **A chain entry needs client data**, so a test that names one opens with
//! `benilla_formats::wow_data_or_skip!()`, like every other archive-backed test.

use benilla_ui::script::UiScript;

/// Load one manifest entry into `script`, panicking on any loader error.
///
/// A `.lua` entry is run as a chunk rather than parsed as a document — `GlobalStrings.lua` and
/// `LocaleProperties.lua` are entries of that shape in the real manifest too. Bytes, not text: a
/// chunk goes to Lua as it sits in the archive and only an XML parse decodes (1193).
pub fn load_ui(script: &UiScript, entry: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    let chain = |req: &str| -> Option<Vec<u8>> {
        let data = benilla_formats::wow_data()?;
        benilla_formats::open_chain(&data).ok()?.read(req).ok()
    };
    let read = |req: &str| -> Option<Vec<u8>> {
        if req.contains('\\') || req.contains('/') {
            return chain(req);
        }
        std::fs::read(dir.join(req)).ok()
    };

    let bytes = read(entry).unwrap_or_else(|| panic!("{entry}: not found"));
    if entry.to_ascii_lowercase().ends_with(".lua") {
        script
            .run_chunk_named(&bytes, &format!("@{entry}"))
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        return;
    }
    let doc = benilla_ui::framexml::parse(&benilla_ui::source::decode(&bytes))
        .unwrap_or_else(|e| panic!("{entry}: {e}"));
    let report = benilla_ui::loader::load_in(script, &doc, &entry.replace('\\', "/"), &read);
    assert!(
        report.errors.is_empty(),
        "{entry}: loader errors: {:#?}",
        report.errors
    );
}
