//! The LIVE-run probe instruments — every plugin here rides a NORMAL connected session (unlike
//! the parent's server-less [`super::CapturePlugin`] harness): scripted chat sends
//! ([`ProbeChatPlugin`]), synthetic key taps ([`ProbeKeyPlugin`]), a Lua chunk in the live UI VM
//! ([`ProbeLuaPlugin`]), the bounded-lifetime self-exit ([`ProbeExitPlugin`]), and the live
//! frame-time sampler ([`LiveFpsPlugin`]). The live screenshot and its validity gates live in the
//! sibling [`super::live_shot`]. Each is env-gated and registered by `main`; compose them for
//! unattended "park, act, observe" probes.

use bevy::prelude::*;

/// The **actuation** probes — the three channels that make a live session do something
/// headlessly: chat/slash lines, synthetic key presses, and a Lua chunk in the UI VM.
mod fx_draw_census;
pub(crate) use fx_draw_census::plugin as fx_draw_census_plugin;
mod act;
pub(crate) use act::{ProbeChatPlugin, ProbeKeyPlugin, ProbeLuaPlugin};

/// The probe **run** itself — its bounded lifetime, and the window it lives in (level,
/// parking, size). Nothing here measures anything; it is the shell every other probe rides.
mod run;
pub(crate) use run::{ProbeExitPlugin, ProbeFocusPlugin, ProbeResizePlugin};

/// The live frame-time sample — `FPS_PROBE` and the companion lines emitted from the same
/// sample window (`VIS_CENSUS`/`VIS_ESCAPED`/`VIS_DUMP`, `MAT_CHURN`, `ASSET_DUMP`).
mod live_fps;
pub(crate) use live_fps::LiveFpsPlugin;

/// The particle census — per-emitter state plus the draw-distance accounting behind B39.
mod particle_census;
pub(crate) use particle_census::ParticleCensusPlugin;

/// The two exclusive-`World` reflection dumps: the bevy_ui node inventory and the archetype
/// census — "what is resident right now, and what is it made of".
mod world_census;
pub(crate) use world_census::{EntityCensusPlugin, NodeProbePlugin};

/// The clock **every probe schedule reads** — real time, never the virtual clock (decision 0789).
///
/// A probe knob is a wall-clock instruction: "send this at 20 s", "sample 300 frames from 25 s",
/// "resize at 12 s", "exit at 480 s". `Time<Virtual>` cannot honour one — it clamps every frame delta
/// to `max_delta` (250 ms, `bevy_time`'s default), so on any leg that hitches (a streaming burst, an
/// occluded window, a loaded machine) it falls behind real time and drags the whole schedule with it.
/// The knob then means something the operator never asked for, *silently*.
///
/// **Third time this clock has cost us a measurement.** 0615 moved the relayed-move replay off it
/// (and cites the UI script clock as the same lesson before that); then B131's first causal leg was
/// destroyed by it here — the probe-chat hops drifted 40 s → 75 s apart, so windows labelled
/// "parked, ticks off" in fact contained a teleport and live ticks, and an eight-minute leg had to be
/// thrown away (decision 0785's discarded run). A named alias is what makes the next probe get it
/// right without knowing the story: type this, and the mistake is unavailable.
///
/// The one deliberate virtual clock in the harness is the fixture **age** in
/// [`super::drive_capture`] — an age on the clock the effect animates on (and which the capture
/// freezes at save time), which is not a schedule at all.
pub(crate) type ProbeClock<'w> = Res<'w, Time<bevy::time::Real>>;

#[cfg(test)]
mod tests {
    /// **The invariant, checked instead of remembered** (decision 0789).
    ///
    /// Naming the right clock [`ProbeClock`] makes it easy to reach for; it does not make the wrong
    /// one unavailable, and `Res<Time>` is the shorter, prelude-blessed, obvious spelling. That
    /// asymmetry is precisely why this same clock has now cost three lanes a correctness bug
    /// (0615's replay clock, the UI script clock, and B131's discarded probe leg) — each time it was
    /// fixed where it was found and left available everywhere else. So the fix is not another
    /// convention: it is this test, in the suite the gates already run.
    ///
    /// Adding a virtual clock to the probe harness now means adding yourself to [`ALLOWED`] with a
    /// reason, which is a much better conversation than discovering the drift in a thrown-away leg.
    #[test]
    fn probe_schedules_read_the_wall_clock() {
        /// `(file, system, why it is genuinely an age or a delta on the animating clock)`.
        const ALLOWED: &[(&str, &str, &str)] = &[
            (
                "capture/mod.rs",
                "drive_capture",
                "the fixture AGE runs on the clock the effect animates on, and the capture freezes \
                 that same clock at save time — an age, not a schedule",
            ),
            (
                "capture/waterfx.rs",
                "spawn / drive",
                "the foam fixture is a SIMULATION rig, not a schedule: it walks a synthetic dummy \
                 through the shipped emitter path, and the emitter reads its velocity on the same \
                 animating clock. A wall clock here would desync the dummy from the thing it is \
                 feeding — the one case where matching the virtual clock is the correctness \
                 requirement rather than the bug",
            ),
        ];

        // The needles are assembled at runtime so the checker does not flag **its own source** —
        // which is exactly what it did on its first run, and is the cheapest possible proof that it
        // has teeth.
        let bare = format!(": Res<{}>,", "Time");
        let bare_last = format!(": Res<{}>", "Time");
        let explicit = format!("Res<{}<{}>>", "Time", "Virtual");

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut allowed_seen = 0usize;
        // The probe harness: everything under `capture/`, plus the one probe that lives beside the
        // player controller because it writes `face_yaw` directly.
        let mut stack = vec![src.join("capture")];
        let mut files = vec![src.join("player/probe_look.rs")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("probe harness dir is readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        for path in files {
            let rel = path
                .strip_prefix(&src)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&path).expect("source is readable");
            for (n, line) in text.lines().enumerate() {
                let t = line.trim();
                // A system param binding the virtual clock. `Time<Real>` is the whole point, and
                // `ResMut<Time<Virtual>>` is the capture's own clock *control*, not a read of it.
                let virtual_clock =
                    (t.ends_with(&bare) || t.ends_with(&bare_last)) || t.contains(&explicit);
                if !virtual_clock {
                    continue;
                }
                match ALLOWED.iter().find(|(f, _, _)| *f == rel) {
                    Some(_) => allowed_seen += 1,
                    None => offenders.push(format!("{rel}:{}  {t}", n + 1)),
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the probe harness must schedule on the wall clock (`ProbeClock`), not the virtual \
             clock — it is clamped to max_delta (250 ms), so any hitching leg silently drifts every \
             `<secs>` knob out from under the operator (decision 0789). Offenders:\n  {}",
            offenders.join("\n  "),
        );
        assert!(
            allowed_seen > 0,
            "the ALLOWED exception list is stale — nothing matched it. If the fixture-age clock \
             moved or went away, drop its entry rather than leaving a rule guarding nothing.",
        );
    }
}
