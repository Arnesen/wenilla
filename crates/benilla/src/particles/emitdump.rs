//! `$WOW_EMIT_DUMP` — what the emission front end **decided**, per emitter, for one model.
//!
//! The particle lane already has an asset-side dump (`benilla-extract m2part` — every authored
//! field of every record) and a draw-side one ([`super::depthdump`] — the depth numbers a pool
//! brings to the compare). The gap between them is where "this effect looks wrong" reports
//! actually live: *which* sequence slot the emitter resolved, what the ten per-frame tracks
//! sampled at that slot, and how many particles that produced. A model whose rate track keys
//! `0` in one sequence and `526/s` in another looks identical in the asset dump and in the draw
//! dump — only the resolved slot says which of the two is on screen.
//!
//! `WOW_EMIT_DUMP=<label-substring>[,<period-seconds>]` — every `period` seconds (default 2),
//! print one line per live emitter whose owning model's [`crate::interact::WorldObject`] label
//! contains the substring, case-insensitively. That label is the model path for doodads, WMO
//! props and GameObjects and the unit name for creatures — the same string the hover inspector
//! shows, so the filter is copied straight off the panel that prompted the question.
//!
//! Scoping by label is what keeps it usable in a populated scene: a Molten Core frame ticks
//! hundreds of emitters, and an unfiltered per-emitter dump both floods the log and costs the
//! framerate of the run it is measuring.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use benilla_formats::{ParamsNow, ParticleEmitterDef};

/// Parsed `$WOW_EMIT_DUMP`: `(lowercased label substring, period seconds)`. `None` = off, and
/// every entry point below then does nothing at all.
static FILTER: std::sync::LazyLock<Option<(String, f32)>> = std::sync::LazyLock::new(|| {
    let v = std::env::var("WOW_EMIT_DUMP").ok()?;
    // Only a trailing NUMBER is a period — a model path may itself contain a comma.
    let (label, period) = match v
        .rsplit_once(',')
        .map(|(l, p)| (l, p.trim().parse::<f32>()))
    {
        Some((l, Ok(p))) => (l, p),
        _ => (v.as_str(), 2.0),
    };
    Some((label.trim().to_ascii_lowercase(), period.max(0.1)))
});

/// The instrument's own system plumbing: the label lookup it filters by and its period clock.
/// One [`SystemParam`] rather than two loose arguments — `simulate_particles` sits at Bevy's
/// 16-parameter ceiling, and a debug affordance has no business spending two of them.
#[derive(SystemParam)]
pub(super) struct EmitDump<'w, 's> {
    /// The owning model's inspector identity. It rides the **drawn parts**, not the entity the
    /// emitter hosts on (the picker hovers a mesh, so that is where `attach::dress` puts it) —
    /// hence the child walk in [`EmitDump::label`]. Looking only at the owner is what made this
    /// instrument's first outing print 10822 unlabelled lines.
    labels: Query<'w, 's, &'static crate::interact::WorldObject>,
    parts: Query<'w, 's, &'static Children>,
    last: Local<'s, f32>,
}

impl EmitDump<'_, '_> {
    /// Is this frame a dump tick? Advances the period clock when it is. Always `false` without
    /// the env, so the per-emitter call site costs one atomic load per frame.
    pub(super) fn due(&mut self, now: f32) -> bool {
        let Some((_, period)) = FILTER.as_ref() else {
            return false;
        };
        if now - *self.last < *period {
            return false;
        }
        *self.last = now;
        true
    }

    /// This emitter's model identity: the owner's own [`crate::interact::WorldObject`] if it has
    /// one, else the first drawn part under it that does — a GameObject/unit carries its label on
    /// the submesh entities, and the emitter hosts on the root above them.
    fn label(&self, owner: Option<Entity>) -> &str {
        let Some(e) = owner else { return "" };
        if let Ok(o) = self.labels.get(e) {
            return o.label.as_str();
        }
        self.parts
            .get(e)
            .ok()
            .and_then(|kids| kids.iter().find_map(|k| self.labels.get(k).ok()))
            .map_or("", |o| o.label.as_str())
    }

    /// Print one emitter's decision line, if its owning model matches the filter.
    pub(super) fn dump(&self, owner: Option<Entity>, d: &Decision<'_>) {
        let Some((want, _)) = FILTER.as_ref() else {
            return;
        };
        let label = self.label(owner);
        if !label.to_ascii_lowercase().contains(want.as_str()) {
            return;
        }
        let n = d.now;
        println!(
            "emit {label} bone {bone:<3} seq {seq} t={t:.3}s · rate {rate:.2}/s {gate} · live \
             {live:<4} · life {life:.2} speed {speed:.3}±{var:.2} lat {lat:.3} lon {lon:.3} \
             grav {grav:.2} area {al:.3}x{aw:.3} · size {s0:.3}/{s1:.3}/{s2:.3} · at \
             [{x:.2},{y:.2},{z:.2}]",
            bone = d.def.bone,
            seq = d.seq.map_or("-".to_string(), |s| s.to_string()),
            t = d.elapsed,
            rate = d.rate,
            gate = if d.emitting { "ON " } else { "off" },
            live = d.live,
            life = n.lifespan,
            speed = n.emission_speed,
            var = n.speed_variation,
            lat = n.vertical_range,
            lon = n.horizontal_range,
            grav = n.gravity,
            al = n.area_length,
            aw = n.area_width,
            s0 = d.def.over_life.scale[0],
            s1 = d.def.over_life.scale[1],
            s2 = d.def.over_life.scale[2],
            x = d.at.x,
            y = d.at.y,
            z = d.at.z,
        );
    }
}

/// One emitter's decision this frame — the sim's live values, not the authored ones.
pub(super) struct Decision<'a> {
    pub(super) def: &'a ParticleEmitterDef,
    /// The sequence FILE slot the rate/gate/params tracks resolved to (`None` = degraded to 0).
    pub(super) seq: Option<usize>,
    /// Seconds into that slot's baked loop.
    pub(super) elapsed: f32,
    pub(super) rate: f32,
    pub(super) emitting: bool,
    pub(super) live: usize,
    pub(super) now: &'a ParamsNow,
    /// The emitter origin's live world position — the number that says whether a model's
    /// emitters are spread across it or collapsed onto one point.
    pub(super) at: Vec3,
}
