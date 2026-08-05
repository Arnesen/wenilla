//! Loader tests — split from `mod.rs` (file-size budget); same `super::*` view.

mod loader_tests {
    use crate::framexml;
    use crate::loader::*;
    use crate::order::ZTarget;
    use crate::script::{QuadContent, UiScript};

    /// No-provider `files` closure (for docs with no `<Include>`/`<Script file=>`).
    fn no_files(_: &str) -> Option<String> {
        None
    }

    fn parse(text: &str) -> framexml::ParsedDocument {
        framexml::parse(text).expect("valid FrameXML")
    }

    /// End-to-end: a virtual template, an instance inheriting it (with `<Size>`, screen `<Anchors>`,
    /// a `<Layers>` coloured `<Texture>`, a nested child `<Frame>` in `<Frames>`, and `<OnLoad>`
    /// handlers on both) — proving name publication, bottom-up OnLoad, `$parent`, and that
    /// resolve()+extract() places the texture at the anchored rect.
    #[test]
    fn synthetic_full_document_materializes() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        // Seed the ordering witness before the load runs any OnLoad.
        s.run("loadorder = {}").unwrap();

        let doc = parse(
            r#"<Ui>
                <Frame name="MyTemplate" virtual="true">
                    <Size><AbsDimension x="200" y="100"/></Size>
                </Frame>
                <Frame name="MyFrame" inherits="MyTemplate">
                    <Anchors>
                        <Anchor point="TOPLEFT" relativePoint="TOPLEFT">
                            <Offset><AbsDimension x="10" y="-20"/></Offset>
                        </Anchor>
                    </Anchors>
                    <Layers>
                        <Layer level="ARTWORK">
                            <Texture name="$parentTex">
                                <Color r="1" g="0" b="0" a="1"/>
                            </Texture>
                        </Layer>
                    </Layers>
                    <Frames>
                        <Frame name="$parentChild">
                            <Scripts>
                                <OnLoad>table.insert(loadorder, "child"); ChildLoaded = self:GetName()</OnLoad>
                            </Scripts>
                        </Frame>
                    </Frames>
                    <Scripts>
                        <OnLoad>table.insert(loadorder, "parent"); ParentLoaded = this:GetName()</OnLoad>
                    </Scripts>
                </Frame>
            </Ui>"#,
        );

        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert_eq!(report.frames, 2, "parent + child materialized");

        // Frame exists by name; the child's $parent resolved to MyFrameChild; the texture published
        // its $parent-resolved name.
        assert!(s.eval::<bool>("return MyFrame ~= nil").unwrap());
        assert!(s.eval::<bool>("return MyFrameChild ~= nil").unwrap());
        assert!(s.eval::<bool>("return MyFrameTex ~= nil").unwrap());

        // OnLoad fired for both (modern `self` in child, legacy `this` in parent — both work).
        assert_eq!(
            s.eval::<String>("return ChildLoaded").unwrap(),
            "MyFrameChild"
        );
        assert_eq!(s.eval::<String>("return ParentLoaded").unwrap(), "MyFrame");

        // Bottom-up: the child's OnLoad ran before the parent's (rf26).
        let order: Vec<String> = s.eval("return loadorder").unwrap();
        assert_eq!(order, vec!["child".to_string(), "parent".to_string()]);

        // resolve()+extract(): the texture quad sits at MyFrame's anchored rect.
        // Screen [0,0,600,800], TOPLEFT+(10,-20), size 200x100 → Rect(bottom 480, left 10, top 580,
        // right 210). Region rect = its owner frame's resolved rect (v1).
        s.resolve();
        let quads = s.extract();
        let tex = quads
            .iter()
            .find(|q| matches!(&q.content, QuadContent::Texture { color: Some(_), .. }))
            .expect("the coloured texture quad");
        assert!(
            matches!(&tex.content, QuadContent::Texture { color: Some(c), path: None, .. } if *c == [1.0, 0.0, 0.0, 1.0])
        );
        assert_eq!(
            tex.rect,
            Some(crate::layout::Rect::new(480.0, 10.0, 580.0, 210.0))
        );
    }

    /// An instance's own `<Size>` beats its template's: expansion appends the instance's children
    /// after the template's, and `apply_size` walks EVERY `<Size>` in document order so the last
    /// write wins — exactly the client's process-each-child behavior. (Regression: `.next()` here
    /// once pinned every templated button to the TEMPLATE's size; the quest log's Abandon button
    /// declared 125×21 and rendered 80×22, wrapping its label onto two lines.)
    #[test]
    fn instance_size_overrides_template_size() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui>
                <Button name="SizedTemplate" virtual="true">
                    <Size><AbsDimension x="80" y="22"/></Size>
                </Button>
                <Button name="SizedInstance" inherits="SizedTemplate">
                    <Size><AbsDimension x="125" y="21"/></Size>
                </Button>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert_eq!(
            s.eval::<f32>("return SizedInstance:GetWidth()").unwrap(),
            125.0
        );
        assert_eq!(
            s.eval::<f32>("return SizedInstance:GetHeight()").unwrap(),
            21.0
        );
    }

    /// A `<Frame setAllPoints="true">` (no `<Size>`/`<Anchors>` of its own) resolves to its
    /// parent's rect — the frame path honors the shorthand like the region path does.
    /// (Regression: only regions applied it; the world map's chrome frames carried the attribute,
    /// never resolved, and their whole subtrees — backdrop, title, buttons — silently vanished.)
    #[test]
    fn frame_set_all_points_pins_to_parent() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Frame name="PinHost">
                    <Size><AbsDimension x="300" y="200"/></Size>
                    <Anchors>
                        <Anchor point="TOPLEFT">
                            <Offset><AbsDimension x="40" y="-50"/></Offset>
                        </Anchor>
                    </Anchors>
                    <Frames>
                        <Frame name="PinChild" setAllPoints="true"/>
                    </Frames>
                </Frame>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        s.resolve();
        assert_eq!(s.eval::<f32>("return PinChild:GetLeft()").unwrap(), 40.0);
        assert_eq!(s.eval::<f32>("return PinChild:GetTop()").unwrap(), 550.0);
        assert_eq!(s.eval::<f32>("return PinChild:GetWidth()").unwrap(), 300.0);
        assert_eq!(s.eval::<f32>("return PinChild:GetHeight()").unwrap(), 200.0);
    }

    /// `<Include>` resolved through an in-memory provider closure.
    #[test]
    fn include_resolves_through_provider() {
        let s = UiScript::new().unwrap();
        let provider = |path: &str| -> Option<String> {
            match path {
                "Sub.xml" => Some(r#"<Ui><Frame name="FromInclude"/></Ui>"#.to_string()),
                _ => None,
            }
        };
        let doc = parse(
            r#"<Ui>
                <Include file="Sub.xml"/>
                <Frame name="FromMain"/>
            </Ui>"#,
        );
        let report = load(&s, &doc, &provider);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.frames, 2);
        assert!(s
            .eval::<bool>("return FromInclude ~= nil and FromMain ~= nil")
            .unwrap());
    }

    /// A missing include is a warning, not an error; the rest of the load proceeds.
    #[test]
    fn missing_include_warns_and_continues() {
        let s = UiScript::new().unwrap();
        let doc = parse(r#"<Ui><Include file="Nope.xml"/><Frame name="Still"/></Ui>"#);
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty());
        assert!(report.warnings.iter().any(|w| w.contains("Nope.xml")));
        assert!(s.eval::<bool>("return Still ~= nil").unwrap());
    }

    /// `$parent` resolves in a child's *name* and in an anchor's `relativeTo`.
    #[test]
    fn parent_token_in_child_name_and_anchor_relative_to() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Frame name="PF">
                    <Size><AbsDimension x="100" y="100"/></Size>
                    <Anchors>
                        <Anchor point="TOPLEFT"><Offset><AbsDimension x="50" y="-50"/></Offset></Anchor>
                    </Anchors>
                    <Frames>
                        <Frame name="$parentInner">
                            <Size><AbsDimension x="100" y="100"/></Size>
                            <Anchors>
                                <Anchor point="TOPLEFT" relativeTo="$parent" relativePoint="TOPLEFT"/>
                            </Anchors>
                        </Frame>
                    </Frames>
                </Frame>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        // Child $parent-name resolved to PFInner and published.
        assert!(s.eval::<bool>("return PFInner ~= nil").unwrap());

        // relativeTo="$parent" resolved to PF, so the child sits exactly on the parent's TOPLEFT.
        s.resolve();
        let quads = s.extract();
        let rects: Vec<_> = quads
            .iter()
            .filter_map(|q| match q.target {
                ZTarget::Frame(_) => q.rect,
                _ => None,
            })
            .collect();
        // Parent: screen [0,0,600,800] TOPLEFT+(50,-50), 100x100 → Rect(450,50,550,150). The child,
        // anchored TOPLEFT→parent TOPLEFT with no offset, resolves to the same rect.
        let expected = crate::layout::Rect::new(450.0, 50.0, 550.0, 150.0);
        assert!(
            rects.iter().filter(|r| **r == expected).count() >= 2,
            "rects: {rects:?}"
        );
    }

    /// A handler with a syntax error yields an `errors[]` entry, the load continues, and the other
    /// frame is still built with a working handler.
    #[test]
    fn bad_handler_is_an_error_and_load_continues() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui>
                <Frame name="Broken">
                    <Scripts><OnLoad>this that syntax error(((</OnLoad></Scripts>
                </Frame>
                <Frame name="Fine">
                    <Scripts><OnLoad>FineRan = true</OnLoad></Scripts>
                </Frame>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert_eq!(report.frames, 2, "both frames still created");
        assert!(
            report.errors.iter().any(|e| e.contains("compiling")),
            "expected a compile error, got {:?}",
            report.errors
        );
        // The broken frame exists but its OnLoad never ran; the good frame's did.
        assert!(s.eval::<bool>("return Broken ~= nil").unwrap());
        assert!(s.eval::<bool>("return FineRan == true").unwrap());
    }

    /// Unsupported handler names (e.g. the keyboard-focus handlers `OnKeyDown`/`OnChar`, not yet
    /// modeled) are warn-once gaps, not hard errors, and don't stop the frame from building.
    #[test]
    fn unsupported_script_name_is_a_warning() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui><Frame name="Keyed"><Scripts><OnKeyDown>x = 1</OnKeyDown></Scripts></Frame></Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert_eq!(report.frames, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.warnings.iter().any(|w| w.contains("OnKeyDown")));
    }

    /// An unknown frame type is an error that drops that subtree but not the rest of the load.
    #[test]
    fn unknown_frame_type_errors_but_continues() {
        let s = UiScript::new().unwrap();
        let doc = parse(r#"<Ui><Bogus name="X"/><Frame name="Real"/></Ui>"#);
        let report = load(&s, &doc, &no_files);
        assert_eq!(report.frames, 1, "only the real frame built");
        assert!(report.errors.iter().any(|e| e.contains("CreateFrame")));
        assert!(s.eval::<bool>("return Real ~= nil").unwrap());
    }

    /// Env-gated smoke test over a real extracted FrameXML file (never committed; extract with
    /// benilla-extract and point `BENILLA_FRAMEXML_LOAD` at it). Parses + loads with a provider over
    /// the file's own directory, asserts no hard *parse/loader-internal* error, and reports how many
    /// frames materialized plus the API/handler gaps the file surfaced. Skips silently otherwise,
    /// matching framexml.rs's `real_framexml_when_available` pattern so gates never depend on client
    /// data.
    #[test]
    fn real_framexml_load_when_available() {
        let Ok(path) = std::env::var("BENILLA_FRAMEXML_LOAD") else {
            return;
        };
        let text = std::fs::read_to_string(&path).expect("reading BENILLA_FRAMEXML_LOAD");
        let dir = std::path::Path::new(&path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        // Provider: resolve an XML/Lua reference against the file's directory, trying the path as
        // given and by basename (Blizzard paths use backslashes and are dir-relative).
        let provider = move |req: &str| -> Option<String> {
            let norm = req.replace('\\', "/");
            let base = norm.rsplit('/').next().unwrap_or(&norm);
            std::fs::read_to_string(dir.join(&norm))
                .or_else(|_| std::fs::read_to_string(dir.join(base)))
                .ok()
        };

        let s = UiScript::new().unwrap();
        let doc = framexml::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
        let report = load(&s, &doc, &provider);

        eprintln!("== {path}: {} frames materialized ==", report.frames);
        for e in &report.errors {
            eprintln!("  error: {e}");
        }
        for w in &report.warnings {
            eprintln!("  warn:  {w}");
        }
        // Script errors surfaced by any OnLoad that ran against missing API:
        for e in s.errors() {
            eprintln!("  script-error: {e}");
        }
    }

    /// `<StatusBar>` LoadXML extras (RF-28): value attributes, `<BarTexture>` + `<BarColor>`
    /// children, orientation — landing in the widget state and scaling the extracted bar quad.
    #[test]
    fn statusbar_xml_extras_apply() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <StatusBar name="XmlBar" minValue="0" maxValue="100" defaultValue="50">
                    <Size><AbsDimension x="100" y="10"/></Size>
                    <Anchors>
                        <Anchor point="BOTTOMLEFT" relativePoint="BOTTOMLEFT">
                            <Offset><AbsDimension x="0" y="0"/></Offset>
                        </Anchor>
                    </Anchors>
                    <BarTexture file="Interface\TargetingFrame\UI-StatusBar"/>
                    <BarColor r="0.1" g="0.9" b="0.1"/>
                </StatusBar>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

        let ok: bool = s
            .eval(
                r#"
            local mn, mx = XmlBar:GetMinMaxValues()
            local r, g, b, a = XmlBar:GetStatusBarColor()
            return mn == 0 and mx == 100 and XmlBar:GetValue() == 50
                and XmlBar:GetOrientation() == "HORIZONTAL"
                and XmlBar:GetStatusBarTexture() ~= nil
                and r < 0.2 and g > 0.8 and a == 1
        "#,
            )
            .unwrap();
        assert!(ok, "XML attributes + children landed in the widget state");

        s.resolve();
        let bar = s
            .extract()
            .into_iter()
            .find(|q| {
                matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("UI-StatusBar"))
            })
            .expect("bar fill quad");
        let r = bar.rect.expect("bar rect resolved");
        assert_eq!(
            (r.left, r.right, r.bottom, r.top),
            (0.0, 50.0, 0.0, 10.0),
            "defaultValue 50/100 fills half the 100px width"
        );
    }

    #[test]
    fn button_xml_extras_apply() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Button name="XmlBtn" text="Push Me">
                    <Size><AbsDimension x="64" y="24"/></Size>
                    <Anchors>
                        <Anchor point="BOTTOMLEFT" relativePoint="BOTTOMLEFT">
                            <Offset><AbsDimension x="0" y="0"/></Offset>
                        </Anchor>
                    </Anchors>
                    <NormalTexture file="Interface\B-Up"/>
                    <PushedTexture file="Interface\B-Down"/>
                    <HighlightTexture file="Interface\B-Hi"/>
                    <Scripts>
                        <OnClick>xml_clicked = true</OnClick>
                    </Scripts>
                </Button>
                <CheckButton name="XmlCheck" checked="true">
                    <Size><AbsDimension x="24" y="24"/></Size>
                    <Anchors>
                        <Anchor point="BOTTOMLEFT" relativePoint="BOTTOMLEFT">
                            <Offset><AbsDimension x="100" y="0"/></Offset>
                        </Anchor>
                    </Anchors>
                    <NormalTexture file="Interface\CB-Up"/>
                    <CheckedTexture file="Interface\CB-Check"/>
                </CheckButton>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

        let ok: bool = s
            .eval(
                r#"
            return XmlBtn:GetText() == "Push Me"
               and XmlBtn:GetNormalTexture() ~= nil
               and XmlBtn:GetPushedTexture() ~= nil
               and XmlCheck:GetChecked() == true
        "#,
            )
            .unwrap();
        assert!(ok, "button XML attrs + textures landed");

        // The checked CheckButton draws its check mark; the idle Button draws only its normal.
        s.resolve();
        let texs: Vec<String> = s
            .extract()
            .iter()
            .filter_map(|q| match &q.content {
                QuadContent::Texture { path: Some(p), .. } => Some(p.clone()),
                _ => None,
            })
            .collect();
        assert!(texs.contains(&"Interface\\B-Up".into()));
        assert!(!texs.contains(&"Interface\\B-Down".into()));
        assert!(!texs.contains(&"Interface\\B-Hi".into()));
        assert!(texs.contains(&"Interface\\CB-Check".into()));

        // A physical click fires the XML OnClick.
        s.mouse_button(30.0, 10.0, "LeftButton", true);
        s.mouse_button(30.0, 10.0, "LeftButton", false);
        assert!(s.eval::<bool>("return xml_clicked == true").unwrap());
        assert!(s.errors().is_empty(), "{:?}", s.errors());
    }

    /// A state texture's own `<Size>` + `<Anchors>` scope it to less than its button (the merchant
    /// row's icon-scoped highlight): hovering the 100px-wide button, the highlight quad renders at
    /// the anchored 20px square, not stretched over the button.
    #[test]
    fn button_state_texture_takes_size_and_anchors() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Button name="ScopedBtn">
                    <Size><AbsDimension x="100" y="30"/></Size>
                    <Anchors>
                        <Anchor point="BOTTOMLEFT" relativePoint="BOTTOMLEFT">
                            <Offset><AbsDimension x="0" y="0"/></Offset>
                        </Anchor>
                    </Anchors>
                    <HighlightTexture file="Interface\Scoped-Hi" alphaMode="ADD">
                        <Size><AbsDimension x="20" y="20"/></Size>
                        <Anchors><Anchor point="TOPLEFT" relativePoint="TOPLEFT"/></Anchors>
                    </HighlightTexture>
                </Button>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

        // Hover the button's far-right end — the highlight still sits at the anchored TOPLEFT.
        // (Resolve first: hit-testing reads the resolved-rect cache.)
        s.resolve();
        s.mouse_move(90.0, 15.0);
        s.resolve();
        let hi = s
            .extract()
            .into_iter()
            .find(|q| {
                matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("Scoped-Hi"))
            })
            .expect("hovered highlight quad");
        let r = hi.rect.expect("highlight rect resolved");
        assert_eq!(
            (r.left, r.right, r.bottom, r.top),
            (0.0, 20.0, 10.0, 30.0),
            "highlight is the anchored 20px square at the button's TOPLEFT, not the whole button"
        );
        assert!(s.errors().is_empty(), "{:?}", s.errors());
    }

    /// `<EditBox>` LoadXML extras (RF-0082): `letters` → SetMaxLetters and the config flags land in
    /// the widget state; a click focuses it and the `<OnTextChanged>` handler fires on a typed char.
    #[test]
    fn editbox_xml_extras_apply() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <EditBox name="XmlEdit" letters="5" numeric="true">
                    <Size><AbsDimension x="120" y="20"/></Size>
                    <Anchors>
                        <Anchor point="BOTTOMLEFT" relativePoint="BOTTOMLEFT">
                            <Offset><AbsDimension x="0" y="0"/></Offset>
                        </Anchor>
                    </Anchors>
                    <Scripts>
                        <OnTextChanged>typed = true</OnTextChanged>
                    </Scripts>
                </EditBox>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

        // letters=5 landed; numeric abort rejects a non-digit insert wholesale.
        assert_eq!(s.eval::<i64>("return XmlEdit:GetNumLetters()").unwrap(), 0);
        s.run(r#"XmlEdit:Insert("12x")"#).unwrap(); // numeric: aborts
        s.run(r#"XmlEdit:Insert("123456")"#).unwrap(); // digits, capped at 5
        assert_eq!(
            s.eval::<String>("return XmlEdit:GetText()").unwrap(),
            "12345"
        );

        // A click focuses the box (mouse-enabled by construction); a typed char fires OnTextChanged.
        s.resolve();
        s.mouse_button(50.0, 10.0, "LeftButton", true);
        s.mouse_button(50.0, 10.0, "LeftButton", false);
        assert!(s.eval::<bool>("return XmlEdit:HasFocus()").unwrap());
        s.run("typed = false").unwrap();
        assert!(s.char_input("7"));
        assert!(s.eval::<bool>("return typed == true").unwrap());
        assert!(s.errors().is_empty(), "{:?}", s.errors());
    }

    // ── TexCoords + Font objects (decision 0084 engine slice) ───────────────────────────────────

    /// `<TexCoords left right top bottom>` on a `<Texture>` parses into the region's UV rect and
    /// surfaces on the extracted quad as `[left, right, top, bottom]`.
    #[test]
    fn texcoords_parse_to_uv_on_extracted_quad() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Frame name="TC">
                    <Size><AbsDimension x="100" y="100"/></Size>
                    <Anchors><Anchor point="CENTER"/></Anchors>
                    <Layers><Layer level="ARTWORK">
                        <Texture name="$parentArt" file="Interface\Foo">
                            <TexCoords left="0.0" right="0.5" top="0.25" bottom="0.75"/>
                        </Texture>
                    </Layer></Layers>
                </Frame>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        s.resolve();
        let tex = s
            .extract()
            .into_iter()
            .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains("Foo")))
            .expect("the Foo texture quad");
        assert!(
            matches!(
                &tex.content,
                QuadContent::Texture { tex_coords: Some(crate::script::TexCoords::Rect(tc)), .. }
                    if *tc == [0.0, 0.5, 0.25, 0.75]
            ),
            "got {:?}",
            tex.content
        );
    }

    /// A `<Font>` `inherits=` chain resolves height + color through two levels: the leaf inherits its
    /// face + one value from the root and overrides another mid-chain (last-wins merge).
    #[test]
    fn font_inherits_chain_resolves_height_and_color_two_levels() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui>
                <Font name="Base" font="Fonts\FRIZQT__.TTF" virtual="true">
                    <FontHeight><AbsValue val="12"/></FontHeight>
                    <Color r="1.0" g="0.82" b="0"/>
                </Font>
                <Font name="Mid" inherits="Base" virtual="true">
                    <Color r="1.0" g="1.0" b="1.0"/>
                </Font>
                <Font name="Leaf" inherits="Mid" virtual="true">
                    <FontHeight><AbsValue val="10"/></FontHeight>
                </Font>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

        let base = s.font_object("Base").expect("Base registered");
        assert_eq!(base.font.as_deref(), Some("Fonts\\FRIZQT__.TTF"));
        assert_eq!(base.height, Some(12.0));
        assert_eq!(base.color, Some([1.0, 0.82, 0.0, 1.0]));

        // Leaf: face inherited from Base (through Mid), color from Mid (white), height overridden (10).
        let leaf = s.font_object("Leaf").expect("Leaf registered");
        assert_eq!(leaf.font.as_deref(), Some("Fonts\\FRIZQT__.TTF"));
        assert_eq!(leaf.height, Some(10.0));
        assert_eq!(leaf.color, Some([1.0, 1.0, 1.0, 1.0]));
    }

    /// A `<FontString inherits="GameFontNormalSmall">` reports the *smaller* resolved height (and the
    /// inherited face + color) on the extracted quad — the per-FontString size the renderer bakes at.
    #[test]
    fn fontstring_inherits_reports_smaller_resolved_height() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Font name="GameFontNormal" font="Fonts\FRIZQT__.TTF" virtual="true">
                    <FontHeight><AbsValue val="12"/></FontHeight>
                    <Color r="1.0" g="0.82" b="0"/>
                </Font>
                <Font name="GameFontNormalSmall" font="Fonts\FRIZQT__.TTF" virtual="true">
                    <FontHeight><AbsValue val="10"/></FontHeight>
                    <Color r="1.0" g="0.82" b="0"/>
                </Font>
                <Frame name="FS">
                    <Size><AbsDimension x="100" y="30"/></Size>
                    <Anchors><Anchor point="CENTER"/></Anchors>
                    <Layers><Layer level="ARTWORK">
                        <FontString name="$parentBig" inherits="GameFontNormal" text="Big"/>
                        <FontString name="$parentSmall" inherits="GameFontNormalSmall" text="Small"/>
                    </Layer></Layers>
                </Frame>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        s.resolve();

        let height_of = |s: &UiScript, want: &str| -> Option<f32> {
            s.extract().into_iter().find_map(|q| match q.content {
                QuadContent::Text {
                    text: Some(t),
                    font_height,
                    ..
                } if t == want => Some(font_height?),
                _ => None,
            })
        };
        assert_eq!(height_of(&s, "Big"), Some(12.0));
        assert_eq!(
            height_of(&s, "Small"),
            Some(10.0),
            "the *Small font object resolves to the smaller height"
        );

        // The inherited face + color also flow through.
        let small = s
            .extract()
            .into_iter()
            .find(|q| matches!(&q.content, QuadContent::Text { text: Some(t), .. } if t == "Small"))
            .unwrap();
        assert!(matches!(
            &small.content,
            QuadContent::Text { font: Some(f), color: Some(c), .. }
                if f == "Fonts\\FRIZQT__.TTF" && *c == [1.0, 0.82, 0.0, 1.0]
        ));
    }

    /// An explicit `<Color>` on a FontString overrides its font object's color; an `inherits=` naming
    /// no registered font object is a warn-once gap, not a dropped region.
    #[test]
    fn fontstring_color_overrides_object_and_unknown_inherits_warns() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Font name="GameFontNormal" font="Fonts\FRIZQT__.TTF" virtual="true">
                    <FontHeight><AbsValue val="12"/></FontHeight>
                    <Color r="1.0" g="0.82" b="0"/>
                </Font>
                <Frame name="OV">
                    <Size><AbsDimension x="100" y="30"/></Size>
                    <Anchors><Anchor point="CENTER"/></Anchors>
                    <Layers><Layer level="ARTWORK">
                        <FontString name="$parentA" inherits="GameFontNormal" text="A">
                            <Color r="0.1" g="0.2" b="0.3" a="1.0"/>
                        </FontString>
                        <FontString name="$parentB" inherits="NoSuchFont" text="B"/>
                    </Layer></Layers>
                </Frame>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.iter().any(|w| w.contains("NoSuchFont")),
            "unknown font object should warn: {:?}",
            report.warnings
        );
        s.resolve();
        // A's explicit <Color> wins over GameFontNormal's gold; its height still comes from the object.
        let a = s
            .extract()
            .into_iter()
            .find(|q| matches!(&q.content, QuadContent::Text { text: Some(t), .. } if t == "A"))
            .unwrap();
        assert!(matches!(
            &a.content,
            QuadContent::Text { color: Some(c), font_height: Some(h), .. }
                if *c == [0.1, 0.2, 0.3, 1.0] && *h == 12.0
        ));
        // B still materialized (the unknown inherits didn't drop it).
        assert!(s
            .extract()
            .into_iter()
            .any(|q| matches!(&q.content, QuadContent::Text { text: Some(t), .. } if t == "B")));
    }

    /// The chat edit box's shape: an EditBox with BOTH a `<Layers>` FontString (the "Say:" header)
    /// and the special direct-child `<FontString>` (the engine's text-font slot). The box must
    /// adopt the SPECIAL one as its text region — the engine ASSIGNS that slot at LoadXML, it
    /// never searches. (The old find-first adoption grabbed the header: typing then overwrote
    /// "Say:", and the `<TextInsets>` re-anchor clobbered the header's LEFT anchor into a
    /// full-width rect — the centered-header + invisible-typing chat bug.)
    #[test]
    fn editbox_adopts_special_fontstring_not_a_layers_header() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <EditBox name="EB" letters="255">
                    <Size><AbsDimension x="600" y="32"/></Size>
                    <Anchors>
                        <Anchor point="BOTTOMLEFT" relativePoint="BOTTOMLEFT">
                            <Offset><AbsDimension x="100" y="100"/></Offset>
                        </Anchor>
                    </Anchors>
                    <TextInsets><AbsInset left="47" right="13" top="0" bottom="0"/></TextInsets>
                    <Layers>
                        <Layer level="ARTWORK">
                            <FontString name="$parentHeader" text="Say:">
                                <Anchors>
                                    <Anchor point="LEFT"><Offset><AbsDimension x="13" y="0"/></Offset></Anchor>
                                </Anchors>
                            </FontString>
                        </Layer>
                    </Layers>
                    <FontString justifyH="LEFT"/>
                </EditBox>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

        // Type into the focused box.
        s.run("EB:SetFocus()").unwrap();
        assert!(s.char_input("h"));
        assert!(s.char_input("i"));

        // Resolve; answer every fontstring measure with a stand-in extent (30×12), resolve again
        // (the round-trip's second solve).
        s.resolve();
        let answers: Vec<(u32, f32, f32, u64)> = s
            .fontstrings_needing_measure()
            .iter()
            .map(|r| (r.id, 30.0, 12.0, r.key))
            .collect();
        s.set_measured_text(&answers);
        s.resolve();

        let quads = s.extract();
        // The header still reads "Say:" — typing lands in the box's own text region, never the
        // header. Both strings render.
        let header = quads
            .iter()
            .find(|q| matches!(&q.content, QuadContent::Text { text: Some(t), .. } if t == "Say:"))
            .expect("the header FontString still renders its own text");
        let typed = quads
            .iter()
            .find(|q| matches!(&q.content, QuadContent::Text { text: Some(t), .. } if t == "hi"))
            .expect("the typed text renders in the box's text region");

        // Header rect: its own LEFT+13 anchor + the measured 30×12 extent (left-flush against the
        // box's left edge at x=100, v-centered on the 100..132 box) — NOT the insets rect.
        assert_eq!(
            header.rect,
            Some(crate::layout::Rect::new(110.0, 113.0, 122.0, 143.0)),
            "header must keep its own anchor + auto-size"
        );
        // Typed text rect: the box minus the XML `<TextInsets>` (47 left, 13 right).
        assert_eq!(
            typed.rect,
            Some(crate::layout::Rect::new(100.0, 147.0, 132.0, 687.0)),
            "the text region is anchored by the insets"
        );
    }

    /// `<HitRectInsets>` reaches SetHitRectInsets, and the inset band stops capturing the mouse
    /// while the frame's own geometry is untouched — the ref micro-button shape (a 29x58 frame whose
    /// art fills only the lower ~40, `top="18"`).
    #[test]
    fn hit_rect_insets_element_shrinks_the_mouse_rect() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Button name="Micro">
                    <Size><AbsDimension x="29" y="58"/></Size>
                    <Anchors><Anchor point="BOTTOMLEFT" relativePoint="BOTTOMLEFT"/></Anchors>
                    <HitRectInsets><AbsInset left="0" right="0" top="18" bottom="0"/></HitRectInsets>
                </Button>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        s.resolve();

        let (l, r, t, b) = s
            .eval::<(f64, f64, f64, f64)>("return Micro:GetHitRectInsets()")
            .unwrap();
        assert_eq!((l, r, t, b), (0.0, 0.0, 18.0, 0.0));
        // Geometry untouched: still a 58-tall button.
        assert_eq!(s.eval::<f64>("return Micro:GetHeight()").unwrap(), 58.0);
        // A button is mouse-enabled by construction, so the hit test is live.
        assert!(
            s.hit_test(14.0, 50.0).is_none(),
            "the dead 18-unit header must not capture"
        );
        assert!(s.hit_test(14.0, 20.0).is_some(), "the art band still hits");
    }
}

mod layer_blend_tests {
    use crate::framexml::parse;
    use crate::loader::*;
    use crate::script::{QuadContent, UiScript};

    fn no_files(_: &str) -> Option<String> {
        None
    }

    /// `alphaMode="ADD"` on a `<Layers>` texture reaches the extracted quad's blend flag — the
    /// state-texture path always applied it, the layer path silently dropped it (the quest log's
    /// selection glow rendered as an opaque gray bar — the 0109 look fix).
    #[test]
    fn layers_texture_alpha_mode_add_reaches_the_quad() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui>
                <Frame name="GlowHost">
                    <Size><AbsDimension x="100" y="30"/></Size>
                    <Anchors><Anchor point="BOTTOMLEFT" relativePoint="BOTTOMLEFT"/></Anchors>
                    <Layers><Layer level="OVERLAY">
                        <Texture name="GlowTex" file="Interface\Glow" alphaMode="ADD">
                            <Anchors><Anchor point="TOPLEFT"/><Anchor point="BOTTOMRIGHT"/></Anchors>
                        </Texture>
                    </Layer></Layers>
                </Frame>
            </Ui>"#,
        )
        .unwrap();
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        let mut s = s;
        s.set_screen_size(800.0, 600.0);
        s.resolve();
        let additive = s
            .extract()
            .iter()
            .find_map(|q| match &q.content {
                QuadContent::Texture {
                    path: Some(p),
                    additive,
                    ..
                } if p == "Interface\\Glow" => Some(*additive),
                _ => None,
            })
            .expect("the layer texture extracted");
        assert!(additive, "alphaMode=ADD must ride into the quad blend flag");
    }
}

mod region_template_tests {
    use crate::framexml::parse;
    use crate::loader::*;
    use crate::script::{QuadContent, UiScript};

    fn no_files(_: &str) -> Option<String> {
        None
    }

    /// A `<Texture inherits="…">` layer region splices its virtual template — file, `<Size>`,
    /// `<Anchors>` — with the instance's own nodes winning (the talent branch/arrow art pool's
    /// shape; before this, the template was silently ignored and every pooled texture sat at
    /// 0×0 with no file: shown, positioned by the runtime SetPoint, and invisible).
    #[test]
    fn layers_texture_inherits_a_virtual_texture_template() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui>
                <Texture name="BranchTemplate" file="Interface\Branches" virtual="true">
                    <Size><AbsDimension x="32" y="32"/></Size>
                    <Anchors><Anchor point="TOPLEFT"/></Anchors>
                </Texture>
                <Frame name="Host">
                    <Size><AbsDimension x="300" y="300"/></Size>
                    <Anchors><Anchor point="TOPLEFT" relativePoint="TOPLEFT"/></Anchors>
                    <Layers><Layer level="BACKGROUND">
                        <Texture name="Branch1" inherits="BranchTemplate"/>
                        <Texture name="Branch2" inherits="BranchTemplate">
                            <Size><AbsDimension x="64" y="16"/></Size>
                        </Texture>
                    </Layer></Layers>
                </Frame>
            </Ui>"#,
        )
        .unwrap();
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "a registered region template must splice silently: {:?}",
            report.warnings
        );
        let mut s = s;
        s.set_screen_size(800.0, 600.0);
        // The template's size/file landed; the instance's own <Size> wins over the template's.
        assert_eq!(
            s.eval::<(f64, f64)>("return Branch1:GetWidth(), Branch1:GetHeight()")
                .unwrap(),
            (32.0, 32.0)
        );
        assert_eq!(
            s.eval::<(f64, f64)>("return Branch2:GetWidth(), Branch2:GetHeight()")
                .unwrap(),
            (64.0, 16.0)
        );
        s.resolve();
        let branch_quads = s
            .extract()
            .iter()
            .filter(|q| {
                matches!(
                    &q.content,
                    QuadContent::Texture { path: Some(p), .. } if p == "Interface\\Branches"
                )
            })
            .count();
        assert_eq!(
            branch_quads, 2,
            "both templated textures extract with the template's file"
        );
    }

    /// A `<FontString inherits="GameFontNormal">` names a FONT OBJECT, not an element template —
    /// the region-template gate must pass it through untouched (no "unknown template" warning).
    #[test]
    fn fontstring_font_object_inherits_stays_out_of_the_template_path() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui>
                <Font name="GameFontNormal" font="Fonts\FRIZQT__.TTF" virtual="true">
                    <FontHeight><AbsValue val="12"/></FontHeight>
                </Font>
                <Frame name="Host2">
                    <Size><AbsDimension x="100" y="30"/></Size>
                    <Anchors><Anchor point="TOPLEFT" relativePoint="TOPLEFT"/></Anchors>
                    <Layers><Layer level="ARTWORK">
                        <FontString name="Label" inherits="GameFontNormal" text="hello"/>
                    </Layer></Layers>
                </Frame>
            </Ui>"#,
        )
        .unwrap();
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "a font-object inherits must not warn as an unknown template: {:?}",
            report.warnings
        );
        assert_eq!(s.eval::<String>("return Label:GetText()").unwrap(), "hello");
    }
}
