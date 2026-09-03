//! The vanilla patch chain — a priority-ordered set of MPQ archives, read through `benilla-mpq`.
//!
//! Replaces `wow-mpq`'s `PatchChain` *and* the old `ChainReader` (decision 0021). Those were two types
//! because `wow-mpq`'s `Archive::open` re-parsed the hash/block (and the useless `(attributes)`) tables
//! on every open, so `ChainReader` bolted a `Mutex<HashMap<…, Archive>>` handle-cache on top to avoid
//! re-paying that per read. `benilla_mpq::Archive` now caches its parsed tables in an `Arc` and reads
//! `&self` (a fresh OS handle per read, no seek-state sharing), so the cache is gone and one `Chain`
//! serves both the `&self` concurrent Bevy `AssetReader` path and the `&mut` streaming-loader path.
//!
//! Later archives override earlier ones for files sharing an internal path (so a patch archive
//! wins); a read resolves a name to the highest-priority archive that holds it. Base content
//! archives carry no `(listfile)`, so resolution is by name **hash**, which works without one.
//! Which archives mount, and in what order, is [`mount_order`]'s law (decision 1300).
//!
//! **On `wasm32`** there is no filesystem to mount archives from at all: the browser build talks
//! to a companion web host over HTTP instead (`crate::web`, the Data URL scheme, Lane A ↔ Lane H
//! of the wasm plan), so `Chain` there is just the host's base URL and every method becomes a
//! fetch. The public API — `open`/`contains`/`find_file_archive`/`read`/`read_file`/`list` — is
//! unchanged on both targets; only the two `impl Chain` blocks below differ.

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
    /// Ascending priority — later archives win. `resolve` scans back-to-front.
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

/// `patch-?.MPQ` with the reference's FindFirstFileW semantics: `?` matches **exactly one**
/// character, case-insensitively — `patch-3.MPQ` mounts, `patch-10.MPQ` does not (VERIFIED at the
/// glob template `0x82edbc` and its wrapper `0x42ad10`; wow-re `patch-mount-order.md`).
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

/// The vanilla mount law over a `Data` directory listing, **ascending priority** (decision 1300;
/// the mounter `0x403740`, carved in wow-re `system/mpq/scratch/patch-mount-order.md`): the ten
/// [`VANILLA_BASE_ORDER`] archives at their fixed priorities, then `patch.MPQ`, then every
/// `patch-?.MPQ` sorted ascending by case-folded name — the binary sorts its glob matches
/// *descending* (`strnicmp`) and walks the array backwards, so the order is deterministic, never
/// filesystem enumeration; `patch-3` overrides `patch-2` — then `speech2.MPQ` above every patch.
/// Names are matched case-insensitively (the reference runs on a case-insensitive filesystem) and
/// returned as found on disk; absent archives are simply not in the result.
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
    /// Open a chain from a vanilla `Data` directory (every archive [`mount_order`] finds in it,
    /// lowest priority first) or a single `.MPQ` file (just that archive).
    ///
    /// An archive that exists but fails to open is a hard error, deliberately: the reference logs
    /// `"Failed to open archive"` and continues, but a silent skip turns a corrupt `dbc.MPQ` — or a
    /// modder's malformed `patch-3.MPQ` — into cryptic missing-file failures far downstream. Same
    /// composite on a healthy install; a clear error instead of a quirk on a broken one (1300).
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

    /// The highest-priority archive with an *entry* for `name` (readable file **or** delete-marker),
    /// if any. Stops at the winning archive — including a tombstone, which correctly shadows any
    /// lower-priority copy (decision 0246). Callers that want "readable" must check
    /// [`Archive::is_delete_marker`].
    fn resolve(&self, name: &str) -> Option<&Archive> {
        self.archives.iter().rev().find(|a| a.contains(name))
    }

    /// Whether the chain holds `name` as a **readable** file (accepts `/` or `\`; case-insensitive).
    /// A path whose winning entry is a delete-marker is *not* present — the client deleted it (0246).
    pub fn contains(&self, name: &str) -> bool {
        self.resolve(name)
            .is_some_and(|a| !a.is_delete_marker(name))
    }

    /// The path of the archive `name` resolves to (the winning override) — for debugging / extract.
    pub fn find_file_archive(&self, name: &str) -> Option<&Path> {
        self.resolve(name).map(|a| a.path())
    }

    /// Read a file by internal path (accepts `/` or `\`), from its winning archive. `&self`: safe to
    /// call concurrently (the Bevy `AssetReader` does).
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let archive = self
            .resolve(name)
            .ok_or_else(|| anyhow!("file not in patch chain: {name}"))?;
        // A tombstone shadows every lower copy: the path is deleted from the composite, so this is a
        // clean "not found", not a fall-through to a stale base version (decision 0246).
        if archive.is_delete_marker(name) {
            bail!(
                "file deleted from patch chain: {name} (tombstoned by {})",
                archive.path().display()
            );
        }
        archive
            .read_file(name)
            .with_context(|| format!("reading {name} from {}", archive.path().display()))
    }

    /// `&mut` alias of [`Chain::read`] — kept so the streaming-loader call sites that thread a
    /// `&mut Chain` read exactly as they did against `wow-mpq`'s `PatchChain`.
    pub fn read_file(&mut self, name: &str) -> Result<Vec<u8>> {
        self.read(name)
    }

    /// List the chain's named files with sizes. Dev/extract use only — files absent from every
    /// listfile (most of `texture.MPQ`) are reachable by name but not enumerated.
    ///
    /// Unions the `(listfile)` of **every** archive that carries one: each archive's listfile names
    /// only the files *it* holds, so resolving `(listfile)` like an ordinary overridden file (as this
    /// used to) returns just the top patch archive's sliver — 92 names from `patch-2.MPQ` instead of
    /// the chain's ~86k. Sizes still resolve per-name to the winning archive.
    pub fn list(&self) -> Result<Vec<ChainEntry>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for archive in &self.archives {
            let Ok(listfile) = archive.read_file("(listfile)") else {
                continue;
            };
            for raw in String::from_utf8_lossy(&listfile).split([';', '\r', '\n']) {
                let name = raw.trim();
                // Dedupe across archives the way MPQ hashing compares names: case-insensitive, `/`≡`\`.
                if name.is_empty() || !seen.insert(name.replace('/', "\\").to_ascii_lowercase()) {
                    continue;
                }
                if let Some(a) = self.resolve(name) {
                    // A tombstoned path isn't a file in the composite — don't list it (0246).
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
        // Zero or two-plus characters: FindFirstFileW's `?` is exactly one.
        assert!(!is_patch_glob_match("patch-.MPQ"));
        assert!(!is_patch_glob_match("patch-10.MPQ"));
        assert!(!is_patch_glob_match("patch-33.MPQ"));
        // Not the glob's shape at all.
        assert!(!is_patch_glob_match("patch.MPQ"));
        assert!(!is_patch_glob_match("patch-2.MPQ.bak"));
        assert!(!is_patch_glob_match("mypatch-2.MPQ"));
    }

    #[test]
    fn mount_order_is_the_carved_law() {
        // A shuffled install with a custom patch, plus files the mounter must ignore:
        // base.MPQ (telemetry-only in the reference), backup.MPQ, loose non-archives.
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
        // Later wins in `Chain`, so ascending case-folded order makes patch-3 override patch-2
        // and `patch-B` override `patch-a` ('a' < 'b' after the strnicmp-style fold).
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
