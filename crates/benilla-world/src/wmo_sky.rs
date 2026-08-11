//! The **WMO skybox** — the authored sky a building swaps in for the `Light.dbc` gradient dome while
//! the camera stands in one of its own groups.
//!
//! A WMO root can name a skybox model in its **MOSB** chunk, and a group can ask for it with group
//! flag **`0x40000`**. Both halves matter, and the flag is tested **on the groups the portal flood
//! REACHES, never on the group the camera stands in** — `0x6b42e0` sits inside the flood `0x6b41c0`
//! with `ebx` = the group being *visited*, seeded from the containment list on the inside leg and
//! from every frustum-visible EXTERIOR group on the outside leg (`0x6b3dd0`), recursing through
//! portals at `0x6b4639` and re-testing at each step. The predicate is therefore
//! *"any flood-reached group carries the bit, and the root names a MOSB"*, published to `[0xca8080]`
//! and read once at `0x681282`.
//!
//! **That distinction is load-bearing, and getting it wrong is what shipped first.** In Stratholme's
//! King's Square the camera stands in group 39 — the root's *only* EXTERIOR (`0x8`) group, which does
//! **not** set `0x40000`. A containing-group test draws no skybox there; the reference draws the
//! painted red sky, because 61 of the 83 groups its flood reaches from group 39 do carry the bit.
//! (Checked on the asset: a BFS over `Stratholme_B`'s MOPR reaches 82 of 83 groups from group 39.)
//! The corpus correlation that seeded the first attempt — across all 815 roots the bit never appears
//! without a MOSB, 0 counter-examples (`benilla-extract skyboxscan`) — was true and still is; it just
//! never established *which* group the renderer tests, and it was over-read as if it had.
//!
//! Five roots exercise both halves: the four Caverns of Time shells (unreleased in 1.12) and
//! **`Stratholme_B`**, the burning city. Four more name a skybox no group ever asks for (DireMaul's
//! instance shell, `Stratholme_A`, and the two Sunken Temple roots — whose MOSB isn't even a model
//! path, it's the string "the temple of atal'hakkar"), so keying off the chunk alone would still
//! paint skies the reference never shows. This is why Stratholme's sky is red where the zone light
//! says otherwise: map 329's only reachable `Light.dbc` atmosphere (global row 341 → `LightParams`
//! 336) is a khaki-brown gradient with a near-black apex, and it is not what draws in there.
//!
//! What we draw is the model as authored: `StratholmeSkybox.m2` is a static, emissive, two-sided cube
//! (three batches × 8 verts, one texture pair per axis), anchored at the camera with identity rotation
//! — the same treatment [`crate::sky`] gives its dome. Occlusion is **not** the box's radius: like
//! every sky element it forces the far depth (`wmo_skybox.wgsl`; the law is in [`crate::sky_order`]),
//! so the world paints over it and the 26-yard shell is free to sit inside the room's own geometry.
//!
//! **Scope — the skybox replaces the WHOLE celestial pass, not just the backdrop.** `CSky::Render`
//! carries one shared boolean (`0x6d49cd` sets it, `0x6d49fb`/`0x6d4a2e` clear it once any slot's
//! weight exceeds `[0x808aac]` = 0.99) and `0x6d4a3b test edi,edi; je` skips **all six** element
//! draws together — stars, sun disc, both moons, gradient band and cloud dome. There is no
//! per-element gating. Confirmed in a live GL capture of the reference standing in King's Square:
//! the entire `[0.975, 0.98]` sky slice is three `count=12` draws (the cube's three texture pairs)
//! and nothing else, identically in every frame. Only the **glare** quads survive — they render on
//! their own later path (`0x483740 → 0x6d48c0 → 0x7e57e0`), outside this pass.
//!
//! What the atmosphere drives (fog colour and distance, ambient, diffuse) is still untouched: no byte
//! law says the flag reaches it, and the reference fogs the world normally underneath the painted sky.

use std::collections::HashSet;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
    MaterialPlugin,
};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

use crate::view::WorldCamera;
use benilla_assets::coords::wow_to_bevy;
use benilla_assets::WmoModel;
use benilla_assets::{LockRecover, WorldAssets};

/// MOGP/MOGI group flag **SHOW_SKYBOX** — this group draws its root's MOSB model as the sky (see the
/// module doc for how the bit was identified). Mirrored between the root's MOGI table and each group
/// file's MOGP header; we read the MOGP copy the loader already keeps in `WmoGroupNav::flags`.
const SHOW_SKYBOX: u32 = 0x40000;

/// Unlit textured backdrop over a `StandardMaterial` shell — the shell supplies `unlit` +
/// `cull_mode: None` (the box is viewed from inside) and the texture; the extension's fragment shader
/// emits the texel raw and forces the sky pass's far depth.
pub type WmoSkyboxMaterial = ExtendedMaterial<StandardMaterial, WmoSkyboxExt>;

/// No per-material uniforms — the fragment reads the base-colour texture and nothing else.
#[derive(Asset, AsBindGroup, Clone, TypePath, Default)]
pub struct WmoSkyboxExt {}

impl MaterialExtension for WmoSkyboxExt {
    fn fragment_shader() -> ShaderRef {
        "shaders/wmo_skybox.wgsl".into()
    }

    /// Depth-write OFF, exactly as [`crate::sky::SkyExt`] does it: the backdrop must leave the
    /// z-buffer at its clear value over sky pixels so the forced-far glare quads stay occluded by
    /// *world* geometry alone.
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        _layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = false;
        }
        Ok(())
    }
}

/// The skybox model the camera's room asks for this frame — `None` (the overwhelming default) means
/// the [`crate::sky`] gradient dome is the backdrop. Resolved from [`CameraInteriorClaim`]: the same
/// down-ray seed that already names the camera's room for the MFOG fog resolve.
#[derive(Resource, Default, PartialEq, Eq)]
pub struct CameraWmoSkybox(pub Option<String>);

/// Marks one batch of a built skybox model, tagged with the model path it belongs to (a session can
/// walk through more than one skybox building, and the built entities are cached, not rebuilt).
#[derive(Component)]
struct WmoSkyboxPart(String);

/// Which skybox paths have been built already — a build is a chain read + BLP decodes, so it happens
/// once per path per session and the entities are then just shown/hidden. A path that FAILED to load
/// is recorded here too: the retry would fail identically every frame, and the gradient dome is the
/// correct thing to fall back to.
#[derive(Resource, Default)]
struct BuiltSkyboxes(HashSet<String>);

/// Ordering handle: [`CameraWmoSkybox`] is settled for the frame after this set. `crate::sky`'s dome
/// gate hangs off it, because the two backdrops must agree *within* a frame — one reading a stale
/// resource is a frame with both drawn or neither.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct WmoSkyResolve;

/// The WMO-skybox subsystem: resolve which model the camera's room wants, build it on first need,
/// then show exactly that one and pin it to the camera.
pub(crate) struct WmoSkyPlugin;

impl Plugin for WmoSkyPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<WmoSkyboxMaterial>::default())
            .init_resource::<CameraWmoSkybox>()
            .init_resource::<BuiltSkyboxes>()
            .add_systems(
                Update,
                (resolve_camera_skybox, build_skybox, apply_skybox_visibility)
                    .chain()
                    // The claim we read is written by the PVS pass in this same schedule. Unordered,
                    // this reads whichever side of it the executor happened to pick — so a camera
                    // move that changes rooms lands a frame late, or not at all on the frame it
                    // matters, and the backdrop flickers between the painted sky and the gradient.
                    .after(crate::wmo_portal::WmoPvsSet)
                    .in_set(WmoSkyResolve),
            )
            // Camera-anchored placement runs post-propagation off the SAME-frame camera pose — the
            // slot decision 0504 moved every camera-anchored shell into.
            .add_systems(
                PostUpdate,
                follow_camera.in_set(crate::billboard::BillboardPlace),
            );
    }
}

/// Which skybox does this frame ask for? **The flag is tested on the groups the portal flood
/// REACHES, never on the group the camera stands in** (`0x6b42e0`, inside the flood `0x6b41c0`, with
/// `ebx` = the group being visited). So the predicate is: *does any group in this placement's PVS
/// carry `SHOW_SKYBOX`, and does its root name a MOSB?*
///
/// That distinction is the whole bug this replaced. Reading the **containing** group instead is
/// wrong twice over at Stratholme's King's Square: the camera stands in group 39, the root's only
/// EXTERIOR (`0x8`) group, which does not set `SHOW_SKYBOX` — and [`CameraInteriorClaim`] is
/// deliberately `None` over an EXTERIOR group's floor, so the old resolve could not even see the
/// placement. The reference draws the painted sky there regardless, because 61 of the 83 groups the
/// flood reaches from group 39 do carry the bit. (Verified on the asset: BFS over `Stratholme_B`'s
/// MOPR reaches 82 of 83 groups from group 39, 61 of them flagged.)
fn resolve_camera_skybox(
    instances: Query<&crate::wmo_portal::WmoPortalInstance>,
    wmos: Res<Assets<WmoModel>>,
    mut want: ResMut<CameraWmoSkybox>,
) {
    // `min()` rather than "first match": `Query` iteration order is not stable across frames, and a
    // tie would otherwise alternate two backdrops frame to frame. Only one 1.12 root can qualify
    // (Stratholme_B — the four Caverns of Time shells are unreleased), so this never actually picks.
    let resolved = instances
        .iter()
        .filter_map(|inst| {
            let model = wmos.get(&inst.handle)?;
            // The MOSB test comes first and exits ~every instance in one deref: 810 of the chain's
            // 815 roots name no skybox, so the group scan below is effectively never reached.
            let sky = model.skybox.as_deref()?;
            model
                .group_nav
                .iter()
                .enumerate()
                .any(|(i, nav)| {
                    // NOT the fail-open `unwrap_or(true)` the rest of the cull takes
                    // ([`WmoGroupVis::drawn_by`]): there a lookup miss must never blank a building,
                    // whereas here it would paint a full-screen sky over the entire world.
                    nav.flags & SHOW_SKYBOX != 0 && inst.visible.get(i).copied().unwrap_or(false)
                })
                .then(|| sky.to_owned())
        })
        .min();
    if want.0 != resolved {
        want.0 = resolved;
    }
}

/// Build the wanted skybox's entities the first time it is asked for. The model is tiny (three static
/// batches) and there are five in the whole game, so a built one is kept for the session rather than
/// torn down on leaving the room — walking in and out of Stratholme's gate must not re-decode art.
fn build_skybox(
    mut commands: Commands,
    want: Res<CameraWmoSkybox>,
    mut built: ResMut<BuiltSkyboxes>,
    world_assets: Option<ResMut<WorldAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<WmoSkyboxMaterial>>,
) {
    let Some(path) = want.0.as_deref() else {
        return;
    };
    if built.0.contains(path) {
        return;
    }
    let Some(mut world_assets) = world_assets else {
        return; // assetless dev run — the gradient dome stays the backdrop
    };
    let subs = benilla_formats::load_m2_mesh(&mut world_assets.chain.lock_recover(), path);
    let subs = match subs {
        Ok(subs) if !subs.is_empty() => subs,
        Ok(_) => {
            warn!("WMO skybox '{path}' has no render batches — keeping the gradient dome");
            built.0.insert(path.to_string());
            return;
        }
        Err(e) => {
            warn!("WMO skybox '{path}' failed to load, keeping the gradient dome: {e:#}");
            built.0.insert(path.to_string());
            return;
        }
    };
    for sub in &subs {
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        let positions: Vec<[f32; 3]> = sub
            .positions
            .iter()
            .map(|p| wow_to_bevy(*p).to_array())
            .collect();
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, sub.uvs.clone());
        mesh.insert_indices(Indices::U32(sub.indices.clone()));
        // The batch's own authored address mode (decision 0763) — a skybox's UVs sit inside 0..1, but
        // reading the flags costs nothing and keeps this lane off a private convention.
        let texture = sub
            .texture
            .as_deref()
            .and_then(|t| world_assets.texture(t, (sub.wrap_x, sub.wrap_y), &mut images));
        let material = materials.add(WmoSkyboxMaterial {
            base: StandardMaterial {
                base_color_texture: texture,
                unlit: true,
                cull_mode: None, // authored two-sided, and we view the box from inside
                ..default()
            },
            extension: WmoSkyboxExt {},
        });
        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material),
            Transform::default(),
            Visibility::Hidden, // `apply_skybox_visibility` turns on exactly the wanted one
            WmoSkyboxPart(path.to_string()),
        ));
    }
    built.0.insert(path.to_string());
}

/// Show the wanted skybox's batches and hide every other built one. This is the sole `Visibility`
/// writer for these entities (the gradient dome's own gate lives in [`crate::sky`], which reads the
/// same resource — one authority per entity class, decision 0025).
fn apply_skybox_visibility(
    want: Res<CameraWmoSkybox>,
    mut parts: Query<(&WmoSkyboxPart, &mut Visibility)>,
) {
    for (part, mut vis) in &mut parts {
        let show = want.0.as_deref() == Some(part.0.as_str());
        let target = if show {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *vis != target {
            *vis = target;
        }
    }
}

/// Pin the box to the camera, world-aligned (identity rotation) so the painted sky stays fixed to the
/// world horizon however the camera turns — the same treatment [`crate::sky::follow_camera`] gives the
/// gradient dome, minus its far-plane scaling.
///
/// The art is drawn at **authored scale** and that is deliberate. Every other shell here scales to a
/// fraction of the far plane, but those radii were only ever standing in for occlusion, and the forced
/// far depth ([`crate::sky_order`], "The depth law") retired that job. Scale is genuinely free here:
/// the eye's offset inside the box scales with the box, so the *angles* the faces subtend — which is
/// all a camera-anchored backdrop can show — are scale-invariant.
///
/// **The ANCHOR POINT is not free — and it is the model's ORIGIN, VERIFIED.** `CM2Model+0xbc` stays
/// identity and `0x707680` is called with a **zeroed** recentre vector (`0x6d4b3a`–`0x6d4b48`) where
/// the world M2 scene gets the live camera position — so the model's local origin sits exactly at the
/// eye, with camera rotation and zero translation. (The capture agrees: the skybox's matrix is
/// orthonormal with translation `(0,0,0)` while the very next model's carries a real offset.)
///
/// It matters because `StratholmeSkybox` is *not* centred on its origin: its 52.87 yd cube sits at
/// z ∈ [−14.67, +38.20], leaving the eye 11.76 yd BELOW the box's centre. That asymmetry is authored,
/// not incidental — the near-black ±Z pair covers only a **34.7°** cone about the zenith rather than
/// a symmetric 45°, and the side pairs' painted horizon lands three-quarters down their gradient
/// (v ≈ 0.72). Re-centring the box on the eye would be a visible change and a wrong one.
#[allow(clippy::type_complexity)]
fn follow_camera(
    cam: Query<&GlobalTransform, With<WorldCamera>>,
    mut parts: Query<
        (&mut Transform, &mut GlobalTransform),
        (With<WmoSkyboxPart>, Without<WorldCamera>),
    >,
) {
    let Some(cam_gt) = cam.iter().next() else {
        return;
    };
    for (mut tf, mut gt) in &mut parts {
        tf.translation = cam_gt.translation();
        tf.rotation = Quat::IDENTITY;
        tf.scale = Vec3::ONE;
        // Propagation already ran this frame — the direct global write is what renders.
        *gt = GlobalTransform::from(*tf);
    }
}
