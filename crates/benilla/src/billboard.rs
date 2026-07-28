//! M2 billboard cards — submeshes that ride a billboard bone (glow cards, chains, the questgiver
//! markers). The real 1.12 client re-orients the bone to the camera every frame; benilla otherwise
//! renders M2 geometry in its static bind pose, single-sided.
//!
//! **The re-orientation law is byte-pinned** (wow-re `animation/scratch/billboard-bone-law.md`,
//! §5): the M2 bone palette is computed in **VIEW space**, and a billboard bone's matrix rows are
//! REPLACED with the camera basis — spherical (`0x08`) takes the whole fixed basis (bone X toward
//! the viewer, Y screen-right, Z screen-up: the identity rows `{(0,0,−1),(1,0,0),(0,1,0)}` at
//! `0x714463`); the lock arms (`0x10`/`0x20`/`0x40` = keep X/Y/Z) keep their authored axis and
//! rebuild the other two from the camera (`0x40` lock-Z — the `?` marker's `0x240` — keeps model
//! up, rebuild the in-plane pair). Crucially this is the **view-matrix basis, one shared
//! orientation for every billboard** — NOT a per-pivot aim, and NOT the geometry's facet normal
//! (the old card aimed its first-triangle normal at the camera: arbitrary for 3-D geometry like
//! the 353-vert `?`, which is exactly why its proportions read wrong). The lock-Z in-plane sign
//! is `Y = Fwd × Z` — the 0168 handedness residual, settled by the director's A/B (the recorded
//! `Z × Fwd` order turned the model 180°: a mirrored `?`); it makes lock-Z agree with the
//! spherical arm's toward-the-viewer X at a level camera.
//!
//! The submesh mesh is built **centred at its bone pivot** (`benilla_assets::build_submesh_mesh`)
//! in the model-local Bevy frame — where the WoW bone axes land as X→−Z, Y→−X, Z→+Y (coords.rs) —
//! so we place the entity at the pivot's world position and write the rebuilt basis as its
//! rotation each frame; the geometry itself is never touched.

use benilla_assets::BillboardInfo;
use benilla_formats::{BillboardKind, BoneScaleAnim};
use bevy::mesh::MeshTag;
use bevy::prelude::*;

use crate::player::WorldCamera;

/// A spawned billboard card: where its pivot sits in the world, the uniform placement scale, how
/// it tracks the camera (the bone-flag arm), and its optional global-sequence scale pulse. The
/// per-frame system rewrites the entity transform from these — including `Visibility` (the
/// hidden-owner mirror), so a card requires it rather than trusting every spawn site's `Mesh3d`
/// to bring it along.
///
/// It requires a [`MeshTag`] for the same reason: a card is a world ROOT, so every per-model alpha
/// that reaches an ordinary submesh by descending the model's tree has to reach a card through its
/// own tag instead (`player::apply_self_model_fade` — the zoom-to-first-person feather). Only the
/// alpha-animated spawn sites used to bring one, which left the channel *incidentally* present;
/// the default `MeshTag(0)` is the shader's untagged-⇒-opaque sentinel, so requiring it changes
/// nothing about how a card draws.
#[derive(Component)]
#[require(Transform, Visibility, MeshTag)]
pub struct BillboardCard {
    world_pivot: Vec3,
    scale: f32,
    kind: BillboardKind,
    /// The billboard bone's looping scale animation (the lamppost glow "breathe"), sampled each frame
    /// and multiplied into [`Self::scale`]. `None` for a static card (no global-sequence scale track).
    scale_anim: Option<BoneScaleAnim>,
    /// Per-instance phase offset (ms) into the global sequence, hashed from the world position — so a
    /// row of identical lampposts breathes out of lockstep (each prop instance arms its own clock) rather
    /// than blinking in unison. No-op when there's no `scale_anim`.
    phase_ms: u32,
    /// The bone's armed first-sequence **translation** loop (the questgiver `?` marker's bob, keys in
    /// Bevy axes) — sampled each frame on the same clock/phase and added at the pivot, rotated by
    /// [`Self::placement_rot`]. `None` (every doodad card today) = the static pivot; only the marker
    /// spawn site arms it via [`Self::with_seq_translation`] — the doodad half of that ride belongs
    /// to the 0130 phase-4 bone-follow work.
    seq_translation: Option<BoneScaleAnim>,
    /// The placement's rotation, so the bob offset (model-local) points where the instance points.
    placement_rot: Quat,
    /// The entity this card FOLLOWS (a unit/GameObject anchor or held-item root): the facing system
    /// re-seats the card from its live `GlobalTransform` every frame and despawns the card when it
    /// goes — the ONE mechanism for every non-doodad spawn path (braziers, held torches, missiles),
    /// so a glow card can never again render at the model origin because a spawn site forgot the
    /// pivot (the recurring "glow on the ground" family — decision 0153). `None` = fixed placement
    /// (terrain doodads, whose transform never moves).
    follow: Option<Entity>,
    /// The pivot in the model's local Bevy frame (re-applied each frame when `follow` is set).
    local_pivot: Vec3,
}

impl BillboardCard {
    /// Build a card from a submesh's [`BillboardInfo`] and its instance `placement`. The pivot is placed
    /// in the world; the card's orientation ignores the placement rotation (a billboard faces the camera
    /// regardless of how the prop is turned).
    pub fn new(info: &BillboardInfo, placement: Transform) -> Self {
        let world_pivot = placement.transform_point(info.pivot);
        // A stable per-instance phase from the world position (same hashing as the particle RNG seed).
        let phase_ms = world_pivot.x.to_bits().wrapping_mul(0x9E37_79B9)
            ^ world_pivot.y.to_bits().rotate_left(11)
            ^ world_pivot.z.to_bits().rotate_left(22);
        Self {
            world_pivot,
            scale: placement.scale.x,
            kind: info.kind,
            scale_anim: info.scale_anim.clone(),
            phase_ms,
            seq_translation: None,
            placement_rot: placement.rotation,
            follow: None,
            local_pivot: info.pivot,
        }
    }

    /// Build a card that FOLLOWS `owner` — the entity-path form (creatures, GameObjects, held
    /// items, missiles, spell effects): world pivot/scale/rotation are re-derived from the owner's
    /// live `GlobalTransform` every frame, and the card despawns when the owner goes.
    pub fn following(info: &BillboardInfo, owner: Entity) -> Self {
        let mut card = Self::new(info, Transform::IDENTITY);
        card.follow = Some(owner);
        card
    }

    /// Build a card riding a live JOINT — an animated host's billboard bone (the swinging lamp,
    /// the mount's lights). The joint's frame already bakes the bone pivot (the 0130 rig identity
    /// `joint = root · M_bone · T(pivot)`), so the card's local pivot is the joint origin.
    pub fn following_joint(info: &BillboardInfo, joint: Entity) -> Self {
        let mut card = Self::following(info, joint);
        card.local_pivot = Vec3::ZERO;
        card
    }

    /// Arm the card's first-sequence translation loop (the questgiver `?` bob) with the client's
    /// arm-time cursor: sampling runs on `elapsed − arm_ms` (the loop starts at its first key the
    /// moment the marker attaches, like the real arm at status receive). Overrides the position-hash
    /// phase — markers of one NPC swap in place and must not inherit a doodad-flavored phase.
    pub(crate) fn with_seq_translation(mut self, anim: Option<BoneScaleAnim>, arm_ms: u32) -> Self {
        self.arm_seq_translation(anim, arm_ms);
        self
    }

    /// Re-arm the translation loop on a LIVE card — the marker swapping between its low (anim 0)
    /// and raised (anim 190) bob when the unit's overhead name toggles: fresh cursor, same law as
    /// [`Self::with_seq_translation`].
    pub(crate) fn arm_seq_translation(&mut self, anim: Option<BoneScaleAnim>, arm_ms: u32) {
        if anim.is_some() {
            self.phase_ms = arm_ms.wrapping_neg();
        }
        self.seq_translation = anim;
    }

    /// The entity this card follows, if any — the anchor/joint that decides both where it sits and
    /// which model it BELONGS to. A card is a world root, so a system that walks a model's tree
    /// (the self-avatar fade) can only recognise the model's own cards by testing this against the
    /// entities it walked.
    pub(crate) fn follows(&self) -> Option<Entity> {
        self.follow
    }

    /// Re-seat a card that FOLLOWS something (the questgiver `!`/`?` markers over a unit that can
    /// move) — recompute the world pivot/scale/rotation from a fresh `placement`, keeping the card's
    /// orientation kind, rest normal, and animation phase. Doodad cards never need this (their
    /// placement is fixed at spawn).
    pub(crate) fn re_place(&mut self, placement: Transform, local_pivot: Vec3) {
        self.world_pivot = placement.transform_point(local_pivot);
        self.scale = placement.scale.x;
        self.placement_rot = placement.rotation;
    }
}

/// The rebuilt orientation for a billboard of `kind` — the byte law (module doc), one function
/// for both consumers: the CARD path (`kept_rot` = the placement/owner rotation) and the JOINT
/// palette pass below (`kept_rot` = the joint's fully-composed pre-billboard world rotation, the
/// law's `normalize(rK)`). `bx/by/bz` are the bone's WoW-frame X/Y/Z axes as world directions
/// after the replacement; the returned quat maps the mesh's model-local Bevy frame onto them
/// (WoW axes sit in that frame as X→−Z, Y→−X, Z→+Y — coords.rs — so local X→−by, Y→bz, Z→−bx).
pub(crate) fn billboard_basis(
    kind: BillboardKind,
    kept_rot: Quat,
    fwd: Vec3,
    right: Vec3,
    up: Vec3,
) -> Quat {
    let (bx, by, bz) = match kind {
        // Spherical (`0x08`): the whole fixed basis — X toward the viewer, Y screen-right,
        // Z screen-up (the view-space identity rows).
        BillboardKind::Spherical => (-fwd, right, up),
        // Lock-Z (`0x40` — the `?` marker, the frost-armor sheets): keep the authored bone Z
        // (model up, pointed by `kept_rot`), rebuild the in-plane pair from the camera. The
        // in-plane sign is `Y = Fwd × Z` — the 0168 residual, settled by the director's A/B
        // (the other order showed the model's back: a mirrored `?`); this order also agrees
        // with the spherical arm at a level camera (X toward the viewer, Y screen-right), the
        // coherence the flipped version lacked. A camera looking straight along the kept axis
        // degenerates the cross — hold screen-right then.
        BillboardKind::LockZ => {
            let bz = (kept_rot * Vec3::Y).normalize_or(Vec3::Y);
            let by = fwd.cross(bz).try_normalize().unwrap_or(right);
            let bx = by.cross(bz);
            (bx, by, bz)
        }
        // Lock-X/-Y: the same verified structure generalized per kept axis — the cyclically
        // PREVIOUS axis takes `Fwd × kept` (that assignment is what reproduces the settled
        // lock-Z arm), the third completes the right-handed WoW triple. No shipped content has
        // A/B'd these two arms yet; if a chain/rope ever reads mirrored, the sign here is the
        // one knob (0168's pattern).
        BillboardKind::LockX => {
            let bx = (kept_rot * -Vec3::Z).normalize_or(-fwd);
            let bz = fwd.cross(bx).try_normalize().unwrap_or(up);
            let by = bz.cross(bx);
            (bx, by, bz)
        }
        BillboardKind::LockY => {
            let by = (kept_rot * -Vec3::X).normalize_or(right);
            let bx = fwd.cross(by).try_normalize().unwrap_or(-fwd);
            let bz = bx.cross(by);
            (bx, by, bz)
        }
    };
    Quat::from_mat3(&Mat3::from_cols(-by, bz, -bx))
}

/// A rigged host whose skeleton authors billboard bones (component beside the rig's
/// `AnimationPlayer`): the joint entities in bone order, each bone's parent, and which joints
/// billboard. [`billboard_joint_palette`] rewrites those joints' propagated world rotations to
/// the camera basis every frame — the byte law operates on the BONE PALETTE
/// (`finalBoneWorld … children multiply onto this`, wow-re `billboard-bone-law.md`), so geometry
/// skinned to a billboard bone's CHILDREN inherits the facing. The per-batch card split can
/// never catch that case: the frost-armor sheets skin every vertex to the scale-in CHILD of the
/// lock-Z bone, which is exactly why they rendered glued to the character.
#[derive(Component)]
pub struct BillboardJointRig {
    /// The host root entity — the frame an `ignore_parent_rotation` joint's rotation snaps back
    /// to (bone flag `0x04` keeps the MODEL's orientation, not the parent bone's).
    root: Entity,
    joints: Vec<Entity>,
    parents: Vec<i16>,
    kinds: Vec<Option<BillboardKind>>,
    /// Bone flag `0x04` per joint (the HandArrow/Bullet attach helpers): pivot rides the parent's
    /// full matrix, rotation resets to the model root's — the nocked arrow lies flat along the
    /// facing instead of twisting with the draw hand (wow-re `nocked-ammo-cancel.md` §E4).
    ignore_rot: Vec<bool>,
}

impl BillboardJointRig {
    /// The host root — the collapsed-rig world pass's do-not-enter set reads it (a nested rig
    /// with its own billboard output owns its interior, whichever lane the outer rig is on).
    pub(crate) fn root(&self) -> Entity {
        self.root
    }

    /// Build for a spawned rig — `None` when the skeleton authors no billboard bone and no
    /// ignore-parent-rotation bone (the common case: ordinary rigs cost nothing). `root` is the
    /// host entity the joints hang under (the model-space frame).
    pub fn new(
        skeleton: &benilla_assets::ModelSkeleton,
        joints: &[Entity],
        root: Entity,
    ) -> Option<Self> {
        if skeleton
            .joints
            .iter()
            .all(|j| j.billboard.is_none() && !j.ignore_parent_rotation)
        {
            return None;
        }
        Some(Self {
            root,
            joints: joints.to_vec(),
            parents: skeleton.joints.iter().map(|j| j.parent).collect(),
            kinds: skeleton.joints.iter().map(|j| j.billboard).collect(),
            ignore_rot: skeleton
                .joints
                .iter()
                .map(|j| j.ignore_parent_rotation)
                .collect(),
        })
    }
}

/// The palette half of the billboard law: for each rigged host, replace every billboard joint's
/// world rotation with the camera basis (scale and pivot translation preserved — the law's
/// `lenK`/`finalTranslation`, which in our rig identity is simply "keep the joint's global
/// scale/translation"), then re-compose every descendant joint from its local TRS so skinned
/// geometry — and emitters/ribbons riding those joints — inherit the facing. Runs after
/// propagation and writes `GlobalTransform` directly (the same exactness argument as
/// [`face_billboards`], which must run after this so following-joint cards read the replaced
/// frames). **Every palette consumer must read AFTER this system, same frame**: avian's physics
/// sync re-propagates the hierarchy from locals inside the fixed loop, so an Update-time read
/// gets the UN-billboarded pose — the Demon Skin flames followed the character's yaw instead of
/// the camera until the particle/ribbon sims moved behind this pass. Bone order is parent-sorted
/// in every real M2 (the format guarantees parent < child); a malformed child whose parent
/// follows it just keeps its propagated pose.
///
/// A rigged model can hang under ANOTHER rig's joint — a spell-effect instance on a unit's
/// attach-helper bone, a rigged held item in a hand. One ownership law keeps the passes from
/// fighting over those frames: the child-recompose walk **never enters a nested rig's subtree**
/// — not even its root, whose propagated global (the live ANIMATED attach-bone frame) is what
/// its emitters' attach frame must read. Without it, the boar's flag-0x04 attach helper
/// re-composed the Eviscerate impact model's frames from raw locals, erasing its camera-born
/// billboard basis or its animated attach rotation depending on per-launch query order — the
/// burst rendered as a body-locked pillar on some launches and correctly on others. With no rig
/// ever writing into another rig's subtree, the passes are order-independent again.
pub(crate) fn billboard_joint_palette(
    cam: Query<&GlobalTransform, With<WorldCamera>>,
    hosts: Query<&BillboardJointRig>,
    // A parked unit's pose is frozen off-frustum (decision 0448) — camera-facing its glow joints
    // would re-dirty the subtree for a rig no one sees. A parked host still sits in the
    // do-not-enter set (its propagated frames are real); it just isn't re-faced.
    parked: Query<Has<crate::creature_anim::AnimParked>>,
    mut joints: Query<(&Transform, &mut GlobalTransform), Without<WorldCamera>>,
    children: Query<&Children>,
) {
    let Ok(cam_tf) = cam.single() else {
        return;
    };
    let (fwd, right, up) = (*cam_tf.forward(), *cam_tf.right(), *cam_tf.up());
    // Every rig root — the walk's do-not-enter set.
    let rig_roots: bevy::platform::collections::HashSet<Entity> =
        hosts.iter().map(|r| r.root).collect();
    for rig in hosts
        .iter()
        .filter(|r| !parked.get(r.root).unwrap_or(false))
    {
        // The model-space frame an ignore-parent-rotation joint (bone flag 0x04) snaps back to:
        // the host root's world rotation. Read before the joint loop (the root is never a joint).
        let root_rot = joints
            .get(rig.root)
            .map(|(_, g)| g.rotation())
            .unwrap_or_default();
        let n = rig.joints.len();
        let mut replaced: Vec<Option<GlobalTransform>> = vec![None; n];
        for i in 0..n {
            let parent_new = usize::try_from(rig.parents[i])
                .ok()
                .filter(|&p| p < i)
                .and_then(|p| replaced[p]);
            if parent_new.is_none() && rig.kinds[i].is_none() && !rig.ignore_rot[i] {
                continue; // untouched subtree — the propagated pose stands
            }
            let Ok((local, mut global)) = joints.get_mut(rig.joints[i]) else {
                continue;
            };
            let mut g = match parent_new {
                Some(pg) => pg.mul_transform(*local),
                None => *global,
            };
            if rig.ignore_rot[i] {
                // Bone flag 0x04: keep the parent-composed pivot (the hand carries the point),
                // reset the rotation to the model root's frame — children (the nocked arrow)
                // inherit the flat model-space orientation, not the hand twist.
                let (scale, _, translation) = g.to_scale_rotation_translation();
                g = GlobalTransform::from(Transform {
                    translation,
                    rotation: root_rot,
                    scale,
                });
            } else if let Some(kind) = rig.kinds[i] {
                let (scale, rot, translation) = g.to_scale_rotation_translation();
                g = GlobalTransform::from(Transform {
                    translation,
                    rotation: billboard_basis(kind, rot, fwd, right, up),
                    scale,
                });
            }
            replaced[i] = Some(g);
            *global = g;
        }
        // Rigid children hanging under a rewritten joint (a held item, the nocked arrow) got
        // their globals from ordinary propagation — BEFORE this rewrite. Re-compose those
        // subtrees from the replaced frames; sibling JOINTS are excluded (the replaced-chain
        // above owns them). Skinned geometry never needs this — it reads the joint frames.
        let joint_set: bevy::platform::collections::HashSet<Entity> =
            rig.joints.iter().copied().collect();
        let mut stack: Vec<(Entity, GlobalTransform)> = Vec::new();
        for (&joint, g) in rig
            .joints
            .iter()
            .zip(&replaced)
            .filter_map(|(j, r)| r.map(|g| (j, g)))
        {
            if let Ok(cs) = children.get(joint) {
                stack.extend(
                    cs.iter()
                        .filter(|c| !joint_set.contains(c) && !rig_roots.contains(c))
                        .map(|c| (c, g)),
                );
            }
        }
        while let Some((e, parent_g)) = stack.pop() {
            let Ok((local, mut global)) = joints.get_mut(e) else {
                continue;
            };
            let g = parent_g.mul_transform(*local);
            *global = g;
            if let Ok(cs) = children.get(e) {
                stack.extend(cs.iter().filter(|c| !rig_roots.contains(c)).map(|c| (c, g)));
            }
        }
    }
}

/// The billboard placement pass (PostUpdate, after `TransformSystems::Propagate`, before
/// visibility) — [`billboard_joint_palette`] then [`face_billboards`] run here, and upstream
/// card re-seaters (the quest markers) order `.before` it. Placement reads the SAME-frame
/// propagated pose: running in Update read last-frame joint/owner globals, so a card over a
/// moving unit trailed a frame behind and snapped forward on stop (the nameplate lag's sibling).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BillboardPlace;

/// Per-frame: face each billboard card to the camera (around its pivot) and apply its scale pulse.
/// A FOLLOWING card (entity-path) is first re-seated from its owner's live global transform — and
/// despawned when the owner is gone (streamed out, unequipped, died). Runs in [`BillboardPlace`]
/// (post-propagation), so it writes `GlobalTransform` directly alongside `Transform` — cards
/// write ABSOLUTE world transforms and live at the root/identity, so the direct write is exact.
#[allow(clippy::type_complexity)] // the owner pose + visibility read, commented inline
fn face_billboards(
    mut commands: Commands,
    time: Res<Time>,
    cam: Query<&GlobalTransform, With<WorldCamera>>,
    owners: Query<
        (&GlobalTransform, Option<&InheritedVisibility>),
        (Without<WorldCamera>, Without<BillboardCard>),
    >,
    mut cards: Query<
        (
            Entity,
            &mut BillboardCard,
            &mut Transform,
            &mut GlobalTransform,
            &mut Visibility,
        ),
        Without<WorldCamera>,
    >,
) {
    let Ok(cam_tf) = cam.single() else {
        return;
    };
    // The camera basis — the VIEW-MATRIX axes the byte law substitutes (one shared orientation
    // for every billboard; never a per-pivot aim).
    let (fwd, right, up) = (*cam_tf.forward(), *cam_tf.right(), *cam_tf.up());
    let elapsed_ms = time.elapsed().as_millis() as u32;
    for (entity, mut card, mut tf, mut global, mut visibility) in &mut cards {
        if let Some(owner) = card.follow {
            match owners.get(owner) {
                Ok((gt, vis)) => {
                    let pivot = card.local_pivot;
                    card.re_place(gt.compute_transform(), pivot);
                    // A card is visually part of its owner — mirror a HIDDEN owner, because the
                    // card is a world root and inherits nothing. The live case: the sea-crossing
                    // transport's off-map leg hides the boat subtree (`tick_transports`); without
                    // the mirror a deck lantern's glow keeps rendering at the other continent's
                    // coordinates. (The owner's inherited visibility is last propagate's — one
                    // frame of lag on a minutes-long hide.)
                    let want = match vis {
                        Some(v) if !v.get() => Visibility::Hidden,
                        _ => Visibility::Inherited,
                    };
                    if *visibility != want {
                        *visibility = want;
                    }
                }
                Err(_) => {
                    commands.entity(entity).despawn();
                    continue;
                }
            }
        }
        let card = &*card;
        let rotation = billboard_basis(card.kind, card.placement_rot, fwd, right, up);
        // The bone's global-sequence scale pulse (the lamppost glow "breathe"), sampled at this prop's
        // own phase into the loop. `Vec3::ONE` (no-op) when the card has no scale track.
        let pulse = card.scale_anim.as_ref().map_or(Vec3::ONE, |a| {
            Vec3::from_array(a.sample(elapsed_ms.wrapping_add(card.phase_ms)))
        });
        // The armed first-sequence translation loop (the questgiver `?` bob): a model-local offset
        // at the pivot, pointed by the placement rotation and sized by its scale.
        let bob = card.seq_translation.as_ref().map_or(Vec3::ZERO, |a| {
            card.placement_rot
                * (Vec3::from_array(a.sample(elapsed_ms.wrapping_add(card.phase_ms))) * card.scale)
        });
        *tf = Transform {
            translation: card.world_pivot + bob,
            rotation,
            scale: Vec3::splat(card.scale) * pulse,
        };
        // Propagation already ran this frame — the direct global write is what renders.
        *global = GlobalTransform::from(*tf);
    }
}

/// Registers the billboard placement pass ([`BillboardPlace`], PostUpdate post-propagation).
/// Cards are spawned by the model spawn sites (in Update — mesh churn stays there).
pub struct BillboardPlugin;

impl Plugin for BillboardPlugin {
    fn build(&self, app: &mut App) {
        // The set carries the schedule constraints so every member — including the
        // particle/ribbon sims other plugins add — lands post-propagation, pre-visibility.
        app.configure_sets(
            PostUpdate,
            BillboardPlace
                .after(bevy::transform::TransformSystems::Propagate)
                .before(bevy::camera::visibility::VisibilitySystems::CheckVisibility),
        )
        .add_systems(
            PostUpdate,
            (
                billboard_joint_palette,
                // The collapsed-rig world pass (decision 0724): palette rows + replaced-subtree
                // anchor re-seats, between the entity lane's joint rewrite and the card facing
                // (cards following a unit's billboard-bone anchor read the replaced frame).
                crate::creature_anim::finalize_rig_worlds,
                face_billboards,
            )
                .chain()
                .in_set(BillboardPlace),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_assets::BillboardInfo;

    /// A FOLLOWING card (decision 0153 — the entity-path glow cards) re-seats from its owner's
    /// live global transform each frame and despawns with it: the brazier glow burns at the bowl
    /// (owner translation + authored pivot), never the model origin — and dies when the owner
    /// streams out / unequips.
    #[test]
    fn following_card_rides_its_owner_and_dies_with_it() {
        let mut app = App::new();
        app.init_resource::<Time>();
        app.add_systems(Update, face_billboards);
        app.world_mut().spawn((
            crate::player::WorldCamera,
            GlobalTransform::from_translation(Vec3::new(0.0, 0.0, 10.0)),
        ));
        let owner = app
            .world_mut()
            .spawn(GlobalTransform::from_translation(Vec3::new(5.0, 0.0, 0.0)))
            .id();
        let info = BillboardInfo {
            bone: 0,
            pivot: Vec3::new(0.0, 1.7, 0.0), // the brazier-bowl height, model-local Bevy frame
            normal: Vec3::Z,
            kind: BillboardKind::Spherical,
            scale_anim: None,
            seq_translations: vec![],
        };
        let card = app
            .world_mut()
            .spawn((BillboardCard::following(&info, owner), Transform::IDENTITY))
            .id();
        app.update();
        let tf = app.world().entity(card).get::<Transform>().unwrap();
        assert_eq!(
            tf.translation,
            Vec3::new(5.0, 1.7, 0.0),
            "owner translation + authored pivot — not the model origin"
        );
        // The hidden-owner mirror: the card is a world root and inherits nothing, so a hidden
        // owner (the sea-crossing transport's off-map leg) must hide it explicitly — and a
        // re-shown owner must bring it back.
        app.world_mut()
            .entity_mut(owner)
            .insert(InheritedVisibility::HIDDEN);
        app.update();
        assert_eq!(
            *app.world().entity(card).get::<Visibility>().unwrap(),
            Visibility::Hidden,
            "a hidden owner hides its card"
        );
        app.world_mut()
            .entity_mut(owner)
            .insert(InheritedVisibility::VISIBLE);
        app.update();
        assert_eq!(
            *app.world().entity(card).get::<Visibility>().unwrap(),
            Visibility::Inherited,
            "a re-shown owner restores its card"
        );
        app.world_mut().entity_mut(owner).despawn();
        app.update();
        assert!(
            app.world().get_entity(card).is_err(),
            "card despawns with its owner"
        );
    }

    /// The palette pass: a lock-Z billboard JOINT gets its propagated world rotation replaced by
    /// the camera basis (translation/scale kept — the pivot stays put, the grow-in scale
    /// survives), and its CHILD joint is re-composed from the replaced parent — so geometry
    /// skinned to the child inherits the facing (the frost-armor case). The host's own yaw must
    /// not leak into the result: two hosts facing opposite ways produce the SAME billboarded
    /// orientation for an upright lock-Z bone.
    #[test]
    fn palette_pass_faces_joints_and_recomposes_children() {
        let mut app = App::new();
        app.add_systems(Update, billboard_joint_palette);
        app.world_mut().spawn((
            crate::player::WorldCamera,
            // Looking along −Z from +Z, world-up Y — the identity camera frame.
            GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, 10.0))),
        ));
        let mut spawn_host = |yaw: f32| {
            let host_rot = Quat::from_rotation_y(yaw);
            // Joint 0: lock-Z billboard at the host's frame, world pivot (5, 1, 0), scale 2.
            let j0_global = GlobalTransform::from(Transform {
                translation: Vec3::new(5.0, 1.0, 0.0),
                rotation: host_rot,
                scale: Vec3::splat(2.0),
            });
            let j0 = app.world_mut().spawn((Transform::IDENTITY, j0_global)).id();
            // Joint 1: the scale-in child, one unit up its parent's Y, half scale.
            let j1_local = Transform::from_translation(Vec3::Y).with_scale(Vec3::splat(0.5));
            let j1 = app
                .world_mut()
                .spawn((j1_local, j0_global.mul_transform(j1_local)))
                .id();
            let skeleton = benilla_assets::ModelSkeleton {
                joints: vec![
                    benilla_assets::ModelJoint {
                        parent: -1,
                        local_translation: Vec3::ZERO,
                        billboard: Some(BillboardKind::LockZ),
                        ignore_parent_rotation: false,
                    },
                    benilla_assets::ModelJoint {
                        parent: 0,
                        local_translation: Vec3::Y,
                        billboard: None,
                        ignore_parent_rotation: false,
                    },
                ],
                spine_bone: None,
                head_bone: None,
            };
            let host = app
                .world_mut()
                .spawn((Transform::IDENTITY, GlobalTransform::IDENTITY))
                .id();
            let rig =
                BillboardJointRig::new(&skeleton, &[j0, j1], host).expect("has a billboard bone");
            app.world_mut().spawn(rig);
            (j0, j1)
        };
        let (a0, a1) = spawn_host(0.0);
        let (b0, _) = spawn_host(std::f32::consts::PI); // faces the other way
        app.update();
        let g0 = *app.world().entity(a0).get::<GlobalTransform>().unwrap();
        let (s0, r0, t0) = g0.to_scale_rotation_translation();
        assert_eq!(t0, Vec3::new(5.0, 1.0, 0.0), "the pivot stays put");
        assert!((s0 - Vec3::splat(2.0)).length() < 1e-5, "scale preserved");
        // Lock-Z at this camera: kept axis = world up; the replaced frame is exactly the
        // camera-agreeing basis — local +Y stays up, local −Z faces the viewer.
        assert!((r0 * Vec3::Y).dot(Vec3::Y) > 0.999, "kept axis upright");
        assert!((r0 * -Vec3::Z).dot(Vec3::Z) > 0.999, "faces the camera");
        // The opposite-facing host lands on the SAME orientation — char yaw does not leak.
        let (_, rb, _) = app
            .world()
            .entity(b0)
            .get::<GlobalTransform>()
            .unwrap()
            .to_scale_rotation_translation();
        assert!(
            rb.angle_between(r0) < 1e-4,
            "host yaw must not change the facing"
        );
        // The child re-composed onto the replaced parent: parent's new Y is world Y, so the
        // child sits one PARENT-scaled unit above the pivot, with composed scale 2·0.5 = 1.
        let (s1, _, t1) = app
            .world()
            .entity(a1)
            .get::<GlobalTransform>()
            .unwrap()
            .to_scale_rotation_translation();
        assert!(
            (t1 - Vec3::new(5.0, 3.0, 0.0)).length() < 1e-4,
            "child rides the new frame"
        );
        assert!(
            (s1 - Vec3::ONE).length() < 1e-5,
            "the grow-in scale chain survives"
        );
    }

    /// The ignore-parent-rotation joint (bone flag 0x04 — the HandArrow/Bullet attach helpers,
    /// wow-re `nocked-ammo-cancel.md` §E4): its pivot rides the parent's full matrix, its
    /// ROTATION resets to the host root's frame — and a rigid child (the nocked arrow) hanging
    /// under it re-composes onto the replaced frame instead of keeping the twisted propagated one.
    #[test]
    fn ignore_parent_rotation_joint_keeps_the_model_frame() {
        let mut app = App::new();
        app.add_systems(Update, billboard_joint_palette);
        app.world_mut().spawn((
            crate::player::WorldCamera,
            GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, 10.0))),
        ));
        // The host root: yawed 90° — the model frame every flag-0x04 joint must land on.
        let host_rot = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let host = app
            .world_mut()
            .spawn((
                Transform::IDENTITY,
                GlobalTransform::from(Transform::from_rotation(host_rot)),
            ))
            .id();
        // Joint 0: the animated hand — twisted a further 90° about X (the draw-hand roll the
        // arrow must NOT inherit), pivot at (1, 2, 3).
        let hand_rot = host_rot * Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
        let j0_global = GlobalTransform::from(Transform {
            translation: Vec3::new(1.0, 2.0, 3.0),
            rotation: hand_rot,
            scale: Vec3::ONE,
        });
        let j0 = app.world_mut().spawn((Transform::IDENTITY, j0_global)).id();
        // Joint 1: the flag-0x04 attach helper, one local unit up the HAND's frame.
        let j1_local = Transform::from_translation(Vec3::Y);
        let j1 = app
            .world_mut()
            .spawn((j1_local, j0_global.mul_transform(j1_local)))
            .id();
        // The rigid arrow child under the helper, at a local offset — propagated PRE-pass with
        // the twisted frame (what the bug rendered).
        let arrow_local = Transform::from_translation(Vec3::X);
        let arrow = app
            .world_mut()
            .spawn((
                arrow_local,
                j0_global.mul_transform(j1_local).mul_transform(arrow_local),
            ))
            .id();
        app.world_mut().entity_mut(j1).add_child(arrow);
        let skeleton = benilla_assets::ModelSkeleton {
            joints: vec![
                benilla_assets::ModelJoint {
                    parent: -1,
                    local_translation: Vec3::ZERO,
                    billboard: None,
                    ignore_parent_rotation: false,
                },
                benilla_assets::ModelJoint {
                    parent: 0,
                    local_translation: Vec3::Y,
                    billboard: None,
                    ignore_parent_rotation: true,
                },
            ],
            spine_bone: None,
            head_bone: None,
        };
        let rig = BillboardJointRig::new(&skeleton, &[j0, j1], host)
            .expect("has an ignore-parent-rotation bone");
        app.world_mut().spawn(rig);
        app.update();

        // The helper joint: pivot carried by the HAND's frame (hand rot · Y above the hand),
        // rotation snapped back to the HOST's.
        let (_, r1, t1) = app
            .world()
            .entity(j1)
            .get::<GlobalTransform>()
            .unwrap()
            .to_scale_rotation_translation();
        let expected_pivot = Vec3::new(1.0, 2.0, 3.0) + hand_rot * Vec3::Y;
        assert!(
            (t1 - expected_pivot).length() < 1e-5,
            "the pivot rides the parent's full matrix"
        );
        assert!(
            r1.angle_between(host_rot) < 1e-3,
            "the rotation resets to the model root's frame"
        );
        // The arrow child re-composed onto the replaced frame: host-frame X off the pivot.
        let (_, ra, ta) = app
            .world()
            .entity(arrow)
            .get::<GlobalTransform>()
            .unwrap()
            .to_scale_rotation_translation();
        assert!(
            (ta - (expected_pivot + host_rot * Vec3::X)).length() < 1e-5,
            "the rigid child rides the replaced frame"
        );
        assert!(
            ra.angle_between(host_rot) < 1e-3,
            "the child inherits the flat model-space orientation"
        );
    }

    /// A rigged model nested under another rig's rewritten joint (the Eviscerate impact instance
    /// on the boar's flag-0x04 attach helper): the outer rig's child walk must not enter the
    /// nested rig's subtree AT ALL — the root keeps its propagated global (the live animated
    /// attach-bone frame its emitters' attach rotation reads), and the interior belongs to the
    /// nested rig's own pass. Both spawn orders must land on the identical result; pre-fix,
    /// whichever rig iterated last won, so the effect's camera-born billboard frame (and its
    /// animated attach frame) survived or died per launch.
    #[test]
    fn nested_rig_interior_is_owned_by_its_own_pass() {
        for nested_first in [false, true] {
            let mut app = App::new();
            app.add_systems(Update, billboard_joint_palette);
            app.world_mut().spawn((
                crate::player::WorldCamera,
                GlobalTransform::from(Transform::from_translation(Vec3::new(0.0, 0.0, 10.0))),
            ));
            // The outer host (a boar): yawed 90°, one flag-0x04 attach-helper joint whose
            // propagated global carries an animated twist the reset must erase.
            let host_rot = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
            let host = app
                .world_mut()
                .spawn((
                    Transform::IDENTITY,
                    GlobalTransform::from(Transform::from_rotation(host_rot)),
                ))
                .id();
            let j0_global = GlobalTransform::from(Transform {
                translation: Vec3::new(1.0, 2.0, 3.0),
                rotation: host_rot * Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                scale: Vec3::ONE,
            });
            let j0 = app.world_mut().spawn((Transform::IDENTITY, j0_global)).id();
            let outer_skeleton = benilla_assets::ModelSkeleton {
                joints: vec![benilla_assets::ModelJoint {
                    parent: -1,
                    local_translation: Vec3::ZERO,
                    billboard: None,
                    ignore_parent_rotation: true,
                }],
                spine_bone: None,
                head_bone: None,
            };
            // The nested effect instance: its root hangs one local X under the helper, and its
            // single joint is a lock-Z billboard whose propagated global still carries the
            // (wrong) host twist — its own pass must replace it, and keep it replaced.
            let fx_local = Transform::from_translation(Vec3::X);
            let fx_root = app
                .world_mut()
                .spawn((fx_local, j0_global.mul_transform(fx_local)))
                .id();
            app.world_mut().entity_mut(j0).add_child(fx_root);
            let fj0 = app
                .world_mut()
                .spawn((Transform::IDENTITY, j0_global.mul_transform(fx_local)))
                .id();
            app.world_mut().entity_mut(fx_root).add_child(fj0);
            let nested_skeleton = benilla_assets::ModelSkeleton {
                joints: vec![benilla_assets::ModelJoint {
                    parent: -1,
                    local_translation: Vec3::ZERO,
                    billboard: Some(BillboardKind::Spherical),
                    ignore_parent_rotation: false,
                }],
                spine_bone: None,
                head_bone: None,
            };
            let outer_rig = BillboardJointRig::new(&outer_skeleton, &[j0], host).unwrap();
            let nested_rig = BillboardJointRig::new(&nested_skeleton, &[fj0], fx_root).unwrap();
            if nested_first {
                app.world_mut().spawn(nested_rig);
                app.world_mut().spawn(outer_rig);
            } else {
                app.world_mut().spawn(outer_rig);
                app.world_mut().spawn(nested_rig);
            }
            app.update();

            // The nested root keeps its PROPAGATED global — the animated attach-bone frame
            // (with the hand twist): the walk never entered the nested subtree.
            let expected = j0_global.mul_transform(fx_local);
            let (_, rr, rt) = app
                .world()
                .entity(fx_root)
                .get::<GlobalTransform>()
                .unwrap()
                .to_scale_rotation_translation();
            let (_, er, et) = expected.to_scale_rotation_translation();
            assert!(
                (rt - et).length() < 1e-5,
                "nested root keeps its propagated seat (nested_first={nested_first})"
            );
            assert!(
                rr.angle_between(er) < 1e-3,
                "nested root keeps the animated attach rotation (nested_first={nested_first})"
            );
            // The nested rig's own billboard frame SURVIVES, in either spawn order: the outer
            // walk stopped at the nested root instead of re-composing fj0 from its raw local.
            let (_, rj, _) = app
                .world()
                .entity(fj0)
                .get::<GlobalTransform>()
                .unwrap()
                .to_scale_rotation_translation();
            // The spherical basis at this camera, through the WoW→Bevy axis fold
            // (`billboard_basis`'s `from_cols(-by, bz, -bx)`): Bevy-local −Z toward the viewer
            // (WoW X), Bevy-local +Y screen-up (WoW Z) — the (π,0) ray ring's camera-born plane
            // (wow-re part-billboard-ring-emulated.md).
            assert!(
                (rj * -Vec3::Z).dot(Vec3::Z) > 0.999,
                "the nested billboard faces the camera (nested_first={nested_first})"
            );
            assert!(
                (rj * Vec3::Y).dot(Vec3::Y) > 0.999,
                "the nested billboard's screen-up axis holds (nested_first={nested_first})"
            );
        }
    }

    /// An armed first-sequence translation loop (the questgiver `?` bob) moves the card off its
    /// pivot by the sampled offset, on the arm-time cursor: armed at t=0, sampled at the loop's
    /// midpoint, the card sits at the middle key's offset. No `Time` plugin — the clock is set by
    /// hand so the sample point is exact.
    #[test]
    fn armed_seq_translation_bobs_the_card() {
        let mut app = App::new();
        app.init_resource::<Time>();
        app.add_systems(Update, face_billboards);
        // A camera straight ahead of the card's rest normal: the facing rotation is identity, so
        // the transform isolates the bob.
        app.world_mut().spawn((
            crate::player::WorldCamera,
            GlobalTransform::from_translation(Vec3::new(0.0, 0.0, 10.0)),
        ));
        let bob = BoneScaleAnim {
            duration_ms: 1000,
            interp: true,
            keys: vec![(0, [0.0; 3]), (500, [0.0, 1.0, 0.0]), (1000, [0.0; 3])],
        };
        let info = BillboardInfo {
            bone: 0,
            pivot: Vec3::ZERO,
            normal: Vec3::Z,
            kind: BillboardKind::LockZ,
            scale_anim: None,
            seq_translations: vec![], // doodad default: `new` never arms one
        };
        let card = app
            .world_mut()
            .spawn((
                BillboardCard::new(&info, Transform::IDENTITY).with_seq_translation(Some(bob), 0),
                Transform::IDENTITY,
            ))
            .id();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(500));
        app.update();
        let tf = app.world().entity(card).get::<Transform>().unwrap();
        assert_eq!(
            tf.translation,
            Vec3::new(0.0, 1.0, 0.0),
            "the middle key's offset, at the pivot"
        );
    }
}
