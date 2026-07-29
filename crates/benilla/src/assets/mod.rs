//! Shared asset foundation for the client: the **one** open patch chain plus dedup caches, and the
//! helpers that decode raw BLP pixels into Bevy textures + build per-submesh `WowModelMaterial`s.
//! Used by the ground-clutter build (which still reads models off the chain); the terrain streamer and
//! streamed entities load through the `benilla-assets` `mpq://` pipeline instead. Deduping by
//! path/material means each unique BLP is decoded + uploaded once, and submeshes sharing a texture+blend
//! share a material handle — which is what lets Bevy batch draws.
//!
//! (Pure relocation out of `main.rs`. The
//! `AssetPlugin` that opens the chain at startup lands in a later step.)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Buffer, Face};
use bevy::render::renderer::RenderDevice;

use crate::model_render::VANILLA_ALPHA_KEY_REF;
use crate::terrain::{WowModelExt, WowModelMaterial};
use benilla_formats::{open_chain, read_texture_mip_chain, read_texture_rgba, Chain, ModelBlend};

/// BLP/RGBA → Bevy `Image` texture helpers (+ solid/cursor/liquid-frame textures). Split out for size;
/// re-exported so callers keep using `crate::assets::{solid_layer_chain, …}`.
mod images;
pub(crate) use images::*;

/// Shared asset store: the **one** open patch chain plus dedup caches, used by every model/texture
/// load (terrain layers, doodads, WMOs, creatures, GameObjects). Replaces the three independent
/// chains we used to open. Deduping by path/material means each unique BLP is decoded + uploaded
/// once, and submeshes sharing a texture+blend share a `StandardMaterial` handle — which is what
/// lets Bevy batch their draw calls.
#[derive(Resource)]
pub(crate) struct WorldAssets {
    /// The **one** open patch chain, behind `Arc<Mutex<…>>` because the streaming loaders thread a
    /// `&mut Chain` (the `read_file` shim); the `Mutex` is `Send + Sync` glue, not a read serializer —
    /// `Chain` reads are `&self` and open a fresh OS handle, so they share no seek state.
    /// Main-thread reads lock via [`LockRecover`] (poison-tolerant); contention is ~nil since after
    /// startup the only main-thread reads are the occasional map-change WDL load + lazy clutter M2s.
    pub(crate) chain: Arc<Mutex<Chain>>,
    /// Decoded GPU textures by normalized path.
    /// Keyed by `(normalized path, wrap_u, wrap_v)`: the SAME BLP is legitimately sampled both
    /// ways by different models (a sheet that tiles on a wall and clamps on a cutout card), and the
    /// address mode lives on the Bevy `Image`'s sampler — so each mode needs its own upload
    /// (decision 0763). Before that key existed, whichever model loaded the texture first decided
    /// the mode for every later one.
    pub(crate) textures: HashMap<(String, bool, bool), Handle<Image>>,
    /// Decoded UI sprite textures by resolved path (`None` = a miss, cached so a bad path never
    /// re-walks the chain per frame). Separate from `textures`: sprites are sRGB/clamped, world
    /// art is Unorm/repeat — the same BLP can legitimately live in both.
    sprites: HashMap<String, Option<Handle<Image>>>,
    /// Decoded UI sprite textures sampled with **repeat** addressing (frame `Backdrop` pieces —
    /// tiled edges/bg). Separate cache from `sprites`: the *same* BLP can be wanted clamp-sampled as
    /// a plain sprite and repeat-sampled as a backdrop piece, and the two need distinct GPU images
    /// (the sampler is baked into the `Image`). See [`Self::sprite_texture_tiled`].
    tiled_sprites: HashMap<String, Option<Handle<Image>>>,
    /// Decoded **portrait** sprites — [`Self::sprite_texture`]'s clamp/sRGB sprite with a circular
    /// alpha mask baked in ([`portrait_image`]). Its own cache, like `tiled_sprites`: the mask bakes
    /// into the GPU image, so the same BLP wanted as a plain icon and as a portrait needs two images.
    portraits: HashMap<String, Option<Handle<Image>>>,
    /// Decoded coverage **masks** ([`mask_image`] — linear, not sRGB): the minimap's circular clip
    /// (decision 0203). Its own cache for the same reason as the others: the format is baked into
    /// the GPU image.
    masks: HashMap<String, Option<Handle<Image>>>,
    /// Materials deduped by their identity (texture path + blend, or the untextured fallback).
    pub(crate) model_materials: HashMap<MaterialKey, Handle<WowModelMaterial>>,
    /// The shared global-light storage buffer (`lighting::global_light`). Held here so `model_material`
    /// can clone it into every deduped model material's `light_buf` without threading a `Buffer` through
    /// the many call sites. One buffer for the whole scene, updated in place each frame.
    pub(crate) shared_light: Buffer,
}

/// Identity of a deduped model material: a textured material is uniquely determined by its texture
/// path, blend mode, sidedness, alpha-key cutoff, and clutter fade-distance; everything textureless
/// (missing/failed texture) shares one fallback. Cutoff + fade are in the key so ground clutter (lower
/// alpha ref + a distance fade) doesn't collide with a tree sharing the same texture.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum MaterialKey {
    /// `(texture, blend, two_sided, alpha_key_u8, fade_far_yd_u16, is_wmo, is_fade_variant,
    /// wrap_u, wrap_v)` — the address mode is part of the identity because it selects a different
    /// `Image` upload (decision 0763).
    Textured(String, ModelBlend, bool, u8, u16, bool, bool, bool, bool),
    Fallback(bool),
}

/// Normalize an internal asset path so case/slash variants of the same file share a cache entry
/// (MPQ lookup is case-insensitive and accepts either slash).
pub(crate) fn normalize_path(path: &str) -> String {
    path.replace('/', "\\").to_ascii_lowercase()
}

/// Mutate an asset only when `differs` says its current state doesn't already match.
///
/// `Assets::get_mut` alone marks the asset Modified — a uniform re-upload and (on the Metal
/// non-bindless path) a bind-group rebuild that frame — so a per-frame writer re-pushing an
/// unchanged value is pure cost (the teleport leak's CPU engine was exactly this shape; the sky
/// family idled at ~17 such writes per frame). The gate decides on the immutable view; pair it
/// with values quantized to display precision ([`quantize`]/[`quant255`]) or continuous inputs
/// defeat the compare.
pub(crate) fn write_gated<M: bevy::asset::Asset>(
    assets: &mut Assets<M>,
    handle: &Handle<M>,
    differs: impl Fn(&M) -> bool,
    apply: impl FnOnce(&mut M),
) {
    if assets.get(handle).is_none_or(differs) {
        if let Some(m) = assets.get_mut(handle) {
            apply(m);
        }
    }
}

/// Quantize to `n`-ths — the write-gate's precision floor for continuously-drifting inputs
/// (time-of-day bands, camera-aim envelopes). Choose `n` at or below what the output can show:
/// the reference packs its celestial/sky lanes to bytes (`floor(255·…)`, the 0xFF broadcasts)
/// and re-submits them as per-frame vertex data, so a 1/255 step IS the reference's own
/// precision for color/alpha; geometry scalars gate at a sub-pixel step instead.
pub(crate) fn quantize(x: f32, n: f32) -> f32 {
    (x * n).round() / n
}

/// [`quantize`] each channel to the display's 1/255 LSB — the byte precision the reference's own
/// color lanes carry.
pub(crate) fn quant255(c: [f32; 3]) -> [f32; 3] {
    [
        quantize(c[0], 255.0),
        quantize(c[1], 255.0),
        quantize(c[2], 255.0),
    ]
}

/// Lock a `Mutex`, recovering the guard even if a previous holder **poisoned** it by panicking.
///
/// The shared [`WorldAssets::chain`] is read from many systems; one panic mid-read would otherwise
/// poison the lock and turn every subsequent `lock().unwrap()` — including the render loop's — into a
/// cascade. The chain read is a best-effort decode (a half-read texture is harmless), so we recover
/// the guard and carry on. (Phase 3 deleted the net subsystem's shared mutexes entirely; this remains
/// only for the asset foundation, which is its one legitimate user.)
pub(crate) trait LockRecover<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> LockRecover<T> for Mutex<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl WorldAssets {
    /// Decode + upload a BLP once per path; later requests for the same texture share the handle.
    /// `None` if the texture is missing or fails to decode.
    pub(crate) fn texture(
        &mut self,
        path: &str,
        wrap: (bool, bool),
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        let key = (normalize_path(path), wrap.0, wrap.1);
        if let Some(handle) = self.textures.get(&key) {
            return Some(handle.clone());
        }
        let chain = read_texture_mip_chain(&mut self.chain.lock_recover(), &key.0).ok()?;
        let handle = images.add(repeat_texture_authored(chain, wrap));
        self.textures.insert(key, handle.clone());
        Some(handle)
    }

    /// Decode a BLP into a clamp-sampled **sRGB** sprite image (mip 0 only) — the celestial discs
    /// (sun/moon) and every player-UI quad. Unlike [`Self::texture`] (repeat-sampled, gamma-space
    /// `Unorm`, mipmapped — for tiling world art), this is sRGB + clamp-to-edge with no mip chain.
    /// Extensionless references get `.blp` appended (the FrameXML convention). Cached by resolved
    /// path — hits and misses both — in its own `sprites` map, separate from the `textures` cache
    /// (the same BLP can live in both, with different sampling).
    pub(crate) fn sprite_texture(
        &mut self,
        path: &str,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        // FrameXML/DBC texture references are canonically EXTENSIONLESS
        // (`Interface\Buttons\UI-Quickslot2`) — the real client appends `.blp` at resolve. Paths
        // that already carry an extension (`textures\moon.blp`) pass through unchanged.
        let mut key = normalize_path(path);
        if !key.rsplit('\\').next().is_some_and(|f| f.contains('.')) {
            key.push_str(".blp");
        }
        // Cached (hits AND misses): the UI extractor asks per quad per frame — uncached, every
        // frame re-decoded every BLP into a fresh `Image` (unbounded asset growth) and the fresh
        // handles defeated the quad-list dirty check.
        if let Some(cached) = self.sprites.get(&key) {
            return cached.clone();
        }
        let loaded = read_texture_rgba(&mut self.chain.lock_recover(), &key)
            .ok()
            .map(|(w, h, rgba)| images.add(sprite_image(w, h, rgba)));
        self.sprites.insert(key, loaded.clone());
        loaded
    }

    /// Decode a texture to raw RGBA8 `(w, h, bytes)` **without** uploading a GPU image — for callers
    /// that resample the pixels themselves before upload (the V-plate border sharpen, decision 0188,
    /// which resizes the 128×32 frame to the plate's exact size). Same extensionless→`.blp` resolve
    /// as [`Self::sprite_texture`]; not cached (a one-shot decode at load, not a per-frame ask).
    pub(crate) fn decode_rgba(&mut self, path: &str) -> Option<(u32, u32, Vec<u8>)> {
        let mut key = normalize_path(path);
        if !key.rsplit('\\').next().is_some_and(|f| f.contains('.')) {
            key.push_str(".blp");
        }
        read_texture_rgba(&mut self.chain.lock_recover(), &key).ok()
    }

    /// A UI sprite decoded with **repeat** (wrap) addressing — the frame `Backdrop` tiled pieces
    /// (`backdrop-mechanism.md`): a border edge strip samples UVs `[0..N]` and a tiled bg `[0..w/period]`,
    /// so the texture must wrap, not clamp. Same sRGB/no-mip decode + extensionless→`.blp` resolve
    /// as [`Self::sprite_texture`], but its own cache (the sampler is baked into the `Image`, so a
    /// path wanted both clamp and repeat needs two GPU images). Cached hits **and** misses.
    pub(crate) fn sprite_texture_tiled(
        &mut self,
        path: &str,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        let mut key = normalize_path(path);
        if !key.rsplit('\\').next().is_some_and(|f| f.contains('.')) {
            key.push_str(".blp");
        }
        if let Some(cached) = self.tiled_sprites.get(&key) {
            return cached.clone();
        }
        let loaded = read_texture_rgba(&mut self.chain.lock_recover(), &key)
            .ok()
            .map(|(w, h, rgba)| images.add(sprite_image_tiled(w, h, rgba)));
        self.tiled_sprites.insert(key, loaded.clone());
        loaded
    }

    /// A **portrait** sprite: [`Self::sprite_texture`]'s clamp/sRGB decode with a circular alpha mask
    /// baked in ([`portrait_image`]) — the unit-frame portrait, round like the client's. Same
    /// extensionless→`.blp` resolve; its own cache (the mask bakes into the GPU image, so a path
    /// wanted both plain and portrait needs two images). Cached hits **and** misses.
    pub(crate) fn portrait_texture(
        &mut self,
        path: &str,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        let mut key = normalize_path(path);
        if !key.rsplit('\\').next().is_some_and(|f| f.contains('.')) {
            key.push_str(".blp");
        }
        if let Some(cached) = self.portraits.get(&key) {
            return cached.clone();
        }
        let loaded = read_texture_rgba(&mut self.chain.lock_recover(), &key)
            .ok()
            .map(|(w, h, rgba)| images.add(portrait_image(w, h, rgba)));
        self.portraits.insert(key, loaded.clone());
        loaded
    }

    /// A coverage **mask** ([`mask_image`] — linear/clamp, the texel bytes handed to the shader
    /// 1:1): the minimap's `MinimapMask.blp` circle (decision 0203, [`crate::ui_pass::UiQuadMask`]).
    /// Same extensionless→`.blp` resolve as [`Self::sprite_texture`]; its own cache, hits AND misses.
    pub(crate) fn mask_texture(
        &mut self,
        path: &str,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        let mut key = normalize_path(path);
        if !key.rsplit('\\').next().is_some_and(|f| f.contains('.')) {
            key.push_str(".blp");
        }
        if let Some(cached) = self.masks.get(&key) {
            return cached.clone();
        }
        let loaded = read_texture_rgba(&mut self.chain.lock_recover(), &key)
            .ok()
            .map(|(w, h, rgba)| images.add(mask_image(w, h, rgba)));
        self.masks.insert(key, loaded.clone());
        loaded
    }

    /// Decode a cursor BLP (e.g. `Interface\Cursor\Point.blp`) into a single-mip GPU image for
    /// winit's custom OS cursor (`CursorIcon::Custom`) — *not* the mipmapped, repeat-sampled
    /// [`repeat_texture_authored`] used for world art. `None` if the file is missing or fails to decode.
    /// macOS doesn't use this (it drives `NSCursor` from the raw RGBA directly — see `crate::cursor`).
    #[cfg(not(target_os = "macos"))]
    pub(crate) fn decode_cursor(
        &mut self,
        path: &str,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        let (w, h, rgba) =
            read_texture_rgba(&mut self.chain.lock_recover(), &normalize_path(path)).ok()?;
        Some(images.add(cursor_texture(w, h, rgba)))
    }

    /// A `StandardMaterial` for a model submesh, deduped by (texture, blend) so submeshes/models
    /// sharing a texture share one material handle (enabling draw-call batching). A missing/failed
    /// texture falls back to a single shared untextured material.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn model_material(
        &mut self,
        texture: Option<&str>,
        blend: ModelBlend,
        two_sided: bool,
        alpha_cutoff: Option<f32>,
        fade_far: Option<f32>,
        is_wmo: bool,
        is_fade_variant: bool,
        // The batch's authored sampler address mode (`RenderSubmesh::wrap_x/wrap_y`).
        wrap: (bool, bool),
        images: &mut Assets<Image>,
        materials: &mut Assets<WowModelMaterial>,
    ) -> Handle<WowModelMaterial> {
        // Alpha-test cutoff: callers may override (ground clutter uses the detail-doodad ref ≈ 0.5);
        // everything else uses the general vanilla model alpha key.
        let cutoff = alpha_cutoff.unwrap_or(VANILLA_ALPHA_KEY_REF);
        // Ground-clutter distance fade (None for normal models). Near = 0.75× far (the client's ratio).
        let clutter_fade = match fade_far {
            Some(far) => Vec4::new(far * 0.75, far, 0.0, 1.0),
            None => Vec4::ZERO,
        };
        let resolved = texture.and_then(|t| {
            self.texture(t, wrap, images)
                .map(|h| (normalize_path(t), h))
        });
        // Only AlphaTest consumes the cutoff, so don't fragment opaque/blend materials by it.
        let key_alpha = if blend == ModelBlend::AlphaTest {
            (cutoff * 255.0).round() as u8
        } else {
            0
        };
        let key_fade = fade_far.map(|f| f.round() as u16).unwrap_or(0);
        let key = match &resolved {
            Some((path, _)) => MaterialKey::Textured(
                path.clone(),
                blend,
                two_sided,
                key_alpha,
                key_fade,
                is_wmo,
                is_fade_variant,
                wrap.0,
                wrap.1,
            ),
            None => MaterialKey::Fallback(is_wmo),
        };
        if let Some(handle) = self.model_materials.get(&key) {
            return handle.clone();
        }
        // Per-submesh blend (opaque trunk/wall vs alpha-cut leaves/windows). This builder serves
        // ground clutter, which never authors the multiply modes — Mod/Mod2x fall to plain Blend
        // here rather than growing this path the marker bits `model_render::model_material` packs
        // (that builder is the one every Mod-capable consumer uses; decision 0528).
        let alpha_mode = match blend {
            ModelBlend::Opaque => AlphaMode::Opaque,
            ModelBlend::AlphaTest => AlphaMode::Mask(cutoff),
            ModelBlend::Blend | ModelBlend::Mod | ModelBlend::Mod2x => AlphaMode::Blend,
        };
        // Single-sided unless the M2's 0x04 flag is set — matches the real client (many canopy planes
        // are visible from one direction only).
        let cull_mode = if two_sided { None } else { Some(Face::Back) };
        // The base StandardMaterial carries texture/alpha/cull; our `wow_model.wgsl` extension does
        // the WoW lighting (PBR fields like reflectance/roughness are ignored by it). Light uniforms
        // are placeholders — `apply_wow_lighting` fills them from Light.dbc + the panel knobs.
        let base = match resolved {
            Some((_, image)) => StandardMaterial {
                base_color_texture: Some(image),
                alpha_mode,
                double_sided: two_sided,
                cull_mode,
                ..default()
            },
            // No resolved texture → untextured WHITE draw (stage disabled in the reference —
            // wow-re m2-runtime-texture-null-bind.md; see model_render.rs for the full note).
            None => StandardMaterial {
                base_color: Color::WHITE,
                ..default()
            },
        };
        let handle = materials.add(WowModelMaterial {
            base,
            extension: WowModelExt {
                clutter_fade,
                // x = WMO (FFP N·L × MOCV, not SH); y = distance-fade blend variant (depth-write-on +
                // stable cutout via `specialize`/the shader — see WowModelKey).
                model_flags: Vec4::new(
                    if is_wmo { 1.0 } else { 0.0 },
                    if is_fade_variant { 1.0 } else { 0.0 },
                    0.0,
                    0.0,
                ),
                // x = doodad-lobe sun scale; `1.0` (lit) here — this builder serves clutter, which lights on
                // the FFP N·L path with its own baked MCSH tint, so the lobe's `sun_scale` is unused.
                sun_scale: Vec4::new(1.0, 0.0, 0.0, 0.0),
                tint: Vec4::ONE, // clutter never carries an animated M2Color tint (w: not a WMO batch)
                sidn: Vec4::ZERO, // clutter is never SIDN/WINDOW glass (WMO-only)
                // The shared global light (light/fog/SH come from here, updated in place each frame).
                light_buf: self.shared_light.clone(),
            },
        });
        self.model_materials.insert(key, handle.clone());
        handle
    }
}

/// World/render configuration read once at startup from the environment, shared by the subsystems
/// that need it: terrain reads `tile_radius` (streaming) + `tex_tiles` (splat tiling); lighting reads
/// `tile_radius` for the fog range. Inserted by [`AssetPlugin`] alongside [`WorldAssets`].
#[derive(Resource, Clone, Copy)]
pub(crate) struct RenderConfig {
    /// Tiles loaded in each direction around the player (`$WOW_TILE_RADIUS`, default 1).
    pub(crate) tile_radius: u32,
    /// Ground-texture repeats per chunk (`$WOW_TEX_TILES`, default 8).
    pub(crate) tex_tiles: f32,
}

/// Startup ordering seam: [`AssetPlugin`] opens the patch chain in this set so every other
/// subsystem's startup (catalog loads, terrain/lighting setup) can run `.after(AssetSet::Open)` and
/// borrow the shared [`WorldAssets`].
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum AssetSet {
    Open,
}

/// The asset foundation plugin: opens the **one** patch chain at startup and inserts the shared
/// [`WorldAssets`] + [`RenderConfig`] that the other subsystems build on.
pub(crate) struct AssetPlugin;

impl Plugin for AssetPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, open_world_assets.in_set(AssetSet::Open));
        app.add_systems(Update, evict_world_art);
    }
}

/// Drop the world-art dedup on a cross-map transition (`world_map::MapChange` — see its doc for
/// why a clear is always safe): `textures` + `model_materials` pin every map's world art forever
/// otherwise (the #bugs teleport leak). The UI sprite caches (`sprites`/`tiled_sprites`/
/// `portraits`/`masks`) stay — they are game-global UI scope, and their negative entries exist
/// precisely to stop per-frame re-walks of the chain.
fn evict_world_art(
    mut changes: MessageReader<crate::world_map::MapChange>,
    assets: Option<ResMut<WorldAssets>>,
) {
    if changes.is_empty() {
        return;
    }
    changes.clear();
    if let Some(mut a) = assets {
        a.textures.clear();
        a.model_materials.clear();
    }
}

/// Open the vanilla patch chain from `$WOW_DATA` (default `WoW/Data`) and insert the shared
/// [`WorldAssets`] (chain + dedup caches) + [`RenderConfig`]. If the client data can't be opened,
/// `WorldAssets` is simply absent and downstream startup falls back to an empty free-fly scene.
fn open_world_assets(mut commands: Commands, device: Res<RenderDevice>) {
    let data = PathBuf::from(std::env::var("WOW_DATA").unwrap_or_else(|_| "WoW/Data".into()));
    // Default 2 (a 5×5 block, ~1066 yd) so loaded terrain extends a buffer past the ~777 yd far-clip
    // wall — tiles are resident before the wall reveals them, no streaming pop-in. Geometry beyond the
    // wall is clipped/culled anyway. Lower $WOW_TILE_RADIUS to 1 to shrink the working set (less view
    // distance / better perf); raise it for more buffer.
    let tile_radius = std::env::var("WOW_TILE_RADIUS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    // Ground-texture repeats per chunk; tunable live in the panel afterward.
    let tex_tiles = std::env::var("WOW_TEX_TILES")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .unwrap_or(8.0);
    commands.insert_resource(RenderConfig {
        tile_radius,
        tex_tiles,
    });

    // The one shared global-light buffer, created here (RenderDevice is live by Startup) so it exists
    // before any material is built. Cloned into `WorldAssets` (for model materials) and inserted as the
    // `SharedLightBuffer` resource (the terrain streamer + the render-world upload read it). Always
    // inserted — even with no client data — so the render upload has a target; harmless if unused.
    let shared_light = crate::lighting::new_shared_light_buffer(&device);
    match open_chain(&data) {
        Ok(chain) => commands.insert_resource(WorldAssets {
            chain: Arc::new(Mutex::new(chain)),
            textures: HashMap::new(),
            sprites: HashMap::new(),
            tiled_sprites: HashMap::new(),
            portraits: HashMap::new(),
            masks: HashMap::new(),
            model_materials: HashMap::new(),
            shared_light: shared_light.0.clone(),
        }),
        Err(e) => error!("failed to open client data: {e:#}"),
    }
    commands.insert_resource(shared_light);
}
