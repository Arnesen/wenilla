//! `CreateFrame`'s fourth argument against the **real** `assets/ui` templates — the corpus idiom,
//! end to end.
//!
//! The unit tests for the runtime template path live in
//! `benilla-ui/src/script/tests/create_frame_template.rs` and drive synthetic templates. This one
//! exists because the thing an addon actually types is
//!
//! ```lua
//! local tab = CreateFrame("Button", "MyTab", UIParent, "TabButtonTemplate")
//! getglobal("MyTab".."Text"):SetText("Hi")
//! ```
//!
//! against a template *we* wrote, loaded the way the client loads it — six hundred lines of real
//! FrameXML above it, a font registry, an anchor graph — not a four-line fixture. Every assertion
//! below is a global an addon would reach for.

use benilla_ui::script::UiScript;

const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ui");

/// The prefix of `benilla.toc`'s load order these templates need: the font registry, the panel kit,
/// UIParent (the parent every addon passes), and the faux-scroll kit.
const FILES: [&str; 4] = [
    "Fonts.xml",
    "UiPanels.xml",
    "UIParent.xml",
    "ScrollTemplates.xml",
];

fn load_ui(script: &UiScript) {
    let dir = std::path::Path::new(UI_DIR);
    let provider = |req: &str| -> Option<Vec<u8>> {
        let norm = req.replace('\\', "/");
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        std::fs::read(dir.join(&norm))
            .or_else(|_| std::fs::read(dir.join(base)))
            .ok()
    };
    for file in FILES {
        let text = std::fs::read_to_string(dir.join(file))
            .unwrap_or_else(|e| panic!("reading {file}: {e}"));
        let doc =
            benilla_ui::framexml::parse(&text).unwrap_or_else(|e| panic!("parsing {file}: {e}"));
        let report = benilla_ui::loader::load(script, &doc, &provider);
        assert!(
            report.errors.is_empty(),
            "{file} loaded with errors: {:#?}",
            report.errors
        );
    }
}

/// The line an addon writes, and the globals it reads on the next one.
///
/// `TabButtonTemplate` (UiPanels.xml) is a `<Button>` carrying a `<Size>`, six `<Layers>` slices,
/// a `<ButtonText name="$parentText">`, a `<HighlightTexture name="$parentHighlightTexture">`, the
/// three state fonts and an `<OnUpdate>` — i.e. every decoration pass at once. Instantiating it as
/// `BenillaTemplateProbeTab` must publish `BenillaTemplateProbeTab*`, and must publish nothing
/// named after the template.
#[test]
fn a_real_template_reaches_an_addon_through_create_frame() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_ui(&s);
    // Clear anything the UI load itself had to say; what follows is this call's alone.
    let _ = s.take_warnings();
    let _ = s.take_errors();

    s.run(
        r#"ProbeTab = CreateFrame("Button", "BenillaTemplateProbeTab", UIParent, "TabButtonTemplate")"#,
    )
    .expect("the corpus idiom must not error");

    assert_eq!(
        s.eval::<(f32, f32)>("return ProbeTab:GetWidth(), ProbeTab:GetHeight()")
            .unwrap(),
        (115.0, 32.0),
        "the template's own <Size>"
    );
    assert_eq!(
        s.eval::<String>("return ProbeTab:GetParent():GetName()")
            .unwrap(),
        "UIParent",
        "the parent argument, not the template's idea of one"
    );

    // The globals an addon addresses the parts by — every one named against the INSTANCE. The
    // reference's own kit does exactly this (`getglobal(tabName.."Text")` in PanelTemplates).
    for suffix in [
        "Text",             // <ButtonText name="$parentText">
        "HighlightTexture", // <HighlightTexture name="$parentHighlightTexture">
        "Left",             // <Layers> slices
        "Middle",
        "Right",
        "LeftDisabled",
    ] {
        assert!(
            s.eval::<bool>(&format!(
                r#"return getglobal("BenillaTemplateProbeTab{suffix}") ~= nil"#
            ))
            .unwrap(),
            "BenillaTemplateProbeTab{suffix} must exist"
        );
        assert!(
            s.eval::<bool>(&format!(
                r#"return getglobal("TabButtonTemplate{suffix}") == nil"#
            ))
            .unwrap(),
            "nothing may be published under the TEMPLATE's name (TabButtonTemplate{suffix})"
        );
    }

    // The label is a real FontString the addon can drive.
    s.run(r#"getglobal("BenillaTemplateProbeTabText"):SetText("Hi")"#)
        .unwrap();
    assert_eq!(
        s.eval::<String>(r#"return getglobal("BenillaTemplateProbeTabText"):GetText()"#)
            .unwrap(),
        "Hi"
    );

    // The template's <Scripts> reached the frame.
    assert!(
        s.eval::<bool>(r#"return ProbeTab:GetScript("OnUpdate") ~= nil"#)
            .unwrap(),
        "the template's OnUpdate is installed"
    );

    assert!(s.take_errors().is_empty());
    assert_eq!(
        s.take_warnings(),
        Vec::<String>::new(),
        "a template that resolves cleanly has nothing to report"
    );
}

/// The template the corpus actually asks for, and the handler that proves the naming rule.
///
/// `FauxScrollFrameTemplate` is the most-instantiated template in the 218-addon vanilla corpus that
/// benilla declares at all — 15 `CreateFrame` call sites across AckisRecipeList, FonzAppraiser,
/// Leader, Optional and oRA2. Its `<OnLoad>` is `ScrollFrame_OnLoad`, whose very first line is
///
/// ```lua
/// getglobal(this:GetName() .. "ScrollBarScrollDownButton"):Disable()
/// ```
///
/// so the handler **dies on a nil index** unless the caller's own name won all the way down: the
/// template's nested `<Slider name="$parentScrollBar">` had to become `<caller>ScrollBar`, and
/// *that* slider's own inherited `$parentScrollDownButton` had to become
/// `<caller>ScrollBarScrollDownButton`. Two levels of `$parent`, composed through a runtime
/// template, checked by shipped code rather than by an assertion we wrote to match.
#[test]
fn the_corpus_favourite_template_composes_parent_two_levels_deep() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_ui(&s);
    let _ = s.take_warnings();
    let _ = s.take_errors();

    s.run(
        r#"Scroller = CreateFrame("Frame", "BenillaTemplateProbeScroll", UIParent, "FauxScrollFrameTemplate")"#,
    )
    .expect("the corpus's most-used template must not error");

    // `ScrollFrame_OnLoad` ran to completion — it set `this.offset`, its last statement, which it
    // cannot reach if either getglobal above returned nil.
    assert_eq!(
        s.eval::<f32>("return Scroller.offset").unwrap(),
        0.0,
        "the template's OnLoad ran to its last line"
    );
    for suffix in [
        "ScrollBar",
        "ScrollBarScrollUpButton",
        "ScrollBarScrollDownButton",
    ] {
        assert!(
            s.eval::<bool>(&format!(
                r#"return getglobal("BenillaTemplateProbeScroll{suffix}") ~= nil"#
            ))
            .unwrap(),
            "BenillaTemplateProbeScroll{suffix} must exist"
        );
    }
    assert_eq!(
        s.eval::<String>("return BenillaTemplateProbeScrollScrollBar:GetParent():GetName()")
            .unwrap(),
        "BenillaTemplateProbeScroll"
    );
    assert!(
        s.eval::<bool>(r#"return getglobal("FauxScrollFrameTemplateScrollBar") == nil"#)
            .unwrap(),
        "nothing may be published under the template's own name"
    );

    assert!(s.take_errors().is_empty());
    assert_eq!(s.take_warnings(), Vec::<String>::new());
}
