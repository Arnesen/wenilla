//! Env-gated one-shot and per-second counters: the premise-checkers a sizing question needs before
//! anyone designs a fix for it.

use bevy::prelude::*;

/// `WOW_CPU_CENSUS=<at>:<secs>` — the per-thread CPU census (macOS): where does the pill's
/// cpu-ms actually go, thread by thread, summing **exactly** to the process total?
///
/// Two snapshots bracket a window: at each, `getrusage(RUSAGE_SELF)` (the pill's own clock,
/// [`super::clock::process_cpu_secs`]) and then every live thread's cumulative CPU via
/// `proc_pidinfo(PROC_PIDLISTTHREADS / PROC_PIDTHREADINFO)`. The report prints each thread's
/// delta over the window in ms/frame, then the **residual** = process delta − Σ live-thread
/// deltas, which is exactly the CPU of threads that exited inside the window (plus the
/// inter-read skew of the sweep itself). Nothing is modeled: every row is a measured delta and
/// the rows plus the residual reproduce the process total by construction.
///
/// Facts this rests on, pinned empirically on this machine (2026-08-20, threadcpu.c probe)
/// before this was written:
/// - `pth_user_time`/`pth_system_time` are **nanoseconds** (a 200 ms calibrated burn read
///   196.6 ms; the mach-timebase interpretation read 31.8× over) — *not* mach ticks, unlike
///   `rusage_info`'s `ri_user_time`.
/// - live-thread sums + exited-thread time = `getrusage` total to 0.05 ms over a ~1 s burn
///   (a thread that burned 250 ms and exited was absent from the list and present in the
///   residual at exactly 250.0 ms).
/// - `PROC_PIDLISTTHREADS = 6` (verified in the SDK's `sys/proc_info.h`; libc lacks the const).
#[cfg(target_os = "macos")]
pub(super) mod cpu_census {
    use std::collections::HashMap;

    use bevy::prelude::*;
    use bevy::time::Real;

    use crate::perf::clock::process_cpu_secs;

    /// `sys/proc_info.h`; not in the libc crate (its sibling `PROC_PIDTHREADINFO = 5` is).
    const PROC_PIDLISTTHREADS: libc::c_int = 6;

    /// One cumulative reading per live thread: `tid → (name, user_ns, system_ns)`. User and
    /// system are kept apart on purpose: real work is user time, while a thread whose CPU is
    /// mostly *system* time is burning it in the kernel — park/wake churn, semaphores, mach
    /// calls — which points at the scheduler/driver seam rather than at any system's body.
    fn thread_snapshot() -> HashMap<u64, (String, u64, u64)> {
        let pid = std::process::id() as libc::c_int;
        let mut tids = [0u64; 512];
        // SAFETY: proc_pidinfo writes at most `buffersize` bytes into the buffer and returns
        // how many it wrote; the buffer is a plain u64 array.
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDLISTTHREADS,
                0,
                tids.as_mut_ptr().cast(),
                std::mem::size_of_val(&tids) as libc::c_int,
            )
        };
        if n <= 0 {
            return HashMap::new();
        }
        let count = n as usize / std::mem::size_of::<u64>();
        let mut out = HashMap::with_capacity(count);
        for &tid in &tids[..count] {
            // SAFETY: zeroed is a valid proc_threadinfo (plain integers + a char array);
            // proc_pidinfo fills it and returns the size written.
            let mut ti: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
            let r = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTHREADINFO,
                    tid,
                    (&raw mut ti).cast(),
                    std::mem::size_of::<libc::proc_threadinfo>() as libc::c_int,
                )
            };
            if r as usize != std::mem::size_of::<libc::proc_threadinfo>() {
                continue; // the thread died mid-sweep: its time lands in the residual
            }
            let name_bytes: Vec<u8> = ti
                .pth_name
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c as u8)
                .collect();
            let name = String::from_utf8_lossy(&name_bytes).into_owned();
            out.insert(tid, (name, ti.pth_user_time, ti.pth_system_time));
        }
        out
    }

    struct Snap {
        t: f32,
        frames: u64,
        process_secs: f64,
        threads: HashMap<u64, (String, u64, u64)>,
    }

    /// Armed from the env at plugin build (on the main thread — which is how `main_tid` is
    /// known without pinning the census system itself).
    #[derive(Resource)]
    pub(in crate::perf) struct CpuCensus {
        at: f32,
        window: f32,
        main_tid: u64,
        frames: u64,
        start: Option<Snap>,
        /// Every thread seen by the once-a-second sweeps inside the window, with its **latest**
        /// cumulative reading — so a thread that dies mid-window can still be *named* in the
        /// residual report (its last reading is a lower bound on what it contributed).
        seen: HashMap<u64, (String, u64, u64)>,
        next_sweep: f32,
        done: bool,
    }

    impl CpuCensus {
        /// Parse `WOW_CPU_CENSUS=<at>:<secs>`. Must be called on the main thread.
        pub(in crate::perf) fn from_env() -> Option<Self> {
            let v = std::env::var("WOW_CPU_CENSUS").ok()?;
            let (at, window) = v.split_once(':')?;
            let mut main_tid = 0u64;
            // SAFETY: null pthread = the calling thread; writes one u64.
            unsafe { libc::pthread_threadid_np(0, &mut main_tid) };
            Some(Self {
                at: at.parse().ok()?,
                window: window.parse().ok()?,
                main_tid,
                frames: 0,
                start: None,
                seen: HashMap::new(),
                next_sweep: 0.0,
                done: false,
            })
        }
    }

    pub(in crate::perf) fn cpu_census(mut census: ResMut<CpuCensus>, time: Res<Time<Real>>) {
        if census.done {
            return;
        }
        census.frames += 1;
        let now = time.elapsed_secs();
        if census.start.is_none() {
            if now >= census.at {
                census.start = Some(Snap {
                    t: now,
                    frames: census.frames,
                    process_secs: process_cpu_secs().unwrap_or(0.0),
                    threads: thread_snapshot(),
                });
            }
            return;
        }
        if now >= census.next_sweep {
            census.next_sweep = now + 1.0;
            for (tid, entry) in thread_snapshot() {
                census.seen.insert(tid, entry);
            }
        }
        let start = census.start.as_ref().unwrap();
        if now - start.t < census.window {
            return;
        }
        let end_process = process_cpu_secs().unwrap_or(0.0);
        let end_threads = thread_snapshot();
        let frames = (census.frames - start.frames).max(1);
        let dt = now - start.t;
        let process_ms = (end_process - start.process_secs) * 1000.0;
        let per_frame = |ms: f64| ms / frames as f64;

        // Per-thread deltas over the window. A thread born inside the window has no start
        // reading — its full cumulative time is its delta (it was zero at birth).
        let mut rows: Vec<(f64, f64, String)> = Vec::new(); // (user_ms, sys_ms, label)
        let mut live_sum_ms = 0.0f64;
        for (tid, (name, end_u, end_s)) in &end_threads {
            let (start_u, start_s) = start.threads.get(tid).map_or((0, 0), |(_, u, s)| (*u, *s));
            let user_ms = end_u.saturating_sub(start_u) as f64 / 1e6;
            let sys_ms = end_s.saturating_sub(start_s) as f64 / 1e6;
            live_sum_ms += user_ms + sys_ms;
            // Hex tids so a row can be matched against `/usr/bin/sample`'s "Thread 0x…" headers.
            let label = if *tid == census.main_tid {
                format!("main (tid 0x{tid:x})")
            } else if name.is_empty() {
                format!("(unnamed tid 0x{tid:x})")
            } else {
                format!("{name} (tid 0x{tid:x})")
            };
            rows.push((user_ms, sys_ms, label));
        }
        rows.sort_by(|a, b| (b.0 + b.1).total_cmp(&(a.0 + a.1)));
        let residual_ms = process_ms - live_sum_ms;

        eprintln!(
            "[cpu-census] window {dt:.2} s, {frames} frames ({:.1} fps), process {process_ms:.1} ms \
             = {:.4} ms/frame  (the pill's number over this window)",
            frames as f32 / dt,
            per_frame(process_ms),
        );
        eprintln!(
            "[cpu-census] {:>9}  {:>8}  {:>8}  {:>6}  thread",
            "ms/frame", "user", "sys", "share"
        );
        for (u, s, label) in &rows {
            let ms = u + s;
            if per_frame(ms) < 0.0005 {
                continue; // folded into the "under" line below — still in the printed sum
            }
            eprintln!(
                "[cpu-census] {:>9.4}  {:>8.4}  {:>8.4}  {:>5.1}%  {label}",
                per_frame(ms),
                per_frame(*u),
                per_frame(*s),
                ms / process_ms * 100.0
            );
        }
        let tiny: f64 = rows
            .iter()
            .filter(|(u, s, _)| per_frame(u + s) < 0.0005)
            .map(|(u, s, _)| u + s)
            .sum();
        let tiny_n = rows
            .iter()
            .filter(|(u, s, _)| per_frame(u + s) < 0.0005)
            .count();
        if tiny_n > 0 {
            eprintln!(
                "[cpu-census] {:>9.4}  {:>5.1}%  ({tiny_n} threads under 0.5 µs/frame each)",
                per_frame(tiny),
                tiny / process_ms * 100.0
            );
        }
        eprintln!(
            "[cpu-census] {:>9.4}  {:>5.1}%  (residual: threads exited in-window + sweep skew)",
            per_frame(residual_ms),
            residual_ms / process_ms * 100.0
        );
        // Name the churn: threads the per-second sweeps saw that are gone by the end snapshot.
        // Their last cumulative reading minus their start reading (0 if born in-window) is a
        // LOWER bound on what they contributed to the residual — they kept burning after the
        // sweep that last saw them.
        let mut churned: HashMap<String, (usize, f64)> = HashMap::new();
        for (tid, (name, last_u, last_s)) in &census.seen {
            if end_threads.contains_key(tid) {
                continue;
            }
            let (start_u, start_s) = start.threads.get(tid).map_or((0, 0), |(_, u, s)| (*u, *s));
            let e = churned.entry(name.clone()).or_insert((0, 0.0));
            e.0 += 1;
            e.1 += (last_u.saturating_sub(start_u) + last_s.saturating_sub(start_s)) as f64 / 1e6;
        }
        for (name, (n, ms)) in &churned {
            eprintln!(
                "[cpu-census]   churn: {n} thread(s) '{}' died in-window, ≥{:.4} ms/frame of the residual",
                if name.is_empty() { "(unnamed)" } else { name },
                per_frame(*ms)
            );
        }
        eprintln!(
            "[cpu-census] sum check: Σ live {live_sum_ms:.1} + residual {residual_ms:.1} \
             = {:.1} vs process {process_ms:.1} ms (identity, by construction)",
            live_sum_ms + residual_ms
        );
        census.done = true;
    }
}

/// `WOW_MESH_EVENTS=1`: per-second Mesh asset-event counts (see the plugin registration). The
/// `sample` list names a few mutated asset ids so the writer can be found by grepping who holds
/// that handle.
pub(super) fn count_mesh_events(
    mut events: MessageReader<bevy::asset::AssetEvent<Mesh>>,
    time: Res<Time>,
    mut acc: Local<(f32, u32, u32, u32, Vec<String>)>,
) {
    let (last, added, modified, removed, sample) = &mut *acc;
    for e in events.read() {
        match e {
            bevy::asset::AssetEvent::Added { .. } => *added += 1,
            bevy::asset::AssetEvent::Modified { id } => {
                *modified += 1;
                if sample.len() < 4 {
                    sample.push(format!("{id:?}"));
                }
            }
            bevy::asset::AssetEvent::Removed { .. } | bevy::asset::AssetEvent::Unused { .. } => {
                *removed += 1;
            }
            bevy::asset::AssetEvent::LoadedWithDependencies { .. } => {}
        }
    }
    if time.elapsed_secs() - *last >= 1.0 {
        eprintln!(
            "[mesh-events] added={added} modified={modified} removed={removed}/s sample={sample:?}"
        );
        (*added, *modified, *removed) = (0, 0, 0);
        sample.clear();
        *last = time.elapsed_secs();
    }
}

/// `WOW_PART_CHURN=1`: per-second M2-part churn. `rm_frames` is the count of frames with ≥1
/// `MeshMaterial3d<WowModelMaterial>` removal — exactly the predicate that promotes
/// `classify_water_side` to its full walk (0930's twin-GC mark), so a regime reading ~60 here
/// full-walks every frame. `mat_mod` counts `WowModelMaterial` asset Modified events — the
/// global asset-changed tick that wakes every `AssetChanged` scan.
pub(super) fn count_part_churn(
    added: Query<(), Added<MeshMaterial3d<benilla_assets::materials::WowModelMaterial>>>,
    mut removed: RemovedComponents<MeshMaterial3d<benilla_assets::materials::WowModelMaterial>>,
    mut mat_events: MessageReader<
        bevy::asset::AssetEvent<benilla_assets::materials::WowModelMaterial>,
    >,
    time: Res<Time>,
    mut acc: Local<(f32, u32, u32, u32, u32)>,
) {
    let (last, add_n, rm_n, rm_frames, mat_mod) = &mut *acc;
    *add_n += added.iter().count() as u32;
    let rm = removed.read().count() as u32;
    *rm_n += rm;
    if rm > 0 {
        *rm_frames += 1;
    }
    *mat_mod += mat_events
        .read()
        .filter(|e| matches!(e, bevy::asset::AssetEvent::Modified { .. }))
        .count() as u32;
    if time.elapsed_secs() - *last >= 1.0 {
        eprintln!(
            "[part-churn] parts +{add_n} -{rm_n} rm_frames={rm_frames}/s mat_modified={mat_mod}/s"
        );
        (*add_n, *rm_n, *rm_frames, *mat_mod) = (0, 0, 0, 0);
        *last = time.elapsed_secs();
    }
}

/// `WOW_MESH_HOLDERS=1`: once a second, the set of mesh asset ids Modified in the last second
/// and the component signature of every entity holding one — [`count_mesh_events`] counts and
/// samples the ids, this names the writer by its holder's archetype (an id alone says nothing).
/// Exclusive, like [`arch_census`], for the same reason: the signature needs the live archetype.
pub(super) fn mesh_holders(
    world: &mut World,
    mut cursor: Local<Option<bevy::ecs::message::MessageCursor<bevy::asset::AssetEvent<Mesh>>>>,
    mut acc: Local<(f32, std::collections::HashSet<bevy::asset::AssetId<Mesh>>)>,
) {
    let messages = world.resource::<bevy::ecs::message::Messages<bevy::asset::AssetEvent<Mesh>>>();
    let cursor = cursor.get_or_insert_with(|| messages.get_cursor());
    let (last, ids) = &mut *acc;
    ids.extend(cursor.read(messages).filter_map(|e| match e {
        bevy::asset::AssetEvent::Modified { id } => Some(*id),
        _ => None,
    }));
    let now = world.resource::<Time>().elapsed_secs();
    if now - *last < 1.0 {
        return;
    }
    *last = now;
    if ids.is_empty() {
        return;
    }
    let short = |full: &str| -> String {
        let base = full.split('<').next().unwrap_or(full);
        let segs: Vec<&str> = base.split("::").collect();
        segs[segs.len().saturating_sub(2)..].join("::")
    };
    let mut found = 0usize;
    let mut holders = world.query::<(Entity, &Mesh3d)>();
    let matches: Vec<Entity> = holders
        .iter(world)
        .filter(|(_, m)| ids.contains(&m.0.id()))
        .map(|(e, _)| e)
        .take(6)
        .collect();
    for e in matches {
        found += 1;
        let sig: Vec<String> = world
            .entity(e)
            .archetype()
            .components()
            .iter()
            .filter_map(|&c| world.components().get_info(c))
            .map(|i| short(&i.name().to_string()))
            .collect();
        eprintln!(
            "[mesh-holders] {e} holds a modified mesh: {}",
            sig.join("+")
        );
    }
    if found == 0 {
        eprintln!(
            "[mesh-holders] {} modified id(s), no Mesh3d holder (2d or unowned)",
            ids.len()
        );
    }
    ids.clear();
}

/// When the one-shot archetype census fires (seconds of `Time` elapsed; `f32::MAX` = spent).
#[derive(Resource)]
pub(super) struct ArchCensusAt(pub(super) f32);

/// The census itself (`WOW_ARCH_CENSUS`): exclusive, so it sees every archetype of the live
/// world in one stop. Component paths are trimmed to their last two segments — the census reads
/// as lanes, not as imports.
pub(super) fn arch_census(world: &mut World) {
    let due = world.resource::<ArchCensusAt>().0;
    if world.resource::<bevy::time::Time>().elapsed_secs() < due {
        return;
    }
    world.resource_mut::<ArchCensusAt>().0 = f32::MAX;
    let short = |full: &str| -> String {
        let base = full.split('<').next().unwrap_or(full);
        let segs: Vec<&str> = base.split("::").collect();
        segs[segs.len().saturating_sub(2)..].join("::")
    };
    let mut rows: Vec<(u32, String)> = world
        .archetypes()
        .iter()
        .filter(|a| !a.is_empty())
        .map(|a| {
            let mut names: Vec<String> = a
                .components()
                .iter()
                .filter_map(|&c| world.components().get_info(c))
                .map(|i| short(&i.name().to_string()))
                .collect();
            names.sort();
            (a.len(), names.join("+"))
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    let total: u32 = rows.iter().map(|r| r.0).sum();
    eprintln!(
        "[census] {} entities across {} archetypes",
        total,
        rows.len()
    );
    for (n, sig) in rows.iter().take(60) {
        eprintln!("[census] {n:>7}  {sig}");
    }
}

/// `WOW_CAM_CHANGED=1`: per-second count of frames whose world-camera `Transform` /
/// `GlobalTransform` registered as changed (see the plugin registration).
pub(super) fn count_camera_changes(
    t_changed: Query<(), (With<benilla_world::view::WorldCamera>, Changed<Transform>)>,
    g_changed: Query<
        (),
        (
            With<benilla_world::view::WorldCamera>,
            Changed<GlobalTransform>,
        ),
    >,
    time: Res<Time>,
    mut acc: Local<(f32, u32, u32, u32)>,
) {
    let (last, frames, t_n, g_n) = &mut *acc;
    *frames += 1;
    *t_n += u32::from(!t_changed.is_empty());
    *g_n += u32::from(!g_changed.is_empty());
    if time.elapsed_secs() - *last >= 1.0 {
        eprintln!("[cam-changed] frames={frames} transform={t_n} global={g_n}/s");
        (*frames, *t_n, *g_n) = (0, 0, 0);
        *last = time.elapsed_secs();
    }
}

/// `WOW_ROW_BLOAT=<n>` — the consolidation question's premise counter (the drastic-options
/// census, 2026-08-17): once the world holds a real static model row, spawn `n` inert CLONES of
/// it — same mesh handle, same material handle, same component shape — parked 10,000 yd
/// underground so the frustum culls every one. The per-frame walks that scale with TOTAL rows
/// (the visibility reset/sweep pair, the `AssetChanged` tick scans, `PreviousGlobalTransform`,
/// `mark_dirty_trees`) pay for these rows exactly as for real ones, while the O(visible) half
/// (specialize/queue/encode) never sees them — so an interleaved leg A/B (bloat off vs on) reads
/// **d(cpu_ms)/d(rows)** directly. That derivative × the rows a mega-merge would delete is the
/// honest ceiling of the consolidation option, measured before anyone builds it.
/// (Measured the same night it was built: +30k rows = +1.33 cpu_ms at LBRS, ~44 ns/row/frame.)
///
/// `BloatSource` is one live static row's clonable component set, named for the lint.
type BloatSource<'w, 's> = Query<
    'w,
    's,
    (
        &'static Mesh3d,
        &'static MeshMaterial3d<benilla_assets::materials::WowModelMaterial>,
        &'static benilla_world::model_render::ModelPart,
        &'static bevy::mesh::MeshTag,
        &'static bevy::camera::primitives::Aabb,
    ),
    Without<benilla_world::rig_palette::RigPart>,
>;

pub(super) fn row_bloat(mut commands: Commands, mut done: Local<bool>, source: BloatSource) {
    if *done {
        return;
    }
    let Some((mesh, mat, part, tag, aabb)) = source.iter().next() else {
        return; // no static row streamed yet — try again next frame
    };
    let n: usize = std::env::var("WOW_ROW_BLOAT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    for _ in 0..n {
        commands.spawn((
            Mesh3d(mesh.0.clone()),
            MeshMaterial3d(mat.0.clone()),
            Transform::from_xyz(0.0, -10_000.0, 0.0),
            *part,
            tag.clone(),
            *aabb,
            bevy::camera::visibility::NoAutoAabb,
        ));
    }
    eprintln!(
        "[row-bloat] spawned {n} inert static rows (cloned a live world row, parked at y=-10000)"
    );
    *done = true;
}

/// `WOW_MESH_TOUCH=<secs>` — from `secs` onward, mark ONE scratch [`Mesh`] asset modified every
/// frame, and nothing else.
///
/// **The tax meter for [`bevy::asset::AssetChanged`].** Bevy 0.18's `AssetChanged<Mesh3d>` fast
/// path is all-or-nothing: one `Assets<Mesh>` modification — of *any* mesh, including a `Mesh2d`
/// UI batch no 3D row will ever reference — arms `mark_3d_meshes_as_changed_if_their_assets_changed`
/// plus one `check_entities_needing_specialization::<M>` walk per registered material type, each
/// over every `Mesh3d` row in the scene. 1361's per-slot skip gate and 1463's pan gate both exist
/// to keep that disarmed, and both are gates on the *UI's* writes; this measures the price of a
/// single arming with the UI's own work subtracted out, which no gate-side experiment can.
///
/// One run, two regimes, so the reading is a WITHIN-run paired delta: run-to-run cpu variance on
/// this machine is ±1 ms (1157), which is the size of the thing being measured.
pub(super) fn mesh_touch(
    mut meshes: ResMut<Assets<Mesh>>,
    time: Res<bevy::time::Time<bevy::time::Real>>,
    at: Res<MeshTouchAt>,
    mut scratch: Local<Option<Handle<Mesh>>>,
) {
    if time.elapsed_secs() < at.0 {
        return;
    }
    let handle = scratch.get_or_insert_with(|| {
        meshes.add(Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        ))
    });
    // `get_mut` is the whole point: it writes `AssetEvent::Modified` for this id, which bumps the
    // global changed tick every `AssetChanged` filter reads. The mesh itself stays empty.
    let _ = meshes.get_mut(&*handle);
}

/// When [`mesh_touch`] starts touching (seconds of `Time<Real>`).
#[derive(Resource)]
pub(super) struct MeshTouchAt(pub f32);
