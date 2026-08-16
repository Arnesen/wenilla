//! Animated **materials** on placed models (decision 0130 phases 2/3): the per-batch baked
//! colour-alpha/weight loops ([`MatAnim`] — composed into the render-alpha `MeshTag` by the
//! visibility authority) and the shared-clock UV-scroll / tint registries
//! ([`UvAnimMaterials`] / [`TintAnimMaterials`]) their samplers tick. Split from the host module
//! ([`super`]) along the concern seam: this file is about *material* channels; the host file is
//! about *bone* channels (the anim host, the variation windows, the draw gate).

use bevy::prelude::*;

use benilla_assets::ModelAnimations;

/// Per-submesh **animated material alpha** (decision 0130 phase 2): the batch's baked
/// colour-alpha/weight loops + how this instance clocks them. [`sample_mat_anim`] keeps
/// [`Self::current`] fresh; the model-`Visibility` authority multiplies it into the render-alpha
/// `MeshTag` it already owns (a composed *input*, not an extra tag writer — the 0066 protocol) and
/// hides the batch at combined 0 (the verified `A ≤ 0` cull, wow-re `m2-alpha-combine-cull`).
///
/// The loops are baked **per sequence** (`benilla_formats::AlphaAnim`), so an instance also has to
/// say *which* sequence it is playing — see [`Self::host`].
#[derive(Component)]
pub struct MatAnim {
    anim: std::sync::Arc<benilla_formats::AlphaAnim>,
    /// The entity whose `AnimationPlayer` decides which sequence's loops to read, re-resolved every
    /// frame — the **unit lane**, where the played sequence changes constantly and the batch's
    /// authored visibility changes with it (a voidwalker's upper armour is weight 0 in Stand and 1
    /// only in Death). `None` for an instance pinned to one sequence for its life: a placed doodad
    /// (armed once at load with `animations[0]`, wow-re `doodad-anim-host.md`) or a spell effect —
    /// both then read [`Self::seq`] on the spawn clock, which is the pre-per-sequence behaviour.
    host: Option<Entity>,
    /// The sequence **file slot** to read: fixed at spawn for the pinned lanes, and the last one
    /// resolved from [`Self::host`] for the unit lane. `None` ⇒ slot 0 (the bake's own degrade).
    seq: Option<usize>,
    /// `Time::elapsed_secs` at spawn — the clock origin (arm-time phase, like the bone host). Only
    /// the pinned lanes use it; a hosted instance reads the player's own seek time instead, so its
    /// alpha stays in phase with the pose that drives it.
    spawned_at: f32,
    /// Captures freeze the clock at 0 for deterministic frames (dimming constants still show).
    frozen: bool,
    /// This instance's sampled value drives the render-alpha `MeshTag` field **by itself** (no
    /// `DoodadFade` on the entity): the spell-effect parts (`entities::spell_fx`), whose alpha
    /// channel has no other writer. `false` on the doodad lane — there the visibility authority
    /// owns the tag and composes [`Self::current`] in (for a fade holder multiplied with the
    /// distance fade; for a lit interior prop written alone into the probe payload's alpha field,
    /// bits 0..=15 since the 0355 re-lane) — and on the unit lane, whose own compose is
    /// [`crate::entities::apply_unit_mat_alpha`].
    pub(crate) drives_tag: bool,
    /// This instance belongs to the **unit lane's** tag compose
    /// ([`crate::entities::apply_unit_mat_alpha`]) even though it has no [`Self::host`] to read a
    /// sequence from: the ATTACH-MODEL case (a held weapon, a helm, a pauldron). Such a model
    /// spawns no rig — it rests in its file's first sequence, so its loops are pinned like a
    /// placed doodad's — but it hangs off a unit, so the compose has to be the one ordered against
    /// the wearer's appear-fade and interior classifier rather than the world-model visibility
    /// authority's. See [`Self::resting`].
    unit_lane: bool,
    /// The gseq factors' ATTACH anchor (secs on the shared clock): `None` until the first
    /// [`sample_mat_anim`] pass stamps it — the reference snapshots the scene clock once per
    /// model instance at attach (`CM2Model+0x68`, wow-re `gseq-anchor.md`; decisions 0856/0858).
    gseq_attach: Option<f64>,
    /// The last sampled combined factor (colour-alpha × weight), read by the visibility authority.
    pub current: f32,
}

impl MatAnim {
    pub(crate) fn new(
        anim: std::sync::Arc<benilla_formats::AlphaAnim>,
        now: f32,
        frozen: bool,
    ) -> Self {
        // The seed sample: both clocks at 0 — nothing armed, and the attach anchor (stamped on
        // the first live pass) makes the gseq cursor open at 0 too.
        let current = anim.sample(None, 0.0, 0.0);
        Self {
            anim,
            host: None,
            seq: None,
            spawned_at: now,
            frozen,
            drives_tag: false,
            unit_lane: false,
            gseq_attach: None,
            current,
        }
    }

    /// The spell-effect-lane constructor: never frozen (the `fxview` instrument ages effects
    /// through captures; golden scenarios spawn no effects), the sampled alpha drives the part's
    /// render-alpha tag directly (see [`Self::drives_tag`]), and the instance is pinned to the one
    /// sequence its rig plays (`seq` — the missile's InFlight, else the file-order-first clip).
    pub fn driving_tag(
        anim: std::sync::Arc<benilla_formats::AlphaAnim>,
        now: f32,
        seq: Option<usize>,
    ) -> Self {
        let mut m = Self::new(anim, now, false);
        m.drives_tag = true;
        m.seq = seq;
        m.current = m.anim.sample(seq, 0.0, 0.0);
        m
    }

    /// Read the sequence (and its clock) from `host`'s live `AnimationPlayer` each frame instead of
    /// staying pinned to the slot this instance opened on — for a spell-effect instance that
    /// **advances** through its authored lifecycle (`Stand` → `Hold` → `Decay`, wow-re
    /// `ceffect-anim-lifecycle.md`), because each leg has its own authored alpha loops: Ice
    /// Barrier's pulse is as much the `Hold` band's oscillating transparency weights as its bone
    /// scale. `None` leaves the instance pinned (a lane with no rig has no player to ask).
    ///
    /// It keeps [`Self::drives_tag`], so this stays an effect part writing its own tag — it borrows
    /// the unit lane's *sequence source*, not its tag-compose ownership.
    pub fn following_host(mut self, host: Option<Entity>) -> Self {
        self.host = host;
        self
    }

    /// The **unit-lane** constructor: the sequence (and its clock) come from `host`'s live
    /// `AnimationPlayer`, so a creature's batches appear and disappear with the animation exactly
    /// as authored. The tag is composed by [`crate::entities::apply_unit_mat_alpha`], not driven
    /// here — the interior classifier and the appear-fade already own that channel.
    pub fn following(anim: std::sync::Arc<benilla_formats::AlphaAnim>, host: Entity) -> Self {
        Self {
            host: Some(host),
            ..Self::new(anim, 0.0, false)
        }
    }

    /// The **attach-model** constructor (a held weapon, a helm, a pauldron): the loops are pinned
    /// to the file's first sequence — an item model spawns no rig and rests there, the same reason
    /// its emitters and ribbons read Stand — while the tag compose stays the unit lane's, ordered
    /// against the wearer's appear-fade and interior classifier.
    ///
    /// Its clock is irrelevant in practice (a rest-pose model's tracks are the constants the file
    /// authors) so it takes no `now`; sampling still runs every frame like every other lane, so a
    /// keyed track on a rest sequence animates rather than latching at its first key.
    pub fn resting(anim: std::sync::Arc<benilla_formats::AlphaAnim>) -> Self {
        Self {
            unit_lane: true,
            ..Self::new(anim, 0.0, false)
        }
    }

    /// Whether this instance's tag alpha is the unit lane's to compose (see
    /// [`crate::entities::apply_unit_mat_alpha`]) — a hosted creature/player batch, or an
    /// attach model's batch ([`Self::resting`]), and never a self-driving effect part.
    pub fn composes_unit_tag(&self) -> bool {
        (self.host.is_some() || self.unit_lane) && !self.drives_tag
    }
}

/// The sequence slot + clip-local time a host is playing, for [`sample_mat_anim`]: the **base**
/// animation with the greatest blend weight. Masked overlays (a torso-only swing, an arm's draw
/// ceremony, the finger grip) run on their own graph nodes and are deliberately skipped — they
/// pose bones, they don't reselect the sequence the material tracks read. During a cross-fade two
/// base clips are live and the heavier one wins; the reference instead blends the two sampled
/// scalars by λ (wow-re `eval.md` FN 0x71af20's blend leg), a sub-blend-time difference on tracks
/// the corpus authors as 0/1 steps — recorded, not modelled.
///
/// A player with **nothing armed** is not "no sequence" — the reference arms the loader-idle clip on
/// every M2 instance at load, so the answer is that sequence at its opening frame
/// ([`ModelAnimations::idle_seq`], decision 0936). benilla's rig tier skips the arm whenever looping
/// the idle would render identically to the static mesh, and that skip used to leak out of the mesh
/// question into this one: both callers read the returned `None` as "keep the pinned slot", which
/// starts life at file slot 0. On a Spawn/Stand/Despawn GameObject slot 0 is the Spawn flourish, so
/// the batch read a sequence the instance was never playing.
pub fn playing_seq(player: &AnimationPlayer, anims: &ModelAnimations) -> Option<(usize, f32)> {
    player
        .playing_animations()
        .filter_map(|(node, active)| {
            let clip = anims.clips.iter().find(|c| c.node == *node)?;
            Some((clip.seq_index, active.seek_time(), active.weight()))
        })
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(seq, t, _)| (seq, t))
        .or_else(|| anims.idle_seq().map(|seq| (seq, 0.0)))
}

/// Sample every instance's material-alpha loops: a hosted instance on its host's playing sequence
/// and clip clock, a pinned one on its own spawn clock. Frozen (capture) instances keep their t=0
/// sample. Hidden instances sample too — the visibility authority's alpha cull reads
/// [`MatAnim::current`], so a batch BORN at weight 0 (`HolyLight_Low_Head`'s light column, keyed
/// `0 → 1` at 300 ms) must keep its clock running or the alpha-hide latches forever (the old
/// skip-while-Hidden held `current` at the 0 that caused the hide — the invisible pala-heal flash).
/// Sampling is a pure function of the clock, so an instance hidden for any *other* reason (draw
/// gate, far-clip) still lands right on re-appear, and the reference animation-evaluates the tracks
/// every frame regardless of the cull (wow-re `m2-alpha-combine-cull`). Runs before the visibility
/// authority so the tag it composes is this frame's value.
pub fn sample_mat_anim(
    time: Res<Time>,
    hosts: Query<(&AnimationPlayer, &ModelAnimations)>,
    mut q: Query<&mut MatAnim>,
) {
    let now = time.elapsed_secs();
    // The scene clock, full-precision (a long-uptime f32 elapsed drifts whole milliseconds,
    // which a 433 ms twinkle shows): the base the per-instance gseq attach anchor subtracts
    // from (0856).
    let shared = time.elapsed_secs_f64();
    for mut m in &mut q {
        if m.frozen {
            continue;
        }
        // A hosted instance whose host has no player yet (the frame before the rig arms, or a
        // rest-pose GameObject) keeps its last resolved slot and reads it at t=0 — the sequence's
        // opening pose, which is what a model sitting at bind pose shows.
        let played = m
            .host
            .and_then(|h| hosts.get(h).ok())
            .and_then(|(p, a)| playing_seq(p, a));
        let (seq, elapsed) = match played {
            Some((seq, t)) => {
                m.seq = Some(seq);
                (Some(seq), t)
            }
            None if m.host.is_some() => (m.seq, 0.0),
            None => (m.seq, now - m.spawned_at),
        };
        // The instance's gseq cursor: sceneNow − attach, the anchor stamped on this first pass
        // (decisions 0856/0858 — every lane, spell effects included: fresh instance per play).
        let attach = *m.gseq_attach.get_or_insert(shared);
        m.current = m.anim.sample(seq, elapsed, shared - attach);
    }
}

/// The **UV-animated materials** registry (decision 0130 phase 3, wow-re `m2-texanim-uv`): each
/// batch material carrying a texture-transform translation loop, keyed by material asset id.
/// [`tick_anim_materials`] re-samples a *drawn* entry's offset into the material's `sun_scale.zw`
/// each frame — one shared uniform per material, so every instance of a model batch scrolls in
/// phase. A recorded, invisible divergence for BOTH clock laws (0856): the reference phases a
/// seq-band loop per play (arm cursor) and a gseq loop per instance (attach anchor,
/// `gseq-anchor.md`), but one uniform per material cannot phase per instance — meaningless for a
/// seamless scroll either way. Entries drop when the material asset does.
/// The scan marker (1375): a part whose material can ever be a [`UvAnimMaterials`]/
/// [`TintAnimMaterials`] key — inserted at spawn, beside the registration itself, and only for a
/// loop with a real period (a period-0 constant is fully served by its material seed). Without
/// it, [`tick_anim_materials`]'s draw scan visited every `WowModelMaterial` row in the world
/// (~48k at Stormwind) to find the placed instances of ~113 animated models.
#[derive(bevy::prelude::Component)]
pub struct AnimMatPart;

#[derive(Resource, Default)]
pub struct UvAnimMaterials(
    pub  std::collections::HashMap<
        bevy::asset::AssetId<benilla_assets::materials::WowModelMaterial>,
        std::sync::Arc<benilla_formats::UvAnim>,
    >,
);

/// Re-sample the **drawn** animated materials on the shared clock — the UV scroll
/// ([`UvAnimMaterials`]) and the RGB tint ([`TintAnimMaterials`]) together, because they share the
/// draw scan below. Skipped entirely in captures (materials keep their t = 0 seed — constants still
/// show, frames stay deterministic).
///
/// **The draw gate, and why it is the fix for B131.** Mutating a material asset marks it Modified,
/// which on the Metal non-bindless path re-creates its uniform buffers *and its bind group* that
/// frame. 0130 sized this as "a few uniform writes per frame" on the premise that the resident
/// population is tiny (113 texanim models exist game-wide, a handful in view) — true per view,
/// **false per map session**: the dedup caches hold every material they ever built until a
/// `MapChange` (decision 0729), and a registry entry only evicts when its material dies, so on a
/// single-map traverse both registries grow monotonically and every entry is re-uploaded every
/// frame for ever. Measured on a parked same-map leg by square-waving this system:
/// **+9.85 ms of CPU per frame at 174 resident entries (~57 µs each)** — and residency 4 → 248
/// entries inside ten minutes, recoverable only by a restart or a map change. That is B131.
///
/// Gating on the draw is not a workaround for that growth, it is this lane finally obeying the
/// module's own law: [`gate_doodad_anim`] already gates the *pose* on "any submesh actually drawn",
/// on exactly the byte ground that makes it free — sampling is clock-indexed, so a material that
/// re-appears is written from `now` and shows the value the shared clock dictates, with nothing to
/// catch up (the module docs' "pausing costs nothing and drifts nothing"). A material nothing draws
/// is a per-frame GPU rebuild with no pixel to show for it.
///
/// `Visibility != Hidden` is the same spelling [`gate_doodad_anim`] uses — the authority's verdict
/// (far clip + size-bucketed fade + portal cull), written in `Update` by
/// `debug_panel::ModelVisSet`, which this system is ordered after so the verdict is *this* frame's.
/// It over-includes (a part left `Inherited` under a hidden ancestor counts as drawn), which is the
/// safe direction: an extra write costs a frame's uniform upload, a missed one would freeze a
/// visible scroll.
#[allow(clippy::too_many_arguments)]
pub(super) fn tick_anim_materials(
    time: Res<Time>,
    real: Res<Time<bevy::time::Real>>,
    mut uv_reg: ResMut<UvAnimMaterials>,
    mut tint_reg: ResMut<TintAnimMaterials>,
    mut materials: ResMut<Assets<benilla_assets::materials::WowModelMaterial>>,
    parts: Query<
        (
            &MeshMaterial3d<benilla_assets::materials::WowModelMaterial>,
            &Visibility,
        ),
        With<AnimMatPart>,
    >,
    twins: Res<crate::model_render::FarSideTwins>,
    mut drawn: Local<
        bevy::platform::collections::HashSet<AssetId<benilla_assets::materials::WowModelMaterial>>,
    >,
) {
    if (uv_reg.0.is_empty() && tint_reg.0.is_empty())
        || crate::dev_state::deterministic_run()
        || matanim_off(&real)
    {
        return;
    }
    drawn.clear();
    for (mat, vis) in &parts {
        if *vis != Visibility::Hidden {
            let id = mat.id();
            // A far-classified instance carries the far TWIN's id — never a registry key. Count
            // it as its near identity, or a batch whose every instance sits beyond the water
            // plane marks its near entry not-drawn and freezes both variants' scroll (1375; the
            // twin itself keeps getting written by `classify_water_side`'s Modified mirror).
            drawn.insert(twins.near_of(id).unwrap_or(id));
        }
    }
    let now = time.elapsed_secs();
    // `contains` on the skip path, never `get_mut`: the entry must survive while its material does
    // (that is the eviction predicate — an entry dies with its material, not with its visibility),
    // and an immutable probe is what keeps a skipped material *un*-Modified.
    uv_reg.0.retain(|id, anim| {
        if !drawn.contains(id) {
            return materials.contains(*id);
        }
        let uv = anim.sample(now);
        // The write gate (`write_gated`'s reasoning, 1375): a `get_mut` deref alone marks the
        // asset Modified — a uniform re-upload, a bind-group rebuild on the non-bindless Metal
        // path, and the whole-population `AssetChanged` walks — so an unchanged value must be
        // decided on the immutable view and never deref. Quantized because the input drifts
        // continuously: 1/4096 of a texture repeat is below what any face can show.
        let (zq, wq) = (
            benilla_assets::quantize(uv[0], 4096.0),
            benilla_assets::quantize(uv[1], 4096.0),
        );
        match materials.get(*id) {
            None => return false, // material unloaded with its cache — drop the entry
            Some(m) if m.extension.sun_scale.z == zq && m.extension.sun_scale.w == wq => {
                return true;
            }
            Some(_) => {}
        }
        let Some(mat) = materials.get_mut(*id) else {
            return false;
        };
        mat.extension.sun_scale.z = zq;
        mat.extension.sun_scale.w = wq;
        true
    });
    tint_reg.0.retain(|id, anim| {
        if !drawn.contains(id) {
            return materials.contains(*id);
        }
        let rgb = benilla_assets::quant255(anim.sample(now));
        let tint = bevy::math::Vec4::new(rgb[0], rgb[1], rgb[2], 1.0);
        match materials.get(*id) {
            None => return false,
            Some(m) if m.extension.tint == tint => return true,
            Some(_) => {}
        }
        let Some(mat) = materials.get_mut(*id) else {
            return false;
        };
        mat.extension.tint = tint;
        true
    });
}

/// **The price-this-system knob** (`WOW_MATANIM_DUTY=<start_s>:<period_s>`, decision 0785): alternate
/// [`tick_anim_materials`] off/on every `period` seconds from `start`. Park at one pin, square-wave
/// it, and the difference between the ON and OFF buckets **is** this system's per-frame cost, with
/// residency, scene, entity count and camera all held identical — a measurement, not an argument.
/// It is how B131's ratchet was priced (+9.85 ms/frame over 174 resident entries) and how the draw
/// gate above was then shown to remove it (+0.22 ms). Kept, not deleted: the ~10 ms-floor ledger
/// (0729's residuals) has more per-frame systems queued for exactly this treatment.
///
/// A **square wave** rather than one flip, because this machine's frame cost drifts on its own —
/// a single before/after pair cannot tell the drift from the signal, and both legs of the verifying
/// run climbed ~10 ms while their ON/OFF difference stayed flat.
///
/// **`Time<Real>`, deliberately:** virtual time is clamped to `max_delta` (250 ms), so on a leg that
/// hitches it lags real time badly. That is what smeared the first attempt at this measurement into
/// nonsense — the probe-chat schedule reads virtual time, so once the leg hitched its hops drifted
/// 40 s → 75 s apart and windows labelled "parked, ticks off" in fact held a teleport and live ticks.
fn matanim_off(time: &Time<bevy::time::Real>) -> bool {
    static SPEC: std::sync::OnceLock<Option<(f32, f32)>> = std::sync::OnceLock::new();
    let spec = SPEC.get_or_init(|| {
        let v = std::env::var("WOW_MATANIM_DUTY").ok()?;
        let (start, period) = v.split_once(':')?;
        Some((start.trim().parse().ok()?, period.trim().parse().ok()?))
    });
    spec.is_some_and(|(start, period)| {
        let t = time.elapsed_secs() - start;
        t >= 0.0 && period > 0.0 && ((t / period) as u32) % 2 == 1
    })
}

/// The **tint-animated materials** registry — the M2Color-RGB twin of [`UvAnimMaterials`]: each
/// batch material whose colour track animates (the vertex bake is skipped for those —
/// `benilla-formats` `m2_batches`), keyed by material asset id. [`tick_anim_materials`]
/// re-samples the tint into the material's `tint` uniform on the same shared clock, for the drawn
/// (the same recorded seq-band phase divergence as the UV scroll — invisible for a placed
/// doodad's ambient loop). Spell-effect instances need real per-instance phase instead (one cast
/// = one 0.9 s pulse), so the effect lane clones its materials and ticks them on the instance
/// clock (`entities::spell_fx`), never through this registry.
#[derive(Resource, Default)]
pub struct TintAnimMaterials(
    pub  std::collections::HashMap<
        bevy::asset::AssetId<benilla_assets::materials::WowModelMaterial>,
        std::sync::Arc<benilla_formats::RgbAnim>,
    >,
);
