//! The shipped `assets/ui/AddonList.xml` — the in-game AddOns list (decision 1197).
//!
//! **The window is the test of the API as much as the API is the test of the window.** This panel
//! is written entirely against the eleven AddOn verbs 1191 landed — `GetNumAddOns`,
//! `GetAddOnInfo`, `GetAddOnDependencies`, `EnableAddOn`, `DisableAddOn`, `EnableAllAddOns`,
//! `DisableAllAddOns` — with no host support of its own. So driving it drives them, through the
//! same Lua an addon author would write, which is worth more than a Rust test of each verb.
//!
//! What these guard: the row painter's three-colour rule and its status column against a registry
//! with a real dependency graph; the checkbox round-trip through `EnableAddOn`/`DisableAddOn`;
//! Cancel restoring what the panel found (`ResetAddOns` written out, since that verb is
//! glue-namespace and absent in-world); the scroll clamp; and the Enable All / Disable All pair.

use benilla_ui::script::{AddOnInfo, UiScript};

/// Fonts + the panel, and a registry with a shape worth painting: one plain addon, a library, a
/// dependent on that library, and one whose dependency is not installed.
fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
        "GameTooltip.xml",
        "AddonList.xml",
    ] {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/ui")
                .join(file),
        )
        .unwrap();
        let doc = benilla_ui::framexml::parse(&text).unwrap();
        let report = benilla_ui::loader::load(&s, &doc, &|_| None);
        assert!(
            report.errors.is_empty(),
            "{file}: loader errors: {:?}",
            report.errors
        );
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    let info = |name: &str, title: &str, deps: &[&str]| AddOnInfo {
        name: name.into(),
        title: Some(title.into()),
        notes: Some(format!("{name}'s notes")),
        url: None,
        secure: false,
        load_on_demand: false,
        dependencies: deps.iter().map(|d| (*d).to_string()).collect(),
        directives: Default::default(),
        files: Vec::new(),
        saved_variables: Vec::new(),
        saved_variables_per_character: Vec::new(),
        enabled: true,
        loaded: false,
    };
    s.register_addons(
        vec![
            info("Plain", "Plain Addon", &[]),
            info("Lib", "The Library", &[]),
            info("Needs", "Needs The Library", &["Lib"]),
            info("Orphan", "Orphaned", &["Nowhere"]),
        ],
        None,
        None,
        None,
    );
    s
}

/// The text of row `i`'s title and status, as the panel painted them.
fn row(s: &UiScript, i: usize) -> (String, String) {
    let title = s
        .eval::<String>(&format!("return AddonListEntry{i}Title:GetText() or ''"))
        .unwrap();
    let status = s
        .eval::<String>(&format!("return AddonListEntry{i}Status:GetText() or ''"))
        .unwrap();
    (title, status)
}

/// The panel paints the registry: title (`## Title` else the folder name), the reference's status
/// token, and a checkbox per row — all through `GetAddOnInfo`.
#[test]
fn the_panel_paints_the_registry_through_the_addon_api() {
    let s = harness();
    s.run("AddonList_Show()").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert!(s.eval::<bool>("return AddonListFrame:IsVisible()").unwrap());
    assert_eq!(
        row(&s, 1).0,
        "Plain Addon",
        "`## Title` wins over the folder"
    );
    assert_eq!(row(&s, 3).0, "Needs The Library");

    // Rows past the registry are hidden, not left showing a stale addon.
    assert!(
        !s.eval::<bool>("return AddonListEntry5:IsVisible()")
            .unwrap(),
        "row 5 has no addon behind it"
    );
    // Four addons is well under MAX_ADDONS_DISPLAYED, so the scroll pair stays hidden — the
    // reference's own GlueScrollFrame_Update condition.
    assert!(!s
        .eval::<bool>("return AddonListScrollUp:IsVisible()")
        .unwrap());

    // A dependency that is not installed is a status, not a checkbox state.
    assert_eq!(
        row(&s, 4).1,
        "DEP_MISSING",
        "the reference's own reason token, shown when GlobalStrings has no ADDON_ string for it"
    );
}

/// The checkbox round-trips through `EnableAddOn`/`DisableAddOn`, and the effect on a *dependent*
/// shows up in its status column — which is the whole reason the panel re-reads rather than
/// caching.
#[test]
fn a_checkbox_click_disables_through_the_api_and_the_dependent_says_so() {
    let s = harness();
    s.run("AddonList_Show()").unwrap();

    // Row 2 is the library. Click its checkbox the way the XML does.
    s.run("AddonList_ToggleEntry(2)").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert_eq!(row(&s, 2).1, "DISABLED", "the player turned it off");
    assert_eq!(
        row(&s, 3).1,
        "DEP_DISABLED",
        "its dependent reports the reference's dependency token, not a stale clean row"
    );
    assert!(
        !s.eval::<bool>("return AddonListEntry2Check:GetChecked()")
            .unwrap(),
        "and the checkbox follows the registry rather than its own memory"
    );

    // Clicking again re-enables, and the dependent recovers.
    s.run("AddonList_ToggleEntry(2)").unwrap();
    assert_eq!(row(&s, 2).1, "");
    assert_eq!(row(&s, 3).1, "");
}

/// **Cancel restores what the panel found** — the reference's `ResetAddOns`, written out in Lua
/// because that verb is glue-namespace and deliberately absent in-world (1191 §6).
#[test]
fn cancel_restores_the_state_the_panel_opened_with() {
    let s = harness();
    s.run("AddonList_Show()").unwrap();
    s.run("AddonList_ToggleEntry(1) AddonList_ToggleEntry(2)")
        .unwrap();
    assert_eq!(row(&s, 1).1, "DISABLED");

    s.run("AddonList_OnCancel()").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(!s.eval::<bool>("return AddonListFrame:IsVisible()").unwrap());

    // Re-open: nothing the cancelled session did survived.
    s.run("AddonList_Show()").unwrap();
    assert_eq!(row(&s, 1).1, "");
    assert_eq!(row(&s, 2).1, "");
}

/// Okay keeps the edit — the other half of the pair, and the one that would silently pass if
/// `AddonList_OnOk` accidentally called the restore loop.
#[test]
fn okay_keeps_the_edit() {
    let s = harness();
    s.run("AddonList_Show()").unwrap();
    s.run("AddonList_ToggleEntry(1)").unwrap();
    s.run("AddonList_OnOk()").unwrap();
    assert!(!s.eval::<bool>("return AddonListFrame:IsVisible()").unwrap());

    s.run("AddonList_Show()").unwrap();
    assert_eq!(row(&s, 1).1, "DISABLED", "Okay is not a disguised Cancel");
}

/// Enable All / Disable All go through the API's own pair, and the panel repaints.
#[test]
fn enable_all_and_disable_all_move_every_row() {
    let s = harness();
    s.run("AddonList_Show()").unwrap();

    s.run("AddonList_SetAll(nil)").unwrap();
    for i in 1..=4 {
        assert_eq!(row(&s, i).1, "DISABLED", "row {i} after Disable All");
    }

    s.run("AddonList_SetAll(1)").unwrap();
    assert_eq!(row(&s, 1).1, "");
    assert_eq!(
        row(&s, 4).1,
        "DEP_MISSING",
        "Enable All cannot fix a dependency that is not installed — the status is not a checkbox"
    );
}

/// The scroll offset clamps at both ends, and a list that fits never scrolls.
#[test]
fn the_scroll_offset_clamps() {
    let s = harness();
    s.run("AddonList_Show()").unwrap();
    s.run("AddonList_Scroll(5)").unwrap();
    assert_eq!(
        s.eval::<i64>("return AddonList.offset").unwrap(),
        0,
        "four addons fit in nineteen rows — there is nothing to scroll"
    );
    s.run("AddonList_Scroll(-5)").unwrap();
    assert_eq!(s.eval::<i64>("return AddonList.offset").unwrap(), 0);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **The ESC-menu path, end to end** — the director's ask stated as a test: open the game menu,
/// click AddOns, and the list is up with the menu gone.
///
/// Loaded in the shipped manifest's own order (`AddonList.xml` before `GameMenuFrame.xml`), which
/// is what makes the button's `AddonList_Toggle()` resolve at click time. A test that loaded them
/// the other way round would pass on the button's `IsEnabled()` and fail on the click, which is
/// exactly the seam worth covering.
#[test]
fn the_esc_menu_addons_entry_opens_the_list() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
        "GameTooltip.xml",
        "AddonList.xml",
        "GameMenuFrame.xml",
    ] {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/ui")
                .join(file),
        )
        .unwrap();
        let doc = benilla_ui::framexml::parse(&text).unwrap();
        let report = benilla_ui::loader::load(&s, &doc, &|_| None);
        assert!(report.errors.is_empty(), "{file}: {:?}", report.errors);
    }

    s.run("ShowUIPanel(GameMenuFrame)").unwrap();
    assert!(
        s.eval::<bool>("return GameMenuButtonAddOns:IsEnabled()")
            .unwrap(),
        "the entry is live — it stopped being GameMenuButton_Pending in 1197"
    );

    s.run("GameMenuButtonAddOns:Click()").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.eval::<bool>("return AddonListFrame:IsVisible()").unwrap(),
        "the list came up"
    );
    assert!(
        !s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap(),
        "and the menu went away — the Macros entry's own body shape"
    );
}

/// **ESC cancels the list**, the reference's own `AddonList_OnKeyDown` arm — and it discards.
///
/// This engine gives plain frames no raw key events, so the window joins `ToggleGameMenu`'s
/// shared ESC chain like the stack-split spinner and the world map. The rung sits above the
/// options window and below the popup engine, so a confirm dialog still wins the key.
#[test]
fn escape_cancels_the_list_and_discards_the_edit() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
        "GameTooltip.xml",
        "AddonList.xml",
    ] {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/ui")
                .join(file),
        )
        .unwrap();
        let doc = benilla_ui::framexml::parse(&text).unwrap();
        let report = benilla_ui::loader::load(&s, &doc, &|_| None);
        assert!(report.errors.is_empty(), "{file}: {:?}", report.errors);
    }
    s.register_addons(
        vec![benilla_ui::script::AddOnInfo {
            name: "Solo".into(),
            title: Some("Solo".into()),
            notes: None,
            url: None,
            secure: false,
            load_on_demand: false,
            dependencies: Vec::new(),
            directives: Default::default(),
            files: Vec::new(),
            saved_variables: Vec::new(),
            saved_variables_per_character: Vec::new(),
            enabled: true,
            loaded: false,
        }],
        None,
        None,
        None,
    );

    s.run("AddonList_Show()").unwrap();
    s.run("AddonList_ToggleEntry(1)").unwrap();
    assert_eq!(
        s.eval::<String>("return AddonListEntry1Status:GetText() or ''")
            .unwrap(),
        "DISABLED"
    );

    s.run("ToggleGameMenu()").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        !s.eval::<bool>("return AddonListFrame:IsVisible()").unwrap(),
        "ESC put the list away"
    );
    s.run("AddonList_Show()").unwrap();
    assert_eq!(
        s.eval::<String>("return AddonListEntry1Status:GetText() or ''")
            .unwrap(),
        "",
        "and discarded the edit — ESC is Cancel, not Okay"
    );
}

/// The hover tooltip runs — `AddonTooltip_Update`'s three lines (title, notes, dependencies) on
/// the shared `GameTooltip` rather than a private frame.
///
/// Worth its own test because it is the one path the other seven never touch: a load error would
/// have shown up in the harness, but a *runtime* error in a hover handler goes to
/// `UiScript::errors` and nothing else, which is exactly the silent kind.
#[test]
fn hovering_a_row_raises_the_tooltip_with_notes_and_dependencies() {
    let s = harness();
    s.run("AddonList_Show()").unwrap();

    // Row 3 is `Needs`, which depends on `Lib` — so it exercises both optional lines.
    s.run("AddonList_RowEnter(AddonListEntry3)").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>("return GameTooltip:IsOwned(AddonListEntry3)")
        .unwrap());

    s.run("AddonList_RowLeave()").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // And a row with no dependencies takes the other branch without erroring.
    s.run("AddonList_RowEnter(AddonListEntry1)").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **The ESC-menu list reports an out-of-date `## Interface`, exactly like its char-select twin.**
///
/// The two screens are the same job over two views of one folder (1197 §1, §3), and they were
/// answering *differently* about the same addon: the glue panel read `## Interface` off the manifest
/// and showed "Out of date", while this one asked `GetAddOnInfo` — whose `reason` has no interface
/// arm, and correctly so, because 1191 §6 reports the mismatch and never enforces it. Not being a
/// reason the addon cannot load, it was simply never shown here. Nothing errored; the column was
/// just blank, which is the silent-gap shape this arc keeps finding.
///
/// The three arms are the whole rule: mismatched reports, matching does not, and **absent is silent
/// rather than out-of-date** — the same case `a manifest with no ## Interface is silent` pins on the
/// glue side.
/// Asserted on the STATUS COLUMN the player reads, not on the helper — the helper being right
/// while the column stayed blank is precisely the bug this replaces.
#[test]
fn the_list_reports_an_out_of_date_interface_like_the_char_select_twin() {
    let mut s = harness();

    // Three real manifests, differing only in `## Interface` — the directive `GetAddOnMetadata`
    // reads, so this drives the production path rather than stubbing it.
    let with_interface = |name: &str, declared: Option<&str>| AddOnInfo {
        name: name.into(),
        title: Some(name.into()),
        directives: declared
            .map(|v| vec![("Interface".to_string(), v.to_string())])
            .unwrap_or_default(),
        enabled: true,
        ..Default::default()
    };
    s.register_addons(
        vec![
            with_interface("Old", Some("11100")),
            with_interface("Current", Some("11200")),
            with_interface("Silent", None),
        ],
        None,
        None,
        None,
    );

    s.run("AddonList_Show()").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert_eq!(
        row(&s, 1).1,
        "Out of date",
        "an older `## Interface` must reach the status column — it was blank here while the \
         char-select twin said so about the same addon"
    );
    assert_eq!(row(&s, 2).1, "", "our own interface is current");
    assert_eq!(
        row(&s, 3).1,
        "",
        "no `## Interface` at all is silent, not out of date"
    );
}

/// `UIParentLoadAddOn(name)` — the on-demand loader 5 corpus addons name (FuBar_MCPFu,
/// Fubar_RareTrackerFu, MCP, MikScrollingBattleText, SpecialTalent), and which raised as a nil
/// global until every piece it needs was joined up in `BasicControls.xml`.
///
/// Two things are asserted, and the second is the one a reconstruction from the name would miss:
/// it answers `LoadAddOn`'s own falsey result so a caller can branch, and `FailedAddOnLoad` shows
/// the failure **once per addon name**, not once per attempt (ref `UIParent.lua` l.641-643).
#[test]
fn ui_parent_load_addon_reports_once_and_returns_the_load_result() {
    let s = UiScript::new().unwrap();
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui/BasicControls.xml"),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(&s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "loader errors: {:?}",
        report.errors
    );

    // The two GlobalStrings this leans on. The real client has them from FrameXML's first file and
    // the corpus survey runs the real `GlobalStrings.lua`; a bare VM does not, so they are seated
    // here rather than left to make `format` raise on a nil.
    s.run(
        r#"
        ADDON_LOAD_FAILED = "Couldn't load %s: %s"
        ADDON_MISSING = "Missing"
        shown = {}
        message = function(t) table.insert(shown, t) end
    "#,
    )
    .unwrap();

    // A name no registry has: LoadAddOn answers (nil, "MISSING"), so this answers falsey.
    assert!(
        s.eval::<bool>(r#"return UIParentLoadAddOn("NoSuchAddonAnywhere") == nil"#)
            .unwrap(),
        "must hand back LoadAddOn's own falsey result for a caller to branch on"
    );
    assert_eq!(
        s.eval::<String>("return shown[1]").unwrap(),
        "Couldn't load NoSuchAddonAnywhere: Missing",
        "the reason word resolves through ADDON_..reason"
    );

    // The latch: a retrying addon gets one modal, not one per attempt.
    for _ in 0..3 {
        s.run(r#"UIParentLoadAddOn("NoSuchAddonAnywhere")"#)
            .unwrap();
    }
    assert_eq!(
        s.eval::<i64>("return table.getn(shown)").unwrap(),
        1,
        "FailedAddOnLoad must suppress every repeat for the same name"
    );

    // A DIFFERENT name is a different latch entry, so it still reports.
    s.run(r#"UIParentLoadAddOn("AlsoNotThere")"#).unwrap();
    assert_eq!(s.eval::<i64>("return table.getn(shown)").unwrap(), 2);
}
