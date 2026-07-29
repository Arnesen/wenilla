//! An M2 (MD20) model reader for **WoW 1.12.1 (build 5875)** — in-repo, replacing the `wow-m2` fork
//! (decision 0021), scoped to the **render path** the renderer consumes.
//!
//! Vanilla `.m2` files are MD20 version **256/257**: a fixed header of `M2Array`s (each a `count` +
//! file `offset`), then the referenced arrays, with the **skin profile(s) embedded** in the same file.
//! We read only what the mesh builder needs — vertices, textures, materials (render flags), bones
//! (pivot + flags), the texture-lookup table, and skin profile 0 (indices / triangles / submeshes /
//! batches) — and deliberately **skip the cosmetic chunks** (particle emitters, lights, texture
//! animations). That sidesteps exactly what the `wow-m2` fork patched: those chunks can be malformed
//! in Classic art and abort a full parse; benilla-formats parses lights/particles itself from raw
//! bytes, so we never touch them here.
//!
//! Proven against `wow-m2` over real models during the decision-0021 migration (oracle test in git
//! history); the creature/GameObject mesh + cosmetic-chunk-model golden tests in benilla-formats pin
//! it end-to-end on every run.
//!
//! Byte access goes through `benilla-bytes` (decision 0064): every read is bounds-checked and a
//! truncated array/record is a typed [`Error::Truncated`], never a panic; header-driven `Vec`
//! reservations are capped by what the input could actually hold, so a corrupt count can no longer
//! OOM-abort the process before the first bounds-checked read ever runs.
//!
//! Split by concern: this file keeps the public re-export surface and the top-level [`parse_m2`]
//! entry + MD20 header walk; `model` holds the plain parsed-record types, `track` the M2Track
//! scalar/vector readers, `skin` the embedded skin-profile decoder, and `error` the parse error type.

mod error;
mod model;
mod skin;
mod track;

pub use error::Error;
pub use model::{
    M2ArrayString, M2Attachment, M2BlendMode, M2Bone, M2BoneFlags, M2EventMarker, M2Format,
    M2Header, M2Material, M2Model, M2PlayableAnim, M2RawData, M2RenderFlags, M2Texture,
    M2TextureTransform, M2TextureType, M2Vertex, C2, C3,
};
pub use skin::{Skin, SkinBatch, SkinSection};
pub use track::{M2QuatTrack, M2ScalarTrack, M2Track, M2Vec3Track};

use std::ffi::CString;
use std::io::Cursor;

use benilla_bytes::{capped, ByteExt};

use error::Result;
use track::{track_fix16, track_quat, track_vec3_timed};

/// Read a `C3Vector` (3×f32) at `o`, bounds-checked. `None` iff any of the three reads would run
/// past the end — callers map that to their own [`Error::Truncated`].
fn rd_c3(b: &[u8], o: usize) -> Option<C3> {
    Some(C3 {
        x: b.f32_at(o)?,
        y: b.f32_at(o + 4)?,
        z: b.f32_at(o + 8)?,
    })
}

/// Parse an MD20 model (v256/257). `cursor` position is ignored; the file starts at the header.
pub fn parse_m2(cursor: &mut Cursor<&[u8]>) -> Result<M2Format> {
    let b: &[u8] = cursor.get_ref();
    if b.len() < 8 || &b[0..4] != b"MD20" {
        return Err(Error::NotMd20);
    }
    let version = b.u32_at(4).ok_or(Error::Truncated)?;
    if !(256..=263).contains(&version) {
        return Err(Error::UnsupportedVersion(version));
    }

    // Walk the header reading the M2Array `(count, offset)` pairs we need (8 bytes each), applying the
    // pre-Wrath version conditionals. Layout order per wowdev ADT/M2 + the reference.
    let arr = |p: usize| -> Result<(u32, u32)> {
        Ok((
            b.u32_at(p).ok_or(Error::Truncated)?,
            b.u32_at(p + 4).ok_or(Error::Truncated)?,
        ))
    };
    let mut p = 8;
    let _name = arr(p)?;
    p += 8;
    p += 4; // flags u32
    let _global = arr(p)?;
    p += 8;
    let _anim = arr(p)?;
    p += 8;
    // AnimationLookup (header `+0x24`/`+0x28`): `AnimationData.dbc` id → this model's first sequence
    // slot for it, `0xffff` where the model doesn't author it. The reference's "does this model own
    // animation id X" predicate (`0x711960`) is exactly a bounds-checked read of this table, so it is
    // parsed rather than inferred from the sequence list — see [`M2Model::owns_animation`].
    let animation_lookup_arr = arr(p)?;
    p += 8;
    // PlayableAnimationLookup (decision 0082, header `+0x2c`/`+0x30`, pre-Wrath only — dropped past
    // version 263, which `parse_m2`'s own top guard already excludes, so this is unconditional on any
    // version that reaches here; the `if` stays for documentation).
    let playable_animation_lookup_arr = if (256..=263).contains(&version) {
        let v = arr(p)?;
        p += 8;
        v
    } else {
        (0, 0)
    };
    let bones = arr(p)?;
    p += 8;
    let _key_bone = arr(p)?;
    p += 8;
    let vertices = arr(p)?;
    p += 8;
    let views = if version <= 263 {
        let v = arr(p)?;
        p += 8;
        v
    } else {
        p += 4;
        (0, 0)
    };
    let colors_arr = arr(p)?; // header 0x54: M2Color[] (colour + alpha tracks)
    p += 8;
    let textures = arr(p)?;
    p += 8;
    let transparency_arr = arr(p)?; // header 0x64: M2TextureWeight[] (weight tracks)
    p += 8;
    if version <= 263 {
        p += 8; // texture_flipbooks (pre-Wrath)
    }
    let tex_anim_arr = arr(p)?; // header 0x74: M2TextureTransform[] (3 UV-animation tracks each)
    p += 8;
    let _color_repl = arr(p)?;
    p += 8;
    let render_flags = arr(p)?;
    p += 8;
    let _bone_lookup = arr(p)?;
    p += 8;
    let texture_lookup_table = arr(p)?;
    p += 8;
    // header 0x9c: texture_unit_lookup (the env-vs-UV mapping table) — skipped for now.
    //
    // The three lookup slots here follow wow-re's *stride-pin reconciliation* (renderFlags pinned
    // at 0x84 ⇒ texLookup 0x94 · texUnitLookup 0x9c · transLookup 0xa4 · texAnimLookup 0xac), NOT
    // the stale one-slot-early labels in models.md's field-map line. Byte-proof on real art:
    // ElwynnTallWaterfall01 (2 transforms, batch combos 0/1) has [0,1] at 0xac, and
    // StormwindMagePortal01 (4 weight tracks) has the identity [0,1,2,3] at 0xa4 with [0] at 0x9c
    // — reading 0x9c as the transparency lookup silently dropped the portal's combo-1..3 tracks.
    p += 8;
    // header 0xa4: transparency_lookup (u16) — `weight_combo_index` indexes it to reach a weight track.
    let transparency_lookup_arr = arr(p)?;
    p += 8;
    // header 0xac: texture_animation_lookup (u16) — `texture_transform_combo_index` (texUnit
    // +0x16) indexes it to reach a texture transform (wow-re rf72 `0x70b897`); 0xffff = none.
    let tex_anim_lookup_arr = arr(p)?;
    p += 8;
    // Authored bounds: AABB (min C3, max C3) then sphere radius.
    let read_box = |o: usize| -> Result<[f32; 3]> {
        Ok([
            b.f32_at(o).ok_or(Error::Truncated)?,
            b.f32_at(o + 4).ok_or(Error::Truncated)?,
            b.f32_at(o + 8).ok_or(Error::Truncated)?,
        ])
    };
    let bounding_box_min = read_box(p)?;
    p += 12;
    let bounding_box_max = read_box(p)?;
    p += 12;
    let bounding_sphere_radius = b.f32_at(p).ok_or(Error::Truncated)?;
    p += 4;
    // The collision box (min C3 + max C3) + collision sphere radius — the tight collision hull (28 bytes).
    let collision_box_min = read_box(p)?;
    p += 12;
    let collision_box_max = read_box(p)?;
    p += 12;
    let collision_sphere_radius = b.f32_at(p).ok_or(Error::Truncated)?;
    p += 4;
    let bounding_triangles = arr(p)?;
    p += 8;
    let bounding_vertices = arr(p)?;
    p += 8;
    // Continue the header to the attachments block: boundingNormals, then attachments + attachLookup.
    let _bounding_normals = arr(p)?;
    p += 8;
    let attachments = arr(p)?;
    p += 8;
    let attach_lookup = arr(p)?;
    p += 8;
    // header 0x114: the animation-event table (the `$CSL`/`$BWR`/`$SND`… records) — the positional
    // markers are kept below; the per-sequence fire keys are `benilla-formats`' concern.
    let events = arr(p)?;
    // The follow-camera pivot height source (wow-re `follow-camera`, `0x50cbc0`): attachment **id 17**'s
    // model-space Z. `None` if the model has no slot-17 attachment (caller falls back to the vertex box).
    let pivot_attach_z = attachment_z(b, 17, attachments, attach_lookup);

    // --- read the arrays we keep ---
    let get = |start: usize, len: usize| -> Result<&[u8]> {
        b.bytes_at(start, len).ok_or(Error::Truncated)
    };

    // Vertices: 48 bytes each. The reservation is capped by what the file could actually hold
    // (decision 0064, `benilla_bytes::capped`): a corrupt/hostile `vertices.0` count can then at
    // worst reserve the input's own size, never OOM-abort the process before the loop reaches its
    // first bounds-checked read. The loop below still walks the *real* declared count — a short
    // file fails cleanly at `get(..)?` instead.
    let vert_avail = b.len().saturating_sub(vertices.1 as usize);
    let mut verts = Vec::with_capacity(capped(vertices.0 as usize, 48, vert_avail));
    for i in 0..vertices.0 as usize {
        let v = get(vertices.1 as usize + i * 48, 48)?;
        verts.push(M2Vertex {
            position: rd_c3(v, 0).ok_or(Error::Truncated)?,
            bone_weights: [v[12], v[13], v[14], v[15]],
            bone_indices: [v[16], v[17], v[18], v[19]],
            normal: rd_c3(v, 20).ok_or(Error::Truncated)?,
            tex_coords: C2 {
                x: v.f32_at(32).ok_or(Error::Truncated)?,
                y: v.f32_at(36).ok_or(Error::Truncated)?,
            },
        });
    }

    // Textures: 16 bytes each (type u32, flags u32, filename M2Array).
    let tex_avail = b.len().saturating_sub(textures.1 as usize);
    let mut texs = Vec::with_capacity(capped(textures.0 as usize, 16, tex_avail));
    for i in 0..textures.0 as usize {
        let t = get(textures.1 as usize + i * 16, 16)?;
        let ttype = M2TextureType::from_u32(t.u32_at(0).ok_or(Error::Truncated)?);
        // `+0x04` — the ADDRESS MODE, and it decides silhouettes, not just tiling. Bit 0 = repeat U,
        // bit 1 = repeat V; clear = clamp to edge. Content authors a cutout card's UVs *outside*
        // `0..1` deliberately, so the border clamps to the texture's transparent edge and the card
        // fades out to nothing — sampled with repeat instead, those margins wrap around into the
        // opaque middle of the sheet and draw as solid geometry, with a hard seam where u crosses
        // the wrap. This field was documented in this very comment and read by nobody until B52
        // (the Dun Morogh snow-firs) was traced to it; decision 0763.
        let tflags = t.u32_at(4).ok_or(Error::Truncated)?;
        let (fcount, fofs) = (
            t.u32_at(8).ok_or(Error::Truncated)? as usize,
            t.u32_at(12).ok_or(Error::Truncated)? as usize,
        );
        let raw = b.bytes_at(fofs, fcount).unwrap_or(&[]);
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        let string = CString::new(&raw[..end]).unwrap_or_default();
        texs.push(M2Texture {
            texture_type: ttype,
            wrap_x: tflags & 0x1 != 0,
            wrap_y: tflags & 0x2 != 0,
            filename: M2ArrayString { string },
        });
    }

    // Materials (render flags): 4 bytes each (flags u16, blend_mode u16).
    let flags_avail = b.len().saturating_sub(render_flags.1 as usize);
    let mut mats = Vec::with_capacity(capped(render_flags.0 as usize, 4, flags_avail));
    for i in 0..render_flags.0 as usize {
        let m = get(render_flags.1 as usize + i * 4, 4)?;
        mats.push(M2Material {
            flags: M2RenderFlags(m.u16_at(0).ok_or(Error::Truncated)?),
            blend_mode: M2BlendMode(m.u16_at(2).ok_or(Error::Truncated)?),
        });
    }

    // Bones: 108 bytes each in v256 — header 12 (id i32, flags u32, parent i16, submesh u16; vanilla
    // has NO boneNameCRC field, unlike TBC+), then 3×28 pre-Wrath tracks, then pivot C3. flags @4,
    // pivot @96. The reference zeroes NaN pivot components (a corruption guard); mirror it.
    let bones_avail = b.len().saturating_sub(bones.1 as usize);
    let mut bone_list = Vec::with_capacity(capped(bones.0 as usize, 108, bones_avail));
    for i in 0..bones.0 as usize {
        let bn = get(bones.1 as usize + i * 108, 108)?;
        let mut pivot = rd_c3(bn, 96).ok_or(Error::Truncated)?;
        if pivot.x.is_nan() {
            pivot.x = 0.0;
        }
        if pivot.y.is_nan() {
            pivot.y = 0.0;
        }
        if pivot.z.is_nan() {
            pivot.z = 0.0;
        }
        bone_list.push(M2Bone {
            key_bone: bn.u32_at(0).ok_or(Error::Truncated)? as i32 as i16,
            flags: M2BoneFlags(bn.u32_at(4).ok_or(Error::Truncated)?),
            parent: bn.u16_at(8).ok_or(Error::Truncated)? as i16,
            pivot,
        });
    }

    // Attachments: 48 bytes each (id u32 @0, bone u32 @4, position C3 @8; a 28-byte visibility
    // M2Track follows and is skipped — see [`M2Attachment`]). `id`/`bone` are narrowed to `u16`; a
    // record whose id/bone doesn't fit, or whose bone indexes past `bone_list` (now fully parsed
    // above), is dropped rather than kept malformed — real attachment ids run ≤ 36, so this never
    // drops legitimate data.
    let att_avail = b.len().saturating_sub(attachments.1 as usize);
    let mut attachment_list = Vec::with_capacity(capped(attachments.0 as usize, 48, att_avail));
    for i in 0..attachments.0 as usize {
        let a = get(attachments.1 as usize + i * 48, 48)?;
        let id = a.u32_at(0).ok_or(Error::Truncated)?;
        let bone = a.u32_at(4).ok_or(Error::Truncated)?;
        let position = rd_c3(a, 8).ok_or(Error::Truncated)?;
        let (Ok(id), Ok(bone)) = (u16::try_from(id), u16::try_from(bone)) else {
            continue;
        };
        if bone as usize >= bone_list.len() {
            continue;
        }
        attachment_list.push(M2Attachment {
            id,
            bone,
            position: [position.x, position.y, position.z],
        });
    }

    // Event markers: 44 bytes each (identifier 4CC @0, data u32 @4, bone u32 @8, position C3 @12;
    // a 20-byte enabled M2TrackBase follows and is skipped). Same bone-range discipline as the
    // attachments above; kept in file order (queries take the first ident match).
    let ev_avail = b.len().saturating_sub(events.1 as usize);
    let mut event_markers = Vec::with_capacity(capped(events.0 as usize, 44, ev_avail));
    for i in 0..events.0 as usize {
        let e = get(events.1 as usize + i * 44, 44)?;
        let ident = [e[0], e[1], e[2], e[3]];
        let bone = e.u32_at(8).ok_or(Error::Truncated)?;
        let position = rd_c3(e, 12).ok_or(Error::Truncated)?;
        let Ok(bone) = u16::try_from(bone) else {
            continue;
        };
        if bone as usize >= bone_list.len() {
            continue;
        }
        event_markers.push(M2EventMarker {
            ident,
            bone,
            position: [position.x, position.y, position.z],
        });
    }

    // AnimationLookup: one u16 each — the id → sequence-slot table `owns_animation` reads.
    let al_avail = b.len().saturating_sub(animation_lookup_arr.1 as usize);
    let mut animation_lookup =
        Vec::with_capacity(capped(animation_lookup_arr.0 as usize, 2, al_avail));
    for i in 0..animation_lookup_arr.0 as usize {
        animation_lookup.push(
            get(animation_lookup_arr.1 as usize + i * 2, 2)?
                .u16_at(0)
                .ok_or(Error::Truncated)?,
        );
    }

    // PlayableAnimationLookup (decision 0082): one dword each, low16 resolved id / high16 dir flags.
    let pal_avail = b
        .len()
        .saturating_sub(playable_animation_lookup_arr.1 as usize);
    let mut playable_animation_lookup = Vec::with_capacity(capped(
        playable_animation_lookup_arr.0 as usize,
        4,
        pal_avail,
    ));
    for i in 0..playable_animation_lookup_arr.0 as usize {
        let dword = get(playable_animation_lookup_arr.1 as usize + i * 4, 4)?
            .u32_at(0)
            .ok_or(Error::Truncated)?;
        playable_animation_lookup.push(M2PlayableAnim {
            resolved_id: (dword & 0xffff) as u16,
            dir_flags: (dword >> 16) as u16,
        });
    }

    // Texture lookup table: u16 each.
    let tlt_avail = b.len().saturating_sub(texture_lookup_table.1 as usize);
    let mut tlt = Vec::with_capacity(capped(texture_lookup_table.0 as usize, 2, tlt_avail));
    for i in 0..texture_lookup_table.0 as usize {
        tlt.push(
            get(texture_lookup_table.1 as usize + i * 2, 2)?
                .u16_at(0)
                .ok_or(Error::Truncated)?,
        );
    }

    // Each M2Color (stride 0x38) is an RGB **colour** track @ +0x00 (C3Vector keys) + an **alpha**
    // track @ +0x1c (fix16); each M2TextureWeight (stride 0x1c) is a single **weight** track. The
    // alpha/weight values gate batch visibility (a constant 0 hides the batch — wow-re
    // `m2-alpha-combine-cull`); the RGB is the per-batch tint multiplied into the vertex colour.
    // `track_fix16`/`track_vec3_timed` bounds-check internally and return an empty key list for
    // an out-of-range track (kept exactly — real art relies on that "no keys" tolerance); the
    // reservation for the *outer* per-record `Vec` is capped the same way as the arrays above.
    let colors_avail = b.len().saturating_sub(colors_arr.1 as usize);
    let colors_cap = capped(colors_arr.0 as usize, 0x38, colors_avail);
    let mut color_alpha_tracks = Vec::with_capacity(colors_cap);
    let mut color_rgb_tracks = Vec::with_capacity(colors_cap);
    for i in 0..colors_arr.0 as usize {
        color_alpha_tracks.push(track_fix16(b, colors_arr.1 as usize + i * 0x38 + 0x1c));
        color_rgb_tracks.push(track_vec3_timed(b, colors_arr.1 as usize + i * 0x38));
    }
    let transparency_avail = b.len().saturating_sub(transparency_arr.1 as usize);
    let transparency_cap = capped(transparency_arr.0 as usize, 0x1c, transparency_avail);
    let mut transparency_tracks = Vec::with_capacity(transparency_cap);
    for i in 0..transparency_arr.0 as usize {
        transparency_tracks.push(track_fix16(b, transparency_arr.1 as usize + i * 0x1c));
    }
    let tlookup_avail = b.len().saturating_sub(transparency_lookup_arr.1 as usize);
    let mut transparency_lookup =
        Vec::with_capacity(capped(transparency_lookup_arr.0 as usize, 2, tlookup_avail));
    for i in 0..transparency_lookup_arr.0 as usize {
        transparency_lookup.push(
            get(transparency_lookup_arr.1 as usize + i * 2, 2)?
                .u16_at(0)
                .ok_or(Error::Truncated)?,
        );
    }

    // Each M2TextureTransform (header 0x74, stride 0x54) is 3 back-to-back M2Tracks — translation
    // (C3Vector) @+0x00, rotation (quaternion) @+0x1c, scaling (C3Vector) @+0x38 (wow-re models.md
    // element table). Same "no keys" tolerance as the colour/weight tracks above.
    let ttf_avail = b.len().saturating_sub(tex_anim_arr.1 as usize);
    let mut texture_transforms =
        Vec::with_capacity(capped(tex_anim_arr.0 as usize, 0x54, ttf_avail));
    for i in 0..tex_anim_arr.0 as usize {
        let base = tex_anim_arr.1 as usize + i * 0x54;
        texture_transforms.push(M2TextureTransform {
            translation: track_vec3_timed(b, base),
            rotation: track_quat(b, base + 0x1c),
            scaling: track_vec3_timed(b, base + 0x38),
        });
    }
    let talookup_avail = b.len().saturating_sub(tex_anim_lookup_arr.1 as usize);
    let mut texture_transform_lookup =
        Vec::with_capacity(capped(tex_anim_lookup_arr.0 as usize, 2, talookup_avail));
    for i in 0..tex_anim_lookup_arr.0 as usize {
        texture_transform_lookup.push(
            get(tex_anim_lookup_arr.1 as usize + i * 2, 2)?
                .u16_at(0)
                .ok_or(Error::Truncated)?,
        );
    }

    // Global-sequence durations (header `0x14` count / `0x18` offset, u32 ms each): the free-running
    // loop clocks a `gseq`-tagged track wraps on (`t mod global_sequences[gseq]`). Bounds-tolerant
    // like the tracks: a truncated table yields what fits.
    let gseq_arr = (
        b.u32_at(0x14).ok_or(Error::Truncated)?,
        b.u32_at(0x18).ok_or(Error::Truncated)?,
    );
    let gseq_avail = b.len().saturating_sub(gseq_arr.1 as usize);
    let mut global_sequences = Vec::with_capacity(capped(gseq_arr.0 as usize, 4, gseq_avail));
    for i in 0..gseq_arr.0 as usize {
        let Some(ms) = b.u32_at(gseq_arr.1 as usize + i * 4) else {
            break;
        };
        global_sequences.push(ms);
    }

    // Collision hull: raw bytes (triangles = count×2 u16, vertices = count×12 C3Vector). Many props
    // carry none (count 0) → empty. `get(..)` is already bounds-checked before the `to_vec()`, so a
    // corrupt count fails at the checked slice, never at an unbounded allocation.
    let bt_bytes = get(
        bounding_triangles.1 as usize,
        bounding_triangles.0 as usize * 2,
    )?
    .to_vec();
    let bv_bytes = get(
        bounding_vertices.1 as usize,
        bounding_vertices.0 as usize * 12,
    )?
    .to_vec();

    Ok(M2Format {
        model: M2Model {
            vertices: verts,
            textures: texs,
            materials: mats,
            color_alpha_tracks,
            color_rgb_tracks,
            transparency_tracks,
            transparency_lookup,
            texture_transforms,
            texture_transform_lookup,
            global_sequences,
            bones: bone_list,
            raw_data: M2RawData {
                texture_lookup_table: tlt,
                bounding_triangles: bt_bytes,
                bounding_vertices: bv_bytes,
            },
            header: M2Header {
                bounding_box_min,
                bounding_box_max,
                bounding_sphere_radius,
                collision_box_min,
                collision_box_max,
                collision_sphere_radius,
            },
            pivot_attach_z,
            attachments: attachment_list,
            event_markers,
            animation_lookup,
            playable_animation_lookup,
            views,
            version,
        },
    })
}

/// The model-space **Z** of an M2 attachment selected by **attachment id** (not array index) — the
/// follow-camera pivot reads id 17 this way. `attachments`/`lookup` are the header `(count, offset)`
/// pairs; the id → array-index map (`lookup`, `u16` each) picks the record, whose `position` C3 sits at
/// `+8` (vanilla stride 48: `id u32 · bone u32 · position C3 · animate M2Track`). `None` when the id has
/// no lookup slot or the record/bytes are out of range.
fn attachment_z(b: &[u8], id: usize, attachments: (u32, u32), lookup: (u32, u32)) -> Option<f32> {
    if id >= lookup.0 as usize {
        return None;
    }
    let li = lookup.1 as usize + id * 2;
    let idx = b.u16_at(li)? as usize;
    if idx >= attachments.0 as usize {
        return None;
    }
    let rec = attachments.1 as usize + idx * 48;
    // position C3 at rec+8 → its Z is rec+16.
    b.f32_at(rec + 16)
}

#[cfg(test)]
mod tests;
