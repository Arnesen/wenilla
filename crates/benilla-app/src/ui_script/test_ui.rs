//! **The tests' interface loader — one copy, both stores** (decision 1751).
//!
//! Half a dozen test files carried an identical private `load_xml` that read
//! `assets/ui/<name>` off disk, parsed it, and asserted the loader reported no errors — each with
//! the comment "duplicated so this file is self-contained". That was fine while `assets/ui` was
//! the only place an interface file could come from. It is not any more: a migrated window's file
//! lives in the player's own patch chain, so a test that wants bag windows has to name
//! `Interface\FrameXML\ContainerFrame.xml`, and six copies of a disk-only reader cannot say that.
//!
//! [`load_ui`] is that reader, generalised the same way [`super::manifest::load_manifest`] is:
//! **a bare filename is a file we ship, a path is one off the player's install.** The two are told
//! apart by [`super::reference_ui::is_chain_entry`], the manifest's own rule, so a test loads the
//! entries in the order `benilla.toc` names them and gets what the client gets.
//!
//! It reads the SOURCE TREE rather than the compiled-in copy, deliberately: these tests exist to
//! catch a mistake in a file somebody just edited, and `content::read`'s dev-build probe already
//! prefers disk for the same reason.

use benilla_ui::script::UiScript;

/// Load one interface file into `s`, panicking on any loader error, and return how many frames it
/// materialized (`0` for a `.lua` entry, which materializes none).
///
/// `entry` is a manifest entry: `"BagFrame.xml"` for one of ours, or
/// `"Interface\\FrameXML\\ContainerFrame.xml"` for the reference's own — which also brings its
/// `<Script file="ContainerFrame.lua"/>` off the chain, exactly as it does in a real load.
///
/// **A chain entry needs client data**, so a test that names one has to open with
/// `benilla_formats::wow_data_or_skip!()`; [`BAG_UI`] is a list that always does.
pub(super) fn load_ui(s: &UiScript, entry: &str) -> usize {
    load_entry(s, entry, false, false)
}

/// [`load_ui`], and **a missing template is a failure too**.
///
/// A frame that inherits a template nothing declares is a loader *warning*, not an error: the frame
/// is built and simply has none of the template's art. So an under-loaded dependency list passes
/// [`load_ui`] and then loses a window's whole skin silently — which is why four of the social
/// windows' test modules grew this check by hand. It is one function now rather than four copies,
/// and any test may ask for it.
pub(super) fn load_ui_strict(s: &UiScript, entry: &str) -> usize {
    load_entry(s, entry, true, false)
}

/// [`load_ui`], and **no loader warning of any kind is tolerated**.
///
/// Stricter than [`load_ui_strict`], which only fails a missing template. A file whose own
/// assignment is "this loads perfectly clean" wants the whole warnings channel asserted empty — a
/// stale unknown-attribute or dropped-script warning is exactly the drift that check exists to
/// catch. It was a fourth private disk-only reader in `group_loot_tests.rs` until 1838; a
/// disk-only reader cannot name a chain file, which is what that test now loads.
pub(super) fn load_ui_no_warnings(s: &UiScript, entry: &str) -> usize {
    load_entry(s, entry, false, true)
}

fn load_entry(s: &UiScript, entry: &str, strict_templates: bool, no_warnings: bool) -> usize {
    let path = entry.replace('\\', "/");
    let bytes = read(&path).unwrap_or_else(|| panic!("{entry}: not found"));
    if path.to_ascii_lowercase().ends_with(".lua") {
        s.run_chunk_named(&bytes, &format!("@{entry}"))
            .unwrap_or_else(|e| panic!("{entry}: {e}"));
        return 0;
    }
    let doc = benilla_ui::framexml::parse(&benilla_ui::source::decode(&bytes))
        .unwrap_or_else(|e| panic!("{entry}: {e}"));
    let provider = |req: &str| -> Option<Vec<u8>> { read(req) };
    let report = benilla_ui::loader::load_in(s, &doc, &path, &provider);
    assert!(
        report.errors.is_empty(),
        "{entry}: loader errors: {:?}",
        report.errors
    );
    if no_warnings {
        assert!(
            report.warnings.is_empty(),
            "{entry}: loader warnings: {:?}",
            report.warnings
        );
    }
    if strict_templates {
        let missing: Vec<&String> = report
            .warnings
            .iter()
            .filter(|w| w.contains("unknown template"))
            .collect();
        assert!(
            missing.is_empty(),
            "{entry}: inherits a template this house does not ship (the frame loads, its ART does \
             not): {missing:?}"
        );
    }
    report.frames
}

/// One file's bytes, from whichever store its path names — the chain for a path, this crate's
/// `assets/ui` for a bare name. Also the `<Include>` / `<Script file=>` provider, which is why it
/// takes an already-resolved path in either space.
fn read(req: &str) -> Option<Vec<u8>> {
    if super::reference_ui::is_chain_entry(req) {
        return super::reference_ui::read(req);
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    std::fs::read(dir.join(req)).ok()
}

/// The files a test needs before it can open a bag window, in manifest order: the four templates
/// stock `ContainerFrame.xml` inherits, then the reference's own file, then our bag bar.
///
/// Named as a list rather than folded into a `setup()` because the callers all want to add their
/// own files around it (a merchant, the bank, the action bar art), and the ORDER is the thing
/// being reused — it is `benilla.toc`'s, trimmed to what the bags actually reach for.
/// What the **loot window** needs before `Interface\FrameXML\LootFrame.xml` will load and behave —
/// the same shape as [`BAG_UI`], for the same reason, and it grew for 1751 exactly as that one did.
///
/// Three of these are load-bearing in a way that is invisible if you leave them out:
///
/// * **`GlobalStrings.lua`** — the stock file concatenates `GROUP` and `GIVE_LOOT` while building
///   the master-loot menu. The app loads the player's own copy ahead of the whole manifest at VM
///   setup; a bare `UiScript::new()` does not. Our deleted `LootFrame.xml` carried
///   `X = X or "…"` fallbacks for precisely these; the stock file carries none.
/// * **`ItemButtonTemplate.xml`** — stock `LootButtonTemplate` inherits it, and a missing template
///   is a loader *warning*, not an error. Leave it out and every row loads clean with no art and
///   no `$parentIconTexture`, which reads as a pass until an assertion looks for an icon.
/// * **`PartyFrame.xml`** — for `MAX_PARTY_MEMBERS`, which stock `LootFrame.lua:217` does
///   arithmetic on at LOAD time, not at click time. Its home is the reference's own
///   (`PartyMemberFrame.lua:1`), and it wants the dropdown kit and `UnitPopup` ahead of it — the
///   manifest's order (167 → 185 → 262), reproduced rather than short-circuited. Setting the
///   constant by hand would pass and teach nothing about the real load.
///
/// Needs client data, like [`BAG_UI`]: open with `benilla_formats::wow_data_or_skip!()`.
/// What the **vendor window** needs before `Interface\FrameXML\MerchantFrame.xml` will load and
/// behave — the same shape as [`BAG_UI`] and [`LOOT_UI`], and it grew for 1751 the same way.
///
/// The two that fail in the ways worth naming:
///
/// * **`BasicControls.xml`** — for `TEXT()`, the reference's identity-function wrapper, which
///   stock `MerchantFrame.lua:70` calls while building every row. Absent, it raises on the first
///   `MerchantFrame_UpdateMerchantInfo`, which is the first thing the window does when it shows.
/// * **`Interface\FrameXML\ItemButtonTemplate.xml`** — the stock rows' `$parentItemButton`
///   inherits it, and a missing template is a loader *warning*: the rows load clean with no art.
///
/// Needs client data: open with `benilla_formats::wow_data_or_skip!()`.
pub(super) const MERCHANT_UI: &[&str] = &[
    "Interface\\FrameXML\\GlobalStrings.lua",
    "Fonts.xml",
    "BasicControls.xml", // TEXT()
    "Interface\\FrameXML\\ItemButtonTemplate.xml",
    "MoneyFrame.xml",
    "UiPanels.xml",
    "GameTooltip.xml", // app load order: tooltip before merchant
];

pub(super) const LOOT_UI: &[&str] = &[
    "Interface\\FrameXML\\GlobalStrings.lua",
    "Fonts.xml", // ITEM_QUALITY_COLORS — the row-name palette
    "Interface\\FrameXML\\ItemButtonTemplate.xml",
    "MoneyFrame.xml",
    "UiPanels.xml", // StaticPopup, and the LOOT_BIND / CONFIRM_LOOT_DISTRIBUTION dialogs
    "GameTooltip.xml", // TOOLTIP_DEFAULT_COLOR, read by the dropdown backdrop
    "Interface\\FrameXML\\UIDropDownMenu.xml", // GroupLootDropDown's OnLoad calls UIDropDownMenu_Initialize
    "UnitPopup.xml",
    // …and what its rows' OnLoad calls: every `PartyMemberFrame<N>` and its pet frame runs
    // `UnitFrame_Initialize`, which lives in UnitFrame.lua and itself calls
    // `SetTextStatusBarText` out of TextStatusBar.lua. Naming PartyFrame without these loads
    // four rows that each raise on their own OnLoad — the loader reports it, but only because
    // `load_ui_strict` looks; a plain load would have gone quiet.
    "Interface\\FrameXML\\TextStatusBar.lua",
    "Interface\\FrameXML\\TextStatusBar.xml",
    "Interface\\FrameXML\\UnitFrame.xml",
    // …and `RefreshBuffs`, which `PartyMemberFrame.lua:60` calls from each row's OnLoad. Its home
    // is BuffFrame.lua, which the chain's BuffFrame.xml sources — the reference's own toc has
    // BuffFrame at 40 and PartyFrame at 45 for exactly this reason.
    "Interface\\FrameXML\\BuffFrame.xml",
    "Interface\\FrameXML\\PartyFrame.xml",
];

/// What the **character window** needs before `Interface\FrameXML\CharacterFrame.xml`,
/// `PaperDollFrame.xml` and `PetPaperDollFrame.xml` will load and behave — the same shape as
/// [`BAG_UI`] / [`LOOT_UI`] / [`MERCHANT_UI`], and grown the same way (decision 1751).
///
/// This one is the longest of the four, and the reason is `CharacterFrame_OnLoad`: it is the only
/// migrated window whose LOAD-time body reaches outside its own file, and it reaches into four
/// others at once —
///
/// * **`Interface\FrameXML\TextStatusBar.lua`** for `SetTextStatusBarTextPrefix`, which it calls
///   three times before doing anything else. Its `.xml` twin comes too, because the pet page's XP
///   bar inherits the `TextStatusBar` template it declares and a missing template is a loader
///   *warning* — the bar would load with no art and no text region and read as a pass.
/// * **`Interface\FrameXML\PlayerFrame.xml`** for `PlayerFrameHealthBar` / `PlayerFrameManaBar`,
///   the frames those three calls name, and **`PetFrame.xml`** for `PetFrameHealthBar` /
///   `PetFrameManaBar`, which `CharacterFrame_OnShow` shows the text on. Both call
///   `UnitFrame_Initialize` in their OnLoad and `PlayerFrame` also calls `CombatFeedback_Initialize`,
///   so `UnitFrame.xml` and `CombatFeedback.xml` come first — the reference's own toc order.
///   (This entry used to be our one `UnitFrames.xml`, and this note used to say "when they are
///   migrated, this becomes the stock pair". They are; it did.)
/// * **`ActionBar.xml`** for `MainMenuExpBar` — the third bar of that prefix call — and for
///   `ShowWatchedReputationBarText` / `HideWatchedReputationBarText`, which the window's
///   show/hide pair calls.
/// * **`UIPanelTemplates.xml`** for `PanelTemplates_SetNumTabs`/`_SetTab`, the last two lines of
///   that same OnLoad.
///
/// And two more that only bite later, which is exactly why they are written down:
///
/// * **`Interface\FrameXML\HonorFrame.xml`** — `PaperDollFrame_SetLevel` and `_SetGuild` write
///   `HonorLevelText` and `HonorGuildText` "while we're at it" (`PaperDollFrame.lua:100`/`:120`).
///   Nothing touches them at load, so leaving this out loads clean and then raises the first time
///   the window is SHOWN. It sits below the character block in the manifest for the same reason.
/// * **`MicroMenu.xml`** — `UpdateMicroButtons` (called by both `CharacterFrame_OnShow` and
///   `_OnHide`) and `MicroButtonTooltipText` (all five tab hovers). A tab hover is the only thing
///   that reaches the second one, so its absence is invisible until a test hovers a tab.
///
/// Needs client data, like its three siblings: open with `benilla_formats::wow_data_or_skip!()`.
pub(super) const CHARACTER_UI: &[&str] = &[
    // The reference's own localized strings. The stock file has no `X = X or "…"` fallbacks —
    // `PaperDollFrame_OnLoad` sets seven labels from them at LOAD, and `PaperDollFrame_SetStats`
    // concatenates `SPELL_STAT0_NAME`..`4` on every repaint.
    "Interface\\FrameXML\\GlobalStrings.lua",
    "Fonts.xml",
    "BasicControls.xml", // TEXT(), which every one of those label sets goes through
    "Interface\\FrameXML\\ItemButtonTemplate.xml", // PaperDollItemSlotButtonTemplate's base
    "MoneyFrame.xml",
    "UIParent.xml", // Model_OnLoad/_Rotate*/_OnUpdate — the model panes' turntable
    "UiPanels.xml", // CharacterFrameTabButtonTemplate, UIPanelWindows, Show/HideUIPanel
    "GameTooltip.xml",
    "Cooldown.xml", // CooldownFrameTemplate + CooldownFrame_SetTimer, per equipment slot
    "UIPanelTemplates.xml",
    // The unit frames' four right-click dropdowns call `UIDropDownMenu_Initialize` at LOAD, so the
    // kit and the menu table it initialises from both precede them — the manifest's own order
    // (175 → 189 → 193 → 263).
    "Interface\\FrameXML\\UIDropDownMenu.xml",
    "Interface\\FrameXML\\UIMenu.xml",
    "UnitPopup.xml",
    "Interface\\FrameXML\\TextStatusBar.lua",
    "Interface\\FrameXML\\TextStatusBar.xml",
    // `UnitFrame_Initialize` and `CombatFeedback_Initialize`, which the two windows below call in
    // their OnLoad. Neither file declares a frame; both are pure script.
    "Interface\\FrameXML\\UnitFrame.xml",
    "Interface\\FrameXML\\CombatFeedback.xml",
    "Interface\\FrameXML\\PlayerFrame.xml",
    // `PetFrame.xml`'s four `PetFrameBuff*` inherit `PartyBuffButtonTemplate`, which lives here.
    // The manifest never names this file: it arrives through `PartyFrame.xml`'s
    // `<Include file="PartyFrameTemplates.xml"/>`, and the reference's own toc puts PartyFrame (45)
    // ahead of TargetFrame (46) and PetFrame (47) for exactly that reason. This kit wants the
    // template and not four party member frames, so it takes the included file directly.
    "Interface\\FrameXML\\PartyFrameTemplates.xml",
    "Interface\\FrameXML\\PetFrame.xml",
    "ActionBar.xml",
    "MicroMenu.xml",
    // The two page files these three need before the window can be OPENED, which is not the same
    // as before it can load: `CHARACTERFRAME_SUBFRAMES` lists all five pages by name and
    // `CharacterFrame_ShowSubFrame` calls `getglobal(value):Hide()` on each one it is not showing
    // (`CharacterFrame.lua:25-32`), unguarded. A missing page is `attempt to index a nil value` on
    // the very first `ToggleCharacter` — load-clean, then dead on the first click. Their own
    // template dependencies come with them.
    "ScrollTemplates.xml",       // SkillFrame's faux list + trough
    "OptionsFrameTemplates.xml", // ReputationFrame's detail check boxes
    "Interface\\FrameXML\\CharacterFrame.xml",
    "Interface\\FrameXML\\PaperDollFrame.xml",
    "Interface\\FrameXML\\PetPaperDollFrame.xml",
    "ReputationFrame.xml",
    "SkillFrame.xml",
    "Interface\\FrameXML\\HonorFrame.xml",
];

pub(super) const BAG_UI: &[&str] = &[
    // The reference's own localized strings — `BACKPACK_TOOLTIP`, `EQUIP_CONTAINER`, `KEYRING`,
    // the `*_FONT_COLOR_CODE` pair. The app loads this at VM setup, ahead of the manifest
    // (`ui_script/mod.rs`, `setup_script`); a test VM has to say so itself. Not optional since
    // 1751's third window: stock `MainMenuBarBagButtons.lua`'s hovers pass these straight into
    // `GameTooltip:SetText`, and `SetText(nil)` raises rather than showing an empty plate. Our
    // deleted `BagFrame.xml` carried `X = X or "…"` fallbacks for exactly this gap; the real file
    // is the better answer, and these tests already gate on the install.
    "Interface\\FrameXML\\GlobalStrings.lua",
    "Fonts.xml",
    // `TEXT()` — the reference's own identity-function wrapper, which stock
    // `MainMenuBarBackpackButton`'s OnEnter calls (`GameTooltip:SetText(TEXT(BACKPACK_TOOLTIP)…)`)
    // and `BagSlotButton_OnEnter` calls for `EQUIP_CONTAINER`. Manifest entry 3, and not optional
    // for the bag bar since 1751's third window made that bar the reference's own.
    "BasicControls.xml",
    // `UIParent` itself: the twelve `ContainerFrame`s declare `parent="UIParent"`, and
    // `updateContainerFrameAnchors` anchors each open bag to `frame:GetParent()` while
    // `OpenAllBags` opens with `if not UIParent:IsVisible() then return end`. Without it the
    // windows fall out of the cascade and the reference's own layout pass has nothing to measure.
    "UIParent.xml",
    "Interface\\FrameXML\\ItemButtonTemplate.xml",
    "MoneyFrame.xml",
    "UiPanels.xml",
    "GameTooltip.xml",
    "Cooldown.xml",
    // The bag BAR declares `parent="MainMenuBarArtFrame"`, resolved at LOAD — so without this the
    // six buttons fall back to UIParent and sit at a level no production run ever puts them at.
    // It also carries `MainMenuBar_UpdateKeyRing`, which is what puts the keyring on the bar.
    "ActionBar.xml",
    // `UpdateMicroButtons` — the KEYRING's own OnShow/OnHide calls it (ContainerFrame.lua l.117,
    // l.137), because in the reference the keyring's existence moves the micro-button row.
    "MicroMenu.xml",
    "UIPanelTemplates.xml",
    "Interface\\FrameXML\\ContainerFrame.xml",
    // `PaperDollItemSlotButtonTemplate` and the `PaperDollItemSlotButton_*` family behind it,
    // which every bag button inherits and runs — resolved at load, so this has to precede the bar
    // exactly as it does in the manifest. Stock `BagSlotButtonTemplate`'s OnLoad *is*
    // `PaperDollItemSlotButton_OnLoad()`, which gives each bag button its inventory-slot id
    // (20..23 via `GetInventorySlotInfo`), its six event registrations and its first paint.
    //
    // The whole paper-doll file, because that is where the reference declares both — our
    // `ItemSlotButtonTemplates.xml` held a transcribed copy of the template only because our own
    // character window loaded too late to declare it, and it is deleted (decision 1751). Its
    // companion `Interface\\FrameXML\\CharacterFrame.xml` is deliberately NOT here: this list is
    // the bags, `PaperDollFrame` only names `CharacterFrame` in `parent=` (a missing parent is a
    // loader warning, not an error), and `CharacterFrame_OnLoad` would drag in the unit frames,
    // the XP bar and the text-status-bar file for a window no bag test opens.
    "Interface\\FrameXML\\PaperDollFrame.xml",
    // The bag BAR itself, the reference's own since 1751's third window: MainMenuBarBackpackButton,
    // CharacterBag0..3Slot, KeyRingButton, `BagSlotButtonTemplate`, and `KEYRING_CONTAINER`.
    "Interface\\FrameXML\\MainMenuBarBagButtons.xml",
    // `StackSplitFrame` is not optional either: the reference's own
    // `ContainerFrameItemButton_OnClick` calls `StackSplitFrame:Hide()` on EVERY plain click
    // (ContainerFrame.lua l.581) before the pickup, and opens it on the shift fork.
    "Interface\\FrameXML\\StackSplitFrame.xml",
    // …nor is the chat edit box. The reference's SHIFT arm opens with
    // `if ( ChatFrameEditBox:IsShown() )` (ContainerFrame.lua l.569) to decide between posting the
    // item's link and splitting the stack, so a VM without it raises before either.
    "Interface\\FrameXML\\UIMenu.xml", // the kit ChatMenu/EmoteMenu/VoiceMacroMenu build from
    "ChatFrame.xml",
    // Our adapters over the reference's container files — the keyring tooltip wrapper, the three
    // bag verbs 0561 shadows (`OpenBackpack`/`CloseBackpack`/`CloseAllBags`), and the item-push
    // card the reference draws with a `<Model>` this engine does not render (0887). It has to be
    // AFTER `ContainerFrame.xml` and after the bar, which is why it is here and not up with
    // UiPanels.xml.
    "ContainerFrameAdapters.xml",
    // `updateContainerFrameAnchors` measures every open bag against `BankFrame:GetRight()`
    // (ContainerFrame.lua l.505) on EVERY open and close, so the bank window is not optional
    // scenery for a bag test — it is a hard dependency of the reference's own layout pass. It is
    // one in the real client too; the manifest just satisfies it far below the bags. The
    // reference's own file since 1751's second window.
    "Interface\\FrameXML\\BankFrame.xml",
];

/// The name of the `ContainerFrame` currently showing bag `id`, or `None` if it is not open.
///
/// **Ask, never assume.** The reference recycles twelve windows across every container
/// (`ContainerFrame_GetOpenFrame`), so which one a bag lands in depends on what else is open —
/// there is no `BenillaBagFrame2` to name any more, and pinning `ContainerFrame3` in a test would
/// pin a coincidence. `IsBagOpen` is the reference's own published scan.
pub(super) fn bag_window(s: &UiScript, id: i64) -> Option<String> {
    s.eval::<Option<i64>>(&format!("return IsBagOpen({id})"))
        .unwrap()
        .map(|i| format!("ContainerFrame{i}"))
}

/// Is bag `id` open? [`bag_window`]'s predicate half.
pub(super) fn bag_open(s: &UiScript, id: i64) -> bool {
    bag_window(s, id).is_some()
}

/// The item button in bag `id`'s open window that holds game slot `slot`.
///
/// Asked of the buttons' own `GetID`, never derived: `ContainerFrame_GenerateFrame` numbers them
/// backwards (`index = size - j + 1`, so `…Item1` is the bag's LAST slot, bottom-right), and a
/// window generated for a different bag size numbers them differently.
pub(super) fn bag_slot_button(s: &UiScript, id: i64, slot: u32) -> String {
    let w = bag_window(s, id).unwrap_or_else(|| panic!("bag {id} is not open"));
    s.eval::<String>(&format!(
        "for j = 1, MAX_CONTAINER_ITEMS do \
           local b = getglobal(\"{w}Item\"..j) \
           if b and b:IsShown() and b:GetID() == {slot} then return \"{w}Item\"..j end \
         end return \"\""
    ))
    .inspect(|n| assert!(!n.is_empty(), "no {w}Item* is bag {id} slot {slot}"))
    .expect("the item-button scan")
}

/// The centre of a named frame, in the y-up UI space `mouse_move`/`mouse_button` take.
pub(super) fn centre_of(s: &mut UiScript, name: &str) -> (f32, f32) {
    s.resolve();
    let r: Vec<f32> = s
        .eval(&format!(
            "local f = getglobal(\"{name}\") \
             return {{ f:GetLeft(), f:GetBottom(), f:GetWidth(), f:GetHeight() }}"
        ))
        .unwrap_or_else(|e| panic!("{name}: no resolved rect: {e}"));
    assert_eq!(r.len(), 4, "{name}: unresolved rect {r:?}");
    (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
}

/// Move the mouse onto the centre of `name` — the whole engine path (hit test → `OnEnter`), never
/// `s.run("Handler(button)")`.
///
/// **This is not a style preference any more.** The reference's own handlers read `this`
/// (`ContainerFrameItemButton_OnClick(button, ignoreModifiers)` takes the MOUSE button as its first
/// argument and gets the frame from `this`), and only the engine sets `this`. A migrated window's
/// tests therefore drive the mouse, which is also the stronger test — it puts the
/// `RegisterForClicks` gate and the template's own script wiring under test.
pub(super) fn hover(s: &mut UiScript, name: &str) {
    let (x, y) = centre_of(s, name);
    s.mouse_move(x, y);
}

/// Move the mouse well clear of everything — the `OnLeave` half of [`hover`].
pub(super) fn unhover(s: &mut UiScript) {
    s.mouse_move(-500.0, -500.0);
}

/// Press and release `button` over the centre of `name`, the way a player's mouse does.
pub(super) fn click(s: &mut UiScript, name: &str, button: &str) {
    let (x, y) = centre_of(s, name);
    s.mouse_move(x, y);
    s.mouse_button(x, y, button, true);
    s.mouse_button(x, y, button, false);
}
