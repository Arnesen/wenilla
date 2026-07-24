//! The **unit blob shadow** — the soft dark oval under every unit (the player, every NPC, every
//! creature), the reference's per-frame shadow pass rebuilt on the shared surface-decal projector
//! ([`crate::decal`]).
//!
//! **The byte-verified mechanism** (wow-re `unit-blob-shadow.md`, a §5 cross-check; the "cloud
//! shadow" label on `0x6d7920` was corrected — it IS the unit shadow draw):
//! - **Draw path**: a per-frame pass over registered model nodes (`0x683dd0`, list `[0xc7cb10]`)
//!   → gate `0x6d78f0` (model streamed, master toggle) → `0x6d7920` → the **same decal chain the
//!   selection ring uses** (`0x6d7330 → 0x6d6fa0 → 0x6d7480`), collector flags `0x2f0122` = the
//!   ring's `0x200122` **+ the liquid receivers** (a gap here: liquid isn't in the
//!   [`GroundDecalSurface`] set yet — the shadow lands on terrain + WMO faces only).
//! - **Texture**: `Textures\ShadowBlob.blp` — a 32×32 grayscale radial blob (flat gray-160 core,
//!   linear rim to white) under a binary alpha disc. The reference multitextures a procedural 64×8
//!   trapezoid ramp on a second stage (`0x6d81a0`/`0x6d82d0`, blend-mode-selected); its combine
//!   wiring is an open RE item (apitrace) — here the ramp is the vertex-alpha vertical fade below.
//! - **Box law** (`0x711a20` + the `0x6d7920` corner build): a sequence CAaBox, clamped INTO ±5
//!   per axis (a cap, never a floor), scaled by the world matrix, yaw-rotated with the unit's
//!   facing then **axis-aligned-bounded**. Vertical about the model origin: `+1.0·(zExt/2)` up,
//!   `−(5/3)·(zExt/2)` down. A degenerate horizontal box is the reference's no-op exit (no
//!   shadow). **No** `OBJECT_FIELD_SCALE_X` re-read (the transform scale already carries it), no
//!   ring-style `sqrt` compression, no floor. **WHICH sequence — settled at bytes + pixels**
//!   (decision 0316; wow-re `27406d9b`, Q3-ORACLE): the draw re-reads
//!   `playableAnimationLookup[0]` every frame — **slot 0 = Stand for characters, from the file
//!   image, so the value never changes** (not the playing sequence: the director's gait-stable
//!   observation falsified that first reading, and the trace oracle confirmed — 1,682 measured
//!   draws, six bit-stable box sizes, HumanMale 0.9134 × 1.0805 yd permanently, Walk/Run extents
//!   never appear). Full extents, no missing half/scale factor — the standing size IS the law.
//! - **Appearance**: multiplicative darken — `GL_DST_COLOR/GL_ZERO` with the fade riding the
//!   combine, which is exactly Bevy's [`AlphaMode::Multiply`] (`dst × lerp(1, src, α)`). Vertex
//!   diffuse is **white** with α = the model's fade alpha (`[model+0x180]` — spawn/despawn fades +
//!   the self first-person fade ride into the shadow); the darkness lives in the texture RGB.
//!   Unlit, no fog, no depth write. The texture loads as the default `WorldArt` `Rgba8Unorm`, so
//!   the modulate multiplies raw bytes in the gamma lane — the reference's own arithmetic (0161).
//! - **Gating**: the reference's `shadowLOD` cvar {0,1} is the master toggle (default on) — we are
//!   always-on; `shadowBias` (default 0.1) is its depth-bias knob — [`SHADOW_DEPTH_BIAS`] plays
//!   that role here. No dead/mount/kind test exists on the draw path; **which** objects register
//!   for shadows is an open RE item (`HANDOFF(-> object-layer)`) — v1 policy: every Player/Unit
//!   entity with a built animated model (GameObjects/doodads excluded).
//!
//! One decal entity per unit, its mesh rebuilt only when the inputs move ([`ShadowKey`]) — an idle
//! unit costs a key compare per frame.

use avian3d::prelude::Collider;
use benilla_assets::ModelAnimations;
use benilla_protocol::EntityKind;
use bevy::camera::primitives::Aabb;
use bevy::ecs::entity::{EntityHashMap, EntityHashSet};
use bevy::prelude::*;

use crate::collision::GroundDecalSurface;
use crate::creature_anim::AnimData;
use crate::decal::{project_decal, seed_mesh, DecalFrame};
use crate::model_fade::{fade_alpha, RenderFade};
use crate::net::{NetEntity, SelfPlayer};
use crate::player::CameraControl;
use crate::schedule::WorldStage;

/// The reference's shadow disc (`Textures\ShadowBlob.blp`, wow-re unit-blob-shadow RE): grayscale
/// radial blob (gray-160 core → white rim) under a binary alpha disc, multiplied onto the ground.
const SHADOW_TEXTURE: &str = "mpq://textures/shadowblob.blp";
/// Rasterizer depth bias — the same coplanarity fix as the ring's (`RING_DEPTH_BIAS`, see the
/// rationale there; the reference's own knob is the `shadowBias` cvar, default 0.1 → a polygon
/// offset). Half the ring's bias, so where both decals stack the ring wins the depth tie
/// deterministically.
const SHADOW_DEPTH_BIAS: f32 = 4096.0;
/// The byte clamp on the animation box: each corner component is clamped INTO ±5 yd pre-scale
/// (`0x6992c0` MAX(−5) / `0x699250` MIN(+5) — a cap on huge authored boxes, never a floor).
const BOX_CLAMP: f32 = 5.0;
/// Degenerate-box epsilon (the reference's `[0x8029d4]` = 2.384e-7): a zero horizontal extent is
/// the no-op exit — no shadow.
const DEGENERATE_EPS: f32 = 2.384e-7;

/// One unit's shadow decal (a top-level entity — the mesh is world-space, so it must not inherit
/// the owner's transform). Despawned when the owner goes.
#[derive(Component)]
struct BlobShadow {
    owner: Entity,
}

/// Last frame's rebuild inputs — the mesh is re-projected only when one moves. `surfaces` counts
/// the [`GroundDecalSurface`] colliders: a tile streaming in under a *standing* unit changes it,
/// re-arming the rebuild its stillness would otherwise skip.
#[derive(Component, Default)]
struct ShadowKey {
    feet: Vec3,
    rotation: Quat,
    box_min: Vec3,
    box_max: Vec3,
    alpha: f32,
    surfaces: usize,
    shown: bool,
}

/// The one shared shadow material (white base × the blob texture, multiplicative) + its texture
/// handle (kept so the census can report the image's load state — a texture that never arrives
/// blocks the whole material from rendering, silently).
#[derive(Resource)]
struct ShadowMaterial {
    material: Handle<StandardMaterial>,
    texture: Handle<Image>,
}

pub(crate) struct BlobShadowPlugin;

impl Plugin for BlobShadowPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_material).add_systems(
            Update,
            (sync_shadows, update_shadows)
                .chain()
                // After net motion + input: the decal follows this frame's unit transforms.
                .after(WorldStage::Input),
        );
    }
}

/// Build the shared multiply material once. `AlphaMode::Multiply` premultiplies in-shader and
/// blends `dst × src + dst × (1−α)` = `dst × lerp(1, srcRGB, α)` — the reference's
/// `GL_DST_COLOR/GL_ZERO` modulate with the fade riding the combine (see module docs).
fn setup_material(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    asset_server: Res<AssetServer>,
) {
    let texture = asset_server.load::<Image>(SHADOW_TEXTURE);
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: Some(texture.clone()),
        // Unlit (the reference's shadow pass: lighting off, fog off) — the darkening is pure
        // framebuffer arithmetic, independent of time-of-day.
        unlit: true,
        alpha_mode: AlphaMode::Multiply,
        cull_mode: None,
        depth_bias: SHADOW_DEPTH_BIAS,
        ..default()
    });
    commands.insert_resource(ShadowMaterial { material, texture });
}

/// Keep one shadow decal per eligible unit: spawn for new Player/Unit entities whose model has
/// built (an animated model — [`ModelAnimations`] arrives with it), despawn orphans (owner
/// destroyed / streamed out). The registration *policy* is the open RE item; this is the v1 set
/// (see module docs).
#[allow(clippy::type_complexity)] // the filtered spawn-gate query, commented inline
fn sync_shadows(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    material: Option<Res<ShadowMaterial>>,
    // A mount child never gets its own decal: its `Transform` is parent-relative (a shadow
    // keyed on it would project at the world origin) — the mounted composite casts ONE shadow,
    // the unit's, which reads the mount's box while mounted (`update_shadows`, decision 0441).
    units: Query<
        (Entity, &NetEntity),
        (
            With<ModelAnimations>,
            Without<crate::entities::mount::MountBody>,
        ),
    >,
    shadows: Query<(Entity, &BlobShadow)>,
) {
    let Some(material) = material else {
        return;
    };
    let mut shadowed = EntityHashSet::default();
    for (entity, shadow) in &shadows {
        // Owner gone or no longer eligible (model torn down) → the decal goes with it.
        if units.get(shadow.owner).is_err() {
            commands.entity(entity).despawn();
        } else {
            shadowed.insert(shadow.owner);
        }
    }
    for (owner, net) in &units {
        if !matches!(net.kind, EntityKind::Player | EntityKind::Unit) || shadowed.contains(&owner) {
            continue;
        }
        commands.spawn((
            BlobShadow { owner },
            ShadowKey::default(),
            Mesh3d(meshes.add(seed_mesh())),
            MeshMaterial3d(material.material.clone()),
            Transform::default(),
            Visibility::Hidden,
        ));
    }
}

/// Re-project each shadow whose inputs moved; hide it when the box degenerates, the fade reaches
/// zero, or no receiving surface is in the box (the reference's no-ground gate).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn update_shadows(
    time: Res<Time>,
    mut meshes: ResMut<Assets<Mesh>>,
    catalog: Option<Res<AnimData>>,
    rig: Option<Res<CameraControl>>,
    shadow_material: Option<Res<ShadowMaterial>>,
    images: Res<Assets<Image>>,
    surfaces: Query<&Collider, With<GroundDecalSurface>>,
    // Spawn/despawn fades live on the model *part* entities; attribute each to its unit root so
    // the shadow can ride the same alpha the body renders with (O(#currently-fading parts) — zero
    // in the steady state).
    fades: Query<(Entity, &RenderFade)>,
    parents: Query<&ChildOf>,
    owners: Query<
        (
            &Transform,
            &ModelAnimations,
            Has<SelfPlayer>,
            Option<&crate::entities::mount::MountChild>,
        ),
        Without<BlobShadow>,
    >,
    // The mounted box source: the composite's one shadow reads the MOUNT's Stand box at the
    // mount's rendered scale while a mount model is attached (the mount IS the footprint on the
    // ground; the rider's box would undersize it). The mount-vs-body source of the client's own
    // shadow box is untraced — this is the named approximation of decision 0441's P2, carried
    // until a wow-re shadow-consumer trace pins it.
    mount_anims: Query<(&NetEntity, &ModelAnimations), With<crate::entities::mount::MountBody>>,
    mut commands: Commands,
    mut shadows: Query<(
        Entity,
        &BlobShadow,
        &mut ShadowKey,
        &mut Transform,
        &mut Visibility,
        &Mesh3d,
    )>,
    // Once-a-second census at debug level (`RUST_LOG=benilla::blob_shadow=debug`): how many
    // shadows exist and why the hidden ones hid — the first question of any "no shadow under X"
    // report, answerable from a log instead of a debugger.
    mut census_at: Local<f32>,
) {
    let now = time.elapsed_secs();
    let census = now >= *census_at;
    if census {
        *census_at = now + 1.0;
    }
    let (mut n_total, mut n_shown, mut n_no_owner, mut n_no_clip, mut n_degen, mut n_no_ground) =
        (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
    let mut root_fade: EntityHashMap<f32> = EntityHashMap::default();
    for (part, fade) in &fades {
        let alpha = fade_alpha(fade.from, fade.to, (now - fade.started) / fade.duration);
        let mut root = part;
        while let Ok(child_of) = parents.get(root) {
            root = child_of.parent();
        }
        let slot = root_fade.entry(root).or_insert(1.0);
        *slot = slot.min(alpha);
    }
    let surface_count = surfaces.iter().count();
    for (entity, shadow, mut key, mut transform, mut visibility, mesh) in &mut shadows {
        n_total += 1;
        let Ok((unit, anims, is_self, mount_child)) = owners.get(shadow.owner) else {
            // sync_shadows despawns next frame; keep it invisible meanwhile.
            *visibility = Visibility::Hidden;
            n_no_owner += 1;
            continue;
        };
        // Mounted: the box and the extra scale column come from the mount child (the
        // `mount_anims` doc above); until its model lands, the rider's own box carries the frame.
        let (anims, extra_scale) = match mount_child.and_then(|mc| mount_anims.get(mc.0).ok()) {
            Some((mnet, manims)) => (manims, mnet.scale),
            None => (anims, 1.0),
        };
        // The byte+pixel law (0316, wow-re 27406d9b): the box is playableAnimationLookup[0]'s
        // sequence — Stand, permanently (the reference re-reads it per frame from the file image;
        // the value can't change). resolve(0) walks the same baked table, so Stand-less models
        // land on their substitute exactly like the binary's row-0 fast path.
        let stand = catalog.as_deref().map_or(0, |c| anims.resolve(0, &c.0).id);
        let clip = anims.find(stand);
        let Some(clip) = clip else {
            hide(&mut key, &mut visibility);
            n_no_clip += 1;
            continue;
        };
        // The box law (see module docs): clamp INTO ±5 pre-scale, scale, yaw-rotate + AA-bound.
        let s = (unit.scale.x * extra_scale).max(0.0);
        let bmin = clip
            .bounds_min
            .clamp(Vec3::splat(-BOX_CLAMP), Vec3::splat(BOX_CLAMP))
            * s;
        let bmax = clip
            .bounds_max
            .clamp(Vec3::splat(-BOX_CLAMP), Vec3::splat(BOX_CLAMP))
            * s;
        if bmax.x - bmin.x <= DEGENERATE_EPS || bmax.z - bmin.z <= DEGENERATE_EPS {
            // The reference's degenerate-box no-op exit (`0x61e9c0`): no shadow.
            hide(&mut key, &mut visibility);
            n_degen += 1;
            continue;
        }
        let mut alpha = root_fade.get(&shadow.owner).copied().unwrap_or(1.0);
        if is_self {
            // The self first-person fade rides the same model-fade slot in the reference.
            alpha *= rig.as_deref().map_or(1.0, CameraControl::self_fade);
        }
        if alpha <= 0.0 {
            hide(&mut key, &mut visibility);
            continue;
        }
        let next = ShadowKey {
            feet: unit.translation,
            rotation: unit.rotation,
            box_min: bmin,
            box_max: bmax,
            alpha,
            surfaces: surface_count,
            shown: true,
        };
        if key.shown && !key_changed(&key, &next) {
            n_shown += 1;
            continue;
        }
        // Horizontal: the model box's 4 rect corners through the unit's rotation with the
        // vertical column dead (`rot × (x, 0, z)`, XZ taken — the byte build zeroes the z-terms),
        // then axis-aligned-bounded.
        let (mut min_x, mut max_x) = (f32::MAX, f32::MIN);
        let (mut min_z, mut max_z) = (f32::MAX, f32::MIN);
        for (x, z) in [
            (bmin.x, bmin.z),
            (bmin.x, bmax.z),
            (bmax.x, bmin.z),
            (bmax.x, bmax.z),
        ] {
            let w = unit.rotation * Vec3::new(x, 0.0, z);
            (min_x, max_x) = (min_x.min(w.x), max_x.max(w.x));
            (min_z, max_z) = (min_z.min(w.z), max_z.max(w.z));
        }
        // Vertical about the model origin: `+1.0·(zExt/2)` up, `−(5/3)·(zExt/2)` down (the byte
        // constants `[0xcea60c]`/`[0xcea610]`).
        let half_v = (bmax.y - bmin.y) * 0.5;
        let frame = DecalFrame {
            center: unit.translation,
            sin: 0.0,
            cos: 1.0,
            min_x,
            max_x,
            min_z,
            max_z,
            min_y: -half_v * (5.0 / 3.0),
            max_y: half_v,
        };
        let span_v = frame.max_y - frame.min_y;
        let projected = span_v > 0.0
            && project_decal(
                &mut meshes,
                mesh,
                &surfaces,
                &frame,
                |p| {
                    // The trapezoid ramp over the vertical span (the reference's second texture
                    // stage, `0x6d81a0`: rise x<2, flat, fall x≥10 over x = 12·u). *Interim
                    // seat*: that the ramp runs vertically is inferred from the box's asymmetric
                    // vertical reach; the combine wiring is the flagged apitrace item.
                    alpha * shadow_ramp((p.y - frame.min_y) / span_v)
                },
                |x, z| frame.rect_uv(x, z),
            );
        if projected {
            transform.translation = unit.translation;
            // A hand-set cull volume: the mesh is rewritten in place and Bevy won't recompute an
            // entity's `Aabb` on asset change — unlike the single ring, dozens of shadows want
            // real frustum culling, so the box is maintained here.
            commands.entity(entity).insert(Aabb::from_min_max(
                Vec3::new(min_x, frame.min_y, min_z),
                Vec3::new(max_x, frame.max_y, max_z),
            ));
        }
        *key = next;
        key.shown = projected;
        if projected {
            n_shown += 1;
        } else {
            n_no_ground += 1;
        }
        *visibility = if projected {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    if census && n_total > 0 {
        let tex = shadow_material.map_or("no-resource", |m| {
            if images.contains(&m.texture) {
                "loaded"
            } else {
                "MISSING"
            }
        });
        debug!(
            "blob shadows: {n_total} ({n_shown} shown, {n_no_owner} ownerless, {n_no_clip} \
             no-clip, {n_degen} degenerate, {n_no_ground} no-ground; {surface_count} surfaces; \
             texture {tex})"
        );
    }
}

/// Hide the decal and drop the cache key so the next eligible frame rebuilds from scratch.
fn hide(key: &mut ShadowKey, visibility: &mut Visibility) {
    key.shown = false;
    *visibility = Visibility::Hidden;
}

/// Did any rebuild input move beyond noise? Position/box at a millimetre, rotation at ~0.05°,
/// alpha at under a colour step.
fn key_changed(a: &ShadowKey, b: &ShadowKey) -> bool {
    const POS_EPS: f32 = 1e-3;
    a.feet.distance_squared(b.feet) > POS_EPS * POS_EPS
        || a.rotation.angle_between(b.rotation) > 1e-3
        || (a.box_min - b.box_min).abs().max_element() > POS_EPS
        || (a.box_max - b.box_max).abs().max_element() > POS_EPS
        || (a.alpha - b.alpha).abs() > 1.0 / 255.0
        || a.surfaces != b.surfaces
}

/// The reference's trapezoid alpha ramp (`0x6d81a0`/`0x6d82d0`, diffed bit-exact in wow-re:
/// `x = 12·u` — rise `x<2 → x/2`, flat `2≤x<10 → 1`, fall `x≥10 → (12−x)/2`, clamped at 0).
fn shadow_ramp(u: f32) -> f32 {
    let x = 12.0 * u.clamp(0.0, 1.0);
    if x < 2.0 {
        0.5 * x
    } else if x < 10.0 {
        1.0
    } else {
        (0.5 * (12.0 - x)).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ramp's byte-verified shape: rise to 1 at x=2 (u=1/6), flat through x=10 (u=5/6), fall
    /// to 0 at x=12 (u=1).
    #[test]
    fn ramp_matches_reference_trapezoid() {
        assert_eq!(shadow_ramp(0.0), 0.0);
        assert!((shadow_ramp(1.0 / 12.0) - 0.5).abs() < 1e-6); // x=1 → 0.5
        assert!((shadow_ramp(1.0 / 6.0) - 1.0).abs() < 1e-6); // x=2 → 1.0
        assert_eq!(shadow_ramp(0.5), 1.0);
        assert!((shadow_ramp(5.0 / 6.0) - 1.0).abs() < 1e-6); // x=10 → 1.0
        assert!((shadow_ramp(11.0 / 12.0) - 0.5).abs() < 1e-6); // x=11 → 0.5
        assert_eq!(shadow_ramp(1.0), 0.0);
        // Out-of-range clamps, never negative.
        assert_eq!(shadow_ramp(-1.0), 0.0);
        assert_eq!(shadow_ramp(2.0), 0.0);
    }

    /// The box law: clamp INTO ±5 pre-scale (a cap, not a floor), then scale.
    #[test]
    fn box_clamp_caps_pre_scale() {
        let raw = Vec3::new(-7.0, 0.0, 3.0);
        let clamped = raw.clamp(Vec3::splat(-BOX_CLAMP), Vec3::splat(BOX_CLAMP));
        assert_eq!(clamped, Vec3::new(-5.0, 0.0, 3.0));
        // A scale-2 unit's clamped box still doubles — the cap is pre-scale.
        assert_eq!(clamped * 2.0, Vec3::new(-10.0, 0.0, 6.0));
    }
}
