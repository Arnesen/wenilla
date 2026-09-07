//! **The additive blend-state check** — does every transparent draw bind the blend state its
//! material asked for?
//!
//! An additive glow card's look is two halves that must agree: the material's marker word
//! (`clutter_fade.z` bit 2) tells `wow_model.wgsl` to fold the radial alpha into the colour,
//! and the SAME marker, through `WowModelExt::specialize`, gives the pipeline its pure
//! `(ONE, ONE)` add. A draw that pairs the folded colour with an alpha-blend pipeline draws a
//! dim skirt; one that pairs an unfolded colour with the pure add draws the texture unweighted
//! — a hard, saturated disc where a soft halo should be. Decision 0071 measured that class on
//! macOS/Metal with the extra UI camera on the window (some draws intermittently bound a
//! blend-enabled pipeline) and immunised *opaque* batches with an alpha pin; glow cards were
//! left alone. The director's strong-halo report reads a correct main-world material on a
//! card whose draw is wrong, which is exactly the half this check reads.
//!
//! Render world, `Cleanup`, every frame, whatever the env: for each `Transparent3d` item whose
//! entity binds one of our materials, compare the pipeline descriptor's colour blend against
//! the material's additive marker. Mismatches are counted into a main-world-readable atomic
//! (the perf pill prints them red the frame they happen, so the director sees the count beside
//! the halo), and the first few of each second are logged with the entity, the material and
//! the blend state actually bound — the number and the names the fix will be judged against.
//! Cost: one hash probe and one descriptor read per transparent item, a few hundred a frame.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bevy::asset::AssetId;
use bevy::core_pipeline::core_3d::Transparent3d;
use bevy::pbr::RenderMaterialInstances;
use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_phase::ViewSortedRenderPhases;
use bevy::render::render_resource::{BlendFactor, PipelineCache};
use bevy::render::{Render, RenderApp, RenderSystems};

use benilla_assets::materials::WowModelMaterial;

/// The realized materials whose marker word says ADDITIVE, by asset id — what a draw of theirs
/// must bind `(ONE, ONE)` for. Maintained from the asset events in the main world (a realized
/// material is Added, an edited one Modified), extracted as an `Arc` clone each frame.
#[derive(Resource, Clone, Default, ExtractResource)]
pub(crate) struct AdditiveMaterials(pub(crate) Arc<HashSet<AssetId<WowModelMaterial>>>);

/// Draws this frame whose bound blend state contradicted their material's additive marker.
/// Written by the render world's [`check_blend_states`], read by the pill and the probe.
#[derive(Resource, Clone)]
pub(crate) struct BlendMismatchShared(pub(crate) Arc<AtomicU64>);

/// How many consecutive frames a mismatch must last before it is worth a warning — one frame is
/// the pipeline specialization catching up with a material swap, which a 2026-09-06 login
/// confirmed by ending four such runs at exactly one frame.
const REPORT_AFTER_FRAMES: u32 = 2;

/// One mismatching draw, tracked across frames so a transient (one frame after a material
/// swap, the specialization tick catching up) reads differently from a PERSISTING one — the
/// director's strong halo holds until a relog, and that difference is the finding.
pub(crate) struct Mismatch {
    pub(crate) material: AssetId<WowModelMaterial>,
    /// Whether the bound pipeline was the pure add (so the material was NOT additive), or the
    /// reverse.
    pub(crate) bound_add: bool,
    /// Consecutive frames this entity has mismatched.
    pub(crate) frames: u32,
    /// The `frames` count the main world last reported at, so each entry logs on its first
    /// frame and then once every couple of seconds while it persists.
    pub(crate) reported: u32,
    /// The draw has left the transparent phase — the run is over, and its LENGTH is the fact the
    /// log could not previously state. Kept one more pass so the main world can narrate the end
    /// before the entry is dropped.
    pub(crate) ended: bool,
    /// …and narrated exactly once.
    pub(crate) end_reported: bool,
}

/// The live mismatch table, by main-world entity — kept by the render world, read and
/// narrated by [`report_blend_mismatches`] with the names only the main world knows.
#[derive(Resource, Clone, Default)]
pub(crate) struct BlendMismatchLive(pub(crate) Arc<Mutex<HashMap<Entity, Mismatch>>>);

/// Main world: keep [`AdditiveMaterials`] current off the material asset events.
fn track_additive_materials(
    mut events: MessageReader<AssetEvent<WowModelMaterial>>,
    materials: Res<Assets<WowModelMaterial>>,
    mut set: ResMut<AdditiveMaterials>,
) {
    let mut changed = false;
    let mut next: Option<HashSet<AssetId<WowModelMaterial>>> = None;
    for ev in events.read() {
        let (id, present) = match ev {
            AssetEvent::Added { id } | AssetEvent::Modified { id } => (*id, true),
            AssetEvent::Removed { id } | AssetEvent::Unused { id } => (*id, false),
            AssetEvent::LoadedWithDependencies { id } => (*id, true),
        };
        let additive = present
            && materials
                .get(id)
                .is_some_and(|m| (m.extension.clutter_fade.z as u32) & 4 != 0);
        let s = next.get_or_insert_with(|| (*set.0).clone());
        let moved = if additive {
            s.insert(id)
        } else {
            s.remove(&id)
        };
        changed |= moved;
    }
    if changed {
        if let Some(s) = next {
            set.0 = Arc::new(s);
        }
    }
}

/// Render world, `Cleanup`: the check itself (module docs).
fn check_blend_states(
    additive: Res<AdditiveMaterials>,
    shared: Res<BlendMismatchShared>,
    live: Res<BlendMismatchLive>,
    instances: Res<RenderMaterialInstances>,
    transparent: Option<Res<ViewSortedRenderPhases<Transparent3d>>>,
    pipeline_cache: Res<PipelineCache>,
) {
    let mut mismatches = 0u64;
    let mut seen: HashSet<Entity> = HashSet::new();
    let mut table = live.0.lock().unwrap();
    if let Some(phases) = transparent {
        for phase in phases.0.values() {
            for item in &phase.items {
                let Some(inst) = instances.instances.get(&item.entity.1) else {
                    continue;
                };
                let Ok(id) = inst.asset_id.try_typed::<WowModelMaterial>() else {
                    continue; // not one of ours (the sky dome's StandardMaterial, …)
                };
                let expect_add = additive.0.contains(&id);
                let blend = pipeline_cache
                    .get_render_pipeline_descriptor(item.pipeline)
                    .fragment
                    .as_ref()
                    .and_then(|f| f.targets.first())
                    .and_then(|t| t.as_ref())
                    .and_then(|t| t.blend);
                let is_add = blend.is_some_and(|b| {
                    b.color.src_factor == BlendFactor::One && b.color.dst_factor == BlendFactor::One
                });
                if expect_add == is_add {
                    continue;
                }
                mismatches += 1;
                let main = item.entity.1.id();
                if seen.insert(main) {
                    let e = table.entry(main).or_insert(Mismatch {
                        material: id,
                        bound_add: is_add,
                        frames: 0,
                        reported: 0,
                        ended: false,
                        end_reported: false,
                    });
                    e.material = id;
                    e.bound_add = is_add;
                    e.frames += 1;
                    e.ended = false; // back in the phase — the run is still running
                }
            }
        }
    }
    // **A run's end is reported, not dropped.** This used to be a bare `retain`, so an entry
    // vanished the moment its entity left the transparent phase — despawned, culled, hidden, or
    // genuinely fixed — and the run's LENGTH went with it. That was half of why a mismatch line
    // was unanswerable: every run of 1..119 frames prints "1 frame(s)" (the report fires on
    // frame 1 and then only on multiples of 120), so a two-second visible defect and a
    // single-frame specialization hiccup were the same text. Entries are held one extra pass
    // marked `ended`, which is what lets the main world state the length it actually ran.
    table.retain(|e, m| {
        if seen.contains(e) {
            return true;
        }
        if m.ended {
            return false; // its end has been narrated — now it can go
        }
        m.ended = true;
        true
    });
    shared.0.store(mismatches, Ordering::Relaxed);
}

/// Main world, `PostUpdate`: narrate the live table with the names the render world lacks —
/// the object the entity is a part of, and the texture the material binds — on an entry's
/// first frame and then every two seconds while it persists. A one-frame entry is the
/// specialization tick catching up with a material swap; a persisting one is the defect.
fn report_blend_mismatches(
    live: Res<BlendMismatchLive>,
    objects: Query<&benilla_world::interact::WorldObject>,
    materials: Res<Assets<WowModelMaterial>>,
    server: Res<AssetServer>,
) {
    let mut table = live.0.lock().unwrap();
    for (entity, m) in table.iter_mut() {
        // **A one-frame run is the specialization tick, and is no longer shouted about.** The
        // module always said so; it warned on frame 1 anyway, because nothing could tell a
        // one-frame run from a 119-frame one. Now something can, and a real login settled it:
        // four entries reported "1 frame(s) so far" and "ran 1 frame(s), now clear" 26 ms later
        // (2026-09-06). So the warn waits for a run to SURVIVE the tick, and a run that does not
        // is recorded at debug rather than as a warning about a defect nobody has.
        let ending = m.ended && !m.end_reported;
        if ending {
            m.end_reported = true;
            if m.reported == 0 {
                debug!(
                    "blend mismatch: transient on entity {entity} — ran {} frame(s), never \
                     survived the specialization tick",
                    m.frames,
                );
                continue;
            }
        } else {
            let due = m.frames == REPORT_AFTER_FRAMES
                || (m.frames > REPORT_AFTER_FRAMES && m.frames % 120 == 0);
            if !due || m.reported == m.frames {
                continue;
            }
        }
        m.reported = m.frames;
        let who = objects.get(*entity).map_or_else(
            |_| "<no WorldObject>".to_string(),
            |o| format!("{:?} #{} {}", o.kind, o.id, o.label),
        );
        let tex = materials
            .get(m.material)
            .and_then(|mat| mat.base.base_color_texture.as_ref())
            .and_then(|h| server.get_path(h.id()))
            .map_or("?".to_string(), |p| p.to_string());
        let mat = match m.material {
            AssetId::Index { index, .. } => format!("#{}", index.to_bits()),
            AssetId::Uuid { uuid } => uuid.to_string(),
        };
        warn!(
            "blend mismatch: {who} entity {entity} material {mat} tex {tex} — material \
             additive={} but bound pipeline {} — {}{}",
            !m.bound_add,
            if m.bound_add {
                "ADD (One,One)"
            } else {
                "alpha-blend"
            },
            // "1 frame(s)" was printed for every run from 1 to 119 frames long, so the line could
            // not distinguish a specialization hiccup from a two-second visible defect. An ended
            // run now states the length it actually ran.
            if ending {
                format!("ran {} frame(s), now clear", m.frames)
            } else {
                format!("{} frame(s) so far", m.frames)
            },
            // Only a run still going can be persisting; an ended one has already said how long
            // it lasted, and "now clear PERSISTING" is a contradiction.
            if !ending && m.frames > 1 {
                " PERSISTING"
            } else {
                ""
            },
        );
    }
}

pub(crate) fn plugin(app: &mut App) {
    let shared = Arc::new(AtomicU64::new(0));
    let live = BlendMismatchLive::default();
    app.init_resource::<AdditiveMaterials>()
        .insert_resource(BlendMismatchShared(shared.clone()))
        .insert_resource(live.clone())
        .add_plugins(ExtractResourcePlugin::<AdditiveMaterials>::default())
        // After the asset event flush, like bevy's own `check_entities_needing_specialization`:
        // a material realized this frame is extracted, prepared and drawn this frame, and a
        // tracker that reads its `Added` a frame late reports every first draw of an additive
        // material as a mismatch — the rig's whole first harvest (glue sword glows, weapon
        // sheens, the lamppost card), all one frame, all false.
        .add_systems(
            PostUpdate,
            (
                track_additive_materials.after(bevy::asset::AssetEventSystems),
                report_blend_mismatches,
            ),
        );
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    render_app
        .insert_resource(BlendMismatchShared(shared))
        .insert_resource(live)
        .add_systems(Render, check_blend_states.in_set(RenderSystems::Cleanup));
}
