//! The **shared effect stream** — the one CPU vertex stream every dynamic-effect family writes
//! into each frame (particles, ribbons, and — since 0733 — the decal family, water foam, and
//! precipitation), and the draw records that slice it (decisions 0732 P1/P2, 0733).
//!
//! Before this, every effect owned a `Mesh` asset rewritten per frame: ~145 mesh `Modified`
//! events/frame at the LBRS pin, each a full free+realloc in Bevy's mesh allocator (no partial
//! update exists — `allocator.rs:650`), and together they held the `AssetChanged<Mesh3d>`
//! short-circuit open so every material type's specialization probe ran over its whole
//! population every frame. One shared buffer + directly-constructed `Transparent3d` items
//! (the bevy_ui_render shape) converts the whole family from population-priced to
//! change-priced: one `write_buffer` per frame, zero mesh assets, zero material assets.
//!
//! The write protocol: a system calls [`EffectQuads::begin`], pushes **world-space** vertices —
//! whole quads (4 corners in perimeter order, closed by the `[0,1,2, 0,2,3]` pattern) or a
//! triangle list — then commits one draw for the range ([`EffectQuads::commit_quads`] /
//! [`EffectQuads::commit_tris`]). The render half rebases every draw's vertices against its
//! target view's camera position before upload (0733 §2 — absolute coordinates through the view
//! transform shear thin geometry apart far from the origin; the precip module learned this
//! empirically at ~9000 yd), builds the frame's index stream in sorted-item order, and merges
//! sort-adjacent draws that share (pipeline, texture, light, fog) into single draw calls
//! (0732 P2). The sort key is [`EffectDraw::anchor`] view-z + [`EffectDraw::bias`] — the ladder
//! rungs (owner-last 0719/0721, the decal biases, foam's water tie-break) moved from material
//! `depth_bias` into the item key (`sky_order`'s sign law); the rasterizer half of the old
//! material `depth_bias` lives on as [`EffectDraw::raster_bias`] (the coplanar decals need it).

use std::ops::Range;

use benilla_formats::{ModelBlend, ParticleBlend};
use bevy::asset::AssetId;
use bevy::prelude::*;
use bevy::render::render_resource::Buffer;

/// One vertex of the shared lane. **World-space** position in the stream; the render-world
/// prepare pass rebases it camera-relative before upload (0733 §2), so instruments reading the
/// stream (depth probe, depth dump) always see world coordinates.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct EffectVertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    /// RAW authored gamma-space RGBA (the GAMMA LANE invariant, 0161) — alpha is the blend
    /// weight, never encoded.
    pub color: [f32; 4],
}

/// The lane's blend variants (0733 §4) — a superset of the file-format enums it serves:
/// [`ParticleBlend`]'s three, plus the multiplicative pair the decal family and rain need.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EffectBlend {
    /// `(SRC_ALPHA, ONE)` via premultiplied-alpha + the shader's gamma `rgb·a` fold (0160/0161).
    Add,
    /// Standard alpha blending.
    Alpha,
    /// No blend, depth-write ON — drawn in the transparent bracket at the owner rung (0719).
    Opaque,
    /// `dst · lerp(1, src, α)` — bevy's `AlphaMode::Multiply` state (`(Dst, 1−srcα)` + shader
    /// premultiply, bevy_pbr mesh.rs:2486): the blob shadow's `GL_DST_COLOR/GL_ZERO`-with-fade,
    /// and `ModelBlend::Mod` (0528) at α = 1.
    Multiply,
    /// `2·src·dst` — `(Dst, Src)`, 0528's factors; rain's verified state (rf-weather-render).
    Mod2x,
}

impl From<ParticleBlend> for EffectBlend {
    fn from(blend: ParticleBlend) -> Self {
        match blend {
            ParticleBlend::Add => EffectBlend::Add,
            ParticleBlend::Alpha => EffectBlend::Alpha,
            ParticleBlend::Opaque => EffectBlend::Opaque,
        }
    }
}

impl EffectBlend {
    /// The ground-fx mapping from a part's authored blend — `model_render.rs`'s law with the
    /// lane's two named approximations: `AlphaTest` folds to `Alpha` (no Mask variant here; the
    /// groundscan census says flat `Spells\` quads are blend batches), and the part renders unlit
    /// (spell fx are; a lit ground quad would differ — none observed).
    ///
    /// `additive` is a SECOND, non-optional input because [`ModelBlend`] cannot express additive:
    /// M2 blend modes 3/4 fold into its `Blend` variant (see its own doc, "Alpha-blended /
    /// additive"), and the material path recovers them from `model_render`'s separate
    /// `is_additive` flag. Taking only the enum made this function *unable* to be right for an
    /// additive batch — every `Spells\` ground quad is mode 4, so Arcane Explosion / Blast Wave /
    /// Battle Shout drew their black-backed additive art alpha-blended: an opaque black tile
    /// (decision 0748). Keeping it in the signature is what stops the next caller repeating it.
    pub fn from_model(blend: ModelBlend, additive: bool) -> Self {
        if additive {
            // `BLEND_ADD` is byte-for-byte the material path's additive: the shader gamma-
            // premultiplies (`rgb·α`) and returns α = 0, turning `(One, 1−srcα)` into pure
            // addition — the same fold `specialize` gates on marker bit 2 (0160/0161).
            return EffectBlend::Add;
        }
        match blend {
            ModelBlend::Opaque => EffectBlend::Opaque,
            ModelBlend::AlphaTest | ModelBlend::Blend => EffectBlend::Alpha,
            ModelBlend::Mod => EffectBlend::Multiply,
            ModelBlend::Mod2x => EffectBlend::Mod2x,
        }
    }
}

/// How a draw's vertex range is indexed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EffectTopology {
    /// Whole quads: 4 perimeter-order corners each, indexed `[b, b+1, b+2, b, b+2, b+3]`.
    Quads,
    /// A plain triangle list: identity indices (the decal projector's fans, rain's streaks).
    Tris,
}

/// The fog COLOUR policy for one draw — `params.x`/`params.y` of the effect shader; each
/// variant is one canonical row of the render-world params uniform.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EffectFog {
    /// File flag 0x8 ("unfogged") — fog disabled outright (params.x = 0). Also the decal
    /// family's and foam's verified state.
    Off,
    /// The ordinary day-night scene fog (params.x = 1) — Alpha/Opaque blends.
    Scene,
    /// Fog toward BLACK (params.x = 2) — an Add-blend emitter fades under a veil instead of
    /// gaining grey (the same per-blend fog table M2 batches take, `0x70baf0`).
    Black,
    /// Fog toward WHITE (params.x = 3) — the `0x70baf0` table's Mod policy (a multiplier fades
    /// to the identity, not to the scene colour). Ground-fx decals with `ModelBlend::Mod`.
    White,
    /// Fog toward GREY-0.5 (params.x = 4) — the table's Mod2x policy (grey is 2·src·dst's
    /// neutral). Ground-fx decals with `ModelBlend::Mod2x`.
    Grey,
    /// Rain's FORCED grey fog (scene fog off; params.y = 1 with zw = 70..75) — under Mod2x the
    /// grey-0.5 fog colour is neutral, so this IS the streak/patter distance fade
    /// (rf-weather-render Q3; the row values live with their law in `weather::precip`).
    Rain,
}

impl EffectFog {
    /// The policy for one particle/ribbon def — the exact table `particle_material` applied
    /// (file flag 0x8 wins, then Add ⇒ black, else scene).
    pub fn for_blend(flags: u32, blend: ParticleBlend) -> Self {
        if flags & 0x8 != 0 {
            EffectFog::Off
        } else if matches!(blend, ParticleBlend::Add) {
            EffectFog::Black
        } else {
            EffectFog::Scene
        }
    }

    /// The `0x70baf0` fog policy baked into a model material's `clutter_fade.z` bits 4..7
    /// (`wow_model.wgsl:761` decodes the same field): 0 scene, 1 black, 2 white, 3 grey,
    /// 4 unfogged — the ground-fx decal reads its part's authored policy through this.
    pub fn from_model_policy(policy: u32) -> Self {
        match policy {
            1 => EffectFog::Black,
            2 => EffectFog::White,
            3 => EffectFog::Grey,
            4 => EffectFog::Off,
            _ => EffectFog::Scene,
        }
    }

    /// The slot index into the render-world params uniform (one canonical `vec4` per policy).
    pub fn slot(self) -> u32 {
        match self {
            EffectFog::Off => 0,
            EffectFog::Scene => 1,
            EffectFog::Black => 2,
            EffectFog::White => 3,
            EffectFog::Grey => 4,
            EffectFog::Rain => 5,
        }
    }
}

/// A per-emitter light-buffer override: this emitter's fragment reads THIS `WowLight` blob
/// instead of the world's shared one. The glue-scene booths are the one author (decision 0539
/// §5 — their braziers are fogged by the SCENE's own light buffer, the ModelFFX fog that
/// covers the whole backdrop model); the call site inserts it on the spawned emitter entity.
#[derive(Component, Clone)]
pub struct EffectLightOverride(pub Buffer);

/// One draw of the shared lane: a contiguous vertex range, its texture/blend/fog identity, and
/// the sort point the render-world queue keys it by.
pub struct EffectDraw {
    /// The MAIN-world camera entity whose view this draw belongs to (the world camera, or a
    /// booth camera for a booth-layered emitter — the sim already resolves this per emitter).
    /// The render-world phase lookup keys on the retained view's main entity, so no
    /// `RenderLayers` plumbing is needed render-side.
    pub cam: Entity,
    pub texture: AssetId<Image>,
    pub blend: EffectBlend,
    pub topology: EffectTopology,
    pub fog: EffectFog,
    /// The cloud's sort point — the emitter anchor / ribbon head node / decal center, exactly
    /// the sort point the material path used.
    pub anchor: Vec3,
    /// The ladder rung added to the view-space sort distance — owner-last (0719/0721) for
    /// emitters, the decal constants (ring/ground-fx 8192, shadow 4096), foam's +1 water
    /// tie-break; `sky_order`'s sign law (positive draws later).
    pub bias: f32,
    /// The rasterizer `DepthBiasState` constant for this draw's pipeline (0733 §4): the
    /// coplanar decals keep the depth-offset half their materials carried (projected verts are
    /// exact sub-pieces of drawn ground — clip-interpolated vertices land within ULPs of it);
    /// everything free-floating passes 0.
    pub raster_bias: i32,
    /// Vertex range in [`EffectQuads::verts`] (a multiple of 4 for quads, 3 for tris).
    pub range: Range<u32>,
    /// The producing entity — the phase probe's identity for this item (`item.entity.1`, so a
    /// phase line still names the pool that produced it).
    pub main_entity: Entity,
    /// [`EffectLightOverride`]'s buffer, when the producer carries one (`None` = the world's
    /// shared light buffer).
    pub light: Option<Buffer>,
}

/// The frame's shared stream. Cleared at the top of `PostUpdate`'s effect set
/// ([`begin_effect_frame`]), filled by the family systems, copied to the render world in
/// `ExtractSchedule`.
#[derive(Resource, Default)]
pub struct EffectQuads {
    pub verts: Vec<EffectVertex>,
    pub draws: Vec<EffectDraw>,
}

/// Everything about one draw except its vertex range — the argument bundle `commit_quads` /
/// `commit_tris` close a range with.
pub struct EffectDrawSpec {
    pub cam: Entity,
    pub texture: AssetId<Image>,
    pub blend: EffectBlend,
    pub fog: EffectFog,
    pub anchor: Vec3,
    pub bias: f32,
    pub raster_bias: i32,
    pub main_entity: Entity,
    pub light: Option<Buffer>,
}

impl EffectQuads {
    /// Open a draw: remember where its vertices start.
    pub fn begin(&self) -> u32 {
        self.verts.len() as u32
    }

    /// Close a quad draw over everything pushed since `begin`. A range that gained no vertices
    /// commits nothing — the idle steady state costs zero here (the a5521180 law, structural).
    pub fn commit_quads(&mut self, start: u32, spec: EffectDrawSpec) {
        debug_assert_eq!((self.verts.len() as u32 - start) % 4, 0, "whole quads only");
        self.commit(start, EffectTopology::Quads, spec);
    }

    /// Close a triangle-list draw over everything pushed since `begin`.
    pub fn commit_tris(&mut self, start: u32, spec: EffectDrawSpec) {
        debug_assert_eq!(
            (self.verts.len() as u32 - start) % 3,
            0,
            "whole triangles only"
        );
        self.commit(start, EffectTopology::Tris, spec);
    }

    fn commit(&mut self, start: u32, topology: EffectTopology, spec: EffectDrawSpec) {
        let end = self.verts.len() as u32;
        if end > start {
            self.draws.push(EffectDraw {
                cam: spec.cam,
                texture: spec.texture,
                blend: spec.blend,
                topology,
                fog: spec.fog,
                anchor: spec.anchor,
                bias: spec.bias,
                raster_bias: spec.raster_bias,
                range: start..end,
                main_entity: spec.main_entity,
                light: spec.light,
            });
        }
    }
}

/// Clear the stream for a new frame — scheduled before every family's writer, so the writer
/// order between them stays free.
pub fn begin_effect_frame(mut quads: ResMut<EffectQuads>) {
    quads.verts.clear();
    quads.draws.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The additive flag OVERRIDES the folded enum (decision 0748). `ModelBlend::Blend` means
    /// "alpha-blended **or** additive" — M2 modes 2/3/4 all land there — so a mapping that reads
    /// only the enum cannot be right. Every `Spells\` flat ground quad in the 1.12.1 corpus is
    /// mode 4 (`m2batch`: ArcaneExplosion_Base, BattleShout_Cast_Base, …), and drawing those
    /// alpha-blended painted their black-backed additive art as an opaque black tile.
    #[test]
    fn additive_wins_over_the_folded_blend_enum() {
        for blend in [
            ModelBlend::Opaque,
            ModelBlend::AlphaTest,
            ModelBlend::Blend,
            ModelBlend::Mod,
            ModelBlend::Mod2x,
        ] {
            assert_eq!(
                EffectBlend::from_model(blend, true),
                EffectBlend::Add,
                "{blend:?} + additive must reach the pure-add state, not {:?}",
                EffectBlend::from_model(blend, false),
            );
        }
    }

    /// The non-additive law is unchanged — `model_render.rs`'s mapping, with `AlphaTest` folded
    /// to `Alpha` (the lane has no Mask variant; flat `Spells\` quads are blend batches).
    #[test]
    fn non_additive_keeps_the_material_paths_law() {
        assert_eq!(
            EffectBlend::from_model(ModelBlend::Opaque, false),
            EffectBlend::Opaque
        );
        assert_eq!(
            EffectBlend::from_model(ModelBlend::AlphaTest, false),
            EffectBlend::Alpha
        );
        assert_eq!(
            EffectBlend::from_model(ModelBlend::Blend, false),
            EffectBlend::Alpha
        );
        assert_eq!(
            EffectBlend::from_model(ModelBlend::Mod, false),
            EffectBlend::Multiply
        );
        assert_eq!(
            EffectBlend::from_model(ModelBlend::Mod2x, false),
            EffectBlend::Mod2x
        );
    }
}
