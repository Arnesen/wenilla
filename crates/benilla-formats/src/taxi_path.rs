//! `TaxiPath.dbc` — the direct-hop fare table between two flight-master nodes: `(from, to, cost)`
//! triples the flight-master UI walks to price a route (decision 0484 phase 1). Distinct from
//! `TaxiPathNode.dbc` (`crate::taxi`), which carries a path's actual waypoints — this table is
//! the coarse "does a direct hop from A to B exist, and what does it cost" lookup phase 2's route
//! computation needs; a multi-hop trip chains several of these rows.
//!
//! **4 fields (verified this session, matching vmangos's own `TaxiPathEntry`,
//! `DBCStructure.h:688-694`):** `ID(0), FromTaxiNode(1), ToTaxiNode(2), Price(3)`.
//!
//! Verified against the live table: id 6 is the direct hop `TaxiNodes` 2 ("Stormwind, Elwynn")
//! → 4 ("Sentinel Hill, Westfall"), cost 110 copper.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, u32_at};
use crate::Chain;

const TAXI_PATH: &str = "DBFilesClient\\TaxiPath.dbc";

/// One `TaxiPath.dbc` row — a direct hop between two flight-master nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaxiPath {
    pub id: u32,
    pub from: u32,
    pub to: u32,
    /// Fare in copper for this one hop, undiscounted — vmangos applies the reputation discount
    /// server-side before charging; the wire's `TaxiPath.dbc` copy never carries a discounted
    /// figure.
    pub cost: u32,
}

/// `TaxiPath.dbc` rows keyed by `ID`, plus a `(from, to)` lookup for the route computation
/// phase 2 needs.
pub struct TaxiPaths {
    rows: HashMap<u32, TaxiPath>,
    by_pair: HashMap<(u32, u32), u32>,
}

impl TaxiPaths {
    pub fn get(&self, id: u32) -> Option<&TaxiPath> {
        self.rows.get(&id)
    }

    /// The direct hop from `from` to `to`, if `TaxiPath.dbc` carries one. The table is
    /// directed — a two-way route is two separate rows, one per direction — so this only ever
    /// matches the exact order asked for.
    pub fn between(&self, from: u32, to: u32) -> Option<&TaxiPath> {
        self.by_pair
            .get(&(from, to))
            .and_then(|id| self.rows.get(id))
    }

    /// Every direct hop leaving `node`, in no particular order — a route search expands a node
    /// through this (phase 2).
    pub fn paths_from(&self, node: u32) -> impl Iterator<Item = &TaxiPath> {
        self.rows.values().filter(move |p| p.from == node)
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// 4 fields per the module doc.
fn schema() -> Schema {
    let mut s = Schema::new("TaxiPath");
    for name in ["ID", "FromTaxiNode", "ToTaxiNode", "Cost"] {
        s.add_field(SchemaField::new(name, FieldType::UInt32));
    }
    s
}

/// Read `TaxiPath.dbc` off the patch chain into a [`TaxiPaths`] catalog.
pub fn load_taxi_paths(chain: &mut Chain) -> Result<TaxiPaths> {
    let bytes = chain.read_file(TAXI_PATH).context("reading TaxiPath.dbc")?;
    let rs = parse(&bytes, schema(), "TaxiPath")?;
    let mut rows = HashMap::with_capacity(rs.records().len());
    let mut by_pair = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let Some(from) = u32_at(r, 1) else { continue };
        let Some(to) = u32_at(r, 2) else { continue };
        let cost = u32_at(r, 3).unwrap_or(0);
        rows.insert(id, TaxiPath { id, from, to, cost });
        by_pair.insert((from, to), id);
    }
    Ok(TaxiPaths { rows, by_pair })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real 5875 table proves the layout and the pinned Stormwind→Sentinel Hill hop: 287
    /// rows total, id 6 is the direct `2 → 4` hop at cost 110. Cross-checked with
    /// [`crate::taxi_nodes::tests::real_taxi_nodes_layout_sanity`]'s node names. Skips without
    /// client data.
    #[test]
    fn real_taxi_path_layout_sanity() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_taxi_paths(&mut chain).expect("load TaxiPath");
        assert_eq!(cat.len(), 287, "1.12 ships 287 taxi path hops");

        let hop = cat.between(2, 4).expect("a direct 2 -> 4 hop exists");
        assert_eq!(hop.id, 6);
        assert_eq!(hop.cost, 110);
        assert!(hop.cost > 0, "a real hop always carries a nonzero fare");

        // paths_from surfaces the same row when walking node 2's outgoing hops.
        assert!(cat.paths_from(2).any(|p| p.to == 4 && p.cost == 110));
    }
}
