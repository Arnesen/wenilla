//! The generic keyed-loop bake shared by the material-alpha (`mat_anim`) and texture-transform
//! (`tex_anim`) channels: ONE kernel-faithful sampler (k0 = last key ≤ t, step holds, linear
//! lerps, clamp past the last key — no wrap-lerp) and ONE clock resolution (gseq wrap vs seq-0
//! band rebase), so the byte-verified sampling semantics (wow-re `eval.md`, and the step-boundary
//! fix) live in exactly one place.

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

/// One baked keyed loop, seconds: sample at `t mod period` (linear or step per [`Self::step`]),
/// holding the first/last key outside the keyed span. `period == 0` ⇒ a constant (single key).
#[derive(Clone, Debug, PartialEq)]
pub struct KeyAnim<V> {
    /// Loop period (secs): the global sequence's duration, or the first sequence's band length.
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

/// Bake one typed track to a [`KeyAnim`], or `None` when it contributes nothing the static path
/// doesn't already handle. `proj` maps the on-disk key value into the baked domain (identity for
/// scalars; vec3 → the UV xy). The two predicates name the channel's semantics:
///
/// - `drop_constant(c)`: an **all-keys-equal** track with value `c` vanishes — because `c` is the
///   channel identity, or because the caller's *static* path already folded it away (the alpha
///   combine's constant-0 cull).
/// - `is_identity(v)`: `v` contributes nothing at runtime — used for the band-empty held key and a
///   lone in-band key, where the static path has NOT looked (a held non-identity value still bakes,
///   including a held alpha 0, which must keep hiding the batch every frame).
///
/// `gseq_durations` is the model's global-sequence table (ms); `seq0` the file-order-first
/// sequence's absolute `(start_ms, end_ms)` band — the loop a placed doodad plays.
pub(super) fn bake_track<T: Copy, V: Lerp + PartialEq>(
    track: &M2Track<T>,
    gseq_durations: &[u32],
    seq0: Option<(u32, u32)>,
    proj: impl Fn(T) -> V,
    drop_constant: impl Fn(V) -> bool,
    is_identity: impl Fn(V) -> bool,
) -> Option<KeyAnim<V>> {
    if track.keys.is_empty() {
        return None;
    }
    let keys: Vec<(u32, V)> = track.keys.iter().map(|&(t, v)| (t, proj(v))).collect();
    let step = track.interp == 0;
    // An all-equal track is a constant: dropped when the static path owns it, else a period-0 key.
    let (_, first) = keys[0];
    if keys.iter().all(|&(_, v)| v == first) {
        if drop_constant(first) {
            return None;
        }
        return Some(KeyAnim {
            period: 0.0,
            step,
            keys: vec![(0.0, first)],
        });
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
    // Sequence-timeline track: keep the keys inside the first sequence's absolute band, rebased —
    // the loop a placed doodad's one-time arm plays (wow-re `doodad-anim-host.md`). No keys in the
    // band ⇒ the channel holds the nearest earlier key while that sequence plays; bake it constant
    // unless that held value is the channel identity (an absent earlier key contributes nothing).
    let (start, end) = seq0?;
    let in_band: Vec<(f32, V)> = keys
        .iter()
        .filter(|&&(t, _)| t >= start && t <= end)
        .map(|&(t, v)| ((t - start) as f32 / 1000.0, v))
        .collect();
    if in_band.is_empty() {
        let held = keys
            .iter()
            .take_while(|&&(t, _)| t < start)
            .last()
            .map(|&(_, v)| v)?;
        if is_identity(held) {
            return None;
        }
        return Some(KeyAnim {
            period: 0.0,
            step,
            keys: vec![(0.0, held)],
        });
    }
    if in_band.len() == 1 && is_identity(in_band[0].1) {
        return None;
    }
    Some(KeyAnim {
        period: ((end - start) as f32 / 1000.0).max(0.001),
        step,
        keys: in_band,
    })
}
