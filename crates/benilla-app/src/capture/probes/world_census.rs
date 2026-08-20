//! The two exclusive-`World` reflection dumps — the pair of one-shots that answer "what is
//! resident right now, and what is it made of" by walking the live `World` itself: the bevy_ui
//! node inventory ([`NodeProbePlugin`]) and the archetype census ([`EntityCensusPlugin`]).
//! They share the shape (fire once at `t`, take `&mut World`, print with the ubiquitous
//! plumbing components filtered out) as well as the question.

use bevy::prelude::*;

/// The bevy_ui node census (`WOW_NODE_PROBE=<secs>`): once, `t` seconds in, print one line per
/// live `ComputedNode` entity — resolved rect (logical px, y-down), visibility, and the entity's
/// full component list — the "who owns this rectangle" instrument for UI drawn OUTSIDE the
/// FrameXML quad pass (the glue widgets, loading screen, overlays), which `WOW_UI_PROBE`'s quad
/// dump can't see. Born hunting a phantom gold-bordered box over the mail window's send tab.
pub(crate) struct NodeProbePlugin;

impl Plugin for NodeProbePlugin {
    fn build(&self, app: &mut App) {
        let at = std::env::var("WOW_NODE_PROBE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10.0);
        app.insert_resource(NodeProbe { at, fired: false })
            .add_systems(Update, fire_node_probe);
    }
}

/// [`NodeProbePlugin`] state: the fire time and the once-latch.
#[derive(Resource)]
struct NodeProbe {
    at: f32,
    fired: bool,
}

fn fire_node_probe(world: &mut World) {
    {
        let time = world.resource::<Time>().elapsed_secs();
        let probe = world.resource::<NodeProbe>();
        if probe.fired || time < probe.at {
            return;
        }
    }
    world.resource_mut::<NodeProbe>().fired = true;
    let scale = world
        .query::<&bevy::window::Window>()
        .iter(world)
        .next()
        .map_or(1.0, bevy::window::Window::scale_factor);
    let mut q = world.query::<(
        Entity,
        &bevy::ui::ComputedNode,
        &GlobalTransform,
        Option<&InheritedVisibility>,
    )>();
    let rows: Vec<(Entity, Vec2, Vec3, bool)> = q
        .iter(world)
        .map(|(e, node, gt, vis)| {
            (
                e,
                node.size(),
                gt.translation(),
                vis.is_none_or(|v| v.get()),
            )
        })
        .collect();
    info!("node probe: {} nodes, scale {scale}", rows.len());
    for (e, size, center, vis) in rows {
        let comps: Vec<String> = world.inspect_entity(e).map_or_else(
            |_| Vec::new(),
            |it| {
                it.map(|c| c.name().shortname().to_string())
                    .filter(|n| {
                        // Drop the ubiquitous plumbing components — the signal is the rest.
                        !matches!(
                            n.as_str(),
                            "Transform"
                                | "GlobalTransform"
                                | "Visibility"
                                | "InheritedVisibility"
                                | "ViewVisibility"
                                | "ChildOf"
                                | "Children"
                        )
                    })
                    .collect()
            },
        );
        // ComputedNode is physical px; translation is the node's center, also physical.
        info!(
            "node probe: [{:.0},{:.0} {:.0}x{:.0}] vis={} {:?}",
            (center.x - size.x * 0.5) / scale,
            (center.y - size.y * 0.5) / scale,
            size.x / scale,
            size.y / scale,
            vis,
            comps
        );
    }
}

/// The entity census (`WOW_ENTITY_CENSUS=<secs>`, REAL seconds): once, `t` seconds in, print one
/// line per live archetype — entity count plus its signal components, largest first — and a machine-readable
/// summary. The "what IS the entity count made of" instrument: the standing HUD reads tens of
/// thousands of entities, and every per-frame cost that scales with *residency* (0362's
/// change-tick sweeps, transform propagation, render extraction) is only attributable once
/// residency itself has names. Born with the cost-ledger campaign.
pub(crate) struct EntityCensusPlugin;

impl Plugin for EntityCensusPlugin {
    fn build(&self, app: &mut App) {
        let at = std::env::var("WOW_ENTITY_CENSUS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10.0);
        app.insert_resource(EntityCensus { at, fired: false })
            .add_systems(Update, fire_entity_census);
    }
}

/// [`EntityCensusPlugin`] state: the fire time and the once-latch.
#[derive(Resource)]
struct EntityCensus {
    at: f32,
    fired: bool,
}

/// Archetype lines the census prints; everything smaller folds into the summary's `other_n`.
const ENTITY_CENSUS_ROWS: usize = 60;

/// Signal components shown per archetype line — enough to name what the entities are without
/// drowning the line in a 30-component render archetype.
const ENTITY_CENSUS_COMPS: usize = 14;

fn fire_entity_census(world: &mut World) {
    {
        // REAL seconds, not virtual: the census is timed to compose with `WOW_LIVE_FPS_AT`
        // (also real), and virtual time lags real by the load stalls — a virtual-timed one-shot
        // scheduled "just before sampling" fires after the probe has already exited.
        let time = world.resource::<Time<bevy::time::Real>>().elapsed_secs();
        let probe = world.resource::<EntityCensus>();
        if probe.fired || time < probe.at {
            return;
        }
    }
    world.resource_mut::<EntityCensus>().fired = true;

    // The anchor split (0732 slice A's premise check). `RigAnchor` leaves are the single largest
    // archetype in the scene — 53 % of all entities — and slice A's claim is that most of them ride
    // nothing. "Rides nothing" is directly observable: an anchor whose entity has no `Children` is
    // hosting no attachment, no emitter, no ribbon, no card. Counted here rather than inferred from
    // the model's bone sources, because the model says what COULD attach and the world says what
    // DID.
    {
        let mut q = world.query_filtered::<Option<&bevy::prelude::Children>, bevy::prelude::With<benilla_world::rig_anim::RigAnchor>>();
        let (mut total, mut childless) = (0u32, 0u32);
        for kids in q.iter(world) {
            total += 1;
            if kids.is_none_or(|k| k.is_empty()) {
                childless += 1;
            }
        }
        eprintln!(
            "ENTITY_CENSUS_ANCHORS total={total} childless={childless} ({:.1}%) hosting={}",
            100.0 * f32::from(u16::try_from(childless).unwrap_or(u16::MAX))
                / f32::from(u16::try_from(total.max(1)).unwrap_or(u16::MAX)),
            total - childless
        );
    }
    let components = world.components();
    let mut rows: Vec<(usize, bool, String)> = world
        .archetypes()
        .iter()
        .filter(|a| !a.is_empty())
        .map(|a| {
            let full: Vec<String> = a
                .components()
                .iter()
                .filter_map(|id| components.get_info(*id))
                .map(|c| c.name().shortname().to_string())
                .collect();
            // Whether this archetype sits in the render-visibility population: bevy's
            // `check_visibility` sweeps every `ViewVisibility` row once PER ACTIVE CAMERA,
            // so `vis=y` rows are the ones a second camera (booth) re-bills the frame for.
            let in_vis_population = full.iter().any(|n| n == "ViewVisibility");
            let signal: Vec<String> = full
                .iter()
                .filter(|n| {
                    // Drop the ubiquitous plumbing components — the signal is the rest.
                    !matches!(
                        n.as_str(),
                        "Transform"
                            | "GlobalTransform"
                            | "Visibility"
                            | "InheritedVisibility"
                            | "ViewVisibility"
                            | "ChildOf"
                            | "Children"
                    )
                })
                .cloned()
                .collect();
            // A bare transform node has no signal left after the filter — and two such
            // archetypes differing only in plumbing (Children vs not) would print as identical
            // rows. For those, the plumbing IS the signal: print the full list.
            let names = if signal.len() <= 1 { full } else { signal };
            let shown = names.len().min(ENTITY_CENSUS_COMPS);
            let more = names.len() - shown;
            let mut comps = names[..shown].join(", ");
            if more > 0 {
                comps.push_str(&format!(" +{more}"));
            }
            (a.len() as usize, in_vis_population, comps)
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    let (total_arch, total_n) = (rows.len(), rows.iter().map(|r| r.0).sum::<usize>());
    let vis_n = rows.iter().filter(|r| r.1).map(|r| r.0).sum::<usize>();
    let other_n = rows
        .iter()
        .skip(ENTITY_CENSUS_ROWS)
        .map(|r| r.0)
        .sum::<usize>();
    for (n, vis, comps) in rows.iter().take(ENTITY_CENSUS_ROWS) {
        println!(
            "ENTITY_CENSUS_ARCH n={n} vis={} comps=[{comps}]",
            if *vis { "y" } else { "n" }
        );
    }
    println!(
        "ENTITY_CENSUS total={total_n} vis_n={vis_n} archetypes={total_arch} \
         rows={} other_n={other_n}",
        rows.len().min(ENTITY_CENSUS_ROWS),
    );
}
