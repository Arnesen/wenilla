//! Loader tests — split from `mod.rs` (file-size budget); same `super::*` view.

mod loader_tests {
    use crate::framexml;
    use crate::loader::*;
    use crate::order::ZTarget;
    use crate::script::{QuadContent, UiScript};

    /// No-provider `files` closure (for docs with no `<Include>`/`<Script file=>`).
    fn no_files(_: &str) -> Option<Vec<u8>> {
        None
    }

    fn parse(text: &str) -> framexml::ParsedDocument {
        framexml::parse(text).expect("valid FrameXML")
    }

    /// **`parent="Name"` attaches a top-level element** — the reference's own way of doing it,
    /// because its FrameXML is flat (decision 1211). 79 corpus addons, 708 sites.
    ///
    /// Three claims in one document: the attribute supplies the parent when there is no lexical
    /// one; an anchor with no `relativeTo` then measures from THAT parent rather than the screen
    /// (the silent half — a frame in the wrong place with nothing to report); and a name that
    /// resolves to nothing warns and falls back instead of raising.
    #[test]
    fn a_top_level_parent_attribute_attaches_and_anchors() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Frame name="Host">
                    <Size><AbsDimension x="200" y="100"/></Size>
                    <Anchors>
                        <Anchor point="TOPLEFT" relativePoint="TOPLEFT">
                            <Offset><AbsDimension x="100" y="-50"/></Offset>
                        </Anchor>
                    </Anchors>
                </Frame>
                <Frame name="Attached" parent="Host">
                    <Size><AbsDimension x="20" y="20"/></Size>
                    <Anchors>
                        <Anchor point="TOPLEFT">
                            <Offset><AbsDimension x="5" y="-5"/></Offset>
                        </Anchor>
                    </Anchors>
                </Frame>
                <Frame name="Orphan" parent="NoSuchFrame">
                    <Size><AbsDimension x="10" y="10"/></Size>
                    <Anchors><Anchor point="CENTER"/></Anchors>
                </Frame>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        s.resolve();

        assert_eq!(
            s.eval::<String>("return Attached:GetParent():GetName()")
                .unwrap(),
            "Host",
            "the attribute supplied the parent"
        );
        // Host's TOPLEFT is screen (100, 550) in y-up; Attached hangs 5 in and 5 down from it.
        assert_eq!(
            s.eval::<(f64, f64)>("return Attached:GetLeft(), Attached:GetTop()")
                .unwrap(),
            (105.0, 545.0),
            "a relativeTo-less anchor measures from the PARENT, not the screen"
        );

        // The miss: a warning, a usable frame, and no parent — never an error.
        assert!(s.eval::<bool>("return Orphan ~= nil").unwrap());
        assert!(s.eval::<bool>("return Orphan:GetParent() == nil").unwrap());
        assert!(
            report.warnings.iter().any(|w| w.contains("NoSuchFrame")),
            "the unresolvable parent is named in a warning: {:?}",
            report.warnings
        );

        // And a name that exists but is NOT a frame is the same miss, not a raise. `_G` is one
        // namespace and an addon's own `MyAddon = {}` lives in it — the corpus writes
        // `parent="TheoryCraft"` where the addon owns that name. Handing a plain table to
        // CreateFrame would kill the element and its whole subtree.
        s.run("NotAFrame = { some = 'table' }").unwrap();
        let doc2 = parse(
            r#"<Ui>
                <Frame name="Confused" parent="NotAFrame">
                    <Size><AbsDimension x="10" y="10"/></Size>
                    <Anchors><Anchor point="CENTER"/></Anchors>
                </Frame>
            </Ui>"#,
        );
        let report2 = load(&s, &doc2, &no_files);
        assert!(report2.errors.is_empty(), "{:?}", report2.errors);
        assert!(s.eval::<bool>("return Confused ~= nil").unwrap());
        assert!(s
            .eval::<bool>("return Confused:GetParent() == nil")
            .unwrap());
        assert!(
            report2.warnings.iter().any(|w| w.contains("NotAFrame")),
            "a non-frame global of the right name is named too: {:?}",
            report2.warnings
        );
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

    /// **`<Include file="X.lua">` RUNS the file** — the client's `<Include>` is a recursion into
    /// its load-one-file routine `0x6ede10`, whose first act is a case-insensitive `.lua` suffix
    /// test on the resolved path. It never sniffs content; it never parses a `.lua` as XML.
    ///
    /// We parsed every target as FrameXML, so a `.lua` target died `unknown token at 1:1` and took
    /// the whole library set with it. Three corpus addons load nothing but these lines —
    /// FonzAppraiser, AckisRecipeList, FonzSummon — and `embeds.xml` is the whole of their Ace
    /// dependency chain.
    ///
    /// The mixed document is the point: one `<Include>` of each kind, in one file, both landing.
    #[test]
    fn include_runs_a_lua_target_and_still_parses_an_xml_one() {
        let s = UiScript::new().unwrap();
        let provider = |path: &str| -> Option<Vec<u8>> {
            match path {
                "libs/Lib.lua" => Some(b"IncludedLua = 41 + 1".to_vec()),
                // Case-insensitive, exactly as `0x64a4c0` compares it.
                "libs/Shouty.LUA" => Some(b"ShoutyLua = true".to_vec()),
                "Sub.xml" => Some(br#"<Ui><Frame name="FromXmlInclude"/></Ui>"#.to_vec()),
                _ => None,
            }
        };
        let doc = parse(
            r#"<Ui>
                <Include file="libs\Lib.lua"/>
                <Include file="libs\Shouty.LUA"/>
                <Include file="Sub.xml"/>
                <Frame name="FromMain"/>
            </Ui>"#,
        );
        let report = load(&s, &doc, &provider);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            s.eval::<i64>("return IncludedLua").unwrap(),
            42,
            "a .lua Include must be executed as a chunk, not parsed as a document"
        );
        assert!(
            s.eval::<bool>("return ShoutyLua").unwrap(),
            "case-insensitive suffix"
        );
        // ...and the XML arm is untouched: both frames still exist.
        assert_eq!(report.frames, 2);
        assert!(s
            .eval::<bool>("return FromXmlInclude ~= nil and FromMain ~= nil")
            .unwrap());
    }

    /// A raise inside an `.lua` Include is reported and the enclosing document CONTINUES — the
    /// reference logs at severity 2 and falls through (`0x6ee00d` -> `0x6ee012` is unconditional,
    /// `eax` never read), so one bad library must not cost the frames declared after it.
    #[test]
    fn a_raising_lua_include_does_not_take_the_rest_of_the_document() {
        let s = UiScript::new().unwrap();
        let provider = |path: &str| -> Option<Vec<u8>> {
            match path {
                "Bad.lua" => Some(b"error('library exploded')".to_vec()),
                _ => None,
            }
        };
        let doc = parse(
            r#"<Ui>
                <Include file="Bad.lua"/>
                <Frame name="AfterTheBadInclude"/>
            </Ui>"#,
        );
        let report = load(&s, &doc, &provider);
        assert_eq!(
            report.errors.len(),
            1,
            "the raise is reported: {:?}",
            report.errors
        );
        assert!(report.errors[0].contains("Bad.lua"));
        assert!(
            s.eval::<bool>("return AfterTheBadInclude ~= nil").unwrap(),
            "the element after a failed Include must still be built"
        );
    }

    /// `<Include>` resolved through an in-memory provider closure.
    #[test]
    fn include_resolves_through_provider() {
        let s = UiScript::new().unwrap();
        let provider = |path: &str| -> Option<Vec<u8>> {
            match path {
                "Sub.xml" => Some(br#"<Ui><Frame name="FromInclude"/></Ui>"#.to_vec()),
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

    /// A missing include is an **error**, and the rest of the load still proceeds.
    ///
    /// It was a warning until decision 1186. The load-and-continue half is unchanged and faithful
    /// (0068: the client logs and carries on) — what changed is the *reporting*, because a warning
    /// is not in the value callers assert on. Bagnon missed all eleven of its references and came
    /// back with zero errors, which read as a clean load of an addon that had built nothing.
    #[test]
    fn missing_include_errors_and_continues() {
        let s = UiScript::new().unwrap();
        let doc = parse(r#"<Ui><Include file="Nope.xml"/><Frame name="Still"/></Ui>"#);
        let report = load(&s, &doc, &no_files);
        assert!(
            report.errors.iter().any(|e| e.contains("Nope.xml")),
            "an unresolved include drops a whole document: {:?}",
            report.errors
        );
        assert!(
            s.eval::<bool>("return Still ~= nil").unwrap(),
            "and the load continues past it"
        );
    }

    /// So is a missing `<Script file=>` — it drops every handler the file would have defined.
    #[test]
    fn missing_script_file_errors_and_continues() {
        let s = UiScript::new().unwrap();
        let doc = parse(r#"<Ui><Script file="Nope.lua"/><Frame name="Still"/></Ui>"#);
        let report = load(&s, &doc, &no_files);
        assert!(
            report.errors.iter().any(|e| e.contains("Nope.lua")),
            "{:?}",
            report.errors
        );
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

    /// Unsupported handler names are warn-once gaps, not hard errors, and don't stop the frame
    /// from building. The example is `OnCursorChanged` — caret geometry is host-side here, so its
    /// four float args would all be zero, which is the silent-drop this warn exists to avoid.
    /// (It used to be `OnKeyDown`: that one is *fired* since decision 1319 and now belongs to
    /// `script::tests::keyboard`.)
    #[test]
    fn unsupported_script_name_is_a_warning() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui><Frame name="Keyed"><Scripts><OnCursorChanged>x = 1</OnCursorChanged></Scripts></Frame></Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert_eq!(report.frames, 1);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.contains("OnCursorChanged")));
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
        let provider = move |req: &str| -> Option<Vec<u8>> {
            let norm = req.replace('\\', "/");
            let base = norm.rsplit('/').next().unwrap_or(&norm);
            std::fs::read(dir.join(&norm))
                .or_else(|_| std::fs::read(dir.join(base)))
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

    /// The creation-path implicit anchor, XML half (decision 1310; wow-re
    /// `region-implicit-anchor.md`, §5 VERIFIED): a `<Texture>` with zero anchors gets
    /// SetAllPoints(parent) right after its LoadXML — two corner anchors that pin all four
    /// edges, so an authored `<Size>` is structurally unread. This is B180's engine shape: the
    /// reference stack-split plate authors a vestigial 256×32 and renders 172×96, the frame.
    #[test]
    fn sized_anchorless_layer_texture_fills_its_frame() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Frame name="Plate">
                    <Size><AbsDimension x="172" y="96"/></Size>
                    <Anchors>
                        <Anchor point="BOTTOMLEFT"><Offset><AbsDimension x="10" y="10"/></Offset></Anchor>
                    </Anchors>
                    <Layers><Layer level="BACKGROUND">
                        <Texture file="Interface\Panel">
                            <Size><AbsDimension x="256" y="32"/></Size>
                        </Texture>
                    </Layer></Layers>
                </Frame>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        s.resolve();
        let plate = s
            .extract()
            .into_iter()
            .find(|q| {
                matches!(&q.content,
                    QuadContent::Texture { path: Some(p), .. } if p == "Interface\\Panel")
            })
            .expect("the plate draws");
        let r = plate.rect.expect("the plate resolved");
        assert_eq!(
            (r.left, r.right, r.bottom, r.top),
            (10.0, 182.0, 10.0, 106.0),
            "implicit SetAllPoints fills the 172×96 frame; the 256×32 size is unread"
        );
    }

    /// The FontString half of the same law: zero anchors ⇒ ONE middle-row SetPoint chosen by the
    /// live justify word (`&7`: 1→LEFT, 4→RIGHT, else CENTER) — and the authored `<Size>` stays
    /// LIVE (single anchor + W/H sizes the opposite edges), unlike the texture's.
    #[test]
    fn anchorless_fontstring_seats_at_its_justify_point() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Frame name="Page">
                    <Size><AbsDimension x="200" y="100"/></Size>
                    <Anchors>
                        <Anchor point="BOTTOMLEFT"><Offset><AbsDimension x="0" y="0"/></Offset></Anchor>
                    </Anchors>
                    <Layers><Layer level="ARTWORK">
                        <FontString name="SeatLeft" justifyH="LEFT" text="body">
                            <Size><AbsDimension x="80" y="20"/></Size>
                        </FontString>
                    </Layer></Layers>
                </Frame>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        s.resolve();
        // LEFT→LEFT at (0,0): left edge at the page's left, vertically on the page's centerline
        // (middle-row point), 80×20 from the size. Page [0,0]..[200,100] → [0,40]..[80,60].
        let (l, r, t, b) = s
            .eval::<(f64, f64, f64, f64)>(
                "return SeatLeft:GetLeft(), SeatLeft:GetRight(), SeatLeft:GetTop(), SeatLeft:GetBottom()",
            )
            .unwrap();
        assert_eq!(
            (l, r, b, t),
            (0.0, 80.0, 40.0, 60.0),
            "justifyH=LEFT seats the implicit single anchor at the page's LEFT, size live"
        );
    }

    /// The XML state-texture ordering (decision 1310's loader half): an authored `<Anchors>` on a
    /// `<NormalTexture>` wins outright — the materializing setter's implicit SetAllPoints is
    /// cleared before the authored layout applies, so no implicit corner survives to weld the
    /// region to the button (the merchant row's icon-scoped highlight is the live consumer).
    #[test]
    fn anchored_state_texture_keeps_its_authored_anchors() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Button name="ScopedBtn">
                    <Size><AbsDimension x="36" y="36"/></Size>
                    <Anchors>
                        <Anchor point="BOTTOMLEFT"><Offset><AbsDimension x="100" y="100"/></Offset></Anchor>
                    </Anchors>
                    <NormalTexture file="Interface\Scoped">
                        <Size><AbsDimension x="24" y="24"/></Size>
                        <Anchors>
                            <Anchor point="TOPLEFT"><Offset><AbsDimension x="2" y="-2"/></Offset></Anchor>
                        </Anchors>
                    </NormalTexture>
                </Button>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        s.resolve();
        let scoped = s
            .extract()
            .into_iter()
            .find(|q| {
                matches!(&q.content,
                    QuadContent::Texture { path: Some(p), .. } if p == "Interface\\Scoped")
            })
            .expect("the state texture draws");
        let r = scoped.rect.expect("resolved");
        // Button [100,100]..[136,136]; TOPLEFT+2,-2 with 24×24 → [102,110]..[126,134].
        assert_eq!(
            (r.left, r.right, r.bottom, r.top),
            (102.0, 126.0, 110.0, 134.0),
            "authored anchors + size hold; no implicit corner welds it to the button"
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
        s.set_measured_text_unwrapped(&answers);
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

    /// **The `<Scripts>` walker auto-enables the mouse kind when it attaches a mouse handler**
    /// (`0x769ef0` → `0x76af00(2,-1)` per handler name; wow-re `ui/scratch/scripts-auto-enable.md`,
    /// §5 cross-checked) — the reference's GameTimeFrame declares `<OnEnter>`/`<OnLeave>` and no
    /// `enableMouse`, yet its tooltip hovers in the real client. The kind-2 name set is exactly
    /// {OnEnter, OnLeave, OnMouseDown, OnMouseUp, OnDragStart}: `OnDragStop`/`OnReceiveDrag` are
    /// NOT in it, and the law is XML-load-time only — the Lua SetScript binding (`0x7748d0`)
    /// never auto-enables (verified negative there).
    #[test]
    fn scripts_block_mouse_handlers_auto_enable_mouse() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui>
                <Frame name="Hoverable">
                    <Scripts><OnEnter>-- hover</OnEnter></Scripts>
                </Frame>
                <Frame name="DropTarget">
                    <Scripts><OnDragStop>-- outside the kind-2 set</OnDragStop></Scripts>
                </Frame>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            s.eval::<bool>("return Hoverable:IsMouseEnabled()").unwrap(),
            "an XML mouse handler arms EnableMouse like the attribute would"
        );
        assert!(
            !s.eval::<bool>("return DropTarget:IsMouseEnabled()")
                .unwrap(),
            "OnDragStop is outside the kind-2 name set"
        );
        // Runtime SetScript never auto-enables — the law is the XML walker's, not SetScript's.
        s.run("rt = CreateFrame('Frame', 'Rt'); rt:SetScript('OnEnter', function() end)")
            .unwrap();
        assert!(
            !s.eval::<bool>("return Rt:IsMouseEnabled()").unwrap(),
            "a runtime-created frame still needs an explicit EnableMouse"
        );
    }

    /// **`text=` is a GLOBAL-STRING LOOKUP, not a literal** (wow-re rf28 l.36/l.115 →
    /// `FrameScript_GetText 0x703bf0`). Every arm of [`Loader::resolve_text`] in one document:
    /// a `<Button text=>`, a `<ButtonText text=>` and a `<FontString text=>` all resolve through
    /// the VM's globals; a value with no matching global falls back to the LITERAL (benilla's own
    /// divergence, so its plain-English FrameXML keeps working); and a **key-shaped** miss warns,
    /// which is the tripwire that "CREATE_MACROS" across a title bar never had.
    #[test]
    fn a_text_attribute_resolves_through_the_global_strings() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        // The reference boots GlobalStrings.lua before any XML; so does the app.
        s.run(r#"DELETE = "Delete" EXIT_GAME = "Exit Game" TITLE = "The Title""#)
            .unwrap();

        let doc = parse(
            r#"<Ui>
                <Frame name="Holder">
                    <Layers>
                        <Layer level="ARTWORK">
                            <FontString name="$parentTitle" text="TITLE"/>
                            <FontString name="$parentProse" text="No results found."/>
                        </Layer>
                    </Layers>
                    <Frames>
                        <Button name="$parentDelete" text="DELETE"/>
                        <Button name="$parentQuit">
                            <ButtonText name="$parentText" text="EXIT_GAME"/>
                        </Button>
                        <Button name="$parentGhost" text="NO_SUCH_KEY"/>
                    </Frames>
                </Frame>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "{:?}", report.errors);

        let texts: Vec<String> = s
            .eval(
                "return HolderTitle:GetText(), HolderProse:GetText(), HolderDelete:GetText(), \
                 HolderQuit:GetText(), HolderGhost:GetText()",
            )
            .map(|(a, b, c, d, e): (String, String, String, String, String)| vec![a, b, c, d, e])
            .unwrap();
        assert_eq!(
            texts,
            vec![
                // The key's VALUE, on all three element shapes…
                "The Title",
                // …a non-key literal untouched (the divergence that keeps our own XML working)…
                "No results found.",
                "Delete",
                "Exit Game",
                // …and a key-shaped MISS keeps its key on screen rather than blanking.
                "NO_SUCH_KEY",
            ]
        );
        // Exactly one warning, and it names the miss — the plain-English literal must not warn.
        assert_eq!(
            report
                .warnings
                .iter()
                .filter(|w| w.contains("GlobalStrings key"))
                .count(),
            1,
            "warnings: {:?}",
            report.warnings
        );
        assert!(report.warnings.iter().any(|w| w.contains("NO_SUCH_KEY")));
    }

    /// The key SHAPE test itself: `SCREAMING_SNAKE`, two characters or more. The floor is what
    /// keeps a close button's `text="X"` from being reported as a missing string.
    #[test]
    fn key_shape_is_screaming_snake_of_two_or_more() {
        for yes in ["DELETE", "EXIT_GAME", "CHARACTER_POINTS1_COLON", "AB", "A1"] {
            assert!(is_global_string_key(yes), "{yes} is key-shaped");
        }
        for no in ["X", "", "Send Mail", "No results found.", "Okay", "1", "12"] {
            assert!(!is_global_string_key(no), "{no} is not key-shaped");
        }
    }

    // ── <SimpleHTML> (the markup widget's XML element) ───────────────────────────────────────

    /// **`<SimpleHTML>`'s `<FontString>` child is a font DECLARATION, not a region** — the shape
    /// stock `ItemTextFrame.xml` uses, and the whole reason its `<H1>` renders at the `<P>` size.
    ///
    /// Four claims in one document: the element materializes as a real `SimpleHTML` frame with its
    /// `<Size>`/`<Anchors>`/`<Scripts>` applied like any frame; the direct-child `<FontString
    /// inherits=>` lands on `elementFont[0]` rather than creating a FontString region; a
    /// `<FontStringHeader1>` lands on `elementFont[1]`; and `hyperlinkFormat=` reaches `+0x360`.
    #[test]
    fn simplehtml_xml_declares_element_fonts_not_regions() {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(800.0, 600.0);
        let doc = parse(
            r#"<Ui>
                <Font name="ItemTextFontNormal" font="Fonts\MORPHEUS.TTF" justifyH="LEFT">
                    <FontHeight val="15"/>
                    <Color r="0.18" g="0.12" b="0.06"/>
                </Font>
                <Font name="BookHeader" font="Fonts\SKURRI.TTF">
                    <FontHeight val="24"/>
                </Font>
                <SimpleHTML name="PageText" hyperlinkFormat="|H%s|h[%s]|h">
                    <Size><AbsDimension x="270" y="304"/></Size>
                    <Anchors><Anchor point="TOPLEFT"/></Anchors>
                    <FontString inherits="ItemTextFontNormal"/>
                    <FontStringHeader1 inherits="BookHeader"/>
                    <Scripts><OnLoad>LOADED = this:GetName()</OnLoad></Scripts>
                </SimpleHTML>
            </Ui>"#,
        );
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);

        assert_eq!(s.eval::<String>("return LOADED").unwrap(), "PageText");
        assert_eq!(
            s.eval::<String>("return PageText:GetObjectType()").unwrap(),
            "SimpleHTML"
        );
        assert_eq!(
            s.eval::<String>("return PageText:GetHyperlinkFormat()")
                .unwrap(),
            "|H%s|h[%s]|h"
        );
        // The two declarations landed on their own elements…
        assert_eq!(
            s.eval::<(String, f32)>("local p, h = PageText:GetFont(); return p, h")
                .unwrap(),
            ("Fonts\\MORPHEUS.TTF".to_string(), 15.0)
        );
        assert_eq!(
            s.eval::<(String, f32)>("local p, h = PageText:GetFont(\"H1\"); return p, h")
                .unwrap(),
            ("Fonts\\SKURRI.TTF".to_string(), 24.0)
        );
        // …and NEITHER became a region. A `<FontString>` handled by the generic special-fontstring
        // pass would leave an unanchored, textless string on the frame here.
        let regions = {
            let lua = s.lua();
            let model = lua
                .app_data_ref::<crate::script::Model>()
                .expect("model app_data");
            let fh = model.arena.lookup("PageText").expect("frame");
            model.arena.frame(fh).expect("live").regions.len()
        };
        assert_eq!(regions, 0, "the font declarations created no regions");

        // Now drive it: three blocks, the H1 in its OWN declared face, at the frame's width.
        s.run(
            r#"PageText:SetText("<HTML><BODY><H1>Title</H1><P align=\"center\">Body</P></BODY></HTML>")"#,
        )
        .unwrap();
        let (kinds, fonts, widths) = {
            let lua = s.lua();
            let model = lua
                .app_data_ref::<crate::script::Model>()
                .expect("model app_data");
            let fh = model.arena.lookup("PageText").expect("frame");
            let blocks = &model.simple_html.get(&fh).expect("state").blocks;
            (
                blocks.len(),
                blocks
                    .iter()
                    .map(|rh| model.region_data[rh].font_path.clone())
                    .collect::<Vec<_>>(),
                blocks
                    .iter()
                    .map(|rh| model.region_data[rh].size)
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(kinds, 2);
        assert_eq!(
            fonts,
            [
                Some("Fonts\\SKURRI.TTF".to_string()),
                Some("Fonts\\MORPHEUS.TTF".to_string())
            ],
            "a DECLARED header font wins; the P falls through to its own"
        );
        assert_eq!(widths, [Some((270.0, 0.0)); 2]);
        assert!(s.errors().is_empty(), "{:?}", s.errors());
    }
}

mod layer_blend_tests {
    use crate::framexml::parse;
    use crate::loader::*;
    use crate::script::{QuadContent, UiScript};

    fn no_files(_: &str) -> Option<Vec<u8>> {
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

    fn no_files(_: &str) -> Option<Vec<u8>> {
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
        // …and the font it inherited is READABLE BACK, which is the half this test did not ask.
        // `EQL3_Options.lua:1086` is the corpus line that needs it: it reads the tracker line's
        // font with `t1, _, t2 = EQL3_QuestWatchLine1:GetFont()` and feeds `t1` straight into
        // `temp:SetFont(t1, height, t2)` — so a nil path there is not a cosmetic gap, it is our own
        // faithful `Usage: <FontString>:SetFont(...)` raise firing on our own nil.
        assert_eq!(
            s.eval::<(String, f32)>("local f, h = Label:GetFont() return f, h")
                .unwrap(),
            ("Fonts\\FRIZQT__.TTF".to_string(), 12.0),
            "a FontString that inherits a Font object reports that object's font"
        );
    }

    /// **A font-object name resolves case-INSENSITIVELY**, because the client's font registry
    /// compares its keys with `SStrCmpI` (`0x783870`/`0x7838c7`, wow-re
    /// `font-object-lua-surface.md`: *"Font names are matched case-insensitively"*).
    ///
    /// `Recap/RecapOptions.xml:32` inherits `GameFontHighLightSmall`; the shipped font is
    /// `GameFontHighlightSmall`, one letter's case apart. On the real client that resolves, and it
    /// used to be a warn-once gap here — a NARROWING fix, like the unit-token fold (1247).
    #[test]
    fn a_font_object_inherits_resolves_whatever_its_case() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui>
                <Font name="GameFontHighlightSmall" font="Fonts\FRIZQT__.TTF" virtual="true">
                    <FontHeight><AbsValue val="10"/></FontHeight>
                </Font>
                <Frame name="CaseHost">
                    <Size><AbsDimension x="100" y="30"/></Size>
                    <Anchors><Anchor point="TOPLEFT" relativePoint="TOPLEFT"/></Anchors>
                    <Layers><Layer level="ARTWORK">
                        <FontString name="CaseLabel" inherits="GameFontHighLightSmall" text="hi"/>
                    </Layer></Layers>
                </Frame>
            </Ui>"#,
        )
        .unwrap();
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "a differently-cased font name must resolve, not warn: {:?}",
            report.warnings
        );
        assert_eq!(
            s.eval::<(String, f32)>("local f, h = CaseLabel:GetFont() return f, h")
                .unwrap(),
            ("Fonts\\FRIZQT__.TTF".to_string(), 10.0),
            "the mis-cased inherit found the shipped font"
        );
    }

    /// **EQL3's actual shape: FontString -> virtual FontString TEMPLATE -> Font object.** One hop
    /// works (above); this is the two-hop chain every "define a line template once, stamp it N
    /// times" addon writes, and `EQL3_Tracker.xml:6/32` is the corpus instance —
    /// `EQL3_QuestWatch_FontTemplate` inherits `GameFontHighlight`, and each
    /// `EQL3_QuestWatchLine<i>` inherits the template.
    #[test]
    fn a_fontstring_template_carries_the_font_object_it_inherits() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui>
                <Font name="GameFontHighlight" font="Fonts\FRIZQT__.TTF" virtual="true">
                    <FontHeight><AbsValue val="12"/></FontHeight>
                </Font>
                <FontString name="LineTemplate" inherits="GameFontHighlight" virtual="true"
                            justifyH="LEFT"/>
                <Frame name="Host3">
                    <Size><AbsDimension x="100" y="30"/></Size>
                    <Anchors><Anchor point="TOPLEFT" relativePoint="TOPLEFT"/></Anchors>
                    <Layers><Layer level="ARTWORK">
                        <FontString name="Line1" inherits="LineTemplate" text="one"/>
                    </Layer></Layers>
                </Frame>
            </Ui>"#,
        )
        .unwrap();
        let report = load(&s, &doc, &no_files);
        assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
        assert_eq!(s.eval::<String>("return Line1:GetText()").unwrap(), "one");
        assert_eq!(
            s.eval::<(String, f32)>("local f, h = Line1:GetFont() return f, h")
                .unwrap(),
            ("Fonts\\FRIZQT__.TTF".to_string(), 12.0),
            "the font survives BOTH hops — template inheritance must not drop it"
        );
    }
}

/// Chunk naming — which FILE and which LINE a `<Script>` raise reports (decision 1217).
mod chunk_name_tests {
    use crate::framexml;
    use crate::loader::*;
    use crate::script::UiScript;

    fn no_files(_: &str) -> Option<Vec<u8>> {
        None
    }

    fn parse(text: &str) -> framexml::ParsedDocument {
        framexml::parse(text).expect("valid FrameXML")
    }

    /// **A raise inside a `<Script>` block names the file it came from, and the file's own line.**
    ///
    /// Both halves are load-bearing and both were wrong. `Loader::run` loaded every chunk with no
    /// name, and mlua's `load` is `#[track_caller]` — so the chunk was named after the *Rust* line
    /// that ran it, and a corpus read-back of 70 failures had 26 rows saying
    /// `crates/benilla-ui/src/loader/mod.rs:327` where the addon's file belonged (decision 1217).
    ///
    /// Naming it is only half. A Lua chunk starts at line 1, so an inline block 40 lines down a
    /// file would report `File.xml:3` for its third line — a lie you can go and check, which is
    /// strictly worse than the Rust path that was at least obviously not yours. The chunk is padded
    /// to the block's own offset, so the number is the number you scroll to.
    #[test]
    fn a_script_raise_names_its_own_file_and_line() {
        let s = UiScript::new().unwrap();
        // `error()` with no level prefixes "chunkname:line:" — exactly what a real raise carries.
        let doc = parse(
            "<Ui>\n\
             <Frame name=\"Pad\"/>\n\
             <Script>\n\
             local x\n\
             error(\"boom\")\n\
             </Script>\n\
             </Ui>",
        );
        let report = load_in(&s, &doc, "Bagnon/src/main.xml", &no_files);
        let err = report.errors.join("\n");
        // The file, in the shape an addon's own `debugstack()` matches (backslashes, no `@`).
        assert!(
            err.contains("Bagnon\\src\\main.xml:"),
            "the raise must name the document, got: {err}"
        );
        // Line 5 of the literal above is `error("boom")` — the body starts on line 4 and the
        // padding carries it there. Without the pad this reads `:2:`.
        assert!(
            err.contains("main.xml:5:"),
            "the line must be the FILE's line, not the block's, got: {err}"
        );
    }

    /// The same, one level down: an `<Include>`d document's inline script names **its own** file,
    /// not the includer's. The path is saved and restored around the recursion exactly as the
    /// directory used to be, so depth-1 and depth-5 each report themselves.
    #[test]
    fn an_included_documents_script_names_the_included_file() {
        let s = UiScript::new().unwrap();
        let inner = "<Ui>\n<Script>\nerror(\"inner\")\n</Script>\n</Ui>";
        let files = |req: &str| -> Option<Vec<u8>> {
            (req == "Addon/sub/inner.xml").then(|| inner.as_bytes().to_vec())
        };
        let doc = parse("<Ui>\n<Include file=\"sub\\inner.xml\"/>\n</Ui>");
        let report = load_in(&s, &doc, "Addon/outer.xml", &files);
        let err = report.errors.join("\n");
        assert!(
            err.contains("Addon\\sub\\inner.xml:3:"),
            "the INCLUDED file and its line, not the includer's: {err}"
        );
        assert!(
            !err.contains("outer.xml"),
            "the includer must not be blamed: {err}"
        );
    }

    /// **The CDATA shape, which is the only shape our own FrameXML uses.** `<![CDATA[` sits
    /// between the element's start and the body's first byte, so taking the line from the
    /// *element* would be short by however much of the opening tag precedes it — and a
    /// single-line-offset lie is the hardest kind to notice. The line comes from the text child's
    /// own range instead, which is what this pins.
    #[test]
    fn a_cdata_script_block_reports_the_files_line() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            "<Ui>\n\
             <Frame name=\"A\"/>\n\
             <Frame name=\"B\"/>\n\
             <Script><![CDATA[\n\
             local ok = 1\n\
             error(\"cdata boom\")\n\
             ]]></Script>\n\
             </Ui>",
        );
        let report = load_in(&s, &doc, "Ours/Bag.xml", &no_files);
        let err = report.errors.join("\n");
        assert!(
            err.contains("Ours\\Bag.xml:6:"),
            "line 6 is `error(\"cdata boom\")` in the literal above, got: {err}"
        );
    }

    /// A `<Script file=>` chunk is named after the file it loaded, not the document that named it.
    #[test]
    fn a_script_file_chunk_is_named_after_that_file() {
        let s = UiScript::new().unwrap();
        let files = |req: &str| -> Option<Vec<u8>> {
            (req == "Addon/code.lua").then(|| b"\nerror(\"from lua\")\n".to_vec())
        };
        let doc = parse("<Ui>\n<Script file=\"code.lua\"/>\n</Ui>");
        let report = load_in(&s, &doc, "Addon/host.xml", &files);
        let err = report.errors.join("\n");
        assert!(
            err.contains("Addon\\code.lua:2:"),
            "the .lua file and its own line: {err}"
        );
    }

    /// **A captured script, called back the reference's way, still knows its frame.**
    ///
    /// This is *the* 1.12 hook idiom and the whole reason `GetScript` exists: an addon takes the
    /// client's handler, installs its own, and calls the original with **no arguments**, because
    /// the reference's contract passes the frame as the `this` global and nothing else. Our
    /// wrapper is `function(self, ...)` and our own FrameXML is written against `self`, so before
    /// the fallback in [`super::Loader::compile_handler`] that re-entry handed every body a nil
    /// frame. The director met it as `bad argument #2: error converting Lua nil to table` out of
    /// `GameTooltip:SetOwner`, from a handler that works perfectly when the engine calls it.
    ///
    /// Both spellings are asserted, because the fallback must not cost the engine's own call.
    #[test]
    fn a_script_captured_and_called_with_no_arguments_still_sees_its_frame() {
        let s = UiScript::new().unwrap();
        let doc = parse(
            r#"<Ui>
                <Frame name="Hooked" hidden="true">
                    <Scripts>
                        <OnShow>SEEN = self:GetName()</OnShow>
                    </Scripts>
                </Frame>
            </Ui>"#,
        );
        let report = load_in(&s, &doc, "Test.xml", &no_files);
        assert!(report.errors.is_empty(), "{:?}", report.errors);

        // The engine's own call: `self` is passed and `this` is set — the modern spelling.
        s.run("SEEN = nil Hooked:Show()").unwrap();
        assert_eq!(s.eval::<String>("return SEEN").unwrap(), "Hooked");

        // The addon's: capture the script, then call it back bare with only `this` standing in for
        // the frame — exactly `Bagnon.lua:87`'s `bMainBag_OnEnter()`.
        s.run(
            "SEEN = nil \
             local original = Hooked:GetScript(\"OnShow\") \
             this = Hooked \
             original() \
             this = nil",
        )
        .unwrap();
        assert_eq!(
            s.eval::<String>("return SEEN").unwrap(),
            "Hooked",
            "the reference's own no-argument contract must reach the same frame"
        );
    }
}

/// [`super::join_ref`] — the FrameXML path rule (decision 1186).
///
/// A reference is relative to the directory of the file containing it, `\` and `/` are the same
/// separator, and `..` walks up. The escape case is the load-bearing one: a `..` above the root is
/// KEPT as a leading `..` rather than clamped away, because the provider has to be able to SEE the
/// escape to refuse it. Clamping would silently hand it a path that looks contained.
#[test]
fn join_ref_resolves_relative_framexml_paths() {
    use super::join_ref;
    // Relative to the including file's own directory.
    assert_eq!(
        join_ref("Bagnon/src", "templates.xml"),
        "Bagnon/src/templates.xml"
    );
    // Backslashes are separators, and `..` walks up — this is how a library addon is reached.
    assert_eq!(
        join_ref("Bagnon/src", "..\\..\\BagBrother\\core\\core.xml"),
        "BagBrother/core/core.xml"
    );
    // At the root, a bare name stays bare (the builtin's flat tree).
    assert_eq!(join_ref("", "Fonts.xml"), "Fonts.xml");
    // `.` is a no-op; redundant separators collapse.
    assert_eq!(join_ref("a/b", "./c//d.xml"), "a/b/c/d.xml");
    // An escape above the root SURVIVES, so the provider can refuse it.
    assert_eq!(join_ref("a", "../../secret"), "../secret");
    assert_eq!(join_ref("", "../secret"), "../secret");
    // A leading `/` re-roots rather than meaning the filesystem root.
    assert_eq!(join_ref("a/b", "/c.xml"), "c.xml");
}
