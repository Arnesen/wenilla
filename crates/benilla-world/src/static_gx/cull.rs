//! The per-frame CPU scene walk of the retained pass (split from `mod.rs` at the
//! 1,000-line budget) — the seven-term collapse at cell/group granularity, the dead-region
//! reap, the fader exile scan (B2, decision 1431), and the `WOW_GX_CENSUS` agreement
//! instrument.

use bevy::prelude::*;

use super::{FaderState, GxFader, StaticGx};

/// Exile hysteresis (yd). Entry into the feather has NO slack — a retained draw at
/// `alpha < 1` is wrong the frame it happens — but both exits (re-admission to Steady, the
/// far-side despawn to Gone) require the camera this far past the crossing, so a camera
/// parked exactly on a band edge cannot flap entities into and out of existence.
const FADE_HYST: f32 = 1.0;

/// Where the exile state machine sits, as a value (the entity list stays on
/// [`FaderState`]).
#[derive(Clone, Copy, PartialEq, Debug)]
enum FadeClass {
    Steady,
    Feather,
    Gone,
}

impl GxFader {
    fn class(&self) -> FadeClass {
        match self.state {
            FaderState::Steady => FadeClass::Steady,
            FaderState::Exiled { .. } => FadeClass::Feather,
            FaderState::Gone => FadeClass::Gone,
        }
    }
}

/// The exile step: where a fader at horizontal centre-distance `d` should sit, given where
/// it is now. Classifies on [`crate::model_fade::doodad_fade_alpha`] itself — the SAME
/// function the entity authority evaluates per frame — so the two lanes cannot disagree at
/// the entry edges; the exits shift the *sample point* by [`FADE_HYST`] instead of keeping a
/// second band table.
fn fade_step(radius: f32, d: f32, current: FadeClass) -> FadeClass {
    use crate::model_fade::doodad_fade_alpha as alpha;
    match current {
        FadeClass::Steady => {
            let a = alpha(radius, d);
            if a >= 1.0 {
                FadeClass::Steady
            } else if a > 0.0 {
                FadeClass::Feather
            } else {
                FadeClass::Gone
            }
        }
        FadeClass::Feather => {
            if alpha(radius, d + FADE_HYST) >= 1.0 {
                FadeClass::Steady
            } else if alpha(radius, d - FADE_HYST) <= 0.0 {
                FadeClass::Gone
            } else {
                FadeClass::Feather
            }
        }
        FadeClass::Gone => {
            if alpha(radius, d + FADE_HYST) >= 1.0 {
                FadeClass::Steady
            } else if alpha(radius, d) > 0.0 {
                FadeClass::Feather
            } else {
                FadeClass::Gone
            }
        }
    }
}

/// `WOW_GX_FADE_TRACE=1` — one line per exile transition (the live-probe instrument for the
/// state machine: what crossed, where, into what).
fn fade_trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WOW_GX_FADE_TRACE").is_some())
}

/// Spawn one exiled placement as ordinary entities — the production assembler's own bundle
/// for a static, non-billboard, exterior doodad batch (`assemble.rs`), from handle clones
/// captured at divert time. The spawn flushes after `CheckVisibility` (the plugin's
/// ordering law), so the entity first draws NEXT frame — seeded with the values the
/// authority will confirm (blend twin + the live fade alpha in `MeshTag`); the authority
/// owns it from the next `Update` on. The explicit `GlobalTransform`/`InheritedVisibility`
/// are belts: next frame's propagation computes both before anything draws, but a fresh
/// root should never sit a frame on defaults that place it at the origin, hidden.
fn spawn_exile(commands: &mut Commands, f: &GxFader, alpha: f32, admitted: bool) -> Vec<Entity> {
    let mut ents = Vec::with_capacity(f.batches.len());
    for b in &f.batches {
        let mut e = commands.spawn((
            Mesh3d(b.stat_mesh.clone()),
            MeshMaterial3d(if alpha < 1.0 {
                b.blend.clone()
            } else {
                b.cutout.clone()
            }),
            f.transform,
            // A placement is a world root, so Transform IS the global (see the fn doc).
            GlobalTransform::from(f.transform),
            crate::model_render::ModelPart {
                kind: crate::model_render::ModelKind::Doodad,
                blend: b.blend_mode,
            },
            crate::interact::PickMesh(b.geometry.clone()),
            bevy::mesh::MeshTag(crate::mesh_tag::alpha_bits(alpha)),
            crate::model_fade::DoodadFade {
                radius: f.radius,
                local_center: f.local_center,
                cutout: b.cutout.clone(),
                blend: b.blend.clone(),
            },
            crate::exterior_cull::ExteriorScene,
            crate::interact::WorldObject {
                kind: crate::model_render::ModelKind::Doodad,
                label: f.label.to_string(),
                id: f.uid,
                detail: "static-gx exile".into(),
            },
        ));
        if admitted {
            e.insert(bevy::camera::visibility::InheritedVisibility::VISIBLE);
        } else {
            e.insert((
                Visibility::Hidden,
                bevy::camera::visibility::InheritedVisibility::HIDDEN,
            ));
        }
        if let Some(aabb) = b.aabb {
            e.insert((aabb, bevy::camera::visibility::NoAutoAabb));
        }
        ents.push(e.id());
    }
    if fade_trace() {
        println!("GX_FADE_SPAWN uid={} ents={ents:?}", f.uid);
    }
    ents
}

/// The CPU scene walk (1429: "nothing needs a GPU-side visibility bitset") — frustum, the
/// farclip wall, and the exterior window gate, each at cell granularity, published as this
/// frame's visible list. Coarser than the entity path's per-submesh tests by design; the gate
/// admits at Aabb granularity either way, so cell-whole is conservative (overdraw, never a
/// hole).
///
/// Slice 2 adds the WMO half: per-GROUP admission over each retained region — the portal
/// PVS bit (fail-open, honouring the panel's `portal_cull` A/B switch) ∧ frustum ∧ farclip
/// ∧ the exterior gate with the own-building exemption (the camera's containing placement is
/// not exterior to itself — `CameraInteriorClaim`, the same term the entity authority reads).
/// This is ALSO where a dead region is reaped: the instance entity despawns with its
/// placement, and that death — not the owner tile's — is the WMO release law (straddler
/// handoffs keep the placement alive under a new owner).
///
/// B2 adds the fader exile scan: per cell, a cheap ring test (the camera's distance interval
/// to the cell's fader centres against the union fade band) skips settled cells wholesale;
/// a straddling cell walks its placements through [`fade_step`]. A placement entering the
/// feather respawns as ordinary entities that first draw next frame, and its kill bits ARM
/// on that same next frame — the retained item dies in the rendered frame the entity
/// appears (the overlap protocol; see [`FaderState`]). A re-admit reverses both in ONE
/// frame: the despawn lands before extract and the cleared bit rides the same publish.
#[allow(clippy::too_many_arguments)]
pub(super) fn cull_cells(
    mut commands: Commands,
    mut gx: ResMut<StaticGx>,
    debug: Res<crate::dev_state::DebugState>,
    view: Res<crate::view::ViewDistance>,
    cam: Query<
        (
            &GlobalTransform,
            &Projection,
            &bevy::camera::primitives::Frustum,
        ),
        With<crate::view::WorldCamera>,
    >,
    windows: Res<crate::wmo_portal::ExteriorWindows>,
    claim: Res<crate::wmo_portal::CameraInteriorClaim>,
    instances: Query<&crate::wmo_portal::WmoPortalInstance>,
) {
    let _t = super::gx_perf_guard(1);
    // Drain the lifecycle queue first: exiled entities whose seed died out from under them
    // (owner release, map clear) — this scan owns the exile lifecycle end to end.
    for e in gx.pending_despawn.drain(..) {
        commands.entity(e).try_despawn();
    }
    let cam_view = cam.iter().next();
    let StaticGx {
        cells,
        wmos,
        world,
        fade_events,
        ..
    } = &mut *gx;
    // Reap dead regions (instance despawned with its placement) — items and published draws.
    wmos.retain(|e, _| instances.contains(*e));
    world.wmos.retain(|e, _| instances.contains(*e));
    world.visible.clear();
    world.visible_wmos.clear();
    let Some((cam_t, proj, frustum)) = cam_view else {
        return;
    };
    let cam_pos = cam_t.translation();
    let cam_fwd = Vec3::from(cam_t.forward());
    let gate = crate::exterior_cull::ExteriorGate::build(&windows, Some((cam_t, proj)));
    let m = &debug.models;
    let doodads_on =
        m.kind_visible[crate::model_render::kind_index(crate::model_render::ModelKind::Doodad)];

    // ---- The fader exile scan (B2, 1431) ----
    let cam_xz = Vec2::new(cam_pos.x, cam_pos.z);
    for (&key, cell) in cells.iter_mut() {
        if cell.faders.is_empty() {
            continue;
        }
        let Some((mn, mx)) = cell.fader_bounds else {
            continue;
        };
        // The camera's distance interval to the fader-centre rect (2D), against the cell's
        // band union — with the hysteresis margin folded in, so a wholesale verdict implies
        // every per-placement verdict and one walk under it settles the whole cell.
        let dmin = cam_xz.distance(cam_xz.clamp(mn, mx));
        let corner = Vec2::new(
            if (cam_xz.x - mn.x).abs() > (cam_xz.x - mx.x).abs() {
                mn.x
            } else {
                mx.x
            },
            if (cam_xz.y - mn.y).abs() > (cam_xz.y - mx.y).abs() {
                mn.y
            } else {
                mx.y
            },
        );
        let dmax = cam_xz.distance(corner);
        let (near_min, far_max) = cell.ring;
        let verdict = if dmax < near_min - FADE_HYST {
            Some(true) // every placement steady, with margin
        } else if dmin > far_max + FADE_HYST {
            Some(false) // every placement gone, with margin
        } else {
            None // the ring straddles the cell — walk it
        };
        if verdict.is_some() && verdict == cell.settled && !cell.bits_stale {
            continue;
        }
        // The cell-granular admission the exile seed borrows (the walk re-derives the exact
        // per-entity verdict next Update; this only bridges the spawn frame).
        let admitted = doodads_on
            && world.cells.get(&key).is_none_or(|d| {
                gate.admits(&GlobalTransform::from_translation(d.origin), Some(&d.aabb))
            });
        let mut bits_changed = cell.bits_stale;
        let mut unarmed_left = false;
        for f in cell.faders.values_mut() {
            // The overlap protocol's second half: LAST frame's exile spawns flushed at the
            // end of that frame's PostUpdate and first draw THIS frame — arm their kill bits
            // now, so the retained item dies in the same rendered frame the entity appears.
            if let FaderState::Exiled { armed, .. } = &mut f.state {
                if !*armed {
                    *armed = true;
                    bits_changed = true;
                }
            }
            let d = Vec2::new(f.center.x, f.center.z).distance(cam_xz);
            let old = f.class();
            let new = fade_step(f.radius, d, old);
            if new == old {
                continue;
            }
            let was_killed = matches!(
                f.state,
                FaderState::Exiled { armed: true, .. } | FaderState::Gone
            );
            if let FaderState::Exiled { ents, .. } =
                std::mem::replace(&mut f.state, FaderState::Steady)
            {
                for e in ents {
                    commands.entity(e).try_despawn();
                }
            }
            f.state = match new {
                FadeClass::Steady => {
                    fade_events[1] += 1;
                    FaderState::Steady
                }
                FadeClass::Gone => {
                    fade_events[2] += 1;
                    FaderState::Gone
                }
                FadeClass::Feather => {
                    fade_events[0] += 1;
                    let alpha = crate::model_fade::doodad_fade_alpha(f.radius, d);
                    // Fresh spawns stay UNARMED this frame (their entities don't draw until
                    // next frame — killing now would hole the handoff)… unless the fader was
                    // already killed as Gone: those bits stay set, nothing to wait for.
                    let armed = old == FadeClass::Gone;
                    unarmed_left |= !armed;
                    FaderState::Exiled {
                        ents: spawn_exile(&mut commands, f, alpha, admitted),
                        armed,
                    }
                }
            };
            let now_killed = matches!(
                f.state,
                FaderState::Exiled { armed: true, .. } | FaderState::Gone
            );
            if was_killed != now_killed {
                bits_changed = true;
            }
            if fade_trace() {
                println!(
                    "GX_FADE cell=({},{}) uid={} {:?}->{:?} d={:.1} r={:.2}",
                    key.0, key.1, f.uid, old, new, d, f.radius
                );
            }
        }
        // An unarmed exile must be revisited next frame whatever the ring says.
        cell.settled = if unarmed_left { None } else { verdict };
        if bits_changed {
            if let Some(draw) = world.cells.get_mut(&key) {
                // Rebuild whole from the states — cheap, and immune to index drift across
                // re-bakes (the flush refreshed `items` and set `bits_stale` this frame).
                draw.killed.iter_mut().for_each(|w| *w = 0);
                for f in cell.faders.values() {
                    let killed = match f.state {
                        FaderState::Steady | FaderState::Exiled { armed: false, .. } => false,
                        FaderState::Exiled { armed: true, .. } | FaderState::Gone => true,
                    };
                    if !killed {
                        continue;
                    }
                    for &i in &f.items {
                        let i = usize::from(i);
                        if let Some(w) = draw.killed.get_mut(i / 64) {
                            *w |= 1u64 << (i % 64);
                        }
                    }
                }
                draw.killed_rev = draw.killed_rev.wrapping_add(1);
            }
            cell.bits_stale = false;
        }
    }

    // The dev doodad toggle, cell-wholesale (see the module doc).
    if doodads_on {
        for (&cell, draw) in &world.cells {
            let center = draw.origin + Vec3::from(draw.aabb.center);
            let radius = Vec3::from(draw.aabb.half_extents).length();
            if !crate::view::within_farclip(view.farclip, cam_pos, cam_fwd, center, radius) {
                continue;
            }
            // The camera's own frustum (bevy's `update_frusta` output — the same planes its
            // per-entity cull tests), against the cell's world bound.
            let sphere = bevy::camera::primitives::Sphere {
                center: center.into(),
                radius,
            };
            if !frustum.intersects_sphere(&sphere, false) {
                continue;
            }
            // ADT doodads are exterior scene (spawn tags them per submesh): from inside a WMO
            // they draw only through a portal window. `admits` takes the same (transform,
            // aabb) shape the per-entity term feeds it; cell granularity is conservative
            // (overdraw, no hole).
            if !gate.admits(
                &GlobalTransform::from_translation(draw.origin),
                Some(&draw.aabb),
            ) {
                continue;
            }
            world.visible.push(cell);
        }
    }
    // The WMO regions — the dev WMO toggle wholesale, then per-group admission.
    if m.kind_visible[crate::model_render::kind_index(crate::model_render::ModelKind::Wmo)] {
        // The own-building exemption: the camera's containing placement (decision 0784's
        // dynamic exemption — a static tag could not be right, the player walks in and out).
        let own = claim.0.map(|c| c.room.instance);
        for (&entity, draw) in &world.wmos {
            // A region whose instance can't answer this frame (spawn-command latency) keeps
            // last frame's absence — one frame of the entity path's own arrival class.
            let Ok(inst) = instances.get(entity) else {
                continue;
            };
            let max_group = draw.groups.iter().map(|(g, _)| *g).max().unwrap_or(0);
            let mut sel = vec![false; usize::from(max_group) + 1];
            let mut any = false;
            for (group, aabb) in &draw.groups {
                // The portal PVS bit — the SAME fail-open read `WmoGroupVis::drawn_by` takes,
                // behind the same panel switch.
                if m.portal_cull
                    && !inst
                        .visible
                        .get(usize::from(*group))
                        .copied()
                        .unwrap_or(true)
                {
                    continue;
                }
                let center = draw.origin + Vec3::from(aabb.center);
                let radius = Vec3::from(aabb.half_extents).length();
                if !crate::view::within_farclip(view.farclip, cam_pos, cam_fwd, center, radius) {
                    continue;
                }
                let sphere = bevy::camera::primitives::Sphere {
                    center: center.into(),
                    radius,
                };
                if !frustum.intersects_sphere(&sphere, false) {
                    continue;
                }
                // Another building is exterior scene; the camera's own is exempt (0784).
                if Some(entity) != own
                    && !gate.admits(&GlobalTransform::from_translation(draw.origin), Some(aabb))
                {
                    continue;
                }
                sel[usize::from(*group)] = true;
                any = true;
            }
            if any {
                world.visible_wmos.push((entity, sel));
            }
        }
    }
    // `WOW_GX_CENSUS=1` — the slice-2 agreement instrument (1429: "LBRS-style drawn-count
    // agreement"): each selected item IS one would-be submesh entity, so this line reads
    // directly against the entity path's VIS_CENSUS/`drawn=` at the same pin (gx ≥ entity is
    // the expected granularity coarsening; gx ≫ entity is a portal leak). ~1 Hz, env-gated.
    // B2 adds the fader tallies (steady/exiled/gone) + the lifetime transition counters
    // (exiles/readmits/gones) — the churn instrument for the exile protocol.
    static CENSUS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *CENSUS.get_or_init(|| std::env::var_os("WOW_GX_CENSUS").is_some())
        && gx.frame.is_multiple_of(64)
    {
        let world = &gx.world;
        let cell_items: usize = world
            .visible
            .iter()
            .filter_map(|c| world.cells.get(c))
            .map(|d| d.draws.len())
            .sum();
        let (mut wmo_items, mut wmo_groups) = (0usize, 0usize);
        for (e, sel) in &world.visible_wmos {
            if let Some(d) = world.wmos.get(e) {
                wmo_items += d
                    .draws
                    .iter()
                    .filter(|i| {
                        i.group
                            .is_some_and(|g| sel.get(usize::from(g)).copied().unwrap_or(false))
                    })
                    .count();
            }
            wmo_groups += sel.iter().filter(|b| **b).count();
        }
        let (mut fs, mut fx, mut fg) = (0usize, 0usize, 0usize);
        for cell in gx.cells.values() {
            for f in cell.faders.values() {
                match f.state {
                    FaderState::Steady => fs += 1,
                    FaderState::Exiled { .. } => fx += 1,
                    FaderState::Gone => fg += 1,
                }
            }
        }
        let ev = gx.fade_events;
        println!(
            "GX_CENSUS cells={}/{} cell_items={cell_items} wmo_regions={}/{} \
             wmo_groups={wmo_groups} wmo_items={wmo_items} faders={fs}s/{fx}x/{fg}g \
             fade_events={}x/{}s/{}g",
            world.visible.len(),
            world.cells.len(),
            world.visible_wmos.len(),
            world.wmos.len(),
            ev[0],
            ev[1],
            ev[2],
        );
    }
    if super::gx_perf_enabled() && gx.frame.is_multiple_of(64) {
        use std::sync::atomic::Ordering::Relaxed;
        let ms: Vec<f64> = super::GX_PERF
            .iter()
            .map(|a| a.swap(0, Relaxed) as f64 / 64.0 / 1.0e6)
            .collect();
        println!(
            "GX_PERF ms/frame flush={:.3} cull={:.3} publish={:.3} prepare={:.3} node={:.3} \
             arrays_mb={}",
            ms[0],
            ms[1],
            ms[2],
            ms[3],
            ms[4],
            super::GX_VRAM.load(Relaxed) / (1024 * 1024)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state machine against the fade law it mirrors: entry edges exact (the retained
    /// draw must stop the frame the authority would feather), exit edges sticky by the
    /// hysteresis (a camera parked on a band edge cannot flap).
    #[test]
    fn the_exile_step_enters_exact_and_exits_sticky() {
        // radius 0 → band 40..50 (the small-prop bucket; golden in model_fade's tests).
        let r = 0.0;
        // Entry: steady holds through the band start, feathers strictly past it.
        assert_eq!(fade_step(r, 39.0, FadeClass::Steady), FadeClass::Steady);
        assert_eq!(fade_step(r, 40.0, FadeClass::Steady), FadeClass::Steady);
        assert_eq!(fade_step(r, 40.5, FadeClass::Steady), FadeClass::Feather);
        // Direct steady→gone (a teleport past the band end).
        assert_eq!(fade_step(r, 60.0, FadeClass::Steady), FadeClass::Gone);
        // Exit to steady needs the hysteresis: at d=40.5 the +1yd probe still feathers.
        assert_eq!(fade_step(r, 40.5, FadeClass::Feather), FadeClass::Feather);
        assert_eq!(fade_step(r, 39.0, FadeClass::Feather), FadeClass::Steady);
        // Exit to gone likewise sticky on the far edge.
        assert_eq!(fade_step(r, 50.5, FadeClass::Feather), FadeClass::Feather);
        assert_eq!(fade_step(r, 51.5, FadeClass::Feather), FadeClass::Gone);
        // Gone re-enters the feather the moment alpha is nonzero again…
        assert_eq!(fade_step(r, 49.9, FadeClass::Gone), FadeClass::Feather);
        assert_eq!(fade_step(r, 50.0, FadeClass::Gone), FadeClass::Gone);
        // …and jumps straight to steady only with the margin (a teleport close).
        assert_eq!(fade_step(r, 38.9, FadeClass::Gone), FadeClass::Steady);
    }
}
