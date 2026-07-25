//! The doodad animation host (decision 0130, phase 1) — world-placed M2 doodads (ADT MDDF + WMO MODD
//! props) animate: flags wave, windmills turn, flame bones jiggle.
//!
//! Byte ground (wow-5875-re `system/animation/scratch/doodad-anim-host.md`, VERIFIED): a doodad's M2
//! instance is armed **once at load** — bone 0, the model's file-order-first sequence
//! (`animations[0].id`), `linkFlag=1` — and the kernel loops it forever via its own modulo-wrap;
//! **global sequences loop with zero arming**, clock-driven; and the kernel ticks as a **draw-time
//! side effect**, so a culled model is simply not evaluated. Because sampling is clock-indexed
//! (`cursor = clock − startOffset`), a re-appearing model shows the pose the shared clock dictates —
//! pausing costs nothing and drifts nothing.
//!
//! benilla's translation: the spawn site ([`crate::terrain_stream`]) classifies each placed model by
//! [`classify`] — the ~90% with no animated channel stay on today's static path (measured,
//! `benilla-extract doodadscan`) — and an animated one spawns the skinned twin + a joint hierarchy
//! under an anim-root entity carrying [`DoodadAnimHost`]: an `AnimationPlayer` looping the
//! first-sequence clip (the one-time arm), and/or a [`GlobalSeqDrive`] for the free-running channels.
//! [`gate_doodad_anim`] here is the draw-time tick made explicit: animation runs iff any of the
//! doodad's submeshes is actually drawn (the `Visibility` verdict the debug-panel authority composes
//! from far-clip + the size-bucketed distance fade + the WMO portal cull), and a resume seeks to
//! `(now − spawned_at) mod duration` — the ref's shared-clock phase, exactly.

use bevy::animation::graph::AnimationNodeIndex;
use bevy::animation::AnimatedBy;
use bevy::app::AnimationSystems;
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;

use benilla_assets::{bone_target_id, AnimClip, M2Model, ModelAnimations, ModelSkeleton};

use crate::creature_anim::GlobalSeqDrive;

/// What a placed doodad model animates — decision 0130's content gate, decided per model at spawn.
pub(crate) enum DoodadAnimTier<'a> {
    /// No animated bone channel: today's static path, untouched (no joints, no player — the ~90%
    /// measured case: trees, fences, barrels, rocks).
    Static,
    /// Only free-running global-sequence channels (candelabra glow pulses): joints +
    /// [`GlobalSeqDrive`], **no** `AnimationPlayer` — the cheapest animated tier.
    GlobalSeqOnly,
    /// The file-order-first sequence moves bones (flags, windmills, torch flames): joints + an
    /// `AnimationPlayer` looping this clip — the client's one-time load arm. Global-sequence
    /// channels ride along if the model also has them.
    FirstSeq(&'a AnimClip),
}

/// Classify a placed model for the spawn site. Boneless models are `Static` regardless of parsed
/// tracks — their skinned twin carries joint attributes with no joints to index (the 0035 guard).
pub(crate) fn classify<'a>(
    skeleton: &ModelSkeleton,
    animations: Option<&'a ModelAnimations>,
) -> DoodadAnimTier<'a> {
    let Some(anims) = animations else {
        return DoodadAnimTier::Static;
    };
    if skeleton.joints.is_empty() {
        return DoodadAnimTier::Static;
    }
    match anims.first_seq.and_then(|i| anims.clips.get(i)) {
        Some(clip) => DoodadAnimTier::FirstSeq(clip),
        None if !anims.global_bones.is_empty() => DoodadAnimTier::GlobalSeqOnly,
        None => DoodadAnimTier::Static,
    }
}

/// What [`spawn_anim_host`] set up for one animated placement — the spawn site binds each ordinary
/// submesh's skinned twin to `joints`/`inverse_bindposes` and arms the [`DoodadAnimHost`] on `root`
/// with the submesh list once it has spawned them.
pub(crate) struct AnimHostSpawn {
    pub root: Entity,
    pub joints: Vec<Entity>,
    pub inverse_bindposes: Handle<SkinnedMeshInverseBindposes>,
    /// The looping first-sequence node + duration, `None` on the gseq-only tier.
    pub clip: Option<(AnimationNodeIndex, f32)>,
}

/// Spawn the animation host for one placed M2, if [`classify`] says it animates: an anim-root entity
/// at the placement transform, the joint hierarchy as its children (so despawning the root cascades),
/// and — per tier — an `AnimationPlayer` looping the file-order-first sequence's clip (the client's
/// one-time load arm, wow-re `doodad-anim-host.md`: `0x7121a0(bone 0, animations[0].id, linkFlag=1)`
/// once, at `0x70ebd0`) and/or the free-running [`GlobalSeqDrive`]. `None` ⇒ the model is static and
/// the caller keeps today's path untouched.
pub(crate) fn spawn_anim_host(
    commands: &mut Commands,
    m: &M2Model,
    transform: Transform,
) -> Option<AnimHostSpawn> {
    // The visual A/B harness (decision 0010) needs deterministic frames; a live animation clock
    // isn't. Captures keep every doodad on the static path — bind pose renders identically to the
    // static mesh (decision 0035), so world baselines stay comparable across runs and branches.
    if crate::capture::scenario_active() {
        return None;
    }
    let tier = classify(&m.skeleton, m.animations.as_ref());
    if matches!(tier, DoodadAnimTier::Static) {
        return None;
    }
    let anims = m.animations.as_ref().expect("animated tier ⇒ animations");
    let root = commands.spawn((transform, Visibility::default())).id();
    let joints = crate::entities::spawn_joints(commands, root, &m.skeleton);
    // Billboard bones on an animated doodad (a swinging lamp's glow child): the palette-level
    // camera facing, children inheriting.
    if let Some(bb) = crate::billboard::BillboardJointRig::new(&m.skeleton, &joints, root) {
        commands.entity(root).insert(bb);
    }
    let mut clip_info = None;
    if let DoodadAnimTier::FirstSeq(head) = tier {
        // The effective load arm rolls `variationIdx = −1` — a frequency-weighted pick over the
        // first sequence's variation chain (wow-re `doodad-anim-host.md` §4a: the holder setup
        // `0x695100` re-arms over the loader's var-0 seed, byte-cited; §5 pair still to close).
        // Seeded off the placement position, not a global RNG: deterministic per placement (a
        // re-streamed tile re-picks the same variation — the reference re-rolls, invisible either
        // way), and instances de-sync exactly like the reference (14 gryphon roosts stop fidgeting
        // in lockstep). Single-variation chains (the overwhelming majority) pick the head — today's
        // behavior, so the roll costs nothing there.
        let t = transform.translation;
        let roll = ((t.x.to_bits() ^ t.y.to_bits().rotate_left(11) ^ t.z.to_bits().rotate_left(22))
            .wrapping_mul(0x9E37_79B9)
            >> 17) as u16
            & 0x7fff;
        let clip = anims.pick_variation(head.anim_id, roll).unwrap_or(head);
        let mut player = AnimationPlayer::default();
        player.play(clip.node).repeat();
        commands
            .entity(root)
            .insert((player, AnimationGraphHandle(anims.graph.clone())));
        for (i, &j) in joints.iter().enumerate() {
            commands
                .entity(j)
                .insert((bone_target_id(i as u16), AnimatedBy(root)));
        }
        clip_info = Some((clip.node, clip.duration));
    }
    if let Some(drive) = GlobalSeqDrive::new(&anims.global_bones, &joints) {
        commands.entity(root).insert(drive);
    }
    Some(AnimHostSpawn {
        root,
        joints,
        inverse_bindposes: m.inverse_bindposes.clone(),
        clip: clip_info,
    })
}

/// The anim-root component of one animated doodad placement: the draw gate's state + what to re-arm
/// on resume. The root entity carries the placement transform, the joint hierarchy as children, and
/// (per tier) the `AnimationPlayer`/[`GlobalSeqDrive`]; it lives in the placement's entity list, so
/// it despawns (joints cascading) when the tile streams out.
#[derive(Component)]
pub(crate) struct DoodadAnimHost {
    /// The placement's skinned submesh entities — animation runs iff ANY of them is drawn (their
    /// `Visibility` is the composed far-clip + distance-fade + portal-cull verdict).
    pub meshes: Vec<Entity>,
    /// The placement's fade sphere (radius, WORLD center) — the draw-set gate for a MESHLESS
    /// host (a particles-only model like the InstancePortal swirl: 0 render batches, so no
    /// submesh carries a `Visibility` verdict). Same law the placement's emitters gate on
    /// (decision 0171: fade alpha > 0 + frustum sphere), so joints and particle pools
    /// freeze/resume in lockstep.
    pub fade: (f32, Vec3),
    /// The looping first-sequence graph node + its duration (secs); `None` on the gseq-only tier.
    pub clip: Option<(AnimationNodeIndex, f32)>,
    /// `Time::elapsed_secs` at spawn — the clock origin. A resume seeks the player to
    /// `(now − spawned_at) mod duration` and re-seats the [`GlobalSeqDrive`] clock, so phase is a
    /// pure function of the shared clock (the ref's arm-time cursor), not of pause history.
    pub spawned_at: f32,
    /// Whether the host was ticking last frame (edge-triggered pause/resume).
    pub active: bool,
}

/// The draw gate: pause a doodad's animation when none of its submeshes is drawn, resume — seeking
/// both clocks to the shared-clock position — when one is again. Runs before [`AnimationSystems`] so
/// a resume's seek lands the same frame. Steady state (nothing flipped) is one `Visibility` read per
/// mesh, no writes.
fn gate_doodad_anim(
    time: Res<Time>,
    mut hosts: Query<(
        &mut DoodadAnimHost,
        Option<&mut AnimationPlayer>,
        Option<&mut GlobalSeqDrive>,
    )>,
    vis: Query<&Visibility>,
    cam: Query<
        (&GlobalTransform, &bevy::camera::primitives::Frustum),
        With<crate::player::WorldCamera>,
    >,
    mut logged: Local<bool>,
) {
    // One breadcrumb per session, the first frame any host exists — the machine-readable "doodads
    // are animating" signal (the live count is in the debug panel).
    if !*logged && !hosts.is_empty() {
        *logged = true;
        info!("doodad anim: first host armed (decision 0130 phase 1)");
    }
    let now = time.elapsed_secs();
    let world_cam = cam.single().ok();
    for (mut host, player, drive) in &mut hosts {
        let drawn = if host.meshes.is_empty() {
            // Meshless (particles-only) host: the emitters' own draw-set law (see the `fade`
            // field doc) — the reference ticks animation for any model in the draw set, and a
            // 0-batch model is admitted on its fade sphere exactly like its emitters are.
            let (radius, center) = host.fade;
            world_cam.is_some_and(|(cam_tf, frustum)| {
                let cam_pos = cam_tf.translation();
                let (dx, dz) = (center.x - cam_pos.x, center.z - cam_pos.z);
                crate::model_fade::doodad_fade_alpha(radius, (dx * dx + dz * dz).sqrt()) > 0.0
                    && frustum.intersects_sphere(
                        &bevy::camera::primitives::Sphere {
                            center: center.into(),
                            radius,
                        },
                        false,
                    )
            })
        } else {
            host.meshes
                .iter()
                .any(|&e| vis.get(e).is_ok_and(|v| *v != Visibility::Hidden))
        };
        if drawn == host.active {
            continue;
        }
        host.active = drawn;
        let t = now - host.spawned_at;
        if let Some(mut p) = player {
            if drawn {
                if let Some((node, duration)) = host.clip {
                    let anim = p.start(node);
                    anim.repeat();
                    if duration > 0.0 {
                        anim.seek_to(t.rem_euclid(duration));
                    }
                }
            } else {
                // Stop (not pause): a paused active animation is still sampled and applied every
                // frame by `animate_targets`; stopping empties the player so a hidden doodad costs
                // nothing. The resume above re-arms + seeks, which is the faithful clock semantics.
                p.stop_all();
            }
        }
        if let Some(mut d) = drive {
            d.set_paused(!drawn);
            if drawn {
                d.sync(t);
            }
        }
    }
}

/// Per-submesh **animated material alpha** (decision 0130 phase 2): the batch's baked
/// colour-alpha/weight loops + how this instance clocks them. [`sample_mat_anim`] keeps
/// [`Self::current`] fresh; the model-`Visibility` authority multiplies it into the render-alpha
/// `MeshTag` it already owns (a composed *input*, not an extra tag writer — the 0066 protocol) and
/// hides the batch at combined 0 (the verified `A ≤ 0` cull, wow-re `m2-alpha-combine-cull`).
///
/// The loops are baked **per sequence** (`benilla_formats::AlphaAnim`), so an instance also has to
/// say *which* sequence it is playing — see [`Self::host`].
#[derive(Component)]
pub(crate) struct MatAnim {
    anim: std::sync::Arc<benilla_formats::AlphaAnim>,
    /// The entity whose `AnimationPlayer` decides which sequence's loops to read, re-resolved every
    /// frame — the **unit lane**, where the played sequence changes constantly and the batch's
    /// authored visibility changes with it (a voidwalker's upper armour is weight 0 in Stand and 1
    /// only in Death). `None` for an instance pinned to one sequence for its life: a placed doodad
    /// (armed once at load with `animations[0]`, wow-re `doodad-anim-host.md`) or a spell effect —
    /// both then read [`Self::seq`] on the spawn clock, which is the pre-per-sequence behaviour.
    host: Option<Entity>,
    /// The sequence **file slot** to read: fixed at spawn for the pinned lanes, and the last one
    /// resolved from [`Self::host`] for the unit lane. `None` ⇒ slot 0 (the bake's own degrade).
    seq: Option<usize>,
    /// `Time::elapsed_secs` at spawn — the clock origin (arm-time phase, like the bone host). Only
    /// the pinned lanes use it; a hosted instance reads the player's own seek time instead, so its
    /// alpha stays in phase with the pose that drives it.
    spawned_at: f32,
    /// Captures freeze the clock at 0 for deterministic frames (dimming constants still show).
    frozen: bool,
    /// This instance's sampled value drives the render-alpha `MeshTag` field **by itself** (no
    /// `DoodadFade` on the entity): the spell-effect parts (`entities::spell_fx`), whose alpha
    /// channel has no other writer. `false` on the doodad lane — there the fade writer owns the
    /// tag and composes [`Self::current`] in (and a lit interior prop's tag carries a packed
    /// colour this flag must never overwrite) — and on the unit lane, whose own compose is
    /// [`crate::entities::apply_unit_mat_alpha`].
    pub(crate) drives_tag: bool,
    /// The last sampled combined factor (colour-alpha × weight), read by the visibility authority.
    pub current: f32,
}

impl MatAnim {
    pub(crate) fn new(
        anim: std::sync::Arc<benilla_formats::AlphaAnim>,
        now: f32,
        frozen: bool,
    ) -> Self {
        let current = anim.sample(None, 0.0);
        Self {
            anim,
            host: None,
            seq: None,
            spawned_at: now,
            frozen,
            drives_tag: false,
            current,
        }
    }

    /// The spell-effect-lane constructor: never frozen (the `fxview` instrument ages effects
    /// through captures; golden scenarios spawn no effects), the sampled alpha drives the part's
    /// render-alpha tag directly (see [`Self::drives_tag`]), and the instance is pinned to the one
    /// sequence its rig plays (`seq` — the missile's InFlight, else the file-order-first clip).
    pub(crate) fn driving_tag(
        anim: std::sync::Arc<benilla_formats::AlphaAnim>,
        now: f32,
        seq: Option<usize>,
    ) -> Self {
        let mut m = Self::new(anim, now, false);
        m.drives_tag = true;
        m.seq = seq;
        m.current = m.anim.sample(seq, 0.0);
        m
    }

    /// The **unit-lane** constructor: the sequence (and its clock) come from `host`'s live
    /// `AnimationPlayer`, so a creature's batches appear and disappear with the animation exactly
    /// as authored. The tag is composed by [`crate::entities::apply_unit_mat_alpha`], not driven
    /// here — the interior classifier and the appear-fade already own that channel.
    pub(crate) fn following(
        anim: std::sync::Arc<benilla_formats::AlphaAnim>,
        host: Entity,
    ) -> Self {
        Self {
            host: Some(host),
            ..Self::new(anim, 0.0, false)
        }
    }

    /// Whether this instance's tag alpha is the unit lane's to compose (see
    /// [`crate::entities::apply_unit_mat_alpha`]).
    pub(crate) fn composes_unit_tag(&self) -> bool {
        self.host.is_some() && !self.drives_tag
    }
}

/// The sequence slot + clip-local time a host is playing, for [`sample_mat_anim`]: the **base**
/// animation with the greatest blend weight. Masked overlays (a torso-only swing, an arm's draw
/// ceremony, the finger grip) run on their own graph nodes and are deliberately skipped — they
/// pose bones, they don't reselect the sequence the material tracks read. During a cross-fade two
/// base clips are live and the heavier one wins; the reference instead blends the two sampled
/// scalars by λ (wow-re `eval.md` FN 0x71af20's blend leg), a sub-blend-time difference on tracks
/// the corpus authors as 0/1 steps — recorded, not modelled.
fn playing_seq(player: &AnimationPlayer, anims: &ModelAnimations) -> Option<(usize, f32)> {
    player
        .playing_animations()
        .filter_map(|(node, active)| {
            let clip = anims.clips.iter().find(|c| c.node == *node)?;
            Some((clip.seq_index, active.seek_time(), active.weight()))
        })
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(seq, t, _)| (seq, t))
}

/// Sample every instance's material-alpha loops: a hosted instance on its host's playing sequence
/// and clip clock, a pinned one on its own spawn clock. Frozen (capture) instances keep their t=0
/// sample. Hidden instances sample too — the visibility authority's alpha cull reads
/// [`MatAnim::current`], so a batch BORN at weight 0 (`HolyLight_Low_Head`'s light column, keyed
/// `0 → 1` at 300 ms) must keep its clock running or the alpha-hide latches forever (the old
/// skip-while-Hidden held `current` at the 0 that caused the hide — the invisible pala-heal flash).
/// Sampling is a pure function of the clock, so an instance hidden for any *other* reason (draw
/// gate, far-clip) still lands right on re-appear, and the reference animation-evaluates the tracks
/// every frame regardless of the cull (wow-re `m2-alpha-combine-cull`). Runs before the visibility
/// authority so the tag it composes is this frame's value.
fn sample_mat_anim(
    time: Res<Time>,
    hosts: Query<(&AnimationPlayer, &ModelAnimations)>,
    mut q: Query<&mut MatAnim>,
) {
    let now = time.elapsed_secs();
    for mut m in &mut q {
        if m.frozen {
            continue;
        }
        // A hosted instance whose host has no player yet (the frame before the rig arms, or a
        // rest-pose GameObject) keeps its last resolved slot and reads it at t=0 — the sequence's
        // opening pose, which is what a model sitting at bind pose shows.
        let played = m
            .host
            .and_then(|h| hosts.get(h).ok())
            .and_then(|(p, a)| playing_seq(p, a));
        let (seq, elapsed) = match played {
            Some((seq, t)) => {
                m.seq = Some(seq);
                (Some(seq), t)
            }
            None if m.host.is_some() => (m.seq, 0.0),
            None => (m.seq, now - m.spawned_at),
        };
        m.current = m.anim.sample(seq, elapsed);
    }
}

/// The **UV-animated materials** registry (decision 0130 phase 3, wow-re `m2-texanim-uv`): each
/// batch material carrying a texture-transform translation loop, keyed by material asset id.
/// [`tick_uv_anim_materials`] re-samples every entry's offset into the material's `sun_scale.zw`
/// each frame — one shared uniform per material, so every instance of a model batch scrolls in
/// phase. Exactly faithful for gseq loops (the reference's free-running shared clock); a recorded,
/// invisible divergence for seq-band loops (the reference phases those per instance at arm time —
/// meaningless for a seamless scroll). Entries drop when the material asset does.
#[derive(Resource, Default)]
pub(crate) struct UvAnimMaterials(
    pub  std::collections::HashMap<
        bevy::asset::AssetId<crate::terrain::WowModelMaterial>,
        std::sync::Arc<benilla_formats::UvAnim>,
    >,
);

/// Scroll every UV-animated material's offset on the shared clock. Skipped entirely in captures
/// (materials keep their t = 0 seed — constants still show, frames stay deterministic). Mutating
/// the material asset re-uploads its packed uniform; the population is tiny (the corpus holds 113
/// texanim models game-wide, a handful in residency), so this is a few uniform writes per frame.
fn tick_uv_anim_materials(
    time: Res<Time>,
    mut reg: ResMut<UvAnimMaterials>,
    mut materials: ResMut<Assets<crate::terrain::WowModelMaterial>>,
) {
    if reg.0.is_empty() || crate::capture::scenario_active() {
        return;
    }
    let now = time.elapsed_secs();
    reg.0.retain(|id, anim| {
        let Some(mat) = materials.get_mut(*id) else {
            return false; // material unloaded with its cache — drop the entry
        };
        let uv = anim.sample(now);
        mat.extension.sun_scale.z = uv[0];
        mat.extension.sun_scale.w = uv[1];
        true
    });
}

/// The **tint-animated materials** registry — the M2Color-RGB twin of [`UvAnimMaterials`]: each
/// batch material whose colour track animates (the vertex bake is skipped for those —
/// `benilla-formats` `m2_batches`), keyed by material asset id. [`tick_tint_anim_materials`]
/// re-samples the tint into the material's `tint` uniform each frame on the same shared clock
/// (the same recorded seq-band phase divergence as the UV scroll — invisible for a placed
/// doodad's ambient loop). Spell-effect instances need real per-instance phase instead (one cast
/// = one 0.9 s pulse), so the effect lane clones its materials and ticks them on the instance
/// clock (`entities::spell_fx`), never through this registry.
#[derive(Resource, Default)]
pub(crate) struct TintAnimMaterials(
    pub  std::collections::HashMap<
        bevy::asset::AssetId<crate::terrain::WowModelMaterial>,
        std::sync::Arc<benilla_formats::RgbAnim>,
    >,
);

/// Re-sample every tint-animated material's RGB on the shared clock (see [`TintAnimMaterials`]).
/// Skipped in captures like the UV scroll (materials keep their first-key seed).
fn tick_tint_anim_materials(
    time: Res<Time>,
    mut reg: ResMut<TintAnimMaterials>,
    mut materials: ResMut<Assets<crate::terrain::WowModelMaterial>>,
) {
    if reg.0.is_empty() || crate::capture::scenario_active() {
        return;
    }
    let now = time.elapsed_secs();
    reg.0.retain(|id, anim| {
        let Some(mat) = materials.get_mut(*id) else {
            return false; // material unloaded with its cache — drop the entry
        };
        let rgb = anim.sample(now);
        mat.extension.tint = bevy::math::Vec4::new(rgb[0], rgb[1], rgb[2], 1.0);
        true
    });
}

pub struct DoodadAnimPlugin;

impl Plugin for DoodadAnimPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UvAnimMaterials>();
        app.init_resource::<TintAnimMaterials>();
        app.add_systems(PostUpdate, gate_doodad_anim.before(AnimationSystems));
        // Before the visibility authority (`ModelVisSet`): it composes `MatAnim::current` into the
        // render-alpha tag the same frame.
        app.add_systems(
            Update,
            (
                sample_mat_anim.before(crate::debug_panel::ModelVisSet),
                tick_uv_anim_materials,
                tick_tint_anim_materials,
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_assets::{GlobalBone, GlobalSeqChannel};

    fn clip(anim_id: u16) -> AnimClip {
        AnimClip {
            anim_id,
            seq_index: 0,
            node: AnimationNodeIndex::new(1),
            looping: true,
            duration: 2.0,
            move_speed: 0.0,
            blend_time: 0.0,
            bounds_center: Vec3::ZERO,
            bounds_radius: 0.0,
            bounds_min: Vec3::ZERO,
            bounds_max: Vec3::ZERO,
            events: Vec::new().into(),
            arm_nodes: None,
            upper_node: None,
            frequency: 0,
            replay: (0, 0),
        }
    }

    fn anims(clips: Vec<AnimClip>, first_seq: Option<usize>, gseq: bool) -> ModelAnimations {
        ModelAnimations {
            graph: Handle::default(),
            clips,
            playable_animation_lookup: Vec::new(),
            hand_close: [None, None],
            global_bones: if gseq {
                vec![GlobalBone {
                    bone: 1,
                    translation: None,
                    rotation: None,
                    scale: Some(GlobalSeqChannel {
                        period: 1.167,
                        keys: vec![(0.0, Vec3::ONE), (0.5, Vec3::splat(1.2))],
                    }),
                }]
            } else {
                Vec::new()
            },
            first_seq,
        }
    }

    /// One batch's per-sequence alpha, as the bake emits it: slot 0 hidden, slot 1 visible.
    fn two_seq_alpha() -> std::sync::Arc<benilla_formats::AlphaAnim> {
        let hidden = benilla_formats::ScalarAnim {
            period: 0.0,
            step: true,
            keys: vec![(0.0, 0.0)],
        };
        std::sync::Arc::new(
            benilla_formats::AlphaAnim::new(vec![
                benilla_formats::AlphaSeq {
                    color: None,
                    weight: Some(hidden),
                },
                benilla_formats::AlphaSeq::default(),
            ])
            .expect("a hiding sequence is worth carrying"),
        )
    }

    fn seq_clip(anim_id: u16, seq_index: usize, node: usize) -> AnimClip {
        AnimClip {
            seq_index,
            node: AnimationNodeIndex::new(node),
            ..clip(anim_id)
        }
    }

    /// The unit lane's resolution: a part reads the sequence its HOST is playing, and follows it
    /// when the host changes animation. This is the plumbing B16/B20 turn on — with the old
    /// single-sequence bake there was nothing to follow, so a voidwalker's death-only armour drew
    /// in every animation.
    #[test]
    fn hosted_mat_anim_follows_the_host_sequence() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_systems(Update, sample_mat_anim);

        // Two clips: graph node 1 is file sequence 0 (which hides the batch), node 2 is slot 1.
        let anims = anims(vec![seq_clip(0, 0, 1), seq_clip(1, 1, 2)], Some(0), false);
        let mut player = AnimationPlayer::default();
        player.play(AnimationNodeIndex::new(1)).repeat();
        let host = app.world_mut().spawn((player, anims)).id();
        let part = app
            .world_mut()
            .spawn(MatAnim::following(two_seq_alpha(), host))
            .id();

        app.update();
        assert_eq!(
            app.world().entity(part).get::<MatAnim>().unwrap().current,
            0.0,
            "playing sequence 0 ⇒ the batch is culled"
        );

        // Switch the host to the other sequence: the same part must now draw.
        let world = app.world_mut();
        let mut entity = world.entity_mut(host);
        {
            let mut p = entity.get_mut::<AnimationPlayer>().unwrap();
            p.stop_all();
            p.play(AnimationNodeIndex::new(2)).repeat();
        }
        app.update();
        assert_eq!(
            app.world().entity(part).get::<MatAnim>().unwrap().current,
            1.0,
            "playing sequence 1 ⇒ the batch draws"
        );
    }

    /// A hosted part whose host has no `AnimationPlayer` yet (the frame before the rig arms, a
    /// rest-pose GameObject) still resolves — to slot 0 at t=0, the sequence's opening pose. It
    /// must not read as fully visible by default, or the batch flashes for a frame at spawn.
    #[test]
    fn hosted_mat_anim_without_a_player_reads_the_first_sequence() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.add_systems(Update, sample_mat_anim);
        let host = app.world_mut().spawn(Transform::default()).id();
        let part = app
            .world_mut()
            .spawn(MatAnim::following(two_seq_alpha(), host))
            .id();
        app.update();
        assert_eq!(
            app.world().entity(part).get::<MatAnim>().unwrap().current,
            0.0
        );
    }

    /// The pinned lanes are unchanged: a doodad/effect instance reads the slot it was built with,
    /// on its own spawn clock, and never consults a host.
    #[test]
    fn pinned_mat_anim_reads_its_own_slot() {
        let a = two_seq_alpha();
        let doodad = MatAnim::new(a.clone(), 0.0, false);
        assert_eq!(doodad.current, 0.0, "slot 0 — the one-time load arm");
        assert!(!doodad.composes_unit_tag());
        let effect = MatAnim::driving_tag(a, 0.0, Some(1));
        assert_eq!(effect.current, 1.0, "the sequence the fx rig armed");
        assert!(effect.drives_tag);
        assert!(!effect.composes_unit_tag());
    }

    fn skeleton(joints: usize) -> ModelSkeleton {
        ModelSkeleton {
            joints: (0..joints)
                .map(|_| benilla_assets::ModelJoint {
                    parent: -1,
                    local_translation: Vec3::ZERO,
                    billboard: None,
                    ignore_parent_rotation: false,
                })
                .collect(),
            spine_bone: None,
            head_bone: None,
        }
    }

    /// The content gate: no animations / no joints / a motionless first sequence ⇒ static (today's
    /// path); gseq channels alone ⇒ the player-less tier; a moving first sequence ⇒ the looping arm.
    #[test]
    fn classify_picks_the_measured_tiers() {
        // No ModelAnimations at all (a barrel): static.
        assert!(matches!(
            classify(&skeleton(3), None),
            DoodadAnimTier::Static
        ));
        // Boneless model: static even with parsed animations (the 0035 out-of-bounds guard).
        let a = anims(vec![clip(0)], Some(0), true);
        assert!(matches!(
            classify(&skeleton(0), Some(&a)),
            DoodadAnimTier::Static
        ));
        // A motionless first sequence, no gseq (a posed tree): static — no skin, no player.
        let a = anims(vec![clip(0)], None, false);
        assert!(matches!(
            classify(&skeleton(3), Some(&a)),
            DoodadAnimTier::Static
        ));
        // Gseq only (a candelabra glow pulse): the player-less tier.
        let a = anims(Vec::new(), None, true);
        assert!(matches!(
            classify(&skeleton(3), Some(&a)),
            DoodadAnimTier::GlobalSeqOnly
        ));
        // A moving first sequence (a flag / the torch flame bone): loop its clip.
        let a = anims(vec![clip(0)], Some(0), true);
        assert!(matches!(
            classify(&skeleton(3), Some(&a)),
            DoodadAnimTier::FirstSeq(c) if c.anim_id == 0
        ));
    }

    /// The draw gate stops the player + pauses the gseq drive when every submesh is hidden, and on
    /// re-appearing re-arms at the shared-clock position (`(now − spawned_at) mod duration`) — the
    /// ref's clock-indexed pose, so pause history never desyncs phase.
    #[test]
    fn gate_pauses_hidden_and_resumes_on_the_shared_clock() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins); // Time
        app.add_systems(Update, gate_doodad_anim);

        let mesh = app.world_mut().spawn(Visibility::Inherited).id();
        let node = AnimationNodeIndex::new(1);
        let mut player = AnimationPlayer::default();
        player.play(node).repeat();
        let joint = app.world_mut().spawn(Transform::default()).id();
        let drive = GlobalSeqDrive::new(
            &anims(Vec::new(), None, true).global_bones,
            &[Entity::PLACEHOLDER, joint],
        )
        .expect("one gseq bone maps");
        let host = app
            .world_mut()
            .spawn((
                DoodadAnimHost {
                    meshes: vec![mesh],
                    fade: (1.0, Vec3::ZERO),
                    clip: Some((node, 2.0)),
                    spawned_at: 0.0,
                    active: true,
                },
                player,
                drive,
            ))
            .id();

        // Visible: stays active, player untouched.
        app.update();
        let playing = |app: &mut App, e: Entity| {
            app.world_mut()
                .entity(e)
                .get::<AnimationPlayer>()
                .unwrap()
                .playing_animations()
                .count()
        };
        assert_eq!(playing(&mut app, host), 1, "drawn ⇒ playing");

        // Hide the one submesh: the player empties (stopped, zero per-frame cost).
        *app.world_mut()
            .entity_mut(mesh)
            .get_mut::<Visibility>()
            .unwrap() = Visibility::Hidden;
        app.update();
        assert_eq!(playing(&mut app, host), 0, "hidden ⇒ stopped");
        assert!(
            !app.world()
                .entity(host)
                .get::<DoodadAnimHost>()
                .unwrap()
                .active
        );

        // Show it again: re-armed, looping, sought to the shared-clock position (< duration).
        *app.world_mut()
            .entity_mut(mesh)
            .get_mut::<Visibility>()
            .unwrap() = Visibility::Inherited;
        app.update();
        assert_eq!(playing(&mut app, host), 1, "re-drawn ⇒ re-armed");
        let world = app.world();
        let p = world.entity(host).get::<AnimationPlayer>().unwrap();
        let active = p.animation(node).expect("the first-seq node is active");
        assert!(
            active.seek_time() >= 0.0 && active.seek_time() < 2.0,
            "seek lands inside the loop: {}",
            active.seek_time()
        );
    }
}
