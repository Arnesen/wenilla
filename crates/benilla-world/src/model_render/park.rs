//! **Parking hidden world submeshes** — a streamed model part that has been hidden for a while
//! puts its `Mesh3d` down, and picks it up the frame its placement is shown again.
//!
//! Why: the retained pass draws the world, and the entity path keeps the batches it declines
//! resident — 5.8k ADT-doodad submeshes at the Stormwind auction house, 5.3k of them hidden by
//! the outdoor election or the far clip at any instant (decision 1947's census: WMO props whose
//! class cannot merge, shared-geometry batches, exterior fader props). Bevy's own sweeps walk
//! every `Mesh3d` whether it draws or not: the asset-changed marks, the specialization check,
//! `calculate_bounds`, the previous-transform update, the visibility reset — ~0.4 ms of an
//! alone city frame on the sampled profile, for rows that draw nothing. Without `Mesh3d` an
//! entity is in none of those queries, and everything else it carries stays: the transform,
//! the material handle, the tag, the pick mesh, the fade state.
//!
//! **The hysteresis is the point.** The election flips as the camera turns; parking on the
//! first hidden frame would move a component per flip per part. A part parks after
//! [`PARK_AFTER_FRAMES`] consecutive hidden frames and unparks the moment its root is shown —
//! so a glance across the square costs nothing, and only what stays hidden gets cheap.
//!
//! **Timing, and the one landmine.** The root election writes in `PostUpdate`; this runs in
//! `Update` on the next frame's verdict, which puts the re-inserted `Mesh3d` in place before
//! that frame's `check_entities_needing_specialization` and visibility check. Inserting it
//! later — after the visibility check, on an entity already view-visible — is the
//! `specialize_material_meshes` tick unwrap `static_gx::cull` documents (B2, 1431), and the
//! price of the safe order is one frame between a placement's election and its first draw.
//! Every world model part takes part — the streamer's placements and the units' bodies alike
//! (a raid standing round the camera keeps half its bodies behind it) — except parts on a
//! render layer of their own: a portrait booth's mirror parts show and hide with the booth's
//! wake, and its bake window is four frames.

use std::any::TypeId;

use bevy::camera::visibility::VisibilityClass;
use bevy::ecs::world::EntityWorldMut;
use bevy::prelude::*;

use super::ModelPart;

/// Consecutive hidden frames before a part parks — half a second at 60 Hz.
const PARK_AFTER_FRAMES: u16 = 30;

/// The mesh a parked part put down, restored verbatim on unpark.
#[derive(Component)]
pub struct ParkedMesh(pub Handle<Mesh>);

/// Consecutive frames this part has been hidden (its own or an ancestor's verdict).
#[derive(Component, Default)]
pub struct HiddenFrames(pub u16);

/// `Update`, after the visibility authority: count hidden frames, park the long-hidden,
/// unpark the shown. Reads last `PostUpdate`'s `InheritedVisibility` — the propagated verdict
/// of this part's whole ancestry.
#[allow(clippy::type_complexity)]
pub(super) fn park_hidden_parts(
    mut commands: Commands,
    mut parts: Query<
        (
            Entity,
            &InheritedVisibility,
            Option<&mut HiddenFrames>,
            Option<&Mesh3d>,
            Option<&ParkedMesh>,
        ),
        (
            With<ModelPart>,
            Without<bevy::camera::visibility::RenderLayers>,
        ),
    >,
) {
    for (entity, inherited, frames, mesh, parked) in &mut parts {
        if inherited.get() {
            if let Some(parked) = parked {
                commands
                    .entity(entity)
                    .insert(Mesh3d(parked.0.clone()))
                    .remove::<ParkedMesh>()
                    // Bevy's `Mesh3d` add hook pushes the mesh's `VisibilityClass` entry every
                    // time the component is (re)added, and nothing ever dedups that list;
                    // `check_visibility` queues an entity once PER entry, so each park →
                    // unpark cycle drew the part one more time. Opaque parts hide it; an
                    // additive glow card stacks on itself — the director's lamppost halo,
                    // doubling after every teleport that hid it and healed only by a relog.
                    // One entry per class, applied after the hook has run.
                    .queue(dedup_visibility_class);
            }
            if let Some(mut f) = frames {
                if f.0 != 0 {
                    f.0 = 0;
                }
            }
            continue;
        }
        let Some(mut frames) = frames else {
            commands.entity(entity).insert(HiddenFrames(1));
            continue;
        };
        frames.0 = frames.0.saturating_add(1);
        if frames.0 == PARK_AFTER_FRAMES {
            if let Some(mesh) = mesh {
                commands
                    .entity(entity)
                    .insert(ParkedMesh(mesh.0.clone()))
                    .remove::<Mesh3d>();
            }
        }
    }
}

/// Leave one entry per class in an entity's [`VisibilityClass`] (see the unpark site).
fn dedup_visibility_class(mut e: EntityWorldMut) {
    if let Some(mut class) = e.get_mut::<VisibilityClass>() {
        let mut seen: Vec<TypeId> = Vec::with_capacity(class.len());
        class.retain(|t| {
            if seen.contains(t) {
                false
            } else {
                seen.push(*t);
                true
            }
        });
    }
}

/// `WOW_NO_MESH_PARK=1` — the A/B lever.
pub(super) fn enabled() -> bool {
    std::env::var_os("WOW_NO_MESH_PARK").is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> (World, Entity) {
        let mut w = World::new();
        let e = w
            .spawn((
                ModelPart {
                    kind: super::super::ModelKind::Doodad,
                    blend: benilla_formats::ModelBlend::Opaque,
                },
                Mesh3d(Handle::default()),
                InheritedVisibility::HIDDEN,
            ))
            .id();
        (w, e)
    }

    fn run(w: &mut World, n: usize) {
        let mut sys = IntoSystem::into_system(park_hidden_parts);
        sys.initialize(w);
        for _ in 0..n {
            sys.run((), w).unwrap();
            sys.apply_deferred(w);
        }
    }

    #[test]
    fn a_part_parks_after_the_hysteresis_and_unparks_when_shown() {
        let (mut w, e) = world();
        run(&mut w, PARK_AFTER_FRAMES as usize - 1);
        assert!(w.entity(e).contains::<Mesh3d>(), "not yet");
        run(&mut w, 1);
        assert!(!w.entity(e).contains::<Mesh3d>(), "parked");
        assert!(w.entity(e).contains::<ParkedMesh>());
        w.entity_mut(e).insert(InheritedVisibility::VISIBLE);
        run(&mut w, 1);
        assert!(
            w.entity(e).contains::<Mesh3d>(),
            "unparked on the shown frame"
        );
        assert!(!w.entity(e).contains::<ParkedMesh>());
        assert_eq!(w.entity(e).get::<HiddenFrames>().unwrap().0, 0);
    }

    /// Bevy's `Mesh3d` add hook pushes a `VisibilityClass` entry on every (re)add and
    /// `check_visibility` queues an entity once per entry: without the dedup an unparked part
    /// is drawn twice, three times after the next cycle — the stacking lamppost halo. The
    /// world here registers the hook and the requirement exactly as bevy's plugin does.
    #[test]
    fn an_unparked_part_keeps_one_visibility_class_entry() {
        let mut w = World::new();
        w.register_required_components::<Mesh3d, VisibilityClass>();
        w.register_component_hooks::<Mesh3d>()
            .on_add(bevy::camera::visibility::add_visibility_class::<Mesh3d>);
        let e = w
            .spawn((
                ModelPart {
                    kind: super::super::ModelKind::Doodad,
                    blend: benilla_formats::ModelBlend::Blend,
                },
                Mesh3d(Handle::default()),
                InheritedVisibility::HIDDEN,
            ))
            .id();
        assert_eq!(w.entity(e).get::<VisibilityClass>().unwrap().len(), 1);
        run(&mut w, PARK_AFTER_FRAMES as usize);
        assert!(!w.entity(e).contains::<Mesh3d>(), "parked");
        w.entity_mut(e).insert(InheritedVisibility::VISIBLE);
        run(&mut w, 1);
        assert!(w.entity(e).contains::<Mesh3d>(), "unparked");
        assert_eq!(
            w.entity(e).get::<VisibilityClass>().unwrap().len(),
            1,
            "the re-added Mesh3d's hook pushed a second class entry — the part draws twice"
        );
    }

    #[test]
    fn a_glance_never_parks() {
        let (mut w, e) = world();
        run(&mut w, 10);
        w.entity_mut(e).insert(InheritedVisibility::VISIBLE);
        run(&mut w, 1);
        w.entity_mut(e).insert(InheritedVisibility::HIDDEN);
        run(&mut w, 10);
        assert!(w.entity(e).contains::<Mesh3d>(), "the count restarted");
    }
}
