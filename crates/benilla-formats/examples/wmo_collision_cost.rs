//! What a WMO's **collision** actually costs, and whether its walking gather has a floor at all:
//! `cargo run -p benilla-formats --example wmo_collision_cost -- <wmo-path-or-substring>...`
//! e.g. `wmo_collision_cost stratholme.wmo zulgurubcity`.
//!
//! The falsifier for a **fall-through** report, which has exactly two shapes and they take opposite
//! fixes:
//!
//! * **No floor to stand on** — the walking gather (`flags & 0x04` DETAIL dropped) came back with no
//!   upward-facing faces in a group the camera gather *does* see. Then the collider is present and
//!   correct and still cannot hold you: the authored floor is flagged decal-only. A data fact, fixed
//!   by widening the gather, and visible here without entering the game.
//! * **A floor that arrives too late** — the faces are there, but the placement's trimesh is big
//!   enough that its off-thread build plus the per-frame attach budget runs past the mover's settle
//!   timeout, so gravity switches on over geometry that is not solid yet. A *timing* fact; this
//!   prints the triangle count that drives it, so the margin can be compared against
//!   `SETTLE_TIMEOUT` without guessing which buildings are the heavy ones.
//!
//! Both readings are properties of the shipped file, so read them here rather than inferring them
//! from a fall you may or may not be able to reproduce on your own machine — the timing shape
//! reproduces only on a machine slow enough, which is exactly why it reaches players and not us.
//!
//! Output is Blizzard-derived; pipe it to the scratchpad, never into the repo.

use benilla_formats::{accumulate_wmo_group_camera_collision, accumulate_wmo_group_collision};

/// Faces at least this upward-facing count as a **floor** — the mover's own walkable test
/// (`GROUND_COS`, ~50° from vertical). A group with collidable faces but none of them walkable is
/// a wall/ceiling shell you slide down, not something you can stand on.
const WALKABLE_COS: f32 = 0.64;

/// One gather's shape: how many triangles, and how many of them you could stand on.
struct Gather {
    tris: usize,
    walkable: usize,
    min_z: f32,
    max_z: f32,
}

fn gather(positions: &[[f32; 3]], indices: &[u32]) -> Gather {
    let (mut walkable, mut min_z, mut max_z) = (0usize, f32::MAX, f32::MIN);
    for t in indices.chunks_exact(3) {
        let p: [[f32; 3]; 3] = [
            positions[t[0] as usize],
            positions[t[1] as usize],
            positions[t[2] as usize],
        ];
        let u = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
        let v = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
        // Raw WMO space is Z-up, so the walkable axis is the normal's Z.
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len > 0.0 && (n[2] / len).abs() >= WALKABLE_COS {
            walkable += 1;
        }
        for c in p {
            min_z = min_z.min(c[2]);
            max_z = max_z.max(c[2]);
        }
    }
    Gather {
        tris: indices.len() / 3,
        walkable,
        min_z,
        max_z,
    }
}

fn main() -> anyhow::Result<()> {
    let pats: Vec<String> = std::env::args().skip(1).collect();
    if pats.is_empty() {
        anyhow::bail!("usage: wmo_collision_cost <wmo-path-or-substring>...");
    }
    let data = benilla_formats::wow_data().expect("no WoW install found (set $WOW_DATA)");
    let mut chain = benilla_formats::open_chain(&data)?;
    let names: Vec<String> = chain.list()?.into_iter().map(|e| e.name).collect();

    for pat in &pats {
        let lower = pat.to_lowercase();
        let path = if lower.ends_with(".wmo") && chain.contains(pat) {
            pat.clone()
        } else {
            let Some(found) = names.iter().find(|n| {
                let l = n.to_lowercase();
                l.ends_with(".wmo")
                    && l.contains(&lower)
                    && !l.rsplit_once('_').is_some_and(|(_, tail)| {
                        tail.len() == 7 && tail.starts_with(|c: char| c.is_ascii_digit())
                    })
            }) else {
                println!("no root .wmo matching {pat:?}\n");
                continue;
            };
            found.clone()
        };

        let bytes = chain.read_file(&path.to_ascii_lowercase())?;
        let n_groups = benilla_formats::parse_wmo_root(&bytes)?.group_count();
        let stem = {
            let l = path.to_ascii_lowercase();
            l.strip_suffix(".wmo").unwrap_or(&l).to_string()
        };

        println!("=== {path} — {n_groups} group(s) ===");
        let (mut tot_walk, mut tot_cam, mut floorless) = (0usize, 0usize, Vec::new());
        for gi in 0..n_groups {
            let Ok(gbytes) = chain.read_file(&format!("{stem}_{gi:03}.wmo")) else {
                continue;
            };
            let (mut wp, mut wi) = (Vec::new(), Vec::new());
            accumulate_wmo_group_collision(&gbytes, &mut wp, &mut wi);
            let (mut cp, mut ci) = (Vec::new(), Vec::new());
            accumulate_wmo_group_camera_collision(&gbytes, &mut cp, &mut ci);
            let (w, c) = (gather(&wp, &wi), gather(&cp, &ci));
            tot_walk += w.tris;
            tot_cam += c.tris;
            // The defect shape: the camera can stand on this group, the player body cannot.
            if w.walkable == 0 && c.walkable > 0 {
                floorless.push(gi);
                println!(
                    "  group {gi:3}: walk {:6} tri ({:5} walkable)   camera {:6} tri ({:5} walkable)  \
                     <-- NO WALKABLE FACE IN THE WALK GATHER (z {:.1}..{:.1})",
                    w.tris, w.walkable, c.tris, c.walkable, c.min_z, c.max_z
                );
            }
        }
        println!(
            "  TOTAL walk {tot_walk} tri, camera {tot_cam} tri  \
             ({} group(s) with a camera-only floor)\n",
            floorless.len()
        );
    }
    Ok(())
}
