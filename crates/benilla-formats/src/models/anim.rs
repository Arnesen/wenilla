//! M2 skeleton + animation parsing (decision 0019): the rest skeleton (bones, parent + pivot) and
//! every sequence's per-bone keyframe tracks, which the skinned-entity path turns into Bevy clips.
//! Split out of the model parser as its own concern.

use std::io::Cursor;

use anyhow::Result;
use benilla_m2::parse_m2;

use super::{le_f32, le_u16, le_u32};

/// One bone of a model's rest skeleton (decision 0019): its parent bone index (`-1` = root) and pivot
/// point in **raw WoW model space** (the render boundary maps it to Bevy space). Vanilla M2 has no
/// inverse-bind-matrix array — the rest pose is identity TRS and the **pivot encodes bind position**
/// (VERIFIED wow-5875-re), so the skinned-entity path builds bone matrices pivot-relative up the chain.
#[derive(Debug, Clone, Copy)]
pub struct SkeletonBone {
    pub parent: i16,
    pub pivot: [f32; 3],
    /// `KeyBoneID` (`-1` = none): 0/1 arms L/R, 2/3 shoulders L/R, … — the per-arm animation masks
    /// find their subtree roots through it.
    pub key_bone: i16,
    /// The bone's billboard arm (flags `0x08/0x10/0x20/0x40`), `None` for an ordinary bone — the
    /// palette-level camera facing a rigged host applies per frame (children inherit it).
    pub billboard: Option<crate::BillboardKind>,
    /// Bone flags `0x1/0x2/0x4` — how this bone's effective **parent matrix** is rebuilt from the
    /// model's own root matrix before it composes (`None` for the ordinary bone). See
    /// [`crate::ParentArm`]: the byte-verified `ignore parent translate / scale / rotate` trio.
    /// Carried by 862 bones across 456 shipped models — the unskinned attach helpers
    /// (HandArrow/Bullet: the nocked arrow lies flat along the facing instead of twisting with the
    /// draw hand, wow-re `nocked-ammo-cancel.md` §E4) and, load-bearingly, **every vanilla player
    /// mount's rider seat**, which is why a galloping horse's spine never rocks its rider.
    pub parent_arm: Option<crate::ParentArm>,
}

/// A model's bone hierarchy — the rest skeleton the skinned-entity path turns into a joint-entity
/// tree + inverse bind poses. Bones are in M2 file order; a vertex's `joints` index into this list.
/// Empty for a boneless model.
#[derive(Debug, Clone, Default)]
pub struct Skeleton {
    pub bones: Vec<SkeletonBone>,
}

impl Skeleton {
    /// The **billboard host** of `bone`: the index of the nearest bone at or above it in the parent
    /// chain that authors a billboard arm (`bone` itself included), or `None` when the chain reaches
    /// the root without one.
    ///
    /// This is the palette-level question, and the reason it has to be asked about ancestors and not
    /// just the bone itself: a billboard bone's matrix ROWS are replaced with the camera basis about
    /// its own pivot, and *`children multiply onto this`* (wow-re `billboard-bone-law.md`) — so every
    /// descendant frame is camera-dependent even though only the ancestor carries the flag. Three
    /// consumers ask it: a skinned vertex (which arm its geometry inherits), a **particle emitter**
    /// (whose record position rides the replaced matrix — wow-re `part-anchoring-live-bone.md` §1
    /// row 3), and a ribbon anchor.
    ///
    /// The nearest host is the whole answer: a nearer replacement discards the rows an outer one
    /// wrote, and on a rest-pose chain the pivot-preserving translation rebuild reads the (identity)
    /// pre-billboard matrix, so nothing above it survives either.
    pub fn billboard_host(&self, bone: u16) -> Option<usize> {
        let mut i = usize::from(bone);
        // Bounded by the bone count — M2 parents precede children, so this terminates on any
        // well-formed file; the bound is the malformed-parent guard.
        for _ in 0..=self.bones.len() {
            let b = self.bones.get(i)?;
            if b.billboard.is_some() {
                return Some(i);
            }
            i = usize::try_from(b.parent).ok()?;
        }
        None
    }
}

/// Parse the M2 bone hierarchy (parent + pivot per bone) into a [`Skeleton`]. Straight off
/// `benilla-m2`'s bone parse (vanilla bone record stride 0x6c: parent i16 @+0x08, pivot C3 @+0x60,
/// VERIFIED wow-5875-re). A separate byte-in entry point alongside [`parse_m2_render_submeshes`], so
/// the asset loader builds the skeleton beside the meshes without changing that signature.
pub fn parse_m2_skeleton(bytes: &[u8]) -> Result<Skeleton> {
    let format =
        parse_m2(&mut Cursor::new(bytes)).map_err(|e| anyhow::anyhow!("parsing M2: {e}"))?;
    let bones = format
        .model()
        .bones
        .iter()
        .map(|b| SkeletonBone {
            parent: b.parent,
            pivot: [b.pivot.x, b.pivot.y, b.pivot.z],
            key_bone: b.key_bone,
            billboard: crate::BillboardKind::from_bone_flags(b.flags.bits()),
            parent_arm: crate::ParentArm::from_bone_flags(b.flags.bits()),
        })
        .collect();
    Ok(Skeleton { bones })
}

/// One M2 attachment-point record (decision 0072 — held items): the attach id (`0` shield, `1`
/// right hand, `2` left hand, sheath/hip/back pairs at `26..33`), the bone it rides, and its
/// **raw WoW model-space** position (see `benilla_m2::M2Attachment` — the same shape, re-exposed
/// here as the bytes-in entry point alongside [`parse_m2_skeleton`], so callers that only take
/// `benilla-formats` don't need a direct `benilla-m2` dependency).
#[derive(Debug, Clone, Copy)]
pub struct M2Attachment {
    pub id: u16,
    pub bone: u16,
    pub position: [f32; 3],
}

/// Parse the M2 attachment-point table **as the reference addresses it**: one entry per attach id
/// the model's AttachLookup resolves (`benilla_m2::M2Model::attachment` — the `0x710310` bounds +
/// `0xffff` sentinel), in id order, so a consumer keyed by id can simply match on
/// [`M2Attachment::id`].
///
/// This is deliberately **not** the raw file table: ~5% of weapon models author several records
/// under one id (both ends of a staff), and the lookup names exactly one of them — the other is
/// unreachable in the reference and must be unreachable here (decision 0805). Records the lookup
/// never names are therefore absent. Empty for a model with no attachment points.
pub fn parse_m2_attachments(bytes: &[u8]) -> Result<Vec<M2Attachment>> {
    let format =
        parse_m2(&mut Cursor::new(bytes)).map_err(|e| anyhow::anyhow!("parsing M2: {e}"))?;
    let model = format.model();
    Ok((0..model.attach_lookup.len())
        .filter_map(|id| {
            let a = model.attachment(id as u16)?;
            Some(M2Attachment {
                // The LOOKUP's id, not the record's own: the reference resolves `lookup[id]` and
                // never re-reads the record's id field, so that is the id this point answers to.
                // (They agree on every shipped model checked; this makes it true by construction.)
                id: id as u16,
                bone: a.bone,
                position: a.position,
            })
        })
        .collect())
}

/// One M2 animation-event **positional marker** (`benilla_m2::M2EventMarker` re-exposed as the
/// bytes-in entry point, like [`M2Attachment`]): the 4CC, the bone it rides, and its **raw WoW
/// model-space** position. The client resolves positions off this table by 4CC, first match
/// (`0x7130e0`/`0x7131b0`) — the cast-release launch points `$CSL`/`$CSR`/`$CST` (casting hand
/// left/right/two-hand, `0x60c9b0`'s cascade) and the ranged release `$BWR` ride it; the fire
/// *times* are the per-sequence [`AnimEvent`] stream.
#[derive(Debug, Clone, Copy)]
pub struct EventMarker {
    /// The identifier 4CC, stored forward (`*b"$CSL"`).
    pub ident: [u8; 4],
    pub bone: u16,
    pub position: [f32; 3],
}

/// Parse the M2 event table's positional markers (see [`EventMarker`]). Straight off
/// `benilla-m2`'s already bone-range-checked parse, file order preserved (queries take the first
/// ident match); empty for a model with no events.
pub fn parse_m2_event_markers(bytes: &[u8]) -> Result<Vec<EventMarker>> {
    let format =
        parse_m2(&mut Cursor::new(bytes)).map_err(|e| anyhow::anyhow!("parsing M2: {e}"))?;
    Ok(format
        .model()
        .event_markers
        .iter()
        .map(|m| EventMarker {
            ident: m.ident,
            bone: m.bone,
            position: m.position,
        })
        .collect())
}

/// A bow M2's **bowstring anchors** — the `$WTT` (top) / `$WTB` (bottom) EVENT records the
/// client's string drawer `0x611ff0` spans (wow-re `nocked-ammo-cancel.md` §G2: `0x7131b0` finds
/// each marker, transforms its local position by its bone's live matrix, and a 2-segment line
/// list runs top → middle → bottom). Each anchor is `(bone index, raw WoW model-space position)`
/// — the limb-tip helper bones (8/9 on the standard bows), which the bow's own BowPull/BowRelease
/// sequences bend. The markers are pure position records (no timestamps), which is why they never
/// surface in the per-sequence [`AnimEvent`] stream.
#[derive(Clone, Copy)]
pub struct StringAnchors {
    pub top: (u16, [f32; 3]),
    pub bottom: (u16, [f32; 3]),
}

/// Parse a model's `$WTT`/`$WTB` bowstring anchors from the raw M2 event table (stride 44:
/// identifier 4CC @+0, bone @+8, position C3 @+12 — the same record `parse_m2_animations` reads
/// timestamps from). `None` when either marker is absent (every non-bow model).
pub fn parse_m2_string_anchors(b: &[u8]) -> Option<StringAnchors> {
    let (ev_count, ev_ofs) = (le_u32(b, 0x114) as usize, le_u32(b, 0x118) as usize);
    let (mut top, mut bottom) = (None, None);
    for e in 0..ev_count {
        let erec = ev_ofs + e * 44;
        if erec + 44 > b.len() {
            break;
        }
        let ident = b.get(erec..erec + 4)?;
        let anchor = (
            le_u32(b, erec + 8) as u16,
            [
                le_f32(b, erec + 12),
                le_f32(b, erec + 16),
                le_f32(b, erec + 20),
            ],
        );
        match ident {
            b"$WTT" => top = Some(anchor),
            b"$WTB" => bottom = Some(anchor),
            _ => {}
        }
    }
    Some(StringAnchors {
        top: top?,
        bottom: bottom?,
    })
}

/// Parse a model's first `$CCH` marker — the fishing line's near anchor on the pole (wow-re
/// `fishing-line.md` §2: the line builder scans the M2 **events** table for `$CCH` and composes
/// its position through the bone's pose; the bobber authors one too but the reference never reads
/// it). Returns `(bone index, raw WoW model-space position)`; `None` for a model with no `$CCH`.
/// Exactly one weapon model in the 5875 chain authors it (`Misc_2H_FishingPole_A_01.m2` — the
/// `scan_events` sweep), which is what lets the held-item attach key on presence alone.
pub fn parse_m2_cch_marker(b: &[u8]) -> Option<(u16, [f32; 3])> {
    let (ev_count, ev_ofs) = (le_u32(b, 0x114) as usize, le_u32(b, 0x118) as usize);
    for e in 0..ev_count {
        let erec = ev_ofs + e * 44;
        if erec + 44 > b.len() {
            break;
        }
        if b.get(erec..erec + 4)? == b"$CCH" {
            return Some((
                le_u32(b, erec + 8) as u16,
                [
                    le_f32(b, erec + 12),
                    le_f32(b, erec + 16),
                    le_f32(b, erec + 20),
                ],
            ));
        }
    }
    None
}

/// One row of the M2's baked **PlayableAnimationLookup** table (decision 0082 — missing-animation-clip
/// resolution): re-exposed from `benilla-m2`'s [`benilla_m2::M2PlayableAnim`] (identical shape) as the
/// bytes-in entry point alongside [`parse_m2_skeleton`]/[`parse_m2_attachments`], so callers that only
/// take `benilla-formats` don't need a direct `benilla-m2` dependency.
#[derive(Debug, Clone, Copy)]
pub struct PlayableAnim {
    /// The `AnimationData.dbc` id this model actually plays for the row's requested id.
    pub resolved_id: u16,
    /// Direction/variant playback code — plumbed through, not yet applied (see
    /// [`benilla_m2::M2PlayableAnim::dir_flags`]).
    pub dir_flags: u16,
}

/// Parse the M2's [`PlayableAnim`] table (decision 0082, wow-re `anim-id-resolution.md`,
/// byte-verified `0x711bf0`): the model's own precomputed answer to "if the game requests
/// `AnimationData.dbc` id X, which id do I actually play, and in which direction/variant" — the
/// source `benilla-assets`' `ModelAnimations::resolve` PATH 1 reads. Straight off `benilla-m2`'s
/// header array (`count@+0x2c`/`ofs@+0x30`); empty for a model with no table (a boneless/malformed
/// M2), which the resolver then degrades to identity for.
pub fn parse_m2_playable_animation_lookup(bytes: &[u8]) -> Result<Vec<PlayableAnim>> {
    let format =
        parse_m2(&mut Cursor::new(bytes)).map_err(|e| anyhow::anyhow!("parsing M2: {e}"))?;
    Ok(format
        .model()
        .playable_animation_lookup
        .iter()
        .map(|p| PlayableAnim {
            resolved_id: p.resolved_id,
            dir_flags: p.dir_flags,
        })
        .collect())
}

/// Parse the M2's **AnimationLookup** table (header `count@+0x24`/`ofs@+0x28`) — `AnimationData.dbc`
/// id → this model's first sequence slot for it, `0xffff` where it authors none. The table behind the
/// reference's ownership predicate `0x711960`; see [`benilla_m2::M2Model::owns_animation`], which is
/// what callers should normally use rather than indexing this by hand.
pub fn parse_m2_animation_lookup(bytes: &[u8]) -> Result<Vec<u16>> {
    let format =
        parse_m2(&mut Cursor::new(bytes)).map_err(|e| anyhow::anyhow!("parsing M2: {e}"))?;
    Ok(format.model().animation_lookup.clone())
}

/// One bone's keyframe tracks for an animation sequence (decision 0019), in **raw WoW model space**:
/// translation/scale are `[f32;3]`, rotation is an uncompressed quaternion `[x,y,z,w]` (vanilla v256).
/// Times are **seconds**, rebased to the sequence start so each clip runs `0..duration`. Only the
/// channels the bone actually animates are non-empty; a bone absent from [`ModelAnimation::bones`]
/// holds its bind pose.
#[derive(Debug, Clone)]
pub struct BoneKeys {
    pub bone: u16,
    pub translation: Vec<(f32, [f32; 3])>,
    pub rotation: Vec<(f32, [f32; 4])>,
    pub scale: Vec<(f32, [f32; 3])>,
}

/// One bone channel driven by a **global sequence**: a free-running loop *independent* of the playing
/// animation, wrapped at `period_ms` off the model's own clock (VERIFIED wow-5875-re `animation.md` /
/// `doodad-anim-host.md`: global sequences loop with zero arming, clock = `[CM2Model+0x2c]+0xc` mod
/// `globalSequences[gseq]`). Keys are absolute ms within `[0, period_ms]`, ascending. The canonical
/// consumer is the character **eye blink** — an eyelid bone whose SCALE track holds `0` (lid retracted,
/// eye open) for most of the loop and pops to `1` (lid full-size, eye shut) for ~100 ms.
#[derive(Debug, Clone)]
pub struct GlobalSeqChannel<T> {
    pub period_ms: u32,
    pub keys: Vec<(u32, T)>,
}

/// A bone's global-sequence-driven channels (any of translation/rotation/scale; each carries its own
/// global sequence and thus its own period). Bones with no such channel are absent from the list. These
/// are exactly the tracks [`parse_m2_animations`] drops (it reads only the playing sequence's band); the
/// runtime samples these on the free clock and composes them over the sequence pose.
#[derive(Debug, Clone)]
pub struct GlobalSeqBone {
    pub bone: u16,
    pub translation: Option<GlobalSeqChannel<[f32; 3]>>,
    pub rotation: Option<GlobalSeqChannel<[f32; 4]>>,
    pub scale: Option<GlobalSeqChannel<[f32; 3]>>,
}

/// One animation **event keyframe**: a 4CC-tagged trigger on the sequence timeline (decision 0070
/// slice 3 — the anim-driven sound/effect surface). Sound-relevant tags: `$SND`/`$DSL`/`$DSO`
/// (play kit `data`), the footstep family (`$FL0..3`/`$FR`/`$RL`/`$RR`/`$SL`/`$SR`/`$BL`/`$BR` +
/// `$FSD`), `$CSS` weapon swing, `$HIT` impact, `$DTH` death thud, `$FD1..4` fidgets,
/// `$AH0..3` custom attacks, `$BTH` breath. Non-sound tags pass through untouched.
#[derive(Debug, Clone, Copy)]
pub struct AnimEvent {
    /// Seconds from the sequence start (same clock as the bone keys).
    pub time: f32,
    /// The identifier 4CC, stored forward (`*b"$FL0"`) — byte-verified on DireWolf.m2.
    pub ident: [u8; 4],
    /// The payload — a SoundEntries id for `$SND`/`$DSL`/`$DSO`, else 0.
    pub data: u32,
}

/// One animation sequence's per-bone keyframes (decision 0019): its `AnimationData.dbc` id, duration,
/// loop flag, and the bones that move. **Stand is `anim_id` 0** — the idle the real client arms by default
/// (VERIFIED wow-5875-re) — but its *record index* is not fixed: Stand is `animationLookup[0]`, which is
/// record 0 only for some models (a chicken's Stand is record 2, a horse's record 1; those models' record
/// 0 is a different sequence). Consumers select always by `anim_id`, never by record index; walk (4),
/// run (5), death, emotes, … are other ids.
#[derive(Debug, Clone)]
pub struct ModelAnimation {
    /// `AnimationData.dbc` id, the selection key (0 Stand, 4 Walk, 5 Run, 13 WalkBackwards, …).
    pub anim_id: u16,
    /// This sequence's **file slot** — its index in the M2's own sequence array, which is NOT this
    /// list's index (zero-duration sequences are dropped below). Load-bearing because the slot is
    /// what indexes every track's per-sequence key ranges: the material-alpha bake keys its
    /// per-sequence loops by it (`mat_anim::AlphaAnim::seq`), so a consumer that knows which clip
    /// is playing can ask for that clip's authored batch visibility.
    pub seq_index: usize,
    /// The sequence's **absolute time band** on the model's global keyframe timeline
    /// (`M2Sequence` start @+0x04 / end @+0x08, ms) — the window every non-global-sequence track's
    /// keys are selected from and rebased against ([`read_bone_track`], `key_anim::bake_track`).
    /// Exposed because "which keys does this sequence actually see?" is unanswerable without it: a
    /// track whose keys all sit in *other* sequences' bands holds a clamped value here, and which
    /// value that is depends on where this band falls among them.
    pub start_ms: u32,
    pub end_ms: u32,
    pub duration: f32,
    pub looping: bool,
    /// The sequence's authored **design movement speed** (`M2Sequence.moveSpeed` @+0x0c, yd/s) — the
    /// speed the locomotion was animated for. The real client scales a locomotion clip's playback rate
    /// by `unitSpeed / (moveSpeed · modelScale)` (VERIFIED wow-5875-re `0x5fe2f0`: a §5-converged pair),
    /// so a unit moving faster than the design speed cycles its legs proportionally faster — without it
    /// a backpedal (slow design speed) played at 1× looks far too slow. `0.0` for a non-locomotion
    /// sequence (idle/emote), which the selector leaves at rate 1×.
    pub move_speed: f32,
    /// The sequence's **blend-in time** (`M2Sequence.blendTime` @+0x20, **seconds**) — how long the real
    /// client cross-fades *into* this animation from the previous pose (VERIFIED wow-5875-re: op4 snapshots
    /// the live pose `+0x98→+0xc4` and decays it over the blend-in, `rf29-playback-setters.md`). The spawn
    /// site uses it as the cross-fade duration so a gait change eases instead of snapping (Walk/Run 0.25 s,
    /// Stand 0.5 s, jump/sit transitions 0.15 s). `0.0` ⇒ an instant cut.
    pub blend_time: f32,
    /// The sequence's bounds-sphere **centre** — the `M2Sequence` CAaBox (min @+0x24, max @+0x30) centre,
    /// **raw WoW model space**. The real client's mouse-pick broad phase tests the cursor ray against
    /// exactly this sphere for the unit's *current* animation — world-placed + scaled, **no pad**
    /// (VERIFIED wow-5875-re `0x7089c0`, the pick-volume RE) — before the per-triangle posed-mesh test.
    pub bounds_center: [f32; 3],
    /// The sequence's bounds-sphere **radius** (`M2Sequence` @+0x3c, model-local yards). `0.0` ⇒
    /// unauthored (the real client falls back to the header sphere).
    pub bounds_radius: f32,
    /// The sequence's CAaBox **min corner** (`M2Sequence` @+0x24), raw WoW model space — the box
    /// [`Self::bounds_center`] is the centre of. The unit **blob shadow** sizes its projection box
    /// from exactly this box for the *current* animation, clamped into ±5 per axis (VERIFIED
    /// wow-5875-re `unit-blob-shadow.md`: `0x711a20` → clamp `0x6992c0`/`0x699250`).
    pub bounds_min: [f32; 3],
    /// The sequence's CAaBox **max corner** (`M2Sequence` @+0x30). See [`Self::bounds_min`].
    pub bounds_max: [f32; 3],
    /// The sequence's **variation frequency** (`M2Sequence.frequency` @+0x14) — this variation's
    /// weight in the client's per-play roll (VERIFIED wow-5875-re `anim-id-resolution.md`: op4
    /// with variationIdx −1 rolls `_rand()` (0..0x7fff) and walks the id's variation chain —
    /// `roll < frequency` picks, else `roll -= frequency` and advance).
    pub frequency: u16,
    /// The sequence's **replay range** (`M2Sequence` minReplay @+0x18 / maxReplay @+0x1c) — the
    /// client rolls a play count `R = max(1, min + ⌊rand·(max−min)/32768⌋)` at every arm (the
    /// second `_rand` site in op4, `0x712692..0x7126cd`) and **multiplies it into the play
    /// window** (`0x7126d8`): a clamp-flag one-shot runs its timeline `R` times before freezing;
    /// a loop-flag sequence ignores it (VERIFIED wow-5875-re `loop-replay-fidget.md`). `(0, 0)`
    /// (the overwhelming majority) rolls to `R = 1`.
    pub min_replay: u32,
    pub max_replay: u32,
    pub bones: Vec<BoneKeys>,
    /// Event keyframes in this sequence's window, rebased like the bone keys (seconds from start).
    pub events: Vec<AnimEvent>,
}

impl ModelAnimation {
    /// Does this sequence leave **every** bone at bind pose for its whole band — one key per
    /// channel, and that key the identity (zero translation, unit-w rotation, unit scale)?
    ///
    /// The **render** content gate (decision 0130, consumed by `benilla_assets`' loader as
    /// `ModelAnimations::first_seq`): looping such a sequence draws exactly the static mesh, so the
    /// placed-doodad tier skips building a skin + `AnimationPlayer` for it — the ~90 % of placed
    /// doodads `doodadscan` measured. It lives here, beside the parse, so the `benilla-extract`
    /// census of what that gate costs the corpus (`idleslotscan`) asks the renderer's own question
    /// instead of a hand-copied twin that can drift from it.
    ///
    /// It answers a question about **pixels only**, and is never a reason to treat the sequence as
    /// absent: a consumer keyed on the sequence *identity* — the per-sequence emission, parameter
    /// and material-alpha bakes — still needs the slot the instance is playing, and the reference
    /// always has one armed (decision 0936, `ModelAnimations::idle_seq`).
    pub fn is_rest_pose(&self) -> bool {
        const EPS: f32 = 1e-4;
        !self.bones.iter().any(|b| {
            b.translation.len() > 1
                || b.rotation.len() > 1
                || b.scale.len() > 1
                || b.translation
                    .iter()
                    .any(|(_, v)| v.iter().any(|c| c.abs() > EPS))
                || b.rotation
                    .iter()
                    .any(|(_, q)| (q[3].abs() - 1.0).abs() > EPS)
                || b.scale
                    .iter()
                    .any(|(_, s)| s.iter().any(|c| (c - 1.0).abs() > EPS))
        })
    }
}

/// One bone-channel `M2Track`, read **once per model** rather than once per sequence: the v256 track
/// (0x1c) is `interp_type`@0, `global_seq`@2, then three `M2Array`s — interpolation_ranges@0x04,
/// **timestamps@0x0c**, **values@0x14** (each `{count u32, offset u32}`). Every sequence carves its
/// own band out of this one shared key list ([`ChannelTrack::band`]), so re-reading the arrays per
/// sequence was pure waste — HumanMale walked 143 sequences × 119 bones × 3 channels of headers to
/// build 357 tracks' worth of keys.
struct ChannelTrack<T> {
    /// `interp_type == 0`: hold each key until the next, no interpolation (the samplers' shared
    /// `cmp word[track],0` dispatch — wow-re `eval.md` FN2/FN6).
    step: bool,
    /// Global-sequence id, `0xffff` for an ordinary sequence-timeline track.
    gseq: u16,
    /// The per-sequence **key-index window** `(lo, hi)`, indexed by the sequence's FILE slot — the
    /// array the reference's key search selects before it looks at a single timestamp (VERIFIED
    /// wow-re `eval.md` FN1 `0x713d50` §1). Empty ⇒ the reference's own `[track+4]==0` fallback,
    /// "search the whole key list".
    ranges: Vec<(u32, u32)>,
    /// `(absolute ms, value)` keys, file order — every sequence's keys concatenated on one
    /// timeline (timestamps absolute, VERIFIED).
    keys: Vec<(u32, T)>,
}

/// One bone's three channel tracks, in the bone record's own order: translation `+0x0c`, rotation
/// `+0x28`, scale `+0x44`.
type BoneChannels = (
    ChannelTrack<[f32; 3]>,
    ChannelTrack<[f32; 4]>,
    ChannelTrack<[f32; 3]>,
);

/// Read one bone channel's `M2Track` whole (see [`ChannelTrack`]). `read_value` decodes one value at
/// a byte offset; `stride` is its size. Out-of-range reads truncate the key list — real vanilla art
/// relies on that tolerance.
fn read_channel_track<T>(
    b: &[u8],
    track: usize,
    stride: usize,
    read_value: impl Fn(&[u8], usize) -> T,
) -> ChannelTrack<T> {
    if track + 0x1c > b.len() {
        return ChannelTrack {
            step: false,
            gseq: 0xffff,
            ranges: Vec::new(),
            keys: Vec::new(),
        };
    }
    let (rn, ro) = (
        le_u32(b, track + 0x04) as usize,
        le_u32(b, track + 0x08) as usize,
    );
    let (tn, to) = (
        le_u32(b, track + 0x0c) as usize,
        le_u32(b, track + 0x10) as usize,
    );
    let vo = le_u32(b, track + 0x18) as usize;
    let ranges = (0..rn)
        .map_while(|i| {
            let e = ro + i * 8;
            (e + 8 <= b.len()).then(|| (le_u32(b, e), le_u32(b, e + 4)))
        })
        .collect();
    let keys = (0..tn)
        .map_while(|k| {
            let (t_off, v_off) = (to + k * 4, vo + k * stride);
            (t_off + 4 <= b.len() && v_off + stride <= b.len())
                .then(|| (le_u32(b, t_off), read_value(b, v_off)))
        })
        .collect();
    ChannelTrack {
        step: le_u16(b, track) == 0,
        gseq: le_u16(b, track + 0x02),
        ranges,
        keys,
    }
}

impl<T: super::key_anim::Lerp + PartialEq> ChannelTrack<T> {
    /// This channel's keyframes for the sequence at file slot `slot`, spanning
    /// `[start, end]` (absolute ms), rebased to seconds from the band start.
    ///
    /// In-band keys are selected by **absolute timestamp**, which is what the reference's search
    /// resolves to: its window is `ranges[slot]`, and that window brackets the band's own keys in
    /// every model of the 1.12.1 corpus (`benilla-extract bonescan`: 0 violations in 516 633 keyed
    /// band-slots — the check that makes reading the window safe at all). Selecting *the window's
    /// keys* as in-clip keys is the thing that never worked: `hi` routinely points at a key in a
    /// later band 14–64 s away, which froze Chicken/Bear/Kobold at a garbage pose. It is a
    /// **bracket**, not a key set.
    ///
    /// Its load-bearing use is the band **edges**, where the reference keeps sampling and a
    /// truncated key list does not (wow-re `eval.md` FN1 `0x713d50`):
    ///
    /// - a band with **no keys of its own** still has an authored pose — the window resolves it,
    ///   either outright (`lo >= hi` ⇒ `keys[lo]`, the degenerate `{lo, lo, 0}` result) or as the
    ///   lerp across the bracketing pair. Leaving the track empty instead froze the bone at
    ///   whatever the *previous* clip left it (decision 0133's tilt: a Stand-variation waist twist
    ///   rode through the entire run);
    /// - past the band's last key the reference keeps ramping toward `k1 = k0 + 1`, which is
    ///   bounded by the TOTAL key count and not by the window — so a key belonging to a later
    ///   sequence is a live lerp endpoint.
    ///
    /// A head/tail sample is emitted only when it differs from the edge key it would otherwise
    /// hold, so the clips are unchanged wherever the reference and a plain hold agree — which,
    /// measured, is everywhere but `Creature\Zombie`'s root translation (decision 0643).
    ///
    /// A **global-sequence** channel (`gseq != 0xffff`) runs on its own clock, not this band: a
    /// lone key is a pure CONSTANT folded into every clip (how vanilla authors the stowed-weapon
    /// attach-bone orientations — HumanMale bones 29/30/58–62), and a multi-key one belongs to
    /// [`parse_m2_global_sequence_bones`] and the `GlobalSeqDrive` lane, so it is left out here.
    fn band(&self, slot: usize, start: u32, end: u32) -> Vec<(f32, T)> {
        if self.keys.is_empty() {
            return Vec::new();
        }
        if self.gseq != 0xffff {
            return match self.keys.len() {
                1 => vec![(0.0, self.keys[0].1)],
                _ => Vec::new(),
            };
        }
        let last = self.keys.len() - 1;
        let (lo, hi) = self
            .ranges
            .get(slot)
            .map_or((0, last), |&(lo, hi)| (lo as usize, hi as usize));
        let at = |t| super::key_anim::sample_window(&self.keys, self.step, lo, hi, t);
        let rebase = |ts: u32| (ts.saturating_sub(start)) as f32 / 1000.0;
        let in_band: Vec<(u32, T)> = self
            .keys
            .iter()
            .copied()
            .filter(|&(ts, _)| ts >= start && ts <= end)
            .collect();
        let Some(&(first_ms, first_v)) = in_band.first() else {
            // An empty band: the window's value, held across it — two keys only when the bracket
            // lerp actually moves within the band.
            let head = match at(start) {
                Some(v) => v,
                None => return Vec::new(),
            };
            return match at(end) {
                Some(tail) if tail != head => vec![(0.0, head), (rebase(end), tail)],
                _ => vec![(0.0, head)],
            };
        };
        let (last_ms, last_v) = in_band[in_band.len() - 1];
        let mut out = Vec::with_capacity(in_band.len() + 2);
        if first_ms > start && at(start).is_some_and(|h| h != first_v) {
            out.push((0.0, at(start).unwrap()));
        }
        out.extend(in_band.iter().map(|&(ts, v)| (rebase(ts), v)));
        if last_ms < end && at(end).is_some_and(|t| t != last_v) {
            out.push((rebase(end), at(end).unwrap()));
        }
        out
    }
}

/// The weapon-grip finger poses: each requested `bone`'s rotation **as the reference samples it at
/// the HandsClosed frame** (`AnimationData` id 15), through the same window kernel every other
/// channel uses ([`ChannelTrack::band`]).
///
/// This exists as its own entry point because the grip is not a *clip* — it is a pose lifted out of
/// one frame and worn as a masked overlay over whatever animation is playing, so the weapon hand
/// stays closed while the body runs (wow-re `hand-grip-mechanism.md`). HandsClosed is a 33 ms band
/// that keys nothing of its own on the finger bones (HumanMale bone 102 is keyed at 43333/70000 ms,
/// both curled, not the 60000 ms frame) — its `interpolation_ranges` pair brackets it, which is
/// exactly the case the window rule resolves.
///
/// Returns `(bone, quat[x,y,z,w])` for the bones that carry a plain rotation track; empty if the
/// model has no HandsClosed sequence.
pub fn hand_grip_finger_poses(bytes: &[u8], bones: &[u16]) -> Vec<(u16, [f32; 4])> {
    let b = bytes;
    if b.len() < 0x40 || &b[0..4] != b"MD20" {
        return Vec::new();
    }
    let (seq_count, seq_ofs) = (le_u32(b, 0x1c) as usize, le_u32(b, 0x20) as usize);
    let (bone_count, bone_ofs) = (le_u32(b, 0x34) as usize, le_u32(b, 0x38) as usize);
    // HandsClosed (anim id 15) → its FILE slot (what indexes the key windows) and frame start ms.
    let mut hands_closed = None;
    for s in 0..seq_count {
        let rec = seq_ofs + s * 0x44;
        if rec + 0x44 > b.len() {
            break;
        }
        if le_u16(b, rec) == 15 {
            hands_closed = Some((s, le_u32(b, rec + 0x04)));
            break;
        }
    }
    let Some((slot, frame)) = hands_closed else {
        return Vec::new();
    };
    let quat = |b: &[u8], o: usize| {
        [
            le_f32(b, o),
            le_f32(b, o + 4),
            le_f32(b, o + 8),
            le_f32(b, o + 12),
        ]
    };
    let mut out = Vec::new();
    for &bone in bones {
        let bi = bone as usize;
        if bi >= bone_count || bone_ofs + bi * 0x6c + 0x6c > b.len() {
            continue;
        }
        // The bone's rotation M2Track (+0x28 in the 0x6c record), stride 16 (a 4×f32 quaternion).
        let tr = read_channel_track(b, bone_ofs + bi * 0x6c + 0x28, 16, quat);
        if tr.gseq != 0xffff || tr.keys.is_empty() {
            continue; // a global-sequence track runs on its own clock — not this
        }
        let last = tr.keys.len() - 1;
        let (lo, hi) = tr
            .ranges
            .get(slot)
            .map_or((0, last), |&(lo, hi)| (lo as usize, hi as usize));
        let Some(v) = super::key_anim::sample_window(&tr.keys, tr.step, lo, hi, frame) else {
            continue;
        };
        out.push((bone, v));
    }
    out
}

/// Parse **all** of a model's animation sequences into per-bone keyframes (decision 0019). Offsets
/// VERIFIED against wow-5875-re: sequences @MD20 `0x1c`/`0x20` (stride 0x44; id@+0x00, start@+0x04,
/// end@+0x08, flags@+0x10 with **bit0 SET ⇒ clamp/one-shot, CLEAR ⇒ loop**); bones @`0x34`/`0x38`
/// (stride 0x6c; the three `M2Track`s at +0x0c/+0x28/+0x44). Empty for a model with no sequences;
/// degenerate (zero-length) sequences are skipped.
///
/// Cost note: each sequence scans every bone track's full timestamp array (the time-band select), so
/// this is `O(sequences · bones · keys)` — fine at async load for the bounded vanilla rigs, a candidate
/// for a one-pass per-sequence key index if a packed-mob load ever shows up hot.
pub fn parse_m2_animations(b: &[u8]) -> Vec<ModelAnimation> {
    if b.len() < 0x40 || &b[0..4] != b"MD20" {
        return Vec::new();
    }
    let (seq_count, seq_ofs) = (le_u32(b, 0x1c) as usize, le_u32(b, 0x20) as usize);
    let (bone_count, bone_ofs) = (le_u32(b, 0x34) as usize, le_u32(b, 0x38) as usize);
    let vec3 = |b: &[u8], o: usize| [le_f32(b, o), le_f32(b, o + 4), le_f32(b, o + 8)];
    let quat = |b: &[u8], o: usize| {
        [
            le_f32(b, o),
            le_f32(b, o + 4),
            le_f32(b, o + 8),
            le_f32(b, o + 12),
        ]
    };
    // The model's event track (events @MD20 0x114/0x118 pre-Wrath, stride 44: identifier 4CC @+0,
    // data @+4, bone @+8, position @+12, M2TrackBase @+24 whose timestamp M2Array sits @+0x0c —
    // BYTE-VERIFIED on DireWolf.m2: 19 events, forward-stored tags `$CSS`/`$DTH`/`$FL0`…, absolute
    // global-timeline ms). Read once here; each sequence below selects + rebases its window,
    // exactly like the bone tracks.
    let (ev_count, ev_ofs) = (le_u32(b, 0x114) as usize, le_u32(b, 0x118) as usize);
    let mut model_events: Vec<([u8; 4], u32, Vec<u32>)> = Vec::with_capacity(ev_count);
    for e in 0..ev_count {
        let erec = ev_ofs + e * 44;
        if erec + 44 > b.len() {
            break;
        }
        let ident: [u8; 4] = match b.get(erec..erec + 4).and_then(|s| s.try_into().ok()) {
            Some(i) => i,
            None => break,
        };
        let data = le_u32(b, erec + 4);
        let (nts, ots) = (le_u32(b, erec + 36) as usize, le_u32(b, erec + 40) as usize);
        let mut times = Vec::with_capacity(nts);
        for t in 0..nts {
            let o = ots + t * 4;
            if o + 4 > b.len() {
                break;
            }
            times.push(le_u32(b, o));
        }
        model_events.push((ident, data, times));
    }

    // Every bone's three channel tracks, read ONCE: the key arrays are shared by every sequence,
    // which only slices its own band out of them (`ChannelTrack::band`).
    let bone_tracks: Vec<BoneChannels> = (0..bone_count)
        .map_while(|i| {
            let brec = bone_ofs + i * 0x6c;
            (brec + 0x60 <= b.len()).then(|| {
                (
                    read_channel_track(b, brec + 0x0c, 12, vec3),
                    read_channel_track(b, brec + 0x28, 16, quat),
                    read_channel_track(b, brec + 0x44, 12, vec3),
                )
            })
        })
        .collect();

    let mut out = Vec::new();
    for s in 0..seq_count {
        let rec = seq_ofs + s * 0x44;
        if rec + 0x44 > b.len() {
            break;
        }
        let anim_id = le_u16(b, rec);
        let seq_index = s;
        let (start, end, flags) = (
            le_u32(b, rec + 0x04),
            le_u32(b, rec + 0x08),
            le_u32(b, rec + 0x10),
        );
        // M2Sequence.moveSpeed @+0x0c (f32, yd/s): the locomotion design speed the playback-rate scaler
        // divides the unit's live speed by (VERIFIED wow-5875-re `0x711a20`/`0x5fe2f0`). `0.0` for a
        // non-locomotion sequence.
        let move_speed = le_f32(b, rec + 0x0c);
        // M2Sequence.blendTime @+0x20 (u32, ms → seconds): the cross-fade-in duration into this sequence.
        let blend_time = le_u32(b, rec + 0x20) as f32 / 1000.0;
        // M2Sequence bounds: CAaBox min @+0x24 / max @+0x30, sphere radius @+0x3c — the mouse-pick
        // broad-phase sphere for the model's current animation (wow-re pick-volume RE, `0x7089c0`).
        let (bmin, bmax) = (vec3(b, rec + 0x24), vec3(b, rec + 0x30));
        let bounds_center = [
            (bmin[0] + bmax[0]) * 0.5,
            (bmin[1] + bmax[1]) * 0.5,
            (bmin[2] + bmax[2]) * 0.5,
        ];
        let bounds_radius = le_f32(b, rec + 0x3c);
        // M2Sequence.frequency @+0x14: this variation's weight in the per-play roll.
        let frequency = le_u16(b, rec + 0x14);
        // M2Sequence minReplay/maxReplay @+0x18/+0x1c: the play-count roll's range.
        let (min_replay, max_replay) = (le_u32(b, rec + 0x18), le_u32(b, rec + 0x1c));
        let duration = end.saturating_sub(start) as f32 / 1000.0;
        if duration <= 0.0 {
            continue;
        }
        let looping = flags & 1 == 0; // VERIFIED polarity: bit0 clear loops, set clamps
        let mut bones = Vec::new();
        for (i, tracks) in bone_tracks.iter().enumerate() {
            let (tr, rot, sc) = tracks;
            let translation = tr.band(seq_index, start, end);
            let rotation = rot.band(seq_index, start, end);
            let scale = sc.band(seq_index, start, end);
            if !(translation.is_empty() && rotation.is_empty() && scale.is_empty()) {
                bones.push(BoneKeys {
                    bone: i as u16,
                    translation,
                    rotation,
                    scale,
                });
            }
        }
        // This sequence's event keys: the same time-band select + rebase as the bone tracks.
        let mut events = Vec::new();
        for (ident, data, times) in &model_events {
            for &ts in times {
                if ts >= start && ts <= end {
                    events.push(AnimEvent {
                        time: (ts - start) as f32 / 1000.0,
                        ident: *ident,
                        data: *data,
                    });
                }
            }
        }
        events.sort_by(|a, b| a.time.total_cmp(&b.time));

        out.push(ModelAnimation {
            anim_id,
            seq_index,
            start_ms: start,
            end_ms: end,
            duration,
            looping,
            move_speed,
            blend_time,
            bounds_center,
            bounds_radius,
            bounds_min: bmin,
            bounds_max: bmax,
            frequency,
            min_replay,
            max_replay,
            bones,
            events,
        });
    }
    out
}

/// Read one bone track (`{interp, gseq, …, key{n,ofs}, val{n,ofs}}`) as a **global-sequence** channel —
/// `None` unless it is tagged to a global sequence (`gseq != 0xffff`) with a non-zero period and >1 key.
/// The single-key global track is the CONSTANT case (stowed-weapon rest quats) [`read_bone_track`]
/// already folds into every clip; it is not a loop and is excluded here.
fn read_global_channel<T>(
    b: &[u8],
    track: usize,
    stride: usize,
    period_of: &impl Fn(u16) -> Option<u32>,
    read_value: impl Fn(&[u8], usize) -> T,
) -> Option<GlobalSeqChannel<T>> {
    if track + 0x1c > b.len() {
        return None;
    }
    let gseq = le_u16(b, track + 0x02);
    if gseq == 0xffff {
        return None;
    }
    let period_ms = period_of(gseq)?;
    let n = le_u32(b, track + 0x0c) as usize;
    let ts_o = le_u32(b, track + 0x10) as usize;
    let val_o = le_u32(b, track + 0x18) as usize;
    if n <= 1 {
        return None; // a lone key is a constant channel, not a loop
    }
    let mut keys = Vec::with_capacity(n);
    for k in 0..n {
        let (t_off, v_off) = (ts_o + k * 4, val_o + k * stride);
        if t_off + 4 > b.len() || v_off + stride > b.len() {
            break;
        }
        keys.push((le_u32(b, t_off), read_value(b, v_off)));
    }
    (keys.len() > 1).then_some(GlobalSeqChannel { period_ms, keys })
}

/// Parse every bone's **global-sequence** animation channels — the free-clock loops
/// [`parse_m2_animations`] deliberately drops (it reads only the *playing* sequence's time band).
/// A channel qualifies when its M2Track is tagged to a global sequence (`gseq != 0xffff`) with a
/// non-zero period and more than one key. The canonical consumer is the character eye-blink (an eyelid
/// bone's looping SCALE); resting fidget pulses ride the same mechanism. Global-sequence array at MD20
/// `0x14/0x18` (VERIFIED), bones at `0x34/0x38` (stride `0x6c`), tracks translation `+0x0c` / rotation
/// `+0x28` / scale `+0x44` (the shared v256 `M2Track`).
pub fn parse_m2_global_sequence_bones(b: &[u8]) -> Vec<GlobalSeqBone> {
    if b.len() < 0x40 || &b[0..4] != b"MD20" {
        return Vec::new();
    }
    let (gseq_count, gseq_ofs) = (le_u32(b, 0x14) as usize, le_u32(b, 0x18) as usize);
    let period_of = |gseq: u16| -> Option<u32> {
        let i = gseq as usize;
        if i >= gseq_count {
            return None;
        }
        let o = gseq_ofs.checked_add(i * 4)?;
        if o + 4 > b.len() {
            return None;
        }
        let d = le_u32(b, o);
        (d > 0).then_some(d)
    };
    let vec3 = |b: &[u8], o: usize| [le_f32(b, o), le_f32(b, o + 4), le_f32(b, o + 8)];
    let quat = |b: &[u8], o: usize| {
        [
            le_f32(b, o),
            le_f32(b, o + 4),
            le_f32(b, o + 8),
            le_f32(b, o + 12),
        ]
    };
    let (bone_count, bone_ofs) = (le_u32(b, 0x34) as usize, le_u32(b, 0x38) as usize);
    let mut out = Vec::new();
    for i in 0..bone_count {
        let brec = bone_ofs + i * 0x6c;
        if brec + 0x60 > b.len() {
            break;
        }
        let translation = read_global_channel(b, brec + 0x0c, 12, &period_of, vec3);
        let rotation = read_global_channel(b, brec + 0x28, 16, &period_of, quat);
        let scale = read_global_channel(b, brec + 0x44, 12, &period_of, vec3);
        if translation.is_some() || rotation.is_some() || scale.is_some() {
            out.push(GlobalSeqBone {
                bone: i as u16,
                translation,
                rotation,
                scale,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo root's `WoW/Data` (gitignored; the real-data tests skip when absent).
    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// The character eye-blink, straight off the real `HumanMale.m2`: exactly one global-sequence bone
    /// (75, the eyelid), a **scale** channel on a real global sequence, whose keys hold `0` (lid gone,
    /// eye open) at the loop start and pop to `1` (lid full, eye shut) ~33 ms later — the blink. Guards
    /// the header offsets (global sequences `0x14/0x18`, bones `0x34/0x38`, scale track `+0x44`) and the
    /// gseq-vs-in-band split against a silent regression. Skips when the client data isn't present.
    #[test]
    fn human_male_eyelid_blink_global_sequence() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Character\\Human\\Male\\HumanMale.m2")
            .expect("read HumanMale.m2");
        let gs = parse_m2_global_sequence_bones(&bytes);

        assert_eq!(
            gs.len(),
            1,
            "HumanMale has one global-seq bone (the eyelid)"
        );
        let eyelid = &gs[0];
        assert_eq!(eyelid.bone, 75);
        assert!(
            eyelid.translation.is_none() && eyelid.rotation.is_none(),
            "the eyelid blink is scale-only"
        );
        let scale = eyelid.scale.as_ref().expect("eyelid has a scale channel");
        assert!(
            scale.period_ms > 1000,
            "a real multi-second loop, not the empty 0/1 ms table (got {} ms)",
            scale.period_ms
        );
        assert!(scale.keys.len() >= 4);
        assert_eq!(scale.keys[0].0, 0);
        assert!(
            scale.keys[0].1.iter().all(|&c| c.abs() < 1e-3),
            "loop starts eye-open (scale 0), got {:?}",
            scale.keys[0].1
        );
        assert!(
            scale
                .keys
                .iter()
                .any(|(_, v)| v.iter().all(|&c| (c - 1.0).abs() < 1e-3)),
            "the loop has a shut frame (scale 1) — the blink"
        );
    }

    /// An empty band still poses the bone instead of dropping the track (decision 0133's tilt):
    /// HumanMale bone 27 (waist) rot has 4 keys, all outside Run's band, and `ranges[Run] = (2, 3)`
    /// brackets it — so the clip must carry the bracketed value as a constant, or a Stand-variation
    /// twist rides through the entire run. And the guarantee that makes it total: on every sequence
    /// of the model, any bone keyed *anywhere* stays keyed.
    #[test]
    fn empty_band_poses_the_bone_and_never_drops_it() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Character\\Human\\Male\\HumanMale.m2")
            .expect("read HumanMale.m2");
        let seqs = parse_m2_animations(&bytes);

        // Run (anim 5): bone 27's rotation must be present as a lone constant key.
        let run = seqs
            .iter()
            .find(|s| s.anim_id == 5)
            .expect("HumanMale has Run");
        let waist = run
            .bones
            .iter()
            .find(|b| b.bone == 27)
            .expect("bone 27 carried into the Run clip");
        assert_eq!(
            waist.rotation.len(),
            1,
            "an empty band emits exactly the one clamp key"
        );
        assert_eq!(waist.rotation[0].0, 0.0);

        // Totality: a bone keyed in ANY sequence is keyed in EVERY sequence (per channel family).
        let mut keyed = [
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        ];
        for s in &seqs {
            for b in &s.bones {
                if !b.translation.is_empty() {
                    keyed[0].insert(b.bone);
                }
                if !b.rotation.is_empty() {
                    keyed[1].insert(b.bone);
                }
            }
        }
        for (i, name) in ["translation", "rotation"].iter().enumerate() {
            for s in &seqs {
                for &bone in &keyed[i] {
                    let present = s.bones.iter().any(|b| {
                        b.bone == bone
                            && if i == 0 {
                                !b.translation.is_empty()
                            } else {
                                !b.rotation.is_empty()
                            }
                    });
                    assert!(
                        present,
                        "seq idx anim {} drops bone {bone}'s {name} — a stale-pose freeze",
                        s.anim_id
                    );
                }
            }
        }
    }

    /// The one place in the whole 1.12.1 corpus where the file's key window and decision 0133's
    /// nearest-key approximation disagree (`benilla-extract bonescan`: 9 of 210 368 empty band-slots,
    /// all three `Creature\Zombie` models) — and it disagrees for a reason worth pinning.
    ///
    /// `Zombie.m2` bone 0 (the root) translation is a **step** track (`interp == 0`) with two keys:
    /// `3333 = (0,0,0)` and `30000 = (0.0047, 0, 0)`. Walk's band `[26667, 27333]` keys nothing and
    /// sits nearer the *second* key in time, so "nearest authored key" hands back a value the
    /// reference has not stepped to yet — a key from the future. The reference searches `ranges[slot]
    /// = (0, 1)`, takes `k0` = the last key at or before the time (key 0), and — being a step track —
    /// copies it. Small in yards, wrong in direction, and only the file's own window gets it right.
    #[test]
    fn empty_band_takes_the_window_key_not_a_later_one() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Creature\\Zombie\\Zombie.m2")
            .expect("read Zombie.m2");
        let seqs = parse_m2_animations(&bytes);
        for anim in [4, 5] {
            let s = seqs
                .iter()
                .find(|s| s.anim_id == anim)
                .unwrap_or_else(|| panic!("Zombie has anim {anim}"));
            let root = s
                .bones
                .iter()
                .find(|b| b.bone == 0)
                .expect("the root is carried into the clip");
            assert_eq!(
                root.translation.len(),
                1,
                "a step track's empty band holds one value across the band"
            );
            assert_eq!(
                root.translation[0].1,
                [0.0, 0.0, 0.0],
                "anim {anim} must hold key 0, not step early to the 0.0047 key at 30000 ms"
            );
        }
    }

    /// The weapon grip, which now reads through the shared window kernel rather than its own key
    /// scan: HandsClosed on `HumanMale.m2` is the 33 ms band `[60000, 60033]`, which keys nothing on
    /// finger bone 102 — its window brackets it with the keys at 43333/70000 ms, both the same
    /// curled quaternion, so the pose the overlay wears is that curl and not the rest pose. The
    /// substitution is a measured no-op: across the eight playable-race models with a HandsClosed
    /// sequence, all 460 finger-capable rotation tracks give the same quaternion under the old
    /// at-or-before clamp and under the window (decision 0643).
    #[test]
    fn hand_grip_reads_the_curled_pose_through_the_window() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Character\\Human\\Male\\HumanMale.m2")
            .expect("read HumanMale.m2");
        let poses = hand_grip_finger_poses(&bytes, &[102]);
        let (bone, q) = *poses.first().expect("bone 102 carries a grip pose");
        assert_eq!(bone, 102);
        let want = [0.2536, -0.016, 0.0577, 0.9654];
        for (i, (&got, &w)) in q.iter().zip(want.iter()).enumerate() {
            assert!(
                (got - w).abs() < 1e-3,
                "grip quat component {i}: got {got}, want ~{w} (the 43333/70000 ms curl)"
            );
        }
    }

    /// The invariant that makes reading `interpolation_ranges` safe at all, pinned on the model
    /// decision 0133 named as the counter-example (`Bear`, which froze at a garbage pose when an
    /// earlier parse treated the window's far bracket as a *playable in-clip key* — it is a
    /// bracket, 14–64 s away). Two halves: no emitted keyframe may fall outside its own clip, and a
    /// sequence that keys a bone must carry every one of that band's own keys. Corpus-wide the
    /// first half is `bonescan`'s "band keys OUTSIDE their own ranges window: 0".
    #[test]
    fn no_clip_carries_a_key_from_another_sequences_band() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        for model in [
            "Creature\\Bear\\Bear.m2",
            "Creature\\Chicken\\Chicken.m2",
            "Character\\Human\\Male\\HumanMale.m2",
        ] {
            let bytes = chain.read_file(model).expect("read model");
            for s in parse_m2_animations(&bytes) {
                for b in &s.bones {
                    let times = b
                        .translation
                        .iter()
                        .map(|&(t, _)| ("translation", t))
                        .chain(b.rotation.iter().map(|&(t, _)| ("rotation", t)))
                        .chain(b.scale.iter().map(|&(t, _)| ("scale", t)));
                    for (ch, t) in times {
                        assert!(
                            (0.0..=s.duration + 1e-3).contains(&t),
                            "{model} anim {} bone {} {ch}: key at {t}s is outside the \
                             {}s clip — a bracket key leaked in as a playable key",
                            s.anim_id,
                            b.bone,
                            s.duration
                        );
                    }
                }
            }
        }
    }

    /// **The billboard host of a particle emitter's bone chain** (decision 0813), on the two real
    /// assets that opened it. `Field Marshal's Chain Spaulders` (display 32092 — nazriel's B118
    /// item) authors a 4-bone chain whose only billboard bone is bone **1** (flags `0x08`,
    /// spherical), with the two sparkle emitters hanging off its children 2 and 3. Neither emitter
    /// bone carries the flag, so a consumer that asks only about the emitter's OWN bone sees an
    /// ordinary rest-pose frame and puts the sparkle ~0.24 yd from where the reference draws it.
    ///
    /// Also pins the authoring pattern the offset is measured against: each emitter's record
    /// `position` **is** its own bone's pivot, so `position − host.pivot` is the chain offset the
    /// camera basis rotates. And the held torch — the other shipped case — is the same shape one
    /// generation shallower (emitter on bone 10, host bone 2, a 0.097 yd offset).
    #[test]
    fn real_pvp_shoulder_emitters_ride_a_billboard_bone() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Item\\ObjectComponents\\Shoulder\\LShoulder_Mail_PVPAlliance_C_01.m2")
            .expect("read the R14 mail shoulder");
        let skel = parse_m2_skeleton(&bytes).expect("skeleton");
        assert_eq!(skel.bones.len(), 4);
        assert_eq!(
            skel.bones.iter().filter(|b| b.billboard.is_some()).count(),
            1,
            "one billboard bone in the chain"
        );
        assert_eq!(
            skel.bones[1].billboard,
            Some(crate::BillboardKind::Spherical),
            "bone 1 authors flag 0x08"
        );
        let emitters = crate::parse_m2_particle_emitters(&bytes).expect("emitters");
        assert_eq!(emitters.len(), 2);
        for (i, em) in emitters.iter().enumerate() {
            let bone = usize::from(em.bone);
            assert_eq!(bone, 2 + i, "the sparkle pair rides bones 2 and 3");
            assert!(
                skel.bones[bone].billboard.is_none(),
                "…neither of which carries the flag itself"
            );
            assert_eq!(
                skel.billboard_host(em.bone),
                Some(1),
                "so the host is their parent"
            );
            // The record position IS the emitter bone's own pivot (the exporter's pattern).
            for c in 0..3 {
                assert!(
                    (em.position[c] - skel.bones[bone].pivot[c]).abs() < 1e-3,
                    "emitter {i} position {:?} vs bone pivot {:?}",
                    em.position,
                    skel.bones[bone].pivot
                );
            }
            // …and it sits ~0.24 yd from the host pivot — the offset the camera basis rotates,
            // i.e. how far a rest-pose placement misses by.
            let d: f32 = (0..3)
                .map(|c| (em.position[c] - skel.bones[1].pivot[c]).powi(2))
                .sum();
            assert!(
                (0.23..0.26).contains(&d.sqrt()),
                "chain offset {} yd",
                d.sqrt()
            );
        }
        // The held torch: same mechanism, emitter on bone 10 under billboard bone 2.
        let torch = chain
            .read_file("Item\\ObjectComponents\\Weapon\\Club_1H_Torch_A_01.m2")
            .expect("read the torch");
        let tskel = parse_m2_skeleton(&torch).expect("skeleton");
        let tem = crate::parse_m2_particle_emitters(&torch).expect("emitters");
        assert_eq!(tem.len(), 1);
        assert_eq!(tem[0].bone, 10);
        assert_eq!(tskel.billboard_host(10), Some(2));
        assert_eq!(
            tskel.bones[2].billboard,
            Some(crate::BillboardKind::Spherical)
        );
        // A bone with no billboard anywhere above it has no host (bone 0, the model root).
        assert_eq!(skel.billboard_host(0), None);
        // …and an out-of-range bone index is a `None`, never a panic.
        assert_eq!(skel.billboard_host(999), None);
    }

    /// **An effect model can be pure camera-facing geometry with no emitters at all** — the premise
    /// that makes a consumer's "keep the meshes, carry the emitters" split wrong on its own.
    ///
    /// `Spells\Enchantments\Sparkle_A.m2` is the shipped proof: **one** bone, spherical billboard
    /// (flags `0x08`), **zero** particle emitters and zero ribbons — the whole model is a single
    /// additive quad that faces the camera. `ItemVisuals` 28 hangs it on slot 4, and 10 of the 29604
    /// `ItemDisplayInfo` rows carry that visual, so a preview path that drops billboard batches and
    /// keeps only emitters renders *nothing whatsoever* for those items (the select-screen half of
    /// `#bugs` B118). Pinned here because the mistake is invisible in code that reads correct.
    #[test]
    fn a_real_item_glow_model_is_pure_billboard_geometry_with_no_emitters() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Spells\\Enchantments\\Sparkle_A.m2")
            .expect("read the Sparkle_A item glow");
        let skel = parse_m2_skeleton(&bytes).expect("skeleton");
        assert_eq!(skel.bones.len(), 1, "one bone — the quad's own");
        assert_eq!(
            skel.bones[0].billboard,
            Some(crate::BillboardKind::Spherical),
            "…and it is a spherical billboard, so its geometry is a card"
        );
        assert!(
            crate::parse_m2_particle_emitters(&bytes)
                .expect("emitters")
                .is_empty(),
            "no particle emitters: carrying only emitters carries nothing"
        );
        assert!(
            crate::parse_m2_ribbon_emitters(&bytes)
                .expect("ribbons")
                .is_empty(),
            "nor ribbons"
        );
    }
}
