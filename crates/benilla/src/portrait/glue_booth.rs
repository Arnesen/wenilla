//! The GLUE booth (decisions 0423 + 0465) — one booth slot serving both pre-world screens,
//! tuple-driven with **no wire entity**: the per-race `UI_<Race>` background scene with a character
//! standing in it — the character being *built* (char create, naked + underwear) or the selected
//! roster character (char select, geared from its enum record) — live, rotatable, fullscreen.
//!
//! The heavy lifting (displayId → model → geoset filter → composited skin → equipment riders) is
//! the entities module's [`crate::entities::attach`] glue-preview builder, which writes the
//! assembled part + rider lists into [`GluePreviewBake`] — it owns the attach path's resources, so
//! the booth doesn't re-implement them. This module owns the *booth* half, exactly like
//! [`super::sync_paperdoll`] does for the character window's paper doll: mirror the parts onto the
//! scene's authored light rig (or the studio buffer scene-less), pose a fresh instance via
//! [`super::spawn_booth_model`] — **looping Stand**, not a frozen still — seat riders on its
//! joints, and spin the root to the screen's live [`GluePreview::yaw`]. The bake lands in
//! [`super::PortraitImages`] under [`GLUE_SLOT`], sampled full-bleed by whichever glue screen is
//! up.

use benilla_assets::M2Model;
use bevy::camera::visibility::RenderLayers;
use bevy::camera::PerspectiveProjection;
use bevy::prelude::*;
use bevy::render::render_resource::Extent3d;
use bevy::window::PrimaryWindow;

use crate::entities::Creatures;
use crate::model_render::m2_url;
use crate::terrain::WowModelMaterial;

use super::framing::{attachment_point, DIAG_TO_VERT};
use super::{
    aim, body_frame, new_target_image, spawn_booth_model, Booth, BoothBillboardSpec, BoothCam,
    BoothLight, BoothMotion, BoothPart, BoothRider, Booths, PortraitImages, PortraitSource,
    PAPERDOLL_LAYER,
};

/// The glue booth slot token (its key in [`super::PortraitImages`] / [`Booths`]).
pub(crate) const GLUE_SLOT: &str = "glue";
/// The glue booth's render layer — the next past the paper doll's.
const GLUE_LAYER: usize = PAPERDOLL_LAYER + 1;
/// The scene-less fallback target resolution — with a scene up the target resizes to the window
/// (the glue screens are fullscreen renders).
const GLUE_SIZE: u32 = 1024;

/// The character appearance the CREATE screen shows — race/sex/**class** + the five appearance
/// dials (decisions 0423 + 0527): the create body dressed in the (race, class, sex) level-1 starting
/// outfit (CharStartOutfit), the ref's create preview. Class is load-bearing here — a different
/// class wears different starting gear, and the ref re-applies equipment on class change
/// (`SelectClass` → `cc_apply_sections`, wow-re `wave-preview.md`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct CreateLook {
    pub(crate) race: u8,
    pub(crate) sex: u8,
    pub(crate) class: u8,
    pub(crate) skin: u8,
    pub(crate) face: u8,
    pub(crate) hair_style: u8,
    pub(crate) hair_color: u8,
    pub(crate) facial_hair: u8,
}

/// The character the SELECT screen shows — the roster entry's full appearance + its visible
/// equipment display pairs, verbatim off `SMSG_CHAR_ENUM` (decision 0465).
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct SelectLook {
    pub(crate) race: u8,
    pub(crate) sex: u8,
    pub(crate) skin: u8,
    pub(crate) face: u8,
    pub(crate) hair_style: u8,
    pub(crate) hair_color: u8,
    pub(crate) facial_hair: u8,
    /// `CHARACTER_FLAG_*` bits (hide-helm / hide-cloak honored by the builder).
    pub(crate) flags: u32,
    pub(crate) equipment: [benilla_protocol::CharEnumItem; 19],
}

impl From<&benilla_protocol::Character> for SelectLook {
    fn from(c: &benilla_protocol::Character) -> Self {
        Self {
            race: c.race,
            sex: c.gender.min(1),
            skin: c.skin,
            face: c.face,
            hair_style: c.hair_style,
            hair_color: c.hair_color,
            facial_hair: c.facial_hair,
            flags: c.flags,
            equipment: c.equipment,
        }
    }
}

/// What stands on the glue stage: the create screen's dial tuple, or a select roster entry.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum GlueLook {
    Create(CreateLook),
    Select(SelectLook),
}

impl GlueLook {
    /// The (race, sex) whose body model carries the look.
    pub(crate) fn body(&self) -> (u8, u8) {
        match self {
            GlueLook::Create(l) => (l.race, l.sex),
            GlueLook::Select(l) => (l.race, l.sex),
        }
    }
}

/// Which glue background scene shows (decision 0539 §5): the login screen's main-menu gate, or a
/// race's `UI_<Race>` stage. An enum, not a fake race value — the login scene isn't a race, and
/// the fog law forks on the kind (MainMenu is always fogged with its authored XML fog; Race keeps
/// the 0429 create/select split).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlueScene {
    /// `UI_MainMenu` — the login screen's burning gate (`AccountLogin.xml`'s ModelFFX).
    MainMenu,
    /// A race's `UI_<Race>` stage (create/select; Gnome→Dwarf, Troll→Orc per the ref's mapping).
    Race(u8),
}

/// The glue screens' live input to the booth (decisions 0423 + 0465 + 0539): which scene shows
/// (`None` = no scene, booth torn down — each screen drives it: login the main menu, create the
/// picked race, select the SELECTED character's race, Orc before a list arrives), the character
/// standing in it (`None` = empty stage — the login screen and an empty account), and the bake
/// **yaw** in radians (drag / rotate buttons write it; the create screen resets it to the ref's
/// −15° on entry, the select screen to 0). The look feeds the part builder; the yaw spins the
/// baked root (cheap — no re-bake).
#[derive(Resource)]
pub(crate) struct GluePreview {
    pub(crate) scene: Option<GlueScene>,
    pub(crate) look: Option<GlueLook>,
    pub(crate) yaw: f32,
}

impl Default for GluePreview {
    fn default() -> Self {
        Self {
            scene: None,
            look: None,
            yaw: 0.0,
        }
    }
}

/// The per-race glue **background scene** (the ref's fullscreen `ModelFFX` — `SetBackgroundModel`
/// loads `Interface\Glues\Models\UI_<Race>\UI_<Race>.mdx`, plays sequence 0, and shows camera 0):
/// the scene model spawns under its own root beside the character's (the character yaws; the scene
/// never does), the character stands on the scene's **attachment 0** (the stage spot, on camera 0's
/// axis in every UI_* scene), and while a scene is up its authored camera owns the booth camera.
/// Gnome shares Dwarf's scene and Troll shares Orc's (the ref's `SetBackgroundModel` mapping).
#[derive(Resource)]
pub(crate) struct CreateScene {
    root: Entity,
    token: Option<&'static str>,
    handle: Option<Handle<M2Model>>,
    spawned: bool,
    /// The authored camera 0, captured at spawn (drives the booth camera while the scene shows).
    cam: Option<benilla_assets::PortraitCamera>,
    /// The character's stage spot — scene attachment 0 (Bevy model space), `ZERO` with no scene.
    /// (Verified for select too: the body seats on attachment **0**, `0x473039` — attachment 1 is
    /// the rare enum secondary model, wow-re `glue-select-model.md` TU-B; 0429's attachment-1
    /// presumption is dead.)
    char_spot: Vec3,
    /// Whether the current spawn wrote the fog rows enabled (the create law) — a screen hop with
    /// the same scene race still rebuilds when this flips.
    fog: bool,
    /// The scene's own light buffer — the scene's **authored M2 light rig** folded per the world
    /// law (see [`SceneRig`]: directionals → ambient + SH probe slot 0, points → the per-vertex
    /// point table) plus the ref's per-race fog (`CharModelFogInfo`) — **create only**: at select
    /// the client overwrites the background's fog callback with the light callback, so the select
    /// scene renders unfogged (wow-re `glue-select-model.md` TU-C, byte-verified `0x472110`) —
    /// rewritten on each scene swap. The character re-lights onto it too — the ref's create character is scene-lit (the
    /// NightElf/Scourge characters are lit almost entirely by stage-local point lights).
    light: Option<bevy::render::render_resource::Buffer>,
    /// Booth twins of the character's materials against the scene buffer (the same dedup law as
    /// `BoothLight::variants`; one rewritten buffer per race keeps entries valid across swaps).
    variants: std::collections::HashMap<AssetId<WowModelMaterial>, Handle<WowModelMaterial>>,
    /// Bumped on every scene spawn — folded into `sync_glue_booth`'s change key so the character
    /// re-lights the moment the scene (and its rig) lands.
    rev: u64,
}

/// The scene's model-name token: `Interface\Glues\Models\UI_<token>\UI_<token>.mdx`. The race
/// mapping is the ref's (`GlueParent.lua`'s `SetBackgroundModel` HACK block); the main menu is
/// `AccountLogin.xml`'s own model.
fn scene_token(scene: GlueScene) -> &'static str {
    match scene {
        GlueScene::MainMenu => "MainMenu",
        GlueScene::Race(race) => match race {
            2 | 8 => "Orc",   // Troll shares Orc's scene
            3 | 7 => "Dwarf", // Gnome shares Dwarf's
            4 => "NightElf",
            5 => "Scourge",
            6 => "Tauren",
            _ => "Human",
        },
    }
}

/// A glue scene's authored M2 light rig, folded for the booth's light buffer under the
/// byte-verified world law (wow-re `m2-dynamic-lights.md`): **directional** lights accumulate —
/// ambient tracks summed into one ambient term, each diffuse×intensity an SH lobe with its *own*
/// direction (the `Model2.bls` fold, [`crate::lighting::prop_probe_coeffs`]) — and **point**
/// lights become per-vertex lights on the buffer's point table, evaluated with the engine's fixed
/// falloff `1/(0.7·d + 0.03·d²)` (the authored attenuation radii are dead code in the reference;
/// `wow_model.wgsl`'s point term already implements the law). Both booth models read this one
/// rig — the scene at its origin, the character at the stage — exactly the reference's shared
/// scene-light gather. Light positions are authored in model space (the attachment convention),
/// bind-pose static in every UI_* scene.
struct SceneRig {
    /// The summed directional-light ambient (every UI_* point light authors ambient ×0).
    ambient: [f32; 3],
    /// Probe slot 0 of the booth buffer: ambient + the directional lobes, SH-folded.
    probe: [Vec4; 7],
    /// Point-table entries, two rows each: `[pos.xyz, range]`, `[rgb, 0]`.
    points: Vec<[[f32; 4]; 2]>,
}

/// Candidacy range packed with a scene point light — the reference gather has no range gate
/// (pure ≤3-nearest), so effectively unbounded; the falloff does the bounding.
const SCENE_POINT_RANGE: f32 = 1.0e6;

/// Fold the scene model's light table into a [`SceneRig`] (see there for the law). A directional
/// light's **to-light direction is its bone's local +Z axis** (`M2Light::bone_z` — the gather
/// reads row 2 of the light bone's pose matrix, never the def position; byte-verified in wow-re
/// `glue/scratch/glue-model-lighting.md §3`, on-surface to-light = `normalize(Bz·BM_rot)`); the
/// model rotation `BM_rot` is identity here (the scene root never rotates).
fn scene_rig(lights: &[benilla_assets::ModelLight]) -> SceneRig {
    let mut ambient = [0.0f32; 3];
    let mut lobes: Vec<(Vec3, [f32; 3])> = Vec::new();
    let mut points = Vec::new();
    for l in lights.iter().map(|l| &l.def) {
        for (a, c) in ambient.iter_mut().zip(l.ambient_color) {
            *a += c * l.ambient_intensity;
        }
        let color = l.diffuse_color.map(|c| c * l.diffuse_intensity);
        if l.is_point() {
            let pos = benilla_assets::coords::wow_to_bevy(l.position);
            // The same RAW commit the world table applies (the `0x71ca80` encode is undone by the
            // `0x593040` decode — wow-re trace-forensics-overgamut-point-commit-d3d) — a glue
            // scene is just another CM2Scene, and the GL commit is shared. Carried here so one law
            // covers both tables, not two by coincidence.
            let c = crate::lighting::commit_raw(color);
            points.push([
                [pos.x, pos.y, pos.z, SCENE_POINT_RANGE],
                [c[0], c[1], c[2], 0.0],
            ]);
        } else {
            // Toward-light unit vector, as `prop_probe_coeffs` expects (it re-normalizes).
            lobes.push((benilla_assets::coords::wow_to_bevy(l.bone_z), color));
        }
    }
    SceneRig {
        ambient,
        probe: crate::lighting::prop_probe_coeffs(ambient, &lobes),
        points,
    }
}

/// The ref's per-scene fog: the race rows are `CharModelFogInfo` (GlueParent.lua) — color + far,
/// near always 0; the main menu's is `AccountLogin.xml`'s own `<FogColor>` + `fogFar` attribute.
fn scene_fog(token: &str) -> ([f32; 3], f32) {
    match token {
        "MainMenu" => ([0.25, 0.06, 0.015], 1200.0),
        "Orc" => ([0.5, 0.5, 0.5], 270.0),
        "Dwarf" => ([0.85, 0.88, 1.0], 500.0),
        "NightElf" => ([0.25, 0.22, 0.55], 611.0),
        "Scourge" => ([0.0, 0.22, 0.22], 26.0),
        "Tauren" => ([1.0, 0.61, 0.42], 153.0),
        _ => ([0.8, 0.65, 0.73], 222.0), // Human
    }
}

/// One assembled preview mesh — the same shape as a mirrored [`super::PortraitPart`], but produced
/// tuple-driven (no entity) by the entities-side builder: both mesh twins + the steady, world-lit
/// material (the booth re-lights it studio via [`BoothLight`]).
#[derive(Clone)]
pub(crate) struct PreviewPart {
    pub(crate) static_mesh: Handle<Mesh>,
    pub(crate) skinned_mesh: Option<Handle<Mesh>>,
    pub(crate) material: Handle<WowModelMaterial>,
}

/// One equipment rider on the assembled preview (helm / shoulder / sheathed weapon — decision
/// 0465): the item model part's mesh + built material, seated under the body skeleton's `bone`
/// joint at the attach point's `offset` — the booth twin of the world path's
/// [`crate::portrait::PortraitRider`].
#[derive(Clone)]
pub(crate) struct PreviewRider {
    pub(crate) mesh: Handle<Mesh>,
    pub(crate) material: Handle<WowModelMaterial>,
    pub(crate) bone: u16,
    pub(crate) offset: Vec3,
}

/// One character billboard batch on the assembled preview — the undead/night-elf **eye-glow** (the
/// world path's camera-facing card, [`crate::billboard`]). The booth is a separate camera, so the
/// glow can't ride the world card system; it's rebuilt here for the booth's own camera
/// ([`super::booth::face_booth_billboards`]). Its centred quad, fullbright material, and the
/// billboard bone/flag it rides.
#[derive(Clone)]
pub(crate) struct PreviewBillboard {
    pub(crate) mesh: Handle<Mesh>,
    pub(crate) material: Handle<WowModelMaterial>,
    pub(crate) bone: u16,
    pub(crate) kind: benilla_formats::BillboardKind,
}

/// The entities-side builder's output (decisions 0423 + 0465): the geoset-filtered, composited
/// parts for the current look (+ its equipment riders for a Select look), plus the body displayId
/// (for the booth's framing/rig) and a revision that bumps on every real change — a new successful
/// bake, or a clear to `look: None`. The booth re-bakes only when `revision` changes; a bare yaw
/// change never touches it.
#[derive(Resource, Default)]
pub(crate) struct GluePreviewBake {
    pub(crate) look: Option<GlueLook>,
    pub(crate) display_id: u32,
    pub(crate) parts: Vec<PreviewPart>,
    pub(crate) riders: Vec<PreviewRider>,
    /// The look's character billboard batches (the eye-glow) — geoset-gated like `parts`, but seated
    /// on their billboard bone's joint and camera-faced by the booth, not drawn as plain body meshes.
    pub(crate) billboards: Vec<PreviewBillboard>,
    /// Per-hand weapon grip `[right, left]` — a hand whose attach point holds a weapon closes into the
    /// `HandsClosed` finger pose (wow-re `hand-grip-mechanism.md`). The builder knows the held items'
    /// attach ids (which the flat rider list drops), so it resolves the grip here for the booth spawn.
    pub(crate) grip: [bool; 2],
    pub(crate) revision: u64,
}

/// Stand up the create booth beside the portrait/paper-doll ones (called from [`super::setup_booths`],
/// which owns the shared image/booth resources). Same off-screen pipeline as the paper doll — HDR +
/// FfxGlow, negative order — but a [`GLUE_SIZE`] target cleared *transparent*, its own layer,
/// framed per-bake.
pub(super) fn spawn_glue_booth(
    commands: &mut Commands,
    images: &mut Assets<Image>,
    portraits: &mut PortraitImages,
    booths: &mut Booths,
) {
    let image = images.add(new_target_image(GLUE_SIZE));
    portraits
        .0
        .insert(GLUE_SLOT.to_string(), PortraitSource::Live(image.clone()));
    let layer = RenderLayers::layer(GLUE_LAYER);
    let root = commands
        .spawn((Transform::IDENTITY, Visibility::Visible, layer.clone()))
        .id();
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: -100 + GLUE_LAYER as isize,
            // Transparent (unlike the portrait/paper-doll booths' backdrop): the create screen
            // composites the character straight over the page, the ref's model-in-the-scene look.
            // FfxGlow's combine carries the scene alpha through for exactly this.
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        bevy::camera::RenderTarget::Image(image.clone().into()),
        bevy::render::view::Hdr,
        bevy::core_pipeline::tonemapping::Tonemapping::None,
        crate::ffx_glow::FfxGlow::WORLD,
        Msaa::Off,
        // Placeholder — `sync_glue_booth` overwrites transform + projection from the model's bounds
        // on the first bake (the same `body_frame` law as the paper doll).
        Projection::from(bevy::camera::PerspectiveProjection {
            fov: super::PORTRAIT_FOV,
            near: 0.02,
            far: 100.0,
            ..default()
        }),
        layer.clone(),
        BoothCam(GLUE_SLOT.to_string()),
    ));
    booths.0.insert(
        GLUE_SLOT.to_string(),
        Booth {
            layer: layer.clone(),
            root,
            target: image,
            baked: None,
            // The glue camera is scene-driven, not wake-driven: `gate_booth_cameras` keeps it
            // active exactly while a glue scene shows (the scene is LIVE — looping animation,
            // global sequences, particle emitters — so it renders continuously, unlike the stills).
            wake: 0,
            pending: Vec::new(),
        },
    );
    // The background scene's own root (the character root above yaws; the scene never does).
    let scene_root = commands
        .spawn((Transform::IDENTITY, Visibility::Visible, layer))
        .id();
    commands.insert_resource(CreateScene {
        root: scene_root,
        token: None,
        handle: None,
        spawned: false,
        cam: None,
        char_spot: Vec3::ZERO,
        fog: true,
        light: None,
        variants: default(),
        rev: 0,
    });
}

/// Drive the background scene (see [`CreateScene`]): load the selected race's `UI_*` model, spawn
/// it looping sequence 0, resize the booth target to the window (the scene is a fullscreen render),
/// seat the character on the stage spot, and hold the booth camera on the scene's authored camera 0
/// — every frame, so the per-bake body framing from [`sync_glue_booth`] never wins while a scene
/// shows. Leaving the screen (`look: None`) tears the scene down and restores the square target.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(super) fn sync_glue_scene(
    mut commands: Commands,
    preview: Res<GluePreview>,
    mut scene: ResMut<CreateScene>,
    booths: Res<Booths>,
    asset_server: Res<AssetServer>,
    m2s: Res<Assets<M2Model>>,
    booth_light: Res<BoothLight>,
    mut materials: ResMut<Assets<WowModelMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut cache: Local<crate::model_render::MaterialCache>,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
    mut cams: Query<(&BoothCam, &mut Transform, &mut Projection)>,
    window: Query<&Window, With<PrimaryWindow>>,
    device: Res<bevy::render::renderer::RenderDevice>,
    queue: Res<bevy::render::renderer::RenderQueue>,
    // The scene's particle-emitter spawn wiring (decision 0539 §5) + the skin-palette table and
    // its mirror registry (decision 0720) — one tuple param (the 16-SystemParam ceiling).
    particle_assets: (
        ResMut<Assets<Mesh>>,
        ResMut<Assets<crate::particles::WowParticleMaterial>>,
        ResMut<crate::rig_palette::RigPalettes>,
        ResMut<crate::rig_palette::RigPaletteMirrors>,
    ),
) {
    let (mut meshes, mut particle_materials, mut palettes, mut mirrors) = particle_assets;
    let Some(booth) = booths.0.get(GLUE_SLOT) else {
        return;
    };
    let Some(which) = preview.scene else {
        // Off the glue screens: tear the scene down, free the model, square target back.
        if scene.token.is_some() {
            commands.entity(scene.root).despawn_related::<Children>();
            commands.entity(scene.root).remove::<(
                AnimationPlayer,
                AnimationGraphHandle,
                crate::creature_anim::GlobalSeqDrive,
            )>();
            scene.token = None;
            scene.handle = None;
            scene.spawned = false;
            scene.cam = None;
            scene.char_spot = Vec3::ZERO;
            resize_target(&mut images, &booth.target, GLUE_SIZE, GLUE_SIZE);
        }
        return;
    };

    // The booth target follows the window while the screen is up — the scene is a fullscreen
    // render, so the bake wants window-native resolution (the fallback character-only render
    // shares it; its projection is aspect-aware).
    if let Ok(w) = window.single() {
        resize_target(
            &mut images,
            &booth.target,
            w.physical_width().max(1),
            w.physical_height().max(1),
        );
    }

    let token = scene_token(which);
    // The fog law forks on the scene kind (decision 0539 §5): the main menu is ALWAYS fogged with
    // its authored XML fog; for a race stage it is per-SCREEN — create keeps `CharModelFogInfo`,
    // select renders the same scene unfogged (`0x472110` overwrites the background's fog callback —
    // wow-re `glue-select-model.md` TU-C). The screen is told by the look's flavor (every create
    // writer sets a Create look with the scene; select feeds Select or, on an empty account, None).
    let fog = match which {
        GlueScene::MainMenu => true,
        GlueScene::Race(_) => matches!(preview.look, Some(GlueLook::Create(_))),
    };
    if scene.token != Some(token) {
        scene.token = Some(token);
        scene.handle = Some(asset_server.load(m2_url(&format!(
            "Interface\\Glues\\Models\\UI_{token}\\UI_{token}.mdx"
        ))));
        scene.spawned = false;
        scene.cam = None;
        scene.char_spot = Vec3::ZERO;
        commands.entity(scene.root).despawn_related::<Children>();
    } else if scene.spawned && scene.fog != fog {
        // Same scene, other screen: rebuild in place (the handle is cached — same-frame respawn).
        scene.spawned = false;
        commands.entity(scene.root).despawn_related::<Children>();
    }
    if !scene.spawned {
        let Some(model) = scene.handle.as_ref().and_then(|h| m2s.get(h)) else {
            return; // still loading (the flat page shows in the meantime)
        };
        if booth_light.studio.buffer.is_none() {
            return; // headless — no booth pipeline, no scene
        }
        // The scene's light: its **authored M2 rig** (ambient + the strongest key toward the
        // stage) plus the ref's per-race fog (`CharModelFogInfo`) — its own buffer, so the
        // portraits' shared studio light stays untouched. Rewritten in place on every scene swap
        // (the cached scene + character materials keep binding it).
        let light = scene
            .light
            .get_or_insert_with(|| {
                device.create_buffer(&bevy::render::render_resource::BufferDescriptor {
                    label: Some("wow_create_scene_light"),
                    size: crate::lighting::light_blob_bytes(),
                    usage: bevy::render::render_resource::BufferUsages::STORAGE
                        | bevy::render::render_resource::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .clone();
        // The scene's rigs skin from THIS buffer's palette region (decision 0720): register it
        // as a mirror (re-registration replaces — a rebuilt scene's old buffer drops).
        mirrors.0.insert("glue_scene", light.clone());
        let stage = attachment_point(&model.skeleton, &model.attachments, 0).unwrap_or(Vec3::ZERO);
        let rig = scene_rig(&model.lights);
        let (fog_rgb, fog_far) = scene_fog(token);
        let mut rows = [[0.0f32; 4]; crate::lighting::LIGHT_HEADER_ROWS];
        // Rig materials read the probe + point table; the core rows still carry the plain ambient
        // (with a zero sun) so any non-rig lane that ever lands in the booth degrades sanely.
        crate::lighting::pack_model_core_rows(&mut rows, rig.ambient, [0.0; 3], Vec3::NEG_Y);
        rows[3] = [0.0, 0.0, 0.0, 20.0]; // spec (unused by models; terrain-shininess convention)
        let fog_w = if fog { 1.0 } else { 0.0 };
        rows[4] = [fog_rgb[0], fog_rgb[1], fog_rgb[2], fog_w]; // fog color, w = enabled
        rows[5] = [0.0, fog_far, 0.0, 10_000.0]; // start 0, the ref's far; farclip wall inert
        rows[12][3] = 1.0; // point gain: the rig's authored point lights at the committed base
        rows[19] = [0.0, 0.0, 0.0, 1.0]; // rig lane ignores the intensity dial; 1.0 = byte levels
        rows[20] = [rig.points.len() as f32, 0.0, 0.0, 0.0]; // point-table count
                                                             // Header rows + the point table are contiguous in the std430 layout — one write covers
                                                             // both (stale entries past `count` never read).
        let mut blob: Vec<[f32; 4]> = rows.to_vec();
        for p in &rig.points {
            blob.extend_from_slice(p);
        }
        queue.write_buffer(&light, 0, bytemuck::cast_slice(&blob));
        // The directional fold lands in probe slot 0 at the buffer's probe region (booth
        // instances carry MeshTag 0 → slot 0 — the rig lane's read side).
        let probe: [[f32; 4]; 7] = rig.probe.map(|v| v.to_array());
        queue.write_buffer(
            &light,
            crate::lighting::prop_probe_region_offset(),
            bytemuck::cast_slice(&probe),
        );
        scene.fog = fog;
        info!(
            "create scene: UI_{token} rig — ambient {:?}, {} point light(s), probe dc ({:.2},{:.2},{:.2})",
            rig.ambient,
            rig.points.len(),
            rig.probe[0].w,
            rig.probe[1].w,
            rig.probe[2].w,
        );
        let scene_parts: Vec<BoothPart> = model
            .submeshes
            .iter()
            .map(|s| {
                let material = crate::model_render::model_material(
                    &mut cache,
                    &mut materials,
                    s.texture.clone(),
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
                    crate::model_render::ShadeSel::Rig, // lit by the scene's own authored rig
                    0,
                    s.uv_anim.as_ref(), // scene clouds/water scroll
                    s.rgb_anim.as_ref(),
                    None,
                    None,
                    false,
                    &light,
                );
                BoothPart {
                    skinned: Some(s.skinned_mesh.clone()),
                    static_mesh: s.mesh.clone(),
                    material,
                }
            })
            .collect();
        let joints = spawn_booth_model(
            &mut commands,
            &mut palettes,
            scene.root,
            booth.layer.clone(),
            &scene_parts,
            &[],
            Some((
                &model.skeleton,
                &model.inverse_bindposes,
                model.animations.as_ref(),
            )),
            anim_data.as_deref().map(|a| &a.0),
            BoothMotion::Loop, // sequence 0 loops — flags wave, clouds drift
            [false, false],    // the backdrop scene has no hands to grip
            &[],               // …nor a character's eye-glow
        );
        // The scene's authored particle emitters (decision 0539 §5) — the braziers/embers every
        // UI_* scene carries (MainMenu 28, Orc 11, NightElf 12…), dead until now. The world spawn
        // path, on the booth's layer, lit/fogged by the SCENE's own light buffer (the ModelFFX fog
        // covers the whole model, emitters included); owner = the emitter bone's joint, so fire
        // flicker rides the animated bones; anchored at the static scene root. No `EmitterFade`:
        // a glue scene always ticks. Parented under the scene root so teardown cascades.
        let scene_light_ref = crate::lighting::SharedLightBuffer(light.clone());
        let mut emitters = 0usize;
        for em in &model.emitters {
            let owner = joints
                .get(em.def.bone as usize)
                .map_or((scene.root, [0.0; 3]), |&j| (j, em.bone_pivot));
            if let Some(e) = crate::particles::spawn_emitter(
                &mut commands,
                &mut meshes,
                &mut particle_materials,
                &scene_light_ref,
                em,
                Transform::IDENTITY,
                Some(owner),
                None, // a backdrop scene is never an attached model
                Some(scene.root),
                // A glue scene loops its one authored clip forever — the doodad law.
                crate::particles::EmitClock::Pinned,
            ) {
                commands
                    .entity(e)
                    .insert((booth.layer.clone(), ChildOf(scene.root)));
                emitters += 1;
            }
        }
        if emitters > 0 {
            info!("create scene: UI_{token} — {emitters} particle emitter(s) up");
        }
        scene.spawned = true;
        scene.cam = model.camera0;
        scene.char_spot = stage;
        scene.rev += 1; // the character re-lights onto the scene rig (sync_glue_booth's key)
        info!(
            "create scene: UI_{token} up — {} submeshes, camera {} stage {:?}",
            model.submeshes.len(),
            scene.cam.is_some(),
            scene.char_spot,
        );
        // Seat the character on the stage at the current facing (the yaw-only path below only
        // runs when the yaw *changes*).
        apply_yaw(&mut commands, booth.root, scene.char_spot, preview.yaw);
    }
    // The authored camera owns the booth camera while the scene shows (re-asserted every frame —
    // cheap, and it also tracks live window-aspect changes through the diagonal-FOV conversion).
    if let Some(cam) = scene.cam {
        let aspect = window
            .single()
            .map(|w| w.width() / w.height().max(1.0))
            .unwrap_or(4.0 / 3.0);
        let fwd = (cam.target - cam.eye).normalize_or_zero();
        let up = Quat::from_axis_angle(fwd, cam.roll) * Vec3::Y;
        // The record fov is the client's *diagonal* convention → vertical = fov/√(aspect²+1)
        // (the portrait path's 0.6 factor is exactly this at 4/3). Far kept generous: fog is not
        // rendered yet, so the authored far (27.8 on Orc) would slice unfogged geometry.
        let rig = (
            Transform::from_translation(cam.eye).looking_at(cam.target, up),
            Projection::from(PerspectiveProjection {
                fov: cam.fov / (aspect * aspect + 1.0).sqrt(),
                near: cam.near,
                far: cam.far.max(1000.0),
                ..default()
            }),
        );
        aim(&mut cams, GLUE_SLOT, &rig);
    }
}

/// Resize the booth's render target if it isn't already `w`×`h` (content is discarded — the next
/// frame repaints it whole).
fn resize_target(images: &mut Assets<Image>, target: &Handle<Image>, w: u32, h: u32) {
    if let Some(image) = images.get_mut(target) {
        if image.width() != w || image.height() != h {
            image.resize(Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            });
        }
    }
}

/// Re-bake the glue booth when the assembled parts change (a dial moved, a different character
/// selected, an item model landed) or the scene's light rig lands, and spin its root to the
/// screen's live yaw (drag / rotate — cheap, no re-bake). Mirrors [`super::sync_paperdoll`]: the
/// parts + riders come pre-assembled from the entities builder, so here we only re-light them —
/// onto the **scene's authored rig** while a scene is up (the ref's glue character is scene-lit;
/// the select screen's hardcoded weather-sun is the in-flight §5's to settle, decision 0465 §5),
/// the studio buffer otherwise — pose a fresh Stand-**looping** instance with the riders on its
/// joints, and frame it full-body.
#[allow(clippy::too_many_arguments)]
pub(super) fn sync_glue_booth(
    mut commands: Commands,
    preview: Res<GluePreview>,
    bake: Res<GluePreviewBake>,
    booths: Res<Booths>,
    mut scene: Option<ResMut<CreateScene>>,
    mut booth_light: ResMut<BoothLight>,
    mut materials: ResMut<Assets<WowModelMaterial>>,
    creatures: Option<Res<Creatures>>,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
    mut cams: Query<(&BoothCam, &mut Transform, &mut Projection)>,
    mut palettes: ResMut<crate::rig_palette::RigPalettes>,
    mut last: Local<Option<(u64, f32, u64)>>,
) {
    let Some(booth) = booths.0.get(GLUE_SLOT) else {
        return;
    };
    // Where the character stands (the scene's stage spot while a scene is up, else the origin) —
    // and the scene revision, so the bake re-lights the moment a scene rig lands.
    let (spot, scene_rev) = scene.as_deref().map_or((Vec3::ZERO, 0), |s| {
        if s.spawned {
            (s.char_spot, s.rev)
        } else {
            (Vec3::ZERO, s.rev)
        }
    });
    let (last_rev, last_yaw, last_scene) = last.unwrap_or((u64::MAX, f32::NAN, u64::MAX));
    let rebake = last_rev != bake.revision || last_scene != scene_rev;
    let reyaw = last_yaw != preview.yaw;
    if !rebake && !reyaw {
        return;
    }

    if rebake {
        // Empty parts (nothing selected, or the model failed) → clear the booth.
        if bake.parts.is_empty() {
            commands.entity(booth.root).despawn_related::<Children>();
            *last = Some((bake.revision, preview.yaw, scene_rev));
            apply_yaw(&mut commands, booth.root, spot, preview.yaw);
            return;
        }
        // The rig + framing come from the display cache — the same readiness gate the builder used
        // to produce the parts, so both are ready together. If somehow not, leave `last` so we retry.
        let Some(creatures) = creatures.as_deref() else {
            return;
        };
        let (Some(rig), Some(anchors)) = (
            creatures.display_rig(bake.display_id),
            creatures.display_anchors(bake.display_id),
        ) else {
            return;
        };
        let scene_buf = scene
            .as_deref()
            .and_then(|s| if s.spawned { s.light.clone() } else { None });
        let relight = |material: &Handle<WowModelMaterial>,
                       scene: &mut Option<ResMut<CreateScene>>,
                       booth_light: &mut BoothLight,
                       materials: &mut Assets<WowModelMaterial>| {
            match (&scene_buf, scene.as_deref_mut()) {
                (Some(buf), Some(s)) => {
                    super::material_variant(&mut s.variants, buf, material, materials, true)
                }
                _ => booth_light.studio.variant(material, materials),
            }
        };
        let mut booth_parts = Vec::with_capacity(bake.parts.len());
        for p in &bake.parts {
            booth_parts.push(BoothPart {
                skinned: p.skinned_mesh.clone(),
                static_mesh: p.static_mesh.clone(),
                material: relight(&p.material, &mut scene, &mut booth_light, &mut materials),
            });
        }
        // The equipment riders (a Select look — helm/shoulders/sheathed weapons, decision 0465):
        // scene-lit like the body, seated on the throwaway skeleton's joints by the booth spawn.
        let booth_riders: Vec<BoothRider> = bake
            .riders
            .iter()
            .map(|r| BoothRider {
                mesh: r.mesh.clone(),
                material: relight(&r.material, &mut scene, &mut booth_light, &mut materials),
                bone: r.bone,
                offset: r.offset,
            })
            .collect();
        // The eye-glow billboards, relit onto the booth's buffer like everything else (harmless —
        // the glow batch is fullbright, so it ignores the buffer; the variant just keeps parity).
        let booth_billboards: Vec<BoothBillboardSpec> = bake
            .billboards
            .iter()
            .map(|b| BoothBillboardSpec {
                mesh: b.mesh.clone(),
                material: relight(&b.material, &mut scene, &mut booth_light, &mut materials),
                bone: b.bone,
                kind: b.kind,
            })
            .collect();
        commands.entity(booth.root).despawn_related::<Children>();
        spawn_booth_model(
            &mut commands,
            &mut palettes,
            booth.root,
            booth.layer.clone(),
            &booth_parts,
            &booth_riders,
            rig.inverse_bindposes
                .as_ref()
                .map(|ibp| (rig.skeleton, ibp, rig.animations)),
            anim_data.as_deref().map(|a| &a.0),
            BoothMotion::Loop, // the glue preview is a live scene, not a still
            bake.grip,         // close each hand that draws a weapon (the ref's paperdoll rule)
            &booth_billboards,
        );
        // The character-only fallback framing (no scene up — art missing / still loading): the
        // body-frame transform with an aspect-aware projection, because the booth target is
        // window-sized now (the square-true `WowPortraitProjection` would stretch on it). While a
        // scene shows, `sync_glue_scene` re-aims onto the authored camera the same frame.
        let (t, _) = body_frame(&anchors);
        aim(
            &mut cams,
            GLUE_SLOT,
            &(
                t,
                Projection::from(PerspectiveProjection {
                    fov: DIAG_TO_VERT * super::framing::BODY_FOV,
                    near: 0.02,
                    far: 100.0,
                    ..default()
                }),
            ),
        );
    }
    apply_yaw(&mut commands, booth.root, spot, preview.yaw);
    *last = Some((bake.revision, preview.yaw, scene_rev));
}

/// Seat the model root on the stage `spot` facing `yaw` (the ref's `Model:SetRotation` — rotation
/// about the root's own origin, never about the scene's).
fn apply_yaw(commands: &mut Commands, root: Entity, spot: Vec3, yaw: f32) {
    commands
        .entity(root)
        .insert(Transform::from_translation(spot).with_rotation(Quat::from_rotation_y(yaw)));
}

/// The phase-3 preview instrument (`WOW_CREATE_TEST="race,sex[,class,skin,face,hairStyle,hairColor,facialHair]"`,
/// decisions 0423 + 0527): set the create-preview look once and show the booth's bake full-screen (an
/// `ImageNode` over everything), so the create body is verifiable server-less before the screen exists
/// (phase 4). Inert without the env. `class` (default 1 = Warrior) selects the CharStartOutfit the
/// preview dresses in; every appearance dial defaults to 0.
pub(super) fn drive_create_test(
    mut commands: Commands,
    mut preview: ResMut<GluePreview>,
    portraits: Res<PortraitImages>,
    mut done: Local<bool>,
) {
    if *done {
        return;
    }
    *done = true;
    let Ok(spec) = std::env::var("WOW_CREATE_TEST") else {
        return;
    };
    let mut it = spec.split(',').map(|s| s.trim().parse::<u8>().unwrap_or(0));
    let mut next = |d: u8| it.next().unwrap_or(d);
    let look = CreateLook {
        race: next(1),
        sex: next(0),
        class: next(1),
        skin: next(0),
        face: next(0),
        hair_style: next(0),
        hair_color: next(0),
        facial_hair: next(0),
    };
    info!(
        "create-test: race {} sex {} class {} — skin {} face {} hair {} hairColor {} facial {}",
        look.race,
        look.sex,
        look.class,
        look.skin,
        look.face,
        look.hair_style,
        look.hair_color,
        look.facial_hair
    );
    preview.scene = Some(GlueScene::Race(look.race));
    preview.look = Some(GlueLook::Create(look));
    // Show the booth's RTT full-screen (behind nothing) so a live shot / capture eyeballs the body.
    if let Some(PortraitSource::Live(image)) = portraits.0.get(GLUE_SLOT) {
        commands.spawn((
            ImageNode::new(image.clone()),
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            GlobalZIndex(2000),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_assets::ModelLight;
    use benilla_formats::M2Light;

    fn light(
        light_type: u16,
        pos: [f32; 3],
        bone_z: [f32; 3],
        dif: [f32; 3],
        di: f32,
        amb: [f32; 3],
        ai: f32,
    ) -> ModelLight {
        ModelLight {
            def: M2Light {
                light_type,
                bone: -1,
                position: pos,
                bone_z,
                ambient_color: amb,
                ambient_intensity: ai,
                diffuse_color: dif,
                diffuse_intensity: di,
                attenuation_start: 2.0,
                attenuation_end: 5.0,
                visibility_off: false,
            },
            bone_pivot: [0.0; 3],
        }
    }

    /// GOLDEN — the rig fold's classification law (wow-re `m2-dynamic-lights.md` +
    /// `glue-model-lighting.md`): directionals accumulate (ambient tracks summed; diffuse×intensity
    /// as per-light SH lobes whose to-light is the **bone's +Z axis**, never the def position),
    /// points land on the point table verbatim (colour × intensity, position wow→bevy) — never
    /// folded into the sun, and their dead authored attenuation radii never scale anything.
    #[test]
    fn rig_fold_classifies_directional_vs_point() {
        let lights = [
            // A warm directional key + a dedicated ambient-only directional (the UI_* pattern).
            // Positions are decoys — a directional's direction comes from bone_z alone.
            light(
                0,
                [99.0, 99.0, 99.0],
                [1.0, 0.0, 0.0],
                [1.0, 0.5, 0.2],
                2.0,
                [1.0; 3],
                0.0,
            ),
            light(
                0,
                [99.0, 99.0, 99.0],
                [0.0, 1.0, 0.0],
                [1.0; 3],
                0.0,
                [0.4, 0.5, 0.6],
                0.5,
            ),
            // A stage-local point light (the NightElf/Scourge character light).
            light(
                1,
                [1.0, 2.0, 3.0],
                [0.0, 0.0, 1.0],
                [0.7, 0.8, 1.0],
                2.5,
                [1.0; 3],
                0.0,
            ),
        ];
        let rig = scene_rig(&lights);
        // Ambient = Σ ambient_color × ambient_intensity, across every light.
        assert_eq!(rig.ambient, [0.2, 0.25, 0.3]);
        // Exactly the one point light on the table, position converted, colour × intensity —
        // committed RAW, over-gamut preserved (the reference's `0x71ca80` encode is undone by the
        // `0x593040` decode; a night terrain draw in the ring capture commits (1.2, 1.035, 0.805)
        // verbatim). This fixture's `(0.7, 0.8, 1.0) × 2.5` is deliberately over-driven so it pins
        // the mechanism: the raw product `(1.75, 2.0, 2.5)` reaches the table untouched — the
        // saturation the eye sees happens at the receiving vertex's lighting clamp, never here.
        assert_eq!(rig.points.len(), 1);
        let p = benilla_assets::coords::wow_to_bevy([1.0, 2.0, 3.0]);
        assert_eq!(rig.points[0][0], [p.x, p.y, p.z, SCENE_POINT_RANGE]);
        let c = rig.points[0][1];
        assert!(
            (c[0] - 1.75).abs() < 1e-6 && (c[1] - 2.0).abs() < 1e-6 && (c[2] - 2.5).abs() < 1e-6,
            "committed raw, over-gamut preserved: {c:?}"
        );
        assert_eq!(c[3], 0.0);
        // The probe folds each directional's bone_z (wow→bevy) as its to-light lobe; the
        // zero-intensity ambient-only light contributes a zero lobe (colour 0). Never the point.
        let expected = crate::lighting::prop_probe_coeffs(
            rig.ambient,
            &[
                (
                    benilla_assets::coords::wow_to_bevy([1.0, 0.0, 0.0]),
                    [2.0, 1.0, 0.4],
                ),
                (
                    benilla_assets::coords::wow_to_bevy([0.0, 1.0, 0.0]),
                    [0.0, 0.0, 0.0],
                ),
            ],
        );
        assert_eq!(rig.probe, expected);
    }
}
