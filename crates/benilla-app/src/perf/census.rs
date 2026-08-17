//! Env-gated one-shot and per-second counters: the premise-checkers a sizing question needs before
//! anyone designs a fix for it.

use bevy::prelude::*;

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
