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

use crate::assets::LockRecover;
use crate::debug_panel::EguiPointerOver;
use crate::schedule::WorldStage;
use crate::ui_unit::UnitFeed;

mod extract;
mod input;

/// Is the pointer over *any* UI this frame — the egui dev overlay OR a mouse-enabled player-UI
/// frame? The single source of truth for "the mouse is talking to the UI, not the world",
/// combined by [`arbitrate_pointer_over_ui`] from both contributors (dev overlay =
/// [`crate::debug_panel::EguiPointerOver`]; player UI = [`PlayerUiHover`]). Gameplay reads it so
/// a drag doesn't start mouse-look; the inspector reads it so a pick doesn't fire behind an
/// overlaid frame. Owned HERE, not by the dev plugin (decision 0026): gameplay's read must
/// survive a build without the dev overlays, so the combiner treats the egui half as optional.
#[derive(Resource, Default)]
pub(crate) struct PointerOverUi(pub(crate) bool);

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

/// Run a Lua chunk, logging (never discarding) a failure. App systems drive the VM with fire-and-
/// forget chunks; `let _ = script.run(…)` swallows the error — the chat header machine died
/// mid-chunk on a missing `EditBox:SetTextColor` every single frame and nothing ever said so
/// (the /w caret bug). A chunk failure is always an app or engine defect; this is the mandatory
/// form for any run whose `Result` isn't otherwise consumed.
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
const DEFAULT_UI_SCALE: f32 = 0.9;

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
            .init_resource::<crate::hover_log::UiFrameCost>()
            .init_resource::<PointerOverUi>()
            .init_resource::<PlayerUiHover>()
            .init_resource::<UiKeyboardCapture>()
            .init_resource::<PlayerUiClickConsumed>()
            .init_resource::<CursorPayloadHeld>()
            // After `AssetSet::Open` so the patch chain exists at boot: the VM's first load is
            // the real `GlobalStrings.lua` (the reference's own FrameXML order), which the
            // cast-fail display (0427) resolves its messages from.
            .add_systems(Startup, setup_script.after(crate::assets::AssetSet::Open))
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
fn capture_ui_active(capture: Option<Res<crate::capture::CaptureMode>>) -> bool {
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
    // The unit frames are the default player UI — always loaded in a normal run. Captures stay
    // pristine (world-render baselines) unless WOW_CAPTURE_UI=1 opts the UI in.
    let capturing = world.contains_resource::<crate::capture::CaptureMode>();
    if !capturing || std::env::var("WOW_CAPTURE_UI").as_deref() == Ok("1") {
        load_default_ui(&script);
    }
    world.insert_non_send_resource(script);
}

/// Execute the real `Interface\FrameXML\GlobalStrings.lua` off the patch chain into the VM —
/// the reference boots FrameXML with exactly this file FIRST, and it is the source of every
/// localized string global the UI reads (the cast-fail display's whole message set, 0427).
/// Loaded before our own `assets/ui` files, matching the reference order. Failures are LOUD:
/// a silently missing GlobalStrings once suppressed every red error line (the 0427 fold's
/// absent-key face is faithful data suppression — but only when the file actually loaded).
fn load_global_strings(world: &mut World, script: &UiScript) {
    let Some(assets) = world.get_resource::<crate::assets::WorldAssets>() else {
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

/// Load benilla's own default UI (`assets/ui/*.xml` — the unit frames and the action bar) through
/// the engine-free loader, resolving any `<Include>`/`<Script file=>` references against the
/// crate's `assets/ui` dir. This is our content (MIT/Apache), committed and read from source —
/// `CARGO_MANIFEST_DIR` points at the `benilla` crate. Textures (`Interface\…`) still resolve at
/// render through the MPQ `sprite_texture` path; the loader only needs the XML/Lua text.
fn load_default_ui(script: &UiScript) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    // Provider for FrameXML/Lua references: try the path as given and by basename (Blizzard-style
    // backslash paths, dir-relative), resolved against our own assets/ui dir.
    let provider = |req: &str| -> Option<String> {
        let norm = req.replace('\\', "/");
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        std::fs::read_to_string(dir.join(&norm))
            .or_else(|_| std::fs::read_to_string(dir.join(base)))
            .ok()
    };
    for file in [
        // The named virtual Font objects (decision 0084) — must load first so any FontString's
        // `inherits="GameFontNormal"` resolves against a registered object.
        "Fonts.xml",
        // The UIPanel slot manager (decision 0084 §2): script-only, no frames of its own — must load
        // before any panel frame (Gossip/Merchant today) so ShowUIPanel/HideUIPanel already exist
        // when those frames' OnLoad/OnEvent reference them.
        "UiPanels.xml",
        // The managed bottom-stack positions (decision 0272): script-only —
        // UIPARENT_MANAGED_FRAME_POSITIONS + UIParent_ManageFramePositions, the ref
        // UIParent.lua mechanism that re-anchors the cast bar / chat over the bottom bars.
        // Loaded early so the stance bar's OnShow/OnHide can call it; applied once by the
        // post-load bootstrap below.
        "UIParent.xml",
        // The tooltip: a hover window the bag/merchant/loot/unit frames drive. Loaded before them so
        // `GameTooltip` + its verbs exist when their OnEnter handlers fire (runtime, so order
        // is not strictly required — but it keeps the dependency legible).
        "GameTooltip.xml",
        // The dropdown kit (0203 phase 2's closing sub-slice, widened for 0434 phase 5):
        // UIDropDownMenuTemplate + DropDownList1/2 + the UIDropDownMenu_* API — before its
        // first customers (the unit popups below, the world map's pickers, the trainer filter).
        "UIDropDownMenu.xml",
        // The unit right-click popups (0434 phase 5): UnitPopupMenus/Buttons + UnitPopup_ShowMenu
        // — after the dropdown kit it drives, before the unit/party frames whose DropDown
        // children's OnLoad initialize into it.
        "UnitPopup.xml",
        // The chat-link click router (SetItemRef): item links → ItemRefTooltip:SetHyperlink; a
        // player name → left-click whisper / right-click FRIEND dropdown. After GameTooltip.xml
        // (GameTooltip_OnLoad) AND the dropdown kit + UnitPopup — its FriendsDropDown inherits
        // UIDropDownMenuTemplate and opens a UnitPopup FRIEND menu.
        "ItemRef.xml",
        "UnitFrames.xml",
        // The center-screen scrolling combat text (decision 0578) — the Blizzard_CombatText
        // transcription; consumes COMBAT_TEXT_UPDATE + the regen/health/power events, needs
        // only Fonts.xml (CombatTextFont) + UIParent before it.
        "CombatText.xml",
        // The party member frames + the PARTY_INVITE popup + its event driver (decision 0434
        // phase 2): a StaticPopup registry entry, so after UiPanels.xml.
        "PartyFrame.xml",
        // The death arc's dialogs + event driver (decision 0308): registry entries on the shared
        // StaticPopup engine, so after UiPanels.xml.
        "DeathFrame.xml",
        // The two duel dialogs + their event driver (decision 0633): registry entries on the same
        // StaticPopup engine, and DUEL_OUTOFBOUNDS leans on its per-tick countdown branch.
        "DuelFrame.xml",
        // The shared CooldownFrame_SetTimer (the ref's Cooldown.lua file split) — before its
        // consumers (ActionBar's buttons, the multibars, the stance bar, BagFrame's slots).
        "Cooldown.xml",
        "ActionBar.xml",
        // The two always-on bottom multibars (actions 61-72 / 49-60): pure XML/Lua over
        // ActionBar.xml's shared button template + handler set — loads right after it
        // (inherits BenillaActionButtonTemplate, anchors to BenillaActionButton1, and its
        // buttons join the BENILLA_ACTION_BUTTONS roster ActionBar.xml declares).
        "MultiBars.xml",
        // The stance/shapeshift bar: hidden until `crate::ui_shapeshift`'s feed pushes a
        // non-empty form list (a formless class never shows it). After Cooldown.xml
        // (CooldownFrame_SetTimer) + ActionBar.xml (BENILLA_FALLBACK_ICON, the BenillaActionBar
        // anchor target).
        "StanceBar.xml",
        // The micro-button row in the bar's right-hand recess (the ref's own file split, its TOC
        // loads MainMenuBarMicroButtons right after MainMenuBar). After ActionBar.xml: it anchors
        // into BenillaActionBarArtFrame by name and seats itself with that file's
        // BenillaActionBarArt_SeatAbove. It must come BEFORE the panels it toggles, because each of
        // their OnShow/OnHide calls the `UpdateMicroButtons` this file defines (the panel frames
        // themselves are read back through getglobal, so the reverse dependency is call-time).
        "MicroMenu.xml",
        // The HUD minimap cluster (decision 0203 phase 1): the chrome around the engine-rendered
        // <Minimap> widget hole (`crate::minimap` fills it). After GameTooltip (its zone-text
        // hover uses the shared tooltip).
        "MinimapCluster.xml",
        // The zone-entry splash (decision 0287): ZoneTextFrame/SubZoneTextFrame + the inlined
        // FadingFrame kit, driven by the ZONE_CHANGED family `crate::area` fires. Needs only
        // Fonts (ZoneTextFont/SubZoneTextFont) + UIParent; kept by the minimap cluster — the
        // other consumer of the zone-text host globals.
        "ZoneText.xml",
        // The faux-scroll kit (decisions 0247/0250/0251): BenillaScrollBarTemplate (the draggable
        // Slider scroll bar) + BenillaFauxScrollFrameTemplate + the BenillaFauxScrollFrame_* API —
        // before its first customer (the trainer list) below.
        "ScrollTemplates.xml",
        // The fullscreen world map (decision 0203 phase 2), over the worldmap host API
        // (`crate::ui_world_map` feeds it); the 'M' binding below calls its ToggleWorldMap().
        "WorldMapFrame.xml",
        // The cast bar (decision 0137 phase 1) — the extracted 1.12 CastingBarFrame, driven by
        // the SPELLCAST_* events `crate::ui_cast` fires. Only needs Fonts before it.
        "CastingBar.xml",
        // The player buff/debuff bar (decisions 0255/0257 — the player aura arc): the HUD row of
        // aura icons under the minimap, driven by the UnitAura feed (`crate::ui_aura`). Needs Fonts
        // (its FontStrings) and ActionBar's `BENILLA_FALLBACK_ICON` global (both already loaded).
        "BuffFrame.xml",
        // The durability alert (the "armor guy"): under the minimap beside BuffFrame, driven by
        // UPDATE_INVENTORY_ALERTS off the same inventory push the character window reads. Needs
        // MinimapCluster (its anchor) — loaded above.
        "DurabilityFrame.xml",
        "ErrorsFrame.xml",
        "BagFrame.xml",
        // The stack-split spinner (decision 0216 §6/slice 2): loaded beside BagFrame, whose slot
        // click handler (`BenillaBagSlot_OnClick`) calls `OpenStackSplitFrame`. Order between the
        // two doesn't matter for correctness (Lua globals resolve at call time, and neither file's
        // XML `inherits=` reaches into the other's templates) — kept adjacent for legibility.
        "StackSplit.xml",
        // The character window (decision 0208 phase 1a): the C-key paper doll. Loaded after BagFrame
        // (needs nothing from it, but keeps player-state windows grouped) and before the NPC session
        // windows (Gossip/Merchant/Quest*) — it depends only on Fonts/UiPanels/GameTooltip, all
        // already loaded above.
        "CharacterFrame.xml",
        // The Skills tab (decision 0437 phase 4): a CharacterFrame child page, but a separate
        // top-level file (SkillFrame.xml's own header comment — this engine has no cross-file
        // `parent=` attachment, so it's positioned by name via `<Anchors relativeTo=
        // "BenillaCharacterFrame">` instead). Loaded immediately after CharacterFrame.xml so it
        // paints ON TOP of it (later file = higher z) and so `BenillaCharacterFrame` already exists
        // for that anchor to resolve against.
        "SkillFrame.xml",
        // The inspect window (decision 0631): another player's paper doll. Beside the character
        // window it mirrors — it needs the same three (Fonts/UiPanels/GameTooltip) plus
        // `CharacterFrameTabButtonTemplate` from UiPanels.xml, and nothing from CharacterFrame.xml
        // itself (its slot template, handlers, and booth slot are all its own — the two windows
        // share only the reference's art).
        "InspectFrame.xml",
        // The spellbook window (decision 0216 §8, slice 5): the P-key window, the spell SOURCE
        // for the cursor-payload arc. Loaded right after CharacterFrame (the same "player-state
        // windows grouped" posture, same Fonts/UiPanels/GameTooltip-only dependency set); the
        // 'P' binding below calls its bare ToggleSpellBook(BOOKTYPE_SPELL).
        "SpellBookFrame.xml",
        // The talent window (decision 0304 §4): the N-key window, the class talent grid over the
        // `benilla-ui::script::talent` engine seam. Loaded right after SpellBookFrame (the same
        // "player-state windows grouped" posture; needs Fonts/UiPanels/GameTooltip plus
        // ScrollTemplates.xml — its real ScrollFrame's scrollbar reuses BenillaScrollBarTemplate —
        // all already loaded above). The 'N' binding (ui_script/input.rs) calls its bare
        // ToggleTalentFrame().
        "TalentFrame.xml",
        // The social window (decision 0668): the O-key friends/ignore/who window over the social
        // API (`benilla-ui/src/script/social.rs`) + `crate::ui_social`'s feed/drain. Needs
        // UiPanels (the panel manager, `CharacterFrameTabButtonTemplate`, and the StaticPopup
        // engine its two name-entry dialogs register into), ScrollTemplates (three faux lists),
        // GameTooltip (the newbie tips) and UIDropDownMenu (the who list's variable column) —
        // all loaded above.
        "FriendsFrame.xml",
        "GossipFrame.xml",
        "MerchantFrame.xml",
        // The mail window (decision 0544 P1/P2): the mailbox inbox, open-letter, and send tab over
        // the Era mail API (`benilla-ui/src/script/mail.rs`) + `crate::ui_mail`'s feed/drain. Loaded
        // right after MerchantFrame because it REUSES that file's global BenillaMoney_Set/_Clear
        // coin helpers (postage/enclosed/COD displays) — they must be defined first.
        "MailFrame.xml",
        // The player-to-player trade window (decision 0592 P1): the two-sided offer over the trade
        // API (`benilla-ui/src/script/trade.rs`) + `crate::ui_trade`'s feed/drain. Loaded after
        // MerchantFrame because it REUSES that file's global BenillaMoney_Set coin helpers (the two
        // gold displays) — they must be defined first.
        "TradeFrame.xml",
        // The item-text reader: letters (mail-made permanent copies) and, later, books/plaques —
        // the ItemTextGet* seam (`benilla-ui/src/script/item_text.rs`) over `crate::ui_item_text`'s
        // feed/drain. Loaded right after MailFrame (the same arc; needs only Fonts/UiPanels, both
        // loaded above).
        "ItemTextFrame.xml",
        // The trainer window (decision 0237 phase 3): the read-only class/profession trainer list.
        // Loaded right after MerchantFrame per the director's brief (kept adjacent even though this
        // v1 uses plain-text costs, not MerchantFrame's coin helpers) and before the other NPC
        // session windows below.
        "TrainerFrame.xml",
        // The bank window (decision 0604 phase 4): the vault, purchase ladder, and the 6 bank-bag
        // popouts, over the C_Container feed already carrying container -1/5..10 (ui_items.rs) +
        // the bank Lua surface (benilla-ui/src/script/bank.rs) + crate::ui_bank's feed/drain.
        // Loaded AFTER MerchantFrame.xml (reuses its BenillaMoney_* coin helpers for the purchase
        // row + purse) and AFTER BagFrame.xml, already above (its BenillaBagSlotTemplate/
        // BenillaBagWindowTemplate + the BenillaBagSlot_*/BenillaBagFrame_* function family are
        // reused verbatim — the 24 generic slots and the 6 bank-bag popouts are, respectively, a
        // container-slot grid and ordinary container windows, exactly like the bag arc's own).
        // Kept in the "NPC session windows grouped" cluster beside Trainer/Taxi.
        "BankFrame.xml",
        // The taxi-map window (decision 0484 phase 2): the flight-master session, driven by the
        // taxi engine seam (`benilla-ui/src/script/taxi.rs`) over `crate::ui_taxi`'s feed/drain.
        // Loaded right after TrainerFrame per the same "NPC session windows grouped" posture — it
        // needs only Fonts/UiPanels/GameTooltip (SetTooltipMoney), both already loaded above.
        "TaxiFrame.xml",
        // The crafting window (decision 0437 phase 2): NOT an NPC session (opens off your own cast of
        // an effect-47 opener spell, TradeSkillFrame.xml's own header comment) — loaded right after
        // TrainerFrame per the same "player-state/profession window" grouping and because it depends
        // on nothing TrainerFrame doesn't already need (Fonts/UiPanels/GameTooltip/ScrollTemplates,
        // all loaded above).
        "TradeSkillFrame.xml",
        // The Enchanting window (decision 0437 phase 3): the exact twin of TradeSkillFrame above —
        // same "opens off your own cast, not an NPC session" shape (CraftFrame.xml's own header
        // comment) — loaded right after it so `BuildColoredListString` (a guarded GLOBAL
        // TradeSkillFrame.xml defines) already exists when CraftFrame.xml reuses it without
        // redefining (CraftFrame.xml's own header comment, deviation 2).
        "CraftFrame.xml",
        "LootFrame.xml",
        // The group-loot roll popups (decision 0591): the Need/Greed/Pass dialogs, a sibling of
        // LootFrame.xml (loaded right after it) over the `benilla-ui::script::loot_roll` seam
        // (`crate::ui_loot_roll`'s feed/drain). NOT a UIPanel — four `frameStrata="DIALOG"` toplevel
        // popups with their own hardcoded anchors — so it needs only Fonts (GameFontNormalSmall) +
        // GameTooltip (the item/PASS/NEED/GREED hovers), both already loaded above.
        "GroupLootFrame.xml",
        // The questgiver window (decision 0088): four sub-panels over the Era quest API. Loaded after
        // MerchantFrame (the BenillaMoney coin helpers live there) and GossipFrame (shared parchment
        // art conventions); its title/panel-button labels use the auto-centred ButtonText slot.
        "QuestFrame.xml",
        // The quest log window (decision 0088 arc, the quest-log slice): durable player state, not an
        // NPC session — loaded after MerchantFrame (the BenillaMoney coin helpers it reuses) and
        // alongside QuestFrame (same arc); the 'L' binding below calls its ToggleQuestLog().
        "QuestLogFrame.xml",
        "ChatFrame.xml",
        // The game menu (decision 0674): the frame ESC opens, plus the CAMP/QUIT dialogs and their
        // event driver. LAST on purpose — it is the outermost layer of the shell, it depends on
        // everything it touches being loaded (UiPanels' popup engine + panel manager, MicroMenu's
        // UpdateMicroButtons, BagFrame's Disable_BagButtons), and nothing depends on it: the ESC
        // chain and the micro button both reach it by name at call time.
        "GameMenuFrame.xml",
    ] {
        let entry = dir.join(file);
        let text = match std::fs::read_to_string(&entry) {
            Ok(t) => t,
            Err(e) => {
                error!("ui_script: reading {}: {e}", entry.display());
                continue;
            }
        };
        let doc = match benilla_ui::framexml::parse(&text) {
            Ok(d) => d,
            Err(e) => {
                error!("ui_script: parsing {file}: {e}");
                continue;
            }
        };
        let report = benilla_ui::loader::load(script, &doc, &provider);
        for w in &report.warnings {
            warn!("ui_script({file}): {w}");
        }
        for e in &report.errors {
            error!("ui_script({file}): {e}");
        }
        info!(
            "ui_script: {file} loaded ({} frames materialized)",
            report.frames
        );
    }

    // The managed positions' startup pass (decision 0272): the ref applies
    // UIPARENT_MANAGED_FRAME_POSITIONS once at load, then re-fires from the bottom bars'
    // OnShow/OnHide. Every frame the table names now exists, so this is that load-time
    // application; the stance bar's show/hide handles the rest at runtime.
    if let Err(e) = script.run("UIParent_ManageFramePositions()") {
        error!("ui_script: managed-positions bootstrap: {e}");
    }
}

/// Feed synthetic `"player"`/`"target"` snapshots each frame (overriding the real feed, which finds
/// no avatar in a server-less capture) and fire the initial events once — proving the full chain
/// end-to-end on a screenshot: snapshot → `Unit*` bindings → Lua `OnEvent` → bars + text, for both
/// unit frames (the target one included, so captures regression-test its show-on-target path).
fn demo_unit_feed(script: Option<NonSendMut<UiScript>>, mut fired: Local<bool>) {
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
            ..Default::default()
        }),
    );
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
mod combat_text_tests;

#[cfg(test)]
mod unit_frame_tests;

#[cfg(test)]
mod unit_popup_tests;

#[cfg(test)]
mod action_bar_tests;

#[cfg(test)]
mod multibar_stance_tests;

#[cfg(test)]
mod micro_menu_tests;

#[cfg(test)]
mod perf_bar_tests;

#[cfg(test)]
mod panel_tests;

#[cfg(test)]
mod merchant_tests;

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

#[cfg(test)]
mod escape_tests;

#[cfg(test)]
mod game_menu_tests;

#[cfg(test)]
mod delete_item_tests;

#[cfg(test)]
mod static_popup_tests;

#[cfg(test)]
mod death_tests;

#[cfg(test)]
mod duel_tests;

#[cfg(test)]
mod friends_tests;

#[cfg(test)]
mod quest_tests;

#[cfg(test)]
mod durability_tests;

#[cfg(test)]
mod questlog_tests;

#[cfg(test)]
mod character_tests;

#[cfg(test)]
mod inspect_tests;

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
