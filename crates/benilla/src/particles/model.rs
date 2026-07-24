//! **3-D MODEL particles** (wow-re `part-model-particles.md`, VERIFIED): an emitter whose
//! record names a geometry model renders each live particle as a tiny 3-D instance of that
//! model — quaternion-oriented, tumbling, over-life scaled and tinted — instead of a billboard
//! quad (Whirlwind's blades, Cone of Cold's shards, the cyclones, Death Wish). The sim
//! integrates position + spin ([`super::sim`]); this module owns the DRAW side: a per-emitter
//! pool of instance entities grown on demand, positioned after the sim each frame.
//!
//! Draw law (`0x7b4840` → `0x7b4510`): per particle, the instance transform is the particle
//! quaternion (× the emitter frame) at the particle position, scaled by the over-life SIZE
//! ramp; over-life COLOR rides a per-instance tint clone (`WowModelExt::tint`, the fx-tint
//! mechanism) and over-life ALPHA rides the per-instance `MeshTag` alpha field. The
//! reference's optional per-emitter depth sort (rt+0x1ac & 0x10) is folded into the pool-order
//! simplification named in decision 0148. The reference draws each instance through its
//! generic model-render pass; our instances are the geometry model's submeshes with their own
//! materials — mini-model *animation* (a rigged geometry model) is not run: every spell-corpus
//! geometry target probed is a static mesh, and a rigged one would surface as a visibly stiff
//! instance, the recorded trigger to extend this.

use benilla_assets::coords::wow_to_bevy;
use benilla_assets::M2Model;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::MeshTag;
use bevy::prelude::*;

use crate::model_render::{model_material, MaterialCache, ShadeSel};
use crate::terrain::WowModelMaterial;

use super::{ChildDraw, ParticleEmitter};

/// A hard cap on one emitter's instance pool — far above any authored steady state
/// (rate ≤ ~25/s × life ≤ 3 s across the corpus); particles past it simply aren't drawn.
const MAX_INSTANCES: usize = 128;

/// One pooled instance slot: the geometry model's submeshes as world-root [`ChildDraw`]
/// entities (flat — no hierarchy, so the sim-frame transform write is exact), each with its
/// per-instance tint-clone material.
pub(super) struct ModelInstance {
    pub(super) meshes: Vec<(Entity, Handle<WowModelMaterial>)>,
}

/// Grow + position each model-particle emitter's instance pool from its freshly-simulated
/// pool. Runs after [`super::sim::simulate_particles`] in the same set, so instances land on
/// this frame's positions (the anchored-exactness rule).
pub(super) fn update_model_particles(
    mut commands: Commands,
    models: Res<Assets<M2Model>>,
    mut materials: ResMut<Assets<WowModelMaterial>>,
    light: Res<crate::lighting::SharedLightBuffer>,
    mut emitters: Query<&mut ParticleEmitter>,
    mut draws: Query<
        (
            &mut Transform,
            &mut GlobalTransform,
            &mut Visibility,
            &mut MeshTag,
        ),
        With<ChildDraw>,
    >,
) {
    for mut emitter in &mut emitters {
        let Some(geometry) = emitter.geometry.clone() else {
            continue;
        };
        let Some(model) = models.get(&geometry) else {
            continue; // still loading — particles simulate meanwhile, nothing draws yet
        };
        // Grow the pool to the live count (bounded).
        let want = emitter.particles.len().min(MAX_INSTANCES);
        while emitter.model_instances.len() < want {
            let meshes = model
                .submeshes
                .iter()
                .map(|sub| {
                    // A fresh (never-deduped) material per instance — its `tint` is mutated
                    // every frame by the over-life ramp, so instances must not share.
                    let mut throwaway = MaterialCache::default();
                    let material = model_material(
                        &mut throwaway,
                        &mut materials,
                        sub.texture.clone(),
                        sub.blend,
                        sub.two_sided,
                        false,
                        false,
                        sub.emissive,
                        sub.additive,
                        false,
                        sub.no_depth_write,
                        sub.no_depth_test,
                        sub.fog_policy,
                        // The same LIT lane as every entity M2 (the §9 chain).
                        ShadeSel::Lit,
                        0,
                        None,
                        None, // over-life tint owns the channel — never the authored M2Color loop
                        None,
                        None,
                        false,
                        &light.0,
                    );
                    let entity = commands
                        .spawn((
                            Mesh3d(sub.mesh.clone()),
                            MeshMaterial3d(material.clone()),
                            Transform::IDENTITY,
                            Visibility::Hidden,
                            NoFrustumCulling,
                            MeshTag(crate::mesh_tag::alpha_bits(1.0)),
                            ChildDraw,
                        ))
                        .id();
                    (entity, material)
                })
                .collect();
            emitter.model_instances.push(ModelInstance { meshes });
        }
        // Position live slots; hide the rest.
        let anchored = !emitter.def.model_space();
        let inst_scale = if emitter.def.scale_size_by_instance() {
            emitter.placement.scale.x.max(1e-4)
        } else {
            1.0
        };
        for (i, slot) in emitter.model_instances.iter().enumerate() {
            let Some(p) = emitter.particles.get(i) else {
                for (e, _) in &slot.meshes {
                    if let Ok((_, _, mut vis, _)) = draws.get_mut(*e) {
                        if *vis != Visibility::Hidden {
                            *vis = Visibility::Hidden;
                        }
                    }
                }
                continue;
            };
            let u = (p.age / emitter.def.lifespan).clamp(0.0, 1.0);
            let (rgba, size, _) = emitter.def.over_life.sample(u);
            let tf = if anchored {
                Transform {
                    translation: emitter.anchor_pos + emitter.attach_rot * p.pos,
                    rotation: emitter.attach_rot * p.quat,
                    scale: Vec3::splat(size * inst_scale),
                }
            } else {
                Transform {
                    translation: emitter
                        .placement
                        .transform_point(wow_to_bevy(p.pos.to_array())),
                    rotation: emitter.placement.rotation * p.quat,
                    scale: Vec3::splat(size * inst_scale),
                }
            };
            for (e, mat) in &slot.meshes {
                if let Ok((mut t, mut g, mut vis, mut tag)) = draws.get_mut(*e) {
                    *t = tf;
                    *g = GlobalTransform::from(tf);
                    if *vis != Visibility::Inherited {
                        *vis = Visibility::Inherited;
                    }
                    *tag = MeshTag(crate::mesh_tag::alpha_bits(rgba[3]));
                }
                if let Some(m) = materials.get_mut(mat) {
                    m.extension.tint = Vec4::new(rgba[0], rgba[1], rgba[2], 1.0);
                }
            }
        }
    }
}
