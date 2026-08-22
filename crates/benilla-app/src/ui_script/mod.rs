//! The UI-engine bridge: hosts [`benilla_ui::script::UiScript`] (the Lua VM + widget arena +
//! layout, all engine-free) and feeds its [`extract`](benilla_ui::script::UiScript::extract) output
//! into the quad pass ([`crate::ui_pass::UiQuads`]) every frame. This is the seam decision 0068
//! draws between the engine-free crate and the app: everything above it is data + Lua; everything
//! below it is Bevy.
//!
//! Coordinates: the script/layout side is WoW UI space — **y-up**, origin bottom-left, in the
//! client's 768-virtual units (decision 0582): `set_screen_size` feeds a screen `768/uiScale`
//! units tall every frame, and [`seam_scale`] carries quads ×s out / mouse ÷s in. The quad pass
//! wants y-down window px, so extraction also flips through the window height.
//!
//! Benilla's own **unit frames** (`assets/ui/UnitFrames.xml` — a template + the player and target
//! instances) load at startup by default through [`benilla_ui::loader`] — the decision 0068 slice-1
//! game-shell: real FrameXML+Lua rendering unit health/power/level/name through the whole chain
//! (snapshot → `Unit*` bindings → Lua → event → StatusBars+text → quad pass). Since captures run
//! server-less (no real game state), `WOW_CAPTURE_UI=1` also feeds synthetic `"player"`/`"target"`
//! snapshots ([`demo_unit_feed`]) so the frames populate on screen. `FontString` regions draw
//! through [`crate::ui_text::layout_text_quads`] against the glyph atlas
//! [`crate::ui_text::UiFontAtlas`] builds at startup (decision 0068 §2).

use bevy::prelude::*;

use benilla_ui::script::{ActionSlot, ScriptValue, UiScript, UnitState};

use crate::ui_unit::UnitFeed;
use benilla_world::schedule::WorldStage;

/// The addon folder: discovery, manifests, the enable file, and the load walk. `pub(crate)`
/// because the AddOns screens read the same folder without a VM (decision 1196).
pub(crate) mod addons;
mod content;
mod extract;
mod input;
mod manifest;

/// The reference FrameXML this client EXECUTES off the player's own patch chain instead of
/// transcribing it — the rule, the list, and the licensing reason are that module's header.
mod reference_ui;

/// What the host remembers about the VM, and how it forgets — the one mechanism that stops a seed
/// or a change-memo outliving the VM it was written against (decision 1290).
mod session;

/// The feed gate (decision 1439): the input-side early-out for a per-frame UI feed, its
/// [`gate::Watch`] counter memory, and the `WOW_FEED_GATE_CHECK=1` audit that catches a gate
/// missing an input.
pub(crate) mod gate;

pub(crate) use session::VmMemo;

// The manifest's loaders read as `ui_script::…` at every call site, including the tests' `super::`.
// `load_default_ui` is no longer test-only: the addon harness (1188 phase 6) loads the whole
// shipped interface under each surveyed addon, because roughly half of what an addon calls is
// FrameXML's Lua rather than the engine's (decision 1190).
pub(crate) use manifest::load_default_ui;
pub(crate) use manifest::{load_font_registry, load_ingame_ui};

/// Is the pointer over *any* UI this frame — the egui dev overlay OR a mouse-enabled player-UI
/// frame? The single source of truth for "the mouse is talking to the UI, not the world",
/// combined by [`arbitrate_pointer_over_ui`] from both contributors (dev overlay =
/// [`EguiPointerOver`]; player UI = [`PlayerUiHover`]). Gameplay reads it so
/// a drag doesn't start mouse-look; the inspector reads it so a pick doesn't fire behind an
/// overlaid frame. Owned HERE, not by the dev plugin (decision 0026): gameplay's read must
/// survive a build without the dev overlays, so the combiner treats the egui half as optional.
#[derive(Resource, Default)]
pub(crate) struct PointerOverUi(pub(crate) bool);

/// The egui dev overlay's half of the pointer arbitration, written each egui pass by the debug
/// panel's `track_pointer_over_ui`. **Defined here, with the arbiter that reads it** (decision
/// 1174 finishing 0026): the type has to exist in a build with no dev overlays compiled in, and
/// the arbiter takes it as `Option<Res<…>>` so its absence simply means "nothing is hovering the
/// dev UI" — which is the player-faithful answer. The writer lives in `debug_panel`; a dev
/// module writing an always-present fact is the allowed direction.
#[derive(Resource, Default)]
pub(crate) struct EguiPointerOver(pub(crate) bool);

/// Whether mouseover **world picking** is armed — the dev-chord `I` inspector's mode, toggled by
/// `debug_panel::inspect`.
///
/// The second of the two resources 0026 named as needing "a player-safe home", and here for the
/// same reason as [`EguiPointerOver`]: `player::control` and `target::click` read it every frame
/// to decide whether a left-click is the inspector's or the game's, and those reads must compile
/// and behave in a build with no inspector. The [`Default`] — **disarmed** — *is* the player
/// behaviour, so a player build's readers take the ordinary branch with nothing to switch them.
#[derive(Resource, Default)]
pub(crate) struct InspectMode {
    pub(crate) enabled: bool,
}

/// One frame's UI-pass phase split, in μs — written by [`extract::drive_script`] under the same
/// marks the `[ui-cost]` line prints. **Owned by the producer** (decision 1174): the split is a
/// fact this pass publishes about itself, so it must exist whether or not the recorder that reads
/// it (`hover_log`) is compiled in. Its consumers are instruments; its writer is not.
#[derive(Resource, Default, Clone)]
pub(crate) struct UiFrameCost {
    /// How many FontStrings the layout asked the font engine to shape this frame, and the first
    /// few by name — a steady hover that keeps asking is the churn the recorder exists to catch.
    pub(crate) measured: usize,
    pub(crate) measured_texts: Vec<String>,
    pub(crate) tick: u128,
    pub(crate) resolve: u128,
    pub(crate) measure: u128,
    pub(crate) extract: u128,
    pub(crate) convert: u128,
    pub(crate) diff: u128,
    pub(crate) quads: usize,
    pub(crate) solves: u64,
    pub(crate) skipped: bool,
    /// How many entries the per-entry splice re-converted this frame — `0` on a settled or
    /// full-conversion frame. Nonzero is the proof the splice path fired (the equivalence tests
    /// and the live `[ui-cost] spliced=` field both read it).
    pub(crate) spliced: usize,
}

/// Does anything want [`UiFrameCost`] filled in this run? Measuring the split costs a clock read
/// per phase plus the churn strings, so the pass only pays when asked.
///
/// `WOW_UI_COST=1` (this module's own `[ui-cost]` line) arms it inline; the hover recorder arms it
/// by setting this — the direction that keeps the pass free of the instrument's name, and the
/// reason the flag is a resource rather than an env read here (decision 1174). Default `false` is
/// the player answer.
#[derive(Resource, Default)]
pub(crate) struct UiCostWanted(pub(crate) bool);

/// The frame the cursor is currently over (a mouse-enabled, visible player-UI frame), or `None`.
/// Written by [`feed_ui_input`], read by the pointer arbiter below so world-pick
/// and camera-look yield to the UI (decision 0026's single-source `PointerOverUi`).
#[derive(Resource, Default)]
pub(crate) struct PlayerUiHover(pub(crate) Option<u32>);

/// Set each frame by [`feed_ui_input`] to [`UiScript::has_keyboard_focus`]: true while an EditBox owns
/// keyboard focus and is eating every key. The gameplay/dev keyboard readers (movement, the Z sheath,
/// the HUD/panel/inspect toggles, chat-open, target-clear) gate on it so a key typed into a focused box
/// never also drives the world — the app-side twin of the client's `DAT_00cf4dc8 != 0` gate (RF-0082
/// §1). One mechanism, written once here; every reader is ordered after `UiInput` so it sees this
/// frame's value, not last frame's.
#[derive(Resource, Default)]
pub(crate) struct UiKeyboardCapture(pub(crate) bool);

/// Set each frame by [`feed_ui_input`]: true for a LEFT press this frame that hit no frame but was
/// consumed by the UI — the world-drop of a held cursor payload over EMPTY world (0216 §3,
/// narrowed by decision 0571: over a world object the reference keeps an item payload and runs
/// SELECT, so the press is NOT consumed there): world click-pick and camera orbit-start must
/// yield exactly as they do for a hovered click.
#[derive(Resource, Default)]
pub(crate) struct PlayerUiClickConsumed(pub(crate) bool);

/// Set each frame by [`feed_ui_input`] (after the mouse feed, so a same-frame pickup counts):
/// whether the cursor currently carries a payload ([`UiScript::cursor_payload`]) — a `Send`
/// mirror for world-click consumers that shouldn't take the `NonSend` VM just to ask. Read by
/// [`crate::target`]'s select router: a click on NOTHING (sky) with a payload held never
/// deselects (the reference's nothing-leg `SetSelection(0,0)` is no-payload-gated — `0x492d30`'s
/// local flag test; the terrain leg is not — decisions 0571 + 0574).
#[derive(Resource, Default)]
pub(crate) struct CursorPayloadHeld(pub(crate) bool);

/// The player-UI input pass — hit-testing + handler firing. Ordered before [`WorldStage::Input`] so
/// the [`PlayerUiHover`] it produces is folded into `PointerOverUi` before `player::control` reads it.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct UiInput;

/// The frame's atomic (`Instant`, `GetTime`) clock pair — the ONE lawful base for mapping a
/// store-side `Instant` onto the VM's `GetTime` clock (`CooldownInfo::ui_triple` and kin).
///
/// Written at the single `script.tick` site ([`extract::drive_script`]): `ui_now` is the value the
/// VM clock just advanced to, `anchor` is `Time<Real>`'s own `last_update()` — the exact instant
/// whose frame-to-frame differences ARE the deltas the VM clock accumulates. Because both legs
/// advance in lockstep by construction, a conversion `ui_now - (anchor - start)` yields the SAME
/// number every frame for one fixed `start` — the frame-stability 0375's absolute-start triples
/// require. Converting through `Instant::now()` sampled inside a feed system instead (the pre-fix
/// shape) re-measures the tick→feed scheduling gap every frame and wobbles the derived start by
/// that jitter (±12 ms observed live), turning every running cooldown into a per-frame "changed"
/// triple — the diff churn 0375 existed to kill.
#[derive(Resource)]
pub(crate) struct UiClock {
    /// The `Instant` leg: `Time<Real>::last_update()` at the tick that produced [`Self::ui_now`].
    pub(crate) anchor: std::time::Instant,
    /// The `GetTime` leg: the VM clock's value after that tick (seconds).
    pub(crate) ui_now: f64,
}

impl Default for UiClock {
    fn default() -> Self {
        Self {
            anchor: std::time::Instant::now(),
            ui_now: 0.0,
        }
    }
}

/// Run a Lua chunk, logging (never discarding) a failure. App systems drive the VM with fire-and-
/// forget chunks; `let _ = script.run(…)` swallows the error — the chat header machine died
/// mid-chunk on a missing `EditBox:SetTextColor` every single frame and nothing ever said so
/// (the /w caret bug). A chunk failure is always an app or engine defect; this is the mandatory
/// form for any run whose `Result` isn't otherwise consumed.
/// The FrameXML digest of the interface this process loads — see [`content::digest`]. Re-exported
/// so the corpus harness can stamp every report with the tree it measured.
pub(crate) fn framexml_digest() -> String {
    content::digest()
}

pub(crate) fn run_or_warn(script: &benilla_ui::script::UiScript, chunk: &str) {
    if let Err(e) = script.run(chunk) {
        warn!("ui_script: chunk failed: {e}");
    }
}

/// The reference's `uiScale` cvar — the user dial on TOP of the 768-virtual base (decision 0584):
/// px-per-UI-unit multiplies by it, so the VM's virtual screen is `768/uiScale` units tall — the
/// same law that makes the reference's `uiScale = 768/screenH` its known pixel-perfect setting.
/// Folded into the one seam scale by [`seam_scale`]; at `1.0` (this `Default`, what every test
/// pins) the pipeline is bit-identical to the pre-dial 0582 behavior. The app inserts
/// [`default_ui_scale`] instead — the director's taste default, `WOW_UI_SCALE=` per-run override —
/// until a real cvar system subsumes this dial (0582's named residual).
#[derive(Resource)]
pub(crate) struct UiScaleCvar(pub(crate) f32);

impl Default for UiScaleCvar {
    fn default() -> Self {
        Self(1.0)
    }
}

/// The shipped default (the reference's slider spans 0.64..1.0; 1.0 read oversized to the
/// director's eye — the taste call this dial exists for).
///
/// **It also happens to be what the reference itself lands on above ~853 px tall**, which we did
/// not know when it was chosen (wow-re `system/ui/scratch/modelframe-uiscale-law.md`, out of the
/// autocast-shine thread): `0x492f70` computes `max((H > 768) ? 768/H : 1.0, 0.9)` and is called
/// **both** on a mode set and from the `useUiScale`-OFF leg (`0x4908ad`) — and OFF is the shipped
/// default (`0x8430c0` = `"0"`). So the reference is 1.0 at 768 and below, 0.96 at 1280×800, and
/// **0.9 at 1080p and 1440p**. Our flat 0.9 therefore agrees with it on every modern window and
/// diverges only *below* ~853 px tall, where the reference is nearer 1.0. Left alone deliberately:
/// this constant is a director taste call, and the divergence is confined to window heights we
/// do not ship against. The reference's own law is written here so the next session inherits it
/// rather than re-deriving it.
pub(crate) const DEFAULT_UI_SCALE: f32 = 0.9;

/// The app's `uiScale`: `WOW_UI_SCALE=` if set (clamped to the plausible dial range), else
/// [`DEFAULT_UI_SCALE`] — the env override makes taste iteration a relaunch, not a rebuild.
fn default_ui_scale() -> f32 {
    std::env::var("WOW_UI_SCALE")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .map(|v| v.clamp(0.5, 1.5))
        .unwrap_or(DEFAULT_UI_SCALE)
}

/// THE seam scale `s` (decision 0582 + the 0584 dial): window px per UI unit —
/// `windowH/768 × uiScale`. Every crossing of the VM boundary uses this one number: extraction
/// ×s out, mouse ÷s in, measures at ×s answered ÷s, the atlas bakes at ×s. A degenerate window
/// (h ≤ 0, the pre-winit frame) is identity.
pub(crate) fn seam_scale(window_h: f32, ui_scale: f32) -> f32 {
    if window_h > 0.0 {
        window_h / 768.0 * ui_scale
    } else {
        1.0
    }
}

/// Adds the Lua UI host as a `NonSend` resource (an mlua VM is `!Send`) and the per-frame
/// tick → resolve → extract pipeline into [`UiQuads`], plus the input pass ([`feed_ui_input`]).
pub(crate) struct UiScriptPlugin;

impl Plugin for UiScriptPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(UiScaleCvar(default_ui_scale()))
            // The UI pass publishes its per-frame phase split here every frame the cost meter or
            // the hover recorder is armed; the producer owns the resource so any minimal app that
            // runs `drive_script` (the extract tests) has it.
            .init_resource::<UiFrameCost>()
            .init_resource::<UiCostWanted>()
            .init_resource::<PointerOverUi>()
            .init_resource::<EguiPointerOver>()
            .init_resource::<InspectMode>()
            .init_resource::<PlayerUiHover>()
            .init_resource::<UiKeyboardCapture>()
            .init_resource::<PlayerUiClickConsumed>()
            .init_resource::<CursorPayloadHeld>()
            .init_resource::<UiClock>()
            .init_resource::<AddOnIdentity>()
            // After `AssetSet::Open` so the patch chain exists at boot: the VM's first load is
            // the real `GlobalStrings.lua` (the reference's own FrameXML order), which the
            // cast-fail display (0427) resolves its messages from.
            .add_systems(Startup, setup_script.after(benilla_assets::AssetSet::Open))
            // The in-game UI materializes on entering the world, not at boot (1051) — the
            // reference's own seam; only the font registry loads at `Startup`. The entry edge
            // ARMS the load; it runs a few frames later, once the loading cover has actually
            // presented — the ~0.5 s burst must never stall the frame whose render would first
            // show the cover, or what covers it is the frozen character screen (0962's frame
            // accounting; see [`lifecycle::PendingEntryUiLoad`]).
            .add_systems(
                OnEnter(crate::char_select::ClientState::InWorld),
                lifecycle::arm_entry_ui_load,
            )
            // The whole shutdown tail — events then writes, in the reference's order. See
            // [`shutdown_ui_state`]; the two edges here are its five roots as far as our session
            // has them.
            .add_systems(
                OnExit(crate::char_select::ClientState::InWorld),
                end_ui_session,
            )
            .add_systems(Update, shutdown_on_exit)
            // A queued `ReloadUI()` runs in `PreUpdate` — one whole frame after the drain that
            // queued it (the reference's own deferral, `0x495590`), and BEFORE every `Update`
            // system, so no per-VM seed or feed can run against the dying VM in the reload frame
            // and then leave the new one unseeded until the next. In `Update` the exclusive
            // system would float: the scheduler could place it between `sync_cvars` and
            // `save_config`, discarding a dirty CVar edit, or after `seed_bindings`, giving one
            // whole frame with an empty binding table. See [`run_pending_reload`].
            .init_resource::<ReloadUiPending>()
            // The armed entry load shares the reload's slot, chained after it: both are
            // exclusive edges on the same VM, and a reload must not interleave a pending entry
            // load.
            .add_systems(
                PreUpdate,
                (run_pending_reload, lifecycle::run_pending_entry_load).chain(),
            )
            // `GetFramerate()`'s host half (decision 1195), before the VM ticks so an `OnUpdate`
            // handler reads this frame's number rather than the previous one's.
            .add_systems(Update, feed_framerate.before(UiInput))
            // `drive_script` resolves layout; `feed_ui_input` hit-tests against those rects, so they
            // chain (also required because both take the single `NonSend` VM). The pair runs before
            // `WorldStage::Input` so the hover result reaches the pointer arbiter in time. The input
            // pass is in-world only (decision 0193): the character-select glue screen owns the
            // pointer + keyboard there (its exit edge resets the latches this pass normally drives).
            .add_systems(
                Update,
                (
                    extract::drive_script,
                    input::feed_ui_input.run_if(in_state(crate::char_select::ClientState::InWorld)),
                )
                    .chain()
                    .in_set(UiInput)
                    // The binding dispatch (0997) runs in this same set, after the feed: it
                    // reads the capture gate the feed just wrote, so a key a focused box
                    // consumed this frame never also fires a binding.
                    .before(crate::bindings::BindingSet)
                    .before(WorldStage::Input),
            )
            // Combine the two pointer contributions (dev overlay + player UI) into the single
            // `PointerOverUi` source of truth, after the hover is known and before gameplay reads it.
            .add_systems(
                Update,
                arbitrate_pointer_over_ui
                    .after(UiInput)
                    .before(WorldStage::Input),
            );

        // `WOW_CAPTURE_UI=1` on a capture: override the "player" token with a synthetic snapshot so
        // the unit frames are populated in a server-less capture. Ordered after the real feed (so it
        // wins) and before the VM ticks/dispatches this frame. Never active outside capture mode —
        // synthetic data must not reach a real run.
        app.add_systems(
            Update,
            demo_unit_feed
                .after(UnitFeed)
                .before(UiInput)
                .run_if(capture_ui_active),
        );
    }
}

/// Should this CAPTURE include the player UI (+ the synthetic unit snapshot)? The visual harness's
/// baselines must stay UI-free (they regression-test the WORLD render), so captures skip the UI
/// unless `WOW_CAPTURE_UI=1` opts in. Normal runs always load the UI and never take synthetic data.
fn capture_ui_active(capture: Option<Res<crate::run_mode::CaptureMode>>) -> bool {
    capture.is_some() && std::env::var("WOW_CAPTURE_UI").as_deref() == Ok("1")
}

/// The pointer arbiter (decision 0026): `PointerOverUi = egui dev overlay ∨ player-UI hover`. Runs
/// regardless of whether the UI VM exists (if it's absent, [`PlayerUiHover`] stays `None` and this is
/// just the egui bit) — and regardless of the DEV overlays existing (their half is `Option`, so a
/// player build without the debug-panel plugin arbitrates on the player UI alone).
fn arbitrate_pointer_over_ui(
    egui: Option<Res<EguiPointerOver>>,
    hover: Res<PlayerUiHover>,
    mut over: ResMut<PointerOverUi>,
) {
    over.0 = egui.is_some_and(|e| e.0) || hover.0.is_some();
}

/// The session lifecycle — the VM's birth, identity, death, and the reload. Every edge function
/// lives there; this module keeps the per-frame bridge. See its header.
mod lifecycle;
pub(crate) use lifecycle::{
    end_ui_session, ingame_ui_pending, run_pending_reload, setup_script, AddOnIdentity,
    PendingEntryUiLoad, ReloadUiPending,
};
// Consumed only from other modules' test code (the emote-table checks, the harness's UI-init
// tail, the quit-once pin) — a plain re-export would warn unused in a non-test build.
#[cfg(test)]
use lifecycle::load_ingame_ui_on_world_entry;
use lifecycle::shutdown_on_exit;
#[cfg(test)]
pub(crate) use lifecycle::{
    finish_ui_load, is_emote_token_line, seat_from_roster, shutdown_ui_state,
};

/// `GetFramerate()`'s host half: push a **smoothed** frames-per-second into the VM each frame
/// (decision 1195).
///
/// Smoothed, not `1.0 / delta`, for the reason the reference smooths it too: 71 corpus addons put
/// this number on a panel, and a raw per-frame reciprocal reads as a flickering three-digit mess
/// that nobody can take a value off. The pole is a one-second time constant — fast enough that a
/// real stall is visible within a frame or two, slow enough to read.
///
/// `Time<Real>` deliberately, the same choice `drive_script` documents: this is a *frame rate*, and
/// a virtual clock that clamps its delta would report a rate the machine is not achieving.
fn feed_framerate(
    script: Option<NonSendMut<UiScript>>,
    time: Res<Time<bevy::time::Real>>,
    mut smoothed: Local<f64>,
) {
    let Some(mut script) = script else {
        return;
    };
    let dt = time.delta_secs_f64();
    if dt <= 0.0 {
        return; // the first frame, and any paused one — nothing to average in
    }
    let instant = 1.0 / dt;
    // One-pole IIR with a 1 s time constant, frame-rate independent.
    let alpha = (dt / 1.0).min(1.0);
    *smoothed = if *smoothed <= 0.0 {
        instant
    } else {
        *smoothed + alpha * (instant - *smoothed)
    };
    script.set_framerate(*smoothed);
}

/// Feed synthetic `"player"`/`"target"` snapshots each frame (overriding the real feed, which finds
/// no avatar in a server-less capture) and fire the initial events once — proving the full chain
/// end-to-end on a screenshot: snapshot → `Unit*` bindings → Lua `OnEvent` → bars + text, for both
/// unit frames (the target one included, so captures regression-test its show-on-target path).
fn demo_unit_feed(script: Option<NonSendMut<UiScript>>, mut fired: Local<VmMemo<bool>>) {
    /// The synthetic target's guid — a creature-family high part, so nothing mistakes it for a
    /// player. Only its *distinctness* matters.
    const DEMO_TARGET_GUID: u64 = 0xF130_0000_0000_0001;

    let Some(mut script) = script else {
        return;
    };
    // Session-keyed like every other seed (1290): the one-shot below is `PLAYER_ENTERING_WORLD`
    // and the bar seeds, which a fresh VM needs again.
    let fired = fired.get(&script);
    // TEMP DEBUG (WOW_CAPTURE_REHOVER=1): re-fire the world hover EVERY frame — the live
    // flapping-raycast simulation for the under-sized-plate investigation.
    if std::env::var("WOW_CAPTURE_REHOVER").as_deref() == Ok("1") {
        script.world_tooltip_unit("mouseover");
    }
    script.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            name: Some("Benilla".into()),
            health: 72,
            max_health: 100,
            level: 12,
            power_type: 0, // mana
            power: 45,
            max_power: 80,
            dead: false,
            reaction: 0, // own avatar — no reaction to itself
            // Race/class so the ui-char capture's level line reads "Level 12 Night Elf Warrior".
            race: Some("Night Elf".into()),
            race_file: Some("NightElf".into()),
            class: Some("Warrior".into()),
            class_file: Some("WARRIOR".into()),
            sex: 2,
            is_player: true,
            ..Default::default()
        }),
    );
    script.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Young Wolf".into()),
            health: 30,
            max_health: 50,
            level: 3,
            // A powerless beast (maxPower 0): exercises the power-bar-hide path in the capture.
            power_type: 0,
            power: 0,
            max_power: 0,
            dead: false,
            reaction: 4, // neutral → the name plate reads yellow (UnitReactionColor[4])
            // A beast: no race/class tokens (UnitRace/UnitClass report the absent shape).
            race: None,
            race_file: None,
            class: None,
            class_file: None,
            sex: 0,
            // A beast type word so the mouseover/target tooltip's level line reads
            // "Level 3 Beast" in captures.
            creature_type_name: Some("Beast".into()),
            // A real guid: the combo seed below banks its points against exactly this unit, and
            // `GetComboPoints` refuses to report points banked on anything but the CURRENT target
            // (decision 0875). Also stops the demo player and target reading as the same unit,
            // which two default zeros would.
            guid: DEMO_TARGET_GUID,
            ..Default::default()
        }),
    );
    // The combo dots (`ComboFrame`) need a class that can SEE them and points banked on the
    // selected unit — seeded only for their own scenario, because the demo player is a warrior
    // everywhere else (ui-char's level line reads "Level 12 Night Elf Warrior" off that) and a
    // warrior authentically lights no dot. Run it with
    // `WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-combopoints`.
    if std::env::var("WOW_CAPTURE").as_deref() == Ok("ui-combopoints") {
        script.set_player_req_state(benilla_ui::script::PlayerReqState {
            level: 12,
            class_id: 4, // rogue
            ..Default::default()
        });
        script.set_combo_points(4, DEMO_TARGET_GUID);
        script.fire_event("PLAYER_COMBO_POINTS", vec![]);
    }
    if !*fired {
        // A few synthetic bar slots so captures show the action bar populated (battle-stance
        // page: actions 73.. — the page a real warrior login lands on). Spread across the 12 wells
        // (buttons 1,2,3,8,12) so the ui-actionbar capture shows icons seated left-to-right. Slot
        // 80 is an ITEM-kind action (decision 0216 §7) with a synthetic count of 5, so the capture
        // also proves the Count fontstring wires up (`GetActionCount` reads the engine's own
        // pushed count directly — no live server template needed in a capture). It must also seed
        // the `IsConsumableAction` GATE, or the count paints nothing: 0926 put a gate in front of
        // it that this seed never fed, so the capture quietly stopped showing the "5" it exists to
        // prove (found while fixing decision 1301).
        script.set_bonus_bar_offset(1);
        for (action, icon, kind, id, count) in [
            (
                73,
                "Interface\\Icons\\Ability_SteelMelee",
                0x00u8,
                100u32,
                0u32,
            ),
            (74, "Interface\\Icons\\Ability_Rogue_Ambush", 0x00, 101, 0),
            (
                75,
                "Interface\\Icons\\Ability_Warrior_BattleShout",
                0x00,
                102,
                0,
            ),
            (80, "Interface\\Icons\\INV_Misc_Food_16", 0x80, 117, 5),
            (84, "Interface\\Icons\\Spell_Holy_SealOfMight", 0x00, 103, 0),
            // The always-on multibars (MultiBars.xml): BottomLeft = actions 61..72, BottomRight
            // = 49..60. A few occupied wells on each so the capture shows both rows seated
            // (empty multibar wells hide — the ref's own default — so without these the rows
            // would be invisible).
            (
                61,
                "Interface\\Icons\\Spell_Nature_Regenerate",
                0x00,
                104,
                0,
            ),
            (62, "Interface\\Icons\\Spell_Shadow_Curse", 0x00, 105, 0),
            (
                72,
                "Interface\\Icons\\Spell_Frost_FrostBolt02",
                0x00,
                106,
                0,
            ),
            (49, "Interface\\Icons\\Spell_Fire_FlameBolt", 0x00, 107, 0),
            (60, "Interface\\Icons\\Spell_Holy_Heal", 0x00, 108, 0),
        ] {
            script.set_action(
                action,
                Some(ActionSlot {
                    texture: Some(icon.into()),
                    kind,
                    action: id,
                    count,
                    // The seed's only ITEM slot is the food stack, which is consumable.
                    consumable: kind == 0x80,
                }),
            );
        }
        // The stance bar (StanceBar.xml): the synthetic warrior's three stances, battle active —
        // matches the bonus offset 1 above (battle stance page). Defensive shows the not-castable
        // grey; berserker a running cooldown swipe.
        script.set_shapeshift_forms(vec![
            benilla_ui::script::ShapeshiftFormView {
                spell_id: 2457,
                texture: Some("Interface\\Icons\\Ability_Warrior_OffensiveStance".into()),
                name: "Battle Stance".into(),
                active: true,
                castable: true,
                cooldown: None,
            },
            benilla_ui::script::ShapeshiftFormView {
                spell_id: 71,
                texture: Some("Interface\\Icons\\Ability_Warrior_DefensiveStance".into()),
                name: "Defensive Stance".into(),
                active: false,
                castable: false,
                cooldown: None,
            },
            benilla_ui::script::ShapeshiftFormView {
                spell_id: 2458,
                texture: Some("Interface\\Icons\\Ability_Racial_Avatar".into()),
                name: "Berserker Stance".into(),
                active: false,
                castable: true,
                cooldown: Some((900, 1500, true)),
            },
        ]);
        script.fire_event("UPDATE_SHAPESHIFT_FORMS", vec![]);
        // XP partway into the level (70%) so the MainMenuBar XP bar renders a purple partial fill.
        // Set before PLAYER_ENTERING_WORLD so the bar's first Update reads it.
        script.set_player_xp(4200, 6000);
        script.fire_event("PLAYER_ENTERING_WORLD", vec![]);
        // The two bottom multibars are player options and ship OFF since 1500, and a capture runs
        // with no server behind it — so the login seed reads a zero toggle byte and neither bar
        // comes up. Raise them the way the Options rows do, or the 61../49.. wells seeded above
        // draw nowhere and `ui-actionbar` loses two rows it exists to show. This is the DEMO's
        // choice about what to photograph, not a default: `MultiActionBar_Update` is the same
        // function the row calls, so nothing here is a private door into the bars. Guarded because
        // this feed also runs with `WOW_CAPTURE_UI` unset, where no interface has been loaded.
        let _ = script.run(
            "SHOW_MULTI_ACTIONBAR_1 = 1 SHOW_MULTI_ACTIONBAR_2 = 1 \
             if MultiActionBar_Update then MultiActionBar_Update() end",
        );
        script.fire_event("PLAYER_XP_UPDATE", vec![]);
        for token in ["player", "target"] {
            script.fire_event("UNIT_HEALTH", vec![ScriptValue::Str(token.into())]);
        }
        script.fire_event("PLAYER_TARGET_CHANGED", vec![]);
        *fired = true;
    }
}

/// A fixed-advance stand-in font engine for tests — every character `.0` wide, one line tall,
/// greedily wrapped. The *numbers* the real engine produces are pinned against the client's own
/// fonts by `ui_text::atlas::metrics_tests`; a fixture wants arithmetic a reader can do in their
/// head, and wants a measure to arrive **in the tick that asked** exactly as the app's does.
#[cfg(test)]
pub(crate) struct FixedWidthFont(pub(crate) f32);

#[cfg(test)]
impl benilla_ui::script::TextMeasure for FixedWidthFont {
    fn measure(&mut self, req: &benilla_ui::script::MeasureRequest) -> (f32, f32, f32) {
        let natural = req.text.chars().count() as f32 * self.0;
        match req.wrap_width {
            Some(w) if w > 0.0 && natural > w => (w, 12.0 * (natural / w).ceil(), natural),
            _ => (natural, 12.0, natural),
        }
    }
}

#[cfg(test)]
mod cast_tests;

#[cfg(test)]
mod mirror_timer_tests;

#[cfg(test)]
mod combat_text_tests;

#[cfg(test)]
mod combo_frame_tests;

#[cfg(test)]
mod unit_frame_tests;

#[cfg(test)]
mod unit_popup_tests;

#[cfg(test)]
mod dropdown_tests;

#[cfg(test)]
mod action_bar_tests;

#[cfg(test)]
mod exp_bar_tests;

#[cfg(test)]
mod multibar_stance_tests;

#[cfg(test)]
mod pet_bar_tests;

#[cfg(test)]
mod pet_frame_tests;

#[cfg(test)]
mod micro_menu_tests;

#[cfg(test)]
mod perf_bar_tests;

#[cfg(test)]
mod panel_tests;

#[cfg(test)]
mod fade_tests;

#[cfg(test)]
mod merchant_tests;

#[cfg(test)]
mod money_frame_tests;

#[cfg(test)]
mod faux_scroll_tests;

/// The reference's shared widget kit (UIPanelTemplates.xml + OptionsFrameTemplates.xml), driven
/// through `CreateFrame`'s fourth argument the way an addon drives it — decision 1203's queue.
#[cfg(test)]
mod panel_template_tests;

/// The reference's BasicControls.xml — TEXT/message/_ERRORMESSAGE and the ScriptErrors dialog,
/// none of which benilla itself calls: every test enters from Lua the way an addon does.
#[cfg(test)]
mod basic_controls_tests;

#[cfg(test)]
mod color_picker_tests;

/// `UIParent.xml`'s loose addon-facing helpers (`MouseIsOver` and kin) — the panel and ESC halves
/// of that file live in `panel_tests` / `escape_tests`.
#[cfg(test)]
mod uiparent_tests;

#[cfg(test)]
mod trainer_tests;

#[cfg(test)]
mod bank_tests;

#[cfg(test)]
mod taxi_tests;

#[cfg(test)]
mod loot_tests;

#[cfg(test)]
mod group_loot_tests;

#[cfg(test)]
mod chat_tests;

#[cfg(test)]
mod bag_tests;
#[cfg(test)]
mod resolve_bench;

#[cfg(test)]
mod tooltip_anchor_tests;

#[cfg(test)]
mod tooltip_compare_tests;

/// `GameTooltipTemplate` as an ADDON sees it — the corpus's most-wanted template, driven through
/// `inherits=` and `CreateFrame` the way the 27 addons that name it do.
#[cfg(test)]
mod tooltip_template_tests;

#[cfg(test)]
mod escape_tests;

#[cfg(test)]
mod game_menu_tests;

#[cfg(test)]
mod macro_tests;

// `pub(crate)` for its `harness`/`on_page` alone: the bindings dispatch tests drive the real
// Keybindings page through the real input systems, and building the page twice would let the two
// copies drift.
#[cfg(test)]
pub(crate) mod keybindings_tests;
#[cfg(test)]
mod options_tests;

#[cfg(test)]
mod delete_item_tests;

#[cfg(test)]
mod static_popup_tests;

#[cfg(test)]
mod binder_tests;

#[cfg(test)]
mod death_tests;

#[cfg(test)]
mod duel_tests;

#[cfg(test)]
mod enchant_confirm_tests;

/// The shared reference-geometry diff (decision 0675) every transcribed window's test calls.
#[cfg(test)]
mod framexml_diff;

#[cfg(test)]
mod friends_tests;

/// The four guild windows — the social window's third tab and its three satellites (decision
/// 1257). Its own module rather than more of `friends_tests` because it stands the whole guild
/// engine API in for in Lua before the XML loads, which that file must not.
#[cfg(test)]
mod guild_tests;

#[cfg(test)]
mod quest_tests;

#[cfg(test)]
mod quest_timer_tests;

#[cfg(test)]
mod durability_tests;

#[cfg(test)]
mod screenshot_tests;

#[cfg(test)]
mod questlog_tests;

#[cfg(test)]
mod character_tests;

#[cfg(test)]
mod skills_frame_tests;

#[cfg(test)]
mod reputation_frame_tests;

#[cfg(test)]
mod pet_paperdoll_tests;

#[cfg(test)]
mod inspect_tests;

#[cfg(test)]
mod dressup_tests;

#[cfg(test)]
mod minimap_tests;

#[cfg(test)]
mod spellbook_tests;

#[cfg(test)]
mod buff_tests;

#[cfg(test)]
mod target_aura_tests;

#[cfg(test)]
mod zone_text_tests;

#[cfg(test)]
mod errors_tests;

#[cfg(test)]
mod shipped_xml_tests;

#[cfg(test)]
mod bottom_hud_tests;

#[cfg(test)]
mod bagnon_render_tests;

/// Bug B267 end to end: a hunter's Quiver publishes its global functions (see the file header for
/// the three walls that stopped it).
#[cfg(test)]
mod quiver_tests;

/// The UI's per-world-entry lifecycle: teardown at the character screen, a genuine second load at
/// the next login (decision 1290).
#[cfg(test)]
mod world_entry_tests;

#[cfg(test)]
mod seam_scale_tests {
    use super::seam_scale;

    #[test]
    fn seam_scale_is_the_768_base_times_the_dial() {
        // The 0582 base: identity at the design height, proportional elsewhere.
        assert_eq!(seam_scale(768.0, 1.0), 1.0);
        assert_eq!(seam_scale(1536.0, 1.0), 2.0);
        // The 0584 dial multiplies it.
        assert_eq!(seam_scale(768.0, 0.9), 0.9);
        // The reference's pixel-perfect setting: uiScale = 768/screenH → 1 px per UI unit.
        assert!((seam_scale(1080.0, 768.0 / 1080.0) - 1.0).abs() < 1e-6);
        // A degenerate (pre-winit) window is identity, never a division blow-up.
        assert_eq!(seam_scale(0.0, 0.9), 1.0);
    }
}
