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

use benilla_formats::{load_faction_catalog, reputation_rank, FactionCatalog, Reaction};
use benilla_protocol::EntityKind;
use bevy::prelude::*;

use crate::net::{NetEntity, ObjectStore, Reputations, SelfPlayer};
use benilla_assets::{LockRecover, WorldAssets};
use benilla_world::decal::{DecalFrame, WorldDecal};
use benilla_world::particles::buffer::EffectVertex;

use super::click::clear;
use super::{CombatFlash, Selection, SelectionRadius};
use crate::creature_anim::Engaged;
use benilla_world::view::WorldCamera;

/// The single ring record (a resource since 0733 — the projection rides the effect stream; no
/// entity, no mesh, no materials). `update_ring` rebuilds `verts` when the [`RingKey`] moves and
/// re-resolves `color` every frame; `push_ring` copies the slice onto the stream tinted.
#[derive(Resource, Default)]
pub(super) struct RingState {
    verts: Vec<EffectVertex>,
    key: RingKey,
    /// This frame's resolved tint (the selector's dword — or the combat flash's wave sample).
    color: Color,
    shown: bool,
}

/// The projection's rebuild inputs — a still target under a still camera costs a compare
/// (0733 §5, the ShadowKey treatment; the old path re-projected every shown frame).
#[derive(Default, PartialEq, Clone, Copy)]
struct RingKey {
    feet: Vec3,
    radius: f32,
    fade_angle: f32,
    surfaces: usize,
}

/// The ring texture (the render-side residency gate withholds the draw until it loads).
#[derive(Resource)]
pub(super) struct RingAssets {
    texture: Handle<Image>,
}

/// The reference's ground selection-circle texture (`Textures\UnitSelectTexture.blp`, wow-re
/// selection-circle RE) — a white ring, sampled top-down, tinted + additively blended below.
const RING_TEXTURE: &str = "mpq://textures/unitselecttexture.blp";
/// Model-local ring radius for a unit with no model (a cube fallback), since it has no M2 footprint.
const RING_FALLBACK_RADIUS: f32 = 0.7;
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

/// The colour `GetSelectionCircleColor` resolves — the pure classification half of the
/// selector, split out so the branch logic (players vs NPCs, the `¬X∧¬Y` party split) is
/// unit-testable.
///
/// **This is the single source for BOTH surfaces the selector feeds** — the ground ring here and
/// the overhead name (`nameplates.rs` fetches the same `vtable+0x2c`, decision 0156). It was a
/// duplicated mirror until decision 0659: the ring gained the PvP/party legs with 0453 and the
/// name's copy did not, so a flagged player drew a green ring under a blue name. One law, one
/// function; the name maps this to its own material cache.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum RingVariant {
    Hostile,
    Unfriendly,
    Neutral,
    Friendly,
    Player,
    Dead,
    Party,
    PartyPvp,
}

impl RingVariant {
    /// Every variant, for a consumer that pre-builds one material per colour.
    pub(crate) const ALL: [Self; 8] = [
        Self::Hostile,
        Self::Unfriendly,
        Self::Neutral,
        Self::Friendly,
        Self::Player,
        Self::Dead,
        Self::Party,
        Self::PartyPvp,
    ];

    /// The selector's dword for this variant (see the palette constants above).
    pub(crate) fn color(self) -> Color {
        match self {
            Self::Hostile => RING_HOSTILE,
            Self::Unfriendly => RING_UNFRIENDLY,
            Self::Neutral => RING_NEUTRAL,
            Self::Friendly => RING_FRIENDLY,
            Self::Player => RING_PLAYER,
            Self::Dead => RING_DEAD,
            Self::Party => RING_PARTY,
            Self::PartyPvp => RING_PARTY_PVP,
        }
    }
}

/// The ring's own colour selector (`0x605960`) on the raw reaction rank (`0..=7`): **players**
/// branch first and never check health (a dead player doesn't gray) — hostile red on rank ≤ 1
/// (the X/Y matrix approximation), else the `¬X∧¬Y` split: PvP-flagged → green / party
/// pale-green, unflagged → soft blue / party pale-blue (the 4-slot party table `0xbc6f48`, our
/// roster — self is never in it, and self reads blue/green exactly like the law's own-guid legs);
/// **NPCs** — dead → gray, else the rank palette (0–1 red, 2 orange, 3 yellow, 4–7 green).
///
/// Shared with the overhead name (decision 0659) — see [`RingVariant`].
pub(crate) fn ring_variant(
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

/// Load the ring texture and seed the ring record. (The old path built 9 tinted
/// `StandardMaterial` clones and a mesh asset here; the tint is per-vertex colour at push time
/// now — the reference writes the selector's dword as every decal vertex's diffuse, which is
/// exactly what the stream does.)
pub(super) fn setup_ring(mut commands: Commands, asset_server: Res<AssetServer>) {
    let texture = asset_server.load::<Image>(RING_TEXTURE);
    commands.insert_resource(RingAssets { texture });
    commands.init_resource::<RingState>();
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
    mut state: ResMut<RingState>,
    camera: Query<&GlobalTransform, With<WorldCamera>>,
    decals: WorldDecal,
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
    mut seam: crate::creature_anim::AttackSeam,
    // Net entities are roots, so their `Transform` is already world-space + current this frame
    // (net motion ran in `WorldStage::Net`), avoiding the 1-frame lag a `GlobalTransform` read
    // would add. Tupled into one param (the 16-param ceiling): `.0` the target's own components;
    // `.1` the mounted footprint source — while a mount model is attached, the ring reads the
    // MOUNT's Stand-box footprint at the mount's rendered scale (VERIFIED wow-re
    // `mount-composition.md`: the `+0xcf0` ring cache recomputes from the mount model's Stand
    // box, `0x60ce70` tail → `0x60aee0`; the scale law is B3 —
    // `SCALE_X × CreatureDisplayInfo.creatureModelScale`, and the child's `NetEntity.scale`
    // carries exactly the CDI column).
    targets: (
        Query<(
            &Transform,
            Option<&SelectionRadius>,
            Option<&ObjectStore>,
            Option<&NetEntity>,
            Option<&crate::entities::mount::MountChild>,
        )>,
        Query<(&NetEntity, Option<&SelectionRadius>), With<crate::entities::mount::MountBody>>,
        // The party roster — the selector's 4-slot guid table (the party ring colours).
        Res<crate::ui_party::GroupState>,
    ),
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    // Are *we* mid auto-attack (server-echoed [`Engaged`])? Both clear paths below end the swing
    // then — the reference's death/teardown edges stop the attack along with the selection.
    engaged: Query<(), (With<Engaged>, With<SelfPlayer>)>,
) {
    let state = &mut *state;
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
                *fade_angle = ring_fade_angle(&camera).unwrap_or(*fade_angle);
                // The rebuild gate (0733 §5): a still target under a still camera keeps the
                // cached projection — the old path re-projected every shown frame.
                let key = RingKey {
                    feet: unit.translation,
                    radius,
                    fade_angle: *fade_angle,
                    surfaces: decals.receiver_count(),
                };
                let projected = if state.shown && key == state.key {
                    !state.verts.is_empty()
                } else {
                    state.verts.clear();
                    state.key = key;
                    project_ring(&mut state.verts, &decals, unit.translation, radius, {
                        *fade_angle
                    })
                };
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
                    clear(&mut selection, &mut seam, !engaged.is_empty());
                    *last_vitals = None;
                    state.shown = false;
                    state.verts.clear();
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
                // The selector's first-priority branch (`0x605960`, byte order): the combat
                // flash outranks player/dead/reaction. The tint is a per-vertex colour at push
                // time now, so the flash's per-frame wave sample costs a resource write — the
                // old path's one material mutation is gone.
                state.color = if flash.unit == Some(target) {
                    flash.color
                } else {
                    ring_variant(rank, is_player, is_dead, pvp, in_party).color()
                };
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
                clear(&mut selection, &mut seam, !engaged.is_empty());
                *last_vitals = None;
                *logged_guid = None;
                true
            }
        },
    };
    state.shown = !hide;
    if hide {
        state.verts.clear();
    }
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
/// the shared projector ([`benilla_world::decal::WorldDecal::project`] — the blob shadow rides the same emit
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
    out: &mut Vec<EffectVertex>,
    decals: &WorldDecal<'_, '_>,
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
    decals.project(
        out,
        &frame,
        |p| ((vert - p.y.abs()) / (1.5 * radius)).clamp(0.0, 1.0),
        |x, z| frame.rect_uv(x, z),
    )
}

/// Push the shown ring's cached projection onto the stream, tinted with this frame's resolved
/// colour — one Add draw at the ring rung (the top decal, over the blob shadows), fog off.
pub(super) fn push_ring(
    assets: Option<Res<RingAssets>>,
    state: Option<Res<RingState>>,
    cam: Query<Entity, With<WorldCamera>>,
    mut draw: benilla_world::particles::buffer::WorldEffectDraw,
) {
    let (Some(assets), Some(state)) = (assets, state) else {
        return;
    };
    let Ok(cam) = cam.single() else { return };
    if !state.shown || state.verts.is_empty() {
        return;
    }
    let tint = state.color.to_linear();
    let mut batch = draw
        .batch(cam, assets.texture.id())
        .additive()
        .anchored(state.key.feet)
        .rung(
            benilla_world::sky_order::Rung::RING,
            benilla_world::sky_order::Rung::RING as i32,
        );
    batch.extend(state.verts.iter().map(|v| EffectVertex {
        pos: v.pos,
        uv: v.uv,
        // The selector's dword as every vertex's diffuse — the reference's own wiring
        // (`0x605960` → vertex colour, alpha = the vertical fade the projector baked).
        color: [tint.red, tint.green, tint.blue, v.color[3]],
    }));
    batch.tris();
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
        if ffa_reaction(&target.0, &own.0) {
            return Reaction::Hostile as u8;
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
    match duel_rung(target, own) {
        DuelRung::Engaged { rank, .. } => Some(rank),
        _ => None,
    }
}

/// The **both-FFA** rung of `UnitReaction` (`0x60632c`, the ladder 0633 §5 read in full; decision
/// 0646 §5): two player-controlled units that are BOTH free-for-all flagged are hostile to each
/// other, whatever their factions say. This is the only rung an ordinary PvP flag does *not* have
/// — a same-faction player who flags for PvP stays friendly, which is why there is no `pvp` arm
/// beside this one.
///
/// The flag is `PLAYER_FLAGS` bit 7 (`PLAYER_FLAGS_FFA_PVP`, vmangos `Player.h:322`), the same
/// field and bit the unit snapshot feeds `UnitIsPVPFreeForAll` from.
fn ffa_reaction(
    target: &benilla_protocol::ObjectFields,
    own: &benilla_protocol::ObjectFields,
) -> bool {
    const PLAYER_FLAGS_FFA_PVP: u32 = 0x80;
    let player_controlled =
        |u: &benilla_protocol::ObjectFields| u.unit_flags() & UNIT_FLAG_PVP_ATTACKABLE != 0;
    player_controlled(target)
        && player_controlled(own)
        && target.player_flags() & PLAYER_FLAGS_FFA_PVP != 0
        && own.player_flags() & PLAYER_FLAGS_FFA_PVP != 0
}

/// `UNIT_FIELD_FLAGS` bits `CanAttack` (`0x606980`) tests on its TARGET, any one of which refuses
/// the attack outright — byte-verified at `0x6069b7`–`0x6069ff` (wow-re
/// `object-layer/scratch/nameplate-category-gate.md` §3b). **The bit NUMBERS are VERIFIED; the
/// vanilla names are not** (three wow-re workers hunted for a labelled consumer of bit 9 and found
/// none), so nothing here is written in terms of a name — 1 `0x2`, 7 `0x80`, 16 `0x10000`,
/// 20 `0x100000`, 25 `0x2000000`.
const CANNOT_BE_ATTACKED: u32 = 0x2 | 0x80 | 0x1_0000 | 0x10_0000 | 0x200_0000;
/// The two cross-flag immunity bits `CanAttack` reads on BOTH sides (`0x606a05`–`0x606a8a`): bit 8
/// `0x100` and bit 9 `0x200`, each paired against the other unit's `PLAYER_CONTROLLED` bit 3.
const IMMUNE_TO_PLAYER_CONTROLLED: u32 = 0x100;
const IMMUNE_TO_UNCONTROLLED: u32 = 0x200;
/// `UNIT_FIELD_FLAGS` bit 12 (`0x1000`) — the PvP-flag the both-players arm of `CanAttack` accepts
/// on its target (`0x606b5c`).
const UNIT_FLAG_PVP: u32 = 0x1000;

/// The **local player's** reaction toward a unit — `0x6061e0(this = localPlayer, arg = unit)`,
/// byte-verified (wow-re `nameplate-category-gate.md` §5 leg 3 + §8a, §5 cross-checked 2026-08-22).
///
/// **This is NOT [`ring_reaction`] with the arguments swapped, and that is the whole point.** The
/// two directions take genuinely different code inside `0x6061e0`, and the client uses both:
///
/// - **This direction** (`A` = the local player) always reaches leg 3 at `0x606372`, because
///   `0x606170(localPlayer)` resolves to the player themselves. Leg 3 answers a rep-slot faction
///   with the **AT-WAR BIT** — `at_war ? 1 : 4` — and never looks at the standing. Only a
///   faction with no reputation slot falls through to the template comparator.
/// - **[`ring_reaction`]'s direction** (`A` = the unit) never reaches leg 3, falls into `0x606530`,
///   and *there* the standing is read. That is the correct input for the plate's bar COLOUR, the
///   ring, and the overhead name.
///
/// So a not-at-war neutral-standing NPC — a Booty Bay goblin, an Argent Dawn quartermaster, a
/// battleground emissary — is **friendly** to this function and **neutral** to the other one, and
/// the client shows exactly that: a friendly-category plate with a yellow bar. Substituting the
/// standing here is what put those 36 shipped faction templates in the enemy category (1530).
///
/// Deferred, and each one only ever makes a unit *more* friendly than we say: the forced-reaction
/// table (`0x4d6490`, `SMSG_SET_FORCED_REACTIONS` — no wire support yet), the party rung of the
/// player-vs-player block, and the charmed-player case that would stop leg 3 firing at all.
pub(crate) fn reaction_from_player(
    factions: Option<&Factions>,
    reputations: &Reputations,
    target_store: Option<&ObjectStore>,
    self_store: Option<&ObjectStore>,
) -> u8 {
    // The player-vs-player block (`0x606217`) sits ahead of all faction work in BOTH directions,
    // and benilla implements the same two rungs of it here that [`ring_reaction`] does.
    if let (Some(target), Some(own)) = (target_store, self_store) {
        if let Some(rank) = duel_reaction(&target.0, &own.0) {
            return rank;
        }
        if ffa_reaction(&target.0, &own.0) {
            return Reaction::Hostile as u8;
        }
    }
    let resolved = (|| {
        let catalog = &factions?.0;
        let target_tpl = catalog.template(target_store?.0.unit_faction_template()?)?;
        // Leg 3: a faction that owns a reputation slot is answered by the AT-WAR bit alone.
        // `faction_flags::AT_WAR` is the same wire byte the reputation pane's war checkbox reads,
        // which is what makes declaring war on Booty Bay turn its goblins' plates red *and* move
        // them into the enemy category — one bit, both consequences, as the reference has it.
        if let Some(info) = catalog.reputation_faction(target_tpl.faction) {
            let at_war = usize::try_from(info.rep_index)
                .ok()
                .and_then(|i| reputations.0.get(i))
                .is_some_and(|&(flags, _)| flags & benilla_formats::faction_flags::AT_WAR != 0);
            return Some(if at_war {
                Reaction::Hostile as u8
            } else {
                Reaction::Friendly as u8
            });
        }
        // Leg 4: no reputation slot → the template comparator, PLAYER → UNIT.
        let self_tpl = catalog.template(self_store?.0.unit_faction_template()?)?;
        Some(self_tpl.reaction_toward(target_tpl) as u8)
    })();
    resolved.unwrap_or(Reaction::Neutral as u8)
}

/// `CGUnit::CanAttack(this = the local player → arg = unit)` — `0x606980`, byte-verified complete
/// (wow-re `nameplate-category-gate.md` §3, §5 cross-checked 2026-08-22). Every leg, in the
/// binary's order.
///
/// This is the predicate the V-plate category actually turns on (see [`plate_is_friendly`]), and
/// it is deliberately **not** a reaction threshold: §3b lists six shipped ways it disagrees with
/// one, including an unflagged opposite-faction player on a PvE realm (hostile reaction, cannot be
/// attacked → friendly plate) and a same-faction duel opponent (friendly reaction, can be attacked
/// → enemy plate).
pub(crate) fn can_attack_from_player(
    factions: Option<&Factions>,
    reputations: &Reputations,
    target_store: Option<&ObjectStore>,
    self_store: Option<&ObjectStore>,
    target_is_player: bool,
) -> bool {
    let (Some(target), Some(own)) = (target_store, self_store) else {
        return false; // fields not streamed yet — the reference's own null path refuses
    };
    let (tflags, oflags) = (target.0.unit_flags(), own.0.unit_flags());
    // (a) The ghost gate (`0x606987`): a ghost PLAYER can only be attacked by an attacker holding a
    // creature record whose type-flags carry `0x2` — which a player never does. So for the local
    // player as attacker this leg is unconditional.
    const PLAYER_FLAGS_GHOST: u32 = 0x10;
    if target_is_player && target.0.player_flags() & PLAYER_FLAGS_GHOST != 0 {
        return false;
    }
    // (b) Five target-flag disqualifiers.
    if tflags & CANNOT_BE_ATTACKED != 0 {
        return false;
    }
    // (c) The four cross-flag immunity legs, each pairing one side's immunity bit against the
    // other's `PLAYER_CONTROLLED`.
    let (t_controlled, o_controlled) = (
        tflags & UNIT_FLAG_PVP_ATTACKABLE != 0,
        oflags & UNIT_FLAG_PVP_ATTACKABLE != 0,
    );
    if o_controlled && tflags & IMMUNE_TO_PLAYER_CONTROLLED != 0
        || !o_controlled && tflags & IMMUNE_TO_UNCONTROLLED != 0
        || oflags & IMMUNE_TO_PLAYER_CONTROLLED != 0 && t_controlled
        || oflags & IMMUNE_TO_UNCONTROLLED != 0 && !t_controlled
    {
        return false;
    }
    // (d) The three terminal arms, selected by the two `PLAYER_CONTROLLED` bits.
    let toward_target = || reaction_from_player(factions, reputations, target_store, self_store);
    match (o_controlled, t_controlled) {
        // Neither is player-controlled: hostile in EITHER direction is enough.
        (false, false) => {
            toward_target() <= Reaction::Hostile as u8
                || ring_reaction(factions, reputations, target_store, self_store)
                    <= Reaction::Hostile as u8
        }
        // Both are: the PvP arm. A friendly reaction refuses outright; past that an attack needs a
        // live duel, the target's PvP flag, or mutual FFA. (The charm-owner resolve `0x606170` is
        // an identity for both sides here — benilla has no charm.)
        (true, true) => {
            if toward_target() >= Reaction::Friendly as u8 {
                return false;
            }
            matches!(duel_rung(&target.0, &own.0), DuelRung::Engaged { .. })
                || tflags & UNIT_FLAG_PVP != 0
                || ffa_reaction(&target.0, &own.0)
        }
        // The mixed arm — the local player against any ordinary NPC, i.e. the case the plate gate
        // takes for almost everything on screen: attackable iff worse than friendly.
        _ => toward_target() < Reaction::Friendly as u8,
    }
}

/// `CanCooperate(this = the local player → arg = unit)` — `0x606ba0`, byte-verified (wow-re
/// `nameplate-category-gate.md` §2a): the two `FactionTemplate` rows' **faction-group masks**
/// (`row + 0xc`) being equal, with neither side mind-controlled and the two not being the same
/// unit. It reads no party or raid state — a committed wow-re note called it a party/raid predicate
/// and was corrected by the same round.
pub(crate) fn can_cooperate_with_player(
    factions: Option<&Factions>,
    target_store: Option<&ObjectStore>,
    self_store: Option<&ObjectStore>,
) -> bool {
    let (Some(target), Some(own)) = (target_store, self_store) else {
        return false;
    };
    if target.0.unit_charmed_by().is_some_and(|g| g != 0)
        || own.0.unit_charmed_by().is_some_and(|g| g != 0)
    {
        return false;
    }
    let resolved = (|| {
        let catalog = &factions?.0;
        let target_tpl = catalog.template(target.0.unit_faction_template()?)?;
        let self_tpl = catalog.template(own.0.unit_faction_template()?)?;
        Some(target_tpl.group_mask == self_tpl.group_mask)
    })();
    resolved.unwrap_or(false)
}

/// **The V-plate category** — `0x60f6b7`–`0x60f6f1`, byte-verified (wow-re
/// `nameplate-category-gate.md` §2). `true` = the FRIENDLY bucket (Shift-V's bit `0x8`), `false` =
/// the ENEMY bucket (V's bit `0x1`). There is no reaction rank anywhere in this expression.
///
/// > A unit lands in the FRIENDLY category iff — for a non-player subject —
/// > `CanAttack(localPlayer → subject)` is FALSE; and for a player subject, additionally
/// > `CanCooperate(localPlayer → subject)` is TRUE.
pub(crate) fn plate_is_friendly(
    factions: Option<&Factions>,
    reputations: &Reputations,
    target_store: Option<&ObjectStore>,
    self_store: Option<&ObjectStore>,
    target_is_player: bool,
) -> bool {
    let attackable = can_attack_from_player(
        factions,
        reputations,
        target_store,
        self_store,
        target_is_player,
    );
    if target_is_player {
        can_cooperate_with_player(factions, target_store, self_store) && !attackable
    } else {
        !attackable
    }
}

/// Why [`duel_reaction`]'s rung did or did not fire, with the values it judged on. This is the
/// diagnostic face of the same walk — `/reaction` prints it, so a duel that fails to turn the
/// opponent red names the gate that refused instead of silently reporting "neutral". Keeping one
/// walk with a richer return (rather than a second copy in the probe) is what stops the
/// instrument and the law from drifting apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DuelRung {
    /// One or both sides lack `UNIT_FIELD_FLAGS` bit 3, so no player-vs-player rung runs at all.
    NotPlayerControlled { own: bool, target: bool },
    /// No duel under way yet: `PLAYER_DUEL_TEAM` is still `0` on one or both sides (it is set
    /// only when `Player::UpdateDuelFlag` fires at the end of the countdown).
    NoTeam { own: u32, target: u32 },
    /// Both carry a team, but they are not duelling **each other** — or the arbiter guid has not
    /// streamed to us.
    ArbiterMismatch { own: u64, target: u64 },
    /// The rung fires: `1` (hostile) on opposing teams, `4` (friendly) on the same one.
    Engaged {
        rank: u8,
        own_team: u32,
        target_team: u32,
    },
}

/// The duel rung's walk, reporting the gate that decided (see [`DuelRung`]).
pub(crate) fn duel_rung(
    target: &benilla_protocol::ObjectFields,
    own: &benilla_protocol::ObjectFields,
) -> DuelRung {
    let own_pc = own.unit_flags() & UNIT_FLAG_PVP_ATTACKABLE != 0;
    let target_pc = target.unit_flags() & UNIT_FLAG_PVP_ATTACKABLE != 0;
    if !own_pc || !target_pc {
        return DuelRung::NotPlayerControlled {
            own: own_pc,
            target: target_pc,
        };
    }
    let own_team = own.player_duel_team();
    let target_team = target.player_duel_team();
    if own_team == 0 || target_team == 0 {
        return DuelRung::NoTeam {
            own: own_team,
            target: target_team,
        };
    }
    let arbiter = own.player_duel_arbiter();
    let target_arbiter = target.player_duel_arbiter();
    if arbiter == 0 || arbiter != target_arbiter {
        return DuelRung::ArbiterMismatch {
            own: arbiter,
            target: target_arbiter,
        };
    }
    DuelRung::Engaged {
        rank: if own_team == target_team {
            Reaction::Friendly as u8
        } else {
            Reaction::Hostile as u8
        },
        own_team,
        target_team,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        plate_is_friendly, ring_reaction, ring_variant, Factions, RingVariant,
        UNIT_FLAG_PVP_ATTACKABLE,
    };

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

    /// **The plate category, end to end on the REAL build-5875 DBC — the two subjects the director
    /// observed on the reference client** (1530), plus the leg that actually discriminates the
    /// mechanism.
    ///
    /// This is the regression pin the old `rank >= 4` model could never have passed. It asserts the
    /// *divergence itself*: the emissary's reaction rank stays 3 (neutral — which is right, and is
    /// what paints its bar yellow) while its plate CATEGORY is friendly. A future refactor that
    /// "simplifies" the category back into a rank threshold fails here, on real data, naming the
    /// creature.
    #[test]
    fn the_plate_category_reproduces_the_reference_on_the_real_dbc() {
        use crate::net::{ObjectStore, Reputations};
        use benilla_protocol::ObjectFields;

        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let factions = Factions(benilla_formats::load_faction_catalog(&mut chain).expect("dbc"));
        // Field indices: 35 = UNIT_FIELD_FACTIONTEMPLATE, 46 = UNIT_FIELD_FLAGS.
        let unit = |tpl: u32| ObjectStore(ObjectFields::from_pairs(&[(35, tpl)]));
        // Us: faction template 1 (PLAYER, Human), carrying `PLAYER_CONTROLLED`.
        let me = ObjectStore(ObjectFields::from_pairs(&[
            (35, 1),
            (46, UNIT_FLAG_PVP_ATTACKABLE),
        ]));
        let quiet = Reputations::default(); // nothing at war — the out-of-box state
        let category = |unit: &ObjectStore, reps: &Reputations| {
            plate_is_friendly(Some(&factions), reps, Some(unit), Some(&me), false)
        };
        let rank = |unit: &ObjectStore, reps: &Reputations| {
            ring_reaction(Some(&factions), reps, Some(unit), Some(&me))
        };

        // A Chicken (FT 31 → faction 28 "Prey", NO reputation slot) falls to the template
        // comparator, which matches nothing → 3 → attackable → the ENEMY bucket, plain V. The
        // director confirmed this on the reference: critters do plate, and nothing excludes them.
        let chicken = unit(31);
        assert!(!category(&chicken, &quiet), "a critter is enemy-category");
        assert_eq!(rank(&chicken, &quiet), 3, "and its bar is neutral yellow");

        // A League of Arathor Emissary (FT 1577 → faction 509, reputation slot 53) is answered by
        // the AT-WAR bit — not at war → 4 → not attackable → the FRIENDLY bucket, Shift-V only.
        // The director confirmed exactly this on the reference.
        let emissary = unit(1577);
        assert!(category(&emissary, &quiet), "friendly-category at neutral");
        assert_eq!(
            rank(&emissary, &quiet),
            3,
            "…while the BAR stays yellow: the two halves run the reaction in opposite \
             directions, and this disagreement is the reference's own"
        );

        // **The discriminating population** (wow-re §7: 36 shipped templates where the at-war leg
        // and the mask comparison disagree). Booty Bay's mask answer is 3 — so if this read as
        // enemy-category we would be back on the comparator, and the at-war leg would be fiction.
        let goblin = unit(120);
        assert!(category(&goblin, &quiet), "Booty Bay: friendly, not at war");
        // …and declaring war flips it, which is the same bit the reputation pane's checkbox writes.
        let mut slots = vec![(0u8, 0i32); 64];
        slots[1] = (benilla_formats::faction_flags::AT_WAR, 0); // Booty Bay = slot 1
        let at_war = Reputations(slots);
        assert!(!category(&goblin, &at_war), "at war → enemy category");
        assert!(
            category(&emissary, &at_war),
            "…and only that faction's own bit moves"
        );

        // A Stormwind guard (FT 12 → faction 72, slot 19) is friendly by BOTH routes — the case
        // that agreed all along, kept so the fix cannot be read as having moved it.
        assert!(category(&unit(12), &quiet));
        assert_eq!(rank(&unit(12), &quiet), 4);
    }

    /// `CanAttack`'s flag disqualifiers put a unit in the FRIENDLY bucket **at any reaction** — the
    /// class of case a rank threshold gets wrong in the other direction (wow-re §3b). Asserted on
    /// the mask path (a Monster-faction template, hostile by reaction) so the reaction is
    /// unambiguously hostile and only the flag can be doing the work.
    #[test]
    fn an_unattackable_flag_beats_a_hostile_reaction() {
        use crate::net::{ObjectStore, Reputations};
        use benilla_protocol::ObjectFields;

        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let factions = Factions(benilla_formats::load_faction_catalog(&mut chain).expect("dbc"));
        let me = ObjectStore(ObjectFields::from_pairs(&[
            (35, 1),
            (46, UNIT_FLAG_PVP_ATTACKABLE),
        ]));
        let reps = Reputations::default();
        let mob = |flags: u32| ObjectStore(ObjectFields::from_pairs(&[(35, 14), (46, flags)]));

        // FT 14 "Monster" — hostile to the player by the mask comparison.
        assert_eq!(
            ring_reaction(Some(&factions), &reps, Some(&mob(0)), Some(&me)),
            1
        );
        assert!(
            !plate_is_friendly(Some(&factions), &reps, Some(&mob(0)), Some(&me), false),
            "a plain hostile mob is enemy-category"
        );
        // Each of the five refusal bits alone flips the category, hostile reaction and all.
        for bit in [0x2u32, 0x80, 0x1_0000, 0x10_0000] {
            assert!(
                plate_is_friendly(Some(&factions), &reps, Some(&mob(bit)), Some(&me), false),
                "flag {bit:#x} refuses the attack → friendly category"
            );
        }
        // Bit 8 (0x100) refuses only because WE are player-controlled — the cross-flag leg.
        assert!(plate_is_friendly(
            Some(&factions),
            &reps,
            Some(&mob(0x100)),
            Some(&me),
            false
        ));
    }
}
