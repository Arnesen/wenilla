use mlua::{ObjectLike, Table};

use crate::framexml::{self, Element};

use super::{children_named, color_of, tex_coords_of, Loader};

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
                if let Some(c) = color {
                    // File + <Color> = a tint on the bar texture.
                    self.call(wrapper, "SetStatusBarColor", (c[0], c[1], c[2], c[3]), dbg);
                }
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
            for t in children_named(el, tag) {
                if let Some(file) = t.attr("file") {
                    this.call(wrapper, method, file.to_string(), dbg);
                } else if let Some(c) = children_named(t, "Color").next().map(color_of) {
                    this.call(wrapper, method, (c[0], c[1], c[2], c[3]), dbg);
                } else if t.attr("atlas").is_some() {
                    // `atlas=` (decision 0950, the Era dialect — same seam as a `<Layers>`
                    // texture's): the byte-verified 1.12 table knows only file=/<Color>, so the
                    // setter first creates the slot empty and SetAtlas below resolves it.
                    this.call(wrapper, method, String::new(), dbg);
                }
                // alphaMode / <Size> / <Anchors> / <TexCoords> apply to the region the setter just
                // created — fetch it back through the matching getter and use the region methods.
                let getter = method.replacen("Set", "Get", 1);
                if let Ok(region) = wrapper.call_method::<Table>(getter.as_str(), ()) {
                    if let Some(atlas) = t.attr("atlas") {
                        this.call_region(
                            &region,
                            "SetAtlas",
                            (atlas.to_string(), t.attr_bool("useAtlasSize")),
                            dbg,
                        );
                    }
                    if let Some(mode) = t.attr("alphaMode") {
                        this.call_region(&region, "SetBlendMode", mode.to_string(), dbg);
                    }
                    this.apply_region_layout(t, &region, self_name, dbg);
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
        for bt in children_named(el, "ButtonText") {
            // Create the label slot even with no text yet (the geometry below must land on a real
            // region; SetText is the slot's lazy constructor), then apply the element's own
            // `<Size>/<Anchors>`/justify to it — the ref anchors ButtonText all over (the quest
            // greeting rows hang theirs at TOPLEFT+20 beside the bullet; without this every
            // labelled Button centered its text over the whole face).
            self.call(
                wrapper,
                "SetText",
                bt.attr("text").unwrap_or_default().to_string(),
                dbg,
            );
            if let Ok(region) = wrapper.call_method::<Table>("GetFontString", ()) {
                self.apply_region_layout(bt, &region, self_name, dbg);
                // Publish the label under its resolved name (`<ButtonText name="$parentText">`) —
                // the ref kit addresses tab/button labels by exactly this global
                // (PanelTemplates_TabResize's `getglobal(tabName.."Text")`).
                if let Some(rname) = bt.name().map(|raw| framexml::resolve_name(raw, self_name)) {
                    if let Err(e) = self.lua().globals().set(rname.clone(), region) {
                        self.report
                            .warnings
                            .push(format!("{dbg}: ButtonText global '{rname}': {e}"));
                    }
                }
            }
        }
        if let Some(text) = el.attr("text") {
            self.call(wrapper, "SetText", text.to_string(), dbg);
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
                if let Some(name) = f.attr("inherits") {
                    self.call(wrapper, method, name.to_string(), dbg);
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
    /// A flag absent from the XML stays at its ctor default (`false`) — a **documented divergence**
    /// from the UI.xsd, whose `autoFocus` default is `true`; benilla applies the C++ ctor default
    /// (flags=0) uniformly for CreateFrame and XML, since autoFocus never focuses on show and click
    /// focus is unconditional, so the practical effect is confined to keyboard self-acquire.
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
            if el.attr_bool(attr) {
                self.call(wrapper, method, true, dbg);
            }
        }
    }

    /// `<ScrollingMessageFrame>` LoadXML extras (msgframe-runtime.md `0x787b20`): `maxLines`
    /// (int, `> 0` — destructive `SetMaxLines`), `displayDuration`/`fadeDuration` (float, `> 0` →
    /// SetTimeVisible/SetFadeDuration), and the `fade` bool (→ SetFading). A missing attr keeps the
    /// ctor default (fading on, 10s/3s, 8 lines). The `<FontString>` child renders through the
    /// generic `<Layers>` path; its resolved font is what the frame's lines bake and stack at
    /// (read at extract, `crate::script::UiScript::extract`).
    pub(super) fn apply_messageframe(&mut self, el: &Element, wrapper: &Table, dbg: &str) {
        if !el.tag.eq_ignore_ascii_case("ScrollingMessageFrame") {
            return;
        }
        if let Some(n) = el
            .attr("maxLines")
            .and_then(|v| v.trim().parse::<i64>().ok())
        {
            if n > 0 {
                self.call(wrapper, "SetMaxLines", n, dbg);
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
        for tt in children_named(el, "ThumbTexture") {
            if let Some(file) = tt.attr("file") {
                self.call(
                    wrapper,
                    "SetThumbTexture",
                    (file.to_string(), layer.clone()),
                    dbg,
                );
            } else if let Some(c) = children_named(tt, "Color").next().map(color_of) {
                self.call(wrapper, "SetThumbTexture", (c[0], c[1], c[2], c[3]), dbg);
            } else if tt.attr("atlas").is_some() {
                // `atlas=` — the Era dialect (0950), same posture as the Button state textures
                // above: create the slot empty, resolve via SetAtlas below.
                self.call(
                    wrapper,
                    "SetThumbTexture",
                    (String::new(), layer.clone()),
                    dbg,
                );
            }
            // alphaMode / <Size> / <Anchors> / <TexCoords> apply to the thumb region the setter just
            // created — fetch it back through the getter and use the region methods (same shape as a
            // Button's state textures).
            if let Ok(region) = wrapper.call_method::<Table>("GetThumbTexture", ()) {
                if let Some(atlas) = tt.attr("atlas") {
                    self.call_region(
                        &region,
                        "SetAtlas",
                        (atlas.to_string(), tt.attr_bool("useAtlasSize")),
                        dbg,
                    );
                }
                if let Some(mode) = tt.attr("alphaMode") {
                    self.call_region(&region, "SetBlendMode", mode.to_string(), dbg);
                }
                self.apply_region_layout(tt, &region, self_name, dbg);
                if let Some(tc) = tex_coords_of(tt) {
                    self.call_region(&region, "SetTexCoord", tc, dbg);
                }
            }
        }
    }
}
