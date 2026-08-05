//! The FrameXML **loader** (decision 0068): the join between the document layer ([`crate::framexml`])
//! and the runtime ([`crate::script`]). It walks a parsed FrameXML document and *materializes* it —
//! turning `<Include>`/`<Script>` sequencing, template instances, `<Layers>` regions, nested
//! `<Frames>`, and `<Scripts>` handlers into live frames in a running [`UiScript`] — by driving the
//! FrameScript object model exactly as an addon does (the `CreateFrame` global + the widget/region
//! method surface installed in [`crate::script`]), never by reaching into the arena directly.
//!
//! ## Ground truth (wow-5875-re, closed VERIFIED ORCHESTRATION region `0x6edc00–0x6f3000`)
//!
//! - `system/ui/scratch/rf24-framexml-loader.md` — the element→op map this module transcribes: the
//!   top-level `<Include>`/`<Script>`/`<Font>`/frame routing (`0x6ede10`), `LoadXML` attribute
//!   handling (`hidden`/`toplevel`/`movable`/`frameStrata`/`frameLevel`/`alpha`/`enableMouse`,
//!   `0x769820`), `<Size>` + `<Anchors>` → `SetWidth`/`SetHeight`/`SetPoint` (`0x767800`), and
//!   `<Layers>`/`<Layer>`/`<Texture>`/`<FontString>` (`0x769d70`), `<Scripts>` → `SetScript`
//!   (`0x769ef0`).
//! - `system/ui/scratch/rf26-nested-frames.md` — `LoadChildFrames 0x76a060`: nested `<Frames>` is a
//!   post-load pass that gives **bottom-up `OnLoad` ordering** (a frame's children are fully built,
//!   and their `OnLoad`s fired, *before* the parent's `OnLoad` runs).
//! - `system/ui/scratch/rf27-parent-name-token.md` — `SetName 0x76c650` / the `$parent` token, applied
//!   here (via [`crate::framexml::resolve_name`]) to frame/region names and to anchor `relativeTo`.
//!
//! ## The engine-free seam
//!
//! `<Include>`/`<Script file=>` need file bytes, which this crate must not read itself (no Bevy, no
//! IO — decision 0068 §1). The app supplies a `files` closure that resolves a FrameXML path to its
//! text (from the MPQ/addon dir); the loader stays IO-free. A path with no provider hit is a warning,
//! not an error — the load continues.
//!
//! ## MAXCSTACK discipline (decision 0068, probe A)
//!
//! Like the host it drives, the loader holds **no** breadth-wise accumulation of Lua handles: a
//! frame's wrapper `Table` and its (optional) `OnLoad` `Function` live only on the Rust stack across
//! *that frame's own* subtree build (depth-bounded), and region wrappers are dropped as soon as their
//! visuals are applied. Everything durable lives Lua-side in the host's registry.

use std::collections::{HashMap, HashSet};

use mlua::{Function, Table};

use crate::framexml::{self, Element, ParsedDocument, ScriptRef, TopLevel};
use crate::script::{FontObject, JustifyH, JustifyV, Outline, UiScript};

mod backdrop;
mod geometry;
mod regions;
mod scripts;
mod widgets;

/// The outcome of a [`load`]: what went wrong tolerably (`warnings`), what went wrong that dropped
/// something (`errors`), and how much got built (`frames`). Never a panic and never an abort — a bad
/// handler body, an unknown frame type, a missing include, or an unsupported attribute is an entry
/// here and the load continues (decision 0068: the client logs-and-continues; so do we).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadReport {
    /// Tolerable issues: a missing include/script provider, an unsupported-in-v1 attribute or script
    /// handler, an unregistered `inherits=` font object, and the parse/expand warnings folded in from
    /// the document layer. None of these dropped a frame.
    pub warnings: Vec<String>,
    /// Things that dropped a frame or a handler: an unknown frame type, a handler that failed to
    /// compile, a method call that errored, a malformed included document.
    pub errors: Vec<String>,
    /// How many frame instances were successfully created (`CreateFrame` returned a wrapper) — the
    /// coverage number the real-file smoke test reports.
    pub frames: usize,
}

/// Materialize a parsed FrameXML document into live frames in `script`.
///
/// Walks `doc.items` in document order (rf24-framexml-loader.md, top level): `<Include>` resolves via
/// `files` and recurses; `<Script file=>`/inline `<Script>` run through the host **in order** (this is
/// how XML-referenced FrameXML Lua loads); `<Font>` resolves + registers a named font object
/// ([`Loader::do_font`]); a `virtual` template is registered for later `inherits=`; a non-virtual
/// instance is expanded ([`crate::framexml::expand`]) and materialized.
///
/// `files` is the engine-free seam (see module docs): it resolves a FrameXML/Lua path to its text.
/// Returning `None` yields a warning, not an error.
pub fn load(
    script: &UiScript,
    doc: &ParsedDocument,
    files: &dyn Fn(&str) -> Option<String>,
) -> LoadReport {
    let mut loader = Loader {
        script,
        files,
        report: LoadReport::default(),
        warned: HashSet::new(),
    };
    // Fold the document layer's own parse warnings in (decision 0068: reuse framexml's warnings).
    loader.report.warnings.extend(doc.warnings.iter().cloned());
    loader.load_doc(doc);
    loader.report
}

struct Loader<'a> {
    pub(super) script: &'a UiScript,
    pub(super) files: &'a dyn Fn(&str) -> Option<String>,
    // The template + font-element registries live ON the UiScript (`framexml_templates` /
    // `framexml_fonts`), persisted ACROSS `load` calls — the client's template table is global
    // (`0x6ee500`), so a file may `inherits=` a template an earlier file registered (the real
    // MerchantFrame.xml ← CharacterFrameTemplates.xml). "Register before use" in load order,
    // including through `<Include>`s. Fonts are a separate namespace (a font inherits a font,
    // never a frame template); each stored font element is already inherits-resolved so a later
    // font inheriting it reads fully-flattened values (rf24).
    pub(super) report: LoadReport,
    /// Warn-once keys (so a document with 200 `OnClick` handlers doesn't emit 200 identical warnings).
    pub(super) warned: HashSet<String>,
}

impl Loader<'_> {
    pub(super) fn lua(&self) -> &mlua::Lua {
        self.script.lua()
    }

    pub(super) fn warn_once(&mut self, key: &str, msg: impl Into<String>) {
        if self.warned.insert(key.to_string()) {
            self.report.warnings.push(msg.into());
        }
    }

    /// A `text=` attribute is a **GLOBAL-STRING LOOKUP, not a literal** — the single most load-bearing
    /// thing about the attribute, and the one this loader used to get wrong.
    ///
    /// wow-re `system/ui/scratch/rf28-typed-widget-loadxml.md`: `<Button text=>` (l.36) and
    /// `<FontString text=>` (l.115) BOTH resolve through `FrameScript_GetText 0x703bf0`, which
    /// `scratch/inventory-change-failure-display.md` l.119 carves VERIFIED — it resolves the value as
    /// a Lua global and, **when that global is not a string, returns a pre-seeded EMPTY string**
    /// (`0x882748`), never the key name. That is why the reference's `text="LOGOUT"` renders "Logout",
    /// and why `GlobalStrings.lua` runs before any XML (`ui_script::load_global_strings`).
    ///
    /// **One deliberate divergence: a miss falls back to the LITERAL** rather than the reference's
    /// empty string. benilla authors its own FrameXML (0068) and writes plain English in it —
    /// `text="Send Mail"`, `text="No results found."` — which the reference's rule would blank. The
    /// fallback is a strict superset for transcriptions (every real key resolves identically) and it
    /// fails LOUDER than the reference: a **key-shaped** value that misses keeps its key on screen
    /// *and* warns here — exactly the signal that was missing when the macro window shipped with
    /// "CREATE_MACROS" across its title bar (0983 → 0991).
    pub(super) fn resolve_text(&mut self, raw: &str, dbg: &str) -> String {
        if let Ok(s) = self.lua().globals().get::<String>(raw) {
            return s;
        }
        if is_global_string_key(raw) {
            self.warn_once(
                &format!("gs:{raw}"),
                format!(
                    "{dbg}: text=\"{raw}\" is shaped like a GlobalStrings key but no such string \
                     global exists — showing the key"
                ),
            );
        }
        raw.to_string()
    }

    /// Walk one document's top-level items in order (rf24-framexml-loader.md, `0x6ede10`).
    pub(super) fn load_doc(&mut self, doc: &ParsedDocument) {
        for item in &doc.items {
            match item {
                TopLevel::Include(path) => self.do_include(path),
                TopLevel::Script(ScriptRef::File(path)) => match (self.files)(path) {
                    Some(text) => {
                        if let Err(e) = self.script.run(&text) {
                            self.report
                                .errors
                                .push(format!("<Script file=\"{path}\">: {e}"));
                        }
                    }
                    None => self.report.warnings.push(format!(
                        "<Script file=\"{path}\">: no provider hit; skipped"
                    )),
                },
                TopLevel::Script(ScriptRef::Inline(body)) => {
                    if let Err(e) = self.script.run(body) {
                        self.report.errors.push(format!("inline <Script>: {e}"));
                    }
                }
                TopLevel::Font(el) => self.do_font(el),
                TopLevel::Template(el) => {
                    // Register the raw (un-expanded) node; `expand` chain-resolves any `inherits` it
                    // carries at instantiation time. An unnamed virtual node was already warned at
                    // parse time and cannot be inherited, so it is simply not registered.
                    if let Some(name) = el.name() {
                        self.script
                            .framexml_templates
                            .borrow_mut()
                            .insert(name.to_string(), el.clone());
                    }
                }
                TopLevel::Instance(el) => {
                    let expanded = self.expand(el);
                    self.materialize(&expanded, None, framexml::DEFAULT_PARENT_NAME);
                }
            }
        }
    }

    pub(super) fn do_include(&mut self, path: &str) {
        let Some(text) = (self.files)(path) else {
            self.report.warnings.push(format!(
                "<Include file=\"{path}\">: no provider hit; skipped"
            ));
            return;
        };
        match framexml::parse(&text) {
            Ok(sub) => {
                self.report.warnings.extend(sub.warnings.iter().cloned());
                self.load_doc(&sub);
            }
            Err(e) => self
                .report
                .errors
                .push(format!("<Include file=\"{path}\">: {e}")),
        }
    }

    /// Resolve `inherits=` against the script's persistent registry (`framexml::expand`),
    /// folding its warnings into the report.
    pub(super) fn expand(&mut self, el: &Element) -> Element {
        let templates = self.script.framexml_templates.borrow();
        let view: HashMap<&str, &Element> =
            templates.iter().map(|(k, v)| (k.as_str(), v)).collect();
        let mut warns = Vec::new();
        let out = framexml::expand(el, &view, &mut warns);
        drop(view);
        drop(templates);
        self.report.warnings.extend(warns);
        out
    }

    /// [`Loader::expand`] for a `<Texture>`/`<FontString>` REGION — gated on the `inherits=`
    /// naming at least one registered element template, because a FontString's `inherits=`
    /// usually names a font OBJECT (`framexml_fonts`, a separate namespace resolved by
    /// `apply_fontstring_font`), which must pass through untouched rather than warn as an
    /// unknown template. The talent branch/arrow art pool is the first region-template user.
    pub(super) fn expand_region(&mut self, el: &Element) -> Element {
        let hit = el.attr("inherits").is_some_and(|names| {
            let templates = self.script.framexml_templates.borrow();
            names
                .split(',')
                .map(str::trim)
                .any(|n| templates.contains_key(n))
        });
        if hit {
            self.expand(el)
        } else {
            el.clone()
        }
    }

    /// A top-level `<Font name=…>`: resolve its `inherits=` chain against the accumulated font
    /// registry (a *separate* namespace from frame templates — a font inherits another font, never a
    /// frame), read the flattened `{font, height, color, outline, justifyH}`, register the resolved
    /// [`FontObject`] in the engine, and store the merged element so later fonts inheriting this one
    /// see fully-flattened values. An unnamed `<Font>` was already warned at parse time; skip it.
    pub(super) fn do_font(&mut self, el: &Element) {
        let Some(name) = el.name().map(str::to_string) else {
            return;
        };
        // Resolve `inherits` against the font registry (reuse the generic template merge).
        let merged = {
            let fonts = self.script.framexml_fonts.borrow();
            let view: HashMap<&str, &Element> =
                fonts.iter().map(|(k, v)| (k.as_str(), v)).collect();
            let mut warns = Vec::new();
            let merged = framexml::expand(el, &view, &mut warns);
            drop(view);
            drop(fonts);
            self.report.warnings.extend(warns);
            merged
        };

        let font = font_object_from_element(&merged);
        self.script.register_font_object(&name, font);
        // Store the merged (flattened) node so a chain rooted here reads resolved values.
        self.script.framexml_fonts.borrow_mut().insert(name, merged);
    }

    /// Materialize one (already template-expanded) frame element into a live frame, then recurse its
    /// nested `<Frames>` and fire its `OnLoad` **after** them (bottom-up, rf26).
    ///
    /// `parent` is the enclosing frame's wrapper (`None` at top level). `parent_name` is the
    /// already-resolved name of the nearest named ancestor (or `"Top"`), used to substitute `$parent`
    /// in this frame's name and in its anchors' `relativeTo` (rf27).
    pub(super) fn materialize(&mut self, el: &Element, parent: Option<&Table>, parent_name: &str) {
        // Name (with $parent substitution) — the resolved name is what CreateFrame publishes and what
        // this frame's own children substitute `$parent` against.
        let resolved_name: Option<String> = el
            .name()
            .map(|raw| framexml::resolve_name(raw, parent_name));

        // 1 · CreateFrame(kind = element tag, name, parent). An unknown frame type errors here (the
        //     factory-table miss, rf24 `0x6ee280`) — record it and skip the whole subtree.
        let create: Function = match self.lua().globals().get("CreateFrame") {
            Ok(f) => f,
            Err(e) => {
                self.report
                    .errors
                    .push(format!("CreateFrame global missing: {e}"));
                return;
            }
        };
        let wrapper: Table =
            match create.call((el.tag.clone(), resolved_name.clone(), parent.cloned())) {
                Ok(w) => w,
                Err(e) => {
                    self.report.errors.push(format!(
                        "CreateFrame(<{}>{}): {e}",
                        el.tag,
                        resolved_name
                            .as_deref()
                            .map(|n| format!(" name=\"{n}\""))
                            .unwrap_or_default()
                    ));
                    return;
                }
            };
        self.report.frames += 1;

        let dbg_name = resolved_name
            .clone()
            .unwrap_or_else(|| format!("<{}>", el.tag));

        // This frame's own name is what its *contents* (regions, nested frames) substitute `$parent`
        // against (rf27: a region/child's parent is this frame); a nameless frame passes the nearest
        // named ancestor through unchanged. This frame's *own* anchors, by contrast, substitute
        // `$parent` against `parent_name` (their `$parent` is this frame's enclosing parent).
        let self_name = resolved_name.as_deref().unwrap_or(parent_name).to_string();

        // 2 · LoadXML attributes (rf24 `0x769820`).
        self.apply_attrs(el, &wrapper, &dbg_name);
        // 3 · <Size> and 4 · <Anchors> (the CLayoutFrame geometry base, rf24 `0x767800`).
        self.apply_size(el, &wrapper, &dbg_name);
        self.apply_anchors(el, &wrapper, parent_name, &dbg_name);
        // 5 · <Layers> regions (rf24 `0x769d70`) — `$parent` in a region name is *this* frame.
        self.apply_layers(el, &wrapper, &self_name, &dbg_name);
        // 5·b — a frame's **direct-child** `<FontString>` (outside `<Layers>`): the engine's special
        // font string (a ScrollingMessageFrame's line font, an EditBox's text font). Created on
        // OVERLAY so its resolved font object (ChatFontNormal — face/height/shadow) is what the
        // frame's line/text rendering reads. See [`Self::apply_special_fontstrings`].
        self.apply_special_fontstrings(el, &wrapper, &self_name, &dbg_name);
        // 5a · <Backdrop> plate (rf24 LoadXML `0x77e6c0`): the tiled bg + 8-piece border.
        self.apply_backdrop(el, &wrapper, &dbg_name);
        // 5b · per-kind LoadXML extras (RF-28's typed tables) — StatusBar + Button/CheckButton;
        //      the EditBox flags/caps (RF-0082).
        self.apply_statusbar(el, &wrapper, &dbg_name);
        self.apply_slider(el, &wrapper, &self_name, &dbg_name);
        self.apply_button(el, &wrapper, &self_name, &dbg_name);
        self.apply_editbox(el, &wrapper, &dbg_name);
        self.apply_messageframe(el, &wrapper, &dbg_name);
        // 6 · <Scripts> handlers (rf24 `0x769ef0`); OnLoad is captured to fire bottom-up below.
        let onload = self.apply_scripts(el, &wrapper, &dbg_name);

        // 7 · nested <Frames> — build children (firing THEIR OnLoads) before ours (rf26). A child's
        //     `$parent` (name and anchors) resolves against this frame's name.
        for frames_el in children_named(el, "Frames") {
            for child in &frames_el.children {
                let expanded = self.expand(child);
                self.materialize(&expanded, Some(&wrapper), &self_name);
            }
        }

        // 8 · this frame's OnLoad, now that its subtree is complete (bottom-up).
        if let Some(func) = onload {
            self.fire_onload(&wrapper, &func, &dbg_name);
        }
    }
}

/// Iterate an element's direct children whose tag matches `tag` (case-insensitively).
pub(super) fn children_named<'a>(
    el: &'a Element,
    tag: &'a str,
) -> impl Iterator<Item = &'a Element> {
    el.children
        .iter()
        .filter(move |c| c.tag.eq_ignore_ascii_case(tag))
}

/// Does this `text=` value LOOK like a GlobalStrings key? `SCREAMING_SNAKE`, two characters or more —
/// at least one letter, and nothing but `A-Z`, `0-9`, `_`. The reference's whole string table is
/// written this way (`CREATE_MACROS`, `DELETE`, `EXIT_GAME`), and benilla's own literals never are
/// ("Send Mail", "No results found."), so the shape cleanly separates "you meant a key" from "you
/// meant these words" — all [`Loader::resolve_text`] needs it for. The length floor is the one real
/// false positive it buys off: a single-character label (`text="X"` on a close button) is a glyph,
/// never a key.
fn is_global_string_key(s: &str) -> bool {
    s.len() >= 2
        && s.chars().any(|c| c.is_ascii_uppercase())
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Read an `x`/`y` dimension from a `<Size>`/`<Offset>` element: prefer a `<AbsDimension>` child,
/// else the element's own `x`/`y` attributes. Either component may be absent.
pub(super) fn abs_dim(el: &Element) -> (Option<f32>, Option<f32>) {
    let src = children_named(el, "AbsDimension").next().unwrap_or(el);
    let x = src.attr("x").and_then(|v| v.trim().parse::<f32>().ok());
    let y = src.attr("y").and_then(|v| v.trim().parse::<f32>().ok());
    (x, y)
}

/// Read an RGBA colour from a `<Color r= g= b= a=/>` element (`a` defaults to 1.0).
pub(super) fn color_of(el: &Element) -> [f32; 4] {
    let c = |k: &str, d: f32| {
        el.attr(k)
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(d)
    };
    [c("r", 0.0), c("g", 0.0), c("b", 0.0), c("a", 1.0)]
}

/// Read a `<TexCoords left right top bottom>` child's UV rect `(l, r, t, b)`, if present and
/// fully-specified. A `<TexCoords>` missing any of the four edges is skipped (the loader leaves the
/// texture at full-texture UVs rather than guess a half-set crop).
pub(super) fn tex_coords_of(el: &Element) -> Option<(f32, f32, f32, f32)> {
    let tc = children_named(el, "TexCoords").next()?;
    let edge = |k: &str| tc.attr(k).and_then(|v| v.trim().parse::<f32>().ok());
    Some((edge("left")?, edge("right")?, edge("top")?, edge("bottom")?))
}

/// Read a `<FontHeight>`/`<AbsValue>`-style scalar: prefer an `<AbsValue val=/>` child, else the
/// element's own `val` attribute (the `Value` type's inline form). `None` if neither parses.
pub(super) fn abs_value(el: &Element) -> Option<f32> {
    let src = children_named(el, "AbsValue").next().unwrap_or(el);
    src.attr("val").and_then(|v| v.trim().parse::<f32>().ok())
}

/// Build a resolved [`FontObject`] from an already-inherits-flattened `<Font>` element: the `font=`
/// path, the *last* `<FontHeight>` and `<Color>` (merge appends inherited-first, so the last wins —
/// an override), the `outline=` OUTLINETYPE, and optional `justifyH=`/`justifyV=`.
pub(super) fn font_object_from_element(el: &Element) -> FontObject {
    let justify_h = el
        .attr("justifyH")
        .map(|j| match j.to_ascii_uppercase().as_str() {
            "LEFT" => JustifyH::Left,
            "RIGHT" => JustifyH::Right,
            _ => JustifyH::Center,
        });
    let justify_v = el
        .attr("justifyV")
        .map(|j| match j.to_ascii_uppercase().as_str() {
            "TOP" => JustifyV::Top,
            "BOTTOM" => JustifyV::Bottom,
            _ => JustifyV::Middle,
        });
    // <Shadow><Offset><AbsDimension x= y=/></Offset><Color r= g= b=/></Shadow> — inherited via the
    // template merge like every other child (the MasterFont root's (1,-1) black reaches the whole
    // GameFont* family; ref-Fonts.xml l.55-62).
    let shadow = children_named(el, "Shadow").last().map(|sh| {
        let offset = children_named(sh, "Offset")
            .next()
            .map(abs_dim)
            .map(|(x, y)| [x.unwrap_or(0.0), y.unwrap_or(0.0)])
            .unwrap_or([0.0, 0.0]);
        let color = children_named(sh, "Color")
            .next()
            .map(color_of)
            .unwrap_or([0.0, 0.0, 0.0, 1.0]);
        crate::script::FontShadow { offset, color }
    });
    FontObject {
        font: el.attr("font").map(str::to_string),
        height: children_named(el, "FontHeight").last().and_then(abs_value),
        color: children_named(el, "Color").last().map(color_of),
        outline: el.attr("outline").map(Outline::parse).unwrap_or_default(),
        justify_h,
        justify_v,
        shadow,
    }
}

#[cfg(test)]
mod tests;
