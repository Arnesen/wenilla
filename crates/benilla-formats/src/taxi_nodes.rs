//! `TaxiNodes.dbc` — every flight-master node: its map + world position, display name, and the
//! per-team taxi-mount creature template it rides (decision 0484 phase 1). Loaded whole at
//! startup, id-keyed — the taxi map's node list and the "which node am I nearest" resolve
//! through this catalog. `TaxiPath.dbc` (`crate::taxi_path`) carries the direct-hop fare table
//! between node pairs; `TaxiPathNode.dbc` (`crate::taxi`) carries a path's actual waypoints —
//! three different tables, three different jobs.
//!
//! **16 fields (verified this session, raw-parsed against build 5875, matching vmangos's own
//! `TaxiNodesEntry`, `DBCStructure.h:789-799`):** `ID(0), MapID(1), X(2), Y(3), Z(4)`, the
//! 9-dword localized-name block (`Name_lang` 8 locale strings, columns 5-12, + a flags dword at
//! 13 — the same loc-string shape `AreaPOI`/`AreaTable` use), then `MountCreatureID[2]` at
//! columns 14-15 — **horde first, alliance second** (vmangos's own field comment:
//! "horde[14]-alliance[15]"). Both are `CreatureTemplate` **entries**, not display ids — the
//! taxi mount's actual look resolves through the normal creature-template chain, same as any
//! other NPC (NOT a `CreatureDisplayInfo` id directly).
//!
//! Verified against the live table: id 2 is "Stormwind, Elwynn" on map 0, with `mount_horde = 0`
//! / `mount_alliance = 541` — a human-city node offers no Horde service and a nonzero Alliance
//! one, exactly as expected.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{f32_at, parse, str_at, u32_at};
use crate::Chain;

const TAXI_NODES: &str = "DBFilesClient\\TaxiNodes.dbc";

/// One `TaxiNodes.dbc` row — a flight-master node.
#[derive(Clone, Debug)]
pub struct TaxiNode {
    /// The row's own id — what `SMSG_SHOWTAXINODES`'s known-mask and `CMSG_ACTIVATETAXI`'s
    /// node fields name.
    pub id: u32,
    /// The map this node's `pos` is expressed in (`Map.dbc` id).
    pub map_id: u32,
    /// World position `(x, y, z)` on `map_id`.
    pub pos: [f32; 3],
    /// The enUS display name (locale slot 0) — the taxi map's node label.
    pub name: String,
    /// `CreatureTemplate` entry of the taxi mount a Horde rider takes off from this node, `0` =
    /// no Horde service here (e.g. an Alliance city node).
    pub mount_horde: u32,
    /// `CreatureTemplate` entry of the taxi mount an Alliance rider takes off from this node,
    /// `0` = no Alliance service here.
    pub mount_alliance: u32,
}

/// `TaxiNodes.dbc` rows keyed by `ID`.
pub struct TaxiNodes {
    rows: HashMap<u32, TaxiNode>,
}

impl TaxiNodes {
    pub fn get(&self, id: u32) -> Option<&TaxiNode> {
        self.rows.get(&id)
    }

    /// Every row, in no particular order — the taxi map's node list iterates this, filtered by
    /// the known-mask and continent.
    pub fn rows(&self) -> impl Iterator<Item = &TaxiNode> {
        self.rows.values()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// 16 fields per the module doc.
fn schema() -> Schema {
    let mut s = Schema::new("TaxiNodes");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    s.add_field(SchemaField::new("MapID", FieldType::UInt32));
    for name in ["X", "Y", "Z"] {
        s.add_field(SchemaField::new(name, FieldType::Float32));
    }
    s.add_field(SchemaField::new("Name", FieldType::String)); // enUS (locale 0)
    s.add_field(SchemaField::new_array(
        "NameOtherLocales",
        FieldType::String,
        7,
    ));
    s.add_field(SchemaField::new("NameFlags", FieldType::UInt32));
    s.add_field(SchemaField::new("MountIdHorde", FieldType::UInt32));
    s.add_field(SchemaField::new("MountIdAlliance", FieldType::UInt32));
    s
}

/// Read `TaxiNodes.dbc` off the patch chain into a [`TaxiNodes`] catalog.
pub fn load_taxi_nodes(chain: &mut Chain) -> Result<TaxiNodes> {
    let bytes = chain
        .read_file(TAXI_NODES)
        .context("reading TaxiNodes.dbc")?;
    let rs = parse(&bytes, schema(), "TaxiNodes")?;
    let mut rows = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let (Some(x), Some(y), Some(z)) = (f32_at(r, 2), f32_at(r, 3), f32_at(r, 4)) else {
            continue;
        };
        let node = TaxiNode {
            id,
            map_id: u32_at(r, 1).unwrap_or(0),
            pos: [x, y, z],
            name: str_at(&rs, r, 5).unwrap_or_default(),
            mount_horde: u32_at(r, 14).unwrap_or(0),
            mount_alliance: u32_at(r, 15).unwrap_or(0),
        };
        rows.insert(id, node);
    }
    Ok(TaxiNodes { rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real 5875 table proves the layout and the pinned Stormwind row: 85 nodes total, id 2
    /// is "Stormwind, Elwynn" on map 0 (Eastern Kingdoms), with a nonzero Alliance mount (541)
    /// and no Horde mount (0) — a human-city node offers Alliance-only service. Values dumped
    /// from the real DBC this session (`dbc_to_csv` + a raw byte cross-check), not guessed.
    /// Skips without client data.
    #[test]
    fn real_taxi_nodes_layout_sanity() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_taxi_nodes(&mut chain).expect("load TaxiNodes");
        assert_eq!(cat.len(), 85, "1.12 ships 85 taxi nodes");

        let stormwind = cat.get(2).expect("node 2 exists");
        assert_eq!(stormwind.name, "Stormwind, Elwynn");
        assert_eq!(stormwind.map_id, 0, "Eastern Kingdoms");
        assert_eq!(stormwind.mount_horde, 0, "no Horde service from Stormwind");
        assert_eq!(
            stormwind.mount_alliance, 541,
            "Stormwind's Alliance gryphon mount template"
        );

        // Sentinel Hill (Westfall), the other end of the pinned TaxiPath test's hop.
        let sentinel_hill = cat.get(4).expect("node 4 exists");
        assert_eq!(sentinel_hill.name, "Sentinel Hill, Westfall");
    }
}
