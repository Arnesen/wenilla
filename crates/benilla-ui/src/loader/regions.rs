use mlua::{ObjectLike, Table};

use crate::framexml::{self, Element};

use super::{abs_dim, abs_value, children_named, color_of, tex_coords_of, Loader};

impl Loader<'_> {
    /// `<Layers>`/`<Layer level=>`/`<Texture>`/`<FontString>` (rf24 `0x769d70`): create each region
    /// via CreateTexture/CreateFontString on the draw layer, then apply its file/color/text.
    pub(super) fn apply_layers(
        &mut self,
        el: &Element,
        wrapper: &Table,
        parent_name: &str,
        dbg: &str,
    ) {
        for layers in children_named(el, "Layers") {
            for layer in children_named(layers, "Layer") {
                let level = layer.attr("level").unwrap_or("ARTWORK").to_string();
                for region in &layer.children {
                    // A region's `inherits=` may name a virtual REGION template (the talent
                    // window's branch/arrow art pool) — splice it exactly like a frame's
                    // (rf24's one template registry serves both). A FontString's `inherits=`
                    // usually names a font OBJECT instead — a separate namespace this gate
                    // skips past to `apply_fontstring_font` below.
                    let region = &self.expand_region(region);
                    let is_texture = region.tag.eq_ignore_ascii_case("Texture");
                    let is_fontstring = region.tag.eq_ignore_ascii_case("FontString");
                    if !is_texture && !is_fontstring {
                        self.report.warnings.push(format!(
                            "{dbg}: <Layer> child <{}> is not a Texture/FontString; skipped",
                            region.tag
                        ));
                        continue;
                    }
                    let rname: Option<String> = region
                        .name()
                        .map(|raw| framexml::resolve_name(raw, parent_name));
                    let method = if is_texture {
                        "CreateTexture"
                    } else {
                        "CreateFontString"
                    };
                    let region_wrapper: Table =
                        match wrapper.call_method(method, (rname, Some(level.clone()))) {
                            Ok(w) => w,
                            Err(e) => {
                                self.report.errors.push(format!("{dbg}: {method}: {e}"));
                                continue;
                            }
                        };
                    // hidden= on a Texture/FontString — the region-level VisibleRegion bit.
                    if region.attr_bool("hidden") {
                        if let Err(e) = region_wrapper.call_method::<()>("Hide", ()) {
                            self.report.errors.push(format!("{dbg}: region Hide: {e}"));
                        }
                    }
                    // alpha= on a Texture/FontString — the region's own alpha, the frame attr's twin.
                    if let Some(a) = region
                        .attr("alpha")
                        .and_then(|v| v.trim().parse::<f32>().ok())
                    {
                        self.call_region(&region_wrapper, "SetAlpha", a, dbg);
                    }
                    self.apply_region_layout(region, &region_wrapper, parent_name, dbg);
                    if is_fontstring {
                        // Font object (`inherits=`) first, then the FontString's own `font=`/
                        // `<FontHeight>`/`outline=` overrides — before the generic visual pass applies
                        // `text`/`<Color>` on top (an explicit `<Color>` overriding the object's).
                        self.apply_fontstring_font(region, &region_wrapper, dbg);
                    }
                    self.apply_region_visual(region, &region_wrapper, is_texture, dbg);
                    // The creation-path implicit anchor (decision 1310): AFTER the authored
                    // `<Size>`/`<Anchors>`/justify are on the region, exactly where the real
                    // engine runs it (`0x7701c0`/`0x771480`, immediately after the region's
                    // LoadXML returns). A region the XML anchored is untouched.
                    if let Err(e) =
                        crate::script::implicit_creation_anchor_lua(self.lua, &region_wrapper)
                    {
                        self.report
                            .errors
                            .push(format!("{dbg}: implicit anchor: {e}"));
                    }
                }
            }
        }
    }

    /// A frame's **direct-child** `<FontString>` (outside `<Layers>`): the client's "special" font
    /// string — the ScrollingMessageFrame's line font, an EditBox's text font — which real FrameXML
    /// attaches at the OVERLAY layer. Only ChatFrame uses this in our shipped XML; without it the
    /// chat lines fall back to the default face with no shadow. Created via the same
    /// CreateFontString + layout/font/visual path as a `<Layers>` FontString.
    pub(super) fn apply_special_fontstrings(
        &mut self,
        el: &Element,
        wrapper: &Table,
        parent_name: &str,
        dbg: &str,
    ) {
        for region in &el.children {
            if !region.tag.eq_ignore_ascii_case("FontString") {
                continue; // <Layers>/<Frames>/<Scripts>/... are handled by their own passes
            }
            let region = &self.expand_region(region);
            let rname: Option<String> = region
                .name()
                .map(|raw| framexml::resolve_name(raw, parent_name));
            let region_wrapper: Table = match wrapper
                .call_method("CreateFontString", (rname, Some("OVERLAY".to_string())))
            {
                Ok(w) => w,
                Err(e) => {
                    self.report
                        .errors
                        .push(format!("{dbg}: CreateFontString (special): {e}"));
                    continue;
                }
            };
            self.apply_region_layout(region, &region_wrapper, parent_name, dbg);
            self.apply_fontstring_font(region, &region_wrapper, dbg);
            self.apply_region_visual(region, &region_wrapper, false, dbg);
            // The creation-path implicit anchor (decision 1310), as in `apply_layers`. For the
            // EditBox adopt below it is moot — `write_inset_anchors` replaces the anchor set
            // wholesale — but a plain special FontString (a chat window's) gets the same
            // justify-point the real engine gives it.
            if let Err(e) = crate::script::implicit_creation_anchor_lua(self.lua, &region_wrapper) {
                self.report
                    .errors
                    .push(format!("{dbg}: implicit anchor: {e}"));
            }
            // The engine's LoadXML slot assignment: an EditBox's direct-child `<FontString>` IS
            // its text region — typed text renders in its font, the box's insets anchor it.
            // Assigned, never searched (a find-first adoption once grabbed the chat header out of
            // `<Layers>`: typing overwrote "Say:" and the insets re-anchor centered it).
            if el.tag.eq_ignore_ascii_case("EditBox") {
                if let Err(e) =
                    crate::script::adopt_text_region(self.lua(), wrapper, &region_wrapper)
                {
                    self.report
                        .errors
                        .push(format!("{dbg}: adopt special FontString: {e}"));
                }
            }
        }
    }

    /// A region's own geometry (decision 0068 v1): `<Size>` → SetWidth/SetHeight, `<Anchors>` →
    /// SetPoint (owner-relative; `relativeTo` is `$parent`-substituted like a frame's), the
    /// `setAllPoints="true"` shorthand, and the FontString `justifyH` attr → SetJustifyH. This is the
    /// join that makes region `<Size>`/`<Anchors>` actually place the region (before, both were
    /// silently dropped — the smeared-merchant root cause).
    pub(super) fn apply_region_layout(
        &mut self,
        region: &Element,
        wrapper: &Table,
        parent_name: &str,
        dbg: &str,
    ) {
        // ALL `<Size>` children in document order (same last-wins rule as `apply_size`: a templated
        // region's own `<Size>` must overwrite its template's).
        for size in children_named(region, "Size") {
            let (x, y) = abs_dim(size);
            if let Some(w) = x {
                self.call_region(wrapper, "SetWidth", w, dbg);
            }
            if let Some(h) = y {
                self.call_region(wrapper, "SetHeight", h, dbg);
            }
        }
        if let Some(j) = region.attr("justifyH") {
            self.call_region(wrapper, "SetJustifyH", j.to_string(), dbg);
        }
        if let Some(j) = region.attr("justifyV") {
            self.call_region(wrapper, "SetJustifyV", j.to_string(), dbg);
        }
        if region.attr_bool("setAllPoints") {
            self.call_region(wrapper, "SetAllPoints", (), dbg);
        }
        for anchors in children_named(region, "Anchors") {
            for anchor in children_named(anchors, "Anchor") {
                let Some(point) = anchor.attr("point") else {
                    self.report
                        .warnings
                        .push(format!("{dbg}: region <Anchor> without a point; skipped"));
                    continue;
                };
                let rel_point = anchor.attr("relativePoint").unwrap_or(point).to_string();
                let rel_to: Option<String> = anchor
                    .attr("relativeTo")
                    .map(|r| framexml::resolve_name(r, parent_name));
                let (x, y) = children_named(anchor, "Offset")
                    .next()
                    .map(abs_dim)
                    .unwrap_or((None, None));
                self.call_region(
                    wrapper,
                    "SetPoint",
                    (
                        point.to_string(),
                        rel_to,
                        rel_point,
                        x.unwrap_or(0.0),
                        y.unwrap_or(0.0),
                    ),
                    dbg,
                );
            }
        }
    }

    pub(super) fn apply_region_visual(
        &mut self,
        region: &Element,
        region_wrapper: &Table,
        is_texture: bool,
        dbg: &str,
    ) {
        let color = children_named(region, "Color").next().map(color_of);
        if is_texture {
            if let Some(file) = region.attr("file") {
                // A `<Texture file=…>` with a `<Color>` child DISCARDS the colour — it is not a
                // tint (wow-re `system/ui/scratch/texture-color-composition.md`, VERIFIED).
                // `CSimpleTexture::LoadXML 0x76fe20` runs its child loop (`0x76fec1`-`0x7700fc`,
                // where `<Color>` lands in `SetTexture(const CImVector*)` `0x770360`) to completion
                // BEFORE it reads `file=` at `0x770102`, and a successful load overwrites the very
                // same `+0xcc` slot (`0x7702cb`/`0x7702dc`) — the generated solid is released
                // unused. Even the load-FAILURE fallback ignores it, hard-coding `0xff00ff00`
                // (`0x77016a`). So: load the art, drop the colour.
                self.call_region(region_wrapper, "SetTexture", file.to_string(), dbg);
            } else if let Some(c) = color {
                // No file: a solid colour fill (SetTexture(r,g,b,a)) — the client really generates
                // an 8×8 texture out of it, which is why its alpha later MULTIPLIES with any
                // SetVertexColor rather than being replaced by it (`RegionData::fill`).
                self.call_region(region_wrapper, "SetTexture", (c[0], c[1], c[2], c[3]), dbg);
            }
            // `<Gradient orientation><MinColor/><MaxColor/></Gradient>` — the texture's FOUR-vertex
            // colour, and the one XML surface that tints a `file=` texture. It survives alongside
            // the art because it lands in a different field: the child loop writes the vertex
            // colours (`+0xb8`, stride 4 — `0x77304d`-`0x77305f`, "4 iff count > 1") while `file=`
            // is read only afterwards into `+0xcc`
            // (`texture-color-composition.md` §1-2). That is why `<Color>` beside a `file=` is
            // discarded above and a `<Gradient>` beside one is not.
            //
            // Dropping this silently is what painted Bagnon's character-list popup as a WHITE SLAB:
            // its background art is `ChatFrameBackground`, which is white by design and is meant to
            // be tinted black→dark-grey by exactly this element (`Bagnon_Core/core/Frame.xml:32`).
            //
            // MinColor → the first stop, MaxColor → the second, which is the natural reading and
            // the Lua binding's own argument order — but the XML arm's stop-to-vertex mapping is
            // **not byte-read** (wow-re records the `<Gradient>` tag compare at `0x7700b0` and the
            // stride, not the arm's body). It cannot matter yet: `RegionData::gradient` is folded to
            // its midpoint by the paint (`script::extract`), which is symmetric in the two stops.
            // Swapping them becomes visible the day a quad carries a real two-stop tint.
            if let Some(g) = children_named(region, "Gradient").next() {
                let stop = |tag: &str| children_named(g, tag).next().map(color_of);
                // A half-declared gradient is skipped whole rather than half-applied against an
                // invented default — an absent stop is not a black one.
                if let (Some(min), Some(max)) = (stop("MinColor"), stop("MaxColor")) {
                    self.call_region(
                        region_wrapper,
                        "SetGradientAlpha",
                        (
                            g.attr("orientation").unwrap_or_default().to_string(),
                            min[0],
                            min[1],
                            min[2],
                            min[3],
                            max[0],
                            max[1],
                            max[2],
                            max[3],
                        ),
                        dbg,
                    );
                }
            }
            // `<TexCoords left right top bottom>`: the UV sub-rect this texture samples (decision
            // 0084 — quadrant/atlas slicing). ref-MerchantFrame.xml l.259/268/395.
            if let Some(tc) = tex_coords_of(region) {
                self.call_region(region_wrapper, "SetTexCoord", tc, dbg);
            }
            // `alphaMode="ADD"` on a `<Layers>` texture — the same blend attribute the state-texture
            // path already applies; dropping it here rendered every layer glow (the quest log's
            // selection highlight, ref UI-QuestLogTitleHighlight) as an opaque gray bar instead of
            // an additive glow (the 0109 look fix).
            if let Some(mode) = region.attr("alphaMode") {
                self.call_region(region_wrapper, "SetBlendMode", mode.to_string(), dbg);
            }
        } else {
            if let Some(text) = region.attr("text") {
                // `<FontString text=>` is a global-string lookup, not a literal — rf28 l.115
                // (`0x703bf0`). See `Loader::resolve_text`.
                let text = self.resolve_text(text, dbg);
                self.call_region(region_wrapper, "SetText", Some(text), dbg);
            }
            if let Some(c) = color {
                self.call_region(
                    region_wrapper,
                    "SetVertexColor",
                    (c[0], c[1], c[2], c[3]),
                    dbg,
                );
            }
        }
    }

    /// Follow a FontString's `inherits=` to the **font object** at the end of it, through any
    /// virtual FontString templates in between.
    ///
    /// Returns the name unchanged when it already names a font object (the one-hop case), the
    /// template chain's font object when it does not, and `None` when the chain ends without one —
    /// which is the case the caller still warns about, because an `inherits=` naming nothing at all
    /// is a real gap and not something to swallow.
    ///
    /// `inherits=` is comma-separated in general; the first entry that resolves wins, which matches
    /// the single-font reality (a region cannot wear two faces) without pretending the list cannot
    /// exist. Depth is bounded: a template registry can contain a cycle, and a loader that hangs on
    /// a malformed addon is worse than one that gives up on it.
    fn font_object_through_templates(&self, inherits: &str) -> Option<String> {
        // **The probes here stay case-EXACT, and the outcome is still folded.** Font names match
        // case-insensitively in 1.12 (`0x783870`, `SStrCmpI`), and that fold lives in
        // `Model::font_object` — so a miss here falls through to the caller's
        // `unwrap_or_else(|| name.to_string())` and `SetFontObject` resolves it anyway.
        //
        // Not folded HERE because the `framexml_fonts` element registry's other reader hands its
        // keys to `framexml::expand`, which is the SAME expansion templates go through — and
        // whether the client matches TEMPLATE names case-insensitively is not carved. Folding this
        // registry would quietly extend a verified font fact to unverified template behaviour.
        const MAX_HOPS: usize = 8;
        let model = self.model();
        let fonts = model.framexml_fonts.borrow();
        let templates = model.framexml_templates.borrow();
        // One name, not a comma list — 1.12's lookup has no splitter, and supporting one is a
        // superset of it. Folded on the probe, like the resolution itself.
        for entry in [inherits] {
            let mut name = entry.to_string();
            for _ in 0..MAX_HOPS {
                if fonts.contains_key(&name) || fonts.keys().any(|k| k.eq_ignore_ascii_case(&name))
                {
                    return Some(name);
                }
                let Some(next) = templates
                    .get(&name)
                    .and_then(|t| t.attr("inherits"))
                    .map(str::to_string)
                else {
                    break;
                };
                name = next;
            }
        }
        None
    }

    /// Is `name` a registered `<Font>` OBJECT? The plain registry probe — **no template walk**,
    /// unlike [`Self::font_object_through_templates`]. This is the reference's `0x783870(value,
    /// create = 0)` lookup, which is a flat registry hit or nothing; the template chain is
    /// `inherits=`'s affordance alone.
    ///
    /// Case-folded, because 1.12 matches font names with `SStrCmpI` (`0x783870`).
    fn is_font_object(&self, name: &str) -> bool {
        let model = self.model();
        let fonts = model.framexml_fonts.borrow();
        fonts.contains_key(name) || fonts.keys().any(|k| k.eq_ignore_ascii_case(name))
    }

    /// A `<FontString>`'s font resolution: `inherits="GameFontNormal"` → `SetFontObject` (the named
    /// virtual Font object's face/height/color/outline become this string's defaults), then the
    /// FontString's own `font=`/`<FontHeight>`/`outline=` → `SetFont` overrides. Called before the
    /// generic visual pass (so an explicit `<Color>` still overrides the object's color). An
    /// unregistered `inherits=` target is a warn-once gap, not a dropped region.
    ///
    /// **`font=` is a font-object NAME first and a file path only on a miss** — the half of this
    /// that was missing, and a silent one. The reference tries the registry before the filesystem:
    /// `0x783d15 call 0x783870(value, create = 0)`, and on a hit takes the same live-link
    /// `0x770c60` that `SetFontObject` uses, skipping the file-path branch entirely
    /// (wow-re `system/ui/scratch/font-object-lua-surface.md` §7 + its `LoadXML` surface table
    /// row 2; `CSimpleFontString::LoadXML 0x770f40` at `0x7710e1`-`0x771114`).
    ///
    /// Reading it as a path only is what made Bagnon's item-stack counts unreadable: its
    /// `<FontString name="$parentCount" font="NumberFontNormal">` (`Bagnon_Core/core/Item.xml:14`)
    /// stored the literal string `"NumberFontNormal"` as a font PATH, which resolves to no face —
    /// so height, colour and, above all, `outline="NORMAL"` were all lost, and the atlas fell back
    /// silently to Friz 12. The black ring around a stack count is that outline (`NumberFontNormal`
    /// carries no `<Shadow>` in 1.12 — the readability is the halo, not a drop shadow).
    ///
    /// Real FrameXML never exercises this: it writes `font=` on `<Font>` elements only, always as a
    /// path. The addon corpus writes it on FontStrings 117 times.
    pub(super) fn apply_fontstring_font(&mut self, region: &Element, wrapper: &Table, dbg: &str) {
        if let Some(name) = region.attr("inherits") {
            // **Resolved through the TEMPLATE chain, not taken literally.** A FontString's
            // `inherits=` may name a font object *or* a virtual FontString template, and a template
            // may itself inherit the font object — which is how every "declare the line once, stamp
            // it N times" addon is written. `expand_region` splices the template's attributes in,
            // but the merged element keeps the INSTANCE's own `inherits=` (the template's name), so
            // the font object at the far end of the chain was simply lost: one hop worked and two
            // did not.
            //
            // `EQL3_Tracker.xml` is the corpus instance — `EQL3_QuestWatch_FontTemplate` inherits
            // `GameFontHighlight` and each `EQL3_QuestWatchLine<i>` inherits the template — and it
            // is not a cosmetic loss. `EQL3_Options.lua:1086` reads the line's font back with
            // `GetFont()` and feeds it straight to `SetFont(t1, height, t2)`, so the dropped face
            // came back as OUR OWN faithful `Usage: <FontString>:SetFont(...)` raise, firing on our
            // own nil, and took the addon's session with it.
            // An UNRESOLVED chain passes the name through UNCHANGED rather than something
            // emptier. Handing `SetFontObject("")` an empty string is not the same request as
            // handing it a bad name — it reads as "clear the font object", which is a silent state
            // change where the old code produced a loud warn, and it cost EQL3 a NEW load-time
            // failure the first time this landed.
            let resolved = self
                .font_object_through_templates(name)
                .unwrap_or_else(|| name.to_string());
            if let Err(e) = wrapper.call_method::<()>("SetFontObject", resolved) {
                self.warn_once(
                    &format!("fontobj:{name}"),
                    format!("{dbg}: <FontString inherits=\"{name}\">: {e}"),
                );
            }
        }
        // **`font=` gates this whole block, and that is the shape of the bytes** — not a set of
        // independent overrides (`0x7710e1`-`0x771254`, wow-re
        // `system/ui/scratch/fontstring-loadxml-font-attrs.md`). Three outcomes, and only three:
        //
        //  1. `font=` names a **registered font object** → `SetFontObject` (`0x771104` lookup,
        //     `0x771114 call 0x770c60` — the same live link `inherits=` takes) and then
        //     `0x771119 jmp 0x771254`, which **skips `<FontHeight>`, `outline=`, `monochrome=` and
        //     the file load entirely**. The skipped values are not lost: the link forces all five
        //     changed bits (`0x770d0c`) and the applier `0x770800` pushes the source's
        //     height/face/outline down through the inherit mask.
        //  2. `font=` names anything else → a font **FILE**, and `<FontHeight>`/`outline=`/
        //     `monochrome=` are read as ITS companions (the miss leg, `0x77110b je 0x77111e`).
        //  3. **No `font=` at all** → `0x7710f1`/`0x7710fa je 0x771254` jump straight past, so
        //     `<FontHeight>` and `outline=` are **never parsed**. That block's only predecessor is
        //     the miss leg in (2); the attributes have no independent existence.
        //
        // (3) is the one that changes something we already draw. Real FrameXML has exactly one such
        // site — `ZoneText.xml`'s `AutoFollowStatusText`, `inherits="GameFontNormal"` with a
        // `<FontHeight val="20">` and no `font=` — so the real client draws that line at
        // GameFontNormal's **12**, and Blizzard's 20 is dead XML. Our copy carries the same dead 20
        // and honoured it until now. The corpus has 22 more of these plus 5 stray `outline=`.
        //
        // Only `spacing=`/`justifyH`/`justifyV`/`<Color>`/`<Shadow>` sit past the join and are
        // genuine post-link overrides — they are applied elsewhere (`apply_region_layout` /
        // `apply_region_visual`), which is already the right side of this line.
        match region.attr("font") {
            Some(name) if self.is_font_object(name) => {
                if let Err(e) = wrapper.call_method::<()>("SetFontObject", name.to_string()) {
                    self.warn_once(
                        &format!("fontobj:{name}"),
                        format!("{dbg}: <FontString font=\"{name}\">: {e}"),
                    );
                }
            }
            Some(path) => {
                // A font FILE, with its two companions. NOT the `SetFont` binding: XML supplies any
                // subset, while the reference's `SetFont` requires a path AND a height (raising
                // `0x87c69c` otherwise), because the real client applies XML font attributes in C++
                // and never through the Lua method. Routing the loader through the binding is
                // exactly what forced that binding to stay lenient — one name doing two jobs.
                let height = children_named(region, "FontHeight")
                    .next()
                    .and_then(abs_value);
                let outline = region.attr("outline").map(str::to_string);
                if let Err(e) = crate::script::apply_font_parts(
                    self.lua,
                    wrapper,
                    Some(path.to_string()),
                    height,
                    outline,
                ) {
                    self.report
                        .errors
                        .push(format!("{dbg}: region font attrs: {e}"));
                }
            }
            None => {}
        }
    }
}
