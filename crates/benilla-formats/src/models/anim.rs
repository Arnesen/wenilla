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
    /// Bone flag `0x04` — the bone keeps the MODEL ROOT's orientation instead of inheriting its
    /// parent's rotation (its pivot still rides the parent's full matrix). Carried by the unskinned
    /// attach-helper bones (HandArrow/Bullet 126/127 — the nocked arrow lies flat along the facing
    /// instead of twisting with the draw hand). The client's bone-palette build honors the flag;
    /// the attach-child transform then inherits it (wow-re `nocked-ammo-cancel.md` §E4).
    pub ignore_parent_rotation: bool,
}

/// A model's bone hierarchy — the rest skeleton the skinned-entity path turns into a joint-entity
/// tree + inverse bind poses. Bones are in M2 file order; a vertex's `joints` index into this list.
/// Empty for a boneless model.
#[derive(Debug, Clone, Default)]
pub struct Skeleton {
    pub bones: Vec<SkeletonBone>,
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
            ignore_parent_rotation: b.flags.bits() & 0x4 != 0,
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

/// Parse the M2 attachment-point table (see [`M2Attachment`]). Straight off `benilla-m2`'s already
/// bone-range-checked parse; empty for a model with no attachment points.
pub fn parse_m2_attachments(bytes: &[u8]) -> Result<Vec<M2Attachment>> {
    let format =
        parse_m2(&mut Cursor::new(bytes)).map_err(|e| anyhow::anyhow!("parsing M2: {e}"))?;
    Ok(format
        .model()
        .attachments
        .iter()
        .map(|a| M2Attachment {
            id: a.id,
            bone: a.bone,
            position: a.position,
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

/// Read one bone `M2Track`'s keyframes for the sequence spanning `[seq_start_ms, seq_end_ms]` (absolute
/// ms), rebased to seconds from the start. `read_value` decodes one value at a byte offset; `stride` is
/// its size. The v256 `M2Track` (0x1c): `interp_type`@0, `global_seq`@2, then three `M2Array`s —
/// interpolation_ranges@0x04, **timestamps@0x0c**, **values@0x14** (each `{count u32, offset u32}`).
///
/// Keys are selected by **absolute timestamp within the sequence's time band**, NOT via
/// `interpolation_ranges`: the flat timestamp/value arrays concatenate every sequence's keys
/// (timestamps absolute, VERIFIED), the sequences occupy disjoint bands, and this is exactly what the
/// real client's kernel does (rebase playback time into `[seqStart, seqEnd]`, then search the
/// timestamps). The `interpolation_ranges` window proved unreliable in real vanilla art — its end index
/// can point past the sequence into a later one (Chicken/Bear/Kobold/…: a key 14–64 s away), which froze
/// those creatures at a garbage pose: the range's far key is a clamp *endpoint*, never a playable
/// in-clip key. A **global-sequence** track (`global_seq != 0xffff`) loops on its own clock and is
/// skipped here (the global-sequence post-pass is deferred).
///
/// A band with **no keys at all** still has an authored pose: the real sampler clamps/lerps between
/// the keys bracketing the band (that is what the per-sequence `interpolation_ranges` pairs encode —
/// HumanMale bone 27 rot has 4 keys and `ranges[Run] = (2, 3)`, both outside Run's band), so a keyed
/// bone is **never** left unsampled. Dropping the track instead froze the bone at whatever the
/// *previous* clip left (the task-#14 tilt: a Stand-variation waist twist rode through the entire
/// run, exposed the day the 0123 variation roll first armed non-head Stands). We emit one constant
/// key holding the **nearest** authored key's value — the value the real clamp lands on whenever the
/// band sits close to one bracket (vanilla bands are 0.3–4 s; bracket gaps are minutes), a named
/// approximation of the mid-gap lerp otherwise.
fn read_bone_track<T>(
    b: &[u8],
    track: usize,
    seq_start_ms: u32,
    seq_end_ms: u32,
    stride: usize,
    read_value: impl Fn(&[u8], usize) -> T,
) -> Vec<(f32, T)> {
    if track + 0x1c > b.len() {
        return Vec::new();
    }
    let (times_n, times_o) = (
        le_u32(b, track + 0x0c) as usize,
        le_u32(b, track + 0x10) as usize,
    );
    let vals_o = le_u32(b, track + 0x18) as usize;
    if le_u16(b, track + 0x02) != 0xffff {
        // A **global-sequence** track: driven by its own looping clock, independent of the playing
        // sequence. The single-key case is a pure CONSTANT channel — this is how vanilla art authors
        // the stowed-weapon attach-bone orientations (HumanMale bones 29/30/58–62: one rotation key
        // at t=0 on global-seq 0, itself 0 ms long — the blade-down / shield-on-back quaternions;
        // dumped 2026-07-03, the floating-stowed-sword bug). Emit the constant into every sequence's
        // window (`keyframe_curve` holds a lone key across the clip). A *time-varying* multi-key
        // global track (a fidget glow on its own clock) still needs the global clock — deferred.
        if times_n == 1 && vals_o + stride <= b.len() {
            return vec![(0.0, read_value(b, vals_o))];
        }
        return Vec::new();
    }
    let mut out = Vec::new();
    // The closest out-of-band key, by distance to the band — the clamp value if the band is empty.
    let mut nearest: Option<(u32, usize)> = None;
    for k in 0..times_n {
        let (t_off, v_off) = (times_o + k * 4, vals_o + k * stride);
        if t_off + 4 > b.len() || v_off + stride > b.len() {
            break;
        }
        let ts = le_u32(b, t_off);
        if ts < seq_start_ms || ts > seq_end_ms {
            // A key in another sequence's band — not playable here, but remembered as the
            // clamp candidate (see the module doc: an empty band still has an authored pose).
            let d = if ts < seq_start_ms {
                seq_start_ms - ts
            } else {
                ts - seq_end_ms
            };
            if nearest.is_none_or(|(best, _)| d < best) {
                nearest = Some((d, k));
            }
            continue;
        }
        out.push(((ts - seq_start_ms) as f32 / 1000.0, read_value(b, v_off)));
    }
    if out.is_empty() {
        if let Some((_, k)) = nearest {
            let v_off = vals_o + k * stride;
            if v_off + stride <= b.len() {
                out.push((0.0, read_value(b, v_off)));
            }
        }
    }
    out
}

/// The weapon-grip finger poses: each requested `bone`'s rotation at the **HandsClosed** (`AnimationData`
/// id 15) frame, **clamped** to the nearest key at-or-before that frame. Scoped to the grip on purpose —
/// a weapon-hand finger bone is keyed in *adjacent* global-timeline bands but NOT the HandsClosed frame
/// itself (on HumanMale bone 102 is keyed at 43333/70000 ms, both curled, not the 60000 ms frame), so the
/// general per-sequence in-band read ([`read_bone_track`]) correctly drops it — yet the real client
/// evaluates the global track there and gets the curled pose (wow-re `hand-grip-mechanism.md`). Doing the
/// clamp *only* for these few finger bones is what makes the weapon hand close without re-introducing the
/// distant-key bleed the in-band read deliberately guards against. Returns `(bone, quat[x,y,z,w])` for the
/// bones that carry a plain rotation track; empty if the model has no HandsClosed sequence.
pub fn hand_grip_finger_poses(bytes: &[u8], bones: &[u16]) -> Vec<(u16, [f32; 4])> {
    let b = bytes;
    if b.len() < 0x40 || &b[0..4] != b"MD20" {
        return Vec::new();
    }
    let (seq_count, seq_ofs) = (le_u32(b, 0x1c) as usize, le_u32(b, 0x20) as usize);
    let (bone_count, bone_ofs) = (le_u32(b, 0x34) as usize, le_u32(b, 0x38) as usize);
    // HandsClosed (anim id 15) → its frame's absolute start ms.
    let mut frame = None;
    for s in 0..seq_count {
        let rec = seq_ofs + s * 0x44;
        if rec + 0x44 > b.len() {
            break;
        }
        if le_u16(b, rec) == 15 {
            frame = Some(le_u32(b, rec + 0x04));
            break;
        }
    }
    let Some(frame) = frame else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for &bone in bones {
        let bi = bone as usize;
        if bi >= bone_count {
            continue;
        }
        let track = bone_ofs + bi * 0x6c + 0x28; // the bone's rotation M2Track (+0x28 in the 0x6c record)
        if track + 0x1c > b.len() || le_u16(b, track + 0x02) != 0xffff {
            continue; // out of range, or a global-sequence track (own clock — not this)
        }
        let (nts, ots) = (
            le_u32(b, track + 0x0c) as usize,
            le_u32(b, track + 0x10) as usize,
        );
        let vals_o = le_u32(b, track + 0x18) as usize;
        if nts == 0 {
            continue;
        }
        // Clamp: the last key with timestamp <= the frame (keys are time-ascending); else the first.
        let mut k = 0usize;
        for i in 0..nts {
            let t_off = ots + i * 4;
            if t_off + 4 > b.len() {
                break;
            }
            if le_u32(b, t_off) <= frame {
                k = i;
            } else {
                break;
            }
        }
        let v = vals_o + k * 16; // rotation stride 16 (a 4×f32 quaternion)
        if v + 16 > b.len() {
            continue;
        }
        out.push((
            bone,
            [
                le_f32(b, v),
                le_f32(b, v + 4),
                le_f32(b, v + 8),
                le_f32(b, v + 12),
            ],
        ));
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
        for i in 0..bone_count {
            let brec = bone_ofs + i * 0x6c;
            if brec + 0x60 > b.len() {
                break;
            }
            let translation = read_bone_track(b, brec + 0x0c, start, end, 12, vec3);
            let rotation = read_bone_track(b, brec + 0x28, start, end, 16, quat);
            let scale = read_bone_track(b, brec + 0x44, start, end, 12, vec3);
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

    /// An empty band clamps to the nearest authored key instead of dropping the track (the task-#14
    /// tilt): HumanMale bone 27 (waist) rot has 4 keys, all outside Run's band — the real sampler
    /// pins it via `interpolation_ranges` (Run's pair is `(2, 3)`, both out-of-band), ours must emit
    /// the constant so a Stand-variation twist can't ride through the run. And the guarantee that
    /// makes the fix total: on every sequence of the model, any bone keyed *anywhere* stays keyed.
    #[test]
    fn empty_band_clamps_to_nearest_key_never_drops_the_bone() {
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
}
