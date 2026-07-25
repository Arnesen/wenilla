//! The M2 render-batch assembly loop: [`parse_m2_render_submeshes`] walks a parsed M2's skin
//! batches building one [`RenderSubmesh`] each — resolving its texture/blend/render-flags, baking the
//! batch's `alpha_anim` (decision 0130 phase 2, via [`mat_anim`](super::mat_anim)) and `uv_anim`
//! (phase 3, via [`tex_anim`](super::tex_anim)), and splitting a batch across billboard bones so each
//! glow card rotates about its own pivot. [`load_m2_mesh`]/[`load_m2_mesh_skinned`] are the
//! chain-reading entry points that feed it.

use std::io::Cursor;

use anyhow::{Context, Result};
use benilla_bytes::ByteExt;
use benilla_m2::parse_m2;
use benilla_m2::M2TextureType;

use crate::Chain;

use super::key_anim::SeqSlot;
use super::mat_anim;
use super::tex_anim;
use super::{le_u16, le_u32, model_path, remap_submesh};
use super::{
    AlphaAnim, Billboard, BillboardKind, BoneScaleAnim, CharSkinSlot, FogPolicy, ModelBlend,
    RenderSubmesh,
};

/// Directory portion of an internal path (everything before the last separator), `""` if none.
fn parent_dir(path: &str) -> &str {
    match path.rfind(['\\', '/']) {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Resolve a batch's texture to a `.blp` path. `Hardcoded` textures carry their own filename;
/// `Monster1/2/3` are blank in the M2 and come from the creature's skin variations (resolved to
/// `<model-dir>\<name>.blp`), falling back to any embedded filename.
fn resolve_texture(
    tex: &benilla_m2::M2Texture,
    dir: &str,
    skins: &[Option<String>],
) -> (Option<String>, Option<u8>, Option<CharSkinSlot>) {
    let embedded = {
        let f = tex.filename.string.to_string_lossy();
        (!f.is_empty()).then(|| f.into_owned())
    };
    let variation = |i: usize| {
        skins
            .get(i)
            .and_then(|o| o.clone())
            .map(|name| format!("{dir}\\{name}.blp"))
    };
    // Character runtime texture slots — no embedded path; the client fills them per-player from the
    // appearance (decisions 0041 / 0044 / 0045) or, for `Object`, from the runtime-bound item/cape
    // skin (decision 0072). M2 texture type 1 = the composited body atlas; type 2 = the object skin;
    // type 6 = the hair-mesh texture; type 8 = the extra skin (the tauren fur BLP). Flagged so the
    // spawn site can swap in the right material.
    let char_slot = match tex.texture_type {
        M2TextureType::Other(1) => Some(CharSkinSlot::Body),
        M2TextureType::Other(2) => Some(CharSkinSlot::Object),
        M2TextureType::Other(6) => Some(CharSkinSlot::Hair),
        M2TextureType::Other(8) => Some(CharSkinSlot::SkinExtra),
        _ => None,
    };
    // Skin slots resolve to the variation if supplied, else the (usually empty) embedded name; the slot
    // is reported so a skin-less load can fill it at spawn.
    match tex.texture_type {
        M2TextureType::Monster1 => (variation(0).or(embedded), Some(0), None),
        M2TextureType::Monster2 => (variation(1).or(embedded), Some(1), None),
        M2TextureType::Monster3 => (variation(2).or(embedded), Some(2), None),
        _ => (embedded, None, char_slot),
    }
}

/// A fix16 visibility track's **static** value: `None` if time-varying (can't decide visibility at
/// build time), else `Some(constant)` — an empty track (0 keys) means the factor doesn't apply, so
/// it reads as `1.0`. Used to fold a batch's colour-alpha / transparency-weight into the cull test.
fn track_constant(track: &benilla_m2::M2ScalarTrack) -> Option<f32> {
    if track.keys.is_empty() {
        return Some(1.0);
    }
    track.constant()
}

/// Normalise a vertex's 4 raw M2 bone weights (`u8`, summing ~255) to `f32` summing to 1.0 — what the
/// GPU skin blend expects. A vertex with no weight (sum 0, e.g. a stray static vertex) binds fully to
/// bone 0 so it rides the root rather than collapsing to the origin.
fn normalize_weights(w: [u8; 4]) -> [f32; 4] {
    let sum = w.iter().map(|&x| f32::from(x)).sum::<f32>();
    if sum <= 0.0 {
        return [1.0, 0.0, 0.0, 0.0];
    }
    [
        f32::from(w[0]) / sum,
        f32::from(w[1]) / sum,
        f32::from(w[2]) / sum,
        f32::from(w[3]) / sum,
    ]
}

/// Read bone `bone_idx`'s **scale** track (M2Track<C3Vector> at `bone + 0x44`) as a global-sequence
/// [`BoneScaleAnim`], straight from the raw M2 bytes. Returns `None` unless the track has >1 keys AND is
/// tagged to a global sequence (`gseq != 0xffff`) — the always-looping pulse case (see [`BoneScaleAnim`]).
///
/// MD20/bone offsets VERIFIED against wow-5875-re (`fields.md`: bones `count@0x34/ofs@0x38`, stride
/// `0x6c`, scale track `+0x44`; global sequences `count@0x14/ofs@0x18`) and the real `Lamppost.m2`. The
/// 28-byte vanilla M2Track tail: `interp@0`, `gseq@0x2`, `key{count@0xc, ofs@0x10}`, `val{count@0x14,
/// ofs@0x18}` (C3Vector values, stride 12) — the same track shape the particle emission tracks use.
fn parse_bone_scale_anim(bytes: &[u8], bone_idx: usize) -> Option<BoneScaleAnim> {
    let bone_count = bytes.u32_at(0x34)? as usize;
    let bones_ofs = bytes.u32_at(0x38)? as usize;
    if bone_idx >= bone_count {
        return None;
    }
    let track = bones_ofs.checked_add(bone_idx * 0x6c)?.checked_add(0x44)?;
    let interp = bytes.u16_at(track)? != 0;
    let gseq = bytes.u16_at(track + 0x02)?;
    if gseq == 0xffff {
        return None; // armed-animation track, not a global-sequence loop — out of scope here
    }
    let nkeys = bytes.u32_at(track + 0x0c)? as usize;
    let ts_ofs = bytes.u32_at(track + 0x10)? as usize;
    let nval = bytes.u32_at(track + 0x14)? as usize;
    let val_ofs = bytes.u32_at(track + 0x18)? as usize;
    if nkeys <= 1 || nval < nkeys {
        return None; // static (≤1 key) — nothing to animate
    }
    // Resolve the global-sequence duration this track loops over.
    let gseq_count = bytes.u32_at(0x14)? as usize;
    let gseq_o = bytes.u32_at(0x18)? as usize;
    if gseq as usize >= gseq_count {
        return None;
    }
    let duration_ms = bytes.u32_at(gseq_o.checked_add(gseq as usize * 4)?)?;
    if duration_ms == 0 {
        return None;
    }
    let keys = (0..nkeys)
        .map(|k| {
            let t = bytes.u32_at(ts_ofs.checked_add(k * 4)?)?;
            let v = val_ofs.checked_add(k * 12)?;
            Some((
                t,
                [bytes.f32_at(v)?, bytes.f32_at(v + 4)?, bytes.f32_at(v + 8)?],
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(BoneScaleAnim {
        duration_ms,
        interp,
        keys,
    })
}

/// Read bone `bone_idx`'s **translation** track (M2Track<C3Vector> at `bone + 0x0c`) restricted to
/// one sequence's absolute time `band`, rebased to it as a loop: the window an arm plays on the
/// bone — anim 0 is the client's one-time load arm (the questgiver `?` marker's bob), anim 190 the
/// marker's raised variant (wow-re `questgiver-marker.md` Q4 / `doodad-anim-host.md`). Returns
/// `None` unless the track is a **sequence** track (`gseq == 0xffff` — the gseq case is the
/// armed-free loop `parse_bone_scale_anim` handles for scale) holding >1 key inside the band.
/// Offsets as [`parse_bone_scale_anim`]; the translation track sits at bone `+0x0c` (VERIFIED
/// wow-5875-re `fields.md`: the three `M2Track`s at `+0x0c/+0x28/+0x44`).
fn parse_bone_seq_translation(
    bytes: &[u8],
    bone_idx: usize,
    band: (u32, u32),
) -> Option<BoneScaleAnim> {
    let (start, end) = band;
    let duration_ms = end.checked_sub(start).filter(|d| *d > 0)?;
    let bone_count = bytes.u32_at(0x34)? as usize;
    let bones_ofs = bytes.u32_at(0x38)? as usize;
    if bone_idx >= bone_count {
        return None;
    }
    let track = bones_ofs.checked_add(bone_idx * 0x6c)?.checked_add(0x0c)?;
    let interp = bytes.u16_at(track)? != 0;
    if bytes.u16_at(track + 0x02)? != 0xffff {
        return None; // a global-sequence loop — not the armed-sequence case
    }
    let nkeys = bytes.u32_at(track + 0x0c)? as usize;
    let ts_ofs = bytes.u32_at(track + 0x10)? as usize;
    let nval = bytes.u32_at(track + 0x14)? as usize;
    let val_ofs = bytes.u32_at(track + 0x18)? as usize;
    if nval < nkeys {
        return None;
    }
    // The flat timestamp array concatenates every sequence's keys on one absolute timeline
    // (`read_bone_track`'s law) — keep only this band's, rebased to its start.
    let mut keys = Vec::new();
    for k in 0..nkeys {
        let t = bytes.u32_at(ts_ofs.checked_add(k * 4)?)?;
        if t < start || t > end {
            continue;
        }
        let v = val_ofs.checked_add(k * 12)?;
        keys.push((
            t - start,
            [bytes.f32_at(v)?, bytes.f32_at(v + 4)?, bytes.f32_at(v + 8)?],
        ));
    }
    if keys.len() <= 1 {
        return None; // static in this band — nothing to animate
    }
    Some(BoneScaleAnim {
        duration_ms,
        interp,
        keys,
    })
}

/// Load an M2 model from the chain, split into renderable submeshes (one per skin batch). Doodads
/// pass no skins; creatures use [`load_m2_mesh_skinned`] to fill their `Monster1/2/3` slots.
pub fn load_m2_mesh(chain: &mut Chain, raw_path: &str) -> Result<Vec<RenderSubmesh>> {
    load_m2_mesh_skinned(chain, raw_path, &[])
}

/// Like [`load_m2_mesh`], but fills the model's `Monster1/2/3` texture slots from `skins`
/// (CreatureDisplayInfo `textureVariation` names, in order).
pub fn load_m2_mesh_skinned(
    chain: &mut Chain,
    raw_path: &str,
    skins: &[Option<String>],
) -> Result<Vec<RenderSubmesh>> {
    let path = model_path(raw_path);
    let dir = parent_dir(&path).to_string();
    let bytes = chain
        .read_file(&path)
        .with_context(|| format!("reading M2 {path}"))?;
    parse_m2_render_submeshes(&bytes, &dir, skins).with_context(|| format!("parsing M2 {path}"))
}

/// Parse an **in-memory** M2 into render submeshes. `dir` is the model's directory (to resolve its
/// relative texture references); `skins` fill the `Monster1/2/3` texture slots (creature variations).
/// The bytes-in entry point used by the Bevy `AssetLoader`; [`load_m2_mesh_skinned`] is the
/// chain-reading wrapper.
pub fn parse_m2_render_submeshes(
    bytes: &[u8],
    dir: &str,
    skins: &[Option<String>],
) -> Result<Vec<RenderSubmesh>> {
    let format =
        parse_m2(&mut Cursor::new(bytes)).map_err(|e| anyhow::anyhow!("parsing M2: {e}"))?;
    let model = format.model();
    let skin = model
        .parse_embedded_skin(bytes, 0)
        .map_err(|e| anyhow::anyhow!("embedded skin: {e}"))?;

    let vertex = |g: u32| {
        let v = &model.vertices[g as usize];
        (
            [v.position.x, v.position.y, v.position.z],
            [v.normal.x, v.normal.y, v.normal.z],
            [v.tex_coords.x, v.tex_coords.y],
            [1.0, 1.0, 1.0, 1.0], // M2 has no MOCV; cleared below so no ATTRIBUTE_COLOR is emitted
        )
    };
    // `triangles` are local indices into `indices` (→ global vertex). A submesh owns a slice of it.
    let lookup = skin.indices();
    let tris = skin.triangles();
    let global = |t_range: std::ops::Range<usize>| -> Vec<u32> {
        tris.get(t_range)
            .unwrap_or(&[])
            .iter()
            .filter_map(|&t| lookup.get(t as usize).copied())
            .map(u32::from)
            .filter(|&g| (g as usize) < model.vertices.len())
            .collect()
    };

    // Every sequence's (anim id, absolute time band): entry stride 0x44, anim id u16 at +0x00,
    // band start/end u32 at +0x04/+0x08. The first entry's band is the loop a placed doodad's
    // one-time arm plays; sequence-timeline colour/weight tracks bake against it (`mat_anim`);
    // the full list feeds the billboard bones' per-sequence translation loops.
    let seq_bands: Vec<(u16, (u32, u32))> = {
        let (n, o) = (le_u32(bytes, 0x1c) as usize, le_u32(bytes, 0x20) as usize);
        (0..n)
            .map_while(|i| {
                let e = o + i * 0x44;
                (e + 0x44 <= bytes.len()).then(|| {
                    (
                        le_u16(bytes, e),
                        (le_u32(bytes, e + 0x04), le_u32(bytes, e + 0x08)),
                    )
                })
            })
            .collect()
    };
    // The same list as the bake's addressing unit: file slot + band. The slot is what indexes a
    // track's per-sequence key ranges, so it must stay the FILE order — no filtering.
    let seq_slots: Vec<SeqSlot> = seq_bands
        .iter()
        .enumerate()
        .map(|(index, &(_, band))| SeqSlot { index, band })
        .collect();
    // Slot 0 — the loop a placed doodad's one-time arm plays, and the only band the shared-material
    // UV/tint registries can key on (see `tex_anim::bake_uv_anim`).
    let seq0_slot = seq_slots.first().copied();

    let sections = skin.submeshes();
    let mut out = Vec::new();
    for batch in skin.batches() {
        let Some(section) = sections.get(batch.skin_section_index as usize) else {
            continue;
        };
        let start = section.triangle_start as usize;
        let global_indices = global(start..start + section.triangle_count as usize);
        if global_indices.is_empty() {
            continue;
        }
        let (texture, skin_slot, char_slot) = model
            .raw_data
            .texture_lookup_table
            .get(batch.texture_combo_index as usize)
            .and_then(|&ti| model.textures.get(ti as usize))
            .map(|t| resolve_texture(t, dir, skins))
            .unwrap_or((None, None, None));
        let material = model.materials.get(batch.material_index as usize);
        let blend = match material.map(|m| m.blend_mode.bits()) {
            Some(0) | None => ModelBlend::Opaque,
            Some(1) => ModelBlend::AlphaTest,
            // Modes 5/6 are the MULTIPLY blends (wow-re `m2-depth-blend-state`, the DAT_00811fe0
            // remap): 5 Mod → DST_COLOR/ZERO, 6 Mod2x → DST_COLOR/SRC_COLOR — the ARMORREFLECT
            // weapon/armor sheen layers. Collapsing them into alpha-Blend painted the reflect
            // texture OVER the blade instead of modulating it (decision 0528).
            Some(5) => ModelBlend::Mod,
            Some(6) => ModelBlend::Mod2x,
            Some(_) => ModelBlend::Blend,
        };
        // M2 blend mode 3 (NoAlphaAdd) / 4 (Add) are **additive** — the renderer adds the batch's colour
        // instead of mixing the background in (glow cards, coronae). Verified: the lamppost glow card
        // (GLOW32.BLP, warm amber) carries blend mode 4.
        let additive = matches!(material.map(|m| m.blend_mode.bits()), Some(3) | Some(4));
        // M2 render flag 0x04 = "Two-sided (no backface culling)" (wowdev.wiki M2 § Render flags).
        // Unset ⇒ the real client backface-culls this batch (visible from one side only).
        let two_sided = material.is_some_and(|m| m.flags.bits() & 0x04 != 0);
        // M2 render flag 0x01 = "Unlit" — lighting disabled, so the batch shows at full texture
        // brightness regardless of sun/ambient: lamp glass, window panes, glow cards. Reuses the same
        // fullbright `emissive` path as WMO self-illuminated glass. Verified: the lantern/wagon glass
        // batches (ElwynnLantern01.blp) carry 0x01; their bodies carry 0x00.
        let emissive = material.is_some_and(|m| m.flags.bits() & 0x01 != 0);
        // M2 render flags **0x10 (no depth-write)** / **0x08 (no depth-test)** — the real client keys
        // per-batch depth state on these bits, NOT on the blend mode (VERIFIED, wow-re
        // `m2-depth-blend-state`): every batch writes depth + tests LEQUAL unless its own bit clears it.
        let no_depth_write = material.is_some_and(|m| m.flags.bits() & 0x10 != 0);
        let no_depth_test = material.is_some_and(|m| m.flags.bits() & 0x08 != 0);
        // The batch's fog COLOUR policy (wow-re rf-weather-emission-timeline ROUND 4, byte-cited): the
        // M2 batch state setter `0x70baf0` disables fog outright when render flag 0x02 is set
        // (`0x70bb24`); otherwise the fog colour follows the blend mode via the policy table
        // `DAT_811fc4 = {1,1,1,2,2,3,4}` dispatched at `0x70bddf`/jump table `0x70c17c` — blend modes
        // 0/1/2 (opaque/alphakey/alpha) fog toward the scene colour, 3/4 (Add/AddAlpha) toward BLACK
        // (an additive batch FADES under a veil instead of gaining grey — the storm-veil level-up fix),
        // 5 (Mod) toward WHITE, 6 (Mod2x) toward GREY. Fog start/end stay the scene values regardless.
        let fog_policy = match material {
            Some(m) if m.flags.bits() & 0x02 != 0 => FogPolicy::Off,
            Some(m) => match m.blend_mode.bits() {
                3 | 4 => FogPolicy::Black,
                5 => FogPolicy::White,
                6 => FogPolicy::Grey,
                _ => FogPolicy::Scene,
            },
            None => FogPolicy::Scene,
        };
        // Static visibility cull (VERIFIED, wow-re `m2-alpha-combine-cull`): the real client multiplies
        // a per-batch alpha `A = instanceAlpha · colorAlpha · transparencyWeight` and **skips the batch
        // when `A ≤ 0`** — *before* the blend mode is even read, so it culls even an Opaque batch. A
        // constant-0 colour-alpha or transparency-weight track is therefore invisible every frame: e.g.
        // `OrgrimmarFloatingEmbers`' 84-yd emitter box (weight track `[0.0]`). Drop it at build time
        // (instanceAlpha is 1 here; the runtime doodad-fade alpha is the renderer's separate concern).
        // Animated tracks (>1 key) can't be decided statically, so we keep them.
        let color_alpha = if (batch.color_index as usize) < model.color_alpha_tracks.len() {
            track_constant(&model.color_alpha_tracks[batch.color_index as usize])
        } else {
            Some(1.0) // colorIndex out of range (incl. 0xffff = none) ⇒ colour factor not applied
        };
        let weight = if batch.texture_count != 0 {
            model
                .transparency_lookup
                .get(batch.weight_combo_index as usize)
                .and_then(|&t| model.transparency_tracks.get(t as usize))
                .map_or(Some(1.0), track_constant)
        } else {
            Some(1.0) // textureCount 0 ⇒ transparency factor not applied (verified gate)
        };
        if let (Some(c), Some(w)) = (color_alpha, weight) {
            if c * w <= 0.0 {
                continue; // constant-invisible batch — the real client never draws it
            }
        }
        // The runtime half of the same combine (decision 0130 phase 2): whatever the static test
        // above could NOT fold away — a time-varying colour-alpha/weight loop, or a dimming non-1
        // constant — bakes to an [`AlphaAnim`] the app samples per instance on the model clock.
        //
        // Baked **once per sequence**: the tracks key on one absolute timeline that every sequence
        // carves a band out of, and the reference re-reads them from the *playing* sequence's key
        // window each frame. Baking only band 0 was right for a placed doodad (one arm, looped
        // forever) and wrong for every creature — a batch authored to appear only on death reads
        // its Stand value forever, so we draw geometry the reference hides.
        let alpha_anim = {
            let color_track = model.color_alpha_tracks.get(batch.color_index as usize);
            // textureCount 0 ⇒ transparency factor not applied (verified gate).
            let weight_track = (batch.texture_count != 0)
                .then(|| {
                    model
                        .transparency_lookup
                        .get(batch.weight_combo_index as usize)
                        .and_then(|&t| model.transparency_tracks.get(t as usize))
                })
                .flatten();
            let per_seq = seq_slots
                .iter()
                .map(|&slot| mat_anim::AlphaSeq {
                    color: color_track.and_then(|t| {
                        mat_anim::bake_scalar_anim(t, &model.global_sequences, Some(slot))
                    }),
                    weight: weight_track.and_then(|t| {
                        mat_anim::bake_scalar_anim(t, &model.global_sequences, Some(slot))
                    }),
                })
                .collect();
            AlphaAnim::new(per_seq)
        };
        // The batch's UV-animation loop (decision 0130 phase 3): batch combo → texAnimLookup →
        // translation track, on the same clocks. Carried on the submesh; the renderer applies it.
        let uv_anim = tex_anim::bake_uv_anim(model, batch.texture_transform_combo_index, seq0_slot);
        // The batch's animated RGB tint (the M2Color colour track's runtime half): only a
        // time-varying track bakes — a `Some` here *replaces* the static vertex tint below (a
        // spell effect's white-hot flash cooling to red would otherwise freeze on its first key).
        let rgb_anim = model
            .color_rgb_tracks
            .get(batch.color_index as usize)
            .and_then(|t| mat_anim::bake_rgb_anim(t, &model.global_sequences, seq0_slot));
        // M2 billboard bones: a batch's vertices ride billboard bones (glow cards, chains) that the
        // real client re-orients to the camera each frame — **each bone independently, about its own
        // pivot**. One batch often packs several such cards: a candelabra's candle glows share a single
        // glow-texture batch, each quad skinned to its own per-candle bone. So we **split the batch
        // into one submesh per billboard bone** (its pivot the rotation centre). Collapsing them onto a
        // single pivot (the old first-vertex approach) swings the off-pivot cards in arcs as the camera
        // turns, and the scale pulse slides them — the candelabra "glows flung across the room" bug.
        // Triangles not on a billboard bone stay one ordinary submesh, so non-billboard batches and a
        // lamppost's single glow are unchanged (one group).
        let make_billboard = |bone_idx: usize| -> Option<Billboard> {
            let bone = model.bones.get(bone_idx)?;
            Some(Billboard {
                pivot: [bone.pivot.x, bone.pivot.y, bone.pivot.z],
                bone: bone_idx as u16,
                kind: BillboardKind::from_bone_flags(bone.flags.bits())?,
                // The glow-card pulse: this bone's global-sequence scale track (e.g. the lamppost's
                // 0.86…1.04 breathe over its 1333 ms global sequence). `None` for a static card.
                scale_anim: parse_bone_scale_anim(bytes, bone_idx),
                // The bone's per-sequence translation loops (the questgiver bob: anim 0 low,
                // anim 190 raised) — carried on the batch; only the marker spawn site arms one
                // (see the field doc).
                seq_translations: seq_bands
                    .iter()
                    .filter_map(|&(id, band)| {
                        Some((id, parse_bone_seq_translation(bytes, bone_idx, band)?))
                    })
                    .collect(),
            })
        };
        // A vertex's primary bone, but only when it is a billboard bone (else `None` → the ordinary
        // group). Group this batch's triangles (chunks of 3 global indices) by that key, in first-
        // appearance order and preserving triangle order within each group.
        let primary_billboard_bone = |g: u32| -> Option<usize> {
            let b = model.vertices.get(g as usize)?.bone_indices[0] as usize;
            model
                .bones
                .get(b)
                .is_some_and(|bone| bone.is_billboard())
                .then_some(b)
        };
        let mut groups: Vec<(Option<usize>, Vec<u32>)> = Vec::new();
        for tri in global_indices.chunks_exact(3) {
            let key = primary_billboard_bone(tri[0]);
            if let Some(pos) = groups.iter().position(|(k, _)| *k == key) {
                groups[pos].1.extend_from_slice(tri);
            } else {
                groups.push((key, tri.to_vec()));
            }
        }
        // M2Color tint (header 0x54, the colour record's RGB track @ +0x00, selected by
        // `texUnit.colorIndex`): the per-batch colour the real client multiplies into the vertex
        // colour. A **constant** track bakes into this batch's vertex colours — dedup-safe (per-mesh,
        // not per-material) and the model shader already folds `ATTRIBUTE_COLOR` into `base_color` →
        // `albedo` on every path incl. the fullbright glow cards. **Load-bearing for additive glows on
        // a neutral glow texture**: the Orgrimmar bonfire's base glow is `GenericGlow_Alpha_128` (a
        // white-cored radial, no colour of its own), its warmth living entirely in this M2Color
        // `(0.93,0.66,0.10)` — without it the additive draw washes the bright core to white.
        // `0xffff`/out-of-range ⇒ no tint (the common case). A **time-varying** track instead rides
        // `rgb_anim` above (the vertex bake is skipped — the two would double-apply); the render side
        // seeds the material tint at the first key, so a lane that doesn't animate materials shows
        // exactly the old static bake. RGB only; alpha stays 1.0 — batch visibility/transparency is
        // already governed by the alpha-combine cull, the transparency weight, and the runtime doodad
        // fade, so folding the (possibly animated) M2Color alpha in here too would double-count.
        let color_tint: Option<[f32; 4]> = match &rgb_anim {
            Some(_) => None,
            None => model
                .color_rgb_tracks
                .get(batch.color_index as usize)
                .and_then(|t| t.keys.first())
                .map(|&(_, rgb)| [rgb[0], rgb[1], rgb[2], 1.0]),
        };
        for (bone, idx) in groups {
            let (mut sub, globals) = remap_submesh(
                idx.into_iter(),
                vertex,
                texture.clone(),
                blend,
                two_sided,
                false, // M2 has no interior/exterior group concept
                emissive,
            );
            sub.billboard = bone.and_then(make_billboard);
            sub.additive = additive; // M2 blend mode 3/4 → additive (glow cards)
            sub.no_depth_write = no_depth_write; // M2 render flag 0x10
            sub.no_depth_test = no_depth_test; // M2 render flag 0x08
            sub.fog_policy = fog_policy; // render flag 0x02 / the per-blend fog table
            sub.skin_slot = skin_slot; // so a skin-less load can fill this batch at spawn
            sub.geoset_id = section.id; // the skinSectionId — character geoset selection keys on it
            sub.char_slot = char_slot; // character runtime slot (body/hair) — filled per-player at spawn
                                       // Skeletal skin binding (decision 0019), per local vertex in `globals` order: the M2
                                       // vertex's 4 bone indices (global bone-array indices → joint indices directly) + their
                                       // normalised weights. The skinned-mesh builder uploads these; the static mesh ignores them.
            sub.joints = globals
                .iter()
                .map(|&g| {
                    let bi = model.vertices[g as usize].bone_indices;
                    [
                        u16::from(bi[0]),
                        u16::from(bi[1]),
                        u16::from(bi[2]),
                        u16::from(bi[3]),
                    ]
                })
                .collect();
            sub.weights = globals
                .iter()
                .map(|&g| normalize_weights(model.vertices[g as usize].bone_weights))
                .collect();

            // Tint via vertex colours when the batch carries an M2Color; else none — M2 has no MOCV, so
            // an empty vec means the renderer skips ATTRIBUTE_COLOR and the texture shows untinted.
            match color_tint {
                Some(c) => sub.vertex_colors = vec![c; sub.positions.len()],
                None => sub.vertex_colors.clear(),
            }
            sub.alpha_anim = alpha_anim.clone(); // every billboard-split group shares the batch's loops
            sub.uv_anim = uv_anim.clone();
            sub.rgb_anim = rgb_anim.clone();
            out.push(sub);
        }
    }
    // Invisible interaction-zone placeholder (the `SpellObject_InvisibleTrap` class — Fire-Festival
    // fury zones, rallying-cry triggers, tonk-game consoles…): a flat (degenerate render box) quad
    // whose every batch is **opaque** and textured solely with the engine utility-white `WHITE1.BLP`.
    // The real 1.12 client DRAWS this mesh, but at per-instance render alpha ≈0 (the doodad-fade alpha
    // slot drives it to ~0 here) so it is invisible — VERIFIED by a reference apitrace (wow-re
    // `object-layer/scratch/go-render-gate.md`, the firing-4 capstone: drawn alpha-blended at
    // α=8.5e-05). What *drives* that ≈0 alpha is an un-RE'd world-scene-render detail (open handoff),
    // so benilla reproduces the observed invisibility by recognising the placeholder asset and dropping
    // its geometry. Scoped tight on purpose: verified against all 1602 GameObject display models, ONLY
    // this placeholder matches — visible flat decals (orc sleep mats, pentagram circles, AQ door runes)
    // carry real textures + Blend/AlphaTest and are kept. Empirical stop-gap, not the client mechanism
    // (decision 0030).
    if is_white1_placeholder(
        model.header.bounding_box_min,
        model.header.bounding_box_max,
        &out,
    ) {
        out.clear();
    }
    Ok(out)
}

/// The invisible-interaction-zone placeholder fingerprint: a model with a **degenerate render box**
/// (zero extent on some axis — a flat quad) whose **every** render batch is opaque and textured solely
/// with the engine utility-white `WHITE1.BLP` (`SpellObject_InvisibleTrap` & kin). Such a model is the
/// real client's invisible-by-near-zero-alpha placeholder, drawn but unseen; benilla drops its geometry
/// to match. Returns `false` for an empty batch set, a 3D box, or any non-opaque / non-`WHITE1` batch
/// (so visible flat decals are kept). See the call site + decision 0030.
fn is_white1_placeholder(bbox_min: [f32; 3], bbox_max: [f32; 3], subs: &[RenderSubmesh]) -> bool {
    if subs.is_empty() {
        return false;
    }
    let degenerate = (0..3).any(|k| (bbox_max[k] - bbox_min[k]).abs() < 1e-3);
    degenerate
        && subs.iter().all(|s| {
            matches!(s.blend, ModelBlend::Opaque)
                && !s.additive
                && s.texture
                    .as_deref()
                    .is_some_and(|t| t.to_ascii_lowercase().ends_with("white1.blp"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo root's `WoW/Data` (gitignored; the real-data test skips when absent).
    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// The tauren body, straight off the real client data: its M2 type-8 (extra skin — the fur)
    /// batches must surface as [`CharSkinSlot::SkinExtra`] parts, in both authored flavors — the
    /// opaque single-sided fur core and the alpha-cut two-sided fringe cards — so the spawn site can
    /// bind the CharSections `_Extra` BLP per flavor. Unmapped, these batches carry no texture and
    /// draw flat white (the "cows are white" bug).
    #[test]
    fn tauren_fur_batches_carry_the_skin_extra_slot() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let subs = load_m2_mesh(&mut chain, "Character\\Tauren\\Male\\TaurenMale.m2")
            .expect("parse TaurenMale");
        let extra: Vec<_> = subs
            .iter()
            .filter(|s| s.char_slot == Some(CharSkinSlot::SkinExtra))
            .collect();
        assert!(
            extra.len() > 20,
            "the tauren fur is many batches, got {}",
            extra.len()
        );
        assert!(
            extra.iter().all(|s| s.texture.is_none()),
            "type 8 has no embedded path — the spawn site fills it"
        );
        assert!(
            extra
                .iter()
                .any(|s| matches!(s.blend, ModelBlend::Opaque) && !s.two_sided),
            "the opaque single-sided fur core flavor exists"
        );
        assert!(
            extra
                .iter()
                .any(|s| matches!(s.blend, ModelBlend::AlphaTest) && s.two_sided),
            "the alpha-cut two-sided fringe flavor exists"
        );
    }

    /// The Whirlwind Axe (display 22734 → `Axe_2H_Horde_C_01.m2`), straight off the real client
    /// data: three batches — handle + blade on the runtime Object skin, and the blade's
    /// ARMORREFLECT layer as **Mod2x** (material `{flags 0x10, blendMode 6}` — the multiply blend,
    /// decision 0528) with no depth write and NOT additive. Collapsing mode 6 into alpha-Blend is
    /// exactly the "flat pale blade" regression this pins against.
    #[test]
    fn whirlwind_axe_reflect_layer_is_mod2x() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let subs = load_m2_mesh(
            &mut chain,
            "Item\\ObjectComponents\\Weapon\\Axe_2H_Horde_C_01.m2",
        )
        .expect("parse Axe_2H_Horde_C_01");
        assert_eq!(subs.len(), 3, "handle + blade + reflect layer");
        let object: Vec<_> = subs
            .iter()
            .filter(|s| s.char_slot == Some(CharSkinSlot::Object))
            .collect();
        assert_eq!(
            object.len(),
            2,
            "handle + blade bind the item's Object skin"
        );
        let reflect = subs
            .iter()
            .find(|s| {
                s.texture
                    .as_deref()
                    .is_some_and(|t| t.to_ascii_uppercase().contains("ARMORREFLECT"))
            })
            .expect("the ARMORREFLECT layer exists");
        assert_eq!(reflect.blend, ModelBlend::Mod2x);
        assert!(!reflect.additive, "Mod2x is a multiply, not an add");
        assert!(reflect.no_depth_write, "render flag 0x10");
    }

    /// The questgiver `?` marker, straight off the real client data: ONE cylindrical-billboard
    /// bone with a 3-key translation bob per sequence — anim **0** (the load arm's low bob) and
    /// anim **190**, the authored **raised** bob the client arms while the unit shows an overhead
    /// name (wow-re `questgiver-marker.md` Q4; m2bones-probed key values: anim 0 spans WoW z
    /// 0.000→−0.089, anim 190 z +0.517→+0.427 — same key COUNT, different values, which is
    /// exactly how the earlier "seq 190 = same bob" count-only probe went wrong). Guards the
    /// band-restricted translation bake end-to-end.
    #[test]
    fn questionmark_marker_batch_carries_both_bob_loops() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let subs = load_m2_mesh(&mut chain, "Interface\\Buttons\\TalkToMeQuestionMark.mdx")
            .expect("parse TalkToMeQuestionMark");
        let bb = subs
            .iter()
            .find_map(|s| s.billboard.as_ref())
            .expect("the ? marker rides a billboard bone");
        assert!(matches!(bb.kind, BillboardKind::LockZ));
        let bob = |id: u16| {
            bb.seq_translations
                .iter()
                .find(|(a, _)| *a == id)
                .map(|(_, l)| l)
        };
        let low = bob(0).expect("the load-arm loop bakes");
        assert_eq!(low.duration_ms, 1533, "the first sequence's band");
        assert!(low.keys.len() >= 2, "a moving track: {:?}", low.keys);
        let (a, b) = (low.sample(0), low.sample(low.duration_ms / 2));
        assert_ne!(a, b, "the sampled offset varies across the loop");
        assert!(
            low.keys.iter().all(|(_, v)| v[2] <= 0.0),
            "anim 0 bobs at/below the attach point: {:?}",
            low.keys
        );
        let raised = bob(190).expect("the raised loop bakes");
        assert_eq!(raised.duration_ms, 1533, "same period as the low bob");
        assert!(
            raised.keys.iter().all(|(_, v)| v[2] > 0.4),
            "anim 190 is the authored raised bob: {:?}",
            raised.keys
        );
    }
}
