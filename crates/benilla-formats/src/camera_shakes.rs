//! `CameraShakes.dbc` — the shipped camera-shake presets.
//!
//! A shake is **not** authored per model or per animation: the model names a preset id, and this
//! 24-row table holds the shape. Two independent producers reach it (decision 1540):
//!
//! - **The big-creature footstep/death thud** — `CreatureModelData.FootstepShakeSize` (field 11)
//!   and `.DeathThudShakeSize` (field 12), read here through
//!   [`CreatureCatalog::footstep_shake`](crate::CreatureCatalog::footstep_shake).
//! - **Spell effects** — `SpellEffectCameraShakes.dbc`, a separate store the `$SHK` anim event
//!   indexes (wow-re `go-display-sound-events.md`: `[0xc0d814]`, bound `0xc0d818`). Not read here.
//!
//! **Layout — VERIFIED** against build 5875 (header + row decode, 2026-08-22): `24 × 8 × 32 B`,
//! string block empty. The **column names** are the conventional map (wowdev.wiki + vmangos
//! `DBCStructure.h`), and the shipped values corroborate them structurally rather than by
//! assertion: rows 4/5/6, 7/8/9, 13/14/15, 16/17/18, 36/37/38 and 76/77/78 are **direction
//! triples** — identical `Duration` within a triple, `Direction` running 0·1·2 — which is what a
//! per-axis column looks like and nothing else does.
//!
//! **The two families are visibly distinct**, which is the check that the creature columns really
//! do index this table:
//!
//! | family | rows | `Type`/`Direction` | `Duration` | `Phase` | `Coefficient` |
//! |---|---|---|---|---|---|
//! | creature (footstep + thud) | 1, 2, 10, 11, 12 | `1` / `2` | 0.40–0.65 s | **nonzero** | 1.0–2.0 |
//! | spell (the direction triples) | 4–9, 13–18, 36–38, 76–78 | 0 or 1 / 0·1·2 | 0.6–20 s | **0.0** | 0.4–3.0 |
//!
//! and within the creature family the amplitude ranks by mass exactly as it should: the Ancient
//! Protector and the kodo take row 1 (`amplitude 2.0`), the Ancient of Lore/War, the giants and
//! the dragons take row 2 (`amplitude 7.0`), and the death thuds (rows 11/12) are longer and
//! stronger than the footsteps.
//!
//! **What is NOT settled here.** The *semantics* — what `Type` and `Direction` select, what the
//! evaluator does with `Amplitude`/`Frequency`/`Phase`/`Coefficient`, and above all the distance
//! attenuation — are the reference's to state, and are the subject of the wow-re dispatch behind
//! decision 1540. This module is the shipped data and nothing more: it deliberately carries **no**
//! evaluator, so nothing can quietly come to depend on a guessed formula.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{f32_at, parse, u32_at};
use crate::Chain;

const CAMERA_SHAKES: &str = "DBFilesClient\\CameraShakes.dbc";

/// One `CameraShakes.dbc` row — the authored shape of a shake, in the table's own units.
///
/// Every field below `id` is data as shipped; see the module doc for what is verified (the layout
/// and the family split) and what still awaits the reference (the evaluator's reading of them).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraShake {
    pub id: u32,
    /// `ShakeType` — 0 or 1 across the shipped table; every creature row is `1`.
    pub shake_type: u32,
    /// `Direction` — 0·1·2, running consistently across the spell rows' triples; every creature
    /// row is `2`.
    pub direction: u32,
    pub amplitude: f32,
    pub frequency: f32,
    /// Seconds.
    pub duration: f32,
    pub phase: f32,
    pub coefficient: f32,
}

/// `CameraShakes.dbc`, keyed by row id.
///
/// `Default` is the **empty** catalog — the same thing "the DBC failed to load" already means to
/// every consumer (every lookup misses and the caller takes its documented fallback).
#[derive(Default)]
pub struct CameraShakeCatalog {
    rows: HashMap<u32, CameraShake>,
}

impl CameraShakeCatalog {
    /// One preset by id. `None` for id 0 (the "no shake" value both creature columns use) and for
    /// any id the shipped table does not carry.
    pub fn get(&self, id: u32) -> Option<&CameraShake> {
        self.rows.get(&id)
    }

    /// Every row, unordered.
    pub fn iter(&self) -> impl Iterator<Item = &CameraShake> {
        self.rows.values()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

fn camera_shakes_schema() -> Schema {
    let mut s = Schema::new("CameraShakes");
    for (name, ty) in [
        ("ID", FieldType::UInt32),
        ("ShakeType", FieldType::UInt32),
        ("Direction", FieldType::UInt32),
        ("Amplitude", FieldType::Float32),
        ("Frequency", FieldType::Float32),
        ("Duration", FieldType::Float32),
        ("Phase", FieldType::Float32),
        ("Coefficient", FieldType::Float32),
    ] {
        s.add_field(SchemaField::new(name, ty));
    }
    s
}

/// Read `CameraShakes.dbc` off the patch chain.
pub fn load_camera_shakes(chain: &mut Chain) -> Result<CameraShakeCatalog> {
    let bytes = chain
        .read_file(CAMERA_SHAKES)
        .with_context(|| format!("reading {CAMERA_SHAKES}"))?;
    let rs = parse(&bytes, camera_shakes_schema(), "CameraShakes")?;
    let mut rows = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        rows.insert(
            id,
            CameraShake {
                id,
                shake_type: u32_at(r, 1).unwrap_or(0),
                direction: u32_at(r, 2).unwrap_or(0),
                amplitude: f32_at(r, 3).unwrap_or(0.0),
                frequency: f32_at(r, 4).unwrap_or(0.0),
                duration: f32_at(r, 5).unwrap_or(0.0),
                phase: f32_at(r, 6).unwrap_or(0.0),
                coefficient: f32_at(r, 7).unwrap_or(0.0),
            },
        );
    }
    Ok(CameraShakeCatalog { rows })
}
