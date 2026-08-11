//! Font objects as first-class Lua objects ([`crate::script::font`]).
//!
//! Every test here is driven **the way the 218-addon corpus drives it** — the bare-global argument,
//! the no-arg `CreateFontString`, `CreateFont` — rather than the way our own shipped XML does,
//! because the string-name form our XML uses was the only one we accepted and it is the form
//! almost nobody writes (6 corpus sites against 3,180).

use super::common::script;
use crate::script::{FontObject, JustifyH, Outline, QuadContent, UiScript};

/// Load a FrameXML fragment through the real loader, asserting it reported no errors.
fn load(s: &UiScript, xml: &str) {
    let doc = crate::framexml::parse(xml).expect("valid FrameXML");
    let report = crate::loader::load(s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "loader errors: {:?}",
        report.errors
    );
}

/// The resolved `(face, height, colour)` of the one text quad on screen.
fn painted(s: &UiScript, text: &str) -> (Option<String>, Option<f32>, Option<[f32; 4]>) {
    s.extract()
        .into_iter()
        .find_map(|q| match q.content {
            QuadContent::Text {
                text: Some(ref t),
                ref font,
                font_height,
                color,
                ..
            } if t == text => Some((font.clone(), font_height, color)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no text quad reading {text:?}"))
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Publication
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A `<Font name="X">` in FrameXML becomes the **global `X`**, an object of type `Font`.
///
/// This is the whole gap: we registered the resolved record and never named it, so every
/// `SetFontObject(GameFontNormal)` in the ecosystem was handed nil.
#[test]
fn a_declared_font_is_published_as_a_global_object() {
    let s = script();
    load(
        &s,
        r#"<Ui>
             <Font name="GameFontNormal" font="Fonts\FRIZQT__.TTF" justifyH="LEFT">
               <FontHeight><AbsValue val="12"/></FontHeight>
               <Color r="1" g="0.82" b="0"/>
               <Shadow><Offset><AbsDimension x="1" y="-1"/></Offset><Color r="0" g="0" b="0"/></Shadow>
             </Font>
           </Ui>"#,
    );
    assert_eq!(
        s.eval::<String>("return GameFontNormal:GetObjectType()")
            .unwrap(),
        "Font"
    );
    assert!(s
        .eval::<bool>("return GameFontNormal:IsObjectType('Font')")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return GameFontNormal:GetName()").unwrap(),
        "GameFontNormal"
    );
    // Object identity is stable: two reads of the global are the same object, which is what makes
    // `if fs:GetFontObject() == GameFontNormal` work at all.
    assert!(s
        .eval::<bool>("return GameFontNormal == GameFontNormal")
        .unwrap());

    // The declared paint reads back through the FontInstance getters.
    assert_eq!(
        s.eval::<(String, f32, String)>("return GameFontNormal:GetFont()")
            .unwrap(),
        ("Fonts\\FRIZQT__.TTF".to_string(), 12.0, String::new())
    );
    let (r, g, b, _) = s
        .eval::<(f32, f32, f32, f32)>("return GameFontNormal:GetTextColor()")
        .unwrap();
    assert_eq!((r, g, b), (1.0, 0.82, 0.0));
    assert_eq!(
        s.eval::<(f32, f32)>("return GameFontNormal:GetShadowOffset()")
            .unwrap(),
        (1.0, -1.0)
    );
    assert_eq!(
        s.eval::<(f32, f32, f32, f32)>("return GameFontNormal:GetShadowColor()")
            .unwrap(),
        (0.0, 0.0, 0.0, 1.0)
    );
    assert_eq!(
        s.eval::<String>("return GameFontNormal:GetJustifyH()")
            .unwrap(),
        "LEFT"
    );
}

/// **`Tablet-2.0.lua:289`, verbatim.** `_, headerSize = GameTooltipHeaderText:GetFont()` — the
/// single most-called font-object read in the corpus (268 sites across the four embedded Ace
/// libraries). Until the global existed, Tablet fell to its hardcoded 14/12 guard branch.
#[test]
fn tablet_reads_the_tooltip_header_size_off_the_global() {
    let s = script();
    load(
        &s,
        r#"<Ui>
             <Font name="GameTooltipText" font="Fonts\FRIZQT__.TTF">
               <FontHeight><AbsValue val="12"/></FontHeight>
             </Font>
             <Font name="GameTooltipHeaderText" inherits="GameTooltipText">
               <FontHeight><AbsValue val="14"/></FontHeight>
             </Font>
           </Ui>"#,
    );
    let (header, normal) = s
        .eval::<(f32, f32)>(
            r#"
            local headerSize, normalSize
            if GameTooltipHeaderText then
                _, headerSize = GameTooltipHeaderText:GetFont()
            else
                headerSize = 14
            end
            if GameTooltipText then
                _, normalSize = GameTooltipText:GetFont()
            else
                normalSize = 12
            end
            return headerSize, normalSize
        "#,
        )
        .unwrap();
    assert_eq!((header, normal), (14.0, 12.0));
}

/// **Requirement 5: publishing must not change what `inherits=` resolves to.** The chain is still
/// flattened once, at load, and the published object carries the *flattened* values — the derived
/// font sees the parent's face and its own height override.
#[test]
fn an_inheriting_font_still_flattens_to_the_same_values() {
    let s = script();
    load(
        &s,
        r#"<Ui>
             <Font name="MasterFont" font="Fonts\FRIZQT__.TTF" justifyH="CENTER">
               <FontHeight><AbsValue val="10"/></FontHeight>
               <Color r="1" g="1" b="1"/>
               <Shadow><Offset><AbsDimension x="1" y="-1"/></Offset><Color r="0" g="0" b="0"/></Shadow>
             </Font>
             <Font name="DerivedFont" inherits="MasterFont" outline="NORMAL">
               <FontHeight><AbsValue val="18"/></FontHeight>
             </Font>
           </Ui>"#,
    );
    // The Rust-side record — unchanged by publication.
    let derived = s.font_object("DerivedFont").expect("registered");
    assert_eq!(derived.font.as_deref(), Some("Fonts\\FRIZQT__.TTF"));
    assert_eq!(derived.height, Some(18.0));
    assert_eq!(derived.color, Some([1.0, 1.0, 1.0, 1.0]));
    assert_eq!(derived.outline, Outline::Normal);
    assert_eq!(derived.justify_h, Some(JustifyH::Center));
    assert!(derived.shadow.is_some(), "the shadow inherits too");
    // …and the same values through the published object.
    assert_eq!(
        s.eval::<(String, f32, String)>("return DerivedFont:GetFont()")
            .unwrap(),
        (
            "Fonts\\FRIZQT__.TTF".to_string(),
            18.0,
            "OUTLINE".to_string()
        )
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// SetFontObject / GetFontObject
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// **`Gratuity-2.0.lua:47-59`, verbatim** — the block that took five corpus addons down at load
/// with `bad argument #2: error converting Lua nil to String`.
///
/// Three separate things have to hold for it: `CreateFontString()` with **no arguments**,
/// `SetFontObject` taking the **object**, and `AddFontStrings` existing at all (fixing only the
/// first two moves the death one line down).
#[test]
fn gratuity_builds_its_thirty_line_scan_tooltip() {
    let s = script();
    load(
        &s,
        r#"<Ui>
             <Font name="GameFontNormal" font="Fonts\FRIZQT__.TTF">
               <FontHeight><AbsValue val="12"/></FontHeight>
               <Color r="1" g="0.82" b="0"/>
             </Font>
           </Ui>"#,
    );
    s.run(
        r#"
        vars = { Llines = {}, Rlines = {} }
        local tt = CreateFrame("GameTooltip")
        vars.tooltip = tt
        tt:SetOwner(tt, "ANCHOR_NONE")
        for i = 1, 30 do
            vars.Llines[i], vars.Rlines[i] = tt:CreateFontString(), tt:CreateFontString()
            vars.Llines[i]:SetFontObject(GameFontNormal)
            vars.Rlines[i]:SetFontObject(GameFontNormal)
            tt:AddFontStrings(vars.Llines[i], vars.Rlines[i])
        end
    "#,
    )
    .expect("Gratuity's CreateTooltip must not raise");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // The font object actually landed on the lines it made.
    assert_eq!(
        s.eval::<(String, f32)>("return vars.Llines[7]:GetFont()")
            .unwrap(),
        ("Fonts\\FRIZQT__.TTF".to_string(), 12.0)
    );
    assert_eq!(
        s.eval::<String>("return vars.Rlines[30]:GetFontObject():GetName()")
            .unwrap(),
        "GameFontNormal"
    );
    // And Gratuity's own next move — `ClearLines` over the grown stack — still works.
    s.run("vars.tooltip:ClearLines()").unwrap();
}

/// The object form and the string form must resolve to **the same paint**. Both are kept: the
/// object is what 3,180 of 3,186 corpus sites pass, the string is what our own `assets/ui` and 6
/// corpus sites pass.
#[test]
fn set_font_object_takes_the_object_or_the_name() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.register_font_object(
        "GameFontHighlightSmall",
        FontObject {
            font: Some("Fonts\\ARIALN.TTF".into()),
            height: Some(11.0),
            color: Some([0.25, 0.5, 0.75, 1.0]),
            outline: Outline::Thick,
            justify_h: None,
            justify_v: None,
            shadow: None,
        },
    );
    s.run(
        r#"
        f = CreateFrame("Frame", "TwoWays")
        f:SetWidth(200); f:SetHeight(40); f:SetPoint("CENTER")
        byObject = f:CreateFontString(nil, "ARTWORK")
        byObject:SetText("obj")
        byObject:SetFontObject(GameFontHighlightSmall)
        byName = f:CreateFontString(nil, "ARTWORK")
        byName:SetText("str")
        byName:SetFontObject("GameFontHighlightSmall")
    "#,
    )
    .unwrap();
    s.resolve();
    assert_eq!(painted(&s, "obj"), painted(&s, "str"));
    assert_eq!(
        painted(&s, "obj"),
        (
            Some("Fonts\\ARIALN.TTF".into()),
            Some(11.0),
            Some([0.25, 0.5, 0.75, 1.0])
        )
    );
    // …and both name the same object back.
    assert!(s
        .eval::<bool>("return byObject:GetFontObject() == byName:GetFontObject()")
        .unwrap());

    // nil is the THIRD form the reference's own usage string names
    // (`SetFontObject(font or "font" or nil)`, `.rdata 0x87c5cc`): it severs the link and leaves
    // the paint standing.
    s.run("byName:SetFontObject(nil)").unwrap();
    assert!(s
        .eval::<bool>("return byName:GetFontObject() == nil")
        .unwrap());
    s.resolve();
    assert_eq!(
        painted(&s, "str").1,
        Some(11.0),
        "unlinking must not repaint"
    );
    // A frame or an unknown name is still an ERROR, never a silent no-op (1203/1205/1211's class).
    assert!(s.run("byName:SetFontObject(f)").is_err());
    assert!(s.run("byName:SetFontObject('NoSuchFont')").is_err());
}

/// `GetFontObject` returns the **object**, not a name — because `Dewdrop-2.0.lua:2181` indexes the
/// result immediately: `button.text:SetTextColor(button.text:GetFontObject():GetTextColor())`.
/// 65 sites across 62 corpus addons do exactly this.
#[test]
fn dewdrop_recolors_a_row_from_its_own_font_object() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.register_font_object(
        "GameFontHighlightSmall",
        FontObject {
            font: Some("Fonts\\FRIZQT__.TTF".into()),
            height: Some(10.0),
            color: Some([1.0, 1.0, 1.0, 1.0]),
            outline: Outline::None,
            justify_h: None,
            justify_v: None,
            shadow: None,
        },
    );
    s.run(
        r#"
        f = CreateFrame("Frame", "DdRow")
        f:SetWidth(120); f:SetHeight(16); f:SetPoint("CENTER")
        button = { text = f:CreateFontString(nil, "ARTWORK") }
        button.text:SetText("row")
        button.text:SetFontObject(GameFontHighlightSmall)
        button.text:SetTextColor(button.text:GetFontObject():GetTextColor())
    "#,
    )
    .expect("Dewdrop's row recolor must not raise");
    s.resolve();
    assert_eq!(painted(&s, "row").2, Some([1.0, 1.0, 1.0, 1.0]));
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Mutability — the decision, pinned
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// **The mutability decision.** A font object is a shared record with a **live** link: mutating it
/// re-paints every FontString that inherits it, which is why addons mutate one at all. What a
/// FontString set *for itself* since its own `SetFontObject` survives that re-paint — our
/// `FONTINSTANCE+0x038 explicitlySetMask`.
///
/// If this ever regresses to a one-shot copy, `inherited` stops moving and the test says so; if the
/// mask is dropped, `overridden` loses its red.
#[test]
fn mutating_a_font_object_repaints_everything_that_inherits_it() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.register_font_object(
        "ThemeFont",
        FontObject {
            font: Some("Fonts\\FRIZQT__.TTF".into()),
            height: Some(12.0),
            color: Some([1.0, 1.0, 1.0, 1.0]),
            outline: Outline::None,
            justify_h: None,
            justify_v: None,
            shadow: None,
        },
    );
    s.run(
        r#"
        f = CreateFrame("Frame", "ThemeHost")
        f:SetWidth(200); f:SetHeight(60); f:SetPoint("CENTER")
        a = f:CreateFontString(nil, "ARTWORK"); a:SetText("inherited")
        a:SetFontObject(ThemeFont)
        b = f:CreateFontString(nil, "ARTWORK"); b:SetText("overridden")
        b:SetFontObject(ThemeFont)
        b:SetTextColor(1, 0, 0)
    "#,
    )
    .unwrap();
    s.resolve();
    assert_eq!(painted(&s, "inherited").1, Some(12.0));
    assert_eq!(painted(&s, "overridden").2, Some([1.0, 0.0, 0.0, 1.0]));

    // The mutation the whole feature exists for.
    s.run(
        r#"
        ThemeFont:SetFont("Fonts\\MORPHEUS.TTF", 20)
        ThemeFont:SetTextColor(0, 1, 0)
    "#,
    )
    .unwrap();
    s.resolve();

    // Both followed the face and size…
    assert_eq!(
        painted(&s, "inherited"),
        (
            Some("Fonts\\MORPHEUS.TTF".into()),
            Some(20.0),
            Some([0.0, 1.0, 0.0, 1.0])
        )
    );
    assert_eq!(
        painted(&s, "overridden").0,
        Some("Fonts\\MORPHEUS.TTF".into())
    );
    assert_eq!(painted(&s, "overridden").1, Some(20.0));
    // …but the one that had set its own colour kept it.
    assert_eq!(
        painted(&s, "overridden").2,
        Some([1.0, 0.0, 0.0, 1.0]),
        "an explicitly-set property must survive a font-object mutation"
    );

    // §5-verified: severance SURVIVES a re-point. The reference's inheritMask bit (`+0x2c`,
    // FontString `+0xd4`) is cleared by the local setter and never restored, so "a FontString that
    // set its own colour stays severed even across a later SetFontObject". Our first cut reset the
    // mask here and this assertion was its inverse.
    s.run("b:SetFontObject(ThemeFont)").unwrap();
    s.resolve();
    assert_eq!(
        painted(&s, "overridden").2,
        Some([1.0, 0.0, 0.0, 1.0]),
        "a re-point must not restore inheritance of a severed property"
    );
    // Dewdrop still works, because it re-reads the colour off the new object explicitly every
    // refresh (`Dewdrop-2.0.lua:2181`) rather than relying on inheritance.
    s.run("b:SetTextColor(b:GetFontObject():GetTextColor())")
        .unwrap();
    s.resolve();
    assert_eq!(painted(&s, "overridden").2, Some([0.0, 1.0, 0.0, 1.0]));
}

/// A mutation of a font object nobody inherits touches nothing, and a FontString that inherits a
/// *different* object is not caught in the sweep.
#[test]
fn propagation_is_scoped_to_the_object_that_changed() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    for (name, face) in [("FontOne", "Fonts\\ONE.TTF"), ("FontTwo", "Fonts\\TWO.TTF")] {
        s.register_font_object(
            name,
            FontObject {
                font: Some(face.into()),
                height: Some(12.0),
                color: Some([1.0, 1.0, 1.0, 1.0]),
                ..FontObject::default()
            },
        );
    }
    s.run(
        r#"
        f = CreateFrame("Frame", "ScopeHost")
        f:SetWidth(200); f:SetHeight(60); f:SetPoint("CENTER")
        one = f:CreateFontString(nil, "ARTWORK"); one:SetText("one"); one:SetFontObject(FontOne)
        two = f:CreateFontString(nil, "ARTWORK"); two:SetText("two"); two:SetFontObject(FontTwo)
        FontOne:SetFont("Fonts\\CHANGED.TTF", 30)
    "#,
    )
    .unwrap();
    s.resolve();
    assert_eq!(painted(&s, "one").0, Some("Fonts\\CHANGED.TTF".into()));
    assert_eq!(painted(&s, "two").0, Some("Fonts\\TWO.TTF".into()));
    assert_eq!(painted(&s, "two").1, Some(12.0));
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// CreateFont
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// **`_Nameplates.lua:149` + `:129` + `:212`, end to end** — mint a font object at runtime, set it
/// up, then paint a FontString with it. Plus `!OmniCC/main.lua:40-41`'s half: the name is published
/// as a global even when the return value is thrown away, and `SetFont`'s return is the font-file
/// validity probe OmniCC uses it as.
#[test]
fn create_font_mints_publishes_and_paints() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        Nameplate = { Font = CreateFont("_NameplatesNameplateFont") }
        Nameplate.Font:SetFont("Fonts\\SKURRI.TTF", 10)
        Nameplate.Font:SetTextColor(0, 0.5, 1)

        f = CreateFrame("Frame", "PlateHost")
        f:SetWidth(120); f:SetHeight(20); f:SetPoint("CENTER")
        Name = f:CreateFontString(nil, "ARTWORK")
        Name:SetText("plate")
        Name:SetFontObject(Nameplate.Font)
    "#,
    )
    .expect("_Nameplates' font setup must not raise");
    s.resolve();
    assert_eq!(
        painted(&s, "plate"),
        (
            Some("Fonts\\SKURRI.TTF".into()),
            Some(10.0),
            Some([0.0, 0.5, 1.0, 1.0])
        )
    );

    // The return value and the published global are the SAME object — OmniCC reads only the global.
    assert!(s
        .eval::<bool>("return _NameplatesNameplateFont == Nameplate.Font")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return _NameplatesNameplateFont:GetObjectType()")
            .unwrap(),
        "Font"
    );

    // `!OmniCC/main.lua:40-41` verbatim: create, discard the return, use the global, and branch on
    // SetFont's return to detect a saved font path that is no longer usable.
    let reverted = s
        .eval::<bool>(
            r#"
            local reverted = false
            if not OmniCCFont then
                CreateFont("OmniCCFont")
                if not OmniCCFont:SetFont("", 20) then
                    reverted = true
                    OmniCCFont:SetFont("Fonts\\FRIZQT__.TTF", 20)
                end
            end
            return reverted
        "#,
        )
        .unwrap();
    assert!(reverted, "SetFont must report an unusable path as false");
    assert_eq!(
        s.eval::<(String, f32, String)>("return OmniCCFont:GetFont()")
            .unwrap(),
        ("Fonts\\FRIZQT__.TTF".to_string(), 20.0, String::new())
    );

    // A name that already names a font object hands the existing one back UNCHANGED — the
    // non-destructive reading (see `create_font`'s doc). The alternative would let
    // `CreateFont("GameFontNormal")` blank the shipped registry entry the whole UI inherits.
    s.run("again = CreateFont('_NameplatesNameplateFont')")
        .unwrap();
    assert!(s.eval::<bool>("return again == Nameplate.Font").unwrap());
    assert_eq!(
        s.eval::<(String, f32, String)>("return again:GetFont()")
            .unwrap(),
        ("Fonts\\SKURRI.TTF".to_string(), 10.0, String::new())
    );

    // A nameless CreateFont is an error: the name IS the publication. The EMPTY name is not —
    // the reference's `lua_isstring` gate takes it (unlike the XML path), so we do too.
    assert!(s.run("CreateFont()").is_err());
    s.run("CreateFont('')").unwrap();
}

/// **`FonzAppraiser/mods/gui/gui.lua:27-30`** — `CreateFont` then `CopyFontObject(<a shipped
/// object>)` then override the face. The corpus's only Font-on-Font call.
#[test]
fn fonz_appraiser_copies_a_shipped_object_then_overrides_the_face() {
    let s = script();
    s.register_font_object(
        "GameFontHighlightSmall",
        FontObject {
            font: Some("Fonts\\FRIZQT__.TTF".into()),
            height: Some(10.0),
            color: Some([1.0, 1.0, 1.0, 1.0]),
            outline: Outline::None,
            justify_h: Some(JustifyH::Right),
            justify_v: None,
            shadow: None,
        },
    );
    s.run(
        r#"
        small_number_font = CreateFont("FonzAppraiser_NumberFontNormalSmall")
        small_number_font:CopyFontObject(GameFontHighlightSmall)
        small_number_font:SetFont("Fonts\\ARIALN.TTF", 12)
    "#,
    )
    .expect("FonzAppraiser's font setup must not raise");
    // The copy brought the colour and justification over; the SetFont replaced the face and size.
    assert_eq!(
        s.eval::<(String, f32, String)>("return small_number_font:GetFont()")
            .unwrap(),
        ("Fonts\\ARIALN.TTF".to_string(), 12.0, String::new())
    );
    assert_eq!(
        s.eval::<(f32, f32, f32, f32)>("return small_number_font:GetTextColor()")
            .unwrap(),
        (1.0, 1.0, 1.0, 1.0)
    );
    assert_eq!(
        s.eval::<String>("return small_number_font:GetJustifyH()")
            .unwrap(),
        "RIGHT"
    );
    // …and the object it copied FROM is untouched.
    assert_eq!(
        s.eval::<(String, f32, String)>("return GameFontHighlightSmall:GetFont()")
            .unwrap(),
        ("Fonts\\FRIZQT__.TTF".to_string(), 10.0, String::new())
    );
}

/// The `Spacing` pair is **deliberately absent** (we model no line spacing, and a stored-but-never
/// drawn setter is this codebase's recurring silent-drop bug). It must fail LOUDLY rather than
/// quietly accept a number nobody honours — this test is the tripwire on that choice, and it flips
/// the day spacing becomes real.
#[test]
fn the_unmodelled_spacing_pair_fails_loudly() {
    let s = script();
    s.register_font_object("SomeFont", FontObject::default());
    assert!(s.run("SomeFont:SetSpacing(4)").is_err());
    assert!(s.eval::<bool>("return SomeFont.SetSpacing == nil").unwrap());
}

/// A button's per-state label fonts take the object or the name too (the corpus splits 5 to 4
/// across the trio), and — because they are stored as NAMES and re-resolved at every extract — a
/// later mutation of that font object reaches the label with no further call.
#[test]
fn button_state_fonts_take_the_object_and_follow_its_mutation() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.register_font_object(
        "GameFontNormal",
        FontObject {
            font: Some("Fonts\\FRIZQT__.TTF".into()),
            height: Some(12.0),
            color: Some([1.0, 0.82, 0.0, 1.0]),
            ..FontObject::default()
        },
    );
    s.run(
        r#"
        b = CreateFrame("Button", "StateFontButton")
        b:SetWidth(80); b:SetHeight(22); b:SetPoint("CENTER")
        b:SetText("go")
        b:SetTextFontObject(GameFontNormal)
    "#,
    )
    .expect("the object form must be accepted");
    s.resolve();
    assert_eq!(painted(&s, "go").1, Some(12.0));

    s.run("GameFontNormal:SetFont('Fonts\\\\MORPHEUS.TTF', 22)")
        .unwrap();
    s.resolve();
    assert_eq!(
        painted(&s, "go"),
        (
            Some("Fonts\\MORPHEUS.TTF".into()),
            Some(22.0),
            Some([1.0, 0.82, 0.0, 1.0])
        )
    );

    // The string form still works alongside it.
    s.run("b:SetHighlightFontObject('GameFontNormal')").unwrap();
}

/// **A `CreateFont` object holds nothing, so pointing a FontString at it copies nothing.**
/// §5-verified: the reference gates every merge on the source's own has-a-value mask, and a fresh
/// font has `mask == 0` — the FontString keeps exactly what it had, no blanking and no fallback to
/// a default. Our first cut wrote face/height through unconditionally and would have wiped it.
#[test]
fn an_empty_font_object_copies_nothing_onto_a_fontstring() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.register_font_object(
        "DressedFont",
        FontObject {
            font: Some("Fonts\\FRIZQT__.TTF".into()),
            height: Some(14.0),
            color: Some([0.2, 0.4, 0.6, 1.0]),
            ..FontObject::default()
        },
    );
    s.run(
        r#"
        f = CreateFrame("Frame", "EmptyFontHost")
        f:SetWidth(120); f:SetHeight(20); f:SetPoint("CENTER")
        fs = f:CreateFontString(nil, "ARTWORK")
        fs:SetText("keep")
        fs:SetFontObject(DressedFont)
        blank = CreateFont("ABlankFont")
        fs:SetFontObject(blank)
    "#,
    )
    .unwrap();
    s.resolve();
    assert_eq!(
        painted(&s, "keep"),
        (
            Some("Fonts\\FRIZQT__.TTF".into()),
            Some(14.0),
            Some([0.2, 0.4, 0.6, 1.0])
        ),
        "an unset property must copy nothing, not blank the region"
    );
    // The link still moved, so once the blank font is dressed the string follows it.
    assert_eq!(
        s.eval::<String>("return fs:GetFontObject():GetName()")
            .unwrap(),
        "ABlankFont"
    );
    s.run("blank:SetFont('Fonts\\\\MORPHEUS.TTF', 22)").unwrap();
    s.resolve();
    assert_eq!(painted(&s, "keep").0, Some("Fonts\\MORPHEUS.TTF".into()));
}

/// `SetFont` returns **1 or nil**, not a boolean — the reference's exact return shape, and what
/// `!OmniCC/main.lua:41` branches on.
#[test]
fn set_font_returns_one_or_nil() {
    let s = script();
    s.run("f = CreateFont('ReturnShapeFont')").unwrap();
    assert_eq!(
        s.eval::<f32>("return f:SetFont('Fonts\\\\FRIZQT__.TTF', 12)")
            .unwrap(),
        1.0
    );
    assert!(s.eval::<bool>("return f:SetFont('', 12) == nil").unwrap());
}
