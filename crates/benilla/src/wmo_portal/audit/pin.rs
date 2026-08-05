//! **The pin probe** — a director's `.go xyz` / `/shot` report, replayed offline through the real
//! flood. The sibling sweeps in [`super`] ask "does the invariant hold anywhere in this building";
//! this asks "what happened at exactly *that* spot", which is the question a bug report actually
//! poses. It reuses the harness's placed subjects and the same [`TraceLog`](super::super::probe)
//! recorder the in-client dump button writes, so there is no second flood to drift.

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use bevy::math::{Mat4, Vec3};

use super::{load_subject, Site, EXTERIOR, UNDERCITY};

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
    let site = Site {
        wmo: Box::leak(
            std::env::var("WOW_PIN_WMO")
                .unwrap_or_else(|_| UNDERCITY.wmo.to_string())
                .into_boxed_str(),
        ),
        map: Box::leak(
            std::env::var("WOW_PIN_MAP")
                .unwrap_or_else(|_| UNDERCITY.map.to_string())
                .into_boxed_str(),
        ),
        tile: std::env::var("WOW_PIN_TILE").map_or(UNDERCITY.tile, |s| {
            let (a, b) = s.split_once(',').expect("WOW_PIN_TILE wants tx,ty");
            (a.trim().parse().unwrap(), b.trim().parse().unwrap())
        }),
        uid: std::env::var("WOW_PIN_UID").map_or(UNDERCITY.uid, |s| s.trim().parse().unwrap()),
    };
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
    let clip = Mat4::perspective_rh(0.9, 16.0 / 9.0, 0.1, 1000.0)
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
        .iter()
        .enumerate()
        .filter(|(_, &v)| v)
        .map(|(i, _)| i)
        .collect();
    println!("visible: {} of {} groups {vis:?}", vis.len(), pvs.len());

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
                if pvs[gi] { "VISIBLE" } else { "CULLED" }
            );
        }
    }
}
