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

use crate::terrain::WowModelMaterial;
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
    pub radius: f32,
    /// The model's authored bounding-box **centre** in Bevy model-local space. At runtime the entity's
    /// `GlobalTransform` maps it to the world sphere centre (the reference's `rec+0x5c/0x60`); the fade
    /// distance is measured to THAT, not the placement origin — `FUN_006952a0` transforms `(min+max)/2`.
    pub local_center: Vec3,
    /// Steady-state material (the submesh's authored blend mode: opaque trunk / alpha-test canopy).
    pub cutout: Handle<WowModelMaterial>,
    /// `AlphaMode::Blend` variant of the same texture — used only while the object is feathering.
    pub blend: Handle<WowModelMaterial>,
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
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct ModelAlpha(pub f32);

/// The render alpha of one streamed model instance this frame — pure, so the composition is pinned
/// by tests without an ECS (the `model_fade` pattern):
///
/// - the **appear** ramp: none ⇒ opaque; pending ⇒ 0 (the entity is not being shown yet, so its
///   effects must not be either — this is the login symptom); live ⇒ the cubic ramp,
/// - × the **despawn** ramp (our stream-out look, 0032/0067) once armed,
/// - × the **self-avatar** zoom feather, which applies to the player's own body alone.
///
/// The product is the reference's own shape: it multiplies the transition alpha into the same
/// `+0x180` slot rather than keeping a second channel.
pub fn model_render_alpha(
    now: f32,
    appear: Option<UnitAppearFade>,
    despawn_started: Option<f32>,
    self_fade: f32,
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
    (appear * despawn * self_fade).clamp(0.0, 1.0)
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
    // `Option`: the rig is the world app's (a booth-only or test app has none) — the same shape
    // `blob_shadow` and `nameplates` read the self feather through.
    rig: Option<Res<crate::player::CameraControl>>,
    mut commands: Commands,
    mut units: Query<
        (
            Entity,
            Option<&UnitAppearFade>,
            Option<&DespawnFade>,
            Has<crate::net::SelfPlayer>,
            Option<&mut ModelAlpha>,
        ),
        With<crate::net::NetEntity>,
    >,
) {
    let now = time.elapsed_secs();
    for (entity, appear, despawn, is_self, current) in &mut units {
        let alpha = model_render_alpha(
            now,
            appear.copied(),
            despawn.map(|d| d.started).filter(|s| *s >= 0.0),
            if is_self {
                rig.as_deref()
                    .map_or(1.0, crate::player::CameraControl::self_fade)
            } else {
                1.0
            },
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
/// plane would begin to slice the model, which is why the two are coupled (see `crate::player::CAM_NEAR`).
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
pub(crate) fn apply_render_fade(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(
        Entity,
        &RenderFade,
        &mut MeshTag,
        &mut MeshMaterial3d<WowModelMaterial>,
        Option<&crate::doodad_anim::MatAnim>,
        Option<&FadeMaterials>,
        Option<&crate::interior::InteriorLit>,
    )>,
) {
    let now = time.elapsed_secs();
    for (entity, fade, mut tag, mut mat, anim, fm, lit) in &mut q {
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
            let want = fm.material_for(lit, alpha < 1.0);
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
    screen: Res<crate::loading_screen::LoadingScreen>,
    mut commands: Commands,
    q: Query<(Entity, &PendingAppearFade)>,
    mut units: Query<&mut UnitAppearFade>,
) {
    let now = time.elapsed_secs();
    let shown = !screen.covering();
    let mut armed = 0usize;
    for (entity, pending) in &q {
        if shown || now - pending.since > PENDING_TIMEOUT_SECS {
            armed += 1;
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
        eprintln!("[fade-arm] t {now:.2} armed {armed} parts (world shown: {shown})");
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

/// Marks a streamed entity (the parent) that went out of range: instead of popping it out,
/// [`apply_despawn_fade`] fades it out then despawns it. **Our** stream-out look, not a verified
/// mechanism — the wow-re teardown RE found no fade-out in the binary (a *destroyed* object pops
/// instantly, and the net bridge despawns it directly, bypassing this). `started < 0` ⇒ not yet
/// armed/stamped.
#[derive(Component)]
pub struct DespawnFade {
    pub started: f32,
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
pub(crate) fn apply_despawn_fade(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut DespawnFade)>,
    children_of: Query<&Children>,
    fm: Query<(), With<FadeMaterials>>,
) {
    let now = time.elapsed_secs();
    for (parent, mut df) in &mut q {
        if df.started < 0.0 {
            let mut any = false;
            arm_despawn_descendants(parent, now, &mut commands, &children_of, &fm, &mut any);
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

/// Depth-first helper for [`apply_despawn_fade`]: arm the fade-out on `entity` if it carries
/// [`FadeMaterials`], and recurse into its children either way (a joint / held-item root carries none
/// itself but has fadeable meshes beneath it).
fn arm_despawn_descendants(
    entity: Entity,
    now: f32,
    commands: &mut Commands,
    children_of: &Query<&Children>,
    fm: &Query<(), With<FadeMaterials>>,
    any: &mut bool,
) {
    if fm.contains(entity) {
        *any = true;
        // No material is chosen here: `apply_render_fade` resolves it per frame from the part's
        // live law (0755), so a bake-classified part fades out probe-lit — and keeps doing so if
        // it re-classifies mid-ramp, which a pair latched at this instant could not express.
        // `try_*`: a child (a held item mid-re-resolve, a gear swap) can be despawned by its own
        // owner in the same frame — the lifetime contract in `arm_appear_fade`.
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
    if let Ok(children) = children_of.get(entity) {
        for &child in children {
            arm_despawn_descendants(child, now, commands, children_of, fm, any);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Default screen = not covering = "world shown" — every pending fade arms this frame.
        world.init_resource::<crate::loading_screen::LoadingScreen>();
        let doomed = world.spawn(PendingAppearFade { since: 0.0 }).id();

        let mut state: SystemState<(
            Res<Time>,
            Res<crate::loading_screen::LoadingScreen>,
            Commands,
            Query<(Entity, &PendingAppearFade)>,
            Query<&mut UnitAppearFade>,
        )> = SystemState::new(&mut world);
        let (time, screen, commands, q, units) = state.get_mut(&mut world);
        arm_appear_fade(time, screen, commands, q, units);

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
                1.0
            ),
            0.0,
            "pending: the unit is not on screen, and neither are its effects"
        );
        let live =
            |now| model_render_alpha(now, Some(UnitAppearFade::Live { started: 10.0 }), None, 1.0);
        assert_eq!(live(10.0), 0.0, "the ramp starts at nothing");
        assert!(
            (live(11.0) - fade_alpha(0.0, 1.0, 0.5)).abs() < 1e-6,
            "mid-ramp is the mesh parts' own cubic, not a second curve"
        );
        assert_eq!(live(12.0), 1.0, "…and latches opaque at the duration");
        assert_eq!(
            model_render_alpha(10.0, None, None, 1.0),
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
        assert_eq!(model_render_alpha(20.0, None, None, 0.0), 0.0);
        assert!((model_render_alpha(20.0, None, None, 0.5) - 0.5).abs() < 1e-6);
        // Streaming out: the despawn ramp eases the same channel to nothing.
        assert_eq!(model_render_alpha(10.0, None, Some(10.0), 1.0), 1.0);
        assert_eq!(model_render_alpha(12.0, None, Some(10.0), 1.0), 0.0);
        // Both at once stay a product, and the result can never leave [0, 1].
        let both = model_render_alpha(
            11.0,
            Some(UnitAppearFade::Live { started: 10.0 }),
            Some(10.0),
            0.5,
        );
        assert!((0.0..=1.0).contains(&both) && both > 0.0);
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
}
