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
use benilla_assets::m2_url;
use benilla_assets::materials::WowModelMaterial;

use super::framing::{attachment_point, diag_to_vert, PORTRAIT_ASPECT};
use super::{
    aim, body_frame, new_target_image, spawn_booth_effects, spawn_booth_model, Booth,
    BoothBillboardSpec, BoothCam, BoothEffects, BoothLight, BoothMotion, BoothPart, BoothRider,
    Booths, PortraitImages, PortraitSource, GLUE_LAYER,
};

/// The glue booth slot token (its key in [`super::PortraitImages`] / [`Booths`]).
pub(crate) const GLUE_SLOT: &str = "glue";
// `GLUE_LAYER` is defined with the rest of the booth ladder in [`super`] — defining it here, from
// the same `PAPERDOLL_LAYER + 1` the inspect booth used, is exactly how the two collided.
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
    /// The roster row's **pet** triple — the hunter/warlock companion that stands beside the
    /// selected character. It rides the look, not a `GluePreview` field of its own, because it is
    /// part of the enum record and wants the same change detection every other select field gets:
    /// swapping rows re-bakes the pet with the body.
    ///
    /// All three words matter. The display names the model; the **level and family are its size**
    /// (`CreatureFamily`'s ramp — decision 1538), so a freshly tamed wolf stands visibly smaller
    /// than a level-60 one.
    pub(crate) pet_display_id: u32,
    pub(crate) pet_level: u32,
    pub(crate) pet_family: u32,
}

/// The select pet, as the booth needs it: which creature, and the two words that size it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PetLook {
    /// `CreatureDisplayInfo` id — never `0` here ([`GlueLook::pet`] is what filters that out).
    pub(crate) display_id: u32,
    pub(crate) level: u32,
    pub(crate) family: u32,
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
            pet_display_id: c.pet_display_id,
            pet_level: c.pet_level,
            pet_family: c.pet_family,
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

    /// The **pet** standing beside the character, or `None`. Only a select look can carry one: the
    /// create screen builds a character that has no pet yet, and the wire field it would come from
    /// doesn't exist until the character does.
    pub(crate) fn pet(&self) -> Option<PetLook> {
        match self {
            GlueLook::Create(_) => None,
            GlueLook::Select(l) => (l.pet_display_id != 0).then_some(PetLook {
                display_id: l.pet_display_id,
                level: l.pet_level,
                family: l.pet_family,
            }),
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
    /// the enum's secondary model, wow-re `glue-select-model.md` TU-B; 0429's attachment-1
    /// presumption is dead.)
    char_spot: Vec3,
    /// The stage's **authored frame** — attachment 0's bone matrix ([`stage_frame`]): the rotation
    /// and uniform scale the scene author parked on that bone. Identity/1.0 with no scene;
    /// UI_Human's holds the −16.5° yaw that aims the character at that scene's off-axis camera
    /// (and a 0.97 scale), UI_Tauren's a 1.04 scale (1533).
    char_facing: Quat,
    char_scale: f32,
    /// The **pet's** spot — scene attachment 1 (`0x47306b` chains the secondary model there), a few
    /// yards off the stage. Every `UI_*` scene authors exactly these two attachments and no more,
    /// and both their bones are parentless roots — so the seat is that bone's own frame, with
    /// nothing above it to compose, which is why the pet is placed the same way the body is.
    pet_root: Entity,
    pet_spot: Vec3,
    /// The pet's share of the stage's frame — attachment **1**'s bone, read the same way
    /// [`Self::char_facing`] is. 1536 placed the pet with a pure translation on the reading that
    /// both attachment bones are unkeyed; that holds on five scenes and **not** on two of them.
    /// UI_Human's attachment-1 bone (bone 5) carries the identical constant −16.5° yaw and 0.97
    /// scale its attachment-0 bone does — without the yaw the pet stands square beside an owner
    /// turned 16.5° away — and UI_Tauren's parks a **0.7324** scale on the pet's bone alone, three
    /// quarters the size we were drawing it (1533).
    pet_facing: Quat,
    pet_stage_scale: f32,
    /// The pet bake standing at [`Self::pet_spot`] — its revision, so a re-bake happens once.
    pet_baked: Option<u64>,
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
/// direction (the `Model2.bls` fold, [`benilla_world::lighting::prop_probe_coeffs`]) — and **point**
/// lights become per-vertex lights on the buffer's point table, evaluated with the engine's fixed
/// falloff `1/(0.7·d + 0.03·d²)` (the authored attenuation radii are dead code in the reference;
/// `wow_model.wgsl`'s point term already implements the law). Both booth models read this one
/// rig — the scene at its origin, the character at the stage — exactly the reference's shared
/// scene-light gather. Light positions are authored in model space (the attachment convention),
/// bind-pose static in every UI_* scene.
struct SceneRig {
    /// The summed directional-light ambient (every UI_* point light authors ambient ×0).
    ambient: [f32; 3],
    /// The directional lobes, toward-light and committed — folded into probe slot 0.
    lobes: Vec<(Vec3, [f32; 3])>,
    /// The point lights: Bevy position + committed colour.
    points: Vec<(Vec3, [f32; 3])>,
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
            // `LightBlob::point` applies the same RAW commit the world table does (the `0x71ca80`
            // encode is undone by the `0x593040` decode — wow-re
            // trace-forensics-overgamut-point-commit-d3d): a glue scene is just another CM2Scene,
            // and the GL commit is shared. One law covers both tables, not two by coincidence.
            points.push((benilla_assets::coords::wow_to_bevy(l.position), color));
        } else {
            // Toward-light unit vector, as the probe fold expects (it re-normalizes).
            lobes.push((benilla_assets::coords::wow_to_bevy(l.bone_z), color));
        }
    }
    SceneRig {
        ambient,
        lobes,
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
    /// The batch's **animated material alpha**, carried to the booth's [`BoothPart`]. The CHARACTER
    /// assembly still supplies `None` here — threading it through the appearance layer is decision
    /// 0807's named gap, and the select body's look is signed off as it stands. The **pet** builder
    /// has the field in hand (it reads the display's `EntityPart`s straight, with no compositor in
    /// between), so it fills it.
    pub(crate) alpha_anim: Option<std::sync::Arc<benilla_formats::AlphaAnim>>,
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

/// One **camera-facing batch** on the assembled preview — the world path's billboard card
/// ([`benilla_world::billboard`]). The booth is a separate camera, so such a batch can't ride the world card
/// system; it's rebuilt here for the booth's own camera ([`super::booth::face_booth_billboards`]).
/// Its centred quad, its material, and the billboard bone/flag it rides.
///
/// Three sources, all seated the same way — under the body bone's joint at [`Self::offset`]:
///
/// - The character's own **eye-glow** (undead/night-elf, geoset 302). Seated on its billboard
///   bone's joint, whose frame already bakes the bone pivot (the 0130 rig identity), so its offset
///   is `ZERO` — the rigged case, exactly like the world's `BillboardCard::following_joint`.
/// - An **equipped item's** camera-facing batch — a wand's gem, a glowing rune on a weapon. 270 of
///   the 2681 `Item\` models author one. An item model gets no rig here, so its offset carries the
///   attach point *and* the batch's own model-local pivot.
/// - An **item glow's** camera-facing batch (decision 0805) — `Spells\Enchantments\Sparkle_A.m2` is
///   exactly one such quad and nothing else, and `ItemVisuals` 28 (10 item displays) hangs it. Same
///   rig-less seat, plus the glow slot's offset on the weapon model.
#[derive(Clone)]
pub(crate) struct PreviewBillboard {
    pub(crate) mesh: Handle<Mesh>,
    pub(crate) material: Handle<WowModelMaterial>,
    pub(crate) bone: u16,
    /// Where the card's **pivot** sits in that bone's joint frame (Bevy axes). `ZERO` for a batch of
    /// the rigged body itself — its joint already bakes the pivot.
    pub(crate) offset: Vec3,
    pub(crate) kind: benilla_formats::BillboardKind,
}

/// One **effect-bearing model** on the assembled preview, carried as its particle emitters plus the
/// composed seat: the BODY bone it rides and its offset expressed in that bone's frame. The booth
/// spawns one host there and owns the emitters off it, exactly as [`sync_glue_scene`] does for a
/// backdrop's braziers; any geometry the same model carries rides the ordinary [`PreviewRider`]
/// list at the same seat.
///
/// Two sources produce these, and the booth treats them identically:
///
/// - **An equipped item's own emitters** — the R14 PVP pauldron's `SPARKLE` twinkle, the held
///   torch's flame (decision 0813; `#bugs` B118 is *this* case, and the select screen showed
///   nothing at all until it was carried here). Seat = the body's attach point for that slot.
/// - **An item glow** (decision 0805) — the `Spells\Enchantments\*.mdx` effect a held weapon's
///   `ItemVisuals` id hangs on the weapon's own attachment point; all but three of the 35 shipped
///   glow models are pure emitters. Seat = the body attach point **plus** the slot's offset on the
///   weapon model.
#[derive(Clone)]
pub(crate) struct PreviewEffects {
    pub(crate) bone: u16,
    pub(crate) offset: Vec3,
    pub(crate) emitters: Vec<benilla_assets::ModelEmitter>,
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
    /// Every emitter-bearing model the look wears — the equipped items' own effects (decision 0813)
    /// and the held items' glows (decision 0805). See [`PreviewEffects`].
    pub(crate) effects: Vec<PreviewEffects>,
    /// The look's camera-facing batches (the eye-glow, a wand's gem, a glow model's quad) — seated at
    /// their seat under a body joint and faced to the booth camera, not drawn as plain meshes. See
    /// [`PreviewBillboard`].
    pub(crate) billboards: Vec<PreviewBillboard>,
    /// Per-hand weapon grip `[right, left]` — a hand whose attach point holds a weapon closes into the
    /// `HandsClosed` finger pose (wow-re `hand-grip-mechanism.md`). The builder knows the held items'
    /// attach ids (which the flat rider list drops), so it resolves the grip here for the booth spawn.
    pub(crate) grip: [bool; 2],
    pub(crate) revision: u64,
}

/// The select screen's **pet** — the hunter/warlock companion standing beside the selected
/// character (`SMSG_CHAR_ENUM`'s pet triple, the reference's "secondary model" `record+0x114`).
///
/// Its own resource, and its own revision, deliberately: the pet's model can land frames after the
/// body's, and folding it into [`GluePreviewBake`] would make that late arrival re-bake the
/// character too — restarting an already-correct Stand loop for nothing. Empty (`display_id == 0`)
/// whenever the look carries no pet, which is every character but a living hunter's or warlock's
/// (the server suppresses the triple for the rest — see [`benilla_protocol::Character`]).
#[derive(Resource, Default)]
pub(crate) struct GluePetBake {
    /// The pet's `CreatureDisplayInfo` id — `0` = no pet, and the booth stands empty.
    pub(crate) display_id: u32,
    pub(crate) parts: Vec<PreviewPart>,
    /// The pet's camera-facing batches — the voidwalker's eyes are exactly this.
    pub(crate) billboards: Vec<PreviewBillboard>,
    /// The pet model's **own** particle emitters, straight off the display cache — the imp's three
    /// green flame jets (bones 17/45/46, `FlameLickSmall`, rate keyed alive in every sequence), the
    /// felhunter's, the succubus's. Unlike the body's [`GluePreviewBake::effects`] — which are an
    /// *equipped item's* emitters, seated on a character bone through a host — these are authored on
    /// the pet's own skeleton, so they ride its own joints: the scene-backdrop recipe, not the
    /// worn-item one ([`sync_glue_pet`]).
    pub(crate) emitters: Vec<benilla_assets::ModelEmitter>,
    /// The uniform render scale — the **`CreatureFamily` level ramp**, `minScale` → `maxScale`
    /// across `minScaleLevel` → `maxScaleLevel`
    /// ([`benilla_formats::CreatureFamily::scale_at`], decision 1538).
    ///
    /// Not the `CreatureModelData.modelScale × CreatureDisplayInfo.scale` product this field
    /// briefly held: the reference computes that product one instruction before the ramp and then
    /// overwrites it (`0x472e87 fstp`), so the product is only ever the fallback for a family the
    /// table doesn't know. Either way the glue pet has no wire object, so — unlike every unit in
    /// the world — nothing has folded a scale in for it and the client must size it itself.
    pub(crate) scale: f32,
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
        // The shared booth view shape (HDR, no tonemap, no MSAA) — its doc in [`super`] says why
        // every booth camera must spawn through it.
        super::booth_view_shape(),
        Camera {
            order: -100 + GLUE_LAYER as isize,
            // Transparent (unlike the portrait/paper-doll booths' backdrop): the create screen
            // composites the character straight over the page, the ref's model-in-the-scene look.
            // FfxGlow's combine carries the scene alpha through for exactly this.
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        bevy::camera::RenderTarget::Image(image.clone().into()),
        benilla_world::ffx_glow::FfxGlow::BOOTH,
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
            // …which is `live_scene` in the gate, keyed on a scene being up rather than on this
            // flag: the glue booth is live from the moment a screen shows, before any bake.
            live: false,
            pending: Vec::new(),
            pending_since: None,
            aspect: 1.0,
            rigged: false,
            parked: false,
        },
    );
    // The background scene's own root (the character root above yaws; the scene never does).
    let scene_root = commands
        .spawn((Transform::IDENTITY, Visibility::Visible, layer.clone()))
        .id();
    // …and the pet's, a third sibling: it stands on its own scene attachment and, like the scene,
    // never yaws — the reference's facing call touches only the character component's model
    // (`0x4730e0` → `cc+0x38`), so dragging the character spins the character alone.
    let pet_root = commands
        .spawn((Transform::IDENTITY, Visibility::Visible, layer))
        .id();
    commands.insert_resource(CreateScene {
        root: scene_root,
        token: None,
        handle: None,
        spawned: false,
        cam: None,
        char_spot: Vec3::ZERO,
        char_facing: Quat::IDENTITY,
        char_scale: 1.0,
        pet_root,
        pet_spot: Vec3::ZERO,
        pet_facing: Quat::IDENTITY,
        pet_stage_scale: 1.0,
        pet_baked: None,
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
    mut mats: benilla_world::model_render::M2BatchMaterials,
    mut images: ResMut<Assets<Image>>,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
    mut cams: Query<(&BoothCam, &mut Transform, &mut Projection)>,
    window: Query<&Window, With<PrimaryWindow>>,
    device: Res<bevy::render::renderer::RenderDevice>,
    queue: Res<bevy::render::renderer::RenderQueue>,
    // The scene's particle-emitter spawn wiring (decision 0539 §5) + the skin-palette table and
    // its mirror registry (decision 0720) + the model-forms cache and mesh store (decision 0834)
    // — one tuple param (the 16-SystemParam ceiling).
    particle_assets: (
        ResMut<benilla_world::rig_palette::RigPalettes>,
        ResMut<benilla_world::rig_palette::RigPaletteMirrors>,
        ResMut<benilla_world::model_forms::ModelForms>,
        ResMut<Assets<Mesh>>,
    ),
) {
    let (mut palettes, mut mirrors, mut forms, mut mesh_assets) = particle_assets;
    let Some(booth) = booths.0.get(GLUE_SLOT) else {
        return;
    };
    let Some(which) = preview.scene else {
        // Off the glue screens: tear the scene down, free the model, square target back.
        if scene.token.is_some() {
            commands.entity(scene.root).despawn_related::<Children>();
            // The whole rig-state strip (pose buffer, player, palette slot — `clear_booth_rig`):
            // the scene root outlives its bakes.
            super::clear_booth_rig(&mut commands, scene.root);
            scene.token = None;
            scene.handle = None;
            scene.spawned = false;
            scene.cam = None;
            scene.char_spot = Vec3::ZERO;
            scene.char_facing = Quat::IDENTITY;
            scene.char_scale = 1.0;
            clear_pet(&mut commands, &mut scene);
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
        clear_pet(&mut commands, &mut scene);
        commands.entity(scene.root).despawn_related::<Children>();
        // The model may still be loading — strip the old rig state off the outliving root now,
        // not only at the next spawn (which may be many frames away).
        super::clear_booth_rig(&mut commands, scene.root);
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
        let stage = attachment_point(&model.skeleton, &model.attachments, 0).unwrap_or(Vec3::ZERO);
        // The stage's frame, not just its point — see [`stage_frame`].
        let (stage_facing, stage_scale) = model
            .attachments
            .iter()
            .find(|a| a.id == 0)
            .map_or((Quat::IDENTITY, 1.0), |a| stage_frame(model, a.bone));
        let rig = scene_rig(&model.lights);
        let (fog_rgb, fog_far) = scene_fog(token);
        // Rig materials read the probe + point table; the core rows still carry the plain ambient
        // (with a zero sun) so any non-rig lane that ever lands in the booth degrades sanely. The
        // directional fold lands in probe slot 0 — booth instances carry MeshTag 0, the rig lane's
        // read side. The dial is 1.0: the rig lane ignores it, so byte levels stand.
        let mut blob =
            benilla_world::lighting::LightBlob::model(rig.ambient, [0.0; 3], Vec3::NEG_Y)
                .probe(rig.ambient, &rig.lobes)
                .fog(fog_rgb, fog_far, fog)
                .dial(1.0);
        for (pos, color) in &rig.points {
            blob = blob.point(*pos, SCENE_POINT_RANGE, *color);
        }
        let light = scene
            .light
            .get_or_insert_with(|| blob.create(&device, "wow_create_scene_light"))
            .clone();
        // The scene's rigs skin from THIS buffer's palette region (decision 0720): register it
        // as a mirror (re-registration replaces — a rebuilt scene's old buffer drops).
        mirrors.0.insert("glue_scene", light.clone());
        blob.write(&queue, &light);
        scene.fog = fog;
        let dc = blob.probe_dc();
        info!(
            "create scene: UI_{token} rig — ambient {:?}, {} point light(s), probe dc ({:.2},{:.2},{:.2})",
            rig.ambient,
            blob.point_count(),
            dc[0],
            dc[1],
            dc[2],
        );
        // The scene model's render forms, built NOW (decision 0834): one backdrop model, behind
        // the glue screen — the booth-lane exception to the paced rule.
        let scene_handle = scene
            .handle
            .as_ref()
            .expect("scene.handle is Some — `model` above came from it");
        forms.ensure_now_rigged(scene_handle, &model.submeshes, &mut mesh_assets);
        let built = forms.slices(scene_handle);
        let (stat_forms, skin_forms) = (built.stat, built.skin.unwrap_or(&[]));
        let scene_parts: Vec<BoothPart> = model
            .submeshes
            .iter()
            .enumerate()
            .map(|(pi, s)| {
                // The create scene is lit by its OWN authored M2 rig against its own buffer —
                // never the world's sun (decisions 0429/0435) — and its clouds and water scroll.
                //
                // The authored batch order (index + 1) rides with it (decision 1449): a scene's
                // batches all hang off the scene root, so they share one transparent sort distance
                // and only this bias tells them apart — without it `UI_Human`'s ground-shadow decal
                // and its street tied, and the tie re-broke every frame.
                let order = u16::try_from(pi + 1).unwrap_or(u16::MAX);
                let material = mats.off_world(s, s.texture.clone(), order, &light, true);
                BoothPart {
                    skinned: skin_forms.get(pi).cloned(),
                    static_mesh: stat_forms
                        .get(pi)
                        .map(|(h, _)| h.clone())
                        .unwrap_or_default(),
                    material,
                    // The scene's authored per-batch material alpha (decision 0807). UI_Tauren
                    // alone: a 0.55 `LENSALPHA` corner vignette, a 0.99 `GROUNDSHADOW` decal, 0.15
                    // `CLOUDS`, a 0.73 gradient. Drawn at 1.0 the vignette blacked out the frame
                    // corners — B121.
                    alpha_anim: s.alpha_anim.clone(),
                }
            })
            .collect();
        let mut scene_rig = spawn_booth_model(
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
        // UI_* scene carries (MainMenu 28, Orc 11, NightElf 12…). Lit/fogged by the SCENE's own
        // light buffer: the ModelFFX fog covers the whole model, emitters included, carried by
        // `EffectLightOverride` into the shared lane's per-draw bind group. The seating, the
        // billboard arm and the teardown parenting are the shared recipe's (1539).
        let (emitters, fx_frames) = super::spawn_booth_own_emitters(
            &mut commands,
            &mut scene_rig,
            scene.root,
            &booth.layer,
            Some(&light),
            &model.emitters,
        );
        if emitters > 0 {
            info!(
                "create scene: UI_{token} — {emitters} particle emitter(s) up \
                 ({fx_frames} on a billboard frame)"
            );
        }
        scene_rig.finish(&mut commands);
        scene.spawned = true;
        scene.cam = model.camera0;
        scene.char_spot = stage;
        scene.char_facing = stage_facing;
        scene.char_scale = stage_scale;
        // The pet's seat comes off the same skeleton, one attachment along — frame and all. A scene
        // that somehow authors none leaves the pet at the origin rather than on the stage — visibly
        // wrong, and it never happens: all six `UI_*` scenes author 0 and 1.
        scene.pet_spot =
            attachment_point(&model.skeleton, &model.attachments, 1).unwrap_or(Vec3::ZERO);
        let (pet_facing, pet_stage_scale) = model
            .attachments
            .iter()
            .find(|a| a.id == 1)
            .map_or((Quat::IDENTITY, 1.0), |a| stage_frame(model, a.bone));
        scene.pet_facing = pet_facing;
        scene.pet_stage_scale = pet_stage_scale;
        // A rebuilt scene moved the seat, so the standing pet is re-seated (its bake is still good;
        // `sync_glue_pet` re-runs the placement every frame the spot changes).
        scene.pet_baked = None;
        scene.rev += 1; // the character re-lights onto the scene rig (sync_glue_booth's key)
        info!(
            "create scene: UI_{token} up — {} submeshes, camera {} stage {:?} facing {:.2}° \
             scale {:.3}",
            model.submeshes.len(),
            scene.cam.is_some(),
            scene.char_spot,
            scene.char_facing.to_euler(EulerRot::YXZ).0.to_degrees(),
            scene.char_scale,
        );
        // Seat the character on the stage at the current facing (the yaw-only path below only
        // runs when the yaw *changes*).
        apply_yaw(
            &mut commands,
            booth.root,
            scene.char_spot,
            scene.char_facing,
            scene.char_scale,
            preview.yaw,
        );
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
    // Through the house write-gate ([`benilla_assets::write_gated`]), for the reason that helper
    // exists: `Assets::get_mut` queues `AssetEvent::Modified` unconditionally — it cannot know
    // whether the caller wrote anything — and a Modified image is re-extracted and re-uploaded
    // whole by `prepare_assets<GpuImage>` next frame. This target is the glue scene's FULLSCREEN
    // render target: at 3200×1800 it is 22 MB, and taking the borrow once per frame just to
    // compare two u32s re-uploaded all 22 MB every frame on a screen where nothing moves
    // (~1.3 GB/s; measured with `WOW_ASSET_CHURN=1`, decision 0772). This was the ONE asset write
    // site in the tree not already behind that gate.
    benilla_assets::write_gated(
        images,
        target,
        |image| image.width() != w || image.height() != h,
        |image| {
            image.resize(Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            });
        },
    );
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
    mut booths: ResMut<Booths>,
    mut scene: Option<ResMut<CreateScene>>,
    mut booth_light: ResMut<BoothLight>,
    mut materials: ResMut<Assets<WowModelMaterial>>,
    creatures: Option<Res<Creatures>>,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
    mut cams: Query<(&BoothCam, &mut Transform, &mut Projection)>,
    mut palettes: ResMut<benilla_world::rig_palette::RigPalettes>,
    mut last: Local<Option<(u64, f32, u64)>>,
) {
    let Some(booth) = booths.0.get_mut(GLUE_SLOT) else {
        return;
    };
    // Where the character stands (the scene's stage spot while a scene is up, else the origin) —
    // and the scene revision, so the bake re-lights the moment a scene rig lands.
    let (spot, stage, stage_scale, scene_rev) =
        scene
            .as_deref()
            .map_or((Vec3::ZERO, Quat::IDENTITY, 1.0, 0), |s| {
                if s.spawned {
                    (s.char_spot, s.char_facing, s.char_scale, s.rev)
                } else {
                    (Vec3::ZERO, Quat::IDENTITY, 1.0, s.rev)
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
            // The despawn reaped meshes and anchors; the rig state on the ROOT needs its own
            // strip ([`super::clear_booth_rig`]).
            super::clear_booth_rig(&mut commands, booth.root);
            booth.rigged = false;
            booth.parked = false;
            *last = Some((bake.revision, preview.yaw, scene_rev));
            apply_yaw(
                &mut commands,
                booth.root,
                spot,
                stage,
                stage_scale,
                preview.yaw,
            );
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
                // The glue screens keep the old fallback deliberately: the create/select scene has
                // its own lifecycle (no map-scope teardown under it) and no parts-key retry to
                // ride, so waiting here would leave the pane empty instead of merely mislit.
                (Some(buf), Some(s)) => {
                    super::material_variant(&mut s.variants, buf, material, materials, true)
                        .unwrap_or_else(|| material.clone())
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
                // `None`, and a KNOWN GAP (decision 0807): a mirrored `PreviewPart` doesn't carry
                // the batch's alpha loops, so a character batch with an authored dimming constant
                // previews at 1.0 here. Closing it means threading `alpha_anim` through the
                // appearance-assembly layer — deliberately not folded into the B121 fix.
                alpha_anim: None,
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
        // The camera-facing batches (the eye-glow, a wand's gem, a glow model's quad), relit onto the
        // booth's buffer like everything else (harmless where the batch is fullbright — the variant
        // just keeps parity).
        let booth_billboards: Vec<BoothBillboardSpec> = bake
            .billboards
            .iter()
            .map(|b| BoothBillboardSpec {
                mesh: b.mesh.clone(),
                material: relight(&b.material, &mut scene, &mut booth_light, &mut materials),
                bone: b.bone,
                offset: b.offset,
                kind: b.kind,
            })
            .collect();
        commands.entity(booth.root).despawn_related::<Children>();
        let mut booth_rig = spawn_booth_model(
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
        // The worn items' effects: an equipped item model's OWN emitters (decision 0813 — the R14
        // pauldron's sparkle, the torch's flame; `#bugs` B118) and the held weapons' `ItemVisuals`
        // glows (decision 0805). One host per effect model at its seat, emitters owned by it — the
        // scene-brazier recipe (0539 §5), so they draw against THIS camera and fog on the scene's own
        // light. Children of the booth root through their joint, so the next re-bake's
        // `despawn_related` reaps them. The reference reaches both through the very same attach
        // primitive it uses in the world (`0x472c91` → `0x47a0c0` → `0x4798c0`), and the enum carries
        // no enchant, so what shows here is each item's intrinsic effects only.
        let (fx_emitters, fx_frames) = spawn_booth_effects(
            &mut commands,
            &mut booth_rig,
            &booth.layer,
            scene_buf.as_ref(),
            &bake
                .effects
                .iter()
                .map(|fx| BoothEffects {
                    bone: fx.bone,
                    offset: fx.offset,
                    emitters: fx.emitters.clone(),
                })
                .collect::<Vec<_>>(),
        );
        if fx_emitters > 0 {
            info!(
                "glue booth: {fx_emitters} item emitter(s) up on {} effect model(s), \
                 {fx_frames} on a billboard frame",
                bake.effects.len()
            );
        }
        // A fresh bake is animated by construction; the park state is the new rig's.
        booth.rigged = booth_rig.rigged();
        booth_rig.finish(&mut commands);
        booth.parked = false;
        // The character-only fallback framing (no scene up — art missing / still loading): the
        // body-frame transform with an aspect-aware projection, because the booth target is
        // window-sized now (the square-true `WowPortraitProjection` would stretch on it). While a
        // scene shows, `sync_glue_scene` re-aims onto the authored camera the same frame.
        // Aspect 1.0 into `body_frame`: only its TRANSFORM is kept here — the projection
        // beside it is the window-sized one this booth needs (decision 1069's aspect rides on
        // the discarded half).
        //
        // Since 1089 that transform is the model's own `<PlayerModel>` pane camera (raw index 1),
        // which is the client's full-body character framing — the right stand-in, and the fov below
        // now comes off the same record rather than from the retired body-fit constant. The crop
        // aspect is the portrait bake's ([`PORTRAIT_ASPECT`]) rather than this window's: exact
        // framing here does not matter, because `sync_glue_scene` re-aims onto the scene's authored
        // camera the frame the art lands, and this rig only ever shows for those few loading
        // frames.
        let (t, _) = body_frame(&anchors, 1.0);
        let record_fov = anchors
            .pane_camera
            .map_or(super::framing::PANE_FIXED_FOV, |c| c.fov);
        aim(
            &mut cams,
            GLUE_SLOT,
            &(
                t,
                Projection::from(PerspectiveProjection {
                    fov: diag_to_vert(record_fov, PORTRAIT_ASPECT),
                    near: 0.02,
                    far: 100.0,
                    ..default()
                }),
            ),
        );
    }
    apply_yaw(
        &mut commands,
        booth.root,
        spot,
        stage,
        stage_scale,
        preview.yaw,
    );
    *last = Some((bake.revision, preview.yaw, scene_rev));
}

/// Stand the select screen's **pet** on scene attachment 1 — the hunter/warlock companion beside
/// the selected character (the reference's secondary model `record+0x114`, wow-re
/// `glue-select-model.md` §A4/§B1).
///
/// The booth twin of [`sync_glue_booth`], and deliberately the simpler one: a pet is an ordinary
/// creature display, so its parts come straight off the shared display cache with no compositor,
/// no equipment, no geoset selection and no riders. What it does share is the law that matters —
/// a fresh instance looping its own **Stand**, relit onto the scene's authored rig, on the booth's
/// layer — plus the model's own particle emitters riding its own joints, which is what makes an
/// imp an imp.
///
/// It runs only while a scene is up: both the seat and the light come from the background model,
/// and a pet standing at the origin next to a body standing at the origin is worse than no pet at
/// all for the frame or two the art takes to land.
///
/// **Not yaw-driven.** Dragging the character spins the character: the reference's facing call
/// (`0x4730e0`) writes the character component's model transform and nothing else, and both scene
/// attachments are parentless unkeyed pivots, so the pet keeps the seat's own orientation.
#[allow(clippy::too_many_arguments)]
pub(super) fn sync_glue_pet(
    mut commands: Commands,
    pet: Res<GluePetBake>,
    booths: Res<Booths>,
    mut scene: Option<ResMut<CreateScene>>,
    mut materials: ResMut<Assets<WowModelMaterial>>,
    creatures: Option<Res<Creatures>>,
    anim_data: Option<Res<crate::creature_anim::AnimData>>,
    mut palettes: ResMut<benilla_world::rig_palette::RigPalettes>,
) {
    let Some(scene) = scene.as_deref_mut() else {
        return;
    };
    let Some(booth) = booths.0.get(GLUE_SLOT) else {
        return;
    };
    // No pet on this character (every class but a living hunter/warlock) — clear once and stop.
    if pet.display_id == 0 || pet.parts.is_empty() {
        if scene.pet_baked.is_some() {
            clear_pet(&mut commands, scene);
        }
        return;
    }
    if !scene.spawned {
        return; // no stage yet — nothing to stand on, nothing to be lit by
    }
    if scene.pet_baked == Some(pet.revision) {
        return; // already standing, and nothing about it changed
    }
    // The rig comes from the same display cache that produced the parts, so both are ready
    // together; if somehow not, leave `pet_baked` alone and retry next frame.
    let (Some(creatures), Some(light)) = (creatures.as_deref(), scene.light.clone()) else {
        return;
    };
    let Some(rig) = creatures.display_rig(pet.display_id) else {
        return;
    };
    let mut relight = |material: &Handle<WowModelMaterial>,
                       variants: &mut std::collections::HashMap<
        AssetId<WowModelMaterial>,
        Handle<WowModelMaterial>,
    >| {
        super::material_variant(variants, &light, material, &mut materials, true)
            .unwrap_or_else(|| material.clone())
    };
    let booth_parts: Vec<BoothPart> = pet
        .parts
        .iter()
        .map(|p| BoothPart {
            skinned: p.skinned_mesh.clone(),
            static_mesh: p.static_mesh.clone(),
            material: relight(&p.material, &mut scene.variants),
            alpha_anim: p.alpha_anim.clone(),
        })
        .collect();
    let booth_billboards: Vec<BoothBillboardSpec> = pet
        .billboards
        .iter()
        .map(|b| BoothBillboardSpec {
            mesh: b.mesh.clone(),
            material: relight(&b.material, &mut scene.variants),
            bone: b.bone,
            offset: b.offset,
            kind: b.kind,
        })
        .collect();
    commands
        .entity(scene.pet_root)
        .despawn_related::<Children>();
    super::clear_booth_rig(&mut commands, scene.pet_root);
    let mut pet_rig = spawn_booth_model(
        &mut commands,
        &mut palettes,
        scene.pet_root,
        booth.layer.clone(),
        &booth_parts,
        &[], // a pet wears nothing
        rig.inverse_bindposes
            .as_ref()
            .map(|ibp| (rig.skeleton, ibp, rig.animations)),
        anim_data.as_deref().map(|a| &a.0),
        BoothMotion::Loop, // Stand, looping — the same live scene the character is
        [false, false],    // …and no hands to grip with
        &booth_billboards,
    );
    // The pet's OWN emitters — the imp's flames (decision 1539). Authored on the pet's own bones,
    // so this is the model's-own recipe, NOT the worn-item one (`spawn_booth_effects`, which seats
    // a *separate* model's emitters on a body bone through a host): the flame at the hand rides the
    // hand through the Stand loop.
    //
    // Anchored at `pet_root`, whose scale is the pet's — a half-size imp gets half-size flames,
    // positions and quads alike (`particles::quads`, the `sizeByInstanceScale` flag all three of
    // the imp's set). Relit like every other booth draw, onto the SCENE's light buffer, so the
    // flames take the backdrop's fog instead of the world's time of day.
    let (emitters, fx_frames) = super::spawn_booth_own_emitters(
        &mut commands,
        &mut pet_rig,
        scene.pet_root,
        &booth.layer,
        Some(&light),
        &pet.emitters,
    );
    // After the emitter spawn: `anchor` writes into the pose buffer, `finish` is what commits it.
    pet_rig.finish(&mut commands);
    // The seat: attachment 1's whole frame — its translation, the constant yaw its bone carries on
    // the Human scene and the constant scale it carries on Human and Tauren (1533) — times the DBC
    // scale product the wire never carries here (see [`GluePetBake::scale`]).
    commands.entity(scene.pet_root).insert(
        Transform::from_translation(scene.pet_spot)
            .with_rotation(scene.pet_facing)
            .with_scale(Vec3::splat(pet.scale.max(0.01) * scene.pet_stage_scale)),
    );
    scene.pet_baked = Some(pet.revision);
    info!(
        "glue pet: display {} up — {} part(s), {} camera-facing, {} emitter(s) ({} on a billboard \
         frame), scale {:.3} at {:?}",
        pet.display_id,
        booth_parts.len(),
        booth_billboards.len(),
        emitters,
        fx_frames,
        pet.scale,
        scene.pet_spot,
    );
}

/// Tear the standing pet down (no pet on this character, or the stage went away).
fn clear_pet(commands: &mut Commands, scene: &mut CreateScene) {
    commands
        .entity(scene.pet_root)
        .despawn_related::<Children>();
    // The rig state lives on the root, which outlives its bakes — the despawn doesn't reach it.
    super::clear_booth_rig(commands, scene.pet_root);
    scene.pet_baked = None;
}

/// Seat the model root on the stage `spot`, in the stage's own frame (`stage` + `stage_scale`),
/// facing `yaw` (the ref's `Model:SetRotation` — rotation about the root's own origin, never about
/// the scene's).
fn apply_yaw(
    commands: &mut Commands,
    root: Entity,
    spot: Vec3,
    stage: Quat,
    stage_scale: f32,
    yaw: f32,
) {
    commands.entity(root).insert(
        Transform::from_translation(spot)
            .with_rotation(stage * Quat::from_rotation_y(yaw))
            .with_scale(Vec3::splat(stage_scale)),
    );
}

/// The stage's **authored frame**: attachment `bone`'s chain, composed from the constant rotation
/// and scale the scene author parked on it.
///
/// A stage is an M2 *attachment*, and an attachment carries a frame, not just a point. The
/// reference composes the whole thing: `child+0xfc = child+0xbc × (T(att.pos) · parentBone[att.bone])`
/// (`MatrixMultiply` at `0x71439b`, inside `0x714260`) — **rotation and scale go in, not only the
/// translation** (wow-re `glue/scratch/glue-preview-facing-law.md`, §the attachment leg).
/// Five of the six shipped `UI_*` scenes leave that bone unrotated,
/// because their camera is authored looking straight down the stage's +X and a WoW model's default
/// facing already looks that way (measured on the files: Orc +0.02°, Dwarf +0.62°, NightElf
/// −0.24°, Scourge 0.00°, Tauren +0.02° between the camera's eye→target and +X).
///
/// **`UI_Human`'s is the exception, and taking only the attachment *point* was why every Human
/// stood angled away on the login and select screens (1533).** That scene is a lifted chunk of
/// Elwynn at world (−225, −81), and its camera sits at azimuth −18.2° from the stage rather than
/// on the axis; the author aimed the character at it by keying the stage bone (bone 4) with a
/// constant **−16.5°** yaw — two identical quaternion keys `(0, 0, −0.14349, 0.98965)` on global
/// sequence 1, byte-read from the shipped `UI_Human.m2`.
///
/// Read off the model's baked global-sequence channels because that is where a *constant* M2
/// rotation lives: vanilla's rest pose is identity rotations with the pivot carrying bind position,
/// so an author who wants a bone held at an angle keys it and parks the key. The channels are
/// already conjugated into Bevy space by the loader, so the composed quaternion drops straight into
/// the character root's transform.
///
/// The **scale** rides the same channels and is uniform on every shipped scene — UI_Human parks
/// 0.97 on both its attachment bones, UI_Tauren 1.04 on the character's and 0.7324 on the pet's,
/// the other four none — so it is carried as one factor rather than a `Vec3`, and `x` is read
/// because a non-uniform stage would be an authoring surprise, not a case to silently average.
fn stage_frame(model: &M2Model, bone: u16) -> (Quat, f32) {
    let Some(anims) = model.animations.as_ref() else {
        return (Quat::IDENTITY, 1.0);
    };
    let mut rot = Quat::IDENTITY;
    let mut scale = 1.0;
    let mut idx = bone;
    // Cycle-guarded by the joint count, exactly as the pivot walk beside it is.
    for _ in 0..=model.skeleton.joints.len() {
        if let Some(g) = anims.global_bones.iter().find(|g| g.bone == idx) {
            // Parent-first composition up the chain; `t = 0` because these are parked keys — a
            // stage bone that genuinely animated would need the live joint, and none does.
            if let Some(c) = g.rotation.as_ref() {
                rot = c.sample(0.0) * rot;
            }
            if let Some(c) = g.scale.as_ref() {
                scale *= c.sample(0.0).x;
            }
        }
        let Some(joint) = model.skeleton.joints.get(usize::from(idx)) else {
            break;
        };
        match u16::try_from(joint.parent) {
            Ok(parent) => idx = parent,
            Err(_) => break, // -1 = root reached
        }
    }
    (rot, scale)
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

    /// **Only a select look stands a pet, and only when the wire named one.**
    ///
    /// `petDisplayId == 0` is the entire client-side gate: the server suppresses the triple for
    /// everything but a living hunter or warlock (vmangos `Player::BuildEnumData`), so a class or
    /// ghost test here would be a second, drifting copy of a rule we do not own. The create screen
    /// builds a character that has no pet yet and must never show one.
    #[test]
    fn a_pet_stands_only_for_a_select_look_whose_wire_named_one() {
        let mut c = benilla_protocol::Character {
            guid: 1,
            name: "Hunter".into(),
            race: 1,
            class: 3,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
            level: 60,
            zone: 0,
            map: 0,
            position: benilla_protocol::wire::Vector3d {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            flags: 0,
            equipment: [benilla_protocol::CharEnumItem::default(); 19],
            pet_display_id: 517,
            pet_level: 60,
            pet_family: 1,
        };
        assert_eq!(
            GlueLook::Select(SelectLook::from(&c)).pet(),
            Some(PetLook {
                display_id: 517,
                level: 60,
                family: 1,
            }),
            "the roster row's whole pet triple rides the select look — the level and family are \
             its size, not decoration"
        );

        c.pet_display_id = 0;
        assert_eq!(
            GlueLook::Select(SelectLook::from(&c)).pet(),
            None,
            "a zeroed triple is 'no pet' — the only gate the client applies"
        );

        // The create screen: a character being built has no pet, whatever class is picked.
        let create = GlueLook::Create(CreateLook {
            race: 1,
            sex: 0,
            class: 3, // hunter
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
        });
        assert_eq!(create.pet(), None);
    }

    /// A same-size `resize_target` must not touch the image at all. `Assets::get_mut` queues
    /// `AssetEvent::Modified` whether or not the caller writes, and this target is the glue
    /// scene's fullscreen render target — one needless borrow per frame re-uploaded 22 MB per
    /// frame at 3200×1800 while the login screen sat still (decision 0772).
    #[test]
    fn a_same_size_resize_does_not_mark_the_target_modified() {
        use bevy::asset::AssetEvent;
        use bevy::image::Image;

        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Image>();
        let handle = {
            let mut images = app.world_mut().resource_mut::<Assets<Image>>();
            images.add(Image::new_target_texture(
                64,
                64,
                bevy::render::render_resource::TextureFormat::bevy_default(),
                None,
            ))
        };
        // Drain the Added event from the insert above.
        app.update();
        let drain = |app: &mut App| {
            let world = app.world_mut();
            let mut events = world.resource_mut::<Messages<AssetEvent<Image>>>();
            let n = events.drain().count();
            n
        };
        drain(&mut app);

        // Same size, ten frames: silence.
        for _ in 0..10 {
            let mut images = app.world_mut().resource_mut::<Assets<Image>>();
            resize_target(&mut images, &handle, 64, 64);
        }
        app.update();
        assert_eq!(
            drain(&mut app),
            0,
            "a no-op resize must not queue AssetEvent::Modified — every one of those re-uploads \
             the whole target to the GPU"
        );

        // A real size change still lands.
        {
            let mut images = app.world_mut().resource_mut::<Assets<Image>>();
            resize_target(&mut images, &handle, 128, 96);
        }
        app.update();
        assert!(drain(&mut app) > 0, "a real resize must still be published");
        let images = app.world().resource::<Assets<Image>>();
        let image = images.get(&handle).expect("target still there");
        assert_eq!((image.width(), image.height()), (128, 96));
    }

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
        // Exactly the one point light, position converted, colour × intensity — over-gamut
        // preserved. This fixture's `(0.7, 0.8, 1.0) × 2.5` is deliberately over-driven so it pins
        // the mechanism: the raw product `(1.75, 2.0, 2.5)` leaves the parse untouched, and
        // `LightBlob::point` commits it RAW from here (the reference's `0x71ca80` encode is undone
        // by the `0x593040` decode) — the saturation the eye sees happens at the receiving
        // vertex's lighting clamp, never on this path.
        assert_eq!(rig.points.len(), 1);
        assert_eq!(
            rig.points[0].0,
            benilla_assets::coords::wow_to_bevy([1.0, 2.0, 3.0])
        );
        let c = rig.points[0].1;
        assert!(
            (c[0] - 1.75).abs() < 1e-6 && (c[1] - 2.0).abs() < 1e-6 && (c[2] - 2.5).abs() < 1e-6,
            "over-gamut preserved: {c:?}"
        );
        // Each directional's bone_z (wow→bevy) becomes a to-light lobe; the zero-intensity
        // ambient-only light contributes a zero lobe (colour 0). Never the point light.
        assert_eq!(
            rig.lobes,
            vec![
                (
                    benilla_assets::coords::wow_to_bevy([1.0, 0.0, 0.0]),
                    [2.0, 1.0, 0.4],
                ),
                (
                    benilla_assets::coords::wow_to_bevy([0.0, 1.0, 0.0]),
                    [0.0, 0.0, 0.0],
                ),
            ]
        );
    }
}
