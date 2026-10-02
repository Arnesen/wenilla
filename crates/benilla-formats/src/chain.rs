//! The vanilla patch chain: a priority-ordered set of MPQ archives, read through `benilla-mpq`. A
//! read resolves a name to the highest-priority archive holding it, so a patch wins; base archives
//! carry no `(listfile)`, so resolution is by name hash.
//!
//! **On `wasm32`** there is no filesystem: `Chain` is the web host's base URL (`crate::web`, the
//! Data URL scheme) and every method is a fetch. The public API is the same on both targets.

use std::path::Path;

use anyhow::{Context, Result};

#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashSet;

#[cfg(not(target_arch = "wasm32"))]
use anyhow::{anyhow, bail};
#[cfg(not(target_arch = "wasm32"))]
use benilla_mpq::Archive;

#[cfg(not(target_arch = "wasm32"))]
use crate::VANILLA_BASE_ORDER;

#[cfg(target_arch = "wasm32")]
use anyhow::anyhow;

/// One entry from a chain listing: an internal path and its uncompressed size.
pub struct ChainEntry {
    pub name: String,
    pub size: u64,
}

/// A priority-ordered patch chain of MPQ archives (`Send + Sync`; reads are `&self` and lock-free).
#[cfg(not(target_arch = "wasm32"))]
pub struct Chain {
    /// Ascending priority: later archives win.
    archives: Vec<Archive>,
}

/// The web build's `Chain`: no archives, just the web host's `/data` base URL every method fetches
/// against (see the module header). `read` carries no cache — a sync XHR pays a round trip (or a
/// browser-cache hit) every call, and the Bevy `AssetServer` above (`benilla-assets`) is what
/// dedups by path, same as it would for a native disk read.
///
/// **`contains` does carry one: the whole chain's name index**, fetched once from `/data/__index`
/// on the first ask. Measured on world entry (2026-08-31): the UI's texture probes asked
/// `contains` **2,145 times** in one entry — the same dozen chat-border and dialog icons over and
/// over, per region per resolve — and each ask was a synchronous `HEAD`, which the browser does
/// not serve from a `GET`-warmed cache. At 100 ms RTT that was ~125 s of a frozen tab, after every
/// other read had been prefetched. One 4.9 MB name list, parsed once, answers all of them from
/// memory; the `HEAD` stays only as the fallback for a host whose index route fails.
#[cfg(target_arch = "wasm32")]
pub struct Chain {
    base: String,
    /// `None` inside = the index could not be fetched/parsed; `contains` falls back to `HEAD`.
    index: std::sync::OnceLock<Option<std::collections::HashSet<String>>>,
}

/// `patch-?.MPQ` as the reference's `FindFirstFileW` glob matches it (template `0x82edbc`, wrapper
/// `0x42ad10`): `?` is exactly one character, any case, so `patch-10.MPQ` never mounts.
#[cfg(not(target_arch = "wasm32"))]
fn is_patch_glob_match(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some(mid) = lower
        .strip_prefix("patch-")
        .and_then(|rest| rest.strip_suffix(".mpq"))
    else {
        return false;
    };
    mid.chars().count() == 1
}

/// The reference's mount order over a `Data` listing, ascending priority (`0x403740`): the ten
/// [`VANILLA_BASE_ORDER`] archives, `patch.MPQ`, every `patch-?.MPQ` by case-folded name (the
/// reference sorts descending with `strnicmp` and walks backwards), then `speech2.MPQ`. Names
/// match case-insensitively and come back as found on disk.
#[cfg(not(target_arch = "wasm32"))]
fn mount_order(dir_names: &[String]) -> Vec<String> {
    let find = |want: &str| {
        dir_names
            .iter()
            .find(|n| n.eq_ignore_ascii_case(want))
            .cloned()
    };
    let mut order: Vec<String> = VANILLA_BASE_ORDER.iter().filter_map(|b| find(b)).collect();
    order.extend(find("patch.MPQ"));
    let mut patches: Vec<String> = dir_names
        .iter()
        .filter(|n| is_patch_glob_match(n))
        .cloned()
        .collect();
    patches.sort_by_key(|n| n.to_ascii_lowercase());
    order.extend(patches);
    order.extend(find("speech2.MPQ"));
    order
}

#[cfg(not(target_arch = "wasm32"))]
impl Chain {
    /// Open a `Data` directory's archives in [`mount_order`], or a single `.MPQ` file.
    ///
    /// Deviation: an archive that fails to open is an error, where the reference logs
    /// `"Failed to open archive"` and goes on, because a skipped corrupt archive surfaces only as
    /// missing files far downstream.
    pub fn open(path: &Path) -> Result<Self> {
        let mut archives = Vec::new();
        if path.is_dir() {
            let mut names: Vec<String> = std::fs::read_dir(path)
                .with_context(|| format!("listing {}", path.display()))?
                .filter_map(|entry| {
                    let entry = entry.ok()?;
                    // `path().is_file()` follows symlinks (`read_dir`'s file_type doesn't).
                    entry.path().is_file().then(|| entry.file_name())
                })
                .filter_map(|name| name.into_string().ok())
                .collect();
            // read_dir order is arbitrary; sort so case-variant ties resolve deterministically.
            names.sort();
            for name in mount_order(&names) {
                let mpq = path.join(&name);
                archives.push(
                    Archive::open(&mpq).with_context(|| format!("opening {}", mpq.display()))?,
                );
            }
            if archives.is_empty() {
                bail!("no known vanilla MPQs found in {}", path.display());
            }
        } else {
            archives.push(
                Archive::open(path).with_context(|| format!("opening MPQ {}", path.display()))?,
            );
        }
        Ok(Self { archives })
    }

    /// The highest-priority archive with an entry for `name`, a delete marker included, as a
    /// tombstone shadows every lower copy: check [`Archive::is_delete_marker`] for a readable file.
    fn resolve(&self, name: &str) -> Option<&Archive> {
        self.archives.iter().rev().find(|a| a.contains(name))
    }

    /// Whether `name` (`/` or `\`, any case) is a readable file, not a delete marker.
    pub fn contains(&self, name: &str) -> bool {
        self.resolve(name)
            .is_some_and(|a| !a.is_delete_marker(name))
    }

    /// The path of the archive `name` resolves to, for debugging and extraction.
    pub fn find_file_archive(&self, name: &str) -> Option<&Path> {
        self.resolve(name).map(|a| a.path())
    }

    /// Read a file by internal path (`/` or `\`) from its winning archive.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let archive = self.archive_for(name)?;
        archive
            .read_file(name)
            .with_context(|| format!("reading {name} from {}", archive.path().display()))
    }

    /// The archive `name` reads from, as a cheap handle: a caller holding the chain behind a lock
    /// looks the file up under it and reads (`Archive::open_file`) after releasing it. No I/O.
    pub fn archive_for(&self, name: &str) -> Result<Archive> {
        let archive = self
            .resolve(name)
            .ok_or_else(|| anyhow!("file not in patch chain: {name}"))?;
        // A tombstone deletes the path from the composite: not found, never a stale lower copy.
        if archive.is_delete_marker(name) {
            bail!(
                "file deleted from patch chain: {name} (tombstoned by {})",
                archive.path().display()
            );
        }
        Ok(archive.clone())
    }

    /// `&mut` alias of [`Chain::read`] for call sites that thread a `&mut Chain`.
    pub fn read_file(&mut self, name: &str) -> Result<Vec<u8>> {
        self.read(name)
    }

    /// The chain's named files with sizes, for development and extraction; a file in no listfile
    /// (most of `texture.MPQ`) is readable by name but not listed. Unions every archive's
    /// `(listfile)`, as each names only its own files; sizes come from the winning archive.
    pub fn list(&self) -> Result<Vec<ChainEntry>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for archive in &self.archives {
            let Ok(listfile) = archive.read_file("(listfile)") else {
                continue;
            };
            for raw in String::from_utf8_lossy(&listfile).split([';', '\r', '\n']) {
                let name = raw.trim();
                // Dedupe the way MPQ hashing compares names: any case, `/` and `\` alike.
                if name.is_empty() || !seen.insert(name.replace('/', "\\").to_ascii_lowercase()) {
                    continue;
                }
                if let Some(a) = self.resolve(name) {
                    // A tombstoned path is not a file in the composite.
                    if a.is_delete_marker(name) {
                        continue;
                    }
                    out.push(ChainEntry {
                        name: name.to_string(),
                        size: a.file_size(name).unwrap_or(0) as u64,
                    });
                }
            }
        }
        Ok(out)
    }
}

#[cfg(target_arch = "wasm32")]
impl Chain {
    /// Open the chain against the web host at `crate::web::data_base()`. `path` is accepted only
    /// to keep the signature identical to the native target's (call sites pass `wow_data()`, which
    /// on wasm is always `/data` — see `install::wow_data`) — it names nothing real on the web,
    /// where every chain file lives behind one HTTP origin, not a directory.
    ///
    /// Unlike the native path, this never fails: there is no directory to fail to list or archive
    /// to fail to open at open time. A web host that is down or missing a file only surfaces on
    /// the first `read`/`contains` call, same as a native disk read surfaces a missing file lazily
    /// too (it's just that native's redundant-archive check happens to run eagerly here).
    pub fn open(_path: &Path) -> Result<Self> {
        Ok(Self {
            base: crate::web::data_base(),
            index: std::sync::OnceLock::new(),
        })
    }

    /// The chain file's Data URL scheme address — the client half of the Lane A ↔ Lane H contract.
    fn url_for(&self, name: &str) -> String {
        format!("{}/{}", self.base, crate::web::encode_name(name))
    }

    /// The index's key for a name: MPQ hashing's equivalence — case-insensitive, `/` ≡ `\`.
    fn index_key(name: &str) -> String {
        name.replace('/', "\\").to_ascii_lowercase()
    }

    /// The chain's name index, fetched and parsed on first use (see the struct doc). `None` when
    /// the host has no working `/data/__index`, in which case every caller falls back to the
    /// per-name request it made before the index existed.
    fn index(&self) -> Option<&std::collections::HashSet<String>> {
        self.index
            .get_or_init(|| {
                let bytes = crate::web::fetch_sync(&format!("{}/__index", self.base)).ok()?;
                let names: Vec<String> = serde_json::from_slice(&bytes).ok()?;
                Some(names.iter().map(|n| Self::index_key(n)).collect())
            })
            .as_ref()
    }

    /// Whether the web host has `name` — from the name index when it loaded, else a `HEAD`
    /// request (no body).
    pub fn contains(&self, name: &str) -> bool {
        match self.index() {
            Some(set) => set.contains(&Self::index_key(name)),
            None => crate::web::exists_sync(&self.url_for(name)),
        }
    }

    /// No archive *files* exist on the web target — everything is served from the one web-host
    /// origin, so there is nothing more specific than `contains` to report.
    pub fn find_file_archive(&self, _name: &str) -> Option<&Path> {
        None
    }

    /// Read a file by internal path (accepts `/` or `\`) via a blocking `GET` — see
    /// `crate::web::fetch_sync` for why this is synchronous. The error wording on a missing file
    /// matches the native path's (`"file not in patch chain: {name}"`) so a caller that matches on
    /// that text — there are some — behaves the same on both targets.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        crate::web::trace(name); // boot-manifest capture; no-op unless the page armed it
                                 // A name the index says is absent is a 404 round trip saved — the same answer, and the
                                 // sprite-candidate walk (`Foo.blp`, then `Foo.tga`) asks for absent names by design.
        if self
            .index()
            .is_some_and(|set| !set.contains(&Self::index_key(name)))
        {
            return Err(anyhow!("file not in patch chain: {name}"));
        }
        crate::web::fetch_sync(&self.url_for(name)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow!("file not in patch chain: {name}")
            } else {
                anyhow!("fetching {name} from web host: {e}")
            }
        })
    }

    /// The URL a [`Chain::read`] of `name` would `GET`, or `None` when the name index already
    /// says the chain has no such file — the same answer `read` gives, without the round trip.
    ///
    /// This exists so a caller that must not block the frame can run the fetch **itself**,
    /// asynchronously, instead of going through `read`'s synchronous `XMLHttpRequest`: the chain
    /// lock is held only to build this string and is released before the request starts, so
    /// nothing holds it across an await. `sound::web_load` is the caller — see its header for the
    /// 206 ms doorway that motivated it.
    pub fn url_for_name(&self, name: &str) -> Option<String> {
        crate::web::trace(name); // boot-manifest capture, exactly as `read` does
        if self
            .index()
            .is_some_and(|set| !set.contains(&Self::index_key(name)))
        {
            return None;
        }
        Some(self.url_for(name))
    }

    /// `&mut` alias of [`Chain::read`] — see the native impl for why this exists.
    pub fn read_file(&mut self, name: &str) -> Result<Vec<u8>> {
        self.read(name)
    }

    /// List the chain's named files via `GET /data/__index` (the Data URL scheme's third route) —
    /// a JSON array of names. Sizes aren't part of that route (dev/extract tooling is the only
    /// consumer and doesn't run on the web target), so every entry reports `size: 0`.
    pub fn list(&self) -> Result<Vec<ChainEntry>> {
        let bytes = crate::web::fetch_sync(&format!("{}/__index", self.base))
            .map_err(|e| anyhow!("fetching chain index: {e}"))?;
        let names: Vec<String> =
            serde_json::from_slice(&bytes).context("parsing chain index JSON")?;
        Ok(names
            .into_iter()
            .map(|name| ChainEntry { name, size: 0 })
            .collect())
    }
}

// Native only: exercises `is_patch_glob_match`/`mount_order`, which don't exist on the web target.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn patch_glob_matches_exactly_one_character_case_insensitively() {
        assert!(is_patch_glob_match("patch-2.MPQ"));
        assert!(is_patch_glob_match("patch-3.MPQ"));
        assert!(is_patch_glob_match("PATCH-A.mpq"));
        assert!(!is_patch_glob_match("patch-.MPQ"));
        assert!(!is_patch_glob_match("patch-10.MPQ"));
        assert!(!is_patch_glob_match("patch-33.MPQ"));
        assert!(!is_patch_glob_match("patch.MPQ"));
        assert!(!is_patch_glob_match("patch-2.MPQ.bak"));
        assert!(!is_patch_glob_match("mypatch-2.MPQ"));
    }

    #[test]
    fn mount_order_is_the_carved_law() {
        // base.MPQ is telemetry-only in the reference and never mounts.
        let dir = owned(&[
            "patch-2.MPQ",
            "backup.MPQ",
            "model.MPQ",
            "base.MPQ",
            "dbc.MPQ",
            "patch.MPQ",
            "eula.html",
            "patch-3.MPQ",
            "speech2.MPQ",
            "texture.MPQ",
        ]);
        assert_eq!(
            mount_order(&dir),
            owned(&[
                "dbc.MPQ",
                "texture.MPQ",
                "model.MPQ",
                "patch.MPQ",
                "patch-2.MPQ",
                "patch-3.MPQ",
                "speech2.MPQ",
            ])
        );
    }

    #[test]
    fn patch_sort_is_ascending_and_case_folded() {
        let dir = owned(&["patch-B.MPQ", "patch-3.MPQ", "patch-a.MPQ", "patch-2.MPQ"]);
        assert_eq!(
            mount_order(&dir),
            owned(&["patch-2.MPQ", "patch-3.MPQ", "patch-a.MPQ", "patch-B.MPQ"])
        );
    }

    #[test]
    fn base_archives_are_found_case_insensitively() {
        let dir = owned(&["DBC.mpq", "Model.MPQ", "PATCH.mpq"]);
        assert_eq!(
            mount_order(&dir),
            owned(&["DBC.mpq", "Model.MPQ", "PATCH.mpq"])
        );
    }
}
