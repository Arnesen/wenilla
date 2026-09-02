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
///
/// **Two of the templates under test are the REFERENCE's since 1860** — `FauxScrollFrameTemplate`
/// and `TabButtonTemplate` were ours until the dead-copy sweep, and both now come off the player's
/// chain from `Interface\FrameXML\UIPanelTemplates.xml`, seated below `UiPanels.xml` exactly as
/// the manifest seats it. So this list carries the chain pair and the loader below has to be able
/// to READ a chain entry, which a disk-only provider under `assets/ui` cannot.
const FILES: [&str; 7] = [
    "Fonts.xml",
    "MoneyFrame.xml",
    "UiPanels.xml",
    r"Interface\FrameXML\UIPanelTemplates.lua",
    r"Interface\FrameXML\UIPanelTemplates.xml",
    "UIParent.xml",
    "ScrollTemplates.xml",
];

fn load_ui(script: &UiScript) {
    let dir = std::path::Path::new(UI_DIR);
    // A manifest entry carrying a path separator is the PLAYER's own file and has to come off the
    // patch chain; a bare name is ours, under `assets/ui`. The shipped loader draws the same line
    // (`reference_ui::is_chain_entry`), and this test grew the chain half when 1860 moved two of
    // the templates it exercises onto it.
    let chain = benilla_formats::wow_data().and_then(|d| benilla_formats::open_chain(&d).ok());
    let read = |req: &str| -> Option<Vec<u8>> {
        let norm = req.replace('\\', "/");
        if norm.contains('/') {
            if let Some(c) = chain.as_ref() {
                if let Ok(b) = c.read(&norm) {
                    return Some(b);
                }
            }
        }
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        std::fs::read(dir.join(&norm))
            .or_else(|_| std::fs::read(dir.join(base)))
            .ok()
    };
    let provider = |req: &str| -> Option<Vec<u8>> { read(req) };
    for file in FILES {
        let bytes = read(file).unwrap_or_else(|| panic!("reading {file}"));
        // A `.lua` manifest entry is a CHUNK, not a document — the shipped loader draws the same
        // line, and `UIPanelTemplates` is split that way because the reference splits it that way.
        if file.to_ascii_lowercase().ends_with(".lua") {
            script
                .run_chunk_named(&bytes, &format!("@{file}"))
                .unwrap_or_else(|e| panic!("{file}: {e}"));
            continue;
        }
        let text = benilla_ui::source::decode(&bytes);
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
    let _data = benilla_formats::wow_data_or_skip!();
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

    // The template's authored `<Size>` is 115x32 in BOTH our retired copy and the reference's, but
    // the reference's `<OnLoad>` calls `PanelTemplates_TabResize(0)` — it FITS the tab to its text
    // on load, and a freshly created tab has none, so what survives is the two end caps. Ours left
    // the fit to an OnUpdate settle (1004), which is why this used to read the pre-fit 115. The
    // height is untouched by the fit and still the template's (1860).
    let (w, h) = s
        .eval::<(f32, f32)>("return ProbeTab:GetWidth(), ProbeTab:GetHeight()")
        .unwrap();
    assert_eq!(h, 32.0, "the template's own <Size> height");
    let caps = s
        .eval::<f32>("return 2 * BenillaTemplateProbeTabLeft:GetWidth()")
        .unwrap();
    // `caps` plus the empty label's one-unit floor (`FONTSTRING_MIN_SPAN`) — the point is that it
    // collapsed to its end caps, nowhere near the authored 115.
    assert!(
        w >= caps && w <= caps + 1.5,
        "an unlabelled tab fits down to its two end caps: {w} vs {caps}"
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

    // The template's <Scripts> reached the frame. **`OnLoad`, not `OnUpdate`, since 1860** — the
    // OnUpdate was our own text-fit settle (1004), and the reference's `TabButtonTemplate` declares
    // exactly one handler: an `<OnLoad>` that calls `PanelTemplates_TabResize(0)` and sizes the
    // highlight. That it ran is what the collapsed width above already proves.
    assert!(
        s.eval::<bool>(r#"return ProbeTab:GetScript("OnLoad") ~= nil"#)
            .unwrap(),
        "the template's OnLoad is installed"
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
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_ui(&s);
    let _ = s.take_warnings();
    let _ = s.take_errors();

    s.run(
        // **A `ScrollFrame`, not a `Frame`, since 1860.** `FauxScrollFrameTemplate` is now the
        // reference's own and the reference declares it as a `<ScrollFrame>` carrying a
        // `<ScrollChild>`; `framexml::merge` takes the OVERRIDING node's tag, so asking for a
        // "Frame" keeps the frame a Frame and the ScrollFrame-only parts — the scroll child among
        // them — cannot apply. Our retired copy was a plain `<Frame>`, which is why the corpus
        // idiom used to read that way here.
        r#"Scroller = CreateFrame("ScrollFrame", "BenillaTemplateProbeScroll", UIParent, "FauxScrollFrameTemplate")"#,
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
