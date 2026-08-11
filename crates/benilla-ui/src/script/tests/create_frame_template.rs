//! `CreateFrame(kind, name, parent, inherits)` — the **runtime** template path
//! ([`crate::loader::apply_template`]).
//!
//! `local b = CreateFrame("Button", "MyButton", UIParent, "UIPanelButtonTemplate")` is the single
//! most common line in the addon corpus, and until the loader's post-`CreateFrame` steps were
//! factored into `Loader::decorate` the fourth argument was dropped on the floor: the addon got a
//! bare frame with no art, no regions, no scripts, and a warning on a channel it never reads.
//!
//! The load-bearing property every test here turns on is that **the caller's own name wins** — a
//! template instantiated as `Mine` publishes `MineTexture`, never `ThatTemplateTexture`, because
//! `getglobal("MineTexture")` is what the addon's next line calls.

use super::common::script;
use crate::script::UiScript;

/// Register templates by loading a small FrameXML document — the same door an addon's own `.xml`
/// comes through, so nothing here is a test-only shortcut into the registry.
fn register(s: &UiScript, xml: &str) {
    let doc = crate::framexml::parse(xml).expect("valid FrameXML");
    let report = crate::loader::load(s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "fixture document failed to load: {:?}",
        report.errors
    );
}

/// The whole shape at once: a virtual template's `<Size>`, `<Anchors>`, `<Layers>` texture and
/// `<Scripts><OnLoad>` all reach a frame built by `CreateFrame`'s fourth argument — and the region
/// is named against **the instance**, not the template.
#[test]
fn a_runtime_template_brings_size_anchors_regions_and_a_fired_onload() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    register(
        &s,
        r#"<Ui>
             <Frame name="Host"/>
             <Frame name="ProbeTemplate" virtual="true">
               <Size><AbsDimension x="160" y="40"/></Size>
               <Anchors>
                 <Anchor point="TOPLEFT" relativeTo="$parent" relativePoint="BOTTOMLEFT">
                   <Offset><AbsDimension x="7" y="-3"/></Offset>
                 </Anchor>
               </Anchors>
               <Layers>
                 <Layer level="ARTWORK">
                   <Texture name="$parentTexture" file="Interface\Probe\Art"/>
                 </Layer>
               </Layers>
               <Scripts>
                 <OnLoad>ProbeLoadedAs = self:GetName(); ProbeLoadWidth = self:GetWidth()</OnLoad>
               </Scripts>
             </Frame>
           </Ui>"#,
    );

    s.run(r#"Mine = CreateFrame("Frame", "Mine", Host, "ProbeTemplate")"#)
        .unwrap();

    assert_eq!(
        s.eval::<(f32, f32)>("return Mine:GetWidth(), Mine:GetHeight()")
            .unwrap(),
        (160.0, 40.0),
        "the template's <Size>"
    );

    // The template's own <Anchors> substitute `$parent` against the frame's PARENT (rf27) — the
    // instance's enclosing frame, not the instance itself.
    let (point, rel, rel_point, x, y): (String, String, String, f32, f32) = s
        .eval(
            "local p, r, rp, ox, oy = Mine:GetPoint(1) \
             return p, r:GetName(), rp, ox, oy",
        )
        .unwrap();
    assert_eq!(
        (point.as_str(), rel.as_str(), rel_point.as_str(), x, y),
        ("TOPLEFT", "Host", "BOTTOMLEFT", 7.0, -3.0)
    );

    // The region carries the INSTANCE's name. This is the whole point: `MineTexture` is what an
    // addon reaches for, and `ProbeTemplateTexture` would be useless to it.
    assert!(
        s.eval::<bool>(r#"return getglobal("MineTexture") ~= nil"#)
            .unwrap(),
        "the template's $parentTexture resolved against the caller's name"
    );
    assert!(
        s.eval::<bool>(r#"return getglobal("ProbeTemplateTexture") == nil"#)
            .unwrap(),
        "nothing may be named after the template"
    );

    // OnLoad fired inside CreateFrame, after the frame was decorated — the reference's ordering,
    // and what `local f = CreateFrame(...); f:Foo()` on the next line assumes.
    assert_eq!(s.eval::<String>("return ProbeLoadedAs").unwrap(), "Mine");
    assert_eq!(s.eval::<f32>("return ProbeLoadWidth").unwrap(), 160.0);

    assert!(s.take_errors().is_empty());
    assert!(s.take_warnings().is_empty());
}

/// A template's nested `<Frames>` child is named against the caller too, and is really parented to
/// the new frame — the `$parent` chain composes through `CreateFrame` exactly as through XML.
#[test]
fn a_nested_frames_child_is_named_against_the_caller() {
    let mut s = script();
    register(
        &s,
        r#"<Ui>
             <Frame name="NestTemplate" virtual="true">
               <Frames>
                 <Frame name="$parentInner">
                   <Size><AbsDimension x="11" y="12"/></Size>
                   <Frames>
                     <Frame name="$parentDeep"/>
                   </Frames>
                 </Frame>
               </Frames>
             </Frame>
           </Ui>"#,
    );
    s.run(r#"Outer = CreateFrame("Frame", "Outer", nil, "NestTemplate")"#)
        .unwrap();

    assert_eq!(
        s.eval::<String>("return OuterInner:GetParent():GetName()")
            .unwrap(),
        "Outer"
    );
    assert_eq!(
        s.eval::<(f32, f32)>("return OuterInner:GetWidth(), OuterInner:GetHeight()")
            .unwrap(),
        (11.0, 12.0)
    );
    // Two levels deep, so the chain is composing rather than substituting once.
    assert_eq!(
        s.eval::<String>("return OuterInnerDeep:GetParent():GetName()")
            .unwrap(),
        "OuterInner"
    );
    assert!(s
        .eval::<bool>(r#"return getglobal("NestTemplateInner") == nil"#)
        .unwrap());
    assert!(s.take_errors().is_empty());
    assert!(s.take_warnings().is_empty());
}

/// `inherits="A, B"` is a LIST, and the runtime path resolves it through the very same
/// [`crate::framexml::expand`] the XML path uses — including chains, splice order and the cycle
/// guard, none of which is written twice.
#[test]
fn an_inherits_list_picks_up_every_template_in_it() {
    let mut s = script();
    register(
        &s,
        r#"<Ui>
             <Frame name="TemplRoot" virtual="true" alpha="0.25">
               <Size><AbsDimension x="50" y="60"/></Size>
             </Frame>
             <Frame name="TemplA" virtual="true" inherits="TemplRoot"/>
             <Frame name="TemplB" virtual="true">
               <Layers>
                 <Layer level="ARTWORK">
                   <Texture name="$parentBTex" file="Interface\Probe\B"/>
                 </Layer>
               </Layers>
             </Frame>
           </Ui>"#,
    );
    s.run(r#"Both = CreateFrame("Frame", "Both", nil, "TemplA, TemplB")"#)
        .unwrap();

    // From A (itself inheriting TemplRoot — the chain resolves).
    assert_eq!(
        s.eval::<(f32, f32)>("return Both:GetWidth(), Both:GetHeight()")
            .unwrap(),
        (50.0, 60.0)
    );
    assert_eq!(s.eval::<f32>("return Both:GetAlpha()").unwrap(), 0.25);
    // From B.
    assert!(s
        .eval::<bool>(r#"return getglobal("BothBTex") ~= nil"#)
        .unwrap());
    assert!(s.take_errors().is_empty());
    assert!(s.take_warnings().is_empty());
}

/// An unusable template name never takes the frame with it — the reference creates the frame
/// anyway, and an addon's next line already assumes it did. Both flavours of "unusable" are here:
/// a name nothing declared, and a name declared **without `virtual="true"`** (only virtual
/// elements are ever registered as templates, so from the registry's side the two look alike).
#[test]
fn an_unusable_template_still_returns_a_usable_frame_and_says_so() {
    let mut s = script();
    register(&s, r#"<Ui><Frame name="PlainFrame"/></Ui>"#);

    for (frame, template) in [("Orphan", "NoSuchTemplate"), ("Plainer", "PlainFrame")] {
        s.run(&format!(
            r#"{frame} = CreateFrame("Button", "{frame}", nil, "{template}")
               {frame}:SetWidth(5)
               {frame}:Show()"#
        ))
        .unwrap_or_else(|e| panic!("{frame} must be usable: {e}"));
        assert_eq!(
            s.eval::<String>(&format!("return {frame}:GetName()"))
                .unwrap(),
            frame
        );
        assert_eq!(
            s.eval::<f32>(&format!("return {frame}:GetWidth()"))
                .unwrap(),
            5.0
        );

        let warnings = s.take_warnings();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains(template) && w.contains(frame)),
            "a warning must name both the template and the frame it was asked for: {warnings:?}"
        );
        assert!(s.take_errors().is_empty(), "a bad template is not an error");
    }
}

/// The two shape mismatches, neither of which may panic or drop the frame.
///
/// A **region** template (a virtual `<Texture>`) asked for as a frame: its frame-shaped content
/// still lands, its region-only content cannot, and the message says which.
///
/// A **kind that disagrees with the template's tag**: the frame keeps the kind `CreateFrame` was
/// given — no template can retype a frame that already exists — so the `<Button>`-only parts of a
/// Button template simply do not apply to a Frame, and, crucially, are not *attempted* (calling
/// `SetNormalTexture` on a plain Frame would be an error, not a warning).
#[test]
fn a_region_template_or_a_mismatched_kind_is_named_never_fatal() {
    let mut s = script();
    register(
        &s,
        r#"<Ui>
             <Texture name="RegionTemplate" virtual="true" file="Interface\Probe\Art">
               <Size><AbsDimension x="33" y="44"/></Size>
             </Texture>
             <Button name="ProbeButtonTemplate" virtual="true">
               <Size><AbsDimension x="90" y="22"/></Size>
               <NormalTexture file="Interface\Probe\Normal"/>
               <ButtonText name="$parentText"/>
             </Button>
           </Ui>"#,
    );

    // 1 · a region template on a frame.
    s.run(r#"FromRegion = CreateFrame("Frame", "FromRegion", nil, "RegionTemplate")"#)
        .unwrap();
    assert_eq!(
        s.eval::<(f32, f32)>("return FromRegion:GetWidth(), FromRegion:GetHeight()")
            .unwrap(),
        (33.0, 44.0),
        "the frame-shaped half of a region template still applies"
    );
    let warnings = s.take_warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("RegionTemplate") && w.contains("REGION template")),
        "the region/frame mismatch must be named: {warnings:?}"
    );
    assert!(s.take_errors().is_empty());

    // 2 · a Button template asked for as a Frame.
    s.run(r#"AsFrame = CreateFrame("Frame", "AsFrame", nil, "ProbeButtonTemplate")"#)
        .unwrap();
    assert_eq!(
        s.eval::<f32>("return AsFrame:GetWidth()").unwrap(),
        90.0,
        "the generic half of the template still applies"
    );
    assert!(
        s.eval::<bool>(r#"return getglobal("AsFrameText") == nil"#)
            .unwrap(),
        "a Frame has no ButtonText slot, so nothing was invented for it"
    );
    let warnings = s.take_warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("ProbeButtonTemplate") && w.contains("<Button>")),
        "the kind mismatch must be named: {warnings:?}"
    );
    assert!(
        s.take_errors().is_empty(),
        "the Button-only steps are SKIPPED, not attempted and failed"
    );

    // 3 · the same template, asked for as the kind it was written as — the control.
    s.run(r#"AsButton = CreateFrame("Button", "AsButton", nil, "ProbeButtonTemplate")"#)
        .unwrap();
    assert_eq!(s.eval::<f32>("return AsButton:GetWidth()").unwrap(), 90.0);
    assert!(
        s.eval::<bool>(r#"return getglobal("AsButtonText") ~= nil"#)
            .unwrap(),
        "the <ButtonText name=\"$parentText\"> label, named against the caller"
    );
    assert!(s.take_errors().is_empty());
    assert!(
        s.take_warnings().is_empty(),
        "a matching kind has nothing to report"
    );
}

/// **`<ScrollChild>` gives a ScrollFrame its range** — the loader element that was missing, and
/// the whole remaining distance for an addon's scrolling list (decision 1205).
///
/// wow-5875-re `rf28-typed-widget-loadxml.md`: the single child is instantiated via the same
/// RF-0026 path `<Frames>` uses, then stored as the scroll child. Without it a `ScrollFrame` has
/// nothing to pan, so `GetVerticalScrollRange()` is 0, `SetVerticalScroll` clamps to 0, and
/// `OnVerticalScroll` fires with 0 forever — the list never moves and nothing errors, which is why
/// no instrument could see it.
#[test]
fn a_scroll_child_element_gives_the_frame_a_real_scroll_range() {
    let mut s = script();
    s.set_screen_size(1024.0, 768.0);
    register(
        &s,
        r#"<Ui>
            <ScrollFrame name="Roller">
              <Size><AbsDimension x="200" y="100"/></Size>
              <Anchors><Anchor point="TOPLEFT"/></Anchors>
              <ScrollChild>
                <Frame name="$parentChild">
                  <Size><AbsDimension x="200" y="400"/></Size>
                </Frame>
              </ScrollChild>
            </ScrollFrame>
          </Ui>"#,
    );
    s.resolve();

    // The child exists, is named against the FRAME (rf27's `$parent`), and is the scroll child.
    assert!(s.eval::<bool>("return RollerChild ~= nil").unwrap());
    assert!(s
        .eval::<bool>("return Roller:GetScrollChild() == RollerChild")
        .unwrap());
    // ...and the range is the overhang: a 400-tall child in a 100-tall window scrolls 300.
    assert_eq!(
        s.eval::<f64>("return Roller:GetVerticalScrollRange()")
            .unwrap(),
        300.0,
        "no child means range 0, which is the silent failure this element fixes"
    );
    // The scroll actually takes, rather than clamping to zero.
    s.run("Roller:SetVerticalScroll(120)").unwrap();
    assert_eq!(
        s.eval::<f64>("return Roller:GetVerticalScroll()").unwrap(),
        120.0
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// An empty `<ScrollChild>` is an error — the reference's own behaviour (`rf28`), and the honest
/// one: a ScrollFrame that declares a child and has none is a typo, not a design.
#[test]
fn an_empty_scroll_child_is_reported() {
    let s = script();
    let doc = crate::framexml::parse(
        r#"<Ui><ScrollFrame name="Hollow"><ScrollChild/></ScrollFrame></Ui>"#,
    )
    .expect("valid FrameXML");
    let report = crate::loader::load(&s, &doc, &|_| None);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("<ScrollChild> is empty")),
        "errors: {:?}",
        report.errors
    );
    // ...and the frame still exists — the client logs and carries on (decision 0068).
    assert!(s.eval::<bool>("return Hollow ~= nil").unwrap());
}
