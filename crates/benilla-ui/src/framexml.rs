//! The FrameXML **document** layer (decision 0068): turns a `.xml` file's text into an owned,
//! order-preserving tree, resolves `virtual`/`inherits` template expansion, and rewrites `$parent`
//! name tokens. This is XML-tree semantics only — no widget instantiation (frame factories, anchor
//! resolution, script compilation) and no Lua; those live in the layer above, which walks the tree
//! this module produces.
//!
//! Ground truth is wow-5875-re's verified transcription of the real client's loader
//! (`0x6edc00–0x6f3000`, closed VERIFIED ORCHESTRATION region):
//! - `system/ui/ui.md` §"The FrameXML XML loader — schema → frame-op interpretation (RF-0024)"
//! - `system/ui/scratch/rf24-framexml-loader.md` — the full top-level + per-element op map
//! - `system/ui/scratch/rf26-nested-frames.md` — `<Frames>` / load-order (widget-layer concern; cited
//!   here only for the "spliced before the instance's own nodes" ordering that also governs template
//!   `<Frames>` children, which this module's generic child-concat already gets right)
//! - `system/ui/scratch/rf27-parent-name-token.md` — the `$parent` substitution rule (RF-0027)
//!
//! XML *parsing* is DELEGATED plumbing per the RE spec ("XML parsing is DELEGATED; the schema→op map
//! is owned") — we use `roxmltree` rather than reproduce a tokenizer; only the schema→op
//! interpretation below is transcribed from the spec.

use std::collections::{HashMap, HashSet};
use std::fmt;

/// An owned XML element: tag, attributes (order preserved — Blizzard XML attribute order is
/// sometimes meaningful for authoring tools, and always useful for round-tripping/debugging), and
/// element children. Case-insensitive attribute lookup: "attrs via `GetAttribute 0x6f2cf0`" and
/// element-name compares are case-insensitive in the real loader (`0x64a4c0`,
/// rf24-framexml-loader.md, header), and Blizzard's own shipped XML is inconsistent about attribute
/// casing (e.g. both `relativeTo` and stray all-lowercase variants appear in the wild), so lookups
/// here fold case rather than assume a canonical spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub tag: String,
    attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    /// The node's own direct text content (the XML node struct's `body-text @ +0xc`, per
    /// rf24-framexml-loader.md's node-struct note). Populated for text-bearing elements such as
    /// `<Scripts>` handler bodies (`<OnLoad>lua…</OnLoad>`) and inline `<Script>` bodies. Empty for
    /// purely structural elements.
    pub body: String,
}

impl Element {
    /// Case-insensitive attribute lookup (see struct docs).
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// A boolean attribute (`hidden="true"`, `virtual="true"`, …): true iff present and equal to
    /// the literal `"true"`, case-insensitively (`0x6f1b30`'s true-cmp, rf24-framexml-loader.md).
    /// Absent or any other value is false — matching the client (there is no explicit `"false"`
    /// branch in the spec; only a `true` match flips the flag).
    pub fn attr_bool(&self, name: &str) -> bool {
        self.attr(name)
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }

    /// The `name` attribute, if any.
    pub fn name(&self) -> Option<&str> {
        self.attr("name")
    }

    /// All attributes, in document order, as written.
    pub fn attrs(&self) -> &[(String, String)] {
        &self.attrs
    }
}

/// A `<Script>` reference: an external file (`<Script file="…"/>`) or an inline body
/// (`<Script>lua…</Script>`). Both run in document order relative to everything else at the top
/// level ("`<Script file=>`/inline `<Script>` → run Lua … **in document order** — this is how
/// XML-referenced FrameXML Lua loads", rf24-framexml-loader.md, top level).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptRef {
    File(String),
    Inline {
        body: String,
        /// The 1-based line **of this XML file** that `body`'s first character sits on.
        ///
        /// Carried so the loader can pad the chunk out to that offset, making Lua's own error
        /// lines the file's lines. Without it every inline block starts at line 1 and a raise
        /// reports a number belonging to no file — worse than no number, because it looks like one
        /// you could go and read (decision 1214).
        line: u32,
    },
}

/// One top-level item of a FrameXML document, in document order. Modeled as an order-preserving
/// `Vec<TopLevel>` rather than a split `{ scripts, fonts, templates, instances }` struct because the
/// real loader executes `<Include>`/`<Script>` interleaved with frame element definitions **in
/// document order** (rf24-framexml-loader.md, top level) — a split-by-kind struct would lose that
/// order and misrepresent Lua load sequencing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopLevel {
    /// `<Include file="…"/>` — recurse-load another file at this point in the sequence
    /// (rf24-framexml-loader.md, top level: `0x8710c0`). File resolution/recursion is the loader
    /// layer's job; this only preserves *where* the include happens relative to everything else.
    Include(String),
    Script(ScriptRef),
    /// `<Font …>` — a named font definition (rf24-framexml-loader.md, top level: `0x87106c`).
    Font(Element),
    /// A frame/region element with `virtual="true"`: registered as a template keyed by `name`, not
    /// instantiated (rf24-framexml-loader.md, top level: `0x6ee500`).
    Template(Element),
    /// A frame/region element without `virtual="true"`: instantiated in place.
    Instance(Element),
}

/// The result of parsing one FrameXML document: its top-level items in order, plus any tolerable
/// issues found along the way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDocument {
    pub items: Vec<TopLevel>,
    /// Tolerable issues: unregistered (unnamed) virtual templates, missing `Include`/`Font`
    /// attributes, a non-`<Ui>` root, and similar — the real client logs and continues rather than
    /// aborting the whole file for these, so parsing does too (see [`parse`]'s docs).
    pub warnings: Vec<String>,
}

impl ParsedDocument {
    /// All registered templates, keyed by name, for use with [`expand`]. A [`TopLevel::Template`]
    /// with no `name` (already flagged in [`ParsedDocument::warnings`] at parse time — an
    /// "Unnamed virtual node" load error in the real client, rf24-framexml-loader.md) is simply
    /// absent here, matching the fact that nothing can `inherits=` it.
    pub fn templates(&self) -> HashMap<&str, &Element> {
        self.items
            .iter()
            .filter_map(|item| match item {
                TopLevel::Template(el) => el.name().map(|name| (name, el)),
                _ => None,
            })
            .collect()
    }
}

/// A document parse error: malformed XML. Tolerable issues (unknown tags, missing optional
/// attributes, an unregistrable template) are not errors — see [`ParsedDocument::warnings`].
#[derive(Debug)]
pub enum Error {
    Xml(roxmltree::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Xml(e) => write!(f, "malformed FrameXML document: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Xml(e) => Some(e),
        }
    }
}

/// Parse FrameXML document text into an owned, order-preserving tree.
///
/// Top-level semantics (rf24-framexml-loader.md, "top level"): the root's children are walked in
/// order; `<Include file=>` and `<Script file=>`/inline `<Script>` are preserved as-is (their
/// execution is the loader layer's job — this only fixes their position); `<Font>` becomes a font
/// definition; any other element with `virtual="true"` registers as a template (by `name`); anything
/// else is an instance. Unknown top-level tags (not one of the 13 known frame-element types) are
/// kept as generic [`Element`]s and classified the same way (template if `virtual="true"`, else
/// instance) — the real client's loader tolerates them the same way (widget-type validation happens
/// later, at instantiation, which is out of this module's scope).
///
/// A root element that isn't (case-insensitively) `<Ui>` is tolerated with a warning, not a hard
/// error: the RE spec never states the loader checks the root's own tag name, only that it walks
/// its children (`0x6ede10`).
pub fn parse(text: &str) -> Result<ParsedDocument, Error> {
    let doc = roxmltree::Document::parse(text).map_err(Error::Xml)?;
    let root = doc.root_element();

    let mut warnings = Vec::new();
    if !root.tag_name().name().eq_ignore_ascii_case("Ui") {
        warnings.push(format!(
            "root element is <{}>, not <Ui>; tolerated (rf24-framexml-loader.md's file loader walks \
             the root's children regardless of the root's own tag)",
            root.tag_name().name()
        ));
    }

    let mut items = Vec::new();
    for child in root.children().filter(|n| n.is_element()) {
        let tag = child.tag_name().name();
        if tag.eq_ignore_ascii_case("Include") {
            match attr_ci(child, "file") {
                Some(file) => items.push(TopLevel::Include(file)),
                None => warnings.push("<Include> without a file attribute; skipped".to_string()),
            }
        } else if tag.eq_ignore_ascii_case("Script") {
            match attr_ci(child, "file") {
                Some(file) => items.push(TopLevel::Script(ScriptRef::File(file))),
                None => items.push(TopLevel::Script(ScriptRef::Inline {
                    body: direct_text(child),
                    line: inline_start_line(&doc, child),
                })),
            }
        } else if tag.eq_ignore_ascii_case("Font") {
            let el = element_from_node(child);
            if el.name().is_none() {
                warnings.push("<Font> without a name attribute".to_string());
            }
            items.push(TopLevel::Font(el));
        } else {
            let el = element_from_node(child);
            if el.attr_bool("virtual") {
                if el.name().is_none() {
                    warnings.push(format!(
                        "virtual <{}> without a name — a load error in the real client \
                         (\"Unnamed virtual node\", rf24-framexml-loader.md); kept but unregistered, \
                         so nothing can inherit it",
                        el.tag
                    ));
                }
                items.push(TopLevel::Template(el));
            } else {
                items.push(TopLevel::Instance(el));
            }
        }
    }

    Ok(ParsedDocument { items, warnings })
}

fn attr_ci(node: roxmltree::Node, name: &str) -> Option<String> {
    node.attributes()
        .find(|a| a.name().eq_ignore_ascii_case(name))
        .map(|a| a.value().to_string())
}

/// Concatenates a node's own direct text/CDATA children (not descending into child elements) —
/// the XML node struct's `body-text @ +0xc` (rf24-framexml-loader.md, header).
/// The 1-based line of the first text byte inside a `<Script>` element.
///
/// Taken from the first text child's own range rather than the element's, because a CDATA section
/// puts `<![CDATA[` between the two and every FrameXML script block in this project uses one — the
/// element's start line would be off by however much of the opening tag precedes the body.
/// Falls back to the element's line for a `<Script></Script>` with no text at all.
fn inline_start_line(doc: &roxmltree::Document, node: roxmltree::Node) -> u32 {
    let at = node
        .children()
        .find(|n| n.is_text())
        .unwrap_or(node)
        .range()
        .start;
    doc.text_pos_at(at).row
}

fn direct_text(node: roxmltree::Node) -> String {
    node.children()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

fn element_from_node(node: roxmltree::Node) -> Element {
    Element {
        tag: node.tag_name().name().to_string(),
        attrs: node
            .attributes()
            .map(|a| (a.name().to_string(), a.value().to_string()))
            .collect(),
        children: node
            .children()
            .filter(|n| n.is_element())
            .map(element_from_node)
            .collect(),
        body: direct_text(node),
    }
}

/// Resolve `inherits="A, B"` template references into a fully materialized [`Element`]: a
/// structural merge with inherited content first, the element's own content last.
///
/// **Splice order** (rf24-framexml-loader.md, "virtual / template inheritance": "the matched
/// entry is a template, its stored attribute/child nodes are spliced in first" [then the instance's
/// own nodes are read on top]; rf26-nested-frames.md confirms the same "inherited-first" ordering
/// for a template's own `<Frames>` children) — so:
/// - **children**: the (fully expanded) template's children first, in order, then the element's own
///   children appended after.
/// - **attributes**: the template's attributes, with the element's own attributes overriding any
///   attribute of the same name (case-insensitively) and adding any the template didn't have. This
///   is the literal reading of "spliced in first, then the instance's own" applied to attributes —
///   note this means an element that omits `name` entirely while inheriting a *named* template would
///   inherit that template's registration name, and `virtual="true"`/`inherits="…"` themselves are
///   ordinary attributes that splice through too (harmlessly inert post-expansion: nothing reads
///   them again after the one-time top-level virtual/instance routing decision, which already
///   happened on the *outer* node before expansion ever runs). Real FrameXML avoids ever hitting the
///   `name` case (instances either supply their own name or stay anonymous), and the RE notes don't
///   call out a `name`/`virtual` exception, so this implementation does not special-case them —
///   flagged as an unverified edge case, not a silent guess.
///
/// **Multiple `inherits`** (`"A, B"`, comma-separated, left-to-right): each named template is
/// resolved and merged in order, so `B`'s content lands on top of `A`'s where they overlap, and the
/// element's own content lands on top of both. The RE spec documents the single-template splice
/// precisely but not multi-template precedence beyond "the named template's[s'] … nodes"
/// (rf24-framexml-loader.md); left-to-right precedence (later name wins ties) is the natural reading
/// and is what this function implements — flagged, not guessed silently, since it isn't
/// byte-verified for the multi-name case specifically.
///
/// **Chains** (a template inheriting another template) recurse: each named template is itself fully
/// expanded (its own `inherits` resolved) before being merged in.
///
/// **Cycles** are guarded: if expanding a chain would re-enter a template already being expanded,
/// that reference is skipped and a warning is appended instead of infinite-recursing.
pub fn expand(
    element: &Element,
    templates: &HashMap<&str, &Element>,
    warnings: &mut Vec<String>,
) -> Element {
    let mut active = HashSet::new();
    expand_inner(element, templates, warnings, &mut active)
}

fn expand_inner(
    element: &Element,
    templates: &HashMap<&str, &Element>,
    warnings: &mut Vec<String>,
    active: &mut HashSet<String>,
) -> Element {
    let Some(inherits) = element.attr("inherits") else {
        return element.clone();
    };

    let mut base: Option<Element> = None;
    for name in inherits.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !active.insert(name.to_string()) {
            warnings.push(format!(
                "inheritance cycle detected at template '{name}'; skipping this reference"
            ));
            continue;
        }
        let Some(template) = templates.get(name) else {
            warnings.push(format!(
                "unknown template '{name}' referenced by inherits; skipping"
            ));
            active.remove(name);
            continue;
        };
        let expanded_template = expand_inner(template, templates, warnings, active);
        active.remove(name);
        base = Some(match base {
            None => expanded_template,
            Some(acc) => merge(&acc, &expanded_template),
        });
    }

    match base {
        Some(base) => merge(&base, element),
        None => element.clone(),
    }
}

/// The structural merge documented on [`expand`]: `base`'s children first, `over`'s appended after;
/// `over`'s attributes override `base`'s of the same name (case-insensitive), and are added if
/// `base` didn't have them. `over`'s tag and body win (an instance's own tag/handler body, if any,
/// is what actually runs).
fn merge(base: &Element, over: &Element) -> Element {
    let mut attrs = base.attrs.clone();
    for (k, v) in &over.attrs {
        if let Some(slot) = attrs.iter_mut().find(|(ek, _)| ek.eq_ignore_ascii_case(k)) {
            slot.1 = v.clone();
        } else {
            attrs.push((k.clone(), v.clone()));
        }
    }

    let mut children = base.children.clone();
    children.extend(over.children.iter().cloned());

    Element {
        tag: over.tag.clone(),
        attrs,
        children,
        body: if over.body.is_empty() {
            base.body.clone()
        } else {
            over.body.clone()
        },
    }
}

/// The synthetic element a **runtime** `CreateFrame(kind, name, parent, "A, B")` expands: a bare
/// node of the caller's own `tag` carrying nothing but the `inherits=` list.
///
/// Handing this to [`expand`] is what makes the Lua path and the XML path *the same* path — one
/// comma-split, one chain resolution, one cycle guard, one unknown-template warning, one splice
/// order. Nothing about `inherits=` is written twice.
///
/// And because [`merge`] takes the **overriding** node's tag, the expanded result carries `tag` —
/// the kind `CreateFrame` was actually given — whatever the templates were declared as. That is
/// the honest answer to `CreateFrame("Frame", n, p, "SomeButtonTemplate")`: the frame already
/// exists as a Frame, so the template's `<Button>` tag cannot retype it, and the loader's per-kind
/// passes (which every one of them gate on the element tag) skip the parts that could not apply
/// anyway.
pub fn inherits_node(tag: &str, inherits: &str) -> Element {
    Element {
        tag: tag.to_string(),
        attrs: vec![("inherits".to_string(), inherits.to_string())],
        children: Vec::new(),
        body: String::new(),
    }
}

/// The literal fallback base name (rf27-parent-name-token.md, rule 5): when a `$parent`-prefixed
/// name has no named ancestor to substitute (e.g. a top-level element), the real client seeds the
/// result with the literal string `"Top"` (VA `0x8788ac`) rather than leaving the token literal,
/// erroring, or producing an empty name.
pub const DEFAULT_PARENT_NAME: &str = "Top";

/// `$parent` name-token substitution (RF-0027, rf27-parent-name-token.md).
///
/// A `name` attribute beginning, **case-insensitively**, with the literal `"$parent"` (rule 5: the
/// compare folds `A`–`Z`; `$Parent`/`$PARENT` all match — the *only* such token, there is no
/// `$parentKey`/`$parentN` variant, rf27 point 1) has that 7-character prefix replaced with
/// `parent_name`, and the remainder of `raw` appended verbatim (`SStrCat`). A `name` not starting
/// with the token is copied through unchanged.
///
/// `parent_name` must be the caller's **already-resolved** name for the nearest ancestor that has
/// one — i.e. the caller walks the ancestor chain (`this+0x9c`, rf27 rule 3) for the first ancestor
/// whose own (already-substituted) name is non-empty, and passes [`DEFAULT_PARENT_NAME`] (`"Top"`)
/// if there is none. Passing the already-resolved name (not the raw ancestor `name=` attribute
/// text) is what makes nested `$parent` chains compose (rf27 rule 4): a child of a
/// `$parent`-named parent inherits the parent's fully-resolved name, e.g. parent `PlayerFrame` +
/// `"$parentHealthBar"` → `"PlayerFrameHealthBar"`, and a grandchild's `"$parentBar"` resolves
/// against `"PlayerFrameHealthBar"`, not against the literal text `"$parentHealthBar"`.
///
/// Applies only to the `name` attribute (rf27 point 2) — `parent=` (which *resolves* the enclosing
/// frame) is never `$parent`-substituted, and no other attribute is either.
pub fn resolve_name(raw: &str, parent_name: &str) -> String {
    const TOKEN: &str = "$parent";
    match raw.get(..TOKEN.len()) {
        Some(prefix) if prefix.eq_ignore_ascii_case(TOKEN) => {
            format!("{parent_name}{}", &raw[TOKEN.len()..])
        }
        _ => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_order_preserves_script_and_element_interleave() {
        let doc = parse(
            r#"<Ui>
                <Script file="A.lua"/>
                <Frame name="First"/>
                <Script>print("inline")</Script>
                <Include file="Other.xml"/>
                <Frame name="Second"/>
            </Ui>"#,
        )
        .unwrap();

        let kinds: Vec<&str> = doc
            .items
            .iter()
            .map(|item| match item {
                TopLevel::Include(_) => "include",
                TopLevel::Script(ScriptRef::File(_)) => "script-file",
                TopLevel::Script(ScriptRef::Inline { .. }) => "script-inline",
                TopLevel::Font(_) => "font",
                TopLevel::Template(_) => "template",
                TopLevel::Instance(_) => "instance",
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "script-file",
                "instance",
                "script-inline",
                "include",
                "instance"
            ],
            "document order must be preserved, not grouped by kind"
        );

        match &doc.items[0] {
            TopLevel::Script(ScriptRef::File(f)) => assert_eq!(f, "A.lua"),
            other => panic!("expected file script, got {other:?}"),
        }
        assert!(doc.warnings.is_empty());
    }

    #[test]
    fn inline_vs_file_script() {
        let doc = parse(
            r#"<Ui>
                <Script file="Foo.lua"/>
                <Script>local x = 1
print(x)</Script>
            </Ui>"#,
        )
        .unwrap();
        assert_eq!(
            doc.items[0],
            TopLevel::Script(ScriptRef::File("Foo.lua".to_string()))
        );
        match &doc.items[1] {
            TopLevel::Script(ScriptRef::Inline { body, line }) => {
                assert!(body.contains("local x = 1"));
                assert!(body.contains("print(x)"));
                // Line 3 of the literal above: `<Ui>`, `<Script file=…>`, then this one.
                assert_eq!(*line, 3, "the inline body's own line in the file");
            }
            other => panic!("expected inline script, got {other:?}"),
        }
    }

    #[test]
    fn virtual_registers_as_template_non_virtual_as_instance() {
        let doc = parse(
            r#"<Ui>
                <Frame name="MyTemplate" virtual="true"/>
                <Frame name="MyInstance"/>
                <Frame VIRTUAL="TRUE" name="CasedTemplate"/>
            </Ui>"#,
        )
        .unwrap();
        assert!(matches!(doc.items[0], TopLevel::Template(_)));
        assert!(matches!(doc.items[1], TopLevel::Instance(_)));
        assert!(
            matches!(doc.items[2], TopLevel::Template(_)),
            "virtual attribute name/value compare is case-insensitive"
        );
        let templates = doc.templates();
        assert!(templates.contains_key("MyTemplate"));
        assert!(templates.contains_key("CasedTemplate"));
        assert_eq!(templates.len(), 2);
    }

    #[test]
    fn unnamed_virtual_warns_but_does_not_fail() {
        let doc = parse(r#"<Ui><Frame virtual="true"/></Ui>"#).unwrap();
        assert!(matches!(doc.items[0], TopLevel::Template(_)));
        assert_eq!(doc.warnings.len(), 1);
        assert!(
            doc.warnings[0].contains("Unnamed virtual node")
                || doc.warnings[0].contains("without a name")
        );
        assert!(
            doc.templates().is_empty(),
            "an unnamed template cannot be inherited"
        );
    }

    #[test]
    fn unknown_top_level_tag_is_tolerated_as_generic_element() {
        let doc = parse(r#"<Ui><TotallyMadeUpTag name="Whatever" foo="bar"/></Ui>"#).unwrap();
        match &doc.items[0] {
            TopLevel::Instance(el) => {
                assert_eq!(el.tag, "TotallyMadeUpTag");
                assert_eq!(el.attr("foo"), Some("bar"));
            }
            other => panic!("expected a generic instance, got {other:?}"),
        }
        assert!(doc.warnings.is_empty());
    }

    #[test]
    fn multi_inherits_merges_attrs_override_and_children_concat_in_order() {
        let doc = parse(
            r#"<Ui>
                <Frame name="A" virtual="true">
                    <Layers><Layer level="ARTWORK"><Texture name="FromA"/></Layer></Layers>
                </Frame>
                <Frame name="B" virtual="true" alpha="0.5">
                    <Layers><Layer level="ARTWORK"><Texture name="FromB"/></Layer></Layers>
                </Frame>
                <Frame name="Inst" inherits="A, B" alpha="1.0">
                    <Layers><Layer level="ARTWORK"><Texture name="FromInst"/></Layer></Layers>
                </Frame>
            </Ui>"#,
        )
        .unwrap();
        let templates = doc.templates();
        let TopLevel::Instance(inst) = &doc.items[2] else {
            panic!("expected instance")
        };
        let mut warnings = Vec::new();
        let expanded = expand(inst, &templates, &mut warnings);
        assert!(warnings.is_empty());

        // Instance's own `alpha` overrides B's (which would otherwise have overridden A's absence).
        assert_eq!(expanded.attr("alpha"), Some("1.0"));

        // Children: A's <Layers> first, then B's <Layers>, then the instance's own <Layers> —
        // inherited-first, instance-own-last (rf24-framexml-loader.md splice order).
        assert_eq!(expanded.children.len(), 3);
        fn texture_name(el: &Element) -> &str {
            el.children[0].children[0].attr("name").unwrap()
        }
        assert_eq!(texture_name(&expanded.children[0]), "FromA");
        assert_eq!(texture_name(&expanded.children[1]), "FromB");
        assert_eq!(texture_name(&expanded.children[2]), "FromInst");
    }

    #[test]
    fn inheritance_chain_recurses() {
        let doc = parse(
            r#"<Ui>
                <Frame name="Root" virtual="true" frameStrata="LOW">
                    <Layers><Layer level="ARTWORK"><Texture name="RootTex"/></Layer></Layers>
                </Frame>
                <Frame name="Mid" virtual="true" inherits="Root" frameStrata="MEDIUM">
                    <Layers><Layer level="ARTWORK"><Texture name="MidTex"/></Layer></Layers>
                </Frame>
                <Frame name="Leaf" inherits="Mid"/>
            </Ui>"#,
        )
        .unwrap();
        let templates = doc.templates();
        let TopLevel::Instance(leaf) = &doc.items[2] else {
            panic!("expected instance")
        };
        let mut warnings = Vec::new();
        let expanded = expand(leaf, &templates, &mut warnings);
        assert!(warnings.is_empty());
        // Mid's own frameStrata (it overrode Root's) wins, since chain-expanding Mid resolves Root
        // first and Mid's own attrs on top, before Leaf (which sets nothing) merges on top of that.
        assert_eq!(expanded.attr("frameStrata"), Some("MEDIUM"));
        assert_eq!(
            expanded.children.len(),
            2,
            "Root's then Mid's <Layers>, in order"
        );
    }

    #[test]
    fn inheritance_cycle_warns_and_terminates() {
        let doc = parse(
            r#"<Ui>
                <Frame name="A" virtual="true" inherits="B"/>
                <Frame name="B" virtual="true" inherits="A"/>
                <Frame name="Inst" inherits="A"/>
            </Ui>"#,
        )
        .unwrap();
        let templates = doc.templates();
        let TopLevel::Instance(inst) = &doc.items[2] else {
            panic!("expected instance")
        };
        let mut warnings = Vec::new();
        // Must terminate (not infinitely recurse) and report the cycle.
        let _expanded = expand(inst, &templates, &mut warnings);
        assert!(
            warnings.iter().any(|w| w.contains("cycle")),
            "expected a cycle warning, got {warnings:?}"
        );
    }

    #[test]
    fn parent_name_substitution_basic_and_case_insensitive() {
        assert_eq!(
            resolve_name("$parentHealthBar", "PlayerFrame"),
            "PlayerFrameHealthBar"
        );
        assert_eq!(
            resolve_name("$PARENThealthbar", "PlayerFrame"),
            "PlayerFramehealthbar"
        );
        assert_eq!(resolve_name("$Parent", "PlayerFrame"), "PlayerFrame");
        assert_eq!(
            resolve_name("NotAParentName", "PlayerFrame"),
            "NotAParentName"
        );
        // No named ancestor: caller passes the literal "Top" fallback.
        assert_eq!(resolve_name("$parentFoo", DEFAULT_PARENT_NAME), "TopFoo");
    }

    #[test]
    fn parent_name_substitution_composes_through_nested_children() {
        // A named parent, a child using $parent, and a grandchild using $parent against the
        // child's *resolved* name — not the child's raw literal "$parent..." text (rf27 rule 4).
        let parent_name = "PlayerFrame";
        let child_name = resolve_name("$parentHealthBar", parent_name);
        assert_eq!(child_name, "PlayerFrameHealthBar");
        let grandchild_name = resolve_name("$parentText", &child_name);
        assert_eq!(grandchild_name, "PlayerFrameHealthBarText");
    }

    /// Parses a REAL shipped FrameXML file when `BENILLA_FRAMEXML` points at one (never committed —
    /// extract e.g. `Interface\FrameXML\PlayerFrame.xml` with benilla-extract). Skips silently
    /// otherwise, matching toc.rs's `real_manifest_when_available` pattern so CI/gates don't depend
    /// on client data.
    #[test]
    fn real_framexml_when_available() {
        let Ok(path) = std::env::var("BENILLA_FRAMEXML") else {
            return;
        };
        let text = std::fs::read_to_string(&path).expect("reading BENILLA_FRAMEXML");
        let doc = parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert!(
            !doc.items.is_empty(),
            "{path}: a real FrameXML file has at least one top-level item"
        );
        for w in &doc.warnings {
            eprintln!("{path}: warning: {w}");
        }
    }
}
