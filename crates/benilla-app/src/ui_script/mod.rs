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
use benilla_assets::LockRecover;
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
            .init_resource::<IngameUiLoaded>()
            .init_resource::<AddOnIdentity>()
            // After `AssetSet::Open` so the patch chain exists at boot: the VM's first load is
            // the real `GlobalStrings.lua` (the reference's own FrameXML order), which the
            // cast-fail display (0427) resolves its messages from.
            .add_systems(Startup, setup_script.after(benilla_assets::AssetSet::Open))
            // The in-game UI materializes on entering the world, not at boot (1051) — the
            // reference's own seam. Only the font registry loads at `Startup`.
            .add_systems(
                OnEnter(crate::char_select::ClientState::InWorld),
                load_ingame_ui_on_world_entry,
            )
            // The whole shutdown tail — events then writes, in the reference's order. See
            // [`shutdown_ui_state`]; the two edges here are its five roots as far as our session
            // has them.
            .add_systems(
                OnExit(crate::char_select::ClientState::InWorld),
                shutdown_on_session_end,
            )
            .add_systems(Update, shutdown_on_exit)
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

fn setup_script(world: &mut World) {
    let script = match UiScript::new() {
        Ok(s) => s,
        Err(e) => {
            error!("ui_script: VM init failed: {e}");
            return;
        }
    };
    load_global_strings(world, &script);
    load_emote_tokens(world, &script);
    // ONLY the font-object registry at boot (1051). The glyph atlas bakes once, on the first
    // `Update`, from `script.font_objects()` — and our native glue screens share that one atlas, so
    // the registry has to exist before the login screen or in-game text loses its outlined variants
    // and its registry-declared sizes for the whole session. The other 55 files are in-game UI and
    // load at world entry ([`load_ingame_ui`]); the reference splits at exactly this seam, with
    // GlueXML carrying its own `GlueFonts.xml`.
    if ui_wanted(world) {
        // Errors are already logged per file as they happen; the returned list is the test-side
        // assertion (`shipped_xml_tests`), not a second reporting channel.
        let _ = load_font_registry(&script);
    }
    world.insert_non_send_resource(script);
}

/// Does this run want the player UI at all? Captures stay pristine — their baselines regression-test
/// the WORLD render — unless `WOW_CAPTURE_UI=1` opts the UI in.
fn ui_wanted(world: &World) -> bool {
    !world.contains_resource::<crate::run_mode::CaptureMode>()
        || std::env::var("WOW_CAPTURE_UI").as_deref() == Ok("1")
}

/// Has the in-game UI been materialized this process? The load is **once per process**, not once
/// per world entry: nothing tears the frame tree down on `OnExit(InWorld)` yet, so a re-entry after
/// `/logout` must not stack a second copy of every window.
///
/// The reference *does* tear down and rebuild (`ClientDestroyGame 0x401ee0` ↔ `0x401570`, and its
/// `ReloadUI` is verified end to end doing exactly that). Matching it is the follow-on; this latch
/// is what makes the move safe without it, and is the thing to delete when the teardown lands.
#[derive(Resource, Default)]
pub(crate) struct IngameUiLoaded(pub(crate) bool);

/// `OnEnter(InWorld)`: materialize the in-game UI, once.
///
/// Safe on the state edge only because 1038 moved the initial transition after `PostStartup` — a
/// capture boots straight into `InWorld`, so before that this would have run ahead of
/// [`benilla_assets::AssetSet::Open`] and loaded against no patch chain.
fn load_ingame_ui_on_world_entry(world: &mut World) {
    if world.resource::<IngameUiLoaded>().0 || !ui_wanted(world) {
        return;
    }
    let Some(mut script) = world.remove_non_send_resource::<UiScript>() else {
        warn!("ui_script: entering the world with no VM — the in-game UI will not load");
        return;
    };
    // The character whose AddOn enable state applies. Resolved before the load because the
    // enable file gates which addons run at all.
    let identity = world
        .get_resource::<crate::char_select::Roster>()
        .and_then(crate::ui_macro::identity);
    // The realm name goes in BEFORE the UI loads, because `GetRealmName()` is read at addon file
    // scope — `MyAddonDB[GetRealmName()] = …` is the corpus idiom, and 24 addons stop on it
    // (decision 1195). The roster carries the auth realm-list entry this session connected to.
    let realm = world
        .get_resource::<crate::char_select::Roster>()
        .and_then(|r| r.realm.as_ref().map(|r| r.name.clone()))
        .unwrap_or_default();
    script.set_realm_name(&realm);
    // …and so does the PLAYER, for the same reason and with more riding on it — see
    // [`seat_from_roster`], which is where the why lives.
    if let Some(seat) = world
        .get_resource::<crate::char_select::Roster>()
        .and_then(seat_from_roster)
    {
        script.set_unit("player", Some(seat));
    }
    let _ = load_ingame_ui(&mut script, identity.as_ref());
    // The Minimap widget was born a moment ago with `MinimapState::default()`; seed its two live
    // zoom indices from the persisted CVars now, before anything reads them — the reference's own
    // minimap reset path copying each CVar object's int into its live index (decision 1131). Once
    // only: from here the widget's index is the live truth and `Minimap:SetZoom` writes the CVar
    // back. Startup always precedes this state edge (1038), so the knob is already loaded.
    let zoom = world.resource::<crate::minimap::MinimapZoom>();
    script.set_minimap_zoom(zoom.outdoor, zoom.inside);
    // The saved-variables chunk runs HERE — after the XML assigned its file-scope defaults, before
    // any consumer reads them — then `VARIABLES_LOADED`. That is the reference's own load order
    // (`AddOn_Load 0x51f240` steps 2 → 4 → 6, decision 1128); reversing it means the defaults
    // always win and nothing can ever be remembered.
    finish_ui_load(&mut script);
    world.insert_non_send_resource(script);
    world.insert_resource(AddOnIdentity(identity));
    world.resource_mut::<IngameUiLoaded>().0 = true;
}

/// The `"player"` snapshot the UI loads **under**, built from the roster row of the pick in
/// flight — `None` when there is no pick (a capture, a scenario, a test world).
///
/// **The reference's invariant is that addon file scope always sees a real character**:
/// `AddOn_Load 0x51f240` runs from inside `UI_Init 0x48fbf0`, which is after the world is entered.
/// benilla's does not. `Connected` flips us `InWorld` a whole server round-trip before the self
/// descriptor streams in ([`crate::ui_unit`]'s own comment measures that gap in *seconds*), and
/// `feed_units` — the only writer of the `"player"` token — is gated on that descriptor existing.
/// So until this existed, every addon's file scope ran in a VM where `UnitName("player")` was
/// **nil**, which is a state a real session cannot present. It is the same argument, at the same
/// line, as the `set_realm_name` above it (decision 1195) — and this is the more load-bearing half.
///
/// **The failure it fixes is silent, which is why it survived every instrument.** The director
/// installed Bagnon, opened their bags, and got a window with a title, a gold line and **no bag
/// slots at all**. `Bagnon_Core/core/Utility.lua:5` opens `local currentPlayer =
/// UnitName("player")`, and every one of Bagnon's "am I looking at a cached snapshot of some OTHER
/// character?" predicates is `currentPlayer ~= frame.player`. With `currentPlayer` nil, Bagnon
/// concluded the live player's own bags belonged to somebody else, took every bag size from
/// Bagnon_Forever's (empty) offline cache instead of `GetContainerNumSlots`, created zero item
/// buttons — and raised nothing, so `loaded`, `session` and the UI probe all scored it a pass.
/// Reproduced both ways in [`bagnon_render_tests`].
///
/// **What is filled is what the roster actually knows**: name, race, class, gender and level, all
/// a round-trip ahead of the descriptor (the same fact [`crate::char_select::Roster::pending_entry`]
/// already exploits for the streamers). Health and power are deliberately left at zero — those are
/// the descriptor's to say, they land within the second, and inventing them would be a different
/// lie from the one being fixed.
fn seat_from_roster(roster: &crate::char_select::Roster) -> Option<benilla_ui::script::UnitState> {
    let row = roster.pending_row()?;
    let race = crate::ui_unit::race_names(row.race);
    let class = crate::ui_unit::class_names(row.class);
    Some(benilla_ui::script::UnitState {
        exists: true,
        name: Some(row.name.clone()),
        level: u32::from(row.level),
        race: race.map(|(n, _)| n.to_string()),
        race_file: race.map(|(_, f)| f.to_string()),
        class: class.map(|(n, _)| n.to_string()),
        class_file: class.map(|(_, f)| f.to_string()),
        // The wire's 0/1 on `UnitSex`'s 2/3 scale — `ui_unit::snapshot`'s own mapping.
        sex: match row.gender {
            0 => 2,
            1 => 3,
            _ => 0,
        },
        is_player: true,
        // Nil here is not "no faction", it is a state a player character cannot be in, and
        // AceDB-2.0 concatenates it at file scope — see [`crate::ui_unit::race_faction_group`].
        faction_group: crate::ui_unit::race_faction_group(row.race).map(str::to_string),
        ..Default::default()
    })
}

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

/// The character the loaded AddOn enable state belongs to, remembered so the shutdown write goes
/// back to the file it came from — the roster's pick can be gone by then.
#[derive(Resource, Default)]
pub(crate) struct AddOnIdentity(pub(crate) Option<(String, String)>);

/// **The UI shutdown, in the reference's own order** — `0x490bd0`, whose ordered tail wow-5875-re
/// carves as (`system/ui/ui.md`):
///
/// > `PLAYER_LEAVING_WORLD` (273) → **`PLAYER_LOGOUT`** (271, `0x490c2a`) → `layout-cache.txt` →
/// > **the flat saved file** (`0x490c7e`) → **the per-addon files** (`0x490c83`) → `AddOns.txt` →
/// > destroy the Lua state
///
/// **`PLAYER_LOGOUT` fires before any write, and that is the point**: it is an addon's last chance
/// to mutate a saved global, so a handler that stores "where I left off" runs while the write is
/// still ahead of it. Firing it after would make the event useless and the bug invisible.
///
/// One function, called from every root, because the steps are ordered *against each other* —
/// three independent Bevy systems on one state edge cannot express that, and until this landed the
/// flat write and the `AddOns.txt` write were exactly that.
///
/// **There is no autosave**, deliberately: the reference has none (decision 1128, and
/// `ds:0xb4b3f4` has three references image-wide). These are a handful of scalars a player toggles
/// a few times a session, and every file is written whole from the live globals.
pub(crate) fn shutdown_ui_state(script: &mut UiScript, identity: Option<&(String, String)>) {
    script.fire_event("PLAYER_LEAVING_WORLD", vec![]);
    script.fire_event("PLAYER_LOGOUT", vec![]);
    crate::ui_saved::save(script);
    addons::save_addon_variables(script, identity);
    addons::save_enable_state(script, identity);
}

/// `OnExit(InWorld)`: a `/logout` back to the glue, or a disconnect — two of the reference's five
/// roots.
fn shutdown_on_session_end(script: Option<NonSendMut<UiScript>>, id: Res<AddOnIdentity>) {
    if let Some(mut script) = script {
        shutdown_ui_state(&mut script, id.0.as_ref());
    }
}

/// `AppExit`: quitting the client — the quit / application-exit roots. Reads the message rather
/// than a state edge because a quit from in-world never leaves `InWorld`.
fn shutdown_on_exit(
    script: Option<NonSendMut<UiScript>>,
    id: Res<AddOnIdentity>,
    mut exits: MessageReader<AppExit>,
) {
    if exits.read().next().is_none() {
        return;
    }
    if let Some(mut script) = script {
        shutdown_ui_state(&mut script, id.0.as_ref());
    }
}

/// The UI-init sequence's ordered tail, once every file — ours and every addon's — has loaded:
/// the saved-variables chunk and `VARIABLES_LOADED`, then `PLAYER_LOGIN`.
///
/// **The order is the reference's**, byte-verified in wow-5875-re (`system/ui/ui.md`, and the
/// cascade in `system/ui/scratch/mail-pending-countdown.md`). Inside `UI_Init 0x48fbf0`, in
/// straight-line address order:
///
/// | | |
/// |---|---|
/// | `0x4900a3` → `0x51f600` | load every non-LoadOnDemand addon — each fires its own **`ADDON_LOADED`** (429, `0x51f5ad`) |
/// | `0x4900b2` → `0x4913b0` | read the flat saved file, fire **`VARIABLES_LOADED`** (430) |
/// | `0x490168` → `0x4908c0` | the world-enter cascade: **`PLAYER_LOGIN`** (`0x49094b`, `0x10e`) then **`PLAYER_ENTERING_WORLD`** (`0x490965`, `0x110`) |
///
/// So every non-LoD addon's `ADDON_LOADED` precedes `VARIABLES_LOADED`, which precedes
/// `PLAYER_LOGIN`. It is one function rather than three inline calls because that sequence is the
/// mechanism — an addon restores state on `ADDON_LOADED` and expects the saved chunk to have run,
/// and a window that waits on `PLAYER_LOGIN` expects both — so it is worth being able to assert.
///
/// **`PLAYER_LOGIN` is the conditional one; `PLAYER_ENTERING_WORLD` is not.** The cascade fires
/// the former only when `[0xb4e260]` is set, and only the FrameXML-loader path sets it, clearing
/// it immediately after — so it means "the UI came up", once, which is exactly the
/// [`IngameUiLoaded`] latch this sits behind. `PLAYER_ENTERING_WORLD` keeps its own per-entry
/// latch in [`crate::ui_unit`] and still lands after this, since it waits on the self descriptor
/// arriving over the wire.
pub(crate) fn finish_ui_load(script: &mut UiScript) {
    crate::ui_saved::load_saved_variables(script);
    script.fire_event("PLAYER_LOGIN", vec![]);
}

/// Execute the real `Interface\FrameXML\GlobalStrings.lua` off the patch chain into the VM —
/// the reference boots FrameXML with exactly this file FIRST, and it is the source of every
/// localized string global the UI reads (the cast-fail display's whole message set, 0427).
/// Loaded before our own `assets/ui` files, matching the reference order. Failures are LOUD:
/// a silently missing GlobalStrings once suppressed every red error line (the 0427 fold's
/// absent-key face is faithful data suppression — but only when the file actually loaded).
fn load_global_strings(world: &mut World, script: &UiScript) {
    let Some(assets) = world.get_resource::<benilla_assets::WorldAssets>() else {
        warn!("ui_script: no patch chain — GlobalStrings absent, error lines will be empty");
        return;
    };
    let bytes = {
        let mut chain = assets.chain.lock_recover();
        chain.read_file("Interface\\FrameXML\\GlobalStrings.lua")
    };
    let src = match bytes {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            error!("ui_script: GlobalStrings.lua read failed — error lines will be empty: {e:#}");
            return;
        }
    };
    if let Err(e) = script.run(&src) {
        error!("ui_script: GlobalStrings.lua failed to run: {e}");
        return;
    }
    // The sentinel: the exact lookup the cast-fail drain performs. If this misses, every
    // message would silently vanish — turn that failure mode into a diagnosable line. Presence
    // only, not the enUS text: a non-enUS install is still a loaded GlobalStrings.
    let sentinel: Option<String> = script.lua().globals().get("SPELL_FAILED_NO_AMMO").ok();
    match sentinel {
        Some(s) if !s.is_empty() => info!("ui_script: GlobalStrings loaded"),
        other => {
            error!("ui_script: GlobalStrings sentinel missing ({other:?}) — error lines broken")
        }
    }
}

/// Execute the reference's own **emote token table** into the VM (`EMOTE87_TOKEN = "SIT"`, …) —
/// the second half of the emote slash grammar (decision 0881). The *aliases* are in
/// `GlobalStrings.lua` above (`EMOTE87_CMD1 = "/sit"`), but the alias → `EmotesText.Name` mapping
/// lives in `ChatFrame.lua`: the reference's chat **code**, which benilla replaces in Rust. So we
/// take that file's **data** and none of its code — only whole lines matching
/// `EMOTE<digits>_TOKEN = "<UPPER>";` ([`is_emote_token_line`]) are executed, and the file's ~2400
/// lines of frame logic never run. Reading the shipped table beats transcribing 170 tokens into
/// Rust: a transcription can be wrong, and a hand-kept alias list is exactly what left 61 real
/// commands (`/lol`, `/hi`, `/ty`, …) unresolvable before 0881.
fn load_emote_tokens(world: &mut World, script: &UiScript) {
    let Some(assets) = world.get_resource::<benilla_assets::WorldAssets>() else {
        return; // already WARNed by load_global_strings
    };
    let bytes = {
        let mut chain = assets.chain.lock_recover();
        chain.read_file("Interface\\FrameXML\\ChatFrame.lua")
    };
    let src = match bytes {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            error!("ui_script: ChatFrame.lua read failed — emote commands will be dead: {e:#}");
            return;
        }
    };
    let table: Vec<&str> = src
        .lines()
        .map(str::trim)
        .filter(|l| is_emote_token_line(l))
        .collect();
    let count = table.len();
    if let Err(e) = script.run(&table.join("\n")) {
        error!("ui_script: emote token table failed to run: {e}");
        return;
    }
    // The sentinel is the command this whole seam exists for: EMOTE87 is `/sit`.
    let sentinel: Option<String> = script.lua().globals().get("EMOTE87_TOKEN").ok();
    match sentinel.as_deref() {
        Some("SIT") => info!("ui_script: {count} emote tokens loaded"),
        other => error!(
            "ui_script: emote token sentinel is {other:?}, not \"SIT\" ({count} lines) — \
             emote slash commands are broken"
        ),
    }
}

/// Is this line one of `ChatFrame.lua`'s emote-token assignments — `EMOTE<digits>_TOKEN = "<NAME>";`
/// with `NAME` in `[A-Z0-9_]`? The whole-line shape is the filter that makes running the matched
/// lines equivalent to reading data (no calls, no expressions, no side effects).
pub(crate) fn is_emote_token_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("EMOTE") else {
        return false;
    };
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let Some(rest) = rest[digits..].strip_prefix("_TOKEN = \"") else {
        return false;
    };
    let Some(name) = rest.strip_suffix("\";") else {
        return false;
    };
    digits > 0
        && !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// Feed synthetic `"player"`/`"target"` snapshots each frame (overriding the real feed, which finds
/// no avatar in a server-less capture) and fire the initial events once — proving the full chain
/// end-to-end on a screenshot: snapshot → `Unit*` bindings → Lua `OnEvent` → bars + text, for both
/// unit frames (the target one included, so captures regression-test its show-on-target path).
fn demo_unit_feed(script: Option<NonSendMut<UiScript>>, mut fired: Local<bool>) {
    /// The synthetic target's guid — a creature-family high part, so nothing mistakes it for a
    /// player. Only its *distinctness* matters.
    const DEMO_TARGET_GUID: u64 = 0xF130_0000_0000_0001;

    let Some(mut script) = script else {
        return;
    };
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
        // pushed count directly — no live server template needed in a capture).
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
        script.fire_event("PLAYER_XP_UPDATE", vec![]);
        for token in ["player", "target"] {
            script.fire_event("UNIT_HEALTH", vec![ScriptValue::Str(token.into())]);
        }
        script.fire_event("PLAYER_TARGET_CHANGED", vec![]);
        *fired = true;
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
mod addon_list_tests;

#[cfg(test)]
mod game_menu_tests;

#[cfg(test)]
mod macro_tests;

#[cfg(test)]
mod keybindings_tests;
#[cfg(test)]
mod options_tests;

#[cfg(test)]
mod delete_item_tests;

#[cfg(test)]
mod static_popup_tests;

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

#[cfg(test)]
mod quest_tests;

#[cfg(test)]
mod quest_timer_tests;

#[cfg(test)]
mod durability_tests;

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
mod bagnon_render_tests;

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
