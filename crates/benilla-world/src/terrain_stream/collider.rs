//! Off-thread static-collider builds for streamed geometry (terrain tiles, doodads, WMOs,
//! props). parry's trimesh/QBVH construction is the hundreds-of-ms cost that hitched the frame
//! on stream-in, so the build runs on the async compute pool ([`build_collider_task`]) and
//! [`finish_colliders`] attaches each result under a per-frame budget.
//!
//! The build being off-thread is only half the story: *attaching* the finished shape is main-thread
//! structural work (an archetype move plus avian's required components and its on-insert
//! mass-property hook), and the async pool finishes a streamed burst together — so a tile boundary
//! used to attach the whole burst in a single frame. Decision 0610 measured that path and caps it.

use std::time::{Duration, Instant};

use avian3d::prelude::{Collider, CollisionLayers, RigidBody};
use benilla_assets::coords::wow_to_bevy;
use benilla_formats::CollisionMesh;
use bevy::ecs::system::SystemState;
use bevy::prelude::*;
use bevy::tasks::futures_lite::future;
use bevy::tasks::{block_on, AsyncComputeTaskPool, Task};

/// Wall-clock spent per frame *attaching* finished colliders before deferring the rest to a later
/// frame — the collider twin of `SPAWN_BUDGET`, and for the same reason. Measured cost of one attach
/// (decision 0610): ~0.004 ms per entity plus ~1.8e-5 ms per triangle, so a burst of a thousand-odd
/// doodads (or a dozen tiles) landing together is the 9.45 ms of main-thread time the traced flight
/// hitch spent here. Deferring is nearly free to *feel*: the streamer loads ahead of the view, so
/// just-streamed static geometry going solid a few frames late is not yet reachable.
const ATTACH_BUDGET: Duration = Duration::from_millis(2);

/// A static collider being built on the async compute pool (see [`build_collider_task`]); once the
/// parry shape is ready, [`finish_colliders`] inserts it (with `layers`, for the WMO walk/camera
/// audiences) onto this entity. Building off-thread keeps a big-WMO / terrain stream-in from hitching
/// the frame.
#[derive(Component)]
pub struct PendingCollider {
    /// The in-flight parry build; taken the frame it completes.
    task: Option<Task<Collider>>,
    /// The finished shape, parked here when [`ATTACH_BUDGET`] deferred its attach to a later frame.
    /// A completed [`Task`] cannot be polled twice, so the shape needs somewhere to live between
    /// "built" and "attached" — this is that somewhere, and it is why no finished build is dropped.
    built: Option<Collider>,
    /// WMO walk / camera collision layer; `None` = the default layer (terrain, doodads, props).
    layers: Option<CollisionLayers>,
    /// Insert `RigidBody::Static` with the collider — every world-anchored placement. `false` = a
    /// bare collider that attaches to its nearest `RigidBody` ancestor: a transport deck prop's
    /// hull riding the boat's kinematic body ([`crate::entities`]' wmo_props). A `Static` body
    /// there would anchor the hull to the world at its spawn pose and the cargo would sail away
    /// from its own collision.
    static_body: bool,
}

impl PendingCollider {
    /// A collider build in flight on the async compute pool.
    pub fn new(task: Task<Collider>, layers: Option<CollisionLayers>, static_body: bool) -> Self {
        Self {
            task: Some(task),
            built: None,
            layers,
            static_body,
        }
    }
}

/// Attach off-thread-built colliders to their entities, at most [`ATTACH_BUDGET`] worth per frame.
///
/// Exclusive on purpose. The previous shape queued each attach as a deferred command, which put the
/// whole burst into one `system_commands` block with nothing able to measure or stop it; owning the
/// `&mut World` is what makes a deadline enforceable at all. Polling stays cheap — the traced cost of
/// the poll pass over the entire pending set was ~0.005 ms — so the budget guards only the inserts.
pub(super) fn finish_colliders(
    world: &mut World,
    state: &mut SystemState<Query<'static, 'static, (Entity, &mut PendingCollider)>>,
) {
    // Pass 1 — poll in-flight builds. A completed task hands its shape to `built`, so running out of
    // budget below can never drop a finished collider on the floor.
    //
    // Capped: a cold zone load queues *tens of thousands* of pending colliders (10 500 at once
    // entering Stormwind), and the budget below can attach at most ~500 of the cheapest ones in a
    // frame. Collecting every ready entity would rebuild a huge list each frame to use a slice of
    // it. `READY_SCAN_CAP` bounds the ready list while staying far above what any frame can spend;
    // the rest are picked up next frame (they stay in the query until attached, so none is skipped).
    // The walk itself covers the whole set regardless (it is the ~0.005 ms part) — it also yields
    // the queue-depth publish below.
    const READY_SCAN_CAP: usize = 2048;
    let t0 = Instant::now();
    let mut ready: Vec<Entity> = Vec::new();
    let mut pending = 0usize;
    for (entity, mut pc) in state.get_mut(world).iter_mut() {
        pending += 1;
        if let Some(task) = pc.task.as_mut() {
            if let Some(collider) = block_on(future::poll_once(task)) {
                pc.task = None;
                pc.built = Some(collider);
            }
        }
        if pc.built.is_some() && ready.len() < READY_SCAN_CAP {
            ready.push(entity);
        }
    }
    // Publish the queue depth (decision 0737): the loading-screen clear and the settle release
    // both refuse to call the world presentable while attaches are outstanding — a spawned
    // building whose collider still sits in this queue is exactly what a body must not be
    // released onto. This system heads the Stream chain (0738), so the consumers read this
    // frame's depth; the count is the depth entering the frame (attaches below shrink it next
    // publish), which can only delay a release, never wrong one.
    if let Some(mut progress) = world.get_resource_mut::<crate::terrain_stream::WorldLoadProgress>()
    {
        progress.colliders_pending = pending;
    }
    if ready.is_empty() {
        // `get_`: the unit tests below drive this system on a minimal App without the perf layer.
        if let Some(mut activity) =
            world.get_resource_mut::<crate::terrain_stream::StreamActivity>()
        {
            activity.collider_ms += t0.elapsed().as_secs_f32() * 1000.0;
        }
        return;
    }

    // Pass 2 — the main-thread cost. The budget is checked *after* each attach so one oversized
    // collider still makes progress rather than stalling the queue forever (the rule
    // `stream_terrain` already applies to its tile spawns).
    let mut attached = 0u32;
    let deadline = Instant::now() + ATTACH_BUDGET;
    for entity in ready {
        // The entity may have streamed out (despawned) while its collider was building, or had its
        // slot reused — a no-op rather than a panic.
        let Ok(mut entity_mut) = world.get_entity_mut(entity) else {
            continue;
        };
        let Some(mut pc) = entity_mut.take::<PendingCollider>() else {
            continue;
        };
        let Some(collider) = pc.built.take() else {
            continue;
        };
        entity_mut.insert(collider);
        if pc.static_body {
            entity_mut.insert(RigidBody::Static);
        }
        if let Some(layers) = pc.layers {
            entity_mut.insert(layers);
        }
        attached += 1;
        if Instant::now() >= deadline {
            break;
        }
    }
    if let Some(mut activity) = world.get_resource_mut::<crate::terrain_stream::StreamActivity>() {
        activity.colliders_attached += attached;
        activity.collider_ms += t0.elapsed().as_secs_f32() * 1000.0;
    }
}

/// The world-space `(vertices, triangles)` for a model's static collider — its raw-WoW collision hull
/// mapped through `wow_to_bevy` then the placement [`Transform`] (so the collider coincides with the
/// drawn model). `None` for a hull-less model or an empty mesh. The (potentially expensive) parry build
/// is deferred off-thread via [`build_collider_task`].
pub fn placement_collider_data(
    hull: Option<&CollisionMesh>,
    transform: &Transform,
) -> Option<(Vec<Vec3>, Vec<[u32; 3]>)> {
    let hull = hull?;
    if hull.indices.len() < 3 {
        return None;
    }
    let verts: Vec<Vec3> = hull
        .positions
        .iter()
        .map(|p| transform.transform_point(wow_to_bevy(*p)))
        .collect();
    let tris: Vec<[u32; 3]> = hull
        .indices
        .chunks_exact(3)
        .map(|c| [c[0], c[1], c[2]])
        .collect();
    Some((verts, tris))
}

/// The `(vertices, triangles)` for a terrain tile's static collider: **one** trimesh welded from the
/// tile's decoded MCNK chunks, in the same world space they are drawn in (so you stand on the visible
/// ground). `None` for a tile with no triangles at all.
///
/// Built from the decoded chunks rather than the render meshes because the tile no longer *has* one
/// merged mesh to read (decision 0780) — and one collider per 33 yd cell would be 256 parry builds
/// and 256 QBVHs where the ground is a single continuous surface. The mapping is the loader's,
/// verbatim: `wow_to_bevy` per position, hole-masked indices rebased onto the running vertex count.
pub(super) fn terrain_collider_data(
    chunks: &[benilla_formats::ChunkMesh],
) -> Option<(Vec<Vec3>, Vec<[u32; 3]>)> {
    let mut verts: Vec<Vec3> = Vec::new();
    let mut tris: Vec<[u32; 3]> = Vec::new();
    for chunk in chunks {
        let base = verts.len() as u32;
        verts.extend(chunk.positions.iter().map(|p| wow_to_bevy(*p)));
        tris.extend(
            chunk
                .indices
                .chunks_exact(3)
                .map(|c| [base + c[0], base + c[1], base + c[2]]),
        );
    }
    (!tris.is_empty()).then_some((verts, tris))
}

/// Build a static-trimesh [`Collider`] on the **async compute pool** (parry's QBVH construction is the
/// hundreds-of-ms cost that hitched the frame on big-WMO / terrain stream-in). [`finish_colliders`]
/// attaches the result. A few-frame delay in collision for just-streamed *static* geometry is
/// imperceptible — the streamer loads ahead of the view, so you never reach it before it's solid.
pub fn build_collider_task(verts: Vec<Vec3>, tris: Vec<[u32; 3]>) -> Task<Collider> {
    AsyncComputeTaskPool::get().spawn(async move { Collider::trimesh(verts, tris) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use avian3d::prelude::PhysicsPlugins;
    use bevy::app::TaskPoolPlugin;
    use bevy::ecs::system::RunSystemOnce;

    /// A tile-sized grid: 256 quads -> 512 triangles per chunk, `chunks` chunks.
    fn grid(chunks: usize) -> (Vec<Vec3>, Vec<[u32; 3]>) {
        let (mut verts, mut tris) = (Vec::new(), Vec::new());
        for c in 0..chunks {
            let base = verts.len() as u32;
            for r in 0..17u32 {
                for q in 0..17u32 {
                    verts.push(Vec3::new(
                        q as f32,
                        ((q + r + c as u32) % 5) as f32,
                        r as f32,
                    ));
                }
            }
            for r in 0..16u32 {
                for q in 0..16u32 {
                    let i = base + r * 17 + q;
                    tris.push([i, i + 1, i + 17]);
                    tris.push([i + 1, i + 18, i + 17]);
                }
            }
        }
        (verts, tris)
    }

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins((
            TaskPoolPlugin::default(),
            bevy::asset::AssetPlugin::default(),
        ));
        app.init_asset::<Mesh>();
        // Registers avian's collider hooks + required components, so an attach here costs what it
        // costs in the client. Its schedules are never run — `run_system_once` drives ours alone.
        app.add_plugins(PhysicsPlugins::default());
        app.finish();
        app.cleanup();
        app
    }

    /// The budget must both bite (a burst is spread over frames) and never lose a finished build.
    /// The second half is the subtle one: a completed `Task` cannot be polled twice, so a deferred
    /// attach has to park its shape rather than leave it in the task.
    #[test]
    fn attach_budget_spreads_a_burst_without_losing_colliders() {
        const N: usize = 40;
        let mut app = test_app();
        let (verts, tris) = grid(16);
        let entities: Vec<Entity> = (0..N)
            .map(|_| {
                let task = build_collider_task(verts.clone(), tris.clone());
                app.world_mut()
                    .spawn((Transform::default(), PendingCollider::new(task, None, true)))
                    .id()
            })
            .collect();

        // Let the pool finish every build, so the whole burst is ready at once — the tile-boundary
        // case the budget exists for.
        std::thread::sleep(std::time::Duration::from_millis(500));

        let attached = |app: &mut App| {
            entities
                .iter()
                .filter(|e| app.world().entity(**e).contains::<Collider>())
                .count()
        };

        app.world_mut().run_system_once(finish_colliders).unwrap();
        let after_one = attached(&mut app);
        assert!(after_one > 0, "budget starved the queue: nothing attached");
        assert!(
            after_one < N,
            "budget never bit: all {N} attached in one frame ({after_one})"
        );

        // Drain: every remaining build must still land, none lost to the deferral.
        for _ in 0..N {
            if attached(&mut app) == N {
                break;
            }
            app.world_mut().run_system_once(finish_colliders).unwrap();
        }
        assert_eq!(attached(&mut app), N, "a finished collider was dropped");
        for e in &entities {
            assert!(app.world().entity(*e).contains::<RigidBody>());
            assert!(!app.world().entity(*e).contains::<PendingCollider>());
        }
    }
}
