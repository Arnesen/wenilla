//! The app side of the Era atlas seam (decision 0950): read the extractor's manifest once at
//! boot and turn it into the [`UiScript`](benilla_ui::script::UiScript) atlas table, whose
//! entries carry `era:`-scheme texture paths that [`super::WorldAssets::sprite_texture`]
//! resolves from disk. Runtime never touches CASC or DB2 — `scripts/era-extract.py` baked the
//! `UiTextureAtlas` indirection offline into `WoW-era/_extracted_ui/{manifest.json,
//! textures/<fdid>.blp}`; a missing or stale extraction WARNs and draws blank, never fails the
//! load (the misses name themselves through `take_era_atlas_misses`).

use benilla_ui::script::EraAtlasEntry;
use serde::Deserialize;

/// The extracted Era UI assets root — through the `WoW-era` symlink `wt.sh link_wow` provisions
/// in every slot (same `CARGO_MANIFEST_DIR` convention as the MPQ `Data` default elsewhere in
/// this crate: the path follows the worktree that built the binary).
pub(crate) fn era_ui_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW-era/_extracted_ui")
}

/// `manifest.json`, exactly as `scripts/era-extract.py` writes it (the fields we consume).
#[derive(Deserialize)]
struct Manifest {
    build: String,
    atlases: std::collections::HashMap<String, Member>,
}

#[derive(Deserialize)]
struct Member {
    /// Relative to the extraction root (`textures/<fdid>.blp`).
    file: String,
    /// The sheet's dimensions in pixels.
    atlas_size: [f32; 2],
    /// Pixel edges `[left, top, right, bottom)` within the sheet.
    rect: [f32; 4],
    /// Nominal draw size in UI units (`useAtlasSize`).
    size: [f32; 2],
}

/// Load and normalize the manifest into UiScript table entries: pixel rects become the
/// `[left, right, top, bottom]` UV form `TexCoords::Rect` stores; files ride the `era:` scheme.
/// `None` = no usable manifest (absent install, extraction never run, or a parse error — each
/// WARNs with the fix spelled out; the distinction matters when diagnosing a blank window).
pub(crate) fn load_era_atlases() -> Option<Vec<(String, EraAtlasEntry)>> {
    let path = era_ui_root().join("manifest.json");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            bevy::log::warn!(
                "era: no UI manifest at {} ({e}) — Era-styled windows will draw blank; \
                 run scripts/era-extract.py",
                path.display()
            );
            return None;
        }
    };
    let manifest: Manifest = match serde_json::from_slice(&bytes) {
        Ok(m) => m,
        Err(e) => {
            bevy::log::warn!(
                "era: manifest.json unparseable ({e}) — re-run scripts/era-extract.py"
            );
            return None;
        }
    };
    bevy::log::debug!(
        "era: atlas manifest for build {} — {} members",
        manifest.build,
        manifest.atlases.len()
    );
    Some(
        manifest
            .atlases
            .into_iter()
            .map(|(name, m)| {
                let [aw, ah] = m.atlas_size;
                let [l, t, r, b] = m.rect;
                (
                    name,
                    EraAtlasEntry {
                        file: format!("era:{}", m.file),
                        uv: [l / aw, r / aw, t / ah, b / ah],
                        size: [m.size[0], m.size[1]],
                    },
                )
            })
            .collect(),
    )
}
