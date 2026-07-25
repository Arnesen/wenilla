//! The ground selection ring — a projected decal under the current target, reaction-coloured.
//!
//! **Geometry: the reference's own mechanism** ([`project_ring`], wow-re selection-circle RE — the
//! collector is byte-verified terrain + WMO, flags `0x200122`, no walkability test): the actual
//! surface triangles inside the ring's box — terrain tiles + WMO faces
//! ([`GroundDecalSurface`]), **never** doodads/GameObjects — clipped to the box and textured
//! top-down, so the ring is pixel-coplanar with the visible ground, drapes down steps/ledges like
//! the reference, and passes *under* props. (A Bevy `ForwardDecal` was rejected: it distorts at
//! WoW's steep camera angle and its depth-prepass broke clutter. An earlier per-vertex height-probe
//! grid was replaced by this — a height field can't represent ledge faces and spiked on them.)
//!
//! **Texture + blend**: the reference's own `UnitSelectTexture.blp`, additive, **not** pulsed (the
//! reference's pulsing circle is a separate spell/AoE indicator). The baked alpha fade (bright arc,
//! fading tail) is oriented **camera-relative** each frame ([`ring_fade_angle`]): the bright arc
//! faces the viewer, the fade points away — the reference decal's behaviour (its projector
//! transform is camera-fed).
//!
//! **Colour: the ring's own selector** (`CGUnit::GetSelectionCircleColor` `0x605960`, trace + byte
//! verified — NOT the nameplate palette): players branch to pale blue / hostile red; NPCs to
//! dead-gray or the reaction-rank palette (red / orange / yellow / green) — see
//! [`RingMaterials::pick`]. The reaction rank resolves in the client's own order
//! ([`ring_reaction`]): **reputation rank first** (a reputation faction's NPCs colour by our
//! standing with them), else the faction-template comparator
//! ([`benilla_formats::FactionTemplate::reaction_toward`], the byte-exact `0x606640`), evaluated as
//! the **unit's** reaction toward the local player (byte-verified direction). Death **clears the
//! target** on the alive→dead transition — the reference's own mechanism, byte-verified (wow-re
//! selection-death-clear RE): the health mirror's death edge fires the CGUnit death handler
//! (`0x605860`), which clears a matching selection and sends `CMSG_SET_SELECTION 0`.
//!
//! Still *interim*: the vertical fade profile on stretched (wall/ledge) pieces is capture-matched —
//! the reference's edge-fade grid (`0x6147f0`) is byte-located but its ramp is underived (open RE
//! item). The ring appears at full brightness the instant of selection — director-verified and
//! byte-confirmed (the retracted "2 s selection fade-in" note was a misread of the *scale-change*
//! easing, see [`crate::net::NetEntity`]; no fade arms on any selection path). The selector's
//! **first-priority branch** — the melee combat flash (`[unit+0xc58]` bit 0x10, the red↔orange
//! triangle pulse) — is live: [`super::CombatFlash`] carries the frame's verdict + colour and
//! outranks every branch below (player/dead/reaction), the byte order. The player path's party
//! legs are live (0434 phase 6 — pale blue / pale green off the roster). Deferred selector
//! states: the full PvP attackability matrix (X/Y), forced reactions, contested-guard.

use avian3d::prelude::Collider;
use benilla_formats::{load_faction_catalog, reputation_rank, FactionCatalog, Reaction};
use benilla_protocol::EntityKind;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::prelude::*;

use crate::assets::{LockRecover, WorldAssets};
use crate::collision::GroundDecalSurface;
use crate::decal::{project_decal, DecalFrame};
use crate::net::{NetCommands, NetEntity, ObjectStore, Reputations, SelfPlayer};

use super::click::clear;
use super::{CombatFlash, Selection, SelectionRadius};
use crate::creature_anim::Engaged;
use crate::player::WorldCamera;

/// The single, persistent ground-ring entity (spawned once, hidden until a target is set).
#[derive(Component)]
pub(super) struct SelectionRing;

/// The reference's ground selection-circle texture (`Textures\UnitSelectTexture.blp`, wow-re
/// selection-circle RE) — a white ring, sampled top-down, tinted + additively blended below.
const RING_TEXTURE: &str = "mpq://textures/unitselecttexture.blp";
/// Model-local ring radius for a unit with no model (a cube fallback), since it has no M2 footprint.
const RING_FALLBACK_RADIUS: f32 = 0.7;
/// Rasterizer depth bias on the ring materials (`StandardMaterial::depth_bias` →
/// `DepthBiasState::constant`, in depth-ulp units). The projected vertices rest **exactly on** the
/// drawn ground — geometrically coplanar — so without a bias the depth test dissolves into
/// per-pixel f32 noise (stipple, view-dependent bites). This pushes the ring's *depth* toward the
/// camera by ~1e-3·distance yards-equivalent (8192 · 2⁻²³): far above the coplanar noise floor
/// (~ulp of Elwynn's 10⁴-yard world coords), far below anything visible. Geometric lifts (0.1,
/// 0.02) read as hovering at grazing angles — director-confirmed; the reference fights the same
/// coplanarity with polygon offset (trace-verified), the fixed-function twin of this bias.
const RING_DEPTH_BIAS: f32 = 8192.0;
/// The ring's own palette — **trace + byte verified end-to-end** (wow-re selection-circle §5, the
/// `CGUnit::GetSelectionCircleColor` selector `0x605960`, per-object vtable `+0x2c`; the dword is
/// written verbatim as every decal vertex's diffuse, alpha 255 — no tint global, no tex-env
/// constant). The ring does **not** use the nameplate palette: player-blue is the pale
/// **`0xFF6060FF`** (96,96,255), not the nameplate's pure blue. NPC branch indexes the raw reaction
/// rank: 0–1 red `0xFFFF0000`, **2 unfriendly orange `0xFFFF8000`**, 3 neutral yellow `0xFFFFFF00`,
/// 4–7 friendly green `0xFF00FF00`; a **dead NPC** overrides to mid-gray `0xFF7F7F7F` (players skip
/// the health check). Tints draw at full strength (an earlier ×0.5 theory was refuted by pixels).
/// The selector's first-priority branch — the combat flash (`0xFFFF0000↔0xFFFF8000` pulse) — is
/// live via [`super::CombatFlash`] and outranks all of these. The player path's `¬X∧¬Y` leg is
/// live (0434 phase 6): PvP-flagged → green `0xFF00FF00` (party member → pale-green
/// `0xFFAAFFAA`), unflagged → the soft blue (party member → pale-blue `0xFFAAAAFF`, the
/// selector's 4-slot party-guid table `0xbc6f48` = our roster). Still deferred: the
/// cross-faction attackability matrix X/Y (approximated as hostile-red on rank ≤ 1) and the
/// CHARMEDBY/SUMMONEDBY owner resolve inside the PvP-flag read (we read the unit's own flag).
// GAMMA LANE (0161): raw authored bytes into the gamma framebuffer (see nameplates.rs).
const RING_HOSTILE: Color = Color::linear_rgb(1.0, 0.0, 0.0);
const RING_UNFRIENDLY: Color = Color::linear_rgb(1.0, 0.502, 0.0);
const RING_NEUTRAL: Color = Color::linear_rgb(1.0, 1.0, 0.0);
const RING_FRIENDLY: Color = Color::linear_rgb(0.0, 1.0, 0.0);
const RING_PLAYER: Color = Color::linear_rgb(0.376, 0.376, 1.0);
const RING_DEAD: Color = Color::linear_rgb(0.498, 0.498, 0.498);
const RING_PARTY: Color = Color::linear_rgb(0.667, 0.667, 1.0); // 0xFFAAAAFF
const RING_PARTY_PVP: Color = Color::linear_rgb(0.667, 1.0, 0.667); // 0xFFAAFFAA

/// The FactionTemplate.dbc catalog, for the ring's reaction colour. Absent if the DBC failed to load
/// (the ring then stays the neutral fallback).
#[derive(Resource)]
pub(crate) struct Factions(FactionCatalog);

impl Factions {
    /// The loaded catalog — for sibling faction consumers (the zone PvP state reads our own
    /// template's group mask through it, decision 0287).
    pub(crate) fn catalog(&self) -> &FactionCatalog {
        &self.0
    }
}

/// The ring's prebuilt reaction materials (same texture/blend, different tint). [`update_ring`]
/// swaps the ring's material handle between them as the resolved reaction changes — no per-frame
/// material mutation, with one deliberate exception: the `flash` material's tint is rewritten each
/// shown frame with the wave sample (the reference recolours its global every frame too).
#[derive(Resource)]
pub(super) struct RingMaterials {
    hostile: Handle<StandardMaterial>,
    unfriendly: Handle<StandardMaterial>,
    neutral: Handle<StandardMaterial>,
    friendly: Handle<StandardMaterial>,
    player: Handle<StandardMaterial>,
    dead: Handle<StandardMaterial>,
    /// The party legs of the player path's `¬X∧¬Y` split (pale blue / pale green).
    party: Handle<StandardMaterial>,
    party_pvp: Handle<StandardMaterial>,
    /// The combat-flash pulse — tint mutated per frame to [`CombatFlash::color`].
    flash: Handle<StandardMaterial>,
}

/// The ring colour the selector resolves — the pure classification half of [`RingMaterials::pick`],
/// split out so the branch logic (players vs NPCs, the `¬X∧¬Y` party split) is unit-testable
/// without material handles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RingVariant {
    Hostile,
    Unfriendly,
    Neutral,
    Friendly,
    Player,
    Dead,
    Party,
    PartyPvp,
}

/// The ring's own colour selector (`0x605960`) on the raw reaction rank (`0..=7`): **players**
/// branch first and never check health (a dead player doesn't gray) — hostile red on rank ≤ 1
/// (the X/Y matrix approximation), else the `¬X∧¬Y` split: PvP-flagged → green / party
/// pale-green, unflagged → soft blue / party pale-blue (the 4-slot party table `0xbc6f48`, our
/// roster — self is never in it, and self reads blue/green exactly like the law's own-guid legs);
/// **NPCs** — dead → gray, else the rank palette (0–1 red, 2 orange, 3 yellow, 4–7 green).
fn ring_variant(
    rank: u8,
    is_player: bool,
    is_dead: bool,
    pvp: bool,
    in_party: bool,
) -> RingVariant {
    if is_player {
        return if rank <= 1 {
            RingVariant::Hostile
        } else if pvp {
            if in_party {
                RingVariant::PartyPvp
            } else {
                RingVariant::Friendly
            }
        } else if in_party {
            RingVariant::Party
        } else {
            RingVariant::Player
        };
    }
    if is_dead {
        return RingVariant::Dead;
    }
    match rank {
        0..=1 => RingVariant::Hostile,
        2 => RingVariant::Unfriendly,
        3 => RingVariant::Neutral,
        _ => RingVariant::Friendly,
    }
}

impl RingMaterials {
    /// The material for a resolved [`RingVariant`] (the handle-mapping half of the selector).
    fn pick(
        &self,
        rank: u8,
        is_player: bool,
        is_dead: bool,
        pvp: bool,
        in_party: bool,
    ) -> &Handle<StandardMaterial> {
        match ring_variant(rank, is_player, is_dead, pvp, in_party) {
            RingVariant::Hostile => &self.hostile,
            RingVariant::Unfriendly => &self.unfriendly,
            RingVariant::Neutral => &self.neutral,
            RingVariant::Friendly => &self.friendly,
            RingVariant::Player => &self.player,
            RingVariant::Dead => &self.dead,
            RingVariant::Party => &self.party,
            RingVariant::PartyPvp => &self.party_pvp,
        }
    }
}

/// Spawn the one ring entity (hidden) and build its shared mesh + the reaction materials once.
pub(super) fn setup_ring(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    asset_server: Res<AssetServer>,
) {
    let mesh = meshes.add(crate::decal::seed_mesh());
    let texture = asset_server.load::<Image>(RING_TEXTURE);
    let mut tinted = |base_color| {
        materials.add(StandardMaterial {
            base_color,
            base_color_texture: Some(texture.clone()),
            // Unlit + additive (the reference's `GL_SRC_ALPHA/GL_ONE`): the ring glows on the ground
            // regardless of time-of-day and never darkens it.
            unlit: true,
            alpha_mode: AlphaMode::Add,
            cull_mode: None,
            // Wins the depth test over the terrain it lies exactly on (see [`RING_DEPTH_BIAS`]).
            depth_bias: RING_DEPTH_BIAS,
            ..default()
        })
    };
    let ring_mats = RingMaterials {
        hostile: tinted(RING_HOSTILE),
        unfriendly: tinted(RING_UNFRIENDLY),
        neutral: tinted(RING_NEUTRAL),
        friendly: tinted(RING_FRIENDLY),
        player: tinted(RING_PLAYER),
        dead: tinted(RING_DEAD),
        party: tinted(RING_PARTY),
        party_pvp: tinted(RING_PARTY_PVP),
        // Seeded red (the wave's G=0 endpoint); update_ring rewrites the tint every flash frame.
        flash: tinted(RING_HOSTILE),
    };
    commands.spawn((
        SelectionRing,
        Mesh3d(mesh),
        MeshMaterial3d(ring_mats.neutral.clone()),
        Transform::default(),
        Visibility::Hidden,
        // The projector mutates the mesh each frame and Bevy doesn't recompute an entity's `Aabb`
        // on asset change — skip culling for this one tiny always-near-the-target mesh.
        NoFrustumCulling,
    ));
    commands.insert_resource(ring_mats);
}

/// Startup (after the MPQ chain opens): load FactionTemplate.dbc for the reaction colour. On failure
/// the resource is simply absent and the ring stays neutral yellow.
pub(super) fn load_factions(mut commands: Commands, world_assets: Option<Res<WorldAssets>>) {
    let Some(world_assets) = world_assets else {
        return;
    };
    let mut chain = world_assets.chain.lock_recover();
    match load_faction_catalog(&mut chain) {
        Ok(catalog) => {
            info!("faction catalog: {} template rows", catalog.len());
            commands.insert_resource(Factions(catalog));
        }
        Err(e) => warn!("faction catalog unavailable, ring stays neutral: {e:#}"),
    }
}

/// Position, size, colour + show the ring under the current target each frame; hide it when nothing is
/// selected. Radius = the unit's model ring footprint ([`SelectionRadius`], the Stand-box
/// `sqrt(0.5·sqrt(dx²+dy²))`) × its transform scale (`OBJECT_FIELD_SCALE_X`). Colour = the target's
/// reaction rank ([`ring_reaction`]), re-resolved each frame (faction can change live — the store
/// merges `Values` deltas), the handle swapped only on change. No pulse — the reference's unit ring
/// is steady. If the target's entity is gone (destroyed / streamed out) the selection clears and the
/// server is told — the reference's teardown clear sends `CMSG_SET_SELECTION 0` on both paths.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn update_ring(
    mut selection: ResMut<Selection>,
    factions: Option<Res<Factions>>,
    reputations: Res<Reputations>,
    flash: Res<CombatFlash>,
    ring_mats: Res<RingMaterials>,
    // The mesh + material stores, tupled into one param (Bevy caps function systems at 16).
    mut assets: (ResMut<Assets<Mesh>>, ResMut<Assets<StandardMaterial>>),
    camera: Query<&GlobalTransform, With<WorldCamera>>,
    surfaces: Query<&Collider, With<GroundDecalSurface>>,
    // The last camera-relative fade angle, kept across frames so a degenerate (straight-down) camera
    // holds the previous orientation instead of snapping.
    mut fade_angle: Local<f32>,
    // The last guid whose colour decision was logged (log once per target change, not per frame).
    mut logged_guid: Local<Option<u64>>,
    // Last frame's (target guid, dead?) — the alive→dead edge on the *same* unit clears the
    // selection. Tracked per frame keyed on the guid, never armed only at selection *change*: a
    // `.respawn`ed creature reuses its spawn guid, so change-armed state goes stale and misses the
    // second kill (the bug this replaces).
    mut last_vitals: Local<Option<(u64, bool)>>,
    net_commands: Res<NetCommands>,
    // `Without<SelectionRing>` makes this disjoint from the ring's own `&mut Transform` below (no access
    // conflict); net entities are roots, so their `Transform` is already world-space + current this frame
    // (net motion ran in `WorldStage::Net`), avoiding the 1-frame lag a `GlobalTransform` read would add.
    // Tupled into one param (the 16-param ceiling): `.0` the target's own components; `.1` the
    // mounted footprint source — while a mount model is attached, the ring reads the MOUNT's
    // Stand-box footprint at the mount's rendered scale (VERIFIED wow-re `mount-composition.md`:
    // the `+0xcf0` ring cache recomputes from the mount model's Stand box, `0x60ce70` tail →
    // `0x60aee0`; the scale law is B3 — `SCALE_X × CreatureDisplayInfo.creatureModelScale`, and
    // the child's `NetEntity.scale` carries exactly the CDI column).
    targets: (
        Query<
            (
                &Transform,
                Option<&SelectionRadius>,
                Option<&ObjectStore>,
                Option<&NetEntity>,
                Option<&crate::entities::mount::MountChild>,
            ),
            Without<SelectionRing>,
        >,
        Query<(&NetEntity, Option<&SelectionRadius>), With<crate::entities::mount::MountBody>>,
        // The party roster — the selector's 4-slot guid table (the party ring colours).
        Res<crate::ui_party::GroupState>,
    ),
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    // Are *we* mid auto-attack (server-echoed [`Engaged`])? Both clear paths below end the swing
    // then — the reference's death/teardown edges stop the attack along with the selection.
    engaged: Query<(), (With<Engaged>, With<SelfPlayer>)>,
    mut ring: Query<
        (
            &mut Transform,
            &mut Visibility,
            &mut MeshMaterial3d<StandardMaterial>,
            &Mesh3d,
        ),
        With<SelectionRing>,
    >,
) {
    let Ok((mut ring_tf, mut vis, mut ring_mat, ring_mesh)) = ring.single_mut() else {
        return;
    };
    let hide = match selection.target {
        None => {
            // No target: drop the per-target trackers, so re-selecting the *same* guid later is a
            // fresh start (vitals re-read, colour decision re-logged).
            *last_vitals = None;
            *logged_guid = None;
            true
        }
        Some(target) => match targets.0.get(target) {
            Ok((unit, sel_radius, store, net, mount_child)) => {
                // A real model uses its own footprint (even if small); only a model-less unit falls back.
                // The 0.05 floor just avoids a degenerate (zero-bounds) model rendering an invisible ring.
                // Mounted, the footprint and the extra scale column come from the mount child
                // (the `mount_parts` doc above); a still-loading mount rides the fallback the
                // way any model-less unit does until its bounds land.
                let (local, mount_scale) = match mount_child.and_then(|mc| targets.1.get(mc.0).ok())
                {
                    Some((mnet, msel)) => (msel.map_or(RING_FALLBACK_RADIUS, |r| r.0), mnet.scale),
                    None => (sel_radius.map_or(RING_FALLBACK_RADIUS, |r| r.0), 1.0),
                };
                let local = local.max(0.05);
                let radius = local * (unit.scale.x * mount_scale).max(0.01);
                // The mesh is built in feet-relative world axes (yards); the transform only places it.
                ring_tf.translation = unit.translation;
                ring_tf.scale = Vec3::ONE;
                ring_tf.rotation = Quat::IDENTITY;
                *fade_angle = ring_fade_angle(&camera).unwrap_or(*fade_angle);
                let projected = project_ring(
                    &mut assets.0,
                    ring_mesh,
                    &surfaces,
                    unit.translation,
                    radius,
                    *fade_angle,
                );
                let rank = ring_reaction(
                    factions.as_deref(),
                    &reputations,
                    store,
                    self_store.single().ok(),
                );
                let is_player = net.is_some_and(|n| n.kind == EntityKind::Player);
                // The ¬X∧¬Y split's inputs: the unit's own PvP flag (UNIT_FIELD_FLAGS 0x1000;
                // the charm-owner resolve is noted residue) + roster membership by guid.
                let pvp = store.is_some_and(|s| s.0.unit_flags() & 0x1000 != 0);
                let in_party = selection
                    .guid
                    .is_some_and(|g| targets.2.members.iter().any(|m| m.guid == g));
                // Dead (health 0, absent-counts-as-zero — `unit_is_dead`) — re-read each frame, so
                // the ring reacts the moment the kill's health delta merges into the store, and an
                // already-dead corpse reads dead on stream-in.
                let is_dead = store.is_some_and(|s| s.0.unit_is_dead());
                // Death clears the target on the alive→dead *transition* — the reference's own
                // mechanism, byte-verified (wow-re selection-death-clear RE): the health mirror-
                // handler's death edge (`0x6046f0`, new ≤ 0 while old > 0) fires the CGUnit death
                // handler `0x605860`, which clears a matching selection via SetSelection(0) and
                // sends `CMSG_SET_SELECTION 0` — exactly what `clear` does. Edge-only, like the
                // reference: deliberately selecting a corpse afterwards stays valid (gray ring).
                // (The same edge also stops auto-attack and cancels a cast at the dying unit —
                // faithful siblings for when combat/casting exist.)
                let died = is_dead
                    && selection
                        .guid
                        .is_some_and(|g| *last_vitals == Some((g, false)));
                *last_vitals = selection.guid.map(|g| (g, is_dead));
                if died {
                    clear(&mut selection, &net_commands, !engaged.is_empty());
                    *last_vitals = None;
                    *vis = Visibility::Hidden;
                    return;
                }
                // One line per target change — the whole colour decision, so a wrong ring colour in
                // the field is diagnosable from the log instead of guessed at.
                if selection.guid != *logged_guid {
                    *logged_guid = selection.guid;
                    info!(
                        "target: guid {:?} ftpl {:?} (self ftpl {:?}) → rank {rank}{}{}",
                        selection.guid,
                        store.and_then(|s| s.0.unit_faction_template()),
                        self_store
                            .single()
                            .ok()
                            .and_then(|s| s.0.unit_faction_template()),
                        if is_player { " [player→blue]" } else { "" },
                        if is_dead { " [dead→gray]" } else { "" },
                    );
                }
                // The selector's first-priority branch (`0x605960`, byte order): the combat flash
                // outranks player/dead/reaction. Its material tint is rewritten with this frame's
                // wave sample — the module's one per-frame material mutation.
                let want = if flash.unit == Some(target) {
                    if let Some(mat) = assets.1.get_mut(&ring_mats.flash) {
                        mat.base_color = flash.color;
                    }
                    &ring_mats.flash
                } else {
                    ring_mats.pick(rank, is_player, is_dead, pvp, in_party)
                };
                if ring_mat.0 != *want {
                    ring_mat.0 = want.clone();
                }
                // No receiving surface in the box (mid-air, unstreamed tile) → hide, the reference's
                // own no-ground gate (`0x6d74b5`: the whole draw is skipped).
                !projected
            }
            // The target entity no longer exists (destroyed or streamed out): clear, informing the
            // server — the reference's teardown does exactly this for both removal paths (object
            // deactivate → the selection clear + `CMSG_SET_SELECTION 0`, byte-verified — wow-re
            // selection-death-clear RE; this is also what drops a selected corpse at respawn, when
            // the server destroys it ahead of the fresh create).
            Err(_) => {
                clear(&mut selection, &net_commands, !engaged.is_empty());
                *last_vitals = None;
                *logged_guid = None;
                true
            }
        },
    };
    *vis = if hide {
        Visibility::Hidden
    } else {
        Visibility::Visible
    };
}

/// The ring fade's camera-relative angle θ: the texture's **faded side always points away from the
/// camera** (the bright arc faces the viewer — the reference decal's behaviour, director-confirmed;
/// its projector transform is camera-fed). The fade is baked into `UnitSelectTexture.blp`'s alpha
/// (bright at v=1 → ring-local **+Z**, transparent at v=0 → **−Z**, measured off the shipped
/// texture); [`project_ring`] rotates each vertex's UV by −θ, which is equivalent to yawing the
/// texture square by θ so its −Z side lands on the camera's ground-projected forward
/// (a yaw θ maps a local `atan2(z,x)` angle φ to `φ − θ`; local −Z sits at −π/2; so
/// `θ = −π/2 − atan2(f.z, f.x)`). `None` looking straight down (degenerate forward) — the caller
/// keeps the previous angle rather than snapping to an arbitrary one.
fn ring_fade_angle(camera: &Query<&GlobalTransform, With<WorldCamera>>) -> Option<f32> {
    let cam = camera.single().ok()?;
    let f = cam.forward();
    let flat = Vec3::new(f.x, 0.0, f.z);
    if flat.length_squared() < 1e-6 {
        return None;
    }
    Some(-std::f32::consts::FRAC_PI_2 - flat.z.atan2(flat.x))
}

/// Rebuild the ring mesh as a **projected decal** — the reference's actual mechanism (wow-re
/// selection-circle RE §2: clip world geometry to the projection box, texture it top-down), via
/// the shared projector ([`crate::decal::project_decal`] — the blob shadow rides the same emit
/// chain, `0x6d7330 → 0x6d6fa0 → 0x6d7480`). The ring's box: the *rotated* texture square
/// (half-extent `s` = radius, yawed by the camera fade angle — the clip frame is exactly the
/// texture frame, so UVs stay in `[0,1]`) × the byte-verified vertical half-range **2s** (the
/// unit ring's box corners are `center±s` horizontal, `center±2s` vertical — wow-re `0x608e00`).
/// Vertex alpha is the vertical trapezoid fade: full within ±0.5s of the feet, ramping to 0 at
/// the box's ±2s — so a smear up a wall / down a ledge dims with height the way the director's
/// reference capture shows, instead of ending in a hard clip line. *Interim profile*: the
/// reference's edge-fade alpha grid is byte-located (`0x6147f0`) but its exact ramp is unrecorded
/// (open RE item). Runs every shown frame (target moves, camera yaws); the per-surface BVH makes
/// the gather O(log n + k). Returns `false` when nothing was gathered (no ground in the box) —
/// the caller hides the ring, the reference's no-ground gate.
fn project_ring(
    meshes: &mut Assets<Mesh>,
    ring_mesh: &Mesh3d,
    surfaces: &Query<&Collider, With<GroundDecalSurface>>,
    feet: Vec3,
    radius: f32,
    fade_angle: f32,
) -> bool {
    let (sin, cos) = fade_angle.sin_cos();
    let vert = 2.0 * radius;
    let frame = DecalFrame {
        center: feet,
        sin,
        cos,
        min_x: -radius,
        max_x: radius,
        min_z: -radius,
        max_z: radius,
        min_y: -vert,
        max_y: vert,
    };
    project_decal(
        meshes,
        ring_mesh,
        surfaces,
        &frame,
        |p| ((vert - p.y.abs()) / (1.5 * radius)).clamp(0.0, 1.0),
        |x, z| frame.rect_uv(x, z),
    )
}

/// The target's reaction toward our player — the direction the reference colours by (its nameplate/ring
/// resolver `0x7cbaa0` calls `unit->UnitReaction(activePlayer)`, byte-verified; the reverse would read
/// every attackable-but-passive yellow beast as hostile red, since a *player* template is enemy-masked
/// against the whole Monster group), resolved in the client's own order (`0x606530`):
///
/// **First reputation, then the comparator.** If the unit's faction *has a reputation slot*
/// (`FactionHasReputation` `0x605fc0`), the reaction is **our reputation rank** with it
/// (`0x4d63a0`): DBC race/class base + the `SMSG_INITIALIZE_FACTIONS` standing, ranked
/// hated→exalted — *before* any template comparison, which is why every Stormwind NPC is green to a
/// human (base 4000 = friendly) **even in GM mode** (director-verified on the reference: the GM
/// faction template never gets consulted for reputation-faction NPCs). Only a reputation-less
/// faction falls through to the faction-template comparator (byte-exact `0x606640`) over both
/// units' `UNIT_FIELD_FACTIONTEMPLATE`.
///
/// **Before either of those, the duel leg** (decision 0633, byte-exact `UnitReaction 0x6061e0`).
/// The real function runs a player-vs-player ladder *ahead of* the faction work, gated on
/// `UNIT_FIELD_FLAGS` bit 3 (`0x8` `UNIT_FLAG_PVP_ATTACKABLE`, behaviourally "player-controlled")
/// being set on **both** parties. Its first rung is the duel ([`duel_reaction`]): when both
/// players carry a non-zero `PLAYER_DUEL_TEAM` **and** the same `PLAYER_DUEL_ARBITER`, the answer
/// is `1` (hostile) for opposing teams and `4` (friendly) for the same team — `0x606296`'s
/// `setne/dec/and 3/inc`, which maps *equal* → 4 and *unequal* → 1. That is the whole reason a
/// duel opponent turns red, becomes attackable, and takes the combat flash without any of those
/// systems knowing what a duel is. The ladder's later rungs — the party leg (`0x6062b0`) and the
/// both-FFA leg (`0x60632c`) — are NOT implemented here: party colouring already runs through
/// [`ring_variant`]'s own split, and FFA waits for PvP. Both are strictly *below* the duel rung,
/// so their absence cannot change a duel's answer.
///
/// Deferred pieces of the real orchestration: contested-guard flag, forced reactions
/// (`SMSG_SET_FORCED_REACTIONS`), and the summon +1 tail. Returns the **raw reaction rank**
/// (`0..=7` — the scale the ring palette indexes: 0–1 red, 2 orange, 3 yellow, 4–7 green); the
/// comparator's `{1, 3, 4}` sit on the same scale. **3 (neutral)** when anything is missing (no
/// catalog, fields not yet streamed) — the reference resolver's own fall-through is the yellow branch.
pub(crate) fn ring_reaction(
    factions: Option<&Factions>,
    reputations: &Reputations,
    target_store: Option<&ObjectStore>,
    self_store: Option<&ObjectStore>,
) -> u8 {
    if let (Some(target), Some(own)) = (target_store, self_store) {
        if let Some(rank) = duel_reaction(&target.0, &own.0) {
            return rank;
        }
    }
    let resolved = (|| {
        let catalog = &factions?.0;
        let self_store = self_store?;
        let target_tpl = catalog.template(target_store?.0.unit_faction_template()?)?;
        // 1. The reputation branch: rank with the unit's faction, when it has a reputation slot.
        if let Some(info) = catalog.reputation_faction(target_tpl.faction) {
            let standing = reputations
                .0
                .get(info.rep_index as usize)
                .map_or(0, |&(_flags, s)| s);
            let race = self_store.0.unit_race().unwrap_or(0);
            let class = self_store.0.unit_class().unwrap_or(0);
            return Some(reputation_rank(info.base_for(race, class) + standing));
        }
        // 2. The faction-template comparator ({1, 3, 4} on the rank scale).
        let self_tpl = catalog.template(self_store.0.unit_faction_template()?)?;
        Some(target_tpl.reaction_toward(self_tpl) as u8)
    })();
    resolved.unwrap_or(Reaction::Neutral as u8)
}

/// `UNIT_FLAG_PVP_ATTACKABLE` — `UNIT_FIELD_FLAGS` bit 3. Behaviourally "player-controlled": the
/// local player and other players carry it, wild creatures do not. `UnitReaction 0x606217`/
/// `0x60622f` requires it on both parties before any player-vs-player rung runs, and `CanAttack
/// 0x606a13` selects its three reaction legs on the same bit.
const UNIT_FLAG_PVP_ATTACKABLE: u32 = 1 << 3;

/// The duel rung of `UnitReaction` (`0x60626b`–`0x6062ad`, byte-exact). `Some(1)` when the two
/// are duelling each other on **opposing** teams, `Some(4)` on the same team (the client's own
/// arithmetic; only pets ever land there), `None` when no duel relates them — the caller then
/// falls through to the faction work.
///
/// The gate is deliberately all three facts and not the arbiter alone: `PLAYER_DUEL_ARBITER` is
/// set on both players the moment the challenge goes out, while `PLAYER_DUEL_TEAM` stays `0`
/// until `Player::UpdateDuelFlag` fires at the end of the countdown. Testing only the arbiter
/// would turn the opponent red during the popup and the 3-second count — before a blow may
/// legally land.
fn duel_reaction(
    target: &benilla_protocol::ObjectFields,
    own: &benilla_protocol::ObjectFields,
) -> Option<u8> {
    if own.unit_flags() & UNIT_FLAG_PVP_ATTACKABLE == 0
        || target.unit_flags() & UNIT_FLAG_PVP_ATTACKABLE == 0
    {
        return None;
    }
    let own_team = own.player_duel_team();
    let target_team = target.player_duel_team();
    if own_team == 0 || target_team == 0 {
        return None;
    }
    let arbiter = own.player_duel_arbiter();
    if arbiter == 0 || arbiter != target.player_duel_arbiter() {
        return None;
    }
    Some(if own_team == target_team {
        Reaction::Friendly as u8
    } else {
        Reaction::Hostile as u8
    })
}

#[cfg(test)]
mod tests {
    use super::{ring_variant, RingVariant};

    /// The selector's player path — the `¬X∧¬Y` split the party arc added (0434 phase 6). A
    /// friendly player reads soft blue solo, pale blue in our party; PvP-flagged reads green
    /// solo, pale green in party; hostile rank still wins for a player; a dead player never grays.
    #[test]
    fn player_ring_party_and_pvp_split() {
        let v = |pvp, in_party| ring_variant(5, true, false, pvp, in_party);
        assert_eq!(
            v(false, false),
            RingVariant::Player,
            "friendly solo = soft blue"
        );
        assert_eq!(
            v(false, true),
            RingVariant::Party,
            "friendly party = pale blue"
        );
        assert_eq!(v(true, false), RingVariant::Friendly, "pvp solo = green");
        assert_eq!(
            v(true, true),
            RingVariant::PartyPvp,
            "pvp party = pale green"
        );
        // A hostile-rank player is red regardless of party/pvp (the X/Y approximation).
        assert_eq!(
            ring_variant(1, true, false, true, true),
            RingVariant::Hostile
        );
        // Players skip the health check — a dead ally still reads its party colour, never gray.
        assert_eq!(ring_variant(5, true, true, false, true), RingVariant::Party);
    }

    /// The NPC path is untouched by the party inputs: dead grays, else the reaction palette,
    /// and pvp/in_party never apply to a non-player.
    #[test]
    fn npc_ring_ignores_party_inputs() {
        assert_eq!(ring_variant(0, false, true, true, true), RingVariant::Dead);
        assert_eq!(
            ring_variant(2, false, false, true, true),
            RingVariant::Unfriendly
        );
        assert_eq!(
            ring_variant(3, false, false, false, false),
            RingVariant::Neutral
        );
        assert_eq!(
            ring_variant(5, false, false, true, true),
            RingVariant::Friendly
        );
    }
}
