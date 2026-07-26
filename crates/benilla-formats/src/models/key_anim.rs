//! The generic keyed-loop bake shared by the material-alpha (`mat_anim`) and texture-transform
//! (`tex_anim`) channels: ONE kernel-faithful sampler (k0 = last key ≤ t, step holds, linear
//! lerps, clamp past the last key — no wrap-lerp) and ONE clock resolution (gseq wrap vs
//! per-sequence band rebase), so the byte-verified sampling semantics (wow-re `eval.md`, and the
//! step-boundary fix) live in exactly one place.
//!
//! **A track bakes per SEQUENCE, not once.** A sequence-timeline track keys on one absolute
//! timeline that every sequence carves a band out of, so "the loop this track plays" is a question
//! about *which sequence is playing* — the reference re-reads it every frame from the playing
//! sequence's own key window (wow-re `eval.md` FN1 `0x713d50`: window = `ranges[seqSlot]`, and a
//! collapsed window `lo >= hi` resolves to the single key `keys[lo]`). Baking only the first
//! sequence's band was correct for a placed doodad — which arms `animations[0]` once and loops it
//! forever — and wrong for anything that changes sequence, i.e. every creature: a voidwalker's two
//! upper armour pieces are weight `0` in Stand/Walk/Run and `1` only in Death, and a seq-0-only
//! bake freezes whichever value Stand happened to hold.

use benilla_m2::M2Track;

/// A value a keyed loop can linearly interpolate.
pub trait Lerp: Copy {
    fn lerp(a: Self, b: Self, f: f32) -> Self;
}
impl Lerp for f32 {
    fn lerp(a: Self, b: Self, f: f32) -> Self {
        a + (b - a) * f
    }
}
impl Lerp for [f32; 2] {
    fn lerp(a: Self, b: Self, f: f32) -> Self {
        [f32::lerp(a[0], b[0], f), f32::lerp(a[1], b[1], f)]
    }
}
impl Lerp for [f32; 3] {
    fn lerp(a: Self, b: Self, f: f32) -> Self {
        [
            f32::lerp(a[0], b[0], f),
            f32::lerp(a[1], b[1], f),
            f32::lerp(a[2], b[2], f),
        ]
    }
}

impl Lerp for [f32; 4] {
    fn lerp(a: Self, b: Self, f: f32) -> Self {
        [
            f32::lerp(a[0], b[0], f),
            f32::lerp(a[1], b[1], f),
            f32::lerp(a[2], b[2], f),
            f32::lerp(a[3], b[3], f),
        ]
    }
}

/// The sequence a sequence-timeline track bakes against: the model's **file** sequence slot (which
/// is what indexes the track's [`M2Track::ranges`] — NOT an index into a filtered animation list)
/// and that sequence's absolute `(start_ms, end_ms)` band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SeqSlot {
    pub index: usize,
    pub band: (u32, u32),
}

/// One baked keyed loop, seconds: sample at `t mod period` (linear or step per [`Self::step`]),
/// holding the first/last key outside the keyed span. `period == 0` ⇒ a constant (single key).
#[derive(Clone, Debug, PartialEq)]
pub struct KeyAnim<V> {
    /// Loop period (secs): the global sequence's duration, or the sequence band's length.
    /// `0.0` for a constant.
    pub period: f32,
    /// Step interpolation (`interp == 0`): hold each key until the next; else linear.
    pub step: bool,
    /// `(secs from loop start, value)`, time-ascending.
    pub keys: Vec<(f32, V)>,
}

impl<V: Lerp> KeyAnim<V> {
    /// Sample at `elapsed` seconds on the loop clock; `empty` is the channel's identity for the
    /// keyless case (the bake never emits that — each channel's `sample` supplies it).
    pub(super) fn sample_or(&self, elapsed: f32, empty: V) -> V {
        let Some(&(t0, v0)) = self.keys.first() else {
            return empty;
        };
        if self.period <= 0.0 || self.keys.len() == 1 {
            return v0;
        }
        let t = elapsed.rem_euclid(self.period);
        if t <= t0 {
            return v0;
        }
        // k0 = the last key with time ≤ t (the kernel's search), mirroring `BoneScaleAnim::sample`.
        let mut k0 = 0;
        for (i, &(tk, _)) in self.keys.iter().enumerate() {
            if tk <= t {
                k0 = i;
            } else {
                break;
            }
        }
        let (ta, va) = self.keys[k0];
        // Step, or past the final key: hold (the kernel's search clamps; no wrap-lerp to key 0).
        if self.step || k0 + 1 >= self.keys.len() {
            return va;
        }
        let (tb, vb) = self.keys[k0 + 1];
        if tb <= ta {
            return va;
        }
        V::lerp(va, vb, (t - ta) / (tb - ta))
    }
}

/// The reference's own track read at absolute time `t_ms`, for the key window `[lo, hi]` — the
/// transcription of wow-re `eval.md` FN1 (`0x713d50`) + the scalar sampler's two-way interp
/// dispatch (`0x71af20`, `cmp word[track],0`):
///
/// - a **collapsed window** (`lo >= hi`) resolves to `keys[lo]` outright — the degenerate
///   `{lo, lo, 0.0}` result, and the reason a band that keys nothing still has an authored value;
/// - otherwise `k0` = the last key in the window at or before `t_ms` (clamped up to `lo`), `k1 =
///   k0 + 1`, and the value is `keys[k0]` for a step track or past the last key, else the lerp.
///
/// **One named deviation:** the reference computes the fraction unclamped, so a `t_ms` outside
/// `[ts[k0], ts[k1]]` extrapolates. That can only arise from a window whose brackets don't contain
/// the band, and an extrapolated *fix16 factor* lands outside `[0, 1]` — where a negative value
/// would cull the batch (`A <= 0`) on a data quirk rather than on authoring. We clamp the fraction
/// to `[0, 1]`, i.e. hold at the bracket instead of running past it.
pub(super) fn sample_window<V: Lerp>(
    keys: &[(u32, V)],
    step: bool,
    lo: usize,
    hi: usize,
    t_ms: u32,
) -> Option<V> {
    let last = keys.len().checked_sub(1)?;
    let (lo, hi) = (lo.min(last), hi.min(last));
    if lo >= hi {
        return keys.get(lo).map(|&(_, v)| v);
    }
    let mut k0 = lo;
    for (k, &(ts, _)) in keys.iter().enumerate().take(hi + 1).skip(lo) {
        if ts <= t_ms {
            k0 = k;
        } else {
            break;
        }
    }
    let (ta, va) = keys[k0];
    if step || k0 + 1 > last {
        return Some(va);
    }
    let (tb, vb) = keys[k0 + 1];
    if tb <= ta {
        return Some(va);
    }
    let f = ((t_ms as f32 - ta as f32) / (tb as f32 - ta as f32)).clamp(0.0, 1.0);
    Some(V::lerp(va, vb, f))
}

/// Bake one typed track to a [`KeyAnim`] for one sequence, or `None` when it contributes nothing
/// the static path doesn't already handle. `proj` maps the on-disk key value into the baked domain
/// (identity for scalars; vec3 → the UV xy). The two predicates name the channel's semantics:
///
/// - `drop_constant(c)`: a **whole-track** constant `c` vanishes — because `c` is the channel
///   identity, or because the caller's *static* path already folded it away (the alpha combine's
///   constant-0 cull). Judged on the whole track, which is what that static path looked at.
/// - `is_identity(v)`: `v` contributes nothing at runtime — applied to a **band** that bakes down
///   to a constant, where the static path has NOT looked. A held non-identity value still bakes,
///   including a held alpha 0, which must keep hiding the batch every frame of that sequence.
///
/// `gseq_durations` is the model's global-sequence table (ms); `seq` the sequence to bake for
/// (see [`SeqSlot`]). A `gseq`-tagged track ignores `seq` entirely — it runs on the global
/// sequence's own free clock, the same loop in every animation.
pub(super) fn bake_track<T: Copy, V: Lerp + PartialEq>(
    track: &M2Track<T>,
    gseq_durations: &[u32],
    seq: Option<SeqSlot>,
    proj: impl Fn(T) -> V,
    drop_constant: impl Fn(V) -> bool,
    is_identity: impl Fn(V) -> bool,
) -> Option<KeyAnim<V>> {
    if track.keys.is_empty() {
        return None;
    }
    let keys: Vec<(u32, V)> = track.keys.iter().map(|&(t, v)| (t, proj(v))).collect();
    let step = track.interp == 0;
    let constant = |v: V| KeyAnim {
        period: 0.0,
        step,
        keys: vec![(0.0, v)],
    };
    // An all-equal track is a constant in every band: dropped when the static path owns it, else a
    // period-0 key. Judged before the band split, because that is the shape the static path tested.
    let (_, first) = keys[0];
    if keys.iter().all(|&(_, v)| v == first) {
        if drop_constant(first) {
            return None;
        }
        return Some(constant(first));
    }
    if track.gseq != 0xffff {
        // Global-sequence clock: keys are already loop-relative ms; wrap on the table duration
        // (a 0/absent duration degrades to the last key's time — a defensive clamp, not observed).
        let period_ms = gseq_durations
            .get(track.gseq as usize)
            .copied()
            .filter(|&d| d > 0)
            .unwrap_or_else(|| keys.last().map(|&(t, _)| t).unwrap_or(0).max(1));
        return Some(KeyAnim {
            period: period_ms as f32 / 1000.0,
            step,
            keys: keys.iter().map(|&(t, v)| (t as f32 / 1000.0, v)).collect(),
        });
    }
    // Sequence-timeline track: rebuild the function the reference samples across THIS sequence's
    // band. Its key window is the track's own per-sequence `(lo, hi)` pair; an absent ranges array
    // is the reference's `[track+4] == 0` fallback — search the whole key list.
    let SeqSlot { index, band } = seq?;
    let (start, end) = band;
    let (lo, hi) = match track.ranges.get(index) {
        Some(&(lo, hi)) => (lo as usize, hi as usize),
        None => (0, keys.len().saturating_sub(1)),
    };
    let at = |t: u32| sample_window(&keys, step, lo, hi, t);
    let head = at(start)?;
    if end <= start {
        // A zero-length band samples one instant — nothing to loop.
        return (!is_identity(head)).then(|| constant(head));
    }
    // The band's own keys, plus both endpoints: the reference's function on `[start, end]` is
    // piecewise between consecutive keys, so sampling it AT the keys (and at the band edges, where
    // it is mid-segment toward an out-of-band bracket) reproduces it exactly.
    let mut baked: Vec<(f32, V)> = vec![(0.0, head)];
    for (k, &(t, v)) in keys.iter().enumerate() {
        if k >= lo && k <= hi && t > start && t < end {
            baked.push(((t - start) as f32 / 1000.0, v));
        }
    }
    if let Some(tail) = at(end) {
        baked.push(((end - start) as f32 / 1000.0, tail));
    }
    // A band the track never actually moves through is a constant hold — the ordinary case for a
    // batch authored to appear in one animation and hide in every other.
    if baked.iter().all(|&(_, v)| v == head) {
        return (!is_identity(head)).then(|| constant(head));
    }
    Some(KeyAnim {
        period: ((end - start) as f32 / 1000.0).max(0.001),
        step,
        keys: baked,
    })
}
