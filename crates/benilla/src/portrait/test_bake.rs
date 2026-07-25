//! The `WOW_PORTRAIT_TEST` debug bake — the booth pipeline's server-less eyeball harness
//! (`WOW_PORTRAIT_TEST=<Model\\Path.mdx>` + optional `WOW_PORTRAIT_TEST_SKIN=<blp>`): bake that
//! model into every slot (portraits + the paper doll's body framing) and own the booths (the live
//! syncs and the 0540 demand gate both stand down). Split from `mod.rs` — the harness concern,
//! not the live bake path.

use benilla_assets::M2Model;
use bevy::prelude::*;

use super::booth::{spawn_booth_model, BoothMotion, BoothPart};
use super::framing::{body_frame, frame, head_anchor, PortraitAnchors};
use super::{aim, test_mode, BoothCam, BoothLight, Booths, PaperDollBooth, PAPERDOLL_SLOT, SLOTS};
use crate::model_render::m2_url;
use crate::terrain::WowModelMaterial;

/// The debug bake driver: when `WOW_PORTRAIT_TEST` is set, bake the named model into every slot once
/// it loads, and own the booths (the live sync yields). See [`bake_test`].
#[allow(clippy::too_many_arguments)]
pub(super) fn sync_test_portraits(
    mut commands: Commands,
    booths: Res<Booths>,
    booth_light: Res<BoothLight>,
    m2s: Res<Assets<M2Model>>,
    asset_server: Res<AssetServer>,
    mut wow_mats: ResMut<Assets<WowModelMaterial>>,
    mut test_cache: Local<crate::model_render::MaterialCache>,
    mut test_handle: Local<Option<Handle<M2Model>>>,
    mut test_done: Local<bool>,
    mut env_cache: Local<Option<bool>>,
    mut cams: Query<(&BoothCam, &mut Transform, &mut Projection)>,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
) {
    if !test_mode(&mut env_cache) || *test_done {
        return;
    }
    let path = std::env::var("WOW_PORTRAIT_TEST").expect("gated by test_mode");
    if bake_test(
        &mut commands,
        &booths,
        &path,
        &asset_server,
        &m2s,
        &mut wow_mats,
        &booth_light,
        &mut test_cache,
        &mut test_handle,
        &mut cams,
        anim_data.as_deref().map(|a| &a.0),
    ) {
        *test_done = true;
    }
}

/// The debug bake: load the env model once, then spawn its submeshes (real WowModelMaterial, untextured
/// → the muted fallback) into every slot and frame each camera. A pipeline eyeball only — no skins, no
/// cache. Returns `true` once the model has loaded + a light buffer exists and it's baked (the caller
/// then stops re-baking).
#[allow(clippy::too_many_arguments)]
fn bake_test(
    commands: &mut Commands,
    booths: &Booths,
    path: &str,
    asset_server: &AssetServer,
    m2s: &Assets<M2Model>,
    wow_mats: &mut Assets<WowModelMaterial>,
    booth_light: &BoothLight,
    cache: &mut crate::model_render::MaterialCache,
    test_handle: &mut Option<Handle<M2Model>>,
    cams: &mut Query<(&BoothCam, &mut Transform, &mut Projection)>,
    catalog: Option<&benilla_formats::AnimDataCatalog>,
) -> bool {
    let handle = test_handle
        .get_or_insert_with(|| asset_server.load(m2_url(path)))
        .clone();
    // Bake only once the asset lands and the studio-light buffer exists (the material needs it).
    let Some(model) = m2s.get(&handle) else {
        return false;
    };
    // The portraits' studio light and the body pane's own (decision 0638) — the harness bakes each
    // slot against the light that slot really uses, so the eyeball shows what ships.
    let (Some(studio), Some(pane)) = (
        booth_light.studio.buffer.clone(),
        booth_light.pane.buffer.clone(),
    ) else {
        return false;
    };
    // Optional real skin for the test bake (WOW_PORTRAIT_TEST_SKIN=<blp path>) — an untextured model
    // reads dark brown by design (the muted fallback is a gamma-dark albedo), so brightness parity
    // with the world is only judgeable textured.
    let skin: Option<Handle<Image>> = std::env::var("WOW_PORTRAIT_TEST_SKIN")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|p| {
            asset_server.load(format!(
                "mpq://{}",
                p.replace('\\', "/").to_ascii_lowercase()
            ))
        });
    let parts_against = |light: &bevy::render::render_resource::Buffer,
                         cache: &mut crate::model_render::MaterialCache,
                         wow_mats: &mut Assets<WowModelMaterial>| {
        model
            .submeshes
            .iter()
            .map(|s| {
                let mat = crate::model_render::model_material(
                    cache,
                    wow_mats,
                    s.texture.clone().or_else(|| skin.clone()),
                    s.blend,
                    s.two_sided,
                    false,
                    false,
                    s.emissive,
                    s.additive,
                    false,
                    s.no_depth_write,
                    s.no_depth_test,
                    s.fog_policy,
                    crate::model_render::ShadeSel::Lit, // booth look: never ground-shaded
                    0,
                    None,                // static UVs
                    s.rgb_anim.as_ref(), // seeded at its first key — the booth freezes at t = 0
                    None,  // booth: no booth buffer has a point table (count 0) — anchor moot
                    None,  // M2 carries no MOMT SIDN colour
                    false, // …nor the WINDOW flag
                    light,
                );
                BoothPart {
                    skinned: Some(s.skinned_mesh.clone()),
                    static_mesh: s.mesh.clone(),
                    material: mat,
                }
            })
            .collect::<Vec<BoothPart>>()
    };
    let parts = parts_against(&studio, cache, wow_mats);
    let pane_parts = parts_against(&pane, cache, wow_mats);
    let (pivot_height, ground_radius) = model
        .bounds
        .map(|b| (b.pivot_z.map_or(0.0, |z| z + 0.0972), b.ring_footprint))
        .unwrap_or((0.0, 0.0));
    let anchors = PortraitAnchors {
        camera: model.portrait_camera,
        head: head_anchor(&model.skeleton, &model.attachments),
        pivot_height,
        ground_radius,
    };
    let rig = frame(&anchors);
    match anchors.camera {
        Some(c) => info!(
            "portrait test bake: {} submeshes, AUTHORED camera eye={:?} target={:?} fov={:.3} near={:.3} far={:.1}",
            model.submeshes.len(), c.eye, c.target, c.fov, c.near, c.far
        ),
        None => info!(
            "portrait test bake: {} submeshes, NO authored camera — heuristic head={:?} pivot={pivot_height:.2} foot={ground_radius:.2}",
            model.submeshes.len(), anchors.head
        ),
    }
    for token in SLOTS {
        let Some(booth) = booths.0.get(token) else {
            continue;
        };
        commands.entity(booth.root).despawn_related::<Children>();
        spawn_booth_model(
            commands,
            booth.root,
            booth.layer.clone(),
            &parts,
            &[], // the test bake dresses no riders
            Some((
                &model.skeleton,
                &model.inverse_bindposes,
                model.animations.as_ref(),
            )),
            catalog,
            BoothMotion::Frozen,
            [false, false], // the WOW_PORTRAIT_TEST bake dresses no weapons
            &[],            // …nor an eye-glow
        );
        aim(cams, token, &rig);
    }
    // Also drive the paper-doll booth from the same model, so `WOW_PORTRAIT_TEST` eyeballs the
    // full-body framing (feet/crown crop) server-less. Same all-submesh caveat as the portraits
    // (no geoset filter — a character bakes stacked hair, 0118); the live pane mirrors the filtered
    // player. Spun to the default yaw so the still reads three-quarter like the pane's default.
    if let Some(booth) = booths.0.get(PAPERDOLL_SLOT) {
        commands.entity(booth.root).despawn_related::<Children>();
        spawn_booth_model(
            commands,
            booth.root,
            booth.layer.clone(),
            &pane_parts, // the pane's own light, not the portraits' studio (decision 0638)
            &[],
            Some((
                &model.skeleton,
                &model.inverse_bindposes,
                model.animations.as_ref(),
            )),
            catalog,
            BoothMotion::Frozen,
            [false, false], // the paper-doll still sheaths its weapons — no in-hand grip
            &[],            // eye-glow in the paper doll is the same follow-up (see above)
        );
        aim(cams, PAPERDOLL_SLOT, &body_frame(&anchors));
        commands
            .entity(booth.root)
            .insert(Transform::from_rotation(Quat::from_rotation_y(
                PaperDollBooth::default().yaw,
            )));
    }
    true
}
