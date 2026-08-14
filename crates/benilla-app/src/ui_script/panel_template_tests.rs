//! The shipped `assets/ui/UIPanelTemplates.xml` + `assets/ui/OptionsFrameTemplates.xml` — the
//! reference's SHARED widget kit, driven the way an addon drives it.
//!
//! **These are not tests of a window.** Nothing benilla ships instantiates a single template in
//! either file: their only consumer is a third-party addon writing
//! `CreateFrame(kind, name, parent, "SomeTemplate")`, which decision 1203 made work and which
//! `addon_harness` then ranked — 69 corpus call sites on `UICheckButtonTemplate` alone, 185 sites
//! in total naming templates we had never declared. So every test here goes in through
//! `CreateFrame`'s fourth argument, from Lua, with a caller-chosen name, because that is the entire
//! surface these files exist to serve.
//!
//! What they guard, in the order the harness ranked them:
//!
//! - **The name.** `CreateFrame("CheckButton", "MyCheck", UIParent, "UICheckButtonTemplate")` must
//!   publish `MyCheckText`, never `UICheckButtonTemplateText` — `getglobal(this:GetName().."Text")`
//!   is the next line every one of those 69 sites writes. 1203 pinned the *mechanism*; this pins it
//!   through the real templates, including the two-level `$parent` case
//!   (`MyScrollScrollBarScrollUpButton`).
//! - **The art.** Each state texture in those files carries the reference's `inherits=` *and* the
//!   art that `inherits=` resolves to, because our loader does not expand `inherits=` in
//!   state-texture position (the deviation is stated at the head of `UIPanelTemplates.xml`). The
//!   extract assertions below are that deviation's falsifier: strip the inline `file=` back to the
//!   reference's `inherits=`-only form and every checkbox and panel button in the corpus goes
//!   invisible with nothing erroring.
//! - **The behaviour.** `SetChecked`/`GetChecked` across a click, the close button hiding its
//!   parent, the edit box round-tripping text, the scroll frame's range reaching its bar.
//! - **The names are 1.12's.** The last test re-reads both files against
//!   `reference/1.12-globals.tsv` — a template we invent under a plausible-looking name is a name
//!   an addon can never have meant.

use benilla_ui::script::UiScript;

/// One shipped file into `s`, asserting it loaded clean.
fn load_xml(s: &UiScript, file: &str) {
    let text = std::fs::read_to_string(ui_dir().join(file)).unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "{file}: loader errors: {:?}",
        report.errors
    );
}

fn ui_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui")
}

/// The manifest prefix these two files sit on: fonts, `UIParent` (the parent every addon passes),
/// `HideUIPanel` (the close button's OnClick), the tooltip (the options widgets' hover) and the
/// scroll kit (`ScrollFrame_OnLoad`), then the two files under test in manifest order.
fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "UIParent.xml",
        "UiPanels.xml",
        "GameTooltip.xml",
        "ScrollTemplates.xml",
        "UIPanelTemplates.xml",
        "OptionsFrameTemplates.xml",
    ] {
        load_xml(&s, file);
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s
}

/// Every texture path the resolved frame tree actually draws.
fn drawn_textures(s: &mut UiScript) -> Vec<String> {
    s.resolve();
    s.extract()
        .into_iter()
        .filter_map(|q| match q.content {
            benilla_ui::script::QuadContent::Texture { path: Some(p), .. } => Some(p),
            _ => None,
        })
        .collect()
}

/// **The test this whole file exists for.** An addon's own line, verbatim, and then the global it
/// reads back on the next line.
///
/// The failure this pins is not "no checkbox": it is a checkbox whose label region is called
/// `UICheckButtonTemplateText`, so `getglobal(this:GetName().."Text")` is nil, so the addon's
/// `SetText` dies — with the `CreateFrame` itself having succeeded and returned an object.
#[test]
fn a_check_button_from_the_template_names_its_label_after_the_caller() {
    let s = harness();
    s.run(r#"MyCheck = CreateFrame("CheckButton", "MyCheck", UIParent, "UICheckButtonTemplate")"#)
        .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert!(
        s.eval::<bool>(r#"return getglobal("MyCheckText") ~= nil"#)
            .unwrap(),
        "the label publishes under the CALLER's name — this is the line an addon writes next"
    );
    assert!(
        s.eval::<bool>(r#"return getglobal("UICheckButtonTemplateText") == nil"#)
            .unwrap(),
        "and never under the template's"
    );

    // The 69-call-site idiom, end to end.
    s.run(r#"getglobal("MyCheckText"):SetText("Show my thing")"#)
        .unwrap();
    assert_eq!(
        s.eval::<String>(r#"return getglobal("MyCheckText"):GetText()"#)
            .unwrap(),
        "Show my thing"
    );
    assert_eq!(
        s.eval::<(f64, f64)>("return MyCheck:GetWidth(), MyCheck:GetHeight()")
            .unwrap(),
        (32.0, 32.0),
        "the template's own 32x32"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The checkbox **paints**, and it paints the reference's art.
///
/// This is the falsifier for `UIPanelTemplates.xml`'s stated deviation: the reference declares its
/// state textures with `inherits=` alone, our loader does not expand `inherits=` there, and a
/// transcription that copied the reference literally would create a frame with no state textures at
/// all — no error, no warning, 69 invisible checkboxes.
#[test]
fn the_templated_check_button_draws_the_reference_checkbox_art() {
    let mut s = harness();
    s.run(
        r#"MyCheck = CreateFrame("CheckButton", "MyCheck", UIParent, "UICheckButtonTemplate")
           MyCheck:SetPoint("TOPLEFT", 20, -20)"#,
    )
    .unwrap();
    assert!(
        s.eval::<bool>("return MyCheck:GetNormalTexture() ~= nil")
            .unwrap(),
        "the state-texture slot exists at all"
    );

    let drawn = drawn_textures(&mut s);
    assert!(
        drawn
            .iter()
            .any(|p| p == r"Interface\Buttons\UI-CheckBox-Up"),
        "the unchecked box is on screen: {drawn:?}"
    );
    assert!(
        !drawn
            .iter()
            .any(|p| p == r"Interface\Buttons\UI-CheckBox-Check"),
        "and the tick is not, until it is checked"
    );

    s.run("MyCheck:SetChecked(1)").unwrap();
    let drawn = drawn_textures(&mut s);
    assert!(
        drawn
            .iter()
            .any(|p| p == r"Interface\Buttons\UI-CheckBox-Check"),
        "checked draws the tick: {drawn:?}"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A click toggles, and the addon's `OnClick` sees the NEW state — the widget contract every
/// options panel in the corpus is written against (`if this:GetChecked() then …`).
#[test]
fn a_templated_check_button_toggles_before_its_on_click_runs() {
    let s = harness();
    s.run(
        r#"MyCheck = CreateFrame("CheckButton", "MyCheck", UIParent, "UICheckButtonTemplate")
           MySeen = {}
           MyCheck:SetScript("OnClick", function()
               table.insert(MySeen, this:GetChecked() and 1 or 0)
           end)"#,
    )
    .unwrap();
    assert!(
        !s.eval::<bool>("return MyCheck:GetChecked() and true or false")
            .unwrap(),
        "a fresh box is unchecked"
    );

    s.run("MyCheck:Click()").unwrap();
    assert!(s
        .eval::<bool>("return MyCheck:GetChecked() and true or false")
        .unwrap());
    s.run("MyCheck:Click()").unwrap();
    assert!(!s
        .eval::<bool>("return MyCheck:GetChecked() and true or false")
        .unwrap());

    assert_eq!(
        s.eval::<Vec<i64>>("return MySeen").unwrap(),
        vec![1, 0],
        "the handler read the post-toggle state both times, not the pre-toggle one"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// `UIPanelButtonTemplate` — 19 corpus call sites, and the shared original of nine private copies
/// in our own `assets/ui`. Its label is a `<ButtonText>` rather than a layer FontString, so the
/// addon idiom is `btn:SetText(...)` and the published global is still `$parentText`.
#[test]
fn a_panel_button_from_the_template_labels_and_paints() {
    let mut s = harness();
    s.run(
        r#"MyBtn = CreateFrame("Button", "MyBtn", UIParent, "UIPanelButtonTemplate")
           MyBtn:SetWidth(90) MyBtn:SetHeight(22)
           MyBtn:SetPoint("TOPLEFT", 20, -20)
           MyBtn:SetText("Okay")"#,
    )
    .unwrap();
    assert_eq!(
        s.eval::<String>(r#"return getglobal("MyBtnText"):GetText()"#)
            .unwrap(),
        "Okay",
        "<ButtonText name=\"$parentText\"> publishes against the caller"
    );
    assert!(s
        .eval::<bool>(r#"return getglobal("UIPanelButtonTemplateText") == nil"#)
        .unwrap());

    let drawn = drawn_textures(&mut s);
    assert!(
        drawn
            .iter()
            .any(|p| p == r"Interface\Buttons\UI-Panel-Button-Up"),
        "the button face is on screen: {drawn:?}"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// `UIPanelCloseButton` — 8 call sites across 4 corpus addons, and the one template here whose
/// whole point is its script: `HideUIPanel(this:GetParent())`, resolved at click time against
/// `UiPanels.xml`.
#[test]
fn the_templated_close_button_hides_the_frame_it_sits_on() {
    let s = harness();
    s.run(
        r#"MyPanel = CreateFrame("Frame", "MyPanel", UIParent)
           MyPanel:SetWidth(200) MyPanel:SetHeight(100)
           MyPanel:SetPoint("CENTER")
           MyPanel:Show()
           MyClose = CreateFrame("Button", "MyClose", MyPanel, "UIPanelCloseButton")
           MyClose:SetPoint("TOPRIGHT")"#,
    )
    .unwrap();
    assert_eq!(
        s.eval::<(f64, f64)>("return MyClose:GetWidth(), MyClose:GetHeight()")
            .unwrap(),
        (32.0, 32.0)
    );
    assert!(s.eval::<bool>("return MyPanel:IsShown()").unwrap());

    s.run("MyClose:Click()").unwrap();
    assert!(
        !s.eval::<bool>("return MyPanel:IsShown()").unwrap(),
        "the template's own OnClick reached HideUIPanel"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// `InputBoxTemplate` — 5 call sites across 3 corpus addons. Its three border slices are `<Layers>`
/// regions (so they publish by name), and its direct-child `<FontString>` is the box's *text*
/// region, not a layer.
#[test]
fn an_input_box_from_the_template_carries_its_border_and_takes_text() {
    let mut s = harness();
    s.run(
        r#"MyBox = CreateFrame("EditBox", "MyBox", UIParent, "InputBoxTemplate")
           MyBox:SetWidth(120) MyBox:SetHeight(20)
           MyBox:SetPoint("TOPLEFT", 20, -20)
           MyBox:SetText("hello")"#,
    )
    .unwrap();
    for slice in ["Left", "Right", "Middle"] {
        assert!(
            s.eval::<bool>(&format!(r#"return getglobal("MyBox{slice}") ~= nil"#))
                .unwrap(),
            "the {slice} border slice publishes against the caller's name"
        );
    }
    assert_eq!(s.eval::<String>("return MyBox:GetText()").unwrap(), "hello");

    let drawn = drawn_textures(&mut s);
    assert!(
        drawn
            .iter()
            .any(|p| p == r"Interface\Common\Common-Input-Border"),
        "the input border draws: {drawn:?}"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// `UIPanelScrollFrameTemplate` — 6 call sites across 2 corpus addons, and the template with the
/// deepest naming problem: its scroll bar's arrow buttons are `$parentScrollUpButton` inside
/// `$parentScrollBar`, so a caller named `MyScroll` must reach
/// `MyScrollScrollBarScrollUpButton` through **two** levels of `$parent` — which is precisely what
/// `ScrollFrame_OnLoad` getglobals on the way past.
///
/// The `UpdateScrollChildRect` leg is also the falsifier for the `<ThumbTexture>` publication:
/// `ScrollFrame_OnScrollRangeChanged` reaches the thumb through
/// `getglobal(bar:GetName().."ThumbTexture")`, which resolves only because our loader publishes a
/// named `<ThumbTexture>` as a global. If that publication regressed, this call would raise on a
/// nil index and `s.errors()` would not be empty.
///
/// (This used to claim the opposite — that we diverged to `GetThumbTexture()` "because our loader
/// never publishes a named `<ThumbTexture>`", and called it "this file's one Lua divergence". It
/// was already false when written: the XML has used `getglobal` throughout, at HEAD and now.
/// Corrected rather than deleted, because a doc comment asserting a divergence that does not exist
/// is exactly how someone later introduces a real one by "restoring" it.)
#[test]
fn a_scroll_frame_from_the_template_wires_its_bar_two_parents_deep() {
    let mut s = harness();
    s.run(
        r#"MyScroll = CreateFrame("ScrollFrame", "MyScroll", UIParent, "UIPanelScrollFrameTemplate")
           MyScroll:SetWidth(290) MyScroll:SetHeight(80)
           MyScroll:SetPoint("TOPLEFT", 20, -20)"#,
    )
    .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    for child in [
        "ScrollBar",
        "ScrollBarScrollUpButton",
        "ScrollBarScrollDownButton",
    ] {
        assert!(
            s.eval::<bool>(&format!(r#"return getglobal("MyScroll{child}") ~= nil"#))
                .unwrap(),
            "MyScroll{child} — the caller's name won through every $parent level"
        );
    }
    // ScrollFrame_OnLoad ran off the template: empty range, both arrows greyed, offset 0.
    assert_eq!(
        s.eval::<(f64, f64)>("return MyScrollScrollBar:GetMinMaxValues()")
            .unwrap(),
        (0.0, 0.0)
    );
    assert!(!s
        .eval::<bool>("return MyScrollScrollBarScrollUpButton:IsEnabled()")
        .unwrap());
    assert_eq!(s.eval::<i64>("return MyScroll.offset").unwrap(), 0);

    // A scroll child taller than the window, then the range change the reference's own handler
    // consumes — the leg that touches the thumb.
    s.run(
        r#"MyScrollChild = CreateFrame("Frame", "MyScrollChild", MyScroll)
           MyScrollChild:SetWidth(290) MyScrollChild:SetHeight(192)
           MyScroll:SetScrollChild(MyScrollChild)"#,
    )
    .unwrap();
    s.resolve();
    s.run("MyScroll:UpdateScrollChildRect()").unwrap();
    assert!(
        s.errors().is_empty(),
        "ScrollFrame_OnScrollRangeChanged ran clean: {:?}",
        s.errors()
    );
    assert_eq!(
        s.eval::<(f64, f64)>("return MyScrollScrollBar:GetMinMaxValues()")
            .unwrap(),
        (0.0, 112.0),
        "192px of content in an 80px window — the bar's range is the overflow"
    );
    assert!(
        s.eval::<bool>("return MyScrollScrollBarScrollDownButton:IsEnabled()")
            .unwrap(),
        "there is somewhere to scroll to, so the down arrow woke"
    );

    // And the frame's own <OnVerticalScroll> seats the bar and re-enables the up arrow.
    s.run("MyScroll:SetVerticalScroll(48)").unwrap();
    assert_eq!(
        s.eval::<f64>("return MyScrollScrollBar:GetValue()")
            .unwrap(),
        48.0
    );
    assert!(s
        .eval::<bool>("return MyScrollScrollBarScrollUpButton:IsEnabled()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// `UIOptionsCheckButtonTemplate` — 8 call sites, all in one addon's options panel, and a
/// two-generation chain: it overrides the size of `OptionsCheckButtonTemplate`, which adds the hit
/// rect and the click sound to `UICheckButtonTemplate`. The chain has to resolve through
/// `CreateFrame`'s fourth argument the same way it resolves through XML `inherits=`.
#[test]
fn the_options_check_button_resolves_its_whole_inheritance_chain() {
    let mut s = harness();
    s.run(
        r#"MyOpt = CreateFrame("CheckButton", "MyOpt", UIParent, "UIOptionsCheckButtonTemplate")"#,
    )
    .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert_eq!(
        s.eval::<(f64, f64)>("return MyOpt:GetWidth(), MyOpt:GetHeight()")
            .unwrap(),
        (26.0, 26.0),
        "UIOptionsFrame.xml's own size override, the LAST link in the chain"
    );
    let (_, right, _, _) = s
        .eval::<(f64, f64, f64, f64)>("return MyOpt:GetHitRectInsets()")
        .unwrap();
    assert_eq!(
        right, -100.0,
        "OptionsCheckButtonTemplate's label-catching hit rect, the MIDDLE link"
    );
    assert!(
        s.eval::<bool>(r#"return getglobal("MyOptText") ~= nil"#)
            .unwrap(),
        "and UICheckButtonTemplate's label, the ROOT link — still named after the caller"
    );

    // The middle link's OnClick is the reference's option-toggle sound pair.
    let _ = s.take_sounds();
    s.run("MyOpt:Click()").unwrap();
    assert!(
        !s.take_sounds().is_empty(),
        "clicking an options checkbox makes the reference's toggle sound"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **No invented names.** Every `virtual="true"` template these two files declare is a name the
/// real 1.12.1 client has, read from `reference/1.12-globals.tsv`.
///
/// Scoped to these two files on purpose: the rest of `assets/ui` is full of deliberately
/// benilla-shaped template names (`BenillaScrollBarTemplate`, `BenillaMacroPanelButtonTemplate`,
/// `OptionsRedButtonTemplate`), and a whole-tree sweep would be asserting something else. These
/// two files make the opposite claim — *these are the reference's own names, which is why an addon
/// can find them* — so that claim is the one worth gating. A template renamed to something
/// plausible-but-absent here is a template no addon will ever name.
#[test]
fn every_template_these_files_declare_is_a_real_1_12_name() {
    let tsv =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../reference/1.12-globals.tsv");
    let text = std::fs::read_to_string(&tsv).unwrap_or_else(|e| {
        panic!(
            "reading {}: {e} — regenerate with scripts/gen-reference-globals.py",
            tsv.display()
        )
    });
    let known: std::collections::HashSet<&str> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split('\t').next())
        .collect();

    let mut checked = 0;
    for file in ["UIPanelTemplates.xml", "OptionsFrameTemplates.xml"] {
        let src = std::fs::read_to_string(ui_dir().join(file)).unwrap();
        let doc = benilla_ui::framexml::parse(&src).unwrap();
        for item in &doc.items {
            let benilla_ui::framexml::TopLevel::Template(el) = item else {
                continue;
            };
            let name = el.name().expect("every template here is named");
            assert!(
                known.contains(name),
                "{file} declares '{name}', which is not a 1.12 global — \
                 a template the reference does not have is one no addon can name"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 18,
        "only {checked} templates swept — the sweep, not the files, is what broke"
    );
}
