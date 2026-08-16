//! **The UI session's lifecycle** — the VM's birth, its identity, its death, and the reload
//! (decisions 1051/1290/1291). Split from `mod.rs` when the reload verb landed and the file held
//! two concerns: this, and the per-frame extract/input bridge that stayed behind.
//!
//! The shape, end to end: `Startup` installs a **boot VM** (strings, emote tokens, fonts — the
//! state the glue screens run against); entering the world loads the in-game UI and every addon
//! onto it under the picked character's identity; leaving the world runs the reference's ordered
//! shutdown tail and installs a fresh boot VM; and `ReloadUI()` is the two edges back to back
//! without leaving the world. Every function here is an edge — nothing per-frame lives here.

use bevy::prelude::*;

use benilla_assets::LockRecover;
use benilla_ui::script::UiScript;

use super::{
    load_font_registry, load_ingame_ui, CursorPayloadHeld, PlayerUiHover, UiClock,
    UiKeyboardCapture,
};
use crate::ui_script::addons;

pub(crate) fn setup_script(world: &mut World) {
    install_boot_vm(world);
}

/// Build a **boot VM** and install it: a Lua state carrying the strings, the emote tokens and the
/// font-object registry, and no frames at all.
///
/// This is the state the client sits at outside the world — the login and character screens — and
/// the state a world entry loads the in-game UI *onto* ([`load_ingame_ui_on_world_entry`]). It is
/// installed twice over a session: once at `Startup`, and again at every [`end_ui_session`], which
/// is what makes a second login a genuine second load rather than a re-entry into the first
/// login's Lua state.
///
/// ONLY the font-object registry at boot (1051). The glyph atlas bakes once, on the first `Update`,
/// from `script.font_objects()` — and our native glue screens share that one atlas, so the registry
/// has to exist before the login screen or in-game text loses its outlined variants and its
/// registry-declared sizes for the whole session. The other 55 files are in-game UI and load at
/// world entry ([`load_ingame_ui`]); the reference splits at exactly this seam, with GlueXML
/// carrying its own `GlueFonts.xml`.
///
/// **Since 1290 this runs per login, and that is load-bearing rather than incidental.** The
/// reference re-loads `Fonts.xml` on every rebuild and *must*: `0x48fbf0`'s toc loop has no
/// already-loaded gate, and both halves of a font object die with the old session — the Lua handle
/// with `_G`, and the native `CSimpleFont` registry with the frame-script owner (`0x7839c0`, torn
/// down at `0x490c97`). Because [`end_ui_session`] routes every login through here, every session's
/// VM gets the registry and every `inherits="GameFontNormal"` in FrameXML resolves. A design that
/// dropped the VM and skipped index 0 on the in-game load would fail exactly there — verified in
/// wow-re's §5 for this change (1291).
fn install_boot_vm(world: &mut World) {
    let mut script = match UiScript::new() {
        Ok(s) => s,
        Err(e) => {
            error!("ui_script: VM init failed: {e}");
            world.remove_non_send_resource::<UiScript>();
            return;
        }
    };
    install_texture_resolvers(world, &mut script);
    load_global_strings(world, &script);
    load_emote_tokens(world, &script);
    if ui_wanted(world) {
        // Errors are already logged per file as they happen; the returned list is the test-side
        // assertion (`shipped_xml_tests`), not a second reporting channel.
        let _ = load_font_registry(&script);
    }
    world.insert_non_send_resource(script);
}

/// Wire up the two halves of `Interface\AddOns\` texture art (decision 1322): the sprite
/// decoder's **loose-file root** (so addon-shipped BLP/TGA files render at all — the store's
/// [`benilla_assets::WorldAssets::set_loose_sprite_root`]) and the VM's **texture probe** (so the
/// path form of `SetTexture` can answer the reference's 1|nil load verdict — Atlas picks its map
/// art by that return). Both resolve the same one folder ([`addons::root`], hermetic-`None` under
/// `$WOW_CAPTURE`), and the probe walks the same [`benilla_assets::sprite_candidates`] the
/// renderer decodes with, so the verdict the Lua caller gets is the verdict the screen shows.
///
/// No `WorldAssets` (no client data) means no backend: nothing to install, and the VM's path form
/// keeps answering nil — the engine-less truth.
fn install_texture_resolvers(world: &mut World, script: &mut UiScript) {
    let root = addons::root();
    let Some(mut assets) = world.get_resource_mut::<benilla_assets::WorldAssets>() else {
        return;
    };
    assets.set_loose_sprite_root(root.clone());
    let chain = assets.chain.clone();
    script.set_texture_probe(Box::new(move |path| {
        benilla_assets::sprite_candidates(path).iter().any(|c| {
            chain.lock_recover().contains(c)
                || root
                    .as_deref()
                    .is_some_and(|r| benilla_assets::loose_sprite_file(r, c).is_some())
        })
    }));
}

/// Does this run want the player UI at all? Captures stay pristine — their baselines regression-test
/// the WORLD render — unless `WOW_CAPTURE_UI=1` opts the UI in.
fn ui_wanted(world: &World) -> bool {
    !world.contains_resource::<crate::run_mode::CaptureMode>()
        || std::env::var("WOW_CAPTURE_UI").as_deref() == Ok("1")
}

/// The world-entry UI load, armed at `OnEnter(InWorld)` and run by [`run_pending_entry_load`]
/// once the loading cover is actually ON THE GLASS.
///
/// This is 0962's frame accounting, second offender: `OnEnter(InWorld)` fires on exactly the
/// frame whose render would first present the cover (the raise frame still renders the glue
/// above it), so a synchronous burst there holds the *previous* present — the frozen character
/// screen — for its whole duration. 0962 taught the menagerie to wait for
/// `WARM_COVER_PRESENT_FRAMES`; 1051 then moved the FrameXML load onto the same unprotected
/// frame, and by 1290+addons the burst had grown to ~0.5 s (measured live: raise 48.317 →
/// reference sourcing + 55 files + addons + saved vars + `PLAYER_LOGIN` → 48.83) — the
/// director's "frozen char for 1 sec" at every Enter World.
///
/// The counter counts covered+in-world frames, exactly like the warm pass: renders are serial,
/// so at [`ENTRY_LOAD_COVER_FRAMES`] the two intermediate frames' renders — which draw the
/// cover — have committed their presents before this frame's stall begins. The loading screen
/// folds this resource's existence into its clear condition, so a reveal can never precede the
/// UI it is supposed to reveal.
#[derive(Resource, Default)]
pub(crate) struct PendingEntryUiLoad {
    covered_frames: u32,
}

/// Covered+in-world frames before the entry load runs — the same present proxy, and the same
/// value, as `pipe_warm::WARM_COVER_PRESENT_FRAMES` (0962's argument, restated there).
const ENTRY_LOAD_COVER_FRAMES: u32 = 3;

/// `OnEnter(InWorld)`: arm the deferred entry load. The load itself runs a few frames later —
/// see [`PendingEntryUiLoad`].
pub(crate) fn arm_entry_ui_load(mut commands: Commands) {
    commands.insert_resource(PendingEntryUiLoad::default());
}

/// `PreUpdate` (chained after [`run_pending_reload`] — same exclusive slot, and a reload must
/// not interleave an armed entry load): run the armed entry load once the cover has presented.
///
/// No cover at all — a capture booting straight `InWorld`, or the screen's assets missing —
/// means there is no glass to protect and nothing watching the previous present: load
/// immediately. Leaving the world first (an instant disconnect) drops the latch unrun;
/// [`end_ui_session`] treats that as "the in-game UI never existed" and skips the shutdown
/// writes, so a UI-less VM can never clobber the saved-variables files with its emptiness.
pub(crate) fn run_pending_entry_load(world: &mut World) {
    if world.get_resource::<PendingEntryUiLoad>().is_none() {
        return;
    }
    let in_world = *world
        .resource::<State<crate::char_select::ClientState>>()
        .get()
        == crate::char_select::ClientState::InWorld;
    if !in_world {
        // Left the world before the load ran — nothing to build a UI for.
        world.remove_resource::<PendingEntryUiLoad>();
        return;
    }
    let covering = world
        .get_resource::<crate::loading_screen::LoadingScreen>()
        .is_some_and(|s| s.covering());
    if covering {
        let mut pending = world.resource_mut::<PendingEntryUiLoad>();
        pending.covered_frames += 1;
        if pending.covered_frames < ENTRY_LOAD_COVER_FRAMES {
            return;
        }
    }
    world.remove_resource::<PendingEntryUiLoad>();
    let start = std::time::Instant::now();
    load_ingame_ui_on_world_entry(world);
    // The standing instrument for this burst: the one number that says whether the cover is
    // still hiding it, on every entry, in every log.
    info!(
        "ui_script: in-game UI up in {:.0} ms (behind the cover: {})",
        start.elapsed().as_secs_f32() * 1000.0,
        covering
    );
}

/// Materialize the in-game UI for **this** session.
///
/// **Once per world entry, not once per process** (decision 1290). The reference builds the whole
/// in-game UI at `CGGameUI::Initialize 0x48fbf0` and destroys it again at `0x490bd0` on the way
/// out, so every login runs every addon's file scope afresh — and that file scope is where the
/// corpus reads the character it is looking at (`local currentPlayer = UnitName("player")`, the
/// idiom [`seat_from_roster`] already documents). While this was latched per process, a second
/// login kept the first login's captured name: the director's "the selected one is always
/// Onewarrior no matter what char I log into", and, worse, its saved variables went to the first
/// character's file. [`world_entry_tests`] holds both ends.
///
/// The load runs onto the fresh boot VM [`end_ui_session`] left behind (or, on the first login,
/// `Startup`'s) — so nothing here has to unpick the previous session; there is nothing to unpick.
///
/// Safe on the state edge only because 1038 moved the initial transition after `PostStartup` — a
/// capture boots straight into `InWorld`, so before that this would have run ahead of
/// [`benilla_assets::AssetSet::Open`] and loaded against no patch chain.
pub(crate) fn load_ingame_ui_on_world_entry(world: &mut World) {
    if !ui_wanted(world) {
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
    // The addon version gate (decision 1292): the live table when this VM has one, else the
    // persisted value. On a first login the boot VM was seeded during the glue phase, so a
    // *Load out of date AddOns* click at character select reads back here even inside the
    // save debounce; on a reload the fresh boot VM has no table yet and the persisted value is
    // current by construction (1291's session-edge fold ran moments ago). Absent both (a bare
    // test world) is the registrar default: check ON.
    let version_check = script
        .cvar("checkAddonVersion")
        .map(|v| v != "0")
        .unwrap_or_else(|| {
            world
                .get_resource::<crate::cvars::CvarPersist>()
                .is_none_or(crate::cvars::CvarPersist::addon_version_check)
        });
    let _ = load_ingame_ui(&mut script, identity.as_ref(), version_check);
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
    // The load edge is over: disarm the instruction bound `load_ingame_ui` installed (decision
    // 1306). From here every OnUpdate and event handler runs unhooked — a session must not kill
    // a player's addon for being slow; only a load that never returns is fair game.
    script.clear_instruction_budget();
    world.insert_non_send_resource(script);
    world.insert_resource(AddOnIdentity(identity));
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
pub(crate) fn seat_from_roster(
    roster: &crate::char_select::Roster,
) -> Option<benilla_ui::script::UnitState> {
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

/// The character the loaded AddOn enable state belongs to, remembered so the shutdown write goes
/// back to the file it came from — the roster's pick can be gone by then.
#[derive(Resource, Default)]
pub(crate) struct AddOnIdentity(pub(crate) Option<(String, String)>);

/// **The UI shutdown, in the reference's own order** — `0x490bd0`, whose ordered tail wow-5875-re
/// carves as (`system/ui/ui.md`):
///
/// > `PLAYER_LEAVING_WORLD` (273) → **`PLAYER_LOGOUT`** (271, `0x490c2a`) → `layout-cache.txt` →
/// > **the flat saved file** (`0x490c7e`) → **the per-addon files** (`0x490c83`) → `AddOns.txt`
/// > (`0x490c88`) → destroy the frame-script owner (`0x490c97`) → nil all 216 C bindings out of
/// > `_G` (`0x490cba` → `0x490ce0`)
///
/// **The last step is not "destroy the Lua state"** — which is what three of wow-re's own notes
/// said, until this client's teardown made the question load-bearing and a §5 cross-check settled
/// it (`system/ui/scratch/lua-state-lifecycle.md`; 1291). `0x490c97` is the frame-script owner's
/// scalar-deleting destructor — the widget tree and the native virtual-font registry — and the Lua
/// state outlives it. The state is closed and re-opened at `0x703b80`, which reaches `InitLua` by a
/// **tail-`jmp`** (`0x703b8e`) rather than a call, which is exactly why a call-census missed it.
/// Its three callers are `UI_Init 0x48fbf0` (at its *head*), `ShutdownGame 0x491180`, and the glue
/// builder `0x46a7b0` — so a logout/login cycle runs through four distinct states, and the glue
/// screen gets its own.
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
/// roots — and, with the writes done, **the end of this session's Lua state**.
///
/// The reference's own shutdown ends by destroying the state (`0x490bd0`'s tail, after
/// `AddOns.txt`); ours ends by replacing it with a fresh boot VM ([`install_boot_vm`]), which is the
/// same guarantee expressed the way our two-phase load wants it — the character screen still needs
/// a font-object registry for the shared glyph atlas, and the next login needs somewhere to load
/// onto. What matters is that **no frame, no global and no addon upvalue crosses this edge**: that
/// is what makes the next login a real login (decision 1290) instead of a re-entry into the
/// previous character's UI.
///
/// Exclusive rather than a `NonSendMut` system because it both drops and installs a `NonSend`, and
/// because the shutdown writes must be ordered against each other — see [`shutdown_ui_state`].
pub(crate) fn end_ui_session(world: &mut World) {
    // An armed-but-unrun entry load ([`PendingEntryUiLoad`]) means this session never built an
    // in-game UI: there are no globals to save and no addon state to write, and running the
    // shutdown tail against the boot VM would overwrite the real files with that emptiness.
    let ui_never_loaded = world.remove_resource::<PendingEntryUiLoad>().is_some();
    let identity = world
        .get_resource::<AddOnIdentity>()
        .and_then(|id| id.0.clone());
    if !ui_never_loaded {
        if let Some(mut script) = world.get_non_send_resource_mut::<UiScript>() {
            shutdown_ui_state(&mut script, identity.as_ref());
        }
    }
    // The CVar bridge (decision 1291): the dying VM's table folds into the persist state — after
    // the shutdown events above (a `PLAYER_LOGOUT` handler may `SetCVar`, and in the reference
    // that lands in an engine-side store that survives), before the VM is replaced. The next
    // VM's registration seeds from what this writes ([`crate::cvars`]'s saved base).
    crate::cvars::fold_dying_vm_cvars(world);
    world.insert_resource(AddOnIdentity(None));

    // **Everything the host is holding that came OUT of the dying VM goes with it.** A change memo
    // handles itself — it is keyed on [`benilla_ui::script::UiScript::session`] ([`VmMemo`]) — but
    // these are plain values other systems read through `Res<…>`, with no VM in hand to key
    // against, so the edge clears them. Each is a fact about a frame tree that is about to stop
    // existing: a hovered frame id, the minimap's extracted hole, a payload the cursor is carrying,
    // and the VM-relative clock the cooldown conversions run through.
    //
    // The two input latches were cleared by `char_select`'s logout and disconnect handlers, one
    // copy each. They belong here: the reason they need clearing is that `feed_ui_input` stops
    // running outside `InWorld`, which is this edge and nothing to do with *why* we left it.
    if let Some(mut hover) = world.get_resource_mut::<PlayerUiHover>() {
        hover.0 = None;
    }
    if let Some(mut keys) = world.get_resource_mut::<UiKeyboardCapture>() {
        keys.0 = false;
    }
    if let Some(mut held) = world.get_resource_mut::<CursorPayloadHeld>() {
        *held = CursorPayloadHeld::default();
    }
    if let Some(mut minimap) = world.get_resource_mut::<crate::minimap::MinimapWidget>() {
        minimap.0 = None;
    }
    if let Some(mut clock) = world.get_resource_mut::<UiClock>() {
        *clock = UiClock::default();
    }

    install_boot_vm(world);
}

/// A `ReloadUI()` waiting to run — set when [`crate::ui_logout`] drains
/// [`benilla_ui::script::SessionRequest::ReloadUi`], consumed by [`run_pending_reload`] at the
/// top of the next frame's `Update`.
///
/// A flag rather than an immediate call for the reference's own reason (`ds:0xb4b3f4`, its only
/// writer `0x491380` and its only reader the per-frame callback `0x495590`): the VM that queued
/// the request must not be mid-call when it is destroyed. Our drain already runs outside any VM
/// dispatch, but the flag keeps the whole rebuild at one point in the frame — before the input
/// pass — instead of wherever the drain happens to sit.
#[derive(Resource, Default)]
pub(crate) struct ReloadUiPending(pub(crate) bool);

/// Run a pending `ReloadUI()`: the reference's teardown/rebuild pair (`0x495664 call 0x490bd0`,
/// `0x495669 call 0x48fbf0`), which for us is [`end_ui_session`] then
/// [`load_ingame_ui_on_world_entry`] — the same two functions the logout/login edges run, called
/// back to back without leaving the world (decision 1291).
///
/// Everything that makes a login correct makes the reload correct **by construction**: the
/// shutdown tail fires `PLAYER_LEAVING_WORLD`/`PLAYER_LOGOUT` and writes the four files (so a
/// `DisableAddOn` staged in the dying VM reaches `AddOns.txt` before the rebuild reads it), the
/// rebuild is a real login's load (fresh file scope, saved variables, `VARIABLES_LOADED`,
/// `PLAYER_LOGIN`), and every host memory keyed on the VM's identity ([`VmMemo`], decision 1290)
/// expires with the old session id. `PLAYER_ENTERING_WORLD` refires from [`crate::ui_unit`]'s
/// feed once it notices the new VM, with the self descriptor already present — the reference's
/// own ordering, where the event follows the rebuild.
///
/// **In-world only.** At the glue there is no in-game UI to rebuild and no identity to load
/// addons under; the reference's own gate (`0x494a50(0xa)`) refuses there too. Dropped with a log
/// line rather than deferred — a reload asked for at the character screen answers nothing.
pub(crate) fn run_pending_reload(world: &mut World) {
    if !std::mem::take(&mut world.resource_mut::<ReloadUiPending>().0) {
        return;
    }
    let in_world = *world
        .resource::<State<crate::char_select::ClientState>>()
        .get()
        == crate::char_select::ClientState::InWorld;
    if !in_world {
        info!("ui_script: ReloadUI outside the world — dropped");
        return;
    }
    info!("ui_script: ReloadUI — ending the UI session and building a new one");
    end_ui_session(world);
    load_ingame_ui_on_world_entry(world);
}

/// `AppExit`: quitting the client — the quit / application-exit roots. Reads the message rather
/// than a state edge because a quit from in-world never leaves `InWorld`.
pub(crate) fn shutdown_on_exit(
    script: Option<NonSendMut<UiScript>>,
    id: Res<AddOnIdentity>,
    pending_entry: Option<Res<PendingEntryUiLoad>>,
    mut exits: MessageReader<AppExit>,
) {
    if exits.read().next().is_none() {
        return;
    }
    // Same guard as [`end_ui_session`]: a quit inside the entry-load window has no UI to save.
    if pending_entry.is_some() {
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
/// it immediately after — so it means "the UI came up". That is once per **UI build**, which since
/// 1290 is once per world entry here too: this runs from
/// [`load_ingame_ui_on_world_entry`], on the same edge that built the tree.
/// `PLAYER_ENTERING_WORLD` keeps its own per-entry latch in [`crate::ui_unit`] and still lands
/// after this, since it waits on the self descriptor arriving over the wire.
pub(crate) fn finish_ui_load(script: &mut UiScript) {
    // Still the load edge, so still bounded (1306) — re-armed because the walk's last addon left
    // an arbitrary amount on the counter, and the saved-variables chunk plus every PLAYER_LOGIN
    // handler deserve the full allowance. The entry edge disarms after this returns.
    script.set_instruction_budget(addons::LOAD_INSTRUCTION_BUDGET);
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
