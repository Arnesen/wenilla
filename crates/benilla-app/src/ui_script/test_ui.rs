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
pub(super) const BAG_UI: &[&str] = &[
    "Fonts.xml",
    // `UIParent` itself: the twelve `ContainerFrame`s declare `parent="UIParent"`, and
    // `updateContainerFrameAnchors` anchors each open bag to `frame:GetParent()` while
    // `OpenAllBags` opens with `if not UIParent:IsVisible() then return end`. Without it the
    // windows fall out of the cascade and the reference's own layout pass has nothing to measure.
    "UIParent.xml",
    "ItemButtonTemplate.xml",
    "MoneyFrame.xml",
    "UiPanels.xml",
    "GameTooltip.xml",
    "Cooldown.xml",
    // The bag BAR hangs off `MainMenuBarArtFrame` and seats itself one frame level above the bar
    // art (`BenillaActionBarArt_SeatAbove`), both at LOAD — so without this the five buttons fall
    // back to UIParent and sit at a level no production run ever puts them at.
    "ActionBar.xml",
    // `UpdateMicroButtons` — the KEYRING's own OnShow/OnHide calls it (ContainerFrame.lua l.117,
    // l.137), because in the reference the keyring's existence moves the micro-button row.
    "MicroMenu.xml",
    "UIPanelTemplates.xml",
    "Interface\\FrameXML\\ContainerFrame.xml",
    "BagFrame.xml",
    // `StackSplitFrame` is not optional either: the reference's own
    // `ContainerFrameItemButton_OnClick` calls `StackSplitFrame:Hide()` on EVERY plain click
    // (ContainerFrame.lua l.581) before the pickup, and opens it on the shift fork.
    "StackSplit.xml",
    // …nor is the chat edit box. The reference's SHIFT arm opens with
    // `if ( ChatFrameEditBox:IsShown() )` (ContainerFrame.lua l.569) to decide between posting the
    // item's link and splitting the stack, so a VM without it raises before either.
    "ChatFrame.xml",
    // Our adapters over the reference's container files — one wrapper for the keyring tooltip, and
    // the three bag verbs 0561 shadows (`OpenBackpack`/`CloseBackpack`/`CloseAllBags`). It has to
    // be AFTER `ContainerFrame.xml` for either job, which is why it is here and not up with
    // UiPanels.xml.
    "ContainerFrameAdapters.xml",
    // `updateContainerFrameAnchors` measures every open bag against `BankFrame:GetRight()`
    // (ContainerFrame.lua l.505) on EVERY open and close, so the bank window is not optional
    // scenery for a bag test — it is a hard dependency of the reference's own layout pass. It is
    // one in the real client too; the manifest just satisfies it far below the bags.
    "BankFrame.xml",
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
