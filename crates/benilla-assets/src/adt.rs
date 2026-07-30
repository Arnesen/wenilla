//! ADT → terrain-tile asset loader.
//!
//! Decodes one vanilla ADT tile into an [`AdtTile`]: **one splat-blended `Mesh` per MCNK chunk**
//! (Bevy space, absolute world coords) plus its three `texture_2d_array`s — ground layers, per-chunk
//! alpha (blend) maps, per-chunk MCSH shadow maps — and the raw doodad/WMO placement lists (for the
//! spawn system to load as `M2Model`/`WmoModel`). Each chunk mesh carries its array indices on every
//! one of its vertices — `COLOR` (4 layer indices) and `UV1` (alpha index `.x`, shadow index `.y`,
//! `-1` = none) — which is why all 256 chunks of a tile still share **one** material; the app's
//! terrain material reads them.
//!
//! Layer textures are read via [`LoadContext::read_asset_bytes`] (raw bytes, dependency-tracked) and
//! packed by [`crate::terrain`] — the BLP's authored mips verbatim (the C2 fidelity invariant).
//! Materials, the collider, and the placement *models* are built by the app at spawn — this loader
//! produces decoded data only.

use std::collections::HashMap;

use benilla_formats::{
    adt_to_tile_mesh, blp_bytes_to_mip_chain, ChunkMesh, Doodad, WmoInstance, ALPHA_MAP_SIZE,
    SHADOW_MAP_SIZE,
};
use bevy::asset::io::Reader;
use bevy::asset::{Asset, AssetLoader, LoadContext, RenderAssetUsages};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::reflect::TypePath;

use crate::coords::wow_to_bevy;
use crate::terrain::{
    alpha_array_image, chain_to_layer, layer_array_image, shadow_array_image, solid_layer_chain,
    LayerTexture, LAYER_TEX_SIZE,
};

/// A loaded ADT terrain tile: one splat mesh per MCNK chunk + its three texture arrays, plus the raw
/// doodad/WMO placements (which the spawn system resolves to `M2Model`/`WmoModel` handles).
#[derive(Asset, TypePath)]
pub struct AdtTile {
    /// One terrain mesh per drawn MCNK chunk (Bevy space, absolute world coords) — the tile's 256
    /// cells, less any that a hole mask emptied. Layer indices ride vertex `COLOR`; the alpha and
    /// shadow array indices ride `UV1.x` / `UV1.y`, uniform across a chunk's own vertices.
    ///
    /// **Per chunk, not per tile, because the cull's unit is the chunk** (decision 0780): the
    /// exterior-scene cull tests one AABB per drawn object, and the reference's object there is the
    /// 33.333 yd cell (`0x683bf0`), never the 533 yd tile — a slab the camera stands on intersects
    /// every portal window, so it could never be hidden from inside a building. Nothing is duplicated
    /// by the split: the merged form never shared a vertex across a chunk boundary either.
    pub chunk_meshes: Vec<Handle<Mesh>>,
    /// `texture_2d_array` of the tile's ground textures (the C2-faithful authored mip chains).
    pub layer_array: Handle<Image>,
    /// `texture_2d_array` of per-chunk alpha (blend-weight) maps.
    pub alpha_array: Handle<Image>,
    /// `texture_2d_array` of per-chunk MCSH baked shadow maps.
    pub shadow_array: Handle<Image>,
    /// M2 doodad placements (raw WoW coords).
    pub doodads: Vec<Doodad>,
    /// WMO building placements (raw WoW coords).
    pub wmos: Vec<WmoInstance>,
    /// The tile's decoded per-MCNK chunks — the source the `chunk_meshes` + arrays were built from,
    /// kept resident so the app can derive what those forms drop: the terrain collider, the MCLQ
    /// liquid surfaces (each
    /// `ChunkMesh.liquid`) and the ground-clutter scatter (per-chunk layers/normals/MCSH + the
    /// app-side ground-effect catalog). Bounded by the loaded-tile count; the heaviest field on the
    /// asset, carried because clutter/liquid are app concerns (the loader stays catalog-independent).
    pub chunks: Vec<ChunkMesh>,
}

/// Bevy [`AssetLoader`] decoding a vanilla `*.adt` tile → [`AdtTile`].
#[derive(Default, TypePath)]
pub struct AdtLoader;

impl AssetLoader for AdtLoader {
    type Asset = AdtTile;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        ctx: &mut LoadContext<'_>,
    ) -> Result<AdtTile, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let to_io = |e: anyhow::Error| std::io::Error::other(format!("{e:#}"));
        let tile = adt_to_tile_mesh(&bytes).map_err(to_io)?;

        // Layer array: a solid-green fallback at index 0, then each unique referenced layer texture.
        let fallback = solid_layer_chain([107, 133, 82, 0]);
        let layer_mips = fallback.mip_count;
        let mut layer_buf = fallback.chain;
        let mut layer_count = 1u32;
        let mut layer_index: HashMap<String, u32> = HashMap::new();

        let mut alpha_buf: Vec<u8> = Vec::new();
        let mut alpha_count = 0u32;
        let mut shadow_buf: Vec<u8> = Vec::new();
        let mut shadow_count = 0u32;

        // One mesh per drawn chunk. COLOR carries the chunk's 4 layer indices, UV1 its alpha/shadow —
        // uniform over the chunk's own vertices, which is what keeps ONE material per tile.
        let mut chunk_meshes: Vec<Handle<Mesh>> = Vec::with_capacity(tile.chunks.len());

        for (i, chunk) in tile.chunks.iter().enumerate() {
            // A chunk the hole mask emptied draws nothing: no mesh, no entity, and no alpha/shadow
            // array slot either (the indices below are running counters, so skipping here is free).
            if chunk.indices.len() < 3 {
                continue;
            }
            let positions: Vec<[f32; 3]> = chunk
                .positions
                .iter()
                .map(|p| wow_to_bevy(*p).to_array())
                .collect();
            // Prefer authored MCNR normals (continuous across MCNK borders — no seams); else flat.
            let normals: Vec<[f32; 3]> = if chunk.normals.len() == chunk.positions.len() {
                chunk
                    .normals
                    .iter()
                    .map(|n| wow_to_bevy(*n).to_array())
                    .collect()
            } else {
                computed_normals(&positions, &chunk.indices)
            };

            // Resolve up to 4 layer textures to array indices (appending new ones). Missing slots
            // reuse layer 0 (its alpha weight is 0, so it never shows).
            let mut li = [0u32; 4];
            for (slot, name) in chunk.layer_textures.iter().take(4).enumerate() {
                let key = normalize_path(name);
                li[slot] = if let Some(&i) = layer_index.get(&key) {
                    i
                } else if let Some(layer) = decode_layer(ctx, &key).await {
                    layer_buf.extend_from_slice(&layer.chain);
                    layer_index.insert(key, layer_count);
                    layer_count += 1;
                    layer_count - 1
                } else {
                    0
                };
            }
            for slot in chunk.layer_textures.len().min(4)..4 {
                li[slot] = li[0];
            }

            let ai = match &chunk.alpha_map {
                Some(rgba) => {
                    alpha_buf.extend_from_slice(rgba);
                    alpha_count += 1;
                    alpha_count - 1
                }
                None => 0,
            };
            let si: f32 = match &chunk.shadow {
                Some(map) => {
                    shadow_buf.extend_from_slice(map);
                    shadow_count += 1;
                    (shadow_count - 1) as f32
                }
                None => -1.0,
            };

            let col = [li[0] as f32, li[1] as f32, li[2] as f32, li[3] as f32];
            let uv1 = [ai as f32, si];
            let n = positions.len();
            let mut mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            );
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, chunk.uvs.clone());
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, vec![uv1; n]);
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, vec![col; n]);
            mesh.insert_indices(Indices::U32(chunk.indices.clone()));
            chunk_meshes.push(ctx.add_labeled_asset(format!("chunk{i}"), mesh));
        }

        // A `texture_2d_array` needs ≥1 layer even when a tile carries no alpha / shadow maps.
        if alpha_count == 0 {
            alpha_buf = vec![0u8; (ALPHA_MAP_SIZE * ALPHA_MAP_SIZE * 4) as usize];
            alpha_count = 1;
        }
        if shadow_count == 0 {
            shadow_buf = vec![0u8; (SHADOW_MAP_SIZE * SHADOW_MAP_SIZE) as usize];
            shadow_count = 1;
        }

        let layer_array = layer_array_image(LAYER_TEX_SIZE, layer_count, layer_mips, layer_buf);
        let alpha_array = alpha_array_image(ALPHA_MAP_SIZE, alpha_count, alpha_buf);
        let shadow_array = shadow_array_image(SHADOW_MAP_SIZE, shadow_count, shadow_buf);

        Ok(AdtTile {
            chunk_meshes,
            layer_array: ctx.add_labeled_asset("layer_array".to_string(), layer_array),
            alpha_array: ctx.add_labeled_asset("alpha_array".to_string(), alpha_array),
            shadow_array: ctx.add_labeled_asset("shadow_array".to_string(), shadow_array),
            doodads: tile.doodads,
            wmos: tile.wmos,
            chunks: tile.chunks,
        })
    }

    fn extensions(&self) -> &[&str] {
        &["adt"]
    }
}

/// Decode a layer texture into the packed array form. Prefers the `_s` specular variant (sheen mask
/// in alpha); else falls back to the base BLP with alpha forced to 0 (matte — no sheen). `key` is a
/// normalized internal path.
async fn decode_layer(ctx: &mut LoadContext<'_>, key: &str) -> Option<LayerTexture> {
    if let Some(spec) = key.strip_suffix(".blp").map(|stem| format!("{stem}_s.blp")) {
        if let Ok(bytes) = ctx.read_asset_bytes(mpq_url(&spec)).await {
            if let Ok(chain) = blp_bytes_to_mip_chain(&bytes) {
                return Some(chain_to_layer(chain, LAYER_TEX_SIZE));
            }
        }
    }
    let bytes = ctx.read_asset_bytes(mpq_url(key)).await.ok()?;
    let chain = blp_bytes_to_mip_chain(&bytes).ok()?;
    let mut layer = chain_to_layer(chain, LAYER_TEX_SIZE);
    layer
        .chain
        .iter_mut()
        .skip(3)
        .step_by(4)
        .for_each(|a| *a = 0); // matte: no sheen mask
    Some(layer)
}

/// An internal (`\`/lowercase) path → an `mpq://` URL.
fn mpq_url(key: &str) -> String {
    format!("mpq://{}", key.replace('\\', "/"))
}

/// Normalize an internal asset path so case/slash variants share one layer-array index.
fn normalize_path(path: &str) -> String {
    path.replace('/', "\\").to_ascii_lowercase()
}

/// Flat per-chunk normals for the rare chunk lacking authored MCNR (most carry it).
fn computed_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut acc = vec![Vec3::ZERO; positions.len()];
    for tri in indices.chunks_exact(3) {
        let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let (va, vb, vc) = (
            Vec3::from(positions[a]),
            Vec3::from(positions[b]),
            Vec3::from(positions[c]),
        );
        let n = (vb - va).cross(vc - va);
        acc[a] += n;
        acc[b] += n;
        acc[c] += n;
    }
    acc.into_iter()
        .map(|n| n.normalize_or_zero().to_array())
        .collect()
}
