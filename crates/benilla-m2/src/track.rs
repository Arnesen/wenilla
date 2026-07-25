//! M2Track readers: the typed [`M2Track`] (with the [`M2ScalarTrack`] / [`M2Vec3Track`] /
//! [`M2QuatTrack`] aliases) plus the track-parsing helpers the header walk (`crate::parse_m2`)
//! calls for the M2Color, M2TextureWeight, and M2TextureTransform tracks.

use benilla_bytes::ByteExt;

/// One typed `M2Track`, fully read: the interpolation type, the global-sequence tag, the
/// per-sequence key **ranges**, and the keys as **absolute global-timeline milliseconds** paired
/// with values. The v256 track (stride `0x1c`): `interp`@0, `global_seq`@2, interpolation_ranges
/// `M2Array`@`0x04/0x08`, timestamps `M2Array`@`0x0c/0x10` (u32 ms), values `M2Array`@`0x14/0x18`.
/// A sequence-timeline track (`gseq == 0xffff`) keys inside each sequence's absolute time band; a
/// global-sequence track loops on `global_sequences[gseq]`'s own clock (wow-re
/// `eval.md`/`doodad-anim-host.md`). Key count is `min(timestamps, values)` — vanilla art
/// occasionally pads one array.
#[derive(Clone, Debug)]
pub struct M2Track<V> {
    /// Interpolation type: `0` = step (nearest-previous key), nonzero = linear (wow-re `eval.md`:
    /// the scalar sampler's `cmp word[track],0` two-way dispatch).
    pub interp: u16,
    /// Global-sequence index, `0xffff` = an ordinary sequence-timeline track.
    pub gseq: u16,
    /// The per-sequence **key-index window** `(lo, hi)`, one entry per sequence in file order —
    /// the array the reference's key search indexes by the playing sequence's slot before it ever
    /// looks at a timestamp (VERIFIED wow-re `eval.md` FN1 `0x713d50` §1: `[track+8][idx*8+{0,4}]`;
    /// an empty array is the `[track+4]==0` fallback, "search the whole key list").
    ///
    /// These are **brackets**, not the playable key set: `hi` routinely points at a key in a LATER
    /// sequence's band, which is why selecting in-clip keys through this window instead of by
    /// timestamp froze creatures at a garbage pose (benilla decision 0133). Its load-bearing use is
    /// the other one — a band that keys nothing resolves to `keys[lo]` (`lo >= hi` ⇒ the degenerate
    /// `{lo, lo, 0}` result), so this array says *exactly* what the reference holds there instead
    /// of leaving it to a nearest-key approximation.
    pub ranges: Vec<(u32, u32)>,
    /// `(absolute ms, value)` keys, file order (time-ascending within a band).
    pub keys: Vec<(u32, V)>,
}

impl<V> Default for M2Track<V> {
    fn default() -> Self {
        Self {
            interp: 0,
            gseq: 0,
            ranges: Vec::new(),
            keys: Vec::new(),
        }
    }
}

/// A scalar (`fix16`, `0..=1`) track — the M2Color **alpha** and M2TextureWeight **weight** tracks.
pub type M2ScalarTrack = M2Track<f32>;
/// A `C3Vector` track — the M2TextureTransform **translation** / **scaling** tracks.
pub type M2Vec3Track = M2Track<[f32; 3]>;
/// A quaternion (4×f32, raw v256 floats) track — the M2TextureTransform **rotation** track.
pub type M2QuatTrack = M2Track<[f32; 4]>;

impl<V: Copy + PartialEq> M2Track<V> {
    /// All key values equal (or a single key): the track is a constant — `Some(value)`; `None` for a
    /// keyless or genuinely time-varying track.
    pub fn constant(&self) -> Option<V> {
        let (_, first) = *self.keys.first()?;
        self.keys.iter().all(|&(_, v)| v == first).then_some(first)
    }
}

/// Read one `M2Track<V>` (see [`M2Track`]): the shared `interp`/`gseq`/timestamp walk with a
/// per-value reader. Out-of-range reads yield an empty key list (= "no keys", the factor/transform
/// does not apply) — real art relies on that tolerance.
fn track_read<V>(
    b: &[u8],
    track_ofs: usize,
    val_size: usize,
    read_val: impl Fn(&[u8], usize) -> Option<V>,
) -> M2Track<V> {
    let (Some(interp), Some(gseq)) = (b.u16_at(track_ofs), b.u16_at(track_ofs + 2)) else {
        return M2Track::default();
    };
    let Some(((tn, to), (vn, vo))) = b
        .u32_at(track_ofs + 0x0c)
        .zip(b.u32_at(track_ofs + 0x10))
        .zip(b.u32_at(track_ofs + 0x14).zip(b.u32_at(track_ofs + 0x18)))
    else {
        return M2Track::default();
    };
    let n = tn.min(vn) as usize;
    let (to, vo) = (to as usize, vo as usize);
    let keys = (0..n)
        .map_while(|i| b.u32_at(to + i * 4).zip(read_val(b, vo + i * val_size)))
        .collect();
    // The per-sequence `(lo, hi)` key-index windows (`M2Array`@0x04/0x08, 8-byte entries) — see
    // [`M2Track::ranges`]. A short/absent array reads as empty, which is the reference's own
    // `[track+4] == 0` "no ranges" fallback.
    let ranges = match b.u32_at(track_ofs + 0x04).zip(b.u32_at(track_ofs + 0x08)) {
        Some((rn, ro)) => (0..rn as usize)
            .map_while(|i| {
                let e = ro as usize + i * 8;
                b.u32_at(e).zip(b.u32_at(e + 4))
            })
            .collect(),
        None => Vec::new(),
    };
    M2Track {
        interp,
        gseq,
        ranges,
        keys,
    }
}

fn rd_vec3(b: &[u8], o: usize) -> Option<[f32; 3]> {
    Some([b.f32_at(o)?, b.f32_at(o + 4)?, b.f32_at(o + 8)?])
}

/// Read a scalar `fix16` track (`uint16/32767` values). Used for the colour-**alpha** and
/// transparency-**weight** tracks that gate batch visibility and drive the animated material combine.
pub(crate) fn track_fix16(b: &[u8], track_ofs: usize) -> M2ScalarTrack {
    track_read(b, track_ofs, 2, |b, o| {
        b.u16_at(o).map(|v| v as f32 / 32767.0)
    })
}

/// Read a timed `C3Vector` (3×f32) track — the texture-transform **translation**/**scaling**
/// tracks and the M2Color **RGB** track.
pub(crate) fn track_vec3_timed(b: &[u8], track_ofs: usize) -> M2Vec3Track {
    track_read(b, track_ofs, 12, rd_vec3)
}

/// Read a timed quaternion (4×f32, raw v256 floats — vanilla does not compress quat keys) track —
/// the texture-transform **rotation** track.
pub(crate) fn track_quat(b: &[u8], track_ofs: usize) -> M2QuatTrack {
    track_read(b, track_ofs, 16, |b, o| {
        Some([
            b.f32_at(o)?,
            b.f32_at(o + 4)?,
            b.f32_at(o + 8)?,
            b.f32_at(o + 12)?,
        ])
    })
}
