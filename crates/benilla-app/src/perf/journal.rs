//! The FPS JOURNAL (`WOW_FPS_JOURNAL=<csv path>`): once a second, append one row of where the
//! player is, what the frame cost, and **what is resident** (see [`JOURNAL_HEADER`] for the column
//! order, written into every fresh file) — the "where does it dip" instrument for a
//! director-driven run. They play normally; the journal turns "it drops in the Dwarven District"
//! into coordinates a headless probe can tele straight back to. Negligible cost: one line of IO a
//! second, samples reused from the frame meters.
//!
//! The residency columns make it the **leak curve** instrument too (B131): `FPS_PROBE`'s residency
//! meter samples once per run, which can only compare two runs at one point each — it cannot tell
//! "grows with distance streamed" from "grows with time elapsed", and cannot show *where* on a
//! route the cost arrives. A per-second row of `cpu_ms` beside `mats/images/uv/tint` plots the
//! per-frame cost directly against residency along one continuous leg, on the same time axis as
//! the position — so a same-map traverse (no `MapChange`, so no map-scoped eviction) shows its
//! accumulation as a slope instead of a before/after pair.

use bevy::prelude::*;
use bevy::time::Real;

use super::clock::{main_thread_cpu_secs, process_cpu_secs};

pub(crate) struct FpsJournalPlugin;

/// The journal's column order, written as the first line of a fresh file. Appended-to files keep
/// whatever header they were created with — the columns only ever grow at the end.
const JOURNAL_HEADER: &str = "t,x,y,z,mean_ms,p95_ms,streamed,entities,cpu_ms,mats,meshes,images,\
                              m2,uv,tint,pmat,emat,skin,cmat,tex,cgeo,evicted,fx,fy,fz,main_ms\n";

impl Plugin for FpsJournalPlugin {
    fn build(&self, app: &mut App) {
        let path = std::env::var("WOW_FPS_JOURNAL").unwrap_or_default();
        // The header goes in exactly once, at creation: the rows are appended for the life of the
        // run (and across runs, deliberately — a journal accumulates legs).
        if !path.is_empty() && !std::path::Path::new(&path).exists() {
            let _ = std::fs::write(&path, JOURNAL_HEADER);
        }
        app.insert_resource(FpsJournal {
            path,
            window: Vec::new(),
            last_flush: 0.0,
            cpu_at_flush: process_cpu_secs(),
            main_at_flush: None,
        })
        .add_systems(Update, journal_fps);
    }
}

#[derive(Resource)]
struct FpsJournal {
    path: String,
    window: Vec<f32>,
    last_flush: f32,
    /// Process CPU seconds at the previous flush — the row's `cpu_ms` is this second's CPU cost
    /// per frame, the load-robust half of the measurement.
    cpu_at_flush: Option<f64>,
    /// Main-thread CPU seconds at the previous flush, for the row's `main_ms`. Exactly parallel to
    /// `cpu_at_flush`, so the two columns are the same measurement at two scopes: `cpu_ms` is every
    /// thread's work, `main_ms` the serialized part of it. A leg where they diverge is a leg whose
    /// cost moved off (or onto) the critical path — which the all-threads column alone cannot say.
    main_at_flush: Option<f64>,
}

/// The journal's residency columns, grouped because `journal_fps` is near Bevy's system-param
/// arity limit.
///
/// The `Assets<T>` counts are the totals — what the process holds. The [`ArtCensus`] half is the
/// same population **broken down by the cache that holds it** (decision 0793), which is what turns
/// "materials are growing" into a named holder in one row instead of a run-length probe. `evicted`
/// is the running total dropped by distance: on a same-map traverse it was structurally zero before
/// 0793, because nothing but a `MapChange` evicted anything (0729).
///
/// [`ArtCensus`]: benilla_world::art_scope::ArtCensus
#[derive(bevy::ecs::system::SystemParam)]
struct JournalResidency<'w> {
    mats: Res<'w, Assets<benilla_assets::materials::WowModelMaterial>>,
    meshes: Res<'w, Assets<Mesh>>,
    images: Res<'w, Assets<bevy::image::Image>>,
    m2: Res<'w, Assets<benilla_assets::M2Model>>,
    uv_reg: Res<'w, benilla_world::doodad_anim::UvAnimMaterials>,
    tint_reg: Res<'w, benilla_world::doodad_anim::TintAnimMaterials>,
    art: Res<'w, benilla_world::art_scope::ArtCensus>,
    /// The **view focus** — where art is actually being asked for. Distinct from the row's `x,y,z`,
    /// which is the avatar: through a detached free-fly the body stands still while the camera covers
    /// kilometres, so on that leg the position columns describe nothing that is happening. The
    /// director's first run was exactly that leg, and reading it needed this column.
    scope: Res<'w, benilla_world::art_scope::ArtScopeState>,
}

/// `NonSendMarker` pins this to the main thread, which the `main_ms` column requires:
/// [`main_thread_cpu_secs`] reports *the calling thread*, so on a worker it would silently log
/// whichever pool thread ran the flush.
fn journal_fps(
    _pin_to_main_thread: bevy::ecs::system::NonSendMarker,
    mut journal: ResMut<FpsJournal>,
    time: Res<Time<Real>>,
    player: Option<Res<crate::player::Player>>,
    streamed: Query<(), With<crate::net::NetEntity>>,
    entities: Query<()>,
    residency: JournalResidency,
) {
    let now = time.elapsed_secs();
    journal.window.push(time.delta_secs() * 1000.0);
    if now - journal.last_flush < 1.0 {
        return;
    }
    journal.last_flush = now;
    let mut v = std::mem::take(&mut journal.window);
    if v.is_empty() {
        return;
    }
    v.sort_by(f32::total_cmp);
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let p95 = v[((v.len() - 1) as f32 * 0.95).round() as usize];
    // Raw WoW coords, so the line pastes straight into a `.go xyz` probe.
    let pos = player
        .filter(|p| p.active)
        .map(|p| benilla_assets::coords::bevy_to_wow(p.pos))
        .unwrap_or([0.0; 3]);
    // CPU per frame over this second — the number the reporter's "CPU %" compares against, and
    // the one that does not move with whatever else is compiling on this machine.
    let cpu_now = process_cpu_secs();
    let cpu_ms = match (journal.cpu_at_flush, cpu_now) {
        (Some(t0), Some(t1)) => format!("{:.2}", (t1 - t0) * 1000.0 / v.len() as f64),
        _ => String::new(),
    };
    journal.cpu_at_flush = cpu_now;
    let mut line = format!(
        "{now:.1},{:.1},{:.1},{:.1},{mean:.2},{p95:.2},{},{},{cpu_ms},{},{},{},{},{},{}",
        pos[0],
        pos[1],
        pos[2],
        streamed.iter().len(),
        entities.iter().len(),
        residency.mats.len(),
        residency.meshes.len(),
        residency.images.len(),
        residency.m2.len(),
        residency.uv_reg.0.len(),
        residency.tint_reg.0.len(),
    );
    // The per-cache breakdown, in `ArtSlot::ALL` order — which IS the header's column order.
    for slot in benilla_world::art_scope::ArtSlot::ALL {
        line.push_str(&format!(",{}", residency.art.live(slot)));
    }
    line.push_str(&format!(",{}", residency.art.dropped_total()));
    match residency.scope.focus() {
        Some(f) => line.push_str(&format!(",{:.1},{:.1},{:.1}", f[0], f[1], f[2])),
        None => line.push_str(",,,"),
    }
    // Appended at the end, per this file's own rule: the columns only ever grow there, so an
    // existing journal keeps parsing against the header it was created with.
    let main_now = main_thread_cpu_secs();
    match (journal.main_at_flush, main_now) {
        (Some(t0), Some(t1)) => {
            line.push_str(&format!(",{:.2}\n", (t1 - t0) * 1000.0 / v.len() as f64))
        }
        _ => line.push_str(",\n"),
    }
    journal.main_at_flush = main_now;
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&journal.path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}
