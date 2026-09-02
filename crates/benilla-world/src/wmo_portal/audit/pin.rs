//! **The pin probe** — a director's `.go xyz` / `/shot` report, replayed offline through the real
//! flood. The sibling sweeps in [`super`] ask "does the invariant hold anywhere in this building";
//! this asks "what happened at exactly *that* spot", which is the question a bug report actually
//! poses. It reuses the harness's placed subjects and the same [`TraceLog`](super::super::probe)
//! recorder the in-client dump button writes, so there is no second flood to drift.

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use bevy::math::{Mat4, Vec3};

use super::{
    load_subject, Site, BLACKSMITH, DEADMINES, EXTERIOR, FARGODEEP, GOLDSHIRE, IRONFORGE,
    SHADOWFANG, UNDERCITY,
};

/// The named subjects `WOW_PIN_SITE` selects — every [`Site`] the harness carries, so a report at a
/// building we have already stood in is one word rather than four env vars (whose `WOW_PIN_WMO`
/// backslash quoting is its own trap). Unknown names list themselves.
const NAMED_SITES: &[(&str, Site)] = &[
    ("goldshire", GOLDSHIRE),
    ("fargodeep", FARGODEEP),
    ("deadmines", DEADMINES),
    ("blacksmith", BLACKSMITH),
    ("undercity", UNDERCITY),
    ("ironforge", IRONFORGE),
    ("shadowfang", SHADOWFANG),
];

/// The pin family's shared env targeting: `WOW_PIN_SITE=<name>` picks a [`NAMED_SITES`] subject
/// (default [`UNDERCITY`]), and `WOW_PIN_WMO`/`_MAP`/`_TILE`/`_UID` each override one field of it —
/// so an unnamed building still works, and a named one needs no quoting.
fn site_from_env() -> Site {
    let base = match std::env::var("WOW_PIN_SITE") {
        Ok(name) => {
            let key = name.trim().to_ascii_lowercase();
            *NAMED_SITES
                .iter()
                .find(|(n, _)| *n == key)
                .map(|(_, s)| s)
                .unwrap_or_else(|| {
                    let known: Vec<&str> = NAMED_SITES.iter().map(|(n, _)| *n).collect();
                    panic!("WOW_PIN_SITE={name:?} is not one of {known:?}")
                })
        }
        Err(_) => UNDERCITY,
    };
    Site {
        wmo: Box::leak(
            std::env::var("WOW_PIN_WMO")
                .unwrap_or_else(|_| base.wmo.to_string())
                .into_boxed_str(),
        ),
        map: Box::leak(
            std::env::var("WOW_PIN_MAP")
                .unwrap_or_else(|_| base.map.to_string())
                .into_boxed_str(),
        ),
        tile: std::env::var("WOW_PIN_TILE").map_or(base.tile, |s| {
            let (a, b) = s.split_once(',').expect("WOW_PIN_TILE wants tx,ty");
            (a.trim().parse().unwrap(), b.trim().parse().unwrap())
        }),
        uid: std::env::var("WOW_PIN_UID").map_or(base.uid, |s| s.trim().parse().unwrap()),
    }
}

/// **The group census** — the pin probe's gazetteer: every group's flags, portal-edge count, and
/// bbox in BOTH frames (model-local and WoW world, via the same placement transform the pin uses),
/// so a "which group is that thing on my screen" question becomes a grep instead of a guess. Same
/// `WOW_PIN_*` targeting as the pin probe.
#[test]
#[ignore = "needs the local game data (WoW/Data); run with --ignored"]
fn wmo_group_census() {
    let site = site_from_env();
    let subject = load_subject(site.wmo, Some(&site));
    let placed = subject
        .placed
        .as_ref()
        .expect("the census needs a placement");
    // The MLIQ leg the harness's nav skips: re-read each group file for its liquid grid, so the
    // census can say "this group owns the lava" (local-space z range of its wet vertices).
    let data = benilla_formats::wow_data().expect("no WoW install found (set $WOW_DATA)");
    let chain = benilla_formats::open_chain(&data).expect("open MPQ chain (set WOW_DATA)");
    let stem = site.wmo.strip_suffix(".wmo").unwrap_or(site.wmo);
    let liquid: Vec<Option<(f32, f32)>> = (0..subject.model.group_nav.len())
        .map(|gi| {
            let bytes = chain.read(&format!("{stem}_{gi:03}.wmo")).ok()?;
            let mesh = benilla_formats::wmo_group_liquid_mesh(&bytes)?;
            let zs = mesh.positions.iter().map(|p| p[2]);
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for z in zs {
                lo = lo.min(z);
                hi = hi.max(z);
            }
            (lo <= hi).then_some((lo, hi))
        })
        .collect();
    println!(
        "== group census: {} ({} groups) ==",
        site.wmo,
        subject.model.group_nav.len()
    );
    // The root's MFOG table, once. Every group line below names the ≤4 record indices it points
    // at, and the fog a room ends up wearing is one of these — which is the whole diagnosis when a
    // far room reads as a flat wash of one colour (B335: SFK's `0xff28444f` at end 106.9 yd). The
    // colour is decoded exactly as [`super::super::fog`] decodes it (`0xAARRGGBB`).
    for (i, f) in subject.model.fogs.iter().enumerate() {
        println!(
            "mfog f{i}: flags {:#x} pos ({:.1},{:.1},{:.1}) r[{:.2},{:.2}] end {:.1} start×{:.3} = {:.1} rgb({},{},{})",
            f.flags,
            f.pos[0],
            f.pos[1],
            f.pos[2],
            f.radius_inner,
            f.radius_outer,
            f.fog_end,
            f.fog_start_scalar,
            f.fog_end * f.fog_start_scalar,
            (f.color >> 16) & 0xff,
            (f.color >> 8) & 0xff,
            f.color & 0xff,
        );
    }
    // `WOW_PIN_COL=<world-x,world-y>`: every walking-collision face crossing that column, per group,
    // as world z — the down-ray's Leg A candidate list for one spot, without running the flood.
    if let Some((cx, cy)) = std::env::var("WOW_PIN_COL").ok().and_then(|v| {
        let c: Vec<f32> = v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        (c.len() == 2).then(|| (c[0], c[1]))
    }) {
        let local = bevy_to_wow(
            placed
                .local_from_world
                .transform_point3(wow_to_bevy([cx, cy, 0.0])),
        );
        println!(
            "-- column ({cx:.1},{cy:.1}) = local ({:.2},{:.2}) --",
            local[0], local[1]
        );
        for (gi, tris) in subject.model.group_collision_tris.iter().enumerate() {
            for t in tris {
                if let Some(z) = benilla_formats::triangle_z_at(t, local[0], local[1]) {
                    let w = bevy_to_wow(
                        placed
                            .world_from_local
                            .transform_point3(wow_to_bevy([local[0], local[1], z])),
                    );
                    println!("  under g{gi:03}: local z {z:.2} = world z {:.1}", w[2]);
                }
            }
        }
        // The unfiltered contrast: every face in the FILE crossing this column, with its MOPY
        // flags — a face listed here but not above is one our walking gather dropped, which
        // against the client's mask (`0x84`, a strict superset of our `0x04` skip) is a parse
        // divergence, not a filter choice.
        println!("-- raw file faces at the column (all MOPY flags) --");
        for gi in 0..subject.model.group_nav.len() {
            let Ok(gbytes) = chain.read(&format!("{stem}_{gi:03}.wmo")) else {
                continue;
            };
            let Ok(benilla_wmo::ParsedWmo::Group(group)) =
                benilla_wmo::parse_wmo(&mut std::io::Cursor::new(gbytes.as_slice()))
            else {
                continue;
            };
            for (t, mopy) in group.material_info.iter().enumerate() {
                let idx = |k: usize| group.vertex_indices.get(t * 3 + k).copied();
                let (Some(a), Some(b), Some(c)) = (idx(0), idx(1), idx(2)) else {
                    continue;
                };
                let pos = |i: u16| {
                    group
                        .vertex_positions
                        .get(i as usize)
                        .map(|p| [p.x, p.y, p.z])
                };
                let (Some(a), Some(b), Some(c)) = (pos(a), pos(b), pos(c)) else {
                    continue;
                };
                if let Some(z) = benilla_formats::triangle_z_at(&[a, b, c], local[0], local[1]) {
                    println!(
                        "  raw g{gi:03}: local z {z:.2}  mopy {:#04x} material {}",
                        mopy.flags, mopy.material_id
                    );
                }
            }
        }
    }
    // `WOW_PIN_FLOOR=<local-z>`: dump every walking-collision face whose centroid sits within 3 yd
    // of that height, as world x/y — the "where exactly is the walkable metalwork at lava level"
    // map a screenshot-pose reconstruction needs.
    if let Some(floor_z) = std::env::var("WOW_PIN_FLOOR")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
    {
        for (gi, tris) in subject.model.group_collision_tris.iter().enumerate() {
            for t in tris {
                let c = [
                    (t[0][0] + t[1][0] + t[2][0]) / 3.0,
                    (t[0][1] + t[1][1] + t[2][1]) / 3.0,
                    (t[0][2] + t[1][2] + t[2][2]) / 3.0,
                ];
                if (c[2] - floor_z).abs() > 3.0 {
                    continue;
                }
                let w = bevy_to_wow(placed.world_from_local.transform_point3(wow_to_bevy(c)));
                println!(
                    "floor g{gi:03} world ({:.1}, {:.1}, {:.1})",
                    w[0], w[1], w[2]
                );
            }
        }
    }
    for (gi, g) in subject.model.group_nav.iter().enumerate() {
        // World bbox: transform all 8 local corners, take the WoW-frame min/max.
        let mut wmin = [f32::INFINITY; 3];
        let mut wmax = [f32::NEG_INFINITY; 3];
        for c in 0..8 {
            let local = [
                if c & 1 == 0 {
                    g.bbox_min[0]
                } else {
                    g.bbox_max[0]
                },
                if c & 2 == 0 {
                    g.bbox_min[1]
                } else {
                    g.bbox_max[1]
                },
                if c & 4 == 0 {
                    g.bbox_min[2]
                } else {
                    g.bbox_max[2]
                },
            ];
            let w = bevy_to_wow(placed.world_from_local.transform_point3(wow_to_bevy(local)));
            for k in 0..3 {
                wmin[k] = wmin[k].min(w[k]);
                wmax[k] = wmax[k].max(w[k]);
            }
        }
        println!(
            "g{gi:03} flags {:#07x}{} edges {:2}  world x[{:.0},{:.0}] y[{:.0},{:.0}] z[{:.0},{:.0}]  local z[{:.1},{:.1}]  walk {:5} camonly {:4}{}",
            g.flags,
            if g.flags & EXTERIOR != 0 { " EXT" } else { "    " },
            g.ref_count,
            wmin[0],
            wmax[0],
            wmin[1],
            wmax[1],
            wmin[2],
            wmax[2],
            g.bbox_min[2],
            g.bbox_max[2],
            subject
                .model
                .group_collision_tris
                .get(gi)
                .map_or(0, Vec::len),
            subject
                .model
                .group_camera_only_tris
                .get(gi)
                .map_or(0, Vec::len),
            liquid[gi]
                .map(|(lo, hi)| format!("  LIQUID local z[{lo:.1},{hi:.1}]"))
                .unwrap_or_default(),
        );
        // Which MFOG records this group's MOGP header points at, and whether the group is on the
        // INTERIOR fog lane at all (`flags & 0x48 == 0` — the drawer router's own test).
        println!(
            "     fog {:?}{}",
            g.fog_indices,
            if g.flags & 0x48 == 0 {
                "  INTERIOR-LANE"
            } else {
                "  scene-lane (0x48 set)"
            }
        );
        // The group's portal edges, each placed in WoW world space. The group lines above answer
        // "which room is that"; without this the doorway between two of them is a local-space
        // vertex span nobody can point at in game, so a `.go xyz` report can't name the portal it
        // is standing in front of.
        let start = g.ref_start as usize;
        let end = (start + g.ref_count as usize).min(subject.model.portal_refs.len());
        for r in &subject.model.portal_refs[start..end] {
            let Some(info) = subject.model.portal_infos.get(r.portal as usize) else {
                continue;
            };
            let vs = (info.start_vertex as usize)
                ..(info.start_vertex as usize + info.count as usize)
                    .min(subject.model.portal_vertices.len());
            let verts = &subject.model.portal_vertices[vs];
            if verts.is_empty() {
                continue;
            }
            let mut c = [0.0f32; 3];
            for v in verts {
                for k in 0..3 {
                    c[k] += v[k] / verts.len() as f32;
                }
            }
            let w = bevy_to_wow(placed.world_from_local.transform_point3(wow_to_bevy(c)));
            // Widest chord across the polygon (yd) — a doorway's opening, so "did that hop really
            // have a 3-yd gap to see through" is answered without re-projecting by hand.
            let span = verts
                .iter()
                .flat_map(|a| {
                    verts.iter().map(move |b| {
                        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2))
                            .sqrt()
                    })
                })
                .fold(0.0f32, f32::max);
            println!(
                "     p{:<3} ->g{:03}(side {:+}) world ({:.1},{:.1},{:.1}) span {:.1} yd",
                r.portal, r.group, r.side, w[0], w[1], w[2], span,
            );
        }
    }
}

/// **The pin probe** — a director's `.go xyz` / `/shot` report, replayed through the real flood.
///
/// A bug report names a *place*, and until now turning that place into evidence meant launching the
/// client, walking there, and clicking the panel's dump button. This runs the same flood offline: give
/// it the WoW-world eye and look point the report carries and it prints the down-ray's seed evidence,
/// every portal hop's verdict, and the resulting visible set — the fixture a diagnosis starts from.
///
/// ```text
/// WOW_PIN_EYE=1565.2,417.1,-56.2 WOW_PIN_LOOK=1517.5,406.7,-67.1 \
///   cargo test -p benilla wmo_pin_probe -- --ignored --nocapture
/// ```
///
/// The subject defaults to [`UNDERCITY`] (B26); `WOW_PIN_WMO` + `WOW_PIN_UID` + `WOW_PIN_MAP` +
/// `WOW_PIN_TILE` retarget it at another placement. Output is Blizzard-derived — keep it out of the repo.
#[test]
#[ignore = "needs the local game data (WoW/Data); run with --ignored"]
fn wmo_pin_probe() {
    fn xyz(var: &str, default: [f32; 3]) -> [f32; 3] {
        let Ok(s) = std::env::var(var) else {
            return default;
        };
        let v: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        <[f32; 3]>::try_from(&v[..]).unwrap_or_else(|_| panic!("{var} wants x,y,z; got {s:?}"))
    }
    let site = site_from_env();
    let subject = load_subject(site.wmo, Some(&site));
    let placed = subject
        .placed
        .as_ref()
        .expect("the pin probe needs a placement");

    // The report's coordinates are WoW world space; the flood works in the placement's model space.
    let eye_world_wow = xyz("WOW_PIN_EYE", [1565.2, 417.1, -56.2]);
    let look_world_wow = xyz("WOW_PIN_LOOK", [1517.5, 406.7, -67.1]);
    let to_local =
        |wow: [f32; 3]| bevy_to_wow(placed.local_from_world.transform_point3(wow_to_bevy(wow)));
    let eye = to_local(eye_world_wow);
    let look = to_local(look_world_wow);

    // The camera exactly as the per-frame pass builds it: clip_from_world in Bevy world space, with
    // the real placement transform, so portal projection sees what the runtime sees.
    let eye_bevy = placed.world_from_local.transform_point3(wow_to_bevy(eye));
    let look_bevy = placed.world_from_local.transform_point3(wow_to_bevy(look));
    // The app's real projection by default (`CAM_FOVY`; near/far don't matter — the flood clips
    // against the 4 side planes only). `WOW_PIN_FOVY`/`WOW_PIN_ASPECT` override both to replay a
    // dump header's recorded values exactly (a window is rarely 16:9).
    let scalar = |var: &str, default: f32| {
        std::env::var(var).map_or(default, |s| {
            s.trim()
                .parse()
                .unwrap_or_else(|_| panic!("{var} wants a number; got {s:?}"))
        })
    };
    let fovy = scalar("WOW_PIN_FOVY", crate::view::CAM_FOVY);
    let aspect = scalar("WOW_PIN_ASPECT", 16.0 / 9.0);
    let clip = Mat4::perspective_rh(fovy, aspect, 0.1, 1000.0)
        * Mat4::look_at_rh(eye_bevy, look_bevy, Vec3::Y);

    let model = &subject.model;
    println!(
        "== pin probe: {} ({} groups, {} portals) ==\n\
         eye  world ({:.2},{:.2},{:.2}) -> local ({:.2},{:.2},{:.2})\n\
         look world ({:.2},{:.2},{:.2}) -> local ({:.2},{:.2},{:.2})",
        site.wmo,
        model.group_nav.len(),
        model.portal_infos.len(),
        eye_world_wow[0],
        eye_world_wow[1],
        eye_world_wow[2],
        eye[0],
        eye[1],
        eye[2],
        look_world_wow[0],
        look_world_wow[1],
        look_world_wow[2],
        look[0],
        look[1],
        look[2],
    );

    let terrain = subject.terrain_z(eye);
    let mut log = crate::wmo_portal::probe::TraceLog::new(model, eye, terrain);
    let pvs = crate::wmo_portal::compute_pvs_traced(
        model,
        eye,
        terrain,
        &clip,
        &placed.world_from_local,
        &mut log,
    );
    // The trace's per-group preamble is 200+ lines on a city — keep only the hop verdicts and the
    // seed evidence, which is what a "why is that room gone" question actually reads.
    for line in log.text.lines() {
        if !line.trim_start().starts_with('g') || line.contains("->") {
            println!("{line}");
        }
    }

    let vis: Vec<usize> = pvs
        .visible
        .iter()
        .enumerate()
        .filter(|(_, &v)| v)
        .map(|(i, _)| i)
        .collect();
    println!(
        "visible: {} of {} groups {vis:?}",
        vis.len(),
        pvs.visible.len()
    );
    // The interior-fog gate (`[0xca7f00]`, [`super::super::GroupPvs`]): which of those groups wear
    // the building's own MFOG triple, and which inherit the scene fog. B335 is read straight off
    // this line — the room behind the courtyard's arches must NOT be on it.
    let fogged: Vec<usize> = pvs
        .interior_fog
        .iter()
        .enumerate()
        .filter(|(_, &v)| v)
        .map(|(i, _)| i)
        .collect();
    println!("interior-fog lane: {fogged:?}");

    // The portal graph of every group the flood reached. A flood that stops has either run out of
    // edges or had them all rejected, and only the edge list tells the two apart.
    println!("-- portal graph of the visible set --");
    for &gi in &vis {
        let g = &model.group_nav[gi];
        let start = g.ref_start as usize;
        let end = (start + g.ref_count as usize).min(model.portal_refs.len());
        let edges: Vec<String> = model.portal_refs[start..end]
            .iter()
            .map(|r| format!("p{}->g{}(side {:+})", r.portal, r.group, r.side))
            .collect();
        println!(
            "  g{gi:02} flags {:#07x}{} bbox z[{:.1},{:.1}] refs[{}..+{}] {}",
            g.flags,
            if g.flags & EXTERIOR != 0 { " EXT" } else { "" },
            g.bbox_min[2],
            g.bbox_max[2],
            g.ref_start,
            g.ref_count,
            if edges.is_empty() {
                "NO EDGES — the flood dead-ends here".to_string()
            } else {
                edges.join(" ")
            }
        );
    }

    // The portal geometry behind every hop verdict above: plane + vertex extent, for each portal
    // referenced by a group that contains the eye or the look point — the "which side is the eye
    // actually on" question needs the plane, not the verdict.
    println!("-- portals of the groups containing eye/look --");
    for (gi, g) in model.group_nav.iter().enumerate() {
        let holds = |p: [f32; 3]| (0..3).all(|k| p[k] >= g.bbox_min[k] && p[k] <= g.bbox_max[k]);
        if !holds(eye) && !holds(look) {
            continue;
        }
        let start = g.ref_start as usize;
        let end = (start + g.ref_count as usize).min(model.portal_refs.len());
        for r in &model.portal_refs[start..end] {
            let Some(info) = model.portal_infos.get(r.portal as usize) else {
                continue;
            };
            let [nx, ny, nz, d] = info.plane;
            let de = nx * eye[0] + ny * eye[1] + nz * eye[2] + d;
            let vs = (info.start_vertex as usize)
                ..(info.start_vertex as usize + info.count as usize)
                    .min(model.portal_vertices.len());
            let mut vmin = [f32::INFINITY; 3];
            let mut vmax = [f32::NEG_INFINITY; 3];
            for v in &model.portal_vertices[vs] {
                for k in 0..3 {
                    vmin[k] = vmin[k].min(v[k]);
                    vmax[k] = vmax[k].max(v[k]);
                }
            }
            println!(
                "  g{gi:02} p{} ->g{}(side {:+}) plane n({:.3},{:.3},{:.3}) d {:.2} | d(eye) {:+.3} | verts x[{:.1},{:.1}] y[{:.1},{:.1}] z[{:.1},{:.1}]",
                r.portal, r.group, r.side, nx, ny, nz, d, de,
                vmin[0], vmax[0], vmin[1], vmax[1], vmin[2], vmax[2],
            );
        }
    }

    // Which room is the director looking *at*? Report every group whose MOGI bbox contains the look
    // point, with its verdict — the culled one there is the bug.
    println!("-- groups containing the look point --");
    for (gi, g) in model.group_nav.iter().enumerate() {
        let inside = (0..3).all(|k| look[k] >= g.bbox_min[k] && look[k] <= g.bbox_max[k]);
        if inside {
            println!(
                "  g{gi:02} flags {:#07x}{} refs[{}..+{}]  -> {}",
                g.flags,
                if g.flags & EXTERIOR != 0 { " EXT" } else { "" },
                g.ref_start,
                g.ref_count,
                match (pvs.visible[gi], pvs.interior_fog[gi]) {
                    (true, true) => "VISIBLE · interior fog",
                    (true, false) => "VISIBLE · scene fog",
                    (false, _) => "CULLED",
                }
            );
        }
    }
}

/// **B335's fixture** — the Shadowfang courtyard's far doorways, as an invariant instead of a
/// screenshot. The reported spot (`.go xyz -224.01 2168.63 79.79 33`, third-person camera behind)
/// stands in the open courtyard **g38**, whose MOGP flags carry `0x40` EXTERIOR_LIT; the two arches
/// at the back of it open (through the second courtyard **g72**, also `0x40`) into room **g61**, a
/// true interior. The report was that g61 fills with a flat blue-cyan at distance: that colour is
/// this building's own MFOG record — `rgb(40,68,79)`, end 106.9 yd, start 10.7 — which at 70 yd is
/// nearly saturated, while the reference shows the room under the scene fog (map 33 noon: end 333,
/// start 83 ⇒ no fog at all at that range).
///
/// So the assertion is not about visibility — the flood reaches g61 from anywhere in the courtyard,
/// and always did. It is about which fog lane it lands on: the seed's own group takes the interior
/// lane, and the chain breaks below the exterior-lit courtyards, so **g61 must be on the scene
/// lane**. The control is the seed itself, which must stay on the interior lane.
#[test]
#[ignore = "needs the local game data (WoW/Data); run with --ignored"]
fn shadowfang_courtyard_leaves_the_far_room_on_the_scene_fog() {
    let site = SHADOWFANG;
    let subject = load_subject(site.wmo, Some(&site));
    let placed = subject
        .placed
        .as_ref()
        .expect("the fixture needs a placement");
    let to_local =
        |wow: [f32; 3]| bevy_to_wow(placed.local_from_world.transform_point3(wow_to_bevy(wow)));
    // The reported camera: third-person behind the player at the fountain, aimed at the p50 arches.
    let eye_world = [-227.1_f32, 2157.0, 83.7];
    let look_world = [-212.5_f32, 2232.3, 84.2];
    let eye = to_local(eye_world);
    let eye_bevy = placed.world_from_local.transform_point3(wow_to_bevy(eye));
    let look_bevy = placed
        .world_from_local
        .transform_point3(wow_to_bevy(to_local(look_world)));
    let clip = Mat4::perspective_rh(crate::view::CAM_FOVY, 16.0 / 9.0, 0.1, 1000.0)
        * Mat4::look_at_rh(eye_bevy, look_bevy, Vec3::Y);
    let pvs = crate::wmo_portal::compute_pvs_traced(
        &subject.model,
        eye,
        subject.terrain_z(eye),
        &clip,
        &placed.world_from_local,
        &mut (),
    );

    // The flags the whole verdict turns on, asserted so a data misread can't pass silently.
    assert_eq!(
        subject.model.group_nav[38].flags & 0x48,
        0x40,
        "g38 is the EXTERIOR_LIT courtyard"
    );
    assert_eq!(
        subject.model.group_nav[72].flags & 0x48,
        0x40,
        "g72 is the second EXTERIOR_LIT courtyard"
    );
    assert_eq!(
        subject.model.group_nav[61].flags & 0x48,
        0,
        "g61 is a true interior room"
    );

    assert!(
        pvs.visible[61],
        "the flood reaches the room behind the arches"
    );
    assert!(
        !pvs.interior_fog[61],
        "B335: the room two exterior-lit courtyards away must inherit the SCENE fog, not this \
         building's MFOG teal"
    );
    assert!(
        !pvs.interior_fog[72],
        "the chain is already broken at the second courtyard"
    );
    assert!(
        !pvs.interior_fog[38],
        "the courtyard the camera stands in is exterior-LIT: the exterior drawer pushes no fog, \
         so its own surfaces are on the scene lane too"
    );
    assert!(
        pvs.interior_fog.iter().all(|f| !f),
        "from this courtyard NOTHING wears the building's MFOG — which is what the reference \
         screenshot shows: not one navy pixel in the frame"
    );

    // The control, from a true interior: standing in g61 itself, the room DOES take the lane —
    // otherwise this fixture would pass on a gate that is simply always off.
    let inside = to_local([-213.90, 2236.15, 81.5]);
    let inside_bevy = placed
        .world_from_local
        .transform_point3(wow_to_bevy(inside));
    let inside_clip = Mat4::perspective_rh(crate::view::CAM_FOVY, 16.0 / 9.0, 0.1, 1000.0)
        * Mat4::look_at_rh(inside_bevy, eye_bevy, Vec3::Y);
    let inside_pvs = crate::wmo_portal::compute_pvs_traced(
        &subject.model,
        inside,
        subject.terrain_z(inside),
        &inside_clip,
        &placed.world_from_local,
        &mut (),
    );
    assert!(
        inside_pvs.interior_fog[61],
        "control: the room seeds its own flood, so it wears the building's MFOG from inside"
    );
}
