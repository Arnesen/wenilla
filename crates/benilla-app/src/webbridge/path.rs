//! A walking path through the loaded collision world, for the bridge's `path` verb.
//!
//! The client has no navigation mesh (the 1.12 client never pathed; the server's mmaps are the
//! server's), and Click to Move walks straight lines, so a page that wants to get into a building
//! or around a fence asks for a path first and walks its waypoints. This is an A* over a grid of
//! one-yard cells in the horizontal plane, built lazily from the same queries the mover makes:
//!
//! - a cell's floor is a one-sided ray down onto the body's world (terrain, WMO walk faces,
//!   doodad hulls), started a little above the neighbour's floor so a ceiling overhead is not
//!   mistaken for it and the floor of the storey we are on is the one found;
//! - an edge is walkable when the floor step is within [`MAX_RISE`] up or [`MAX_DROP`] down, and
//!   a body-sized capsule, its foot above step height, sweeps from one cell to the next without
//!   a hit (a wall, a fence, a tree trunk, a closed door).
//!
//! Then the path is pulled tight: a waypoint is dropped when the straight walk from the previous
//! one is clear and its floor continuous. Only what is loaded around the player can be searched —
//! collision streams with the terrain — so the search is bounded ([`MAX_EXPANSIONS`]) and, when
//! the goal is out of reach of that, answers the path to the nearest point it found (`partial`).

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use avian3d::prelude::Collider;
use bevy::math::{Dir3, Vec3};

use benilla_world::collision::WorldCollision;

/// Grid pitch, yards.
const CELL: f32 = 1.0;
/// Highest floor step up an edge may take (the mover's step-up height, a little slope on top).
const MAX_RISE: f32 = 1.1;
/// Deepest floor step down an edge may take: a ledge the body would drop off without harm, not a
/// cliff it would take damage from or not climb back.
const MAX_DROP: f32 = 2.0;
/// The floor ray starts this far above the reference floor and reaches this far below it.
const RAY_UP: f32 = 1.6;
const RAY_DOWN: f32 = 4.0;
/// The swept body: a capsule this wide whose lowest point clears step height, up to head height.
const BODY_RADIUS: f32 = 0.35;
const BODY_FOOT: f32 = 0.6;
const BODY_HEAD: f32 = 1.9;
/// Search budget: cells expanded before the search gives up and answers the nearest point.
pub(super) const MAX_EXPANSIONS: usize = 12_000;
/// Arrival: within this many yards of the goal (horizontally) and this of its height.
const GOAL_RADIUS: f32 = 1.5;
const GOAL_HEIGHT: f32 = 3.0;

/// A found path: Bevy-space floor points from the start to the goal (or the nearest point).
pub(super) struct Path {
    pub(super) points: Vec<Vec3>,
    pub(super) partial: bool,
    pub(super) expanded: usize,
}

/// The collision queries the search makes; a trait so the search is tested on a synthetic world.
pub(super) trait Probe {
    /// The floor under `(x, z)` near height `y`.
    fn floor(&self, x: f32, z: f32, y: f32) -> Option<f32>;
    /// Can a body walk from floor point `a` to floor point `b` without hitting anything?
    fn clear(&self, a: Vec3, b: Vec3) -> bool;
}

/// The live world: [`WorldCollision`]'s body queries.
pub(super) struct WorldProbe<'a, 'w, 's> {
    pub(super) world: &'a WorldCollision<'w, 's>,
    pub(super) body: Collider,
}

impl<'a, 'w, 's> WorldProbe<'a, 'w, 's> {
    pub(super) fn new(world: &'a WorldCollision<'w, 's>) -> Self {
        let half = (BODY_HEAD - BODY_FOOT) * 0.5 - BODY_RADIUS;
        Self {
            world,
            body: Collider::capsule(BODY_RADIUS, 2.0 * half.max(0.05)),
        }
    }
}

impl Probe for WorldProbe<'_, '_, '_> {
    fn floor(&self, x: f32, z: f32, y: f32) -> Option<f32> {
        let origin = Vec3::new(x, y + RAY_UP, z);
        self.world
            .ray_body(origin, Dir3::NEG_Y, RAY_UP + RAY_DOWN)
            .map(|hit| origin.y - hit.distance)
    }

    fn clear(&self, a: Vec3, b: Vec3) -> bool {
        let lift = Vec3::Y * (BODY_FOOT + BODY_HEAD) * 0.5;
        // The sweep climbs with the floor: from above `a`'s floor to above `b`'s.
        self.world
            .cast_body(&self.body, a + lift, b - a, 0.01)
            .is_none()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Key {
    x: i32,
    z: i32,
    /// The storey: floors more than a step apart over one cell are different nodes.
    level: i32,
}

#[derive(Clone, Copy)]
struct Node {
    pos: Vec3,
    g: f32,
    parent: Option<Key>,
}

#[derive(PartialEq)]
struct Open {
    f: f32,
    key: Key,
}
impl Eq for Open {}
impl Ord for Open {
    fn cmp(&self, other: &Self) -> Ordering {
        other.f.total_cmp(&self.f) // a min-heap on f
    }
}
impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn key_of(p: Vec3) -> Key {
    Key {
        x: (p.x / CELL).round() as i32,
        z: (p.z / CELL).round() as i32,
        level: (p.y / 2.5).round() as i32,
    }
}

fn flat_dist(a: Vec3, b: Vec3) -> f32 {
    Vec3::new(a.x - b.x, 0.0, a.z - b.z).length()
}

/// Search from `from` to `to` (Bevy space, feet). `None` when the start has no floor.
pub(super) fn find(probe: &impl Probe, from: Vec3, to: Vec3, budget: usize) -> Option<Path> {
    let start_y = probe.floor(from.x, from.z, from.y)?;
    let start = Vec3::new(from.x, start_y, from.z);
    let goal = to;
    let mut nodes: HashMap<Key, Node> = HashMap::new();
    let mut open = BinaryHeap::new();
    let start_key = key_of(start);
    nodes.insert(start_key, Node { pos: start, g: 0.0, parent: None });
    open.push(Open { f: flat_dist(start, goal), key: start_key });
    let mut best = (flat_dist(start, goal), start_key);
    let mut closed = std::collections::HashSet::new();
    let mut expanded = 0;
    let mut reached = None;

    while let Some(Open { key, .. }) = open.pop() {
        if !closed.insert(key) {
            continue;
        }
        let node = nodes[&key];
        let h = flat_dist(node.pos, goal);
        if h < best.0 {
            best = (h, key);
        }
        if h <= GOAL_RADIUS && (node.pos.y - goal.y).abs() <= GOAL_HEIGHT {
            reached = Some(key);
            break;
        }
        expanded += 1;
        if expanded >= budget {
            break;
        }
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let x = (key.x + dx) as f32 * CELL;
            let z = (key.z + dz) as f32 * CELL;
            let Some(y) = probe.floor(x, z, node.pos.y) else {
                continue;
            };
            let rise = y - node.pos.y;
            if rise > MAX_RISE || rise < -MAX_DROP {
                continue;
            }
            let pos = Vec3::new(x, y, z);
            let nkey = key_of(pos);
            if closed.contains(&nkey) {
                continue;
            }
            let g = node.g + node.pos.distance(pos);
            if nodes.get(&nkey).is_some_and(|n| n.g <= g) {
                continue;
            }
            if !probe.clear(node.pos, pos) {
                continue;
            }
            nodes.insert(nkey, Node { pos, g, parent: Some(key) });
            open.push(Open { f: g + flat_dist(pos, goal), key: nkey });
        }
    }

    let end = reached.unwrap_or(best.1);
    let mut points = Vec::new();
    let mut k = Some(end);
    while let Some(key) = k {
        let n = nodes[&key];
        points.push(n.pos);
        k = n.parent;
    }
    points.reverse();
    points[0] = start;
    if reached.is_some() {
        // The last cell centre stands within a yard of the goal; finish on the goal itself when
        // that last step is walkable.
        let last = *points.last().expect("the start");
        if let Some(y) = probe.floor(goal.x, goal.z, last.y) {
            let g = Vec3::new(goal.x, y, goal.z);
            if probe.clear(last, g) {
                points.push(g);
            }
        }
    }
    Some(Path {
        points: smooth(probe, &points),
        partial: reached.is_none(),
        expanded,
    })
}

/// Can the body walk the straight line `a`→`b`: clear of obstacles, and with a floor under every
/// yard of it that never steps more than an edge may?
fn straight(probe: &impl Probe, a: Vec3, b: Vec3) -> bool {
    let n = (flat_dist(a, b) / CELL).ceil().max(1.0) as usize;
    let mut prev = a;
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let p = a.lerp(b, t);
        let Some(y) = probe.floor(p.x, p.z, prev.y) else {
            return false;
        };
        let rise = y - prev.y;
        if rise > MAX_RISE || rise < -MAX_DROP {
            return false;
        }
        let here = Vec3::new(p.x, y, p.z);
        if !probe.clear(prev, here) {
            return false;
        }
        prev = here;
    }
    true
}

/// String-pull: from each kept point, jump to the farthest later point walkable in a straight line.
fn smooth(probe: &impl Probe, points: &[Vec3]) -> Vec<Vec3> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut out = vec![points[0]];
    let mut i = 0;
    while i < points.len() - 1 {
        let mut j = points.len() - 1;
        while j > i + 1 && !straight(probe, points[i], points[j]) {
            j -= 1;
        }
        out.push(points[j]);
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat floor at y = 0 with a wall along x = 5 from z = -10 to z = 10.
    struct Walled;
    impl Probe for Walled {
        fn floor(&self, _x: f32, _z: f32, _y: f32) -> Option<f32> {
            Some(0.0)
        }
        fn clear(&self, a: Vec3, b: Vec3) -> bool {
            // Does the segment cross x = 5 within the wall's extent (with the body's radius)?
            let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
            if lo > 5.0 + BODY_RADIUS || hi < 5.0 - BODY_RADIUS {
                return true;
            }
            let t = if (b.x - a.x).abs() < 1e-6 { 0.0 } else { (5.0 - a.x) / (b.x - a.x) };
            let z = a.z + (b.z - a.z) * t.clamp(0.0, 1.0);
            !(-10.0 - BODY_RADIUS..=10.0 + BODY_RADIUS).contains(&z)
        }
    }

    #[test]
    fn a_path_goes_around_a_wall_and_is_pulled_tight() {
        let p = find(&Walled, Vec3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 0.0, 0.0), MAX_EXPANSIONS)
            .expect("a floor");
        assert!(!p.partial, "the far side is reachable");
        assert!(p.points.len() >= 3, "it bends round the wall: {:?}", p.points);
        assert!(p.points.len() <= 5, "and is pulled tight: {:?}", p.points);
        for w in p.points.windows(2) {
            assert!(Walled.clear(w[0], w[1]), "every leg is walkable: {w:?}");
        }
        let end = *p.points.last().unwrap();
        assert!(flat_dist(end, Vec3::new(10.0, 0.0, 0.0)) <= GOAL_RADIUS);
    }

    #[test]
    fn an_open_line_is_one_leg() {
        let p = find(&Walled, Vec3::new(0.0, 0.0, 20.0), Vec3::new(10.0, 0.0, 20.0), MAX_EXPANSIONS)
            .unwrap();
        assert_eq!(p.points.len(), 2);
    }

    /// A pit: no floor within x in 3..7.
    struct Pit;
    impl Probe for Pit {
        fn floor(&self, x: f32, _z: f32, _y: f32) -> Option<f32> {
            (!(3.0..=7.0).contains(&x)).then_some(0.0)
        }
        fn clear(&self, _a: Vec3, _b: Vec3) -> bool {
            true
        }
    }

    #[test]
    fn an_unreachable_goal_answers_the_nearest_point() {
        let p = find(&Pit, Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), 400).unwrap();
        assert!(p.partial);
        let end = *p.points.last().unwrap();
        assert!(end.x <= 3.0 && end.x >= 1.5, "stops at the pit's edge: {end:?}");
    }
}
