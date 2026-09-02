//! Corpus-scan reports: the **population instruments** that sweep many models or placements to
//! answer "how big is this class, and where does it live", rather than dumping one asset (that is
//! [`crate::m2dump`]'s job). Every one of them is reached as `scan::<name>` from the dispatch in
//! [`crate::main`]; the submodules group them by the question they answer, not by the file format
//! they read:
//!
//! - [`world`] — placed content: a WMO root's own tables, an ADT block's placements.
//! - [`lighting`] — what lights a model: the WMO prop lanes, M2 light blocks, terrain shadow.
//! - [`geometry`] — what geometry a model draws: billboards, geosets, flat ground quads.
//! - [`material`] — how a batch is textured and blended: blend modes, UV wrap, env stages,
//!   the batches whose UV/tint loop differs between sequence slots.
//! - [`particles`] — particle and ribbon emitters, and the features the corpus authors.
//! - [`skeleton`] — the bone tree and the attachment table that addresses it.
//! - [`sequence`] — which sequence an arm plays, what breaks when it is the wrong one, and
//!   which of its event markers an arm can reach at all.
//!
//! Split out of a single 3.9k-line `scan.rs` once it had grown ~30 independent sweeps: the file
//! was one *kind* of thing, but not one concern, and nothing in it was shared across families
//! except the corpus listing below.

use anyhow::{Context, Result};
use benilla_formats::Chain;

mod geometry;
mod lighting;
mod material;
mod particles;
mod sequence;
mod skeleton;
mod world;

pub use geometry::{animboundscan, bbfacescan, bbscan, geosetscan, groundscan, normalscan};
pub use lighting::{darkpropscan, m2lightscan, shadeat};
pub use material::{alphascan, blendscan, envmapscan, texmodescan, uvslotscan, uvwrapscan};
pub use particles::{
    cellscan, fxordercensus, partcensus, partscan, partslotscan, ribbonscan, shardcensus,
};
pub use sequence::{fxlifescan, goanimscan, idleslotscan, seqclockscan, soundeventscan};
pub use skeleton::{attachscan, bonescan};
pub use world::{doodadscan, placescan, skyboxscan, wmodoodads};

/// Every `.m2` in the chain, in listfile order, narrowed to a path `prefix` when one is given.
///
/// The opening line of nearly every sweep here, and — before the split — twenty-two copy-pasted
/// copies of it. Names come back in their listfile casing (that is what a report prints and what
/// [`Chain::read_file`] is handed); the `.m2` test and the prefix match are both done on a
/// lowercased copy, and the prefix is normalized to the chain's own `\` separators, so a caller
/// may pass either slash in either case.
pub(crate) fn m2_names(chain: &mut Chain, prefix: Option<&str>) -> Result<Vec<String>> {
    let pfx = prefix.map(|p| p.to_ascii_lowercase().replace('/', "\\"));
    Ok(chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".m2") && pfx.as_deref().is_none_or(|p| l.starts_with(p))
        })
        .collect())
}

/// Every WMO **root** in the chain, narrowed to a path `prefix` when one is given — same casing
/// and matching rules as [`m2_names`].
///
/// A WMO ships as one root plus one file per group, named `<stem>_NNN.wmo`. Only the root carries
/// the tables a sweep wants (MOHD/MODS/MODD/MOGI/MOLT/MOSB); a group file handed to
/// [`benilla_formats::parse_wmo_root`] is just a parse failure to skip. Underscore then exactly
/// three digits is the whole test — and it is here, once, because the two sweeps that used to
/// spell it inline had drifted into spelling it *differently* (one guarded a stem shorter than
/// four characters, the other did not, so a hypothetical `123.wmo` was a root to one and a group
/// to the other).
fn wmo_roots(chain: &mut Chain, prefix: Option<&str>) -> Result<Vec<String>> {
    let pfx = prefix.map(|p| p.to_ascii_lowercase().replace('/', "\\"));
    Ok(chain
        .list()
        .context("listing chain contents")?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            let Some(stem) = l.strip_suffix(".wmo") else {
                return false;
            };
            let group = stem.len() >= 4
                && stem.as_bytes()[stem.len() - 4] == b'_'
                && stem[stem.len() - 3..].bytes().all(|b| b.is_ascii_digit());
            !group && pfx.as_deref().is_none_or(|p| l.starts_with(p))
        })
        .collect())
}

/// Capitalize an `Item\ObjectComponents\<sub>` path component for consistent family-key display
/// regardless of how a given asset's listfile entry happened to be cased (`WEAPON`/`weapon`/
/// `Weapon` all collapse to `Weapon`).
fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => {
            first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
        }
        None => String::new(),
    }
}

/// The top-level content-family bucket for an internal M2 path — the summary dimension for "how
/// much content is affected, and of what KIND", shared by [`lighting::m2lightscan`] and
/// [`material::uvslotscan`]. Derived from the path's first one or two components,
/// case-insensitively.
///
/// The split that earns its keep is `World\` versus everything else: a `World\` model is a placed
/// doodad or WMO prop, which reaches the screen through the shared-material lanes (the M2-light
/// spawn, the `UvAnimMaterials`/`MatAnimTable` registries), while a `Creature\`/`Spells\` one
/// reaches an entirely different consumer. `World\Goober\` (GameObject displays) splits off again
/// because it is the hosted lane, not the ADT/WMO one; everything not otherwise named folds into
/// `other`.
pub(super) fn family_of(name: &str) -> String {
    let comps: Vec<&str> = name.split('\\').collect();
    let low = |s: &str| s.to_ascii_lowercase();
    match comps.first().map(|s| low(s)).as_deref() {
        Some("creature") => "Creature\\".to_string(),
        Some("character") => "Character\\".to_string(),
        Some("spells") => "Spells\\".to_string(),
        Some("item") if comps.get(1).map(|s| low(s)).as_deref() == Some("objectcomponents") => {
            match comps.get(2) {
                Some(sub) => format!("Item\\ObjectComponents\\{}\\", title_case(sub)),
                None => "Item\\ObjectComponents\\".to_string(),
            }
        }
        Some("world") if comps.get(1).map(|s| low(s)).as_deref() == Some("goober") => {
            "World\\Goober\\".to_string()
        }
        Some("world") => "World\\".to_string(),
        _ => "other".to_string(),
    }
}
