//! Faithful 1.12 world-doodad distance fade — the size-bucketed per-object alpha fade the reference
//! runs every frame in `FUN_00683f80` (the world-M2-doodad fade, called unconditionally from the world
//! render `FUN_00681070`). VERIFIED from `WoW.exe` 2026-06-01 (capstone disasm + raw PE byte reads of
//! the operands; the 9-agent reconcile workflow `wf_33fb17c5`).
//!
//! ## The mechanism (VERIFIED-bytefact)
//! Per doodad the engine computes `d = horizontal_distance(center.xy, camera.xy) − boundingRadius`
//! (center `rec+0x5c/0x60`, radius `rec+0x68`), then picks a fade band **purely by the doodad's
//! bounding-sphere radius** (the size split the user observed — big things stay, small props fade near):
//!
//! | bounding radius | fade band (start→end yd) | examples |
//! |---|---|---|
//! | `> 7.0`         | never fades (`1.0`)      | trees, buildings — drawn until the frustum far-clip |
//! | `≤ 0.5`         | `40 → 50`                | fences, haystacks, pumpkins (fade nearest) |
//! | `0.5 … 2.5`     | `100 → 125`              | mid props |
//! | `2.5 … 7.0`     | `150 → 200`              | large props (far end clamped by farclip) |
//!
//! `fade = 1 − (d − start) / range`, clamped to `[0, 1]`; monotonic in size (bigger ⇒ fades farther).
//! `fade ≥ 1` ⇒ fully opaque (drawn); `fade ≤ 0` ⇒ culled (not added to the draw list). The scalar
//! flows `CM2Model+0x180` → `inst+0x19c` → batch alpha → the per-vertex **diffuse.a** consumed by the
//! cutout fragment shader, where the hard alpha test (`discard if tex0.a × diffuse.a < 224/255`) turns
//! a dropping `fade` into per-pixel edge-first erosion, and the **blend pass while `0 < fade < 1`**
//! makes the small-prop fade read as a soft gradient rather than the trees' hard cutout edge.
//!
//! Verified operand bytes (`WoW.exe`): radius cutoffs `0x810188=0.5`, `0x81018c=2.5`, `0x810190=7.0`;
//! band starts `0x8101a0/a4/a8 = 50/125/200` (ends) with ranges `0x810194/98/9c = 10/25/50`; `1.0` at
//! `0x7ff9d8`. This REFUTES an old static-RE claim of "no size/radius weighting" — a null byte-fact
//! lost to the user's direct observation.

use benilla_assets::materials::WowModelMaterial;
use bevy::mesh::MeshTag;
use bevy::prelude::*;

/// Per-instance data driving the world-doodad distance fade, attached to every doodad/WMO submesh
/// entity at spawn. `apply_model_visibility` reads `radius` + the camera distance each frame, computes
/// the fade via [`doodad_fade_alpha`], encodes it into the entity's `MeshTag` (raw f32 bits, consumed by
/// `wow_model.wgsl`), and swaps `MeshMaterial3d` between `cutout` (steady, `fade==1`) and `blend`
/// (feathering, `0<fade<1`) so the soft small-prop gradient reads correctly while trees stay hard-edged.
#[derive(Component, Clone)]
pub struct DoodadFade {
    /// World bounding-sphere radius = M2 `bounding_sphere_radius` × placement scale (yd) — VERIFIED as
    /// the reference's `rec+0x68` (`FUN_006952a0`: `radius × scale`). Selects the fade band.
    pub(crate) radius: f32,
    /// The model's authored bounding-box **centre** in Bevy model-local space. At runtime the entity's
    /// `GlobalTransform` maps it to the world sphere centre (the reference's `rec+0x5c/0x60`); the fade
    /// distance is measured to THAT, not the placement origin — `FUN_006952a0` transforms `(min+max)/2`.
    pub(crate) local_center: Vec3,
    /// Steady-state material (the submesh's authored blend mode: opaque trunk / alpha-test canopy).
    pub(crate) cutout: Handle<WowModelMaterial>,
    /// `AlphaMode::Blend` variant of the same texture — used only while the object is feathering.
    pub(crate) blend: Handle<WowModelMaterial>,
}

/// Radius above which a doodad **never** distance-fades (trees/buildings): drawn until the frustum
/// far-clip drops the whole object. `> 7.0` yd bounding-sphere radius.
pub const NEVER_FADE_RADIUS: f32 = 7.0;

/// The three fading size buckets: `(max_radius, band_start_yd, band_range_yd)`. A doodad uses the first
/// bucket whose `max_radius` it does not exceed; `fade = 1 − (d − start) / range`. Exact `FUN_00683f80`
/// constants (see module docs). Buckets are ordered small→large; `NEVER_FADE_RADIUS` caps the table.
const BUCKETS: [(f32, f32, f32); 3] = [
    (0.5, 40.0, 10.0),                // ≤ 0.5 yd → 40→50  (fences/hay/pumpkins)
    (2.5, 100.0, 25.0),               // ≤ 2.5 yd → 100→125
    (NEVER_FADE_RADIUS, 150.0, 50.0), // ≤ 7.0 yd → 150→200
];

/// The per-object distance-fade alpha for a doodad of bounding-sphere `radius` (yd, already scaled by
/// the placement scale) whose center is `horiz_dist` yd from the camera **in the horizontal plane**
/// (vertical offset ignored — the reference uses 2D distance). Returns the alpha multiplier in
/// `[0.0, 1.0]`:
/// - `1.0`  → fully opaque (draw normally; the cutout alpha test is unaffected).
/// - `0.0`  → fully faded (the caller should cull the object — it contributes nothing).
/// - `0<f<1`→ feathering; the object should draw **blended** so the fade reads as a soft gradient.
///
/// Doodads with `radius > NEVER_FADE_RADIUS` always return `1.0` (trees/buildings never fade here —
/// they rely on the separate frustum far-clip cull).
pub fn doodad_fade_alpha(radius: f32, horiz_dist: f32) -> f32 {
    if radius > NEVER_FADE_RADIUS {
        return 1.0;
    }
    // `d` is distance-to-surface (center distance minus the bounding radius), matching the reference's
    // `dist − radius`; a bigger object therefore starts fading at a greater center distance.
    let d = horiz_dist - radius;
    let (_, start, range) = BUCKETS
        .iter()
        .copied()
        .find(|(max_r, _, _)| radius <= *max_r)
        // radius ≤ NEVER_FADE_RADIUS here, so the last bucket (max_r == NEVER_FADE_RADIUS) always matches.
        .unwrap_or((NEVER_FADE_RADIUS, 150.0, 50.0));
    (1.0 - (d - start) / range).clamp(0.0, 1.0)
}

/// The fade-band edges for a fader of bounding-sphere `radius`: `(near, far)` horizontal
/// CENTRE distances — [`doodad_fade_alpha`] is `1.0` at `d ≤ near`, `0.0` at `d ≥ far`, and
/// feathers between. `None` for a never-fader. The static-gx exile scan (B2, decision 1431)
/// classifies placements on these edges, derived from the SAME bucket table the per-frame
/// alpha evaluates — one source, so the two verdicts can never disagree.
pub fn fade_band(radius: f32) -> Option<(f32, f32)> {
    if radius > NEVER_FADE_RADIUS {
        return None;
    }
    let (_, start, range) = BUCKETS
        .iter()
        .copied()
        .find(|(max_r, _, _)| radius <= *max_r)
        .unwrap_or((NEVER_FADE_RADIUS, 150.0, 50.0));
    Some((start + radius, start + radius + range))
}

/// The faithful per-object **appear / spawn fade** (`wow-5875-re` object-layer/`appear-fade`): a CGObject
/// — every streamed unit, GameObject, and player — ramps its render alpha `α = t³` over **2 s wall-clock**
/// when it first becomes visible, then latches opaque. This is the *temporal* sibling of [`DoodadFade`]'s
/// *distance* fade: both drive the **same** per-instance render-alpha channel (the `MeshTag` the shader
/// reads, with a cutout↔blend material swap while `α < 1`), matching the reference's single render-alpha
/// slot (`CM2Model+0x19c`) written by separate sources for disjoint object categories (map doodads →
/// distance fade; CGObjects → appear fade). [`apply_render_fade`] drives it.
///
/// It owns the part's **alpha** and its **material** while it lives; [`crate::interior`]'s
/// classifier keeps owning the part's **light law** right through the ramp (decision 0755), and
/// the fade reads that law each frame to pick the blend twin of the right family
/// ([`FadeMaterials::material_for`]) — so an entity that streams in indoors ramps up already lit by
/// its room instead of appearing under exterior light and snapping to the room's when it latches.
///
/// Built general (`from`/`to`/`duration`) so the **despawn fade-out** (our stream-out look — the
/// binary has no teardown fade, see [`DespawnFade`]) is a `{from: α, to: 0}` instance + a
/// despawn-on-complete system — no new channel needed. The appear case is
/// `{from: 0, to: 1, duration: 2 s}`. (Decision 0032; teardown fidelity corrected in 0067.)
#[derive(Component, Clone)]
pub struct RenderFade {
    /// `Time::elapsed_secs` when the fade was armed (the entity's first-visible moment).
    pub started: f32,
    /// Fade length in seconds. Appear = [`APPEAR_FADE_SECS`].
    pub duration: f32,
    /// Cubic ramp endpoints. Appear `0 → 1`; despawn `α → 0`.
    pub from: f32,
    pub to: f32,
}

/// The reference's appear-fade duration — `FadeTo(1.0, 2000 ms)` (`wow-5875-re` object-layer/`appear-fade`:
/// byte `0x7d0`, wall-clock via `OsGetAsyncTimeMs`, framerate-independent).
pub const APPEAR_FADE_SECS: f32 = 2.0;

impl RenderFade {
    /// A spawn appear-fade armed at `now`: `α = t³` from 0 → 1 over [`APPEAR_FADE_SECS`].
    pub fn appear(now: f32) -> Self {
        Self {
            started: now,
            duration: APPEAR_FADE_SECS,
            from: 0.0,
            to: 1.0,
        }
    }
}

/// The reference's cubic-ease render-alpha: `α = lerp(from, to, clamp(t, 0, 1)³)` (`0x614a90`: `fld t;
/// fmul t; fmul t`). `t` is the fractional fade age. Appear (0→1) accelerates up from invisible; a
/// despawn (α→0) eases out.
pub fn fade_alpha(from: f32, to: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    from + (to - from) * t * t * t
}

/// One model instance's live **render alpha** — the reference's `CM2Model+0x19c`, and the single
/// slot every fade in the client writes through (wow-re object-layer/`appear-fade` §"the alpha
/// slot"): `+0x19c = argAlpha · +0x180`, with `+0x180 = obj+0x100 (master) · obj+0xf4 (the appear
/// ramp)` for a CGObject and the distance fade for a map doodad.
///
/// benilla keeps that alpha on the MESH side in the per-part `MeshTag` (the appear/despawn ramp,
/// the self-avatar feather, the doodad fade — three writers, one channel). This component is the
/// same number published **per model instance**, for the consumers that are not meshes and cannot
/// read a `MeshTag`: an emitter's particles and a ribbon's strip, whose alpha is per-vertex colour
/// (decision 0827). Missing ⇒ `1.0`.
///
/// An **attached** model inherits its parent's, which is why an item's effects read their WEARER's
/// (the `0x714000` recursion: a child model with `[model+0x1cc] ≠ 0` composes onto the parent's
/// computed colours and alpha — wow-re `selection-circle.md` §scope, `0x714260`'s `[ebp+0x14]`).
/// That inheritance is [`ParentModel`] + [`ModelAlphas`], never a spawn site pointing at the top
/// of the chain by hand (decision 0833).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ModelAlpha(pub f32);

/// **A fade the game declares on an instance root** — a plain multiplier the engine folds into
/// the composed model alpha beside its own appear and despawn ramps.
///
/// 1164's inversion, in one component. The alpha channel had six writers, three of them gameplay,
/// each carrying a `Without<…>` lockout against the others; the reference has **one** slot
/// (`CM2Model+0x19c`). The rule this establishes: the game says *how translucent this thing should
/// be and why* (an aura's ramp, a stealth dip, a ghost form); the engine owns the write, the
/// composition down the `ParentModel` chain, and the material swap that goes with it.
///
/// Absent reads `1.0`, so a root that has never declared one costs nothing.
#[derive(Component, Clone, Copy, Debug)]
pub struct ModelFade(pub f32);

/// The model instance this one is **chained to** — the reference's `[model+0x1cc]` parent link,
/// set on a model that was attached to another model (`0x712f70 CM2Model::attachChild`). An item
/// root's parent is its wearer; a weapon's enchant-glow instance's parent is that item root; a
/// spell kit hung on a unit's is the unit.
///
/// It exists because composition is **recursive** in the reference (`0x714000` walks the chain and
/// composes each child onto its parent's *computed* colour and alpha) and every spawn site knows
/// exactly one thing for certain: who it just attached itself to. Asking a site for the far end of
/// the chain instead is what left a weapon's glow — two links down — with no alpha source at all,
/// blazing at full strength over a character that had not faded in yet (decision 0833).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ParentModel(pub Entity);

/// How far a [`ParentModel`] walk goes before giving up — [`ModelAlphas`], and the tint's twin walk
/// in [`crate::aura_visual`]. The real chains are 1–3 links (unit → item → glow); the bound is a
/// cycle backstop, not a policy.
pub const MAX_MODEL_CHAIN: usize = 8;

/// The composed render alpha of a model instance — [`ModelAlpha`] multiplied along the
/// [`ParentModel`] chain, i.e. the reference's `0x714000` recursion as a lookup. This is the number
/// an effect multiplies into its vertex alpha; **an effect passes its OWN instance root** and the
/// chain supplies everything above it.
///
/// Missing components read as opaque throughout: an entity with no `ModelAlpha` and no parent is
/// `1.0`, which is why the steady-state world pays nothing for this.
#[derive(bevy::ecs::system::SystemParam)]
pub struct ModelAlphas<'w, 's> {
    chain: Query<'w, 's, (Option<&'static ModelAlpha>, Option<&'static ParentModel>)>,
}

impl ModelAlphas<'_, '_> {
    /// `instance`'s composed alpha. A despawned link ends the walk (its effects are being freed
    /// this frame anyway — [`crate::particles::OwnerLoss::Free`]).
    pub fn get(&self, instance: Entity) -> f32 {
        let mut alpha = 1.0;
        let mut at = instance;
        for _ in 0..MAX_MODEL_CHAIN {
            let Ok((own, parent)) = self.chain.get(at) else {
                break;
            };
            if let Some(a) = own {
                alpha *= a.0;
            }
            match parent {
                Some(p) => at = p.0,
                None => break,
            }
        }
        alpha
    }
}

/// The render alpha of one streamed model instance this frame — pure, so the composition is pinned
/// by tests without an ECS (the `model_fade` pattern):
///
/// - the **appear** ramp: none ⇒ opaque; pending ⇒ 0 (the entity is not being shown yet, so its
///   effects must not be either — this is the login symptom); live ⇒ the cubic ramp,
/// - × the **despawn** ramp (our stream-out look, 0032/0067) once armed,
/// - × the **self-avatar** zoom feather, which applies to the player's own body alone,
/// - × the **aura** alpha (`crate::aura_visual` — stealth's 0.3, invisibility's 0.5, …): the ramped
///   CharProc-14 product. In the reference this is not a separate channel at all — the aura
///   recompute drives the SAME `StartAlphaFade` slot the appear-fade uses (`0x60d180` →
///   `0x614f80` → `obj+0xf4` → `+0x180` → `+0x19c` → `emitter+0x1a8`), which is exactly why a
///   stealthed unit's weapon-glow particles and ribbons dim with it.
///
/// The product is the reference's own shape: it multiplies the transition alpha into the same
/// `+0x180` slot rather than keeping a second channel.
pub fn model_render_alpha(
    now: f32,
    appear: Option<UnitAppearFade>,
    despawn_started: Option<f32>,
    self_fade: f32,
    declared: f32,
) -> f32 {
    let appear = match appear {
        None => 1.0,
        Some(UnitAppearFade::Pending { .. }) => 0.0,
        Some(UnitAppearFade::Live { started }) => {
            fade_alpha(0.0, 1.0, (now - started) / APPEAR_FADE_SECS)
        }
    };
    let despawn = despawn_started.map_or(1.0, |started| {
        fade_alpha(1.0, 0.0, (now - started) / APPEAR_FADE_SECS)
    });
    (appear * despawn * self_fade * declared).clamp(0.0, 1.0)
}

/// Publish [`ModelAlpha`] on every streamed object each frame. Runs in `PostUpdate`, after every
/// Update-side fade writer and the camera controller that computes the self feather, and before the
/// effect sims that consume it ([`crate::particles`], [`crate::ribbons`]).
///
/// Opaque is the default and costs nothing: an entity that has never faded gets no component at all
/// (a missing one reads `1.0`), so the steady-state world carries none of these.
#[allow(clippy::type_complexity)] // one query, five optional facets of the same entity
pub(crate) fn publish_model_alpha(
    time: Res<Time>,
    viewer: Res<crate::view::Viewer>,
    mut commands: Commands,
    mut units: Query<
        (
            Entity,
            Option<&UnitAppearFade>,
            Option<&DespawnFade>,
            Has<crate::world_unit::ViewerUnit>,
            Option<&ModelFade>,
            Option<&mut ModelAlpha>,
        ),
        With<crate::world_unit::WorldUnit>,
    >,
) {
    let now = time.elapsed_secs();
    for (entity, appear, despawn, is_self, declared, current) in &mut units {
        let alpha = model_render_alpha(
            now,
            appear.copied(),
            despawn.map(|d| d.started).filter(|s| *s >= 0.0),
            if is_self { viewer.self_fade } else { 1.0 },
            // The game's declared fade — the aura ramp ticks it in Update (`apply_aura_alpha`)
            // and this runs PostUpdate, so it is fresh. `self_fade` above is the bare zoom
            // feather (the mesh-side writer folds the aura factor separately), so this is not a
            // double application on the self body.
            declared.map_or(1.0, |d| d.0),
        );
        match current {
            Some(mut c) => {
                if c.0 != alpha {
                    c.0 = alpha;
                }
            }
            None if alpha >= 1.0 => {}
            None => {
                commands.entity(entity).insert(ModelAlpha(alpha));
            }
        }
    }
}

/// The camera-to-target span (yd) over which the **player's own** avatar fades from hidden (camera near)
/// to opaque (camera out) as you zoom into first-person. VERIFIED from `WoW.exe` 5875 (`0x8089b0`; the
/// self-model transparency setter `0x5b7bb0`, recorded in wow-re `system/ui/scratch/follow-camera.md`).
pub const SELF_FADE_WINDOW: f32 = 1.8315;
/// Distance-above-nearclip below which the avatar **hard-hides** (fully invisible — true first-person).
/// VERIFIED `0x5b7bb0`: `D ≤ 0.00278` ⇒ α 0 + first-person, else the cosine ramp.
pub const SELF_FADE_HIDE: f32 = 0.00278;

/// The faithful **self-avatar transparency** as the camera nears its target — the fade that turns your
/// own character translucent while zooming in, then fully invisible in first-person. VERIFIED from
/// `WoW.exe` 5875 (`0x5b7bb0`, wow-re `follow-camera`): a cosine smoothstep on the camera-to-target
/// distance, `α = (1 − cos(π·D/F))/2` over `D = dist − nearclip ∈ (SELF_FADE_HIDE, window]`. Below
/// [`SELF_FADE_HIDE`] above the near clip the model hard-hides (`0.0`); at/after `window` it's opaque
/// (`1.0`). `nearclip` is the camera's near-plane distance — the fade completes exactly as the near
/// plane would begin to slice the model, which is why the two are coupled (see `crate::view::CAM_NEAR`).
///
/// Unlike [`doodad_fade_alpha`] (horizontal *world* distance, size-bucketed) this is the *camera*
/// distance to a single tracked object; both feed the same per-instance render-alpha channel.
pub fn self_model_fade_alpha(dist: f32, nearclip: f32, window: f32) -> f32 {
    let d = dist - nearclip;
    if d <= SELF_FADE_HIDE {
        return 0.0;
    }
    if d >= window {
        return 1.0;
    }
    0.5 * (1.0 - (std::f32::consts::PI * d / window).cos())
}

/// Drive every live [`RenderFade`]: ramp the cubic alpha into the per-instance `MeshTag`, ride the
/// blend twin of the part's **current light law** while feathering (the cutout ignores `α`), and
/// drop the component once an appear fade latches opaque (handing the material back to
/// [`crate::interior`] on the law's steady variant). Only freshly-streamed or streaming-out
/// entities carry a `RenderFade` — it's removed after [`APPEAR_FADE_SECS`] — so the steady-state
/// cost is nil.
///
/// The material is resolved from [`FadeMaterials`] + the live [`crate::interior::InteriorLit`]
/// every frame rather than from a pair latched at arm time (decision 0755), so a part that
/// classifies (or re-classifies) *during* its ramp follows its room's light immediately instead of
/// finishing the ramp on whichever law happened to hold when the fade armed.
#[allow(clippy::type_complexity)]
pub fn apply_render_fade(
    time: Res<Time>,
    mut commands: Commands,
    // The water-plane axis (`model_render::classify_water_side`), composed into this writer's
    // own pick via `far_resolved` — every owner of the handle channel derives the same composed
    // handle from state, so the ramp and the classifier converge instead of re-swapping.
    far_twins: Res<crate::model_render::FarSideTwins>,
    mut q: Query<(
        Entity,
        &RenderFade,
        &mut MeshTag,
        &mut MeshMaterial3d<WowModelMaterial>,
        Option<&crate::doodad_anim::MatAnim>,
        Option<&FadeMaterials>,
        Option<&crate::interior::InteriorLit>,
        Has<crate::model_render::FarSideOfWater>,
    )>,
) {
    let now = time.elapsed_secs();
    for (entity, fade, mut tag, mut mat, anim, fm, lit, far_side) in &mut q {
        let t = if fade.duration > 0.0 {
            (now - fade.started) / fade.duration
        } else {
            1.0
        };
        // The fade owns the alpha field while it lives, so it is also where the batch's animated
        // material factor multiplies in — the reference's combine is literally a product,
        // `A = instanceAlpha × colourAlpha × weight` (wow-re `m2-alpha-combine-cull.md`), and the
        // fade ramp IS this instance's alpha. Without this a unit appearing mid-Death would flash
        // its death-only geometry opaque for the length of the ramp.
        let alpha = fade_alpha(fade.from, fade.to, t) * anim.map_or(1.0, |a| a.current);
        // `with_alpha` handles the `MeshTag == 0` opaque-sentinel — else a just-spawned object at
        // α 0 would flash fully opaque — and preserves the ground-shade byte, so a unit fading in
        // under MCSH shadow doesn't flash lit (the conventions live in `crate::mesh_tag`).
        let bits = crate::mesh_tag::with_alpha(tag.0, alpha);
        if tag.0 != bits {
            tag.0 = bits;
        }
        // Feather on the blend twin while translucent; the cutout (opaque/alpha-test) ignores `α`.
        // Both come from the part's CURRENT law — an indoor part feathers probe-lit and settles on
        // the probe-lit steady material, with no law change at the latch. A part without
        // `FadeMaterials` has no twin to swap to (it never fades in practice — every arming site
        // pairs the two); its alpha still ramps, so it can never hang invisible.
        if let Some(fm) = fm {
            let want = crate::model_render::far_resolved(
                fm.material_for(lit, alpha < 1.0),
                far_side,
                &far_twins,
            );
            if mat.0 != *want {
                mat.0 = want.clone();
            }
        }
        // Appear fade reached opaque: latch and release the channel back to the interior classifier.
        // `try_remove`: wire-owned entity — see the lifetime contract in `arm_appear_fade`.
        if t >= 1.0 && fade.to >= 1.0 {
            commands.entity(entity).try_remove::<RenderFade>();
        }
    }
}

/// A streamed entity that should appear-fade, **waiting to be armed**. The reference arms the fade when
/// an object *becomes visible to the player* (visibility processor `0x4651a0`), NOT at stream-in — so the
/// ramp plays while you can see it instead of completing **behind the loading screen** (login / `.tele`)
/// or far off-screen. benilla attaches this at spawn (the entity rendered invisible meanwhile) and
/// [`arm_appear_fade`] converts it into a live [`RenderFade`] once the world is actually being shown.
/// (Decision 0032.)
#[derive(Component, Clone)]
pub struct PendingAppearFade {
    /// `Time::elapsed_secs` at attach — the backstop-timeout origin.
    pub since: f32,
}

/// Backstop: arm a pending fade after this long even if the world never reports "shown" — bounds the
/// worst case (a stuck load) so an entity can never hang invisible. Generous so it almost never fires
/// before the real signal (the loading screen dropping) does.
const PENDING_TIMEOUT_SECS: f32 = 8.0;

/// The appear-fade's clock, mirrored onto a streamed unit's **root** entity the moment its body first
/// arms an appear-fade — decision 0032 read as a **per-unit** property (the reference fades a whole
/// CGUnit — body plus every attached model — as one), not a per-mesh stamp taken only at the instant a
/// given submesh happens to spawn. A held item / helm / shoulder resolves and spawns *later* than the
/// body (an async template round trip, a model load — `entities::equipment::attach_held_items`); it
/// reads this marker to **join** the unit's ramp instead of either spawning fully opaque (no marker
/// consulted — the original bug: attachments popped in while the body eased) or restarting its own
/// fade from zero (would desync the two visually). Mirrors [`PendingAppearFade`]/[`RenderFade`]'s two
/// states one level up the hierarchy: [`arm_appear_fade`] advances both in lockstep (same trigger, same
/// instant, so a late joiner reads "live" exactly when the rest of the unit does), and
/// [`retire_unit_appear_fade`] drops it once the mirrored ramp completes — after that instant the unit
/// has fully appeared, so anything spawning later (a gear swap, a delayed resolve) is a fresh
/// presentation and correctly spawns steady, nil ongoing cost, same as the per-mesh channel.
///
/// See [`join_unit_appear_fade`] for the join decision a spawn site makes from this, kept as a pure
/// function over plain data so it's unit-testable without the ECS.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub enum UnitAppearFade {
    /// Waiting for the world to be shown — mirrors [`PendingAppearFade::since`].
    Pending { since: f32 },
    /// The live ramp in progress — mirrors [`RenderFade::started`]. A joiner copies `started` verbatim
    /// rather than sampling the current alpha: [`fade_alpha`] is a pure function of elapsed time, so an
    /// identical `started` reproduces the identical curve for as long as both instances live — that
    /// *is* what "joining the current position" means, no stored alpha required.
    Live { started: f32 },
}

/// What a part spawning onto a unit should do about the unit's appear-fade, derived by
/// [`join_unit_appear_fade`] from the unit root's [`UnitAppearFade`] (or its absence).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JoinedFade {
    /// No unit-level fade in flight (never armed, e.g. a WMO-display entity — or already latched):
    /// spawn steady, exactly as a part attached long after the unit fully appeared does today.
    Steady,
    /// Join the still-pending fade: arm together with the rest of the unit once the world is shown.
    Pending { since: f32 },
    /// Join the live ramp already in progress, at its current position (see [`UnitAppearFade::Live`]).
    Live { started: f32 },
}

/// The join decision for a part spawning onto a unit whose root carries `unit` (or doesn't). Pure and
/// total — extracted so the decision is testable without spinning up the ECS (see the tests below).
pub fn join_unit_appear_fade(unit: Option<UnitAppearFade>) -> JoinedFade {
    match unit {
        None => JoinedFade::Steady,
        Some(UnitAppearFade::Pending { since }) => JoinedFade::Pending { since },
        Some(UnitAppearFade::Live { started }) => JoinedFade::Live { started },
    }
}

/// Arm each [`PendingAppearFade`] into a live [`RenderFade`] once the world is on-screen — the
/// loading screen is not covering (it used to read the `focus_resident` proxy, which goes true
/// well before the screen actually drops now that the clear waits for the whole scene — decision
/// 0737) — or after [`PENDING_TIMEOUT_SECS`] as a backstop. This is the faithful trigger: the ramp
/// starts when the player can actually see the entity. Also advances each unit-root
/// [`UnitAppearFade`] clock in lockstep (same trigger, same instant) — a separate, smaller query
/// since the root marker carries no material handles.
pub(crate) fn arm_appear_fade(
    time: Res<Time>,
    viewer: Res<crate::view::Viewer>,
    mut commands: Commands,
    q: Query<(
        Entity,
        &PendingAppearFade,
        Has<crate::billboard::BillboardCard>,
    )>,
    mut units: Query<&mut UnitAppearFade>,
) {
    let now = time.elapsed_secs();
    let shown = !viewer.world_covered;
    let mut armed = 0usize;
    // Of those, the world-ROOT billboard cards (decision 0836). Called out separately because a
    // card reaches this system through nothing but its own spawn — no tree walk can find it — so
    // "how many cards armed" is the one readout that separates "the cards fade" from "the cards
    // are simply absent from the ramp", which is what they were.
    let mut armed_cards = 0usize;
    for (entity, pending, is_card) in &q {
        if shown || now - pending.since > PENDING_TIMEOUT_SECS {
            armed += 1;
            armed_cards += usize::from(is_card);
            // `try_*`, like every fade command here: these entities are **wire-owned** — a net
            // destroy can apply at any sync point between this system's query and its own
            // commands, so fade bookkeeping on an already-dead entity is a no-op, never an error.
            // (The observed crash: the login load ends, every pending fade arms in one frame,
            // and a same-frame wire despawn beat this insert — decision 0200.)
            commands
                .entity(entity)
                .try_insert(RenderFade::appear(now))
                .try_remove::<PendingAppearFade>();
        }
    }
    for mut unit_fade in &mut units {
        if let UnitAppearFade::Pending { since } = *unit_fade {
            if shown || now - since > PENDING_TIMEOUT_SECS {
                *unit_fade = UnitAppearFade::Live { started: now };
            }
        }
    }
    // `WOW_INTERIOR_LOG=1`: the appear-fade's arming instant — the reference time the interior
    // classifier's `[interior]` lines are read against. The seam between the two is where an
    // indoor entity's light law used to land only AFTER the ramp latched (2 s of exterior light,
    // then a pop); a run where the two instants coincide is the observable that closes it.
    if armed > 0 && std::env::var_os("WOW_INTERIOR_LOG").is_some() {
        eprintln!(
            "[fade-arm] t {now:.2} armed {armed} parts ({armed_cards} billboard cards) \
             (world shown: {shown})"
        );
    }
}

/// Drop a [`UnitAppearFade::Live`] once its mirrored ramp completes ([`APPEAR_FADE_SECS`] after
/// `started`): past that instant the unit has fully appeared, so a part spawning later (a delayed
/// template resolve, a gear change) is a fresh presentation rather than a continuation, and
/// [`join_unit_appear_fade`] should treat it exactly like a unit that never had a marker at all.
pub(crate) fn retire_unit_appear_fade(
    time: Res<Time>,
    mut commands: Commands,
    q: Query<(Entity, &UnitAppearFade)>,
) {
    let now = time.elapsed_secs();
    for (entity, fade) in &q {
        if let UnitAppearFade::Live { started } = fade {
            if now - started >= APPEAR_FADE_SECS {
                // `try_remove`: wire-owned entity — the lifetime contract in `arm_appear_fade`.
                commands.entity(entity).try_remove::<UnitAppearFade>();
            }
        }
    }
}

/// Persistent per-submesh fade material set — **the** source of truth for which material a part
/// draws with, across every fade in the client: `cutout` = the steady opaque/alpha-test material,
/// `blend` = its `AlphaMode::Blend` twin. Attached to every fadeable entity submesh at spawn.
///
/// Held persistently rather than copied into each fade (decision 0755): a latched pair is a
/// snapshot of the part's light law at arm time, and a law that changes mid-ramp — a streamed
/// indoor unit classifying while it appears, an NPC crossing a doorway as it fades out — cannot be
/// expressed by one. Every fade instead resolves its material *per frame* through
/// [`Self::material_for`].
#[derive(Component, Clone)]
pub struct FadeMaterials {
    pub cutout: Handle<WowModelMaterial>,
    pub blend: Handle<WowModelMaterial>,
    /// The interior-BAKE variant's blend twin (probe-lit): a fade on a bake-classified part rides
    /// this so its light stays the room's probe through the feather instead of jumping to the
    /// exterior twin's lit-outdoor intensity (0355). `None` for parts without a bake variant.
    pub bake_blend: Option<Handle<WowModelMaterial>>,
    /// The depth-prime twin material ([`crate::model_render::zfill_material`] — the reference's
    /// `M2UseZFill` clone, wow-re `m2-blend-promotion-zfill.md` §4, decision 0831). While this
    /// part's instance alpha sits in `(0, 1)`, [`sync_zfill_twins`] keeps a colour-masked,
    /// z-writing child mesh alive on it, drawn before the model's colour parts — one blended layer
    /// everywhere, no self-overlap darkening. `None` for a batch whose material disables
    /// z-write/z-test (the reference's own twin gate).
    pub zfill: Option<Handle<WowModelMaterial>>,
}

impl FadeMaterials {
    /// The material a part should draw with, given its current interior law (`lit` — `None` for a
    /// part the classifier doesn't light) and whether it is still `feathering` (`α < 1`).
    ///
    /// The two axes are independent and this is the one place they compose, so every fade writer —
    /// the appear/despawn ramp ([`apply_render_fade`]) and the self-avatar zoom feather
    /// ([`crate::player::apply_self_model_fade`]) — agrees on the answer regardless of which runs
    /// last in the frame. Feathering picks the law's **blend** twin (the probe-lit one indoors, so
    /// the room's light rides the fade rather than jumping to the exterior twin's outdoor
    /// intensity — 0355); settled hands back the law's **steady** material, which is the
    /// classifier's own choice ([`crate::interior::InteriorLit::steady_material`]).
    pub fn material_for<'a>(
        &'a self,
        lit: Option<&'a crate::interior::InteriorLit>,
        feathering: bool,
    ) -> &'a Handle<WowModelMaterial> {
        match (feathering, lit) {
            (true, Some(l)) if l.is_bake() => self.bake_blend.as_ref().unwrap_or(&self.blend),
            (true, _) => &self.blend,
            (false, Some(l)) => l.steady_material(),
            (false, None) => &self.cutout,
        }
    }
}

/// The material handles one batch fades *through*, as a spawn site borrows them — the inputs
/// [`FadeMaterials`] is built from. `blend: None` means the batch cannot feather at all (a
/// WMO-display batch builds no twin), which is the one legitimate reason for a part to spawn
/// opaque while its unit is still ramping. A MULTIPLY batch (Mod/Mod2x) passes its **steady self**
/// as the twin: its blend equation reads no alpha (decision 0528), so no material swap can feather
/// it — instead it arms the ramp like any other part and `wow_model.wgsl` lerps its colour toward
/// the blend identity by the tag alpha (the deliberate deviation of decision 0865).
pub struct FadeSet<'a> {
    pub steady: &'a Handle<WowModelMaterial>,
    pub blend: Option<&'a Handle<WowModelMaterial>>,
    pub bake_blend: Option<&'a Handle<WowModelMaterial>>,
    /// The depth-prime twin (decision 0831). Carried here for the same reason as the rest of the
    /// set: it is needed exactly while the batch is feathering — which a card now does too.
    pub zfill: Option<&'a Handle<WowModelMaterial>>,
}

/// One batch's appear-fade membership for a spawn: the unit's clock ([`JoinedFade`]) folded with
/// whether the batch is fade-capable at all.
///
/// **Every** batch spawn resolves it here and dresses through [`Self::dress`] — the body's mesh
/// parts and its billboard cards, a held item's mesh parts and its cards. The law is a property of
/// the *model*, not of how benilla happens to parent a batch: the reference has one instance alpha
/// (`CM2Model+0x19c`) that every batch of the model draws through, billboard batches included, so a
/// lane that skips this is a batch that pops opaque over a body still fading in. Both card lanes did
/// exactly that until decision 0836.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PartFade {
    /// No ramp to join (unit already appeared, or the batch cannot feather): open opaque.
    Steady,
    /// Join the unit's still-pending ramp — arm together with it once the world is on screen.
    Pending(f32),
    /// Join the ramp already running, at its current position.
    Live(f32),
}

impl PartFade {
    /// Fold the unit's join decision with the batch's own fade-capability.
    pub fn resolve(joined: JoinedFade, set: &FadeSet<'_>) -> Self {
        match (joined, set.blend) {
            (_, None) | (JoinedFade::Steady, _) => Self::Steady,
            (JoinedFade::Pending { since }, _) => Self::Pending(since),
            (JoinedFade::Live { started }, _) => Self::Live(started),
        }
    }

    /// The material and `MeshTag` alpha the spawn should OPEN on. A pending part opens on the blend
    /// twin at ≈0 so it never flashes opaque for a frame before [`apply_render_fade`] takes over; a
    /// joiner opens at the ramp's *current* alpha so it doesn't flash invisible either.
    pub fn seed(self, set: &FadeSet<'_>, now: f32) -> (Handle<WowModelMaterial>, f32) {
        match self {
            Self::Steady => (set.steady.clone(), 1.0),
            Self::Pending(_) => (set.blend.cloned().unwrap_or_default(), 0.0),
            Self::Live(started) => (
                set.blend.cloned().unwrap_or_default(),
                fade_alpha(0.0, 1.0, (now - started) / APPEAR_FADE_SECS),
            ),
        }
    }

    /// Dress the spawned batch: the persistent [`FadeMaterials`] record whenever it is fade-capable
    /// at all — that is a *material record*, not a record of having armed a fade, and the stream-out
    /// fade and the self-avatar zoom feather both re-arm from it — plus the arm itself when this
    /// spawn is joining the unit's ramp. Returns whether it armed, which the attach path mirrors
    /// onto the unit root so a later-resolving attachment can join the same ramp.
    pub fn dress(self, child: &mut bevy::ecs::system::EntityCommands, set: &FadeSet<'_>) -> bool {
        if let Some(blend) = set.blend {
            child.insert(FadeMaterials {
                cutout: set.steady.clone(),
                blend: blend.clone(),
                bake_blend: set.bake_blend.cloned(),
                zfill: set.zfill.cloned(),
            });
        }
        match self {
            Self::Steady => false,
            // `arm_appear_fade` starts the ramp once the world is on-screen (not behind the loading
            // screen), so it plays where the player can actually see it.
            Self::Pending(since) => {
                child.insert(PendingAppearFade { since });
                true
            }
            Self::Live(started) => {
                child.insert(RenderFade {
                    started,
                    duration: APPEAR_FADE_SECS,
                    from: 0.0,
                    to: 1.0,
                });
                true
            }
        }
    }
}

/// Marks a streamed entity (the parent) that went out of range: instead of popping it out,
/// [`apply_despawn_fade`] fades it out then despawns it. **Our** stream-out look, not a verified
/// mechanism — the wow-re teardown RE found no fade-out in the binary (a *destroyed* object pops
/// instantly, and the net bridge despawns it directly, bypassing this). `started < 0` ⇒ not yet
/// armed/stamped.
#[derive(Component)]
pub struct DespawnFade {
    pub(crate) started: f32,
}

impl Default for DespawnFade {
    fn default() -> Self {
        Self { started: -1.0 }
    }
}

/// Drive the despawn fade-out. On first sight, arm a `{from: 1, to: 0}` [`RenderFade`] on **every
/// fadeable descendant** (reusing the appear machinery — same cubic curve, material swap, classifier
/// yield) and stamp the start; once [`APPEAR_FADE_SECS`] elapses, despawn the entity (children cascade).
/// An entity with no fadeable geometry (cube / model-less) pops straight out — nothing to fade.
///
/// Descendants, not direct children: body submeshes hang directly under the unit root, but a held
/// weapon / helm / shoulder is a child of a **joint** entity several levels down
/// ([`crate::entities::BoneAttach`]) — the fade is a per-*unit* property (decision 0032's shape), so
/// the whole tree fades as one instead of the body thinning around a still-opaque weapon.
///
/// **And the tree is not the whole model.** A BILLBOARD batch is a world ROOT that merely *follows*
/// an anchor inside the tree (decision 0153), so the walk cannot reach it — the cards are picked up
/// by testing their follow-anchor against the walked set, the same idiom
/// [`crate::player::apply_self_model_fade`] uses (decision 0836).
pub(crate) fn apply_despawn_fade(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut DespawnFade)>,
    children_of: Query<&Children>,
    fm: Query<(), With<FadeMaterials>>,
    cards: Query<(Entity, &crate::billboard::BillboardCard), With<FadeMaterials>>,
) {
    let now = time.elapsed_secs();
    for (parent, mut df) in &mut q {
        if df.started < 0.0 {
            let mut any = false;
            let mut walked = bevy::ecs::entity::EntityHashSet::default();
            arm_despawn_descendants(
                parent,
                now,
                &mut commands,
                &children_of,
                &fm,
                &mut any,
                &mut walked,
            );
            for (card, follow) in &cards {
                if follow.follows().is_some_and(|a| walked.contains(&a)) {
                    any = true;
                    arm_fade_out(card, now, &mut commands);
                }
            }
            if any {
                df.started = now;
            } else {
                commands.entity(parent).try_despawn();
            }
        } else if now - df.started >= APPEAR_FADE_SECS {
            commands.entity(parent).try_despawn();
        }
    }
}

/// Arm one fadeable entity's `{from: 1, to: 0}` ramp. No material is chosen here:
/// [`apply_render_fade`] resolves it per frame from the part's live law (0755), so a
/// bake-classified part fades out probe-lit — and keeps doing so if it re-classifies mid-ramp,
/// which a pair latched at this instant could not express. `try_*`: a child (a held item
/// mid-re-resolve, a gear swap) can be despawned by its own owner in the same frame — the
/// lifetime contract in [`arm_appear_fade`].
fn arm_fade_out(entity: Entity, now: f32, commands: &mut Commands) {
    commands
        .entity(entity)
        .try_insert(RenderFade {
            started: now,
            duration: APPEAR_FADE_SECS,
            from: 1.0,
            to: 0.0,
        })
        .try_remove::<PendingAppearFade>();
}

/// Depth-first helper for [`apply_despawn_fade`]: arm the fade-out on `entity` if it carries
/// [`FadeMaterials`], and recurse into its children either way (a joint / held-item root carries none
/// itself but has fadeable meshes beneath it). Every entity visited — parts, joints, attach roots,
/// billboard anchors alike — is recorded in `walked`, which the caller uses to recognise the
/// world-root billboard cards that follow this model.
#[allow(clippy::too_many_arguments)]
fn arm_despawn_descendants(
    entity: Entity,
    now: f32,
    commands: &mut Commands,
    children_of: &Query<&Children>,
    fm: &Query<(), With<FadeMaterials>>,
    any: &mut bool,
    walked: &mut bevy::ecs::entity::EntityHashSet,
) {
    walked.insert(entity);
    if fm.contains(entity) {
        *any = true;
        arm_fade_out(entity, now, commands);
    }
    if let Ok(children) = children_of.get(entity) {
        for &child in children {
            arm_despawn_descendants(child, now, commands, children_of, fm, any, walked);
        }
    }
}

/// The model-fade lane's own registrations (decision 1163, stage zero).
///
/// These five systems were registered by `EntitiesPlugin` — a *gameplay* plugin — for no reason
/// beyond history: the fade lane grew inside the entity streamer and never claimed a plugin of its
/// own. Nothing here is about streamed entities; it is the M2 render-alpha channel, which the
/// terrain streamer's doodads and a world with no game in it need exactly as much. Owning the
/// registrations is what lets the world viewer boot the lane without booting the entity streamer.
pub fn plugin(app: &mut App) {
    // Arm a queued appear-fade once the world is on-screen, then drive it each frame; the despawn
    // fade-out re-arms the same `RenderFade` channel on stream-out. All disjoint from the interior
    // classifier + the doodad fade (they touch different entities / the same channel only while a
    // fade is pending or live).
    app.add_systems(
        Update,
        (
            arm_appear_fade,
            apply_despawn_fade,
            apply_render_fade,
            // Retires the unit-root clock (`UnitAppearFade`) once its mirrored ramp completes, so
            // a part spawning after the unit has fully appeared reads no marker and spawns steady
            // (`entities::attach`/`entities::equipment` join off it).
            retire_unit_appear_fade,
        )
            .chain(),
    )
    // Publish those same ramps as ONE number per model instance, for the consumers that are not
    // meshes and so cannot read a `MeshTag`: an emitter's particles and a ribbon's strip (decision
    // 0827 — the reference's `CM2Model+0x19c`). PostUpdate, so every Update-side fade writer and
    // the camera controller's self feather have already run this frame, and before the effect sims
    // that read it.
    .add_systems(
        PostUpdate,
        publish_model_alpha.before(crate::billboard::BillboardPlace),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `0x714000` recursion as a lookup (decision 0833). A weapon's enchant glow is **two**
    /// links from the body wearing it — glow → item → wearer — and the director's report is what
    /// happens when a lane has no expression for that at all: the glow's particles blazed at full
    /// strength over a character that was still at alpha 0, because the site that spawned them
    /// could name only its immediate host and was asked for the far end of the chain.
    ///
    /// The `dimmed` case pins the composition as the reference's **product** (own × parent's), not
    /// a pick of one — that is what makes a fading item on a fading wearer behave.
    #[test]
    fn an_attached_models_alpha_composes_through_the_whole_chain() {
        use bevy::ecs::system::SystemState;

        let mut world = World::new();
        let wearer = world.spawn(ModelAlpha(0.25)).id();
        let item = world.spawn(ParentModel(wearer)).id();
        let glow = world.spawn(ParentModel(item)).id();
        let dimmed = world.spawn((ModelAlpha(0.5), ParentModel(wearer))).id();
        let loose = world.spawn_empty().id();
        let gone = world.spawn(ParentModel(wearer)).id();
        world.despawn(gone);

        let mut state: SystemState<ModelAlphas> = SystemState::new(&mut world);
        let alphas = state.get(&world);
        assert_eq!(alphas.get(wearer), 0.25, "the unit's own");
        assert_eq!(alphas.get(item), 0.25, "an item takes its wearer's");
        assert_eq!(alphas.get(glow), 0.25, "…and its glow takes the item's");
        assert_eq!(alphas.get(dimmed), 0.125, "own × parent's — a product");
        assert_eq!(alphas.get(loose), 1.0, "chained to nothing ⇒ opaque");
        assert_eq!(
            alphas.get(gone),
            1.0,
            "a despawned instance never gates a draw"
        );
    }

    /// The cycle backstop: [`MAX_MODEL_CHAIN`] bounds the walk, so a link that somehow closes on
    /// itself costs a few lookups instead of hanging the frame. Nothing builds one today — that is
    /// exactly why the guard is worth a test rather than a comment.
    #[test]
    fn a_looping_chain_terminates() {
        use bevy::ecs::system::SystemState;

        let mut world = World::new();
        let a = world.spawn_empty().id();
        let b = world.spawn(ParentModel(a)).id();
        world.entity_mut(a).insert(ParentModel(b));

        let mut state: SystemState<ModelAlphas> = SystemState::new(&mut world);
        assert_eq!(state.get(&world).get(a), 1.0);
    }

    /// The 0200 login crash, reproduced at the seam: [`arm_appear_fade`]'s query sees the entity
    /// alive, a wire despawn applies first, and only then do the system's own commands land — the
    /// `try_*` contract makes that a no-op instead of a panic. `SystemState::apply` after the
    /// manual despawn recreates the exact interleaving of the parallel schedule.
    #[test]
    #[allow(clippy::type_complexity)] // the SystemState tuple IS the system's real signature
    fn arm_appear_fade_tolerates_a_same_frame_wire_despawn() {
        use bevy::ecs::system::SystemState;

        let mut world = World::new();
        world.init_resource::<Time>();
        // Default viewer = nothing covering the world = "world shown" — every pending fade arms
        // this frame.
        world.init_resource::<crate::view::Viewer>();
        let doomed = world.spawn(PendingAppearFade { since: 0.0 }).id();

        let mut state: SystemState<(
            Res<Time>,
            Res<crate::view::Viewer>,
            Commands,
            Query<(
                Entity,
                &PendingAppearFade,
                Has<crate::billboard::BillboardCard>,
            )>,
            Query<&mut UnitAppearFade>,
        )> = SystemState::new(&mut world);
        let (time, viewer, commands, q, units) = state.get_mut(&mut world);
        arm_appear_fade(time, viewer, commands, q, units);

        // The wire destroy beats the fade commands to the sync point.
        world.despawn(doomed);
        state.apply(&mut world); // would panic without the `try_*` contract
        assert!(world.get_entity(doomed).is_err(), "stays despawned");
    }

    /// Decision 0755: one law-aware material rule, shared by every fade writer. A bake-classified
    /// part feathers probe-lit and settles probe-lit; everything else rides the exterior pair. The
    /// pair is resolved per frame from the part's LIVE law, so a part that classifies during its
    /// ramp is lit by its room for the rest of it instead of finishing on the law that happened to
    /// hold when the fade armed.
    #[test]
    fn the_material_rule_follows_the_law_not_the_arm_instant() {
        use crate::interior::{InteriorKind, InteriorLit};

        let cutout: Handle<WowModelMaterial> =
            bevy::asset::uuid_handle!("c0000000-0000-4000-8000-000000000001");
        let blend: Handle<WowModelMaterial> =
            bevy::asset::uuid_handle!("b1000000-0000-4000-8000-000000000002");
        let bake: Handle<WowModelMaterial> =
            bevy::asset::uuid_handle!("ba000000-0000-4000-8000-000000000003");
        let bake_blend: Handle<WowModelMaterial> =
            bevy::asset::uuid_handle!("bb000000-0000-4000-8000-000000000004");
        let fm = FadeMaterials {
            cutout: cutout.clone(),
            blend: blend.clone(),
            bake_blend: Some(bake_blend.clone()),
            zfill: None,
        };

        // Unclassified (no `InteriorLit` at all — a WMO-display part): the exterior pair.
        assert_eq!(*fm.material_for(None, true), blend);
        assert_eq!(*fm.material_for(None, false), cutout);

        // Bake-CAPABLE but not yet resolved indoors (the state a part spawns in): exterior pair.
        let kind = InteriorKind::Bake {
            material: bake.clone(),
            center: Vec3::ZERO,
        };
        let unresolved = InteriorLit::new(kind.clone(), cutout.clone());
        assert_eq!(*fm.material_for(Some(&unresolved), true), blend);
        assert_eq!(*fm.material_for(Some(&unresolved), false), cutout);

        // The law flips to the room's bake MID-RAMP: the very next frame feathers probe-lit, and
        // the latch settles on the bake variant — no law change, and so no pop, at the latch.
        let lit = InteriorLit::applied_bake_for_test(kind, cutout.clone());
        assert_eq!(*fm.material_for(Some(&lit), true), bake_blend);
        assert_eq!(*fm.material_for(Some(&lit), false), bake);

        // A part with no bake blend twin authored falls back to the exterior blend rather than
        // dropping the feather.
        let no_twin = FadeMaterials {
            bake_blend: None,
            zfill: None,
            ..fm.clone()
        };
        assert_eq!(*no_twin.material_for(Some(&lit), true), blend);
    }

    #[test]
    fn trees_and_buildings_never_fade() {
        // radius > 7.0 → always 1.0 regardless of distance.
        assert_eq!(doodad_fade_alpha(7.01, 0.0), 1.0);
        assert_eq!(doodad_fade_alpha(20.0, 500.0), 1.0);
        assert_eq!(doodad_fade_alpha(7.0001, 195.0), 1.0);
    }

    /// The property the gx exile scan (B2, 1431) rests on: outside the band edges — with the
    /// scan's own 1-yd hysteresis of margin — the alpha is EXACTLY the constant the retained
    /// pass assumes (1.0 steady / 0.0 gone), and strictly between them inside. A band table
    /// duplicated into the scan is the drift this test forbids: `fade_band` derives from the
    /// same `BUCKETS` row `doodad_fade_alpha` evaluates.
    #[test]
    fn fade_band_edges_agree_with_the_alpha_they_summarize() {
        for radius in [0.0, 0.3, 0.5, 1.7, 2.5, 4.0, 7.0] {
            let (near, far) = fade_band(radius).expect("faders carry a band");
            assert_eq!(doodad_fade_alpha(radius, near - 1.0), 1.0, "r={radius}");
            assert_eq!(doodad_fade_alpha(radius, far + 1.0), 0.0, "r={radius}");
            let mid = doodad_fade_alpha(radius, (near + far) / 2.0);
            assert!(mid > 0.0 && mid < 1.0, "r={radius} mid={mid}");
        }
        assert_eq!(fade_band(7.01), None, "never-faders have no band");
    }

    #[test]
    fn small_props_fade_40_to_50() {
        // radius ≤ 0.5: band on d = dist − radius. Use radius 0.0 so d == dist for clean goldens.
        let r = 0.0;
        assert_eq!(doodad_fade_alpha(r, 39.0), 1.0); // before the band → opaque
        assert_eq!(doodad_fade_alpha(r, 40.0), 1.0); // band start → opaque
        assert!((doodad_fade_alpha(r, 45.0) - 0.5).abs() < 1e-6); // midpoint → half
        assert_eq!(doodad_fade_alpha(r, 50.0), 0.0); // band end → gone
        assert_eq!(doodad_fade_alpha(r, 60.0), 0.0); // past the band → gone
    }

    #[test]
    fn radius_offsets_the_band() {
        // d = dist − radius, so a 0.5-yd prop reaches band start (d=40) at center distance 40.5.
        assert_eq!(doodad_fade_alpha(0.5, 40.5), 1.0);
        assert!((doodad_fade_alpha(0.5, 45.5) - 0.5).abs() < 1e-6);
        assert_eq!(doodad_fade_alpha(0.5, 50.5), 0.0);
    }

    #[test]
    fn mid_bucket_100_to_125() {
        // 0.5 < radius ≤ 2.5 → band 100→125 (range 25). radius 2.5 → d = dist − 2.5.
        let r = 2.5;
        assert_eq!(doodad_fade_alpha(r, 100.0 + r), 1.0);
        assert!((doodad_fade_alpha(r, 112.5 + r) - 0.5).abs() < 1e-6);
        assert_eq!(doodad_fade_alpha(r, 125.0 + r), 0.0);
    }

    #[test]
    fn large_bucket_150_to_200() {
        // 2.5 < radius ≤ 7.0 → band 150→200 (range 50). radius 5.0 → d = dist − 5.0.
        let r = 5.0;
        assert_eq!(doodad_fade_alpha(r, 150.0 + r), 1.0);
        assert!((doodad_fade_alpha(r, 175.0 + r) - 0.5).abs() < 1e-6);
        assert_eq!(doodad_fade_alpha(r, 200.0 + r), 0.0);
    }

    #[test]
    fn monotonic_bigger_fades_farther() {
        // At a fixed far distance a larger prop should be more visible than a smaller one (bigger =
        // fades at a greater distance). Compare a small prop (≤0.5, band ~40-50) vs a large one
        // (≤7, band ~150-200) at 120 yd: small is long gone, large is still fully opaque.
        let small = doodad_fade_alpha(0.3, 120.0);
        let large = doodad_fade_alpha(6.0, 120.0);
        assert_eq!(small, 0.0);
        assert_eq!(large, 1.0);
        assert!(large >= small);
    }

    #[test]
    fn appear_fade_is_cubic_0_to_1() {
        assert_eq!(fade_alpha(0.0, 1.0, 0.0), 0.0); // spawn: invisible
        assert!((fade_alpha(0.0, 1.0, 0.5) - 0.125).abs() < 1e-6); // t³: half-time ⇒ 1/8 (accelerating)
        assert_eq!(fade_alpha(0.0, 1.0, 1.0), 1.0); // latches opaque
        assert_eq!(fade_alpha(0.0, 1.0, -1.0), 0.0); // clamps below
        assert_eq!(fade_alpha(0.0, 1.0, 2.0), 1.0); // clamps above
    }

    #[test]
    fn despawn_fade_eases_to_0() {
        // The same curve targeting 0 (the coming despawn fade-out): 1 + (0−1)·t³.
        assert_eq!(fade_alpha(1.0, 0.0, 0.0), 1.0);
        assert!((fade_alpha(1.0, 0.0, 0.5) - 0.875).abs() < 1e-6);
        assert_eq!(fade_alpha(1.0, 0.0, 1.0), 0.0);
    }

    #[test]
    fn self_fade_hidden_at_first_person() {
        // Camera on top of the target (zoomed fully in): D ≤ SELF_FADE_HIDE ⇒ hard-hide.
        let nc = 1.0;
        assert_eq!(self_model_fade_alpha(nc, nc, SELF_FADE_WINDOW), 0.0); // dist == nearclip → D = 0
        assert_eq!(self_model_fade_alpha(0.0, nc, SELF_FADE_WINDOW), 0.0); // inside the near clip
        assert_eq!(
            self_model_fade_alpha(nc + SELF_FADE_HIDE, nc, SELF_FADE_WINDOW),
            0.0
        ); // exactly at the hide threshold
    }

    #[test]
    fn self_fade_opaque_when_zoomed_out() {
        // Camera a full window (or more) beyond the near clip ⇒ fully opaque.
        let nc = 1.0;
        assert_eq!(
            self_model_fade_alpha(nc + SELF_FADE_WINDOW, nc, SELF_FADE_WINDOW),
            1.0
        );
        assert_eq!(self_model_fade_alpha(100.0, nc, SELF_FADE_WINDOW), 1.0);
    }

    #[test]
    fn no_unit_marker_joins_steady() {
        // A part attached to a unit with no in-flight appear-fade (already settled, or a WMO-display
        // entity that never fades at all) spawns steady — matches today's long-after-login behavior.
        assert_eq!(join_unit_appear_fade(None), JoinedFade::Steady);
    }

    #[test]
    fn pending_unit_joins_at_the_same_since() {
        // A part spawning while the unit is still behind the loading screen joins the same "arm once
        // shown" timer — not its own, so both go live on the same frame.
        let since = 3.25;
        assert_eq!(
            join_unit_appear_fade(Some(UnitAppearFade::Pending { since })),
            JoinedFade::Pending { since }
        );
    }

    #[test]
    fn live_unit_joins_at_the_same_started() {
        let started = 12.5;
        assert_eq!(
            join_unit_appear_fade(Some(UnitAppearFade::Live { started })),
            JoinedFade::Live { started }
        );
    }

    #[test]
    fn joining_mid_ramp_reproduces_the_original_curve() {
        // The whole point of copying `started` instead of resetting to zero or sampling a stored
        // alpha: a part that joins 0.7s into the body's ramp must read the *same* alpha as the body
        // does at every subsequent instant, because both derive it from one shared `started` through
        // the same pure `fade_alpha` — no synchronization between the two entities is needed.
        let started = 1.4; // the body's ramp began at t = 1.4s (arbitrary wall-clock origin)
        let joined = join_unit_appear_fade(Some(UnitAppearFade::Live { started }));
        assert_eq!(joined, JoinedFade::Live { started });
        for now in [1.4f32, 1.7, 2.0, 2.9, 3.4] {
            let body_alpha = fade_alpha(0.0, 1.0, (now - started) / APPEAR_FADE_SECS);
            let JoinedFade::Live {
                started: joined_started,
            } = joined
            else {
                unreachable!()
            };
            let item_alpha = fade_alpha(0.0, 1.0, (now - joined_started) / APPEAR_FADE_SECS);
            assert_eq!(body_alpha, item_alpha);
        }
    }

    /// **The login symptom, at its cause** (decision 0827): a unit whose appear-fade has not been
    /// armed yet is not being shown at all, so its render alpha is **0** — and the item sparkle
    /// that reads this number is therefore invisible too, instead of burning at full strength in
    /// front of a body that hasn't faded in. Then the ramp: the same cubic the mesh parts run, so
    /// the pauldron's glow and the pauldron arrive together rather than 2 s apart.
    #[test]
    fn a_units_render_alpha_is_zero_until_it_is_shown_then_rides_the_same_cubic() {
        assert_eq!(
            model_render_alpha(
                10.0,
                Some(UnitAppearFade::Pending { since: 9.0 }),
                None,
                1.0,
                1.0
            ),
            0.0,
            "pending: the unit is not on screen, and neither are its effects"
        );
        let live = |now| {
            model_render_alpha(
                now,
                Some(UnitAppearFade::Live { started: 10.0 }),
                None,
                1.0,
                1.0,
            )
        };
        assert_eq!(live(10.0), 0.0, "the ramp starts at nothing");
        assert!(
            (live(11.0) - fade_alpha(0.0, 1.0, 0.5)).abs() < 1e-6,
            "mid-ramp is the mesh parts' own cubic, not a second curve"
        );
        assert_eq!(live(12.0), 1.0, "…and latches opaque at the duration");
        assert_eq!(
            model_render_alpha(10.0, None, None, 1.0, 1.0),
            1.0,
            "no fade: opaque"
        );
    }

    /// The other two writers of the same slot compose as a PRODUCT — the reference multiplies the
    /// transition alpha into `+0x180` rather than keeping a second channel. The self-avatar feather
    /// is what takes a held torch's flame out of your face in first person (ledger F05).
    #[test]
    fn the_render_alpha_multiplies_its_writers() {
        // Zooming to first person: the body is opaque-by-appear but the feather is 0.
        assert_eq!(model_render_alpha(20.0, None, None, 0.0, 1.0), 0.0);
        assert!((model_render_alpha(20.0, None, None, 0.5, 1.0) - 0.5).abs() < 1e-6);
        // Streaming out: the despawn ramp eases the same channel to nothing.
        assert_eq!(model_render_alpha(10.0, None, Some(10.0), 1.0, 1.0), 1.0);
        assert_eq!(model_render_alpha(12.0, None, Some(10.0), 1.0, 1.0), 0.0);
        // Both at once stay a product, and the result can never leave [0, 1].
        let both = model_render_alpha(
            11.0,
            Some(UnitAppearFade::Live { started: 10.0 }),
            Some(10.0),
            0.5,
            1.0,
        );
        assert!((0.0..=1.0).contains(&both) && both > 0.0);
    }

    /// **The stealth-particle symptom, at its cause**: the aura CharProc alpha (stealth 0.3,
    /// invisibility 0.5) is a factor of the SAME render-alpha slot in the reference (`0x60d180`'s
    /// recompute drives the same `StartAlphaFade` the appear-fade uses), which is what carries a
    /// stealthed rogue's weapon-enchant glow and kit particles down with the body. Before this
    /// factor existed, `ModelAlpha` stayed 1.0 under stealth and every emitter burned at full
    /// brightness beside a 30 %-alpha body.
    #[test]
    fn the_aura_alpha_is_a_factor_of_the_effect_render_alpha() {
        assert!((model_render_alpha(20.0, None, None, 1.0, 0.3) - 0.3).abs() < 1e-6);
        // …and it composes with the other writers as a product, never a replacement.
        assert!((model_render_alpha(20.0, None, None, 0.5, 0.3) - 0.15).abs() < 1e-6);
        assert_eq!(model_render_alpha(20.0, None, Some(20.0), 1.0, 0.3), 0.3);
    }

    #[test]
    fn unit_fade_retires_after_the_appear_duration() {
        // The join marker itself doesn't retire (that's `retire_unit_appear_fade`, an ECS system), but
        // its retirement condition is a pure time check — pin it here so the threshold can't drift.
        let started = 5.0;
        let almost_done = started + APPEAR_FADE_SECS - 0.001;
        let done = started + APPEAR_FADE_SECS;
        assert!(almost_done - started < APPEAR_FADE_SECS);
        assert!(done - started >= APPEAR_FADE_SECS);
    }

    #[test]
    fn self_fade_cosine_ramp_midpoint() {
        // Half-way through the window the cosine smoothstep passes through 0.5 (cos(π/2) = 0).
        let nc = 1.0;
        let mid = nc + SELF_FADE_WINDOW / 2.0;
        assert!((self_model_fade_alpha(mid, nc, SELF_FADE_WINDOW) - 0.5).abs() < 1e-6);
        // Monotonic: closer ⇒ more transparent than farther, across the ramp.
        let near = self_model_fade_alpha(nc + 0.4, nc, SELF_FADE_WINDOW);
        let far = self_model_fade_alpha(nc + 1.4, nc, SELF_FADE_WINDOW);
        assert!(near < far);
        assert!((0.0..=1.0).contains(&near) && (0.0..=1.0).contains(&far));
    }

    /// **The stream-out half of decision 0836.** The arm walk descends `Children`, and a billboard
    /// card is a world ROOT that only *follows* an anchor inside the tree — so a unit fading out
    /// left its eye glow / gem cards at full strength for the whole 2 s and then blinked them out
    /// with the despawn. They're picked up by their follow-anchor, exactly as the self-avatar
    /// feather picks them up.
    ///
    /// The second card, following nothing of this unit's, is the negative half: the sweep is by
    /// membership, not "every card on screen".
    #[test]
    fn a_despawn_fade_reaches_the_models_billboard_cards() {
        let fm = || FadeMaterials {
            cutout: Handle::default(),
            blend: Handle::default(),
            bake_blend: None,
            zfill: None,
        };
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        let unit = app.world_mut().spawn((Transform::default(), fm())).id();
        // A joint under the unit, an attach root under it, and the card following that root —
        // the real held-item shape, three levels deep.
        let joint = app.world_mut().spawn(Transform::default()).id();
        let root = app.world_mut().spawn(Transform::default()).id();
        app.world_mut().entity_mut(unit).add_child(joint);
        app.world_mut().entity_mut(joint).add_child(root);
        let info = benilla_assets::BillboardInfo {
            pivot: Vec3::ZERO,
            bone: 1,
            kind: benilla_formats::BillboardKind::Spherical,
            scale_anim: None,
            seq_translations: Vec::new(),
        };
        let mine = app
            .world_mut()
            .spawn((
                crate::billboard::BillboardCard::following(&info, root),
                fm(),
            ))
            .id();
        let stranger = app.world_mut().spawn(Transform::default()).id();
        let theirs = app
            .world_mut()
            .spawn((
                crate::billboard::BillboardCard::following(&info, stranger),
                fm(),
            ))
            .id();
        app.world_mut()
            .entity_mut(unit)
            .insert(DespawnFade::default());
        app.add_systems(Update, apply_despawn_fade);
        app.update();

        let out = |e: Entity| {
            app.world()
                .entity(e)
                .get::<RenderFade>()
                .map(|f| (f.from, f.to))
        };
        assert_eq!(
            out(unit),
            Some((1.0, 0.0)),
            "the body arms, as it always did"
        );
        assert_eq!(out(mine), Some((1.0, 0.0)), "…and so does its own card");
        assert_eq!(out(theirs), None, "a stranger's card is left alone");
    }
}

#[cfg(test)]
mod inversion_tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// The two rewired inputs of [`publish_model_alpha`], end to end.
    ///
    /// Neither lane has a capture behind it — the golden six are scenery and carry no auras, and
    /// nothing screenshots a first-person zoom — so this is the only thing standing between a
    /// mis-wired multiplier and a director noticing that stealth stopped working. Both used to be
    /// direct reads of gameplay types (`aura_visual::AuraNodes`, `player::CameraControl`); they are
    /// now a component the game declares and a field on `view::Viewer`.
    #[test]
    fn a_declared_fade_and_the_self_feather_both_reach_the_composed_alpha() {
        let mut app = App::new();
        app.init_resource::<Time>();
        app.insert_resource(crate::view::Viewer {
            self_fade: 0.5,
            ..default()
        });

        let plain = app
            .world_mut()
            .spawn((world_unit_for_test(), ModelFade(0.25)))
            .id();
        let me = app
            .world_mut()
            .spawn((world_unit_for_test(), crate::world_unit::ViewerUnit))
            .id();
        let both = app
            .world_mut()
            .spawn((
                world_unit_for_test(),
                crate::world_unit::ViewerUnit,
                ModelFade(0.5),
            ))
            .id();

        app.world_mut()
            .run_system_once(publish_model_alpha)
            .expect("publisher runs");

        let w = app.world();
        let got = |e| w.get::<ModelAlpha>(e).expect("published").0;
        assert!(
            (got(plain) - 0.25).abs() < 1e-6,
            "a declared fade reaches the alpha on any body"
        );
        assert!(
            (got(me) - 0.5).abs() < 1e-6,
            "the self feather applies to the viewer's body and nothing else"
        );
        assert!(
            (got(both) - 0.25).abs() < 1e-6,
            "…and the two multiply (0.5 feather x 0.5 declared) rather than one winning"
        );
    }

    fn world_unit_for_test() -> crate::world_unit::WorldUnit {
        crate::world_unit::WorldUnit {
            wades: true,
            scale: 1.0,
            height: 2.0,
            bound: None,
        }
    }
}
