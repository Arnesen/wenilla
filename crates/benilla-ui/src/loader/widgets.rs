use mlua::{ObjectLike, Table};

use crate::framexml::{self, Element};

use super::{abs_dim, abs_value, children_named, color_of, tex_coords_of, Loader};

impl Loader<'_> {
    /// `<StatusBar>` LoadXML extras (RF-28, byte-verified table `0x782ef0`): `minValue`/`maxValue`
    /// (a reversed pair is swapped — SetMinMaxValues normalizes identically), `defaultValue` →
    /// SetValue, `orientation`, and the `<BarTexture>`/`<BarColor>` children; the element's
    /// `drawLayer` names the bar texture's layer (widget default ARTWORK). The Slider's parallel
    /// `<ThumbTexture>` path lives in [`Self::apply_slider`].
    pub(super) fn apply_statusbar(&mut self, el: &Element, wrapper: &Table, dbg: &str) {
        if !el.tag.eq_ignore_ascii_case("StatusBar") {
            return;
        }
        let parse = |attr: &str| el.attr(attr).and_then(|v| v.trim().parse::<f32>().ok());
        let (min, max) = (parse("minValue"), parse("maxValue"));
        if min.is_some() || max.is_some() {
            self.call(
                wrapper,
                "SetMinMaxValues",
                (min.unwrap_or(0.0), max.unwrap_or(0.0)),
                dbg,
            );
        }
        if let Some(o) = el.attr("orientation") {
            self.call(wrapper, "SetOrientation", o.to_string(), dbg);
        }
        let layer = el.attr("drawLayer").map(str::to_string);
        for bar in children_named(el, "BarTexture") {
            let color = children_named(bar, "Color").next().map(color_of);
            if let Some(file) = bar.attr("file") {
                self.call(
                    wrapper,
                    "SetStatusBarTexture",
                    (file.to_string(), layer.clone()),
                    dbg,
                );
                // File + `<Color>` DISCARDS the colour, it does not tint — the same
                // `CSimpleTexture::LoadXML` ordering `regions.rs`'s `<Texture>` arm cites
                // (`0x76fe20`: the child loop runs first, then `file=` overwrites the same `+0xcc`).
                // `<BarColor>` below is this widget's real tint.
            } else if let Some(c) = color {
                // No file: a solid-color bar (the SetStatusBarTexture(r,g,b,a) form).
                self.call(
                    wrapper,
                    "SetStatusBarTexture",
                    (c[0], c[1], c[2], c[3]),
                    dbg,
                );
            }
        }
        for bc in children_named(el, "BarColor") {
            let c = color_of(bc);
            self.call(wrapper, "SetStatusBarColor", (c[0], c[1], c[2], c[3]), dbg);
        }
        if let Some(v) = parse("defaultValue") {
            self.call(wrapper, "SetValue", v, dbg);
        }
    }

    /// `<Button>`/`<CheckButton>` extras (RF-28 `0x7788c0`/`0x785170` — the checkbox loader runs
    /// the button one first, which this shared body mirrors): the four state textures, CheckButton's
    /// two checked textures + `checked` attr, `<ButtonText text=>`, and the `text` attribute. Each
    /// texture child takes a `file` or a `<Color>` (the same two forms as the Set*Texture methods)
    /// plus the generic region layout — `<Size>`/`<Anchors>`/`setAllPoints` — so a state texture can
    /// cover less than its button (the merchant row's icon-scoped highlight); `$parent` in a state
    /// texture's anchors resolves against this button's own name, like a `<Layers>` region's.
    /// Not modeled (stated): per-state fonts/colors, `<PushedTextOffset>`.
    pub(super) fn apply_button(
        &mut self,
        el: &Element,
        wrapper: &Table,
        self_name: &str,
        dbg: &str,
    ) {
        let is_check = el.tag.eq_ignore_ascii_case("CheckButton");
        if !is_check && !el.tag.eq_ignore_ascii_case("Button") {
            return;
        }
        let tex = |tag: &str, method: &str, this: &mut Self| {
            for raw in children_named(el, tag) {
                // A state texture may `inherits=` a virtual `<Texture>` — `<NormalTexture
                // inherits="UIPanelButtonUpTexture"/>` is how the reference's whole shared button
                // kit carries its art, and it is the ONLY form those templates use. Expanding here
                // is the same call a `<Layers>` region gets (`expand_region`, which passes a
                // non-template `inherits=` through untouched), and without it the reads below found
                // no `file=`, no `<TexCoords>` and no `<Size>`: a button with no art and NO ERROR.
                let expanded = this.expand_region(raw);
                let t = &expanded;
                if let Some(file) = t.attr("file") {
                    this.call(wrapper, method, file.to_string(), dbg);
                } else if let Some(c) = children_named(t, "Color").next().map(color_of) {
                    this.call(wrapper, method, (c[0], c[1], c[2], c[3]), dbg);
                } else if t.name().is_some() {
                    // A NAMED state texture with no art of its own — `<NormalTexture
                    // name="$parentIcon">` on the ref's own MacroFrameButtonTemplate, whose art
                    // arrives later through `SetTexture`. The setter is what MATERIALIZES the
                    // slot's region, so without this call the getter below finds nothing and the
                    // name never publishes: `getglobal("MacroButton1Icon")` would be nil and the
                    // window's whole Update would die on it. `""` is the live API's own blank form
                    // (`set_slot_texture`'s empty-string arm), so this creates without painting.
                    this.call(wrapper, method, String::new(), dbg);
                }
                // alphaMode / <Size> / <Anchors> / <TexCoords> apply to the region the setter just
                // created — fetch it back through the matching getter and use the region methods.
                let getter = method.replacen("Set", "Get", 1);
                if let Ok(region) = wrapper.call_method::<Table>(getter.as_str(), ()) {
                    if let Some(mode) = t.attr("alphaMode") {
                        this.call_region(&region, "SetBlendMode", mode.to_string(), dbg);
                    }
                    // The setter above MATERIALIZED the region with the runtime path's implicit
                    // SetAllPoints (decision 1310) — but this is the XML path, where the real
                    // engine routes state textures through the region adder (`0x778903`…: authored
                    // `<Anchors>` first, the implicit step after). Reproduce that order: clear,
                    // apply the authored layout, then re-run the conditional step — a same-point
                    // SetPoint over a live implicit corner would otherwise leave the OTHER corner
                    // standing (the slot law) and weld an anchored state texture to the button.
                    this.call_region(&region, "ClearAllPoints", (), dbg);
                    this.apply_region_layout(t, &region, self_name, dbg);
                    if let Err(e) = crate::script::implicit_creation_anchor_lua(this.lua, &region) {
                        this.report
                            .errors
                            .push(format!("{dbg}: implicit anchor: {e}"));
                    }
                    if let Some(tc) = tex_coords_of(t) {
                        this.call_region(&region, "SetTexCoord", tc, dbg);
                    }
                    // Publish a NAMED state texture (`<HighlightTexture
                    // name="$parentHighlightTexture">`) — the ref kit addresses these by global
                    // (the tab template's own OnShow does `getglobal(name.."HighlightTexture")`),
                    // exactly like a named `<Layers>` region or the ButtonText.
                    if let Some(rname) = t.name().map(|raw| framexml::resolve_name(raw, self_name))
                    {
                        if let Err(e) = this.lua().globals().set(rname.clone(), region) {
                            this.report
                                .warnings
                                .push(format!("{dbg}: state-texture global '{rname}': {e}"));
                        }
                    }
                }
            }
        };
        // The LABEL is published before the state textures, and the order is load-bearing: a
        // state texture may anchor to `$parentText` (the reference's own sort-header template
        // hangs its arrow off the label's RIGHT), and this loader resolves `relativeTo` eagerly by
        // name at SetPoint time. Applied the other way round, every such anchor missed and fell
        // back to the owner — 14 of them in one window, each a sort arrow sitting in the wrong
        // place. The real client is order-free here because it resolves anchors later; we are not,
        // so we order it ourselves rather than making each author work around it.
        for bt in children_named(el, "ButtonText") {
            // Create the label slot even with no text yet (the geometry below must land on a real
            // region; SetText is the slot's lazy constructor), then apply the element's own
            // `<Size>/<Anchors>`/justify to it — the ref anchors ButtonText all over (the quest
            // greeting rows hang theirs at TOPLEFT+20 beside the bullet; without this every
            // labelled Button centered its text over the whole face).
            // `<ButtonText text=>` is a FontString's own attribute — the same global-string lookup
            // rf28 l.115 gives every `<FontString text=>`. See `Loader::resolve_text`.
            let label = match bt.attr("text") {
                Some(raw) => self.resolve_text(raw, dbg),
                None => String::new(),
            };
            // A NAMED `<ButtonText name="$parentText">` is created as a named region and bound,
            // rather than left to `SetText`'s lazy constructor and aliased afterwards.
            //
            // Publishing a Lua global is not the same thing as naming the region, and the gap was
            // invisible until something anchored to it: `relativeTo="$parentText"` resolves through
            // the engine's own named-region lookup, which a global alias never reaches, so every
            // such anchor missed and fell back to the owner. `GetFontString():GetName()` also
            // answered nil, which is wrong for any addon that asks.
            let bt_name = bt.name().map(|raw| framexml::resolve_name(raw, self_name));
            if let Some(rname) = bt_name.clone() {
                match wrapper
                    .call_method::<Table>("CreateFontString", (rname, Option::<String>::None))
                {
                    Ok(fs) => self.call(wrapper, "SetFontString", fs, dbg),
                    Err(e) => self
                        .report
                        .warnings
                        .push(format!("{dbg}: ButtonText CreateFontString: {e}")),
                }
            }
            self.call(wrapper, "SetText", label, dbg);
            if let Ok(region) = wrapper.call_method::<Table>("GetFontString", ()) {
                // Same clear→layout→implicit order as the state textures above (decision 1310):
                // SetText materialized the label with the runtime path's implicit CENTER anchor;
                // the XML path re-derives it AFTER the element's own `<Anchors>`/justify apply
                // (so a `<ButtonText justifyH="LEFT">` with no anchors seats LEFT, as the real
                // FontString post-step `0x771480` would).
                self.call_region(&region, "ClearAllPoints", (), dbg);
                self.apply_region_layout(bt, &region, self_name, dbg);
                if let Err(e) = crate::script::implicit_creation_anchor_lua(self.lua, &region) {
                    self.report
                        .errors
                        .push(format!("{dbg}: implicit anchor: {e}"));
                }
                // Publish the label under its resolved name (`<ButtonText name="$parentText">`) —
                // the ref kit addresses tab/button labels by exactly this global
                // (PanelTemplates_TabResize's `getglobal(tabName.."Text")`).
                if let Some(rname) = bt_name {
                    if let Err(e) = self.lua().globals().set(rname.clone(), region) {
                        self.report
                            .warnings
                            .push(format!("{dbg}: ButtonText global '{rname}': {e}"));
                    }
                }
            }
        }
        tex("NormalTexture", "SetNormalTexture", self);
        tex("PushedTexture", "SetPushedTexture", self);
        tex("DisabledTexture", "SetDisabledTexture", self);
        tex("HighlightTexture", "SetHighlightTexture", self);
        if is_check {
            tex("CheckedTexture", "SetCheckedTexture", self);
            tex("DisabledCheckedTexture", "SetDisabledCheckedTexture", self);
            if let Some(c) = el.attr("checked") {
                let checked = c.eq_ignore_ascii_case("true") || c == "1";
                self.call(wrapper, "SetChecked", checked, dbg);
            }
        }
        if let Some(text) = el.attr("text") {
            // `<Button text=>` → the ButtonText fontstring, global-string resolved (rf28 l.36,
            // `0x703bf0`). This is what makes the reference's `text="DELETE"` read "Delete".
            let text = self.resolve_text(text, dbg);
            self.call(wrapper, "SetText", text, dbg);
        }
        // The per-state label fonts (`<NormalFont inherits=>` etc. — UIPanelButtonTemplate's
        // gold/white/gray label trio) → the 1.12 setter trio; every occurrence applies in
        // document order (same last-wins rule as `apply_size`).
        for (child, method) in [
            ("NormalFont", "SetTextFontObject"),
            ("HighlightFont", "SetHighlightFontObject"),
            ("DisabledFont", "SetDisabledFontObject"),
        ] {
            for f in children_named(el, child) {
                // These three are `<Font>`-TYPED elements — `CSimpleButton::LoadXML 0x7788c0`
                // routes them at `0x778bf4` into the SAME `0x783c30` a top-level `<Font>` uses — so
                // they take `inherits=` and `font=` alike, and `font=` wins: both land in one slot
                // and `0x770c60` unlinks the previous parent (wow-re
                // `fontstring-loadxml-font-attrs.md` C5; `font=`'s registry-first path is
                // `0x783d15` → `0x783d22 call 0x770c60` → `0x783d27 jmp 0x783ee0`).
                //
                // Reading only `inherits=` here left every corpus button that writes `font=` on its
                // own default font — Bagnon writes it at seven sites, which is why its character
                // list's names and its "Show Bags" label were not the Large/normal faces they ask
                // for. Real FrameXML always writes `inherits=`, so nothing we ship noticed.
                //
                // `style=` is deliberately NOT read: it does not exist in 1.12.1. An isolated-token
                // scan for it returns zero against nine controls that each return one — it is a
                // later-client idiom, and the reference writes `<NormalFont inherits="GameFontNormal"/>`
                // (`UIPanelTemplates.xml:20-22`).
                for attr in ["inherits", "font"] {
                    if let Some(name) = f.attr(attr) {
                        self.call(wrapper, method, name.to_string(), dbg);
                    }
                }
                // An element-level justify (`<NormalFont inherits="QuestFont" justifyH="LEFT"/>`)
                // lands on the label region itself — v1: one region-level set (the ref declares
                // the same justify on all three state elements; a per-state justify would need
                // per-state font-object clones).
                if let Some(j) = f.attr("justifyH") {
                    // Ensure the slot exists WITHOUT clobbering a label the `<ButtonText>`/`text=`
                    // handling above already set: only a missing slot gets the creating SetText("").
                    if wrapper.call_method::<Table>("GetFontString", ()).is_err() {
                        self.call(wrapper, "SetText", String::new(), dbg);
                    }
                    if let Ok(region) = wrapper.call_method::<Table>("GetFontString", ()) {
                        self.call_region(&region, "SetJustifyH", j.to_string(), dbg);
                    }
                }
            }
        }
    }

    /// `<EditBox>` extras (RF-0082 §"THE STRUCTURAL KEY"/§3): the `letters` cap → SetMaxLetters,
    /// `historyLines` → SetHistoryLines (the submitted-line recall ring), and the config flags
    /// `autoFocus`/`numeric`/`password`/`multiLine`/`ignoreArrows` → their setters.
    /// A flag absent from the XML stays at its ctor default, and the ctor's own value is
    /// `flags = 1` — **`autoFocus` defaults ON**, every other flag off (`0x779a29 mov eax,1` /
    /// `0x779a2e mov [esi+0x318],eax`; LoadXML's `autoFocus` leg writes nothing for an absent or
    /// empty attribute, `0x77a0b3`/`0x77a0b8`). So the UI.xsd's `true` default is the client's too,
    /// and the divergence documented here — benilla applying `flags = 0` uniformly — is retired
    /// (decision 1686). Its stated justification, "autoFocus never focuses on show", was wow-re's
    /// own `call`-only-census error, corrected 2026-08-29.
    ///
    /// **The flags are read presence-aware.** This loop used to call the setter only when an
    /// attribute parsed as `true`, so `autoFocus="false"` was a no-op — harmless while the default
    /// was off, and precisely backwards once it is on: the ten boxes in the shipped chain that opt
    /// OUT (MailFrame ×3, MoneyInputFrame ×3, FriendsFrame ×3, AddonList ×1) are the only places
    /// the attribute appears at all.
    /// `<TextInsets>` maps to `SetTextInsets`; `blinkSpeed` → SetBlinkSpeed (the caret
    /// half-period, `E+0x370`). A declared `<FontString>` is ASSIGNED as the box's text region by
    /// the special-fontstring pass (`adopt_text_region` — the engine's LoadXML slot, never a
    /// search).
    pub(super) fn apply_editbox(&mut self, el: &Element, wrapper: &Table, dbg: &str) {
        if !el.tag.eq_ignore_ascii_case("EditBox") {
            return;
        }
        if let Some(n) = el
            .attr("letters")
            .and_then(|v| v.trim().parse::<i64>().ok())
        {
            self.call(wrapper, "SetMaxLetters", n, dbg);
        }
        if let Some(n) = el
            .attr("historyLines")
            .and_then(|v| v.trim().parse::<i64>().ok())
        {
            self.call(wrapper, "SetHistoryLines", n, dbg);
        }
        if let Some(s) = el
            .attr("blinkSpeed")
            .and_then(|v| v.trim().parse::<f32>().ok())
        {
            self.call(wrapper, "SetBlinkSpeed", s, dbg);
        }
        // <TextInsets><AbsInset left= right= top= bottom=/></TextInsets> → SetTextInsets(l,r,t,b)
        // (the ref chat box drives these from ChatEdit_UpdateHeader at runtime; the XML form seeds
        // the static case).
        if let Some(ins) = children_named(el, "TextInsets").next() {
            let src = children_named(ins, "AbsInset").next().unwrap_or(ins);
            let get = |k: &str| {
                src.attr(k)
                    .and_then(|v| v.trim().parse::<f32>().ok())
                    .unwrap_or(0.0)
            };
            let (l, r, t, b) = (get("left"), get("right"), get("top"), get("bottom"));
            self.call(wrapper, "SetTextInsets", (l, r, t, b), dbg);
        }
        for (attr, method) in [
            ("autoFocus", "SetAutoFocus"),
            ("numeric", "SetNumeric"),
            ("password", "SetPassword"),
            ("multiLine", "SetMultiLine"),
            ("ignoreArrows", "SetIgnoreArrows"),
        ] {
            if let Some(on) = el.attr_bool_opt(attr) {
                self.call(wrapper, method, on, dbg);
            }
        }
    }

    /// The two message-frame classes' LoadXML extras — **`0x787b20` and `0x785910`, two separate
    /// tables** (msgframe-runtime.md's XML-attribute section), which is why the shared attrs are
    /// applied for either tag and the two divergent ones are gated on the tag that has them:
    ///
    /// - both: `displayDuration`/`fadeDuration` (float, applied **iff `> 0`** — the client's own
    ///   gate) → SetTimeVisible/SetFadeDuration, and the `fade` bool → SetFading.
    /// - `<ScrollingMessageFrame>` only: `maxLines` (int, `> 0`, destructive `SetMaxLines`).
    /// - `<MessageFrame>` only: `insertMode` → SetInsertMode. The scrolling class has no such
    ///   attribute and no such binding.
    ///
    /// A missing attr keeps the ctor default (fading on, 10s/3s; 8 lines / insertMode BOTTOM). The
    /// `<FontString>` child renders through the generic `<Layers>` path; its resolved font **and its
    /// `justifyH`** are what the frame's lines bake, stack and align at (read at extract,
    /// `crate::script::UiScript::extract`) — that child is how `UIErrorsFrame.xml` centres its
    /// toasts and how a chat frame keeps its lines flush left.
    pub(super) fn apply_messageframe(&mut self, el: &Element, wrapper: &Table, dbg: &str) {
        let scrolling = el.tag.eq_ignore_ascii_case("ScrollingMessageFrame");
        let plain = el.tag.eq_ignore_ascii_case("MessageFrame");
        if !scrolling && !plain {
            return;
        }
        if scrolling {
            if let Some(n) = el
                .attr("maxLines")
                .and_then(|v| v.trim().parse::<i64>().ok())
            {
                if n > 0 {
                    self.call(wrapper, "SetMaxLines", n, dbg);
                }
            }
        }
        if plain {
            if let Some(mode) = el.attr("insertMode") {
                self.call(wrapper, "SetInsertMode", mode.trim().to_string(), dbg);
            }
        }
        if let Some(s) = el
            .attr("displayDuration")
            .and_then(|v| v.trim().parse::<f32>().ok())
        {
            if s > 0.0 {
                self.call(wrapper, "SetTimeVisible", s, dbg);
            }
        }
        if let Some(s) = el
            .attr("fadeDuration")
            .and_then(|v| v.trim().parse::<f32>().ok())
        {
            if s > 0.0 {
                self.call(wrapper, "SetFadeDuration", s, dbg);
            }
        }
        if let Some(fade) = el.attr("fade") {
            let on = fade.eq_ignore_ascii_case("true") || fade == "1";
            self.call(wrapper, "SetFading", on, dbg);
        }
    }

    /// `<Slider>` LoadXML extras (RF-28, byte-verified table `0x789580`): `orientation` →
    /// SetOrientation (the ctor default is VERTICAL — decision 0250 — so an omitted attr leaves a
    /// vertical scrollbar); `minValue`/`maxValue` → SetMinMaxValues, with `valueStep` → SetValueStep
    /// and `defaultValue` → SetValue — the latter three **gated on BOTH minValue AND maxValue
    /// present** (RF-28). Unlike StatusBar, a reversed `min > max` pair is NOT swapped. The
    /// `<ThumbTexture>` child → SetThumbTexture (file or `<Color>`) plus the generic region layout
    /// (`<Size>`/`<Anchors>`/`<TexCoords>`/alphaMode); the element's `drawLayer` names the thumb's
    /// layer (widget default OVERLAY). The scrollbar template declares only the thumb + orientation;
    /// its range/value are set at runtime by `FauxScrollFrame_Update`.
    pub(super) fn apply_slider(
        &mut self,
        el: &Element,
        wrapper: &Table,
        self_name: &str,
        dbg: &str,
    ) {
        if !el.tag.eq_ignore_ascii_case("Slider") {
            return;
        }
        if let Some(o) = el.attr("orientation") {
            self.call(wrapper, "SetOrientation", o.to_string(), dbg);
        }
        let parse = |attr: &str| el.attr(attr).and_then(|v| v.trim().parse::<f32>().ok());
        if let (Some(min), Some(max)) = (parse("minValue"), parse("maxValue")) {
            self.call(wrapper, "SetMinMaxValues", (min, max), dbg);
            if let Some(step) = parse("valueStep") {
                self.call(wrapper, "SetValueStep", step, dbg);
            }
            if let Some(v) = parse("defaultValue") {
                self.call(wrapper, "SetValue", v, dbg);
            }
        }
        let layer = el.attr("drawLayer").map(str::to_string);
        for raw in children_named(el, "ThumbTexture") {
            // Same two rules as a Button's state textures, for the same reasons: `inherits=` on a
            // virtual `<Texture>` is expanded here (see `apply_button`), and a NAMED thumb is
            // published as a global — `UIPanelScrollBarTemplate`'s own
            // `ScrollFrame_OnScrollRangeChanged` reaches it with
            // `getglobal(bar:GetName().."ThumbTexture")`.
            let expanded = self.expand_region(raw);
            let tt = &expanded;
            if let Some(file) = tt.attr("file") {
                self.call(
                    wrapper,
                    "SetThumbTexture",
                    (file.to_string(), layer.clone()),
                    dbg,
                );
            } else if let Some(c) = children_named(tt, "Color").next().map(color_of) {
                self.call(wrapper, "SetThumbTexture", (c[0], c[1], c[2], c[3]), dbg);
            }
            // alphaMode / <Size> / <Anchors> / <TexCoords> apply to the thumb region the setter just
            // created — fetch it back through the getter and use the region methods (same shape as a
            // Button's state textures).
            if let Ok(region) = wrapper.call_method::<Table>("GetThumbTexture", ()) {
                if let Some(mode) = tt.attr("alphaMode") {
                    self.call_region(&region, "SetBlendMode", mode.to_string(), dbg);
                }
                self.apply_region_layout(tt, &region, self_name, dbg);
                if let Some(tc) = tex_coords_of(tt) {
                    self.call_region(&region, "SetTexCoord", tc, dbg);
                }
                if let Some(rname) = tt.name().map(|raw| framexml::resolve_name(raw, self_name)) {
                    crate::script::region::publish_region_name(self.lua(), &rname, &region);
                    if let Err(e) = self.lua().globals().set(rname.clone(), region) {
                        self.report
                            .warnings
                            .push(format!("{dbg}: thumb-texture global '{rname}': {e}"));
                    }
                }
            }
        }
    }

    /// `<ColorSelect>`'s four texture sub-elements (RF-28 loader hooks `0x78b580`, `0x78b850`,
    /// `0x78b8a0`, `0x78ba90`) — the hue wheel, the brightness strip, and a marker for each.
    /// Structurally `apply_slider`'s `<ThumbTexture>` loop, four times over.
    ///
    /// The one thing that is NOT like a thumb: **two of the four carry no `file=`, and that is not
    /// an omission.** The client generates the wheel and the strip; there is no BLP in the chain
    /// that is a colour wheel. So the setter is still called — with nothing — because the *region*
    /// must exist regardless: it is what layout resolves, what the press handler hit-tests, and
    /// what the app renderer paints into. An element that skipped the setter would leave the picker
    /// with no wheel to click.
    pub(super) fn apply_colorselect(
        &mut self,
        el: &Element,
        wrapper: &Table,
        self_name: &str,
        dbg: &str,
    ) {
        if !el.tag.eq_ignore_ascii_case("ColorSelect") {
            return;
        }
        let layer = el.attr("drawLayer").map(str::to_string);
        for (tag, setter, getter) in [
            (
                "ColorWheelTexture",
                "SetColorWheelTexture",
                "GetColorWheelTexture",
            ),
            (
                "ColorWheelThumbTexture",
                "SetColorWheelThumbTexture",
                "GetColorWheelThumbTexture",
            ),
            (
                "ColorValueTexture",
                "SetColorValueTexture",
                "GetColorValueTexture",
            ),
            (
                "ColorValueThumbTexture",
                "SetColorValueThumbTexture",
                "GetColorValueThumbTexture",
            ),
        ] {
            for raw in children_named(el, tag) {
                let expanded = self.expand_region(raw);
                let tt = &expanded;
                if let Some(file) = tt.attr("file") {
                    self.call(wrapper, setter, (file.to_string(), layer.clone()), dbg);
                } else if let Some(c) = children_named(tt, "Color").next().map(color_of) {
                    self.call(wrapper, setter, (c[0], c[1], c[2], c[3]), dbg);
                } else {
                    // The file-less form — create the region and leave it for the renderer.
                    self.call(wrapper, setter, (), dbg);
                }
                if let Ok(region) = wrapper.call_method::<Table>(getter, ()) {
                    if let Some(mode) = tt.attr("alphaMode") {
                        self.call_region(&region, "SetBlendMode", mode.to_string(), dbg);
                    }
                    self.apply_region_layout(tt, &region, self_name, dbg);
                    if let Some(tc) = tex_coords_of(tt) {
                        self.call_region(&region, "SetTexCoord", tc, dbg);
                    }
                    if let Some(rname) = tt.name().map(|raw| framexml::resolve_name(raw, self_name))
                    {
                        // Into the region-name registry as well as `_G`: `<ColorValueTexture>`
                        // anchors to `ColorPickerWheel` BY NAME, and a name that only reaches the
                        // globals resolves to nothing there.
                        crate::script::region::publish_region_name(self.lua(), &rname, &region);
                        if let Err(e) = self.lua().globals().set(rname.clone(), region) {
                            self.report
                                .warnings
                                .push(format!("{dbg}: {tag} global '{rname}': {e}"));
                        }
                    }
                }
            }
        }
    }

    /// `<SimpleHTML>` LoadXML extras — `CSimpleHTML::LoadXML 0x78a130`, whose whole job past the
    /// base `CSimpleFrame::LoadXML 0x769820` is to fill the four **element fonts** and the
    /// hyperlink format (wow-re `simplehtml-markup-engine.md` §5.5):
    ///
    /// - attribute **`font="NAME"`** (`0x78a152`) — looked up as a font object and `SetFontObject`ed
    ///   onto **all four** elements at once (the `edi = 4` loop at `0x78a17a`), with
    ///   `"Couldn't find font object named %s"` on a miss.
    /// - child **`<FontString>`** → `elementFont[0]` (`P`), **`<FontStringHeader1|2|3>`** →
    ///   `[1]`/`[2]`/`[3]` (`0x78a1fe`…`0x78a26e`), each through `CSimpleFont::LoadXML 0x783c30`.
    /// - attribute **`hyperlinkFormat`** (`0x87a87c` → `0x78a540`) and attribute **`file`**
    ///   (`0x8710b8`), the latter a localized-string lookup fed straight to `SetText`.
    ///
    /// **These `<FontString>` children are font DECLARATIONS, not regions**, which is why
    /// [`Self::apply_special_fontstrings`] skips a `<SimpleHTML>`: creating a real FontString for
    /// one would put an extra, unanchored string on the frame and leave the element font empty.
    ///
    /// Stock `ItemTextFrame.xml` declares exactly one `<FontString inherits="ItemTextFontNormal"/>`,
    /// which is the whole reason an `<H1>` in a `page_text` body renders at the `<P>` size: nothing
    /// declares a header font, so `0x78ae30`'s empty-path test sends every element back to `P`'s.
    pub(super) fn apply_simplehtml(&mut self, el: &Element, wrapper: &Table, dbg: &str) {
        if !el.tag.eq_ignore_ascii_case("SimpleHTML") {
            return;
        }
        if let Some(fmt) = el.attr("hyperlinkFormat") {
            self.call(wrapper, "SetHyperlinkFormat", fmt.to_string(), dbg);
        }
        // `font=` on the widget itself paints all four elements; a per-element child below can
        // still override any of them, exactly as the reference's ordering allows.
        if let Some(name) = el.attr("font").filter(|n| !n.is_empty()) {
            if self.is_font_object(name) {
                for elem in ["P", "H1", "H2", "H3"] {
                    self.call(wrapper, "SetFontObject", (elem, name.to_string()), dbg);
                }
            } else {
                self.warn_once(
                    &format!("shtmlfont:{name}"),
                    format!("{dbg}: <SimpleHTML font=\"{name}\">: couldn't find font object"),
                );
            }
        }
        for child in &el.children {
            let Some(elem) = crate::script::simplehtml_element_of_xml_tag(&child.tag) else {
                continue; // <Size>/<Anchors>/<Scripts>/<Layers>/... have their own passes
            };
            let child = &self.expand_region(child);
            self.apply_element_font(child, wrapper, elem, dbg);
        }
        if let Some(raw) = el.attr("file") {
            let text = self.resolve_text(raw, dbg);
            self.call(wrapper, "SetText", text, dbg);
        }
    }

    /// One `<FontString>`/`<FontStringHeaderN>` child of a `<SimpleHTML>` → one element font.
    ///
    /// The `inherits=` → `font=` gate is the same three-outcome law a `<Layers>` `<FontString>`
    /// takes ([`Self::apply_fontstring_font`], `0x7710e1`-`0x771254`): a `font=` naming a registered
    /// object is a `SetFontObject` and **skips** `<FontHeight>`/`outline=` entirely; a `font=`
    /// naming anything else is a file with those two as its companions; no `font=` at all means
    /// neither is ever parsed. Only `<Color>`/`<Shadow>`/`justifyH`/`justifyV`/`spacing` sit past
    /// that join and are genuine post-link overrides.
    fn apply_element_font(&mut self, el: &Element, wrapper: &Table, elem: usize, dbg: &str) {
        let name = ["P", "H1", "H2", "H3"][elem];
        if let Some(inherits) = el.attr("inherits").filter(|n| !n.is_empty()) {
            let resolved = self
                .font_object_through_templates(inherits)
                .unwrap_or_else(|| inherits.to_string());
            self.call(wrapper, "SetFontObject", (name, resolved), dbg);
        }
        match el.attr("font") {
            Some(f) if self.is_font_object(f) => {
                self.call(wrapper, "SetFontObject", (name, f.to_string()), dbg);
            }
            Some(path) => {
                let height = children_named(el, "FontHeight").last().and_then(abs_value);
                let outline = el.attr("outline").map(str::to_string);
                if let Err(e) = crate::script::apply_simplehtml_font_parts(
                    self.lua,
                    wrapper,
                    elem,
                    Some(path.to_string()),
                    height,
                    outline,
                ) {
                    self.report
                        .errors
                        .push(format!("{dbg}: <SimpleHTML> {name} font attrs: {e}"));
                }
            }
            None => {}
        }
        if let Some(c) = children_named(el, "Color").last().map(color_of) {
            self.call(wrapper, "SetTextColor", (name, c[0], c[1], c[2], c[3]), dbg);
        }
        if let Some(sh) = children_named(el, "Shadow").last() {
            if let Some(c) = children_named(sh, "Color").next().map(color_of) {
                self.call(
                    wrapper,
                    "SetShadowColor",
                    (name, c[0], c[1], c[2], c[3]),
                    dbg,
                );
            }
            if let Some((x, y)) = children_named(sh, "Offset").next().map(abs_dim) {
                self.call(
                    wrapper,
                    "SetShadowOffset",
                    (name, x.unwrap_or(0.0), y.unwrap_or(0.0)),
                    dbg,
                );
            }
        }
        if let Some(j) = el.attr("justifyH") {
            self.call(wrapper, "SetJustifyH", (name, j.to_string()), dbg);
        }
        if let Some(j) = el.attr("justifyV") {
            self.call(wrapper, "SetJustifyV", (name, j.to_string()), dbg);
        }
        if let Some(sp) = el
            .attr("spacing")
            .and_then(|v| v.trim().parse::<f32>().ok())
        {
            self.call(wrapper, "SetSpacing", (name, sp), dbg);
        }
    }
}
