//! The shipped FrameXML manifest and the three ways it is loaded.
//!
//! Split out of `ui_script/mod.rs` as its own concern: a 250-entry ordered data table plus the
//! loader that walks it. The interesting thing here is the **seam at index 0** — see
//! [`load_default_ui`].

use bevy::prelude::*;

use benilla_ui::script::UiScript;

/// The shipped FrameXML manifest, in load order. Index 0 (`Fonts.xml`) is the **font-object
/// registry** and is deliberately separable from the rest: it materializes zero frames, and the
/// glyph atlas bake ([`crate::ui_text`]) reads its objects to build the bake plan. Splitting the
/// manifest here is what lets the in-game UI load at world entry while the glue screens — which
/// share our one atlas — still get every (face, size, outline) combination they need (1051).
const UI_MANIFEST: &[&str] = &[
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
    // The combo-point dots (decision 0869) — anchored to BenillaTargetFrame, so after
    // UnitFrames.xml; its fade chain needs UiPanels.xml's UIFrameFade above.
    "ComboFrame.xml",
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
    // The two enchant-apply confirms + their event driver (decision 0928): the same StaticPopup
    // registry again. Not CraftFrame's — the gate that raises them is the generic item-target
    // bind, reached by a poison or a sharpening stone with no profession window open.
    "EnchantConfirm.xml",
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
    // The pet action bar (decision 0982): hidden until `crate::ui_pet`'s feed reports a bar,
    // i.e. until the server sends `SMSG_PET_SPELLS` for a live pet or charm — so a class that
    // never has one never sees it. Right after the stance bar, whose row it shares (either
    // one showing raises UIParent.xml's managed "pet" delta), and after Cooldown.xml
    // (CooldownFrame_SetTimer), ActionBar.xml (the `BenillaActionBar` anchor target) and
    // GameTooltip.xml (its hover).
    "PetActionBar.xml",
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
    // The mirror timers (decision 0874): the breath/fatigue bars at top-center, driven by the
    // MIRROR_TIMER_* events `crate::ui_mirror` fires. Needs only Fonts (GameFontHighlight)
    // before it; kept beside the cast bar, whose border art it shares.
    "MirrorTimer.xml",
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
    // The macro window (decision 0983): the editor + its name/icon popup, driven over the
    // engine's OWN macro table (`benilla_ui::script::macros` — 1.12 macros have no server
    // side, so this is the one window whose model is not an app feed). Needs UiPanels (the
    // panel manager + `TabButtonTemplate`), ScrollTemplates (BOTH kits — the real one scrolls
    // the body box, the faux one the icon grid), MicroMenu (`UpdateMicroButtons` on
    // show/hide), and it must come AFTER SpellBookFrame, whose shift-click reaches
    // `BenillaMacroFrame_AddMacroLine`. Before GameMenuFrame, whose Macros button opens it.
    "MacroFrame.xml",
    // The Keybindings page MODULE (decision 1008, superseding 0997's standalone window):
    // the templates + script of the Options window's Keybindings category, over the
    // engine's binding table (benilla_ui::script::keybind + crate::bindings — the 1.12
    // GetBinding/SetBinding Lua API). Needs UiPanels (popup engine) and GameTooltip (the
    // character-specific checkbox's hover); MUST load before OptionsFrame.xml, whose
    // Keybindings body inherits these templates and calls this script at OnLoad.
    "KeyBindingsPage.xml",
    // The options window (the 0985 provenance split: 0950's era structure, 0978's
    // 1.12-native art, 0981/0984's 1.14 chrome + working era search, 0989's directed
    // cuts — bare fully-live sliders, no corner X; 0992's dropdown rows + the Nameplates
    // page): the era-shaped OptionsFrame the game menu's Options button opens. Needs
    // Fonts' objects, UiPanels' panel manager, and UIDropDownMenu's capsule template
    // (all far above); sits right before GameMenuFrame so the menu — which reaches it by
    // name at click time — stays the last, outermost layer.
    "OptionsFrame.xml",
    // The game menu (decision 0674): the frame ESC opens, plus the CAMP/QUIT dialogs and their
    // event driver. LAST on purpose — it is the outermost layer of the shell, it depends on
    // everything it touches being loaded (UiPanels' popup engine + panel manager, MicroMenu's
    // UpdateMicroButtons, BagFrame's Disable_BagButtons), and nothing depends on it: the ESC
    // chain and the micro button both reach it by name at call time.
    "GameMenuFrame.xml",
];

/// Load a slice of [`UI_MANIFEST`] into the VM, in the order given. Returns per-file errors.
///
/// `bootstrap_positions` runs decision 0272's load-time `UIParent_ManageFramePositions()` pass —
/// only meaningful once the frames that table names exist, so the font-registry-only load
/// ([`load_font_registry`]) skips it. It is defined in `UIParent.xml`, which is in the deferred
/// half; calling it after `Fonts.xml` alone is a nil-global error, not a no-op.
fn load_ui_files(script: &UiScript, files: &[&str], bootstrap_positions: bool) -> Vec<String> {
    let mut failures = Vec::new();
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
    for file in files {
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
            failures.push(format!("{file}: {e}"));
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
    if bootstrap_positions {
        if let Err(e) = script.run("UIParent_ManageFramePositions()") {
            error!("ui_script: managed-positions bootstrap: {e}");
            failures.push(format!("managed-positions bootstrap: {e}"));
        }
    }
    failures
}

/// Load benilla's own default UI (`assets/ui/*.xml` — the unit frames and the action bar) through
/// the engine-free loader, resolving any `<Include>`/`<Script file=>` references against the
/// crate's `assets/ui` dir. This is our content (MIT/Apache), committed and read from source —
/// `CARGO_MANIFEST_DIR` (compile-time) points at this crate. Textures (`Interface\…`) still resolve at
/// render through the MPQ `sprite_texture` path; the loader only needs the XML/Lua text.
///
/// Returns every loader error, tagged `"<file>: <error>"` — the app ignores the value (each is
/// already logged as it happens) and [`shipped_xml_tests`] asserts it empty. The manifest is an
/// inline array no other test walks, so before that assertion a broken entry — a bad file name, a
/// frame that collides with a later window's, a template referenced before its definer — reached a
/// real run with nothing but a log line. Capture runs cannot cover it either: they skip this
/// function entirely unless `WOW_CAPTURE_UI=1`.
///
/// **Split across the boot boundary (1051).** `Fonts.xml` — index 0, zero frames materialized —
/// is the font-object registry the glyph atlas bakes its plan from, and our native glue screens
/// share that one atlas, so it must exist before the login screen. Everything after it is in-game
/// UI and loads at world entry ([`load_ingame_ui`]). This whole-manifest entry point stays for the
/// tests, which assert over the complete shipped set — production now loads in two phases, so this
/// has no non-test caller.
#[cfg(test)]
pub(crate) fn load_default_ui(script: &UiScript) -> Vec<String> {
    load_ui_files(script, UI_MANIFEST, true)
}

/// The font-object registry alone (`Fonts.xml`), loaded at `Startup` — see [`load_default_ui`].
///
/// Verified lossless for the atlas: the full manifest and this file alone both yield the **same 19
/// distinct `(font, height, outline)` combinations**. The three font objects defined outside it
/// (`GameFontNormalMed1` 13, `OptionsFontHighlightMedium` 14, `OptionsFontHighlightHuge` 20) are
/// un-outlined and their heights are already declared here, so they add nothing to the bake plan.
pub(crate) fn load_font_registry(script: &UiScript) -> Vec<String> {
    load_ui_files(script, &UI_MANIFEST[..1], false)
}

/// The in-game UI — everything after the font registry — loaded on entering the world.
///
/// The reference does the same at `CGGameUI::Initialize 0x48fbf0`, reached only from world entry
/// (`0x401570` ← `0x46c236`); its glue screens run GlueXML with their own `GlueFonts.xml` registry,
/// which is why the reference has no equivalent of our shared-atlas coupling (wow-5875-re, 1051).
pub(crate) fn load_ingame_ui(script: &UiScript) -> Vec<String> {
    load_ui_files(script, &UI_MANIFEST[1..], true)
}
