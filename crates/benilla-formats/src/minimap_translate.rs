//! `textures\Minimap\md5translate.trs` — the minimap tile hash catalog: a plain-text index mapping
//! each authored minimap tile name to the content-hashed filename actually stored under
//! `textures\Minimap\` (Blizzard's dedup scheme — many tiles across zones/instances share one hash;
//! decision 0203 phase 0).
//!
//! Format — VERIFIED against build 5875 (727 561 B, extracted via `benilla-extract` this session,
//! 2026-07-07): CRLF-terminated text, 235 `dir: <Dir>` section headers, 8401 tab-separated data
//! lines `<Dir>\<file>.blp\t<hash>.blp`. Every data line already repeats its full left-hand
//! directory (checked exhaustively: all 8401 left-hand paths start, case-insensitively, with the
//! preceding `dir:` header) — the header is cosmetic for parsing, so keying straight off the left
//! column needs no header-tracking state. Two path shapes share the file and the same table,
//! un-special-cased: **ADT tiles** (`<MapDir>\map<X>_<Y>.blp`, one per streamed terrain tile — e.g.
//! `AhnQiraj\map27_46.blp`) and **WMO icon tiles** (`WMO\...\<name>_<n>_<row>_<col>.blp`, the
//! minimap building-corner icons). All 8401 left-hand paths are unique case-insensitively.
//!
//! Verified example: `Azeroth\map32_48.blp → ea283abc0bf9637c3fad5e840a65b38b.blp`, and that hash is
//! itself readable at `textures\Minimap\ea283abc0bf9637c3fad5e840a65b38b.blp` (decodes as a 256×256
//! BLP2 — confirmed this session).

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::Chain;

const MD5_TRANSLATE: &str = "textures\\Minimap\\md5translate.trs";

/// The parsed `md5translate.trs`: every left-hand path (lowercased) to its hashed `.blp` filename.
pub struct MinimapTranslate {
    /// `"<dir>\<file>.blp"` (lowercased) → the hashed filename on disk under `textures\Minimap\`
    /// (e.g. `"ea283abc0bf9637c3fad5e840a65b38b.blp"`).
    entries: HashMap<String, String>,
}

impl MinimapTranslate {
    /// The hashed minimap tile filename for `map_dir`'s ADT tile `(x, y)`, or `None` if this tile
    /// was never authored (open ocean / unstreamed tiles have no minimap art). `map_dir` is
    /// case-insensitive (MPQ path convention) — e.g. `tile("Azeroth", 32, 48)` resolves the
    /// verified `ea283abc0bf9637c3fad5e840a65b38b.blp`. Join the result onto
    /// `textures\Minimap\<hash>` to read the tile itself.
    pub fn tile(&self, map_dir: &str, x: u32, y: u32) -> Option<&str> {
        let key = format!("{map_dir}\\map{x}_{y}.blp").to_ascii_lowercase();
        self.entries.get(&key).map(String::as_str)
    }

    /// The hashed filename for an arbitrary logical tile path (case-insensitive, `\`-separated), or
    /// `None` if unauthored — e.g. the WMO interior tiles
    /// `wmo\KhazModan\Cities\Ironforge\ironforge_001_00_00.blp`. The ADT [`Self::tile`] is the
    /// `map<x>_<y>` special case of this.
    pub fn get(&self, logical_path: &str) -> Option<&str> {
        self.entries
            .get(&logical_path.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Parse `md5translate.trs`'s text body (CRLF or LF line endings; a `dir:` header line is skipped
/// — see the module doc, every data line already carries its full directory).
fn parse_trs(text: &str) -> HashMap<String, String> {
    let mut entries = HashMap::new();
    for raw in text.split('\n') {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with("dir:") {
            continue;
        }
        let Some((left, right)) = line.split_once('\t') else {
            continue;
        };
        entries.insert(left.to_ascii_lowercase(), right.to_string());
    }
    entries
}

/// Read `md5translate.trs` off the patch chain into a [`MinimapTranslate`].
pub fn load_minimap_translate(chain: &mut Chain) -> Result<MinimapTranslate> {
    let bytes = chain
        .read_file(MD5_TRANSLATE)
        .context("reading md5translate.trs")?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(MinimapTranslate {
        entries: parse_trs(&text),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dir_headers_and_tab_separated_rows_case_insensitively() {
        let text = "dir: AhnQiraj\r\n\
             AhnQiraj\\map27_46.blp\t1fcd95d6d410e7557d6b62081c5e87b5.blp\r\n\
             dir: Azeroth\r\n\
             Azeroth\\map32_48.blp\tea283abc0bf9637c3fad5e840a65b38b.blp\r\n";
        let entries = parse_trs(text);
        assert_eq!(entries.len(), 2);
        let cat = MinimapTranslate { entries };
        assert_eq!(
            cat.tile("Azeroth", 32, 48),
            Some("ea283abc0bf9637c3fad5e840a65b38b.blp")
        );
        assert_eq!(
            cat.tile("azeroth", 32, 48),
            Some("ea283abc0bf9637c3fad5e840a65b38b.blp"),
            "map_dir is case-insensitive (MPQ path convention)"
        );
        assert_eq!(cat.tile("Azeroth", 99, 99), None);
    }

    /// Real chain: the verified Azeroth tile resolves, and the hashed file it names is itself
    /// readable in the chain as a 256×256 BLP2. Skips without client data.
    #[test]
    fn real_md5translate_resolves_azeroth_tile_and_hash_is_readable() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_minimap_translate(&mut chain).expect("load md5translate.trs");
        assert_eq!(cat.len(), 8401, "all 8401 data rows parse to unique keys");

        let hash = cat
            .tile("Azeroth", 32, 48)
            .expect("Azeroth\\map32_48 resolves");
        assert_eq!(hash, "ea283abc0bf9637c3fad5e840a65b38b.blp");

        let tile_path = format!("textures\\Minimap\\{hash}");
        let (w, h, _rgba) =
            crate::read_texture_rgba(&mut chain, &tile_path).expect("hashed tile decodes as BLP");
        assert_eq!((w, h), (256, 256));

        // The WMO interior tile key format the minimap builds (`<stem>_<group>_<X>_<Y>.blp`, the
        // stem being the `.wmo` path minus `World\` + extension) resolves case-insensitively against
        // the real trs — the end-to-end link the interior renderer's tile lookup depends on.
        assert!(
            cat.get("wmo\\KhazModan\\Cities\\Ironforge\\ironforge_001_00_00.blp")
                .is_some(),
            "the Ironforge group-1 interior tile resolves"
        );
    }
}
