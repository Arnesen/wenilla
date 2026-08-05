//! The doodad animation host (decision 0130, phase 1) — world-placed M2 doodads (ADT MDDF + WMO MODD
//! props) animate: flags wave, windmills turn, flame bones jiggle.
//!
//! Byte ground (wow-5875-re `system/animation/scratch/doodad-anim-host.md`, VERIFIED): a doodad's M2
//! instance is armed at load — bone 0, animation id 0, `linkFlag=1` — and then **re-arms itself every
//! play-window, for ever**, rolling a fresh frequency-weighted variation each time (§5, decision
//! 0768: the watchdog `0x719370` fires on `now ≥ windowHi`, enqueues the doodad-only completion
//! callback `0x6951b0` that `0x695100` installed at `[model+0x70]`, and that callback re-runs op4
//! with `variationIdx = -1`, which writes the next `windowHi` and clears the latch — self-sustaining).
//! **Global sequences loop with zero arming**, clock-driven. The cycle is gated on **linkage** — the
//! per-frame walk `0x7074b0` only advances models spliced into the scene list, which the doodad drain
//! `0x683f80` does per **in-range** doodad — not on the draw, so a doodad behind the camera keeps
//! cycling. Because sampling is clock-indexed (`cursor = clock − startOffset`), a re-appearing model
//! shows the pose the shared clock dictates — pausing costs nothing and drifts nothing.
//!
//! benilla's translation: the spawn site ([`crate::terrain_stream`]) classifies each placed model by
//! [`classify`] — the ~90% with no animated channel stay on today's static path (measured,
//! `benilla-extract doodadscan`) — and an animated one spawns the skinned twin + a joint hierarchy
//! under an anim-root entity carrying [`DoodadAnimHost`]: an `AnimationPlayer` on the armed clip,
//! re-rolled every window by [`reroll_doodad_variation`], and/or a [`GlobalSeqDrive`] for the
//! free-running channels. [`gate_doodad_anim`] here is the draw-time tick made explicit: animation
//! runs iff any of the doodad's submeshes is actually drawn (the `Visibility` verdict the debug-panel
//! authority composes from far-clip + the size-bucketed distance fade + the WMO portal cull), and a
//! resume seeks to `(now − armed_at) mod duration` — the ref's shared-clock phase, measured from the
//! current arm. Note the two gates are deliberately different: the *draw* gates the pose, the
//! *linkage* (here: residency) gates the variation cycle.

use bevy::animation::graph::AnimationNodeIndex;
use bevy::animation::AnimatedBy;
use bevy::app::AnimationSystems;
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;

use benilla_assets::{bone_target_id, AnimClip, M2Model, ModelAnimations, ModelSkeleton};

use crate::creature_anim::GlobalSeqDrive;

mod lazy;
mod mat_anim;
pub(crate) use lazy::{LazyRig, SkinnedTwin};
use mat_anim::tick_anim_materials;
pub(crate) use mat_anim::{
    playing_seq, sample_mat_anim, MatAnim, TintAnimMaterials, UvAnimMaterials,
};

/// The client's single global `rand()` stream — the MSVC LCG at `0x7400e5`, returning `[0, 32767]`
/// (wow-re `doodad-anim-host.md` §5, decision 0768). Every doodad's variation roll draws from **one**
/// shared stream, which is what de-syncs a stand of identical props: not a per-placement seed, just
/// consecutive draws off one sequence.
///
/// This replaced a position-derived hash. The hash de-synced instances correctly but was *permanent* —
/// the same placement rolled the same variation on every re-stream and every run — which is exactly
/// how the Blasted Lands lightning ended up striking from one fixed spot for ever instead of wandering
/// the Tainted Scar (bug B63's residual). Captures are unaffected: [`spawn_anim_host`] returns `None`
/// under a capture scenario, so no doodad animates in a golden frame and determinism is untouched.
#[derive(Resource)]
pub(crate) struct AnimRng(u32);

impl Default for AnimRng {
    fn default() -> Self {
        Self(1) // the CRT's own initial seed
    }
}

impl AnimRng {
    /// One `rand()` draw: `seed = seed·214013 + 2531011`, result `(seed >> 16) & 0x7fff`.
    fn draw(&mut self) -> u16 {
        self.0 = self.0.wrapping_mul(214_013).wrapping_add(2_531_011);
        ((self.0 >> 16) & 0x7fff) as u16
    }

    /// The play-window's replay count `R = max(1, min + ((rand()·(max−min)) >> 15))` — the reference's
    /// `windowHi = now + span·R` (wow-re §5). `replay = (0, 0)`, the overwhelming majority and the
    /// lightning's own value, always yields `R = 1`: one loop per window, so the variation re-rolls
    /// every single pass. The draw is taken unconditionally — it is one sub-expression of the
    /// reference's formula, so the shared stream advances the same way whatever the span.
    fn replay_count(&mut self, replay: (u32, u32)) -> u32 {
        let (lo, hi) = replay;
        let r = lo + ((u32::from(self.draw()) * hi.saturating_sub(lo)) >> 15);
        r.max(1)
    }
}

/// What a placed doodad model animates — decision 0130's content gate, decided per model at spawn.
pub(crate) enum DoodadAnimTier<'a> {
    /// No animated bone channel: today's static path, untouched (no joints, no player — the ~90%
    /// measured case: trees, fences, barrels, rocks).
    Static,
    /// Only free-running global-sequence channels (candelabra glow pulses): joints +
    /// [`GlobalSeqDrive`], **no** `AnimationPlayer` — the cheapest animated tier.
    GlobalSeqOnly,
    /// The **loader-idle seed** moves bones (flags, windmills, torch flames): joints + an
    /// `AnimationPlayer` looping this clip — the client's one-time load arm. That seed is animation
    /// id 0 (Stand) resolved through the model's own `playableAnimationLookup`, NOT the file-order
    /// first sequence (decision 0637; the variant name predates the correction and is kept because
    /// it is the tier's identity, not a claim about which sequence). Global-sequence channels ride
    /// along if the model also has them.
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
    // `first_seq` — 0130's content gate, and only that (decision 0936's split; the loader arm's
    // *identity* answer is `idle_seq`/`idle_clip`, which the entity lane arms from). It stays the
    // rig question here: a seed that holds bind pose renders identically to the static mesh.
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
    /// The host's joint entities, bone-indexed. NO palette slot rides along any more (decision
    /// 0863): the terrain-stream caller stores these in a [`LazyRig`] and the draw gate
    /// allocates at first wake; a gate-less caller (quest markers) allocates eagerly itself.
    pub joints: Vec<Entity>,
    pub inverse_bindposes: Handle<SkinnedMeshInverseBindposes>,
    /// The looping first-sequence node + duration, `None` on the gseq-only tier.
    pub clip: Option<(AnimationNodeIndex, f32)>,
    /// The **file sequence slot** the load arm seeded ([`AnimClip::seq_index`]) — the axis every
    /// per-sequence bake is keyed on. `None` on the gseq-only tier (no arm).
    ///
    /// This is the *loader's* var-0 seed only. It is no longer what the placement ends up playing:
    /// [`reroll_doodad_variation`] overrides it on the first frame with a real weighted roll and
    /// keeps re-rolling every play-window (decision 0768), exactly as the reference's holder setup
    /// `0x695100` lands its `variationIdx = -1` arm after the loader's var-0 one. Consumers that
    /// must track the *current* slot therefore read the host's live player instead — the emitters
    /// ride [`crate::particles::EmitClock::Host`].
    pub seq: Option<usize>,
    /// The animation id the arm rolls its variation over (id 0 — the loader-idle seed's id, resolved
    /// through the model's own `playableAnimationLookup`). `None` on the gseq-only tier: nothing was
    /// armed, so there is no play-window and nothing to re-roll.
    pub anim_id: Option<u16>,
}

/// Whether [`spawn_anim_host`] would rig this model — the model-forms requester's twin of the
/// spawn gate (decision 0834), so the skinned twins are built exactly for the placed models that
/// will draw them. Mirrors both of the host's gates: the tier, and the capture freeze (captures
/// keep every doodad static, so they need no twins either).
pub(crate) fn wants_rig(m: &M2Model) -> bool {
    !crate::capture::scenario_active()
        && !matches!(
            classify(&m.skeleton, m.animations.as_ref()),
            DoodadAnimTier::Static
        )
}

/// Spawn the animation host for one placed M2, if [`classify`] says it animates: an anim-root entity
/// at the placement transform, the joint hierarchy as its children (so despawning the root cascades),
/// and — per tier — an `AnimationPlayer` looping the **loader-idle seed's** clip (the client's
/// one-time load arm at `0x70ebd0`: `0x7121a0(bone 0, animation id 0 resolved through the model's
/// own `playableAnimationLookup`, linkFlag=1)` once — wow-re `gameobject-anim-arm.md` §1, which
/// CORRECTED `doodad-anim-host.md` §1's prose reading of `animations[0].id`; decision 0637) and/or
/// the free-running [`GlobalSeqDrive`]. `None` ⇒ the model is static and
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
    let joints = crate::entities::spawn_joints(commands, root, root, &m.skeleton);
    // NO palette rig here (decision 0863). The host's slot allocation is the CALLER's policy:
    // the terrain-stream lane goes lazy — parts spawn on the static form and the draw gate
    // allocates at the first wake ([`LazyRig`]) — because the resident population is thousands
    // of parked hosts while the 2048-slot table is the scarce axis (the 0863 census: 1300–1750
    // parked of ~1900 live slots, active peak ~630). A gate-less caller (quest markers)
    // allocates eagerly itself, or its host would never skin.
    //
    // Billboard bones on an animated doodad (a swinging lamp's glow child): the joint-level
    // camera facing, children inheriting — independent of the palette slot (its pass rewrites
    // joint GlobalTransforms, which the cards follow whether or not the parts skin yet).
    if let Some(bb) = crate::billboard::BillboardJointRig::new(&m.skeleton, &joints, root) {
        commands.entity(root).insert(bb);
    }
    let mut clip_info = None;
    let mut armed_seq = None;
    let mut arm_id = None;
    if let DoodadAnimTier::FirstSeq(head) = tier {
        // The **loader's** seed only — `0x70ebd0`'s var-0 arm on the head of the chain. The real
        // pick is the holder setup's second op4 call (`0x695100`, `variationIdx = -1`), which lands
        // *after* it and is the effective arm (wow-re §4a); here that second arm is
        // [`reroll_doodad_variation`]'s first pass, which fires the same frame because the host is
        // born with an already-expired window. Splitting it that way is not a convenience: it is
        // the one code path the reference has, since every later re-arm is byte-identical to the
        // holder's first one.
        let mut player = AnimationPlayer::default();
        player.play(head.node).repeat();
        commands.entity(root).insert((
            player,
            AnimationGraphHandle(anims.graph.clone()),
            // The re-roll needs the variation chain, and — now that a placed doodad's played
            // sequence CHANGES — so does every per-sequence consumer that resolves off this host
            // (`playing_seq`): the emitters' rate/enable tracks, the material-alpha sampler.
            anims.clone(),
        ));
        for (i, &j) in joints.iter().enumerate() {
            commands
                .entity(j)
                .insert((bone_target_id(i as u16), AnimatedBy(root)));
        }
        clip_info = Some((head.node, head.duration));
        armed_seq = Some(head.seq_index);
        arm_id = Some(head.anim_id);
    }
    if let Some(drive) = GlobalSeqDrive::new(&anims.global_bones, &joints) {
        commands.entity(root).insert(drive);
    }
    Some(AnimHostSpawn {
        root,
        joints,
        inverse_bindposes: m.inverse_bindposes.clone(),
        clip: clip_info,
        seq: armed_seq,
        anim_id: arm_id,
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
    /// `Time::elapsed_secs` at the current **arm** — the player's clock origin: the variation
    /// re-arms every play-window, and a resume must seek to `(now − armed_at) mod duration` or it
    /// lands at the phase of a clip this host stopped playing several windows ago. (The
    /// [`GlobalSeqDrive`] needs no origin at all — global sequences sample the shared world clock
    /// directly, decision 0855.)
    pub armed_at: f32,
    /// When the armed play-window ends, on the shared clock — the reference's `windowHi`
    /// (`[model+0xac]`), rewritten by every arm. `now ≥ window_hi` ⇒ re-roll. A host is born with
    /// this at `NEG_INFINITY` so the first frame performs the holder's `variationIdx = -1` arm.
    pub window_hi: f32,
    /// The animation id to re-roll over ([`AnimHostSpawn::anim_id`]); `None` on the gseq-only tier,
    /// which has no arm and therefore no window.
    pub anim_id: Option<u16>,
    /// Whether the host was ticking last frame (edge-triggered pause/resume).
    pub active: bool,
    /// `Time::elapsed_secs` when `active` last flipped false — the reaper's age key (decision
    /// 0863): under table pressure the longest-parked rigged hosts demote first. Never read
    /// while `active`.
    pub parked_at: f32,
}

/// The doodad's self-sustaining re-arm (decision 0768, wow-re `doodad-anim-host.md` §5): when the
/// armed play-window ends, roll a fresh frequency-weighted variation of the same animation id, snap
/// to it, and write the next window. This is the whole of bug B63's residual — the Blasted Lands
/// lightning keys its entire burst in a 5.0 %-weighted variation (`frequency` 1638 of 32767), so
/// with a once-at-load pick ~1.5 of the Tainted Scar's 31 placements strobed from a *fixed* spot
/// every 1.3 s for ever, while the reference re-rolls all 31 every window and the strike wanders.
///
/// Two details that are easy to get wrong, both byte-pinned:
/// - **The re-arm is a snap, not a blend** (`blendFlag = 0` at `0x6951c8`) — hence `stop_all` before
///   the play rather than a cross-fade.
/// - **The window advances on linkage, not on the draw.** The reference's per-frame walk covers
///   every model spliced into the scene list, which the doodad drain does per *in-range* doodad, so
///   a doodad behind the camera keeps cycling. This system therefore runs over ALL hosts and never
///   consults [`DoodadAnimHost::active`] — gating it on the draw instead would freeze the whole
///   field while you looked away and then re-roll all 31 placements on the same frame you turned
///   back, a burst of simultaneous strikes that the reference cannot produce.
///
/// A host that is not currently drawn still re-rolls; it just updates [`DoodadAnimHost::clip`] and
/// leaves the (stopped) player alone, so [`gate_doodad_anim`]'s resume arms whatever the latest
/// window rolled.
fn reroll_doodad_variation(
    time: Res<Time>,
    mut rng: ResMut<AnimRng>,
    mut hosts: Query<(
        &mut DoodadAnimHost,
        &ModelAnimations,
        Option<&mut AnimationPlayer>,
    )>,
) {
    let now = time.elapsed_secs();
    for (mut host, anims, player) in &mut hosts {
        let Some(anim_id) = host.anim_id else {
            continue; // gseq-only: free-clock loops, never armed (§1)
        };
        if now < host.window_hi {
            continue;
        }
        let Some(clip) = anims.pick_variation(anim_id, rng.draw()) else {
            // No chain for this id (a model whose sequence set changed under us): stop asking.
            host.anim_id = None;
            continue;
        };
        let (node, duration) = (clip.node, clip.duration);
        let replay = rng.replay_count(clip.replay);
        host.armed_at = now;
        // `span · R` — for `replay = (0, 0)` that is exactly one loop, so the roll repeats every
        // pass. A zero-duration clip would make this a busy loop, so it costs one window minimum.
        host.window_hi = now + (duration * replay as f32).max(f32::EPSILON);
        host.clip = Some((node, duration));
        if host.active {
            if let Some(mut p) = player {
                p.stop_all(); // the snap
                p.play(node).repeat();
            }
        }
    }
}

/// The draw gate: pause a doodad's animation when none of its submeshes is drawn, resume — seeking
/// both clocks to the shared-clock position — when one is again. Runs before [`AnimationSystems`] so
/// a resume's seek lands the same frame. Steady state (nothing flipped) is one `Visibility` read per
/// mesh, no writes.
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // one Bevy system's full input set
fn gate_doodad_anim(
    time: Res<Time>,
    mut hosts: Query<(
        Entity,
        &mut DoodadAnimHost,
        Option<&lazy::LazyRig>,
        Has<crate::rig_palette::RigSkin>,
        Option<&mut AnimationPlayer>,
        Option<&mut GlobalSeqDrive>,
    )>,
    vis: Query<&Visibility>,
    cam: Query<
        (
            &GlobalTransform,
            &bevy::camera::primitives::Frustum,
            &bevy::camera::Projection,
        ),
        With<crate::player::WorldCamera>,
    >,
    // The far-clip wall — the meshless host's draw-set gate needs the same depth bound the
    // emitters and the doodad meshes use…
    view: Res<crate::view::ViewDistance>,
    // …and the same exterior-window term, for the same reason: a meshless prop OUTSIDE, seen from a
    // WMO interior, is not in the frame's worklist and must not tick its bones either (0786).
    exterior_windows: Res<crate::wmo_portal::ExteriorWindows>,
    camera_claim: Res<crate::wmo_portal::CameraInteriorClaim>,
    // The lazy-rig lane's wake half (decision 0863, [`lazy`]): a drawn host without a slot
    // promotes here — allocation, row seed, part swap.
    mut palettes: ResMut<crate::rig_palette::RigPalettes>,
    ibps: Res<Assets<SkinnedMeshInverseBindposes>>,
    worlds: Query<&GlobalTransform>,
    mut twin_parts: lazy::TwinParts,
    mut commands: Commands,
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
    let exterior_gate = crate::exterior_cull::ExteriorGate::build(
        &exterior_windows,
        world_cam.map(|(tf, _, proj)| (tf, proj)),
    );
    let camera_instance = camera_claim.0.map(|c| c.room.instance);
    for (entity, mut host, lazy, has_rig, player, drive) in &mut hosts {
        let drawn = if host.meshes.is_empty() {
            // Meshless (particles-only) host: the emitters' own draw-set law (see the `fade`
            // field doc) — the reference ticks animation for any model in the draw set, and a
            // 0-batch model is admitted on its fade sphere exactly like its emitters are.
            // "Exactly like" is now literal: this calls `EmitterFade::in_draw_set`, the single
            // spelling of the rule, rather than a second copy of it. The copy is how the far-clip
            // term went missing here as well as in the emitters (decision 0678 / bug B39) — a
            // meshless fire prop kept animating its bones at any distance past the wall.
            let (radius, center) = host.fade;
            world_cam.is_some_and(|(cam_tf, frustum, _)| {
                let cam_pos = cam_tf.translation();
                // A meshless host is a placement's own particles-only model; it carries no building
                // instance of its own, so the honest answer here is "test my sphere".
                let fade = crate::particles::EmitterFade {
                    radius,
                    center,
                    instance: None,
                };
                fade.in_draw_set(
                    cam_pos,
                    Vec3::from(cam_tf.forward()),
                    view.farclip,
                    frustum.intersects_sphere(
                        &bevy::camera::primitives::Sphere {
                            center: center.into(),
                            radius,
                        },
                        false,
                    ),
                    fade.exterior_admitted(&exterior_gate, camera_instance),
                )
            })
        } else {
            host.meshes
                .iter()
                .any(|&e| vis.get(e).is_ok_and(|v| *v != Visibility::Hidden))
        };
        // The lazy-rig promote (decision 0863, [`lazy`]): a host DRAWN two frames running (`&&
        // host.active` — last frame's verdict) holding a [`lazy::LazyRig`] and no slot yet — its
        // first wake, or a denial being retried — claims its palette slot now. Checked every
        // frame it stays drawn, not just on the flip edge, so a momentarily full table is a
        // short delay at bind pose, never the permanent statue this lane was built to kill.
        //
        // Two frames, not one, because the first frame is a LIE: a spawned part's `Visibility`
        // defaults to `Inherited` (a `Mesh3d` required component) and the fade/cull authorities
        // ran before the spawner's commands applied — so on its first frame every far doodad in
        // the streamed tile reads "drawn". Promoting on that frame re-created the eager design
        // one frame late (measured on the 0863 verification leg: ~1500 parked hosts holding
        // slots, the pre-fix census's own number). The second frame's verdict is the
        // authorities' real one; a genuinely visible host promotes one frame late, at fade-in
        // distance, invisibly.
        if drawn && host.active && !has_rig {
            if let Some(lazy) = lazy {
                lazy::promote_lazy_rig(
                    &mut commands,
                    &mut palettes,
                    &ibps,
                    &worlds,
                    entity,
                    lazy,
                    &mut twin_parts,
                );
            }
        }
        if drawn == host.active {
            continue;
        }
        host.active = drawn;
        if !drawn {
            host.parked_at = now; // the reaper's age key (decision 0863)
        }
        if let Some(mut p) = player {
            if drawn {
                if let Some((node, duration)) = host.clip {
                    let anim = p.start(node);
                    anim.repeat();
                    if duration > 0.0 {
                        // Phase from the current ARM, not from spawn: the variation re-arms every
                        // play-window, so `spawned_at` names a clip this host may have stopped
                        // playing many windows ago (decision 0768).
                        anim.seek_to((now - host.armed_at).rem_euclid(duration));
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
            // No re-seek on resume: the drive samples the shared world clock (decision 0855), so
            // a re-appearing doodad lands on the phase the clock dictates by construction.
            d.set_paused(!drawn);
        }
    }
}

pub struct DoodadAnimPlugin;

impl Plugin for DoodadAnimPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UvAnimMaterials>();
        app.init_resource::<TintAnimMaterials>();
        app.init_resource::<AnimRng>();
        // The re-roll runs BEFORE the draw gate: a window that expires this frame must arm its new
        // clip before the gate decides what to resume, or a host re-appearing on the same frame
        // resumes the previous window's variation for one frame.
        app.add_systems(
            PostUpdate,
            (reroll_doodad_variation, gate_doodad_anim)
                .chain()
                .before(AnimationSystems),
        );
        // The lazy-rig pressure reaper (decision 0863): one headroom read per frame when the
        // table has room; demotes longest-parked rigged hosts when it doesn't.
        app.add_systems(Update, lazy::reap_parked_rigs);
        // `sample_mat_anim` runs before the visibility authority (`ModelVisSet`): it composes
        // `MatAnim::current` into the render-alpha tag the same frame. The material tick runs
        // *after* it, because its draw gate reads the verdict that authority writes — one frame
        // later would freeze a scroll for a frame on every doodad that comes into view.
        app.add_systems(
            Update,
            (
                sample_mat_anim.before(crate::debug_panel::ModelVisSet),
                tick_anim_materials.after(crate::debug_panel::ModelVisSet),
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
            poses_bones: true,
        }
    }

    fn anims(clips: Vec<AnimClip>, first_seq: Option<usize>, gseq: bool) -> ModelAnimations {
        ModelAnimations {
            graph: Handle::default(),
            clips,
            playable_animation_lookup: Vec::new(),
            animation_lookup: Vec::new(),
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
            pose: Default::default(),
        }
    }

    /// One batch's per-sequence alpha, as the bake emits it: slot 0 hidden, slot 1 visible.
    fn two_seq_alpha() -> std::sync::Arc<benilla_formats::AlphaAnim> {
        let hidden = benilla_formats::ScalarAnim {
            period: 0.0,
            step: true,
            wrap: true, // period 0: a constant has no clock
            gseq: false,
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
                    parent_arm: None,
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
        // A CLOCK-ONLY model (decision 0941): every sequence now builds a clip, so a model whose
        // bones never move — the Molten Core rune, the flame ring, 807 corpus models whose
        // animation lives entirely in their emitter tracks — arrives here with clips and NO
        // `first_seq` (the content gate declined it). It must still classify Static: its clock is
        // armed by the entity lane off `idle_clip`, and rigging it could only reproduce the
        // bind-pose mesh. This is the case that used to reach `first_seq: None` by never having
        // built a clip at all.
        let a = anims(vec![clip(0)], None, false);
        assert!(matches!(
            classify(&skeleton(3), Some(&a)),
            DoodadAnimTier::Static
        ));
        // ...and one that also pulses on a global sequence takes the player-less tier, not a rig.
        let a = anims(vec![clip(0)], None, true);
        assert!(matches!(
            classify(&skeleton(3), Some(&a)),
            DoodadAnimTier::GlobalSeqOnly
        ));
    }

    /// The client's `rand()` is the MSVC LCG, and its seed-1 stream is the textbook one. Pinning the
    /// first draws keeps the weighted roll on the reference's actual sequence rather than on "some
    /// uniform generator" (wow-re `_rand 0x7400e5`, decision 0768).
    #[test]
    fn the_anim_rng_is_the_reference_msvc_stream() {
        let mut rng = AnimRng::default();
        let first: Vec<u16> = (0..6).map(|_| rng.draw()).collect();
        assert_eq!(first, vec![41, 18467, 6334, 26500, 19169, 15724]);
        assert!(first.iter().all(|&v| v <= 0x7fff), "range is [0, 32767]");
    }

    /// `R = max(1, min + ((rand()·(max−min)) >> 15))`. `(0, 0)` — the overwhelming majority, and the
    /// lightning's own value — pins to 1, so the window is exactly one loop and the variation
    /// re-rolls every pass.
    #[test]
    fn the_replay_window_is_one_loop_for_the_common_case() {
        let mut rng = AnimRng::default();
        for _ in 0..32 {
            assert_eq!(rng.replay_count((0, 0)), 1);
        }
        // A real range stays inside it, and never degenerates to 0 windows.
        for _ in 0..64 {
            let r = rng.replay_count((2, 5));
            assert!((2..=5).contains(&r), "R = {r} outside [2, 5]");
        }
    }

    /// The lightning's own chain, read off the bytes (`benilla-extract m2seq` on
    /// `World\Generic\PassiveDoodads\ParticleEmitters\BlastedLandsLightningbolt01.M2`): two
    /// variations of animation id 0, weights 31129 / 1638 — so slot 1, which is where all four
    /// emitters key their entire burst, carries exactly 5.0 %.
    fn lightning_anims() -> ModelAnimations {
        let mut a = anims(vec![seq_clip(0, 0, 1), seq_clip(0, 1, 2)], Some(0), false);
        a.clips[0].frequency = 31129;
        a.clips[0].duration = 1.333;
        a.clips[1].frequency = 1638;
        a.clips[1].duration = 1.300;
        a
    }

    fn reroll_app() -> App {
        // Deliberately NOT MinimalPlugins: `TimePlugin` drives `Time` off the real clock, which
        // would clobber the manual advance these tests need to step whole play-windows.
        let mut app = App::new();
        app.init_resource::<Time>();
        app.init_resource::<AnimRng>();
        app.add_systems(Update, reroll_doodad_variation);
        app
    }

    fn lightning_host(app: &mut App) -> Entity {
        app.world_mut()
            .spawn((
                DoodadAnimHost {
                    meshes: Vec::new(),
                    fade: (1.0, Vec3::ZERO),
                    clip: None,
                    armed_at: 0.0,
                    window_hi: f32::NEG_INFINITY,
                    anim_id: Some(0),
                    active: true,
                    parked_at: 0.0,
                },
                lightning_anims(),
                AnimationPlayer::default(),
            ))
            .id()
    }

    /// **Bug B63's residual, and the whole of decision 0768.** A placed doodad re-rolls its
    /// frequency-weighted variation every play-window, for ever — it does NOT hold the one it
    /// picked at load.
    ///
    /// Under the superseded contract the pick was once-at-load *and* seeded off the placement's
    /// world position, so a Tainted Scar bolt that rolled slot 0 was silent for the life of the
    /// session and one that rolled slot 1 strobed from that same fixed spot every 1.3 s for ever.
    /// The reference re-rolls all 31 placements every window, which is why its strikes wander.
    #[test]
    fn a_placed_doodad_rerolls_its_variation_every_play_window() {
        let mut app = reroll_app();
        let host = lightning_host(&mut app);

        // Frame 1: born with an expired window ⇒ the holder's `variationIdx = -1` arm lands at once.
        app.update();
        let armed = |app: &App| {
            app.world()
                .entity(host)
                .get::<DoodadAnimHost>()
                .unwrap()
                .clip
        };
        assert!(armed(&app).is_some(), "the first frame arms");

        // Step whole windows and record which variation each one landed on.
        let mut seen = std::collections::HashMap::new();
        for _ in 0..400 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_millis(1400));
            app.update();
            let h = app.world().entity(host).get::<DoodadAnimHost>().unwrap();
            let (node, duration) = h.clip.expect("a window always leaves something armed");
            // R = 1 for `replay = (0, 0)`, so the window is exactly the armed clip's own length.
            assert!(
                (h.window_hi - h.armed_at - duration).abs() < 1e-3,
                "window {} != clip duration {duration}",
                h.window_hi - h.armed_at
            );
            *seen.entry(node).or_insert(0u32) += 1;
        }

        let slot0 = seen.get(&AnimationNodeIndex::new(1)).copied().unwrap_or(0);
        let slot1 = seen.get(&AnimationNodeIndex::new(2)).copied().unwrap_or(0);
        assert_eq!(slot0 + slot1, 400, "every window arms one of the two");
        // The bug was a placement stuck on ONE variation for ever. Both must occur.
        assert!(
            slot1 > 0,
            "the 5 % strike variation never came up in 400 windows — the bolt is stuck again"
        );
        assert!(slot0 > 0, "the 95 % silent variation never came up");
        // A sanity band, not a distribution test — 400 windows off one fixed seed is far too few
        // for that (it lands on 10, ~2.3σ under the mean, unremarkable). The weighting itself was
        // measured separately over 200 000 draws: **4.9665 %** taken at the real
        // two-draws-per-window stride, against the authored 1638/32768 = 4.9988 %. MSVC's LCG shows
        // no stride bias here, which is worth having checked: the variation roll and the replay
        // count come off the same stream, and a strided LCG is exactly where a rare variation could
        // quietly under-fire and leave the field half as active as the reference's.
        assert!(
            (2..=60).contains(&slot1),
            "slot 1 came up {slot1}/400 — far enough off its authored 5 % to suspect the roll"
        );
    }

    /// The gseq-only tier has no arm, so it has no play-window and nothing to re-roll: its channels
    /// are free-clock loops the reference drives with zero arming (§1).
    #[test]
    fn a_gseq_only_host_never_rerolls() {
        let mut app = reroll_app();
        let host = app
            .world_mut()
            .spawn((
                DoodadAnimHost {
                    meshes: Vec::new(),
                    fade: (1.0, Vec3::ZERO),
                    clip: None,
                    armed_at: 0.0,
                    window_hi: f32::NEG_INFINITY,
                    anim_id: None,
                    active: true,
                    parked_at: 0.0,
                },
                anims(Vec::new(), None, true),
                AnimationPlayer::default(),
            ))
            .id();
        for _ in 0..8 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_millis(1400));
            app.update();
        }
        let h = app.world().entity(host).get::<DoodadAnimHost>().unwrap();
        assert!(h.clip.is_none(), "nothing was ever armed");
        assert_eq!(h.window_hi, f32::NEG_INFINITY, "no window was ever opened");
    }

    /// A host that is not currently drawn still advances its window — the reference gates the cycle
    /// on **linkage** (the in-range doodad drain), not on the draw. Gating on the draw would freeze
    /// the field while the camera looks away and then re-roll every placement on the one frame it
    /// turns back: a burst of simultaneous strikes the reference cannot produce. The undrawn host
    /// keeps its stopped player and just updates what a resume will arm.
    #[test]
    fn an_undrawn_host_keeps_cycling() {
        let mut app = reroll_app();
        let host = lightning_host(&mut app);
        app.world_mut()
            .entity_mut(host)
            .get_mut::<DoodadAnimHost>()
            .unwrap()
            .active = false;

        app.update();
        let first = app
            .world()
            .entity(host)
            .get::<DoodadAnimHost>()
            .unwrap()
            .window_hi;
        for _ in 0..3 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_millis(1400));
            app.update();
        }
        let h = app.world().entity(host).get::<DoodadAnimHost>().unwrap();
        assert!(
            h.window_hi > first,
            "an undrawn host still opened new windows"
        );
        assert!(
            h.clip.is_some(),
            "and still has something for the resume to arm"
        );
        assert_eq!(
            app.world()
                .entity(host)
                .get::<AnimationPlayer>()
                .unwrap()
                .playing_animations()
                .count(),
            0,
            "but its stopped player was left alone"
        );
    }

    /// The draw gate stops the player + pauses the gseq drive when every submesh is hidden, and on
    /// re-appearing re-arms at the shared-clock position (`(now − armed_at) mod duration`) — the
    /// ref's clock-indexed pose, so pause history never desyncs phase.
    #[test]
    fn gate_pauses_hidden_and_resumes_on_the_shared_clock() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default())); // Time + asset stores
                                                                   // The gate reads the far-clip wall (0678) — this host is mesh-BACKED, so it takes the
                                                                   // `Visibility` branch and never consults it, but the system still needs the resource.
        app.init_resource::<crate::view::ViewDistance>();
        // …and the exterior-window gate's two terms (0786), for the same reason: this host is
        // mesh-BACKED so it never consults them, but the system's params must resolve. Their
        // defaults are the outdoor case (`Unrestricted`, no room claimed).
        app.init_resource::<crate::wmo_portal::ExteriorWindows>();
        app.init_resource::<crate::wmo_portal::CameraInteriorClaim>();
        // The lazy-rig promote's params (decision 0863) — this host carries no `LazyRig`, so
        // the wake edge never fires here, but the params must resolve.
        app.init_resource::<crate::rig_palette::RigPalettes>();
        app.init_asset::<SkinnedMeshInverseBindposes>();
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
                    armed_at: 0.0,
                    // Far future: this test exercises the DRAW gate, so no window may expire
                    // under it and change the armed clip mid-assertion.
                    window_hi: f32::INFINITY,
                    anim_id: Some(0),
                    active: true,
                    parked_at: 0.0,
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
