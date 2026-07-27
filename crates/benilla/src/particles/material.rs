use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Buffer};
use bevy::shader::ShaderRef;

/// The particle/ribbon material: the `StandardMaterial` shell supplies the texture binding,
/// alpha mode and cull state; the extension swaps in the fragment that does the vanilla
/// gamma-space combine — `tex (Rgba8Unorm, authored bytes) × raw authored vertex colour`, emitted
/// raw (`shaders/wow_particle.wgsl`; the buffer holds gamma bytes end-to-end and the frame's one
/// decode is the FFXGlow combine — GAMMA LANE, 0161). StandardMaterial alone
/// can't express it: its unlit path assumes a linearised texture, which left the texture term
/// gamma-brightened on screen (the bonfire's dark authored smoke reading pale and thick).
pub type WowParticleMaterial = ExtendedMaterial<StandardMaterial, WowParticleExt>;

/// The extension: the gamma-combine fragment shader + the shared global light/fog buffer (the
/// reference fogs particle quads with the SAME scene day-night fog as world geometry — wow-re
/// `part-scene-multipliers.md`, decision 0155; missing fog was the "night smoke glows" bug).
#[derive(Asset, AsBindGroup, Clone, TypePath)]
#[bind_group_data(WowParticleKey)]
pub struct WowParticleExt {
    /// `lighting::global_light`'s storage buffer — the same binding every world material carries.
    #[storage(90, read_only, buffer, visibility(fragment))]
    pub light_buf: Buffer,
    /// Per-material params: `x` = the fog COLOUR policy, shifted +1 off [`benilla_formats::FogPolicy`]'s
    /// own discriminants so `0.0` keeps its established meaning (fog off — the water foam, whose
    /// reference render state disables fog, `0x68fcc1`): `1.0` = scene (particles/ribbons — the
    /// ordinary day-night fog), `2.0` = black (an `Add`-blend emitter fades under a veil instead of
    /// gaining grey — the SAME per-blend fog table M2 batches take, `0x70baf0`, wow-re
    /// rf-weather-emission-timeline ROUND 4, refuting models.md:782's "no per-blend fog"; `3.0`/`4.0`
    /// = white/grey, unused here — no M2 particle blend distinguishes Mod/Mod2x today). `y` =
    /// forced-fog mode (rain, render-state ids 0x0a/0x0b/0x0d: fog toward grey-0.5 over the fixed
    /// `z..w` window regardless of scene fog — the streak's distance fade under Mod2x; wow-re
    /// `rf-weather-render.md`). `zw` = forced fog start/end (`y > 0.5`).
    #[uniform(91)]
    pub params: Vec4,
    /// Rain's **Mod2x** blend (`glBlendFunc(GL_DST_COLOR, GL_SRC_COLOR)` = `2·src·dst`, EGxBlend
    /// mode 5 — the byte-verified rain streak/patter state, wow-re `rf-weather-render.md`).
    /// Swaps the pipeline blend state in [`MaterialExtension::specialize`]; the base
    /// `StandardMaterial` keeps `AlphaMode::Blend` so the draw stays in the transparent pass with
    /// depth-write off (the reference's 0x12=0) — only the blend equation differs.
    pub mod2x: bool,
}

/// Pipeline key for [`WowParticleExt`] — materials differing here specialize their own pipeline.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct WowParticleKey {
    pub mod2x: bool,
}

impl From<&WowParticleExt> for WowParticleKey {
    fn from(ext: &WowParticleExt) -> Self {
        Self { mod2x: ext.mod2x }
    }
}

impl MaterialExtension for WowParticleExt {
    fn fragment_shader() -> ShaderRef {
        "shaders/wow_particle.wgsl".into()
    }

    fn specialize(
        _pipeline: &bevy::pbr::MaterialExtensionPipeline,
        descriptor: &mut bevy::render::render_resource::RenderPipelineDescriptor,
        _layout: &bevy::mesh::MeshVertexBufferLayoutRef,
        key: bevy::pbr::MaterialExtensionKey<Self>,
    ) -> std::result::Result<(), bevy::render::render_resource::SpecializedMeshPipelineError> {
        // `$WOW_PARTICLE_NODEPTH=1` — the occlusion A/B. Force the depth COMPARE to `Always` on
        // every particle quad, so "this effect doesn't draw" splits cleanly into *nothing is
        // emitted* (the census's job — `WOW_PARTICLE_CENSUS` counts live particles) and *it is
        // emitted, rasterized, and the depth buffer eats it*. Those two look identical on screen
        // and want completely different fixes.
        //
        // Its most useful reading on B16 was a NEGATIVE one: the voidwalker's eye glow stays gone
        // from above under this switch, and `WOW_DEPTH_QUADS` then measured 30–48% of each quad
        // surviving the compare at every elevation. Depth was never what killed it — the owner's
        // own blend batches were drawing over it (`particles::owner_last_bias`, decision 0719).
        // An earlier note here read the switch the other way round; that reading is retracted.
        if std::env::var_os("WOW_PARTICLE_NODEPTH").is_some() {
            if let Some(ds) = descriptor.depth_stencil.as_mut() {
                ds.depth_compare = bevy::render::render_resource::CompareFunction::Always;
            }
        }
        // `$WOW_PARTICLE_FLAT=1` — the fragment-input A/B (B16): the shader emits solid magenta,
        // ignoring texture, vertex colour and fog. Splits "the quad rasterizes but its inputs
        // multiply to zero" from "the fragments never execute".
        if std::env::var_os("WOW_PARTICLE_FLAT").is_some() {
            if let Some(fragment) = descriptor.fragment.as_mut() {
                fragment.shader_defs.push("WOW_PARTICLE_FLAT".into());
            }
        }
        if key.bind_group_data.mod2x {
            use bevy::render::render_resource::{
                BlendComponent, BlendFactor, BlendOperation, BlendState,
            };
            if let Some(fragment) = descriptor.fragment.as_mut() {
                for target in fragment.targets.iter_mut().flatten() {
                    // Mod2x: src·dst + dst·src = 2·src·dst. Alpha keeps the destination.
                    target.blend = Some(BlendState {
                        color: BlendComponent {
                            src_factor: BlendFactor::Dst,
                            dst_factor: BlendFactor::Src,
                            operation: BlendOperation::Add,
                        },
                        alpha: BlendComponent {
                            src_factor: BlendFactor::Zero,
                            dst_factor: BlendFactor::One,
                            operation: BlendOperation::Add,
                        },
                    });
                }
            }
        }
        Ok(())
    }
}
