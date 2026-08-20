//! **The dev/player seam** — the two instrument groups, and the one rule that governs them
//! (decision 0026 set the target, 1173 mandated the build, 1174 is what got built).
//!
//! ```text
//! Dev may see anything; nothing may depend on dev.
//! ```
//!
//! That is the whole boundary, and it is one-directional on purpose. An instrument's job *is* to
//! see everything — 1160 measured 266 references from the instruments into 63 of the app's 142
//! modules — so a crate wall here would mean publishing hundreds of internals as permanent public
//! API shaped by whatever a panel wanted to poke at. A `cfg` seam has the opposite property: the
//! instruments keep reaching in exactly as far as they like, and only the *direction* is
//! constrained.
//!
//! **The mechanism is not this comment.** It is `cargo build -p benilla --no-default-features` in
//! `scripts/gates.sh`. 0026 wrote this same rule down in June 2026 with no failing build behind
//! it, and by August there were 24 references from non-dev code into the instruments across 12
//! files (1173) — not through carelessness, but because a rule with no failing test is a wish.
//! When gameplay needs a fact an instrument happens to know, the fact goes to
//! [`crate::run_mode`] (always present, player-faithful default) and the instrument writes it
//! there. It does not get a `use` line from gameplay.
//!
//! **What is deliberately NOT in here:** `benilla_world::dev_state` (the always-present config
//! layer — its defaults *are* the player behaviour, so it ships), `pipe_warm` (a player on macOS
//! eats every synchronous pipeline stall without it — 0837/1116), and `art_scope` (within-map art
//! residency: engine, and it travels with `WorldPlugins` — 0729).

use bevy::prelude::*;

/// `WOW_CAPTURE=list` — the harness scenario names `scripts/visual.sh` reads, printed before any
/// window or asset setup. Answers nothing in a player build, which has no scenarios.
pub(crate) fn print_scenario_names() {
    #[cfg(feature = "dev")]
    crate::capture::print_scenario_names();
}

/// `WOW_HOVER_LOG_REPORT=<csv>` — re-read a recorded run and print its report, no window, no game.
/// New analysis lands on runs already captured (see `hover_log`).
pub(crate) fn report_recorded_hover_log(_path: &str) {
    #[cfg(feature = "dev")]
    crate::hover_log::report_recorded_file(_path);
}

/// **The instruments.** One group, added where the debug panel has always sat — first among them,
/// because `PerfPlugin` needs the egui plugin and context it sets up.
pub(crate) struct DevToolsPlugin;

impl Plugin for DevToolsPlugin {
    #[cfg_attr(not(feature = "dev"), allow(unused_variables))]
    fn build(&self, app: &mut App) {
        #[cfg(feature = "dev")]
        {
            app.add_plugins(crate::debug_panel::DebugPanelPlugin)
                .add_plugins(crate::perf::PerfPlugin)
                // `WOW_FX_CENSUS=1`: where this frame's particle draws are addressed, and whether
                // the view they name is switched on (decision 0775). An instrument, and one that
                // reads the portrait booths — so it belongs on this side of the line.
                .add_plugins(crate::capture::fx_draw_census_plugin)
                // The hover-cost recorder (`WOW_HOVER_LOG`) and the asset-churn meter
                // (`WOW_ASSET_CHURN`) — both no-ops without their variable.
                .add_plugins(crate::hover_log::HoverLogPlugin)
                .add_plugins(crate::asset_churn::AssetChurnPlugin)
                // The session preflight (decision 0649): one banner per world entry naming the body
                // we logged into, and loud warnings for the states — dead/ghost, GM mode,
                // server-blocked movement — that silently invalidate a session's readings. Never
                // env-gated; a warning nobody switches on isn't one.
                .add_plugins(crate::preflight::PreflightPlugin)
                // The probe shield (decision 0677): a body on a probe account is put into vmangos's
                // `.cheat god` on every world entry — damage clamps at 1 hp instead of killing —
                // and GM mode is turned OFF, because the shield replaces the only reason it was
                // ever on. Inert on any other account.
                .add_plugins(crate::probe_shield::ProbeShieldPlugin);
        }
    }
}

/// **The probe fleet** — the capture harness and every scripted live probe, each armed by its own
/// environment variable and inert without it. Added last so they observe the fully-built app.
pub(crate) struct DevProbesPlugin;

impl Plugin for DevProbesPlugin {
    #[cfg_attr(not(feature = "dev"), allow(unused_variables))]
    fn build(&self, app: &mut App) {
        #[cfg(feature = "dev")]
        {
            // The capture harness drives one deterministic screenshot then exits — added last so it observes
            // the fully-built app. Inert unless `$WOW_CAPTURE` is set.
            if crate::run_mode::scenario_active() {
                app.add_plugins(crate::capture::CapturePlugin);
            }
            // The LIVE probe shot (orthogonal to the harness): `WOW_LIVE_SHOT=<png>` on a NORMAL connected
            // run writes one screenshot `WOW_LIVE_SHOT_AT` seconds (default 12) after startup and keeps
            // running — the agent-side instrument for seeing a live server scene (NPCs, GameObjects, event
            // spawns) without a scenario. Pair with `WOW_USER`/`WOW_CHAR` + an outer `timeout`.
            if std::env::var("WOW_LIVE_SHOT").is_ok() {
                app.add_plugins(crate::capture::LiveShotPlugin);
            }
            // The probe RIG (decision 0651): `WOW_RIG="tauren druid 60 gear:heal-preraid-bis"` finds-or-
            // creates that body on this slot's probe account, logs in as it, and applies level/spells/gear/
            // spec/place — the one verb that replaces the hand-assembled GM recipe every session used to
            // re-derive (see `capture::ProbeRigPlugin`).
            if std::env::var("WOW_RIG").is_ok() {
                app.add_plugins(crate::capture::ProbeRigPlugin);
            }
            // Any scripted probe keeps its window un-occludable: a fully covered macOS window drops to
            // ~1 fps drawables, and every probe schedule is wall-clock — a throttled run doesn't measure
            // slowly, it runs the wrong script (see `capture::ProbeFocusPlugin`, decision 0906).
            // (`WOW_LIVE_FPS` is in the list because an occluded SETTLE phase streams the world at ~1 fps
            // and under-warms the scene before sampling even starts — the assertion has to be live from
            // the first tick, not at the uncap.)
            if [
                "WOW_PROBE",
                "WOW_PROBE_CHAT",
                "WOW_PROBE_KEY",
                "WOW_PROBE_LUA",
                "WOW_RIG",
                "WOW_LIVE_FPS",
            ]
            .iter()
            .any(|k| std::env::var(k).is_ok())
            {
                app.add_plugins(crate::capture::ProbeFocusPlugin);
            }
            // The probe-chat one-shot: `WOW_PROBE_CHAT=".go xyz …"` sends GM/chat lines once in-world —
            // the "park the probe character anywhere" instrument (see `capture::ProbeChatPlugin`).
            if std::env::var("WOW_PROBE_CHAT").is_ok() {
                app.add_plugins(crate::capture::ProbeChatPlugin);
            }
            // The probe-lua one-shot: `WOW_PROBE_LUA="CastSpell(…)"` runs a chunk in the live UI VM once
            // in-world — the "press the button headlessly" instrument (see `capture::ProbeLuaPlugin`).
            if std::env::var("WOW_PROBE_LUA").is_ok() {
                app.add_plugins(crate::capture::ProbeLuaPlugin);
            }
            // The probe-key taps: `WOW_PROBE_KEY="Space@14"` presses keys once in-world — the "press
            // space headlessly" instrument for input-gated behavior (see `capture::ProbeKeyPlugin`).
            if std::env::var("WOW_PROBE_KEY").is_ok() {
                app.add_plugins(crate::capture::ProbeKeyPlugin);
            }
            // The probe self-termination: `WOW_PROBE_EXIT_AT=<secs>` bounds any scripted live probe's
            // lifetime — its own knob, not a rider on the Lua probe (see `capture::ProbeExitPlugin`).
            if std::env::var("WOW_PROBE_EXIT_AT").is_ok() {
                app.add_plugins(crate::capture::ProbeExitPlugin);
            }
            // The ray pick: `WOW_PICK="<x>,<y>"` names every surface along the ray through a screenshot
            // pixel, nearest first — "what is at the spot `benilla-visual hotspot` flagged, and what is
            // right behind it" (see `capture::PickProbePlugin`).
            if std::env::var("WOW_PICK").is_ok() {
                app.add_plugins(crate::capture::PickProbePlugin);
            }
            // The render-phase census: `WOW_PHASE=<uniqueId>` reports, per frame, which phase each of one
            // placement's batches landed in and where in the draw order — the one thing every scene-side
            // instrument is blind to, namely whether a surface was submitted at all (see
            // `capture::PhaseProbePlugin`).
            if std::env::var("WOW_PHASE").is_ok() {
                app.add_plugins(crate::capture::PhaseProbePlugin);
            }
            // The depth readback: `WOW_DEPTH="<x>,<y>"` reports what depth actually won each named pixel,
            // per frame, as a distance in yards — the link past submission that decides the pixel. Pair it
            // with `WOW_PICK` at the same pixels to turn "what won" into "whose it was" (see
            // `capture::DepthProbePlugin`).
            // `WOW_DEPTH_QUADS=<bone>…` is the same readback taken at a particle quad's OWN pixels — the
            // moving-subject form, which no hand-written pixel list can hold (see `capture::depth_probe`).
            if std::env::var("WOW_DEPTH").is_ok() || std::env::var("WOW_DEPTH_QUADS").is_ok() {
                app.add_plugins(crate::capture::DepthProbePlugin);
            }
            // The bevy_ui node census — "who owns this rectangle" for UI outside the FrameXML quad pass
            // (see `capture::NodeProbePlugin`).
            if std::env::var("WOW_NODE_PROBE").is_ok() {
                app.add_plugins(crate::capture::NodeProbePlugin);
            }
            // The mid-run window resize: `WOW_PROBE_RESIZE="<secs>:<W>x<H>"` — the headless fullscreen-
            // toggle stand-in for resize-reactive layout (see `capture::ProbeResizePlugin`).
            if std::env::var("WOW_PROBE_RESIZE").is_ok() {
                app.add_plugins(crate::capture::ProbeResizePlugin);
            }
            // The particle census: `WOW_PARTICLE_CENSUS=<secs>` prints per-emitter live counts once —
            // the trace-comparable coverage number (see `capture::ParticleCensusPlugin`).
            if std::env::var("WOW_PARTICLE_CENSUS").is_ok() {
                app.add_plugins(crate::capture::ParticleCensusPlugin);
            }
            // The under-floor census: `WOW_GROUND_CENSUS=<secs>[,<every>]` prints one line per
            // streamed unit near the body — the server's Z for it, the Z we drew it at, the drop
            // between them, and the floor over its head. The instrument B197 was missing: it says
            // whether a unit below a floor was put there by the server or pulled there by us (see
            // `capture::GroundCensusPlugin`).
            if std::env::var("WOW_GROUND_CENSUS").is_ok() {
                app.add_plugins(crate::capture::GroundCensusPlugin);
            }
            // The unit-visual census: `WOW_UNIT_VISUALS=<secs>[,<every>]` prints one line per
            // streamed entity near the body — whether it got a debug cube, real geometry, or
            // nothing at all. The instrument B13 was missing: a black slab in a screenshot cannot
            // say whether the display named no model (our gap) or named one that draws nothing
            // (an invisible trigger creature — see `capture::UnitVisualsPlugin`, decision 1403).
            if std::env::var("WOW_UNIT_VISUALS").is_ok() {
                app.add_plugins(crate::capture::UnitVisualsPlugin);
            }
            // The entity census: `WOW_ENTITY_CENSUS=<secs>` prints per-archetype entity counts once —
            // what the resident entity count is made of (see `capture::EntityCensusPlugin`).
            if std::env::var("WOW_ENTITY_CENSUS").is_ok() {
                app.add_plugins(crate::capture::EntityCensusPlugin);
            }
            // The schedule census: `WOW_SCHED_CENSUS=1` prints every schedule's systems with their
            // executor-relevant flags, both worlds, then exits — the structural inventory under
            // the orchestration bands (see `capture::SchedCensusPlugin`, decision 1437).
            if std::env::var("WOW_SCHED_CENSUS").is_ok() {
                app.add_plugins(crate::capture::SchedCensusPlugin);
            }
            // The melee live probe: `WOW_PROBE=melee` auto-fights the nearest enemy so the dbg-trace
            // sink can record the combat-text timeline (see `capture::ProbeMeleePlugin`).
            if std::env::var("WOW_PROBE").as_deref() == Ok("melee") {
                app.add_plugins(crate::capture::ProbeMeleePlugin);
            }
            // The partner live probe: `WOW_PROBE=partner` auto-accepts group invites — the party arc's
            // second-client instrument (decision 0434; see `capture::ProbePartnerPlugin`).
            if std::env::var("WOW_PROBE").as_deref() == Ok("partner") {
                app.add_plugins(crate::capture::ProbePartnerPlugin);
            }
            // The sea-crossing live probe: `WOW_PROBE=crossing` boards a cross-continent boat and reports
            // the map seam surviving — decision 0455's instrument (see `capture::ProbeCrossingPlugin`).
            if std::env::var("WOW_PROBE").as_deref() == Ok("crossing") {
                app.add_plugins(crate::capture::ProbeCrossingPlugin);
            }
            // The taxi-flight live probe: `WOW_PROBE=taxi` opens the flight-master menu on the real wire
            // and rides Stormwind → Sentinel Hill to a measured verdict — decision 0484's end-to-end
            // instrument (see `capture::ProbeTaxiPlugin`).
            if std::env::var("WOW_PROBE").as_deref() == Ok("taxi") {
                app.add_plugins(crate::capture::ProbeTaxiPlugin);
            }
            // The mail-arc live probe: `WOW_PROBE_MAIL=1` GM-mails the probe's own character, opens the
            // Goldshire mailbox on the real wire, and drives the inbox/take/send/delete surface through
            // the live Lua VM — decisions 0544/0548's end-to-end instrument (see `capture::ProbeMailPlugin`).
            if std::env::var("WOW_PROBE_MAIL").is_ok() {
                app.add_plugins(crate::capture::ProbeMailPlugin);
            }
            // The bank-arc live probe: `WOW_PROBE_BANK=1` GM-hops to a pure banker, drives the whole
            // six-opcode bank wire (activate/deposit/withdraw/buy-slot/refusal) — decision 0604's
            // end-to-end instrument (see `capture::ProbeBankPlugin`).
            if std::env::var("WOW_PROBE_BANK").is_ok() {
                app.add_plugins(crate::capture::ProbeBankPlugin);
            }
            // The innkeeper-bind live probe: `WOW_PROBE_BINDER=1` GM-hops to Innkeeper Keldamyr, asserts
            // the bind row's icon reads "binder", selects it, and answers the server's confirm through the
            // live VM's own `ConfirmBinder()` — decision 1331's end-to-end instrument, the evidence that
            // closes B249 (see `capture::ProbeBinderPlugin`).
            if std::env::var("WOW_PROBE_BINDER").is_ok() {
                app.add_plugins(crate::capture::ProbeBinderPlugin);
            }
            // The world-book live probe: `WOW_PROBE_BOOK=1` teleports to the Old Town plaque and
            // measures what having the item-text reader open costs per frame, closed vs open —
            // B240's instrument (see `capture::ProbeBookPlugin`).
            if std::env::var("WOW_PROBE_BOOK").is_ok() {
                app.add_plugins(crate::capture::ProbeBookPlugin);
            }
            // The cast-cancel live probe: `WOW_PROBE=castcancel` hearths and presses W mid-cast — the
            // local self-cancel's end-to-end timing instrument (see `capture::ProbeCastCancelPlugin`).
            if std::env::var("WOW_PROBE").as_deref() == Ok("castcancel") {
                app.add_plugins(crate::capture::ProbeCastCancelPlugin);
            }
            // The char-create live probe: `WOW_PROBE_CHARCREATE="<name>[,race,class,gender,…]"` creates (and
            // cleans up) a character at select to verify the char-create/delete wire (decision 0423 phase 1;
            // see `capture::ProbeCharCreatePlugin`).
            if std::env::var("WOW_PROBE_CHARCREATE").is_ok() {
                app.add_plugins(crate::capture::ProbeCharCreatePlugin);
            }
            // The live FPS probe: `WOW_LIVE_FPS=<frames>` samples frame times on a NORMAL connected run
            // and exits — the harness probe's numbers with the live world in (see `capture::LiveFpsPlugin`).
            if std::env::var("WOW_LIVE_FPS").is_ok() {
                app.add_plugins(crate::capture::LiveFpsPlugin);
            }
            // The two scripted probe drivers that lived in `player/` until decision 1174 — the mouse-turn
            // (`WOW_PROBE_LOOK`, decision 0621) and the camera park (`WOW_PROBE_CAM`, decision 0653). Added
            // unconditionally because each plugin's own `from_env` is its gate, so the variable's name is
            // spelled in exactly one place; both order themselves before `player::PlayerControlSet`.
            app.add_plugins(crate::capture::ProbeLookPlugin);
            app.add_plugins(crate::capture::ProbeCamPlugin);
            // The FPS journal: `WOW_FPS_JOURNAL=<csv>` appends per-second position + frame-time rows on a
            // director-driven run — "where does it dip" as coordinates (see `perf::FpsJournalPlugin`).
            if std::env::var("WOW_FPS_JOURNAL").is_ok() {
                app.add_plugins(crate::perf::FpsJournalPlugin);
            }
        }
    }
}
