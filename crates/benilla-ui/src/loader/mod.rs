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
//! **bytes** (from the MPQ/addon dir); the loader stays IO-free.
//!
//! **Bytes, not text** (decision 1193). A `<Script file=>` chunk is handed to Lua as the bytes on
//! disk, exactly as the reference's `luaL_loadbuffer` receives them; only an `<Include>`d document
//! is decoded ([`crate::source::decode`]), because roxmltree needs `&str` and Lua does not. Before
//! 1193 the provider returned `String`, so a cp1252 locale file did not lose a glyph — it read as
//! *absent*, and the include or every handler in the script vanished with it.
//!
//! **The loader owns relative-path resolution; the provider owns the root** (decision 1186). A
//! reference is relative to the directory of the *file that contains it*, and may walk up with `..`
//! — `Bagnon/src/main.xml` reaches its sibling as `templates.xml` and a shared library addon as
//! `..\..\BagBrother\core\core.xml`. Only the loader knows the include tree, so it does the joining
//! ([`join_ref`]) and hands the provider one already-resolved path; the provider decides what that
//! path is allowed to reach. [`load`] starts at the root, [`load_in`] starts at a named directory.
//!
//! A path with **no provider hit is an error**, not a warning: by [`LoadReport`]'s own definition
//! errors are "things that dropped a frame or a handler", and an unresolved `<Include>` drops a
//! whole document while an unresolved `<Script file=>` drops every handler in it. It was a warning
//! until 1186, and the cost was that an addon which resolved *nothing* reported success — Bagnon
//! missed all eleven of its references and came back with zero errors. The load still continues
//! (0068: the client logs and carries on); only the reporting changed.
//!
//! ## MAXCSTACK discipline (decision 0068, probe A)
//!
//! Like the host it drives, the loader holds **no** breadth-wise accumulation of Lua handles: a
//! frame's wrapper `Table` and its (optional) `OnLoad` `Function` live only on the Rust stack across
//! *that frame's own* subtree build (depth-bounded), and region wrappers are dropped as soon as their
//! visuals are applied. Everything durable lives Lua-side in the host's registry.

use std::collections::{HashMap, HashSet};

use mlua::{Function, ObjectLike, Table, Value};

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
/// `files` is the engine-free seam (see module docs): it resolves a FrameXML/Lua path to its
/// **bytes**. Returning `None` yields a warning, not an error.
pub fn load(
    script: &UiScript,
    doc: &ParsedDocument,
    files: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> LoadReport {
    load_in(script, doc, "", files)
}

/// [`load`] for a document that does **not** sit at the provider's root.
///
/// `path` is the document's **own path** in the provider's path space (`/`-separated; `""` for a
/// document with no path, which only tests have). Its directory is what every `<Include>`/`<Script
/// file=>` inside resolves against, and each nested include resolves against *its* own directory in
/// turn — so an addon whose manifest lists `src\main.xml` passes `"<Addon>/src/main.xml"` here and
/// its `<Include file="templates.xml">` reaches `<Addon>/src/templates.xml` (decision 1186).
///
/// **The path, not the directory, because the loader has to be able to say where a raise came
/// from.** Every `<Script>` chunk is named after the file carrying it; an unnamed chunk takes
/// mlua's `#[track_caller]` default, which is *this file's* Rust source line, and 26 of the corpus
/// survey's 70 readable failures pointed at `crates/benilla-ui/src/loader/mod.rs` instead of the
/// addon (decision 1217). Three callers used to derive this directory themselves with the same
/// three lines; they pass the path now and [`dir_of`] does it once.
pub fn load_in(
    script: &UiScript,
    doc: &ParsedDocument,
    path: &str,
    files: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> LoadReport {
    load_into(script.lua(), doc, path, files)
}

/// [`load_in`] against the VM directly, for a caller that holds `&Lua` rather than `&UiScript` —
/// which is every Lua binding, and therefore `LoadAddOn` (1188 phase 2). Identical behaviour;
/// `load_in` is the same call with the script's own VM.
pub fn load_into(
    lua: &mlua::Lua,
    doc: &ParsedDocument,
    path: &str,
    files: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> LoadReport {
    let mut loader = Loader {
        lua,
        files,
        path: path.to_string(),
        report: LoadReport::default(),
        warned: HashSet::new(),
    };
    // Fold the document layer's own parse warnings in (decision 0068: reuse framexml's warnings).
    loader.report.warnings.extend(doc.warnings.iter().cloned());
    loader.load_doc(doc);
    loader.report
}

/// Apply a **template** to a frame that already exists — the runtime half of `inherits=`.
///
/// This is the fourth argument of `CreateFrame(kind, name, parent, "UIPanelButtonTemplate")`, and
/// it is the single most common way an addon builds anything. `wrapper` is the frame the binding
/// has just created, `kind` is the kind string it was given, and `template` is the raw — possibly
/// comma-separated — `inherits=` list. Returns the messages worth surfacing (warnings first, then
/// the errors that dropped something); the caller decides where they go. Nothing here can fail
/// the call.
///
/// **The caller's own name wins, and that is the load-bearing part.** `CreateFrame("Button",
/// "MyButton", p, "SomeTemplate")` is a frame named `MyButton`, so the template's `$parentText`
/// resolves to `MyButtonText` — which is the global the addon's very next line calls `getglobal`
/// on. A template whose children came out named after the *template* would be useless.
///
/// **The frame is never dropped, whatever the template turns out to be.** Four cases, each a
/// message and a usable frame:
/// - an **unknown** name — the reference creates the frame anyway, so we do;
/// - a name whose element was **not `virtual="true"`** — only virtual elements are ever registered
///   ([`Loader::load_doc`]), so from the registry's side it is indistinguishable from unknown, and
///   it reads as unknown;
/// - a **region** template (a virtual `<Texture>`/`<FontString>`) where a frame was asked for —
///   its frame-shaped content (`<Size>`, `<Anchors>`) still applies, its region-only content
///   cannot, and it says so;
/// - a **kind that disagrees with the template's tag** — the frame already exists as the kind
///   `CreateFrame` was given and no template can retype it, so [`framexml::inherits_node`] keeps
///   the caller's kind and the per-kind passes skip what would not apply. Named, not pretended.
///
/// **Why this cannot simply call [`Loader::materialize`]:** materialize's step 1 *is*
/// `CreateFrame`, and this runs from inside that binding — it would recurse forever. The steps
/// after it are [`Loader::decorate`], which is what both entry points share.
///
/// Reachable from a Lua binding at all only because decision 1191 §4 made the loader `&Lua`-native.
pub fn apply_template(lua: &mlua::Lua, wrapper: &Table, kind: &str, template: &str) -> Vec<String> {
    let template = template.trim();
    if template.is_empty() {
        return Vec::new();
    }
    // A template is a node in the registry, never a file: `<Include>`/`<Script file=>` are
    // top-level-only, so nothing reachable under `decorate` can ask for bytes.
    let no_files = |_: &str| -> Option<Vec<u8>> { None };
    let mut loader = Loader {
        lua,
        files: &no_files,
        path: String::new(),
        report: LoadReport::default(),
        warned: HashSet::new(),
    };

    let (own_name, parent_name) = loader.frame_names(wrapper);
    let self_name = own_name.clone().unwrap_or_else(|| parent_name.clone());
    let dbg = format!(
        "CreateFrame(\"{kind}\", \"{}\", inherits=\"{template}\")",
        own_name.as_deref().unwrap_or("<unnamed>")
    );

    // The one thing `expand` cannot tell us: what kind each named template was *declared* as. Read
    // it straight off the registry — a diagnostic pass only; the resolution below is still the
    // document layer's single implementation.
    let mut notes = Vec::new();
    let mut any_resolved = false;
    {
        let model = loader.model();
        let templates = model.framexml_templates.borrow();
        // One name, verbatim, folded — the same law `framexml::expand` resolves under, kept in
        // step so this diagnostic cannot disagree with the resolution it describes.
        for name in [template].into_iter().filter(|s| !s.is_empty()) {
            // A name that is missing entirely is `expand`'s warning to give, just below.
            let Some(el) = templates.get(name).or_else(|| {
                templates
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v)
            }) else {
                continue;
            };
            any_resolved = true;
            let tag = &el.tag;
            if tag.eq_ignore_ascii_case("Texture") || tag.eq_ignore_ascii_case("FontString") {
                notes.push(format!(
                    "{dbg}: '{name}' is a <{tag}> REGION template, not a frame template; its \
                     frame-shaped content still applies, its region-only content cannot"
                ));
            } else if !tag.eq_ignore_ascii_case(kind) {
                notes.push(format!(
                    "{dbg}: '{name}' is a <{tag}>, but a {kind} was created; the frame keeps the \
                     kind CreateFrame was given, so the <{tag}>-only parts of the template do not \
                     apply"
                ));
            }
        }
    }
    if !any_resolved {
        notes.push(format!(
            "{dbg}: no template of that name is registered — the frame exists and is usable, but \
             it is bare (only a `virtual=\"true\"` element is ever registered as a template)"
        ));
    }
    loader.report.warnings.extend(notes);

    // One expansion, through the document layer's own splice/chain/cycle path — the same call the
    // XML `inherits=` path makes. Its warnings are terse by design (they were written for a
    // document walk), so prefix the ones this call produced: a log line has to name the
    // `CreateFrame` it came from or nobody can act on it.
    let first = loader.report.warnings.len();
    let expanded = loader.expand(&framexml::inherits_node(kind, template));
    for w in &mut loader.report.warnings[first..] {
        *w = format!("{dbg}: {w}");
    }

    loader.decorate(&expanded, wrapper, &self_name, &parent_name, &dbg);

    let mut out = loader.report.warnings;
    out.extend(loader.report.errors);
    out
}

/// Join a FrameXML reference against the directory of the file that contains it, **lexically**.
///
/// FrameXML paths are `\`- or `/`-separated and may walk up with `..` — that is how a shared
/// library addon is reached (`..\..\BagBrother\core\core.xml`). Resolution here touches no
/// filesystem and follows no symlink: it is pure string arithmetic, so the provider is the only
/// thing that decides what a resolved path may reach.
///
/// A `..` that walks above the root is **kept** as a leading `..` rather than clamped, so the
/// provider can see the escape and refuse it instead of silently being handed a path that looks
/// contained.
pub fn join_ref(base: &str, path: &str) -> String {
    let path = path.replace('\\', "/");
    let combined = if path.starts_with('/') || base.is_empty() {
        path.trim_start_matches('/').to_string()
    } else {
        format!("{base}/{path}")
    };
    let mut out: Vec<&str> = Vec::new();
    for seg in combined.split('/') {
        match seg {
            "" | "." => {}
            ".." => match out.last() {
                Some(&"..") | None => out.push(".."),
                _ => {
                    out.pop();
                }
            },
            s => out.push(s),
        }
    }
    out.join("/")
}

/// The `.lua` test `0x6ede10` opens with: `strrchr(path, '.')` on the **whole resolved path**,
/// then a **case-insensitive** compare against `".lua"` (`0x8710c8`, through `0x64a4c0`).
///
/// Deliberately the last dot anywhere in the path rather than the basename's, because that is what
/// `strrchr` does — a directory carrying a dot and a file carrying none would take the same branch
/// in the client as it does here.
fn has_lua_suffix(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("lua"))
}

/// The directory part of a provider path — `""` for a bare name.
fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

struct Loader<'a> {
    /// **The VM itself, not a `UiScript`** — every piece of state this loader touches
    /// (the two FrameXML registries, the font objects, the chunk runner) is reachable from a bare
    /// `&Lua` through [`Model`]. That is what lets `LoadAddOn` load an addon from inside a Lua
    /// binding, synchronously, the way the reference does (1188 phase 2): a binding is handed
    /// `&Lua` and can never obtain the `&UiScript` that owns it.
    pub(super) lua: &'a mlua::Lua,
    pub(super) files: &'a dyn Fn(&str) -> Option<Vec<u8>>,
    /// The current document's own path in the provider's path space. Its **directory**
    /// ([`Loader::base`]) is what a relative `<Include>`/`<Script file=>` inside it resolves
    /// against; its **whole path** is what names an inline `<Script>`'s chunk. Saved and restored
    /// around each nested include, so depth-1 and depth-5 both resolve — and report — against
    /// their own file.
    pub(super) path: String,
    // The template + font-element registries live on the `Model` (`framexml_templates` /
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
        self.lua
    }

    /// The current document's own directory — what a relative `<Include>`/`<Script file=>` inside
    /// it resolves against.
    fn base(&self) -> &str {
        dir_of(&self.path)
    }

    /// Run a chunk in the one global state — `UiScript::run`'s body, reachable from `&Lua`.
    ///
    /// Takes **bytes**, because a `<Script file=>` chunk is whatever is on disk (decision 1193) and
    /// Lua 5.0 strings are byte strings. An inline `<Script>` body arrives as `&str` from the XML
    /// and is passed through as its own bytes, which is the same thing.
    ///
    /// `path` names the chunk, and **naming it is not cosmetic**: mlua's `load` is `#[track_caller]`
    /// and an unnamed chunk is named after the Rust line that loaded it, so every raise from every
    /// FrameXML and addon `<Script>` block in this project reported `loader/mod.rs` as its source
    /// (decision 1217). Lua's leading `@` is what makes it a *file* name rather than a quoted
    /// source snippet, and the separator is `\` because that is the shape an addon parsing its own
    /// `debugstack()` matches against (`crate::script::addon_chunk_name`).
    fn run(&self, chunk: &[u8], path: &str) -> mlua::Result<()> {
        self.lua
            .load(chunk)
            .set_name(format!("@{}", path.replace('/', "\\")))
            .set_mode(mlua::ChunkMode::Text)
            .exec()
    }

    /// The model, for the two FrameXML registries below.
    fn model(&self) -> mlua::AppDataRefMut<'_, crate::script::Model> {
        self.lua
            .app_data_mut::<crate::script::Model>()
            .expect("model app_data")
    }

    /// Run a widget handler with the client's calling convention — `UiScript::invoke_handler`'s
    /// body, reachable from `&Lua`.
    pub(super) fn invoke_handler(
        &self,
        wrapper: &mlua::Table,
        func: &mlua::Function,
    ) -> mlua::Result<()> {
        crate::script::event::invoke_with_globals(self.lua, wrapper.clone(), func, None, Vec::new())
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
                // **A DELIBERATE DIVERGENCE, and a strict superset of the reference.**
                //
                // `<Script file=X>` does NOT share `<Include>`'s path law. `0x6ee070`-`0x6ee079`
                // tests X itself for a separator and, when it has one, uses X **verbatim** — no
                // `dirname(referrer)` prefix, and no `..` collapse either, since that arm never
                // enters `0x6ede10`. Only a *bare* name gets `dirname(referrer) + name`.
                //
                // A verbatim value cannot open on 1.12. Resolution runs through `0x647e60` against
                // a hash index whose key is each file's path **relative to the scan root**, and the
                // root is the process CWD (Storm's base-path global `0xc52418` has no writer
                // image-wide, so `0x646ebc` substitutes `"."`). The basename retry at `0x647ed3` is
                // real code but off — it needs a flag bit that `0x648be0(0)` clears at startup. So
                // `Libs\Ace\Ace.lua` misses every leg: full-string, base-prefixed and the MPQ
                // chain. There is no per-addon or per-document current directory anywhere in the
                // client. (wow-re `scratch/include-lua-dispatch.md` §4.1.)
                //
                // We resolve it against the including document's directory anyway. That converts a
                // guaranteed failure into a success and **cannot break anything that worked on the
                // reference**, because under 1.12 there is no install-root `Libs\` for the verbatim
                // value to have found. The corpus addons carrying the form are TBC-era anyway —
                // all three report `iface=20400` and ship LibStub/CallbackHandler-1.0, none of
                // which existed in 1.12 — so reproducing the failure would buy nothing and cost
                // `FuBar_AtlasFu`, which works today.
                TopLevel::Script(ScriptRef::File(path)) => {
                    let joined = join_ref(self.base(), path);
                    match (self.files)(&joined) {
                        Some(bytes) => {
                            if let Err(e) = self.run(crate::source::chunk(&bytes), &joined) {
                                self.report
                                    .errors
                                    .push(format!("<Script file=\"{path}\">: {e}"));
                            }
                        }
                        None => self.report.errors.push(format!(
                            "<Script file=\"{path}\">: no provider hit for \"{joined}\"; \
                             every handler in it is missing"
                        )),
                    }
                }
                TopLevel::Script(ScriptRef::Inline { body, line }) => {
                    // Padded to the block's own offset in the XML, so Lua counts from where the
                    // file does. A chunk always starts at line 1; naming it after the file without
                    // this would make every reported line a confident, checkable lie.
                    let mut chunk = "\n".repeat(line.saturating_sub(1) as usize).into_bytes();
                    chunk.extend_from_slice(body.as_bytes());
                    let path = self.path.clone();
                    if let Err(e) = self.run(&chunk, &path) {
                        self.report.errors.push(format!("inline <Script>: {e}"));
                    }
                }
                TopLevel::Font(el) => self.do_font(el),
                TopLevel::Template(el) => {
                    // Register the raw (un-expanded) node; `expand` chain-resolves any `inherits` it
                    // carries at instantiation time. An unnamed virtual node was already warned at
                    // parse time and cannot be inherited, so it is simply not registered.
                    if let Some(name) = el.name() {
                        self.model()
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
        let joined = join_ref(self.base(), path);
        let Some(bytes) = (self.files)(&joined) else {
            self.report.errors.push(format!(
                "<Include file=\"{path}\">: no provider hit for \"{joined}\"; the whole \
                 document it names is missing"
            ));
            return;
        };
        // **`<Include>` dispatches on the EXTENSION, and a `.lua` target is RUN, not parsed.**
        //
        // `0x6ede10` is not "the XML loader" — it is *load one file by path*, and its first act
        // after the `..` collapse is a case-insensitive `.lua` suffix test on the **resolved** path
        // (`0x6edee6`-`0x6edf0f`: `strrchr(path,'.')`, then a case-insensitive compare against
        // `".lua"` @`0x8710c8`). Extension, never a content sniff — no byte of the file has been
        // read at that point. A match runs the chunk (`0x6edf0f call 0x704bc0`, chunk name
        // `"@<resolved path>"` via `0x8716e0`); anything else is opened and parsed as XML.
        //
        // `<Include file=X>` is a **recursion into that same routine** (`0x6ee00d`), so it inherits
        // the dispatch for free: there is no `<Include>`-specific Lua code in the client at all,
        // and it is the same mechanism that makes a `.lua` line in a `.toc` work. Which is also the
        // internal evidence that this was ours and not the corpus's — our own `.toc` loader has
        // always split on extension (`ui_script::addons`, 1185), and only this arm never did.
        //
        // Three addons ship the form — FonzAppraiser (67 sites), AckisRecipeList (24), FonzSummon
        // (22): an authoring habit rather than a convention, but each lost its ENTIRE library set
        // here, because `embeds.xml` is nothing but these lines. (wow-re
        // `scratch/include-lua-dispatch.md`, §5-verified.)
        if has_lua_suffix(&joined) {
            if let Err(e) = self.run(crate::source::chunk(&bytes), &joined) {
                self.report
                    .errors
                    .push(format!("<Include file=\"{path}\">: {e}"));
            }
            return;
        }
        // An included document is the one place text is forced: roxmltree parses `&str` (1193).
        match framexml::parse(&crate::source::decode(&bytes)) {
            Ok(sub) => {
                self.report.warnings.extend(sub.warnings.iter().cloned());
                // The included document's OWN path is what ITS relative references resolve against
                // and what ITS inline scripts are named after — restored after, so a sibling
                // include at this level is unaffected.
                let outer = std::mem::replace(&mut self.path, joined.clone());
                self.load_doc(&sub);
                self.path = outer;
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
        let model = self.model();
        let templates = model.framexml_templates.borrow();
        let view: HashMap<&str, &Element> =
            templates.iter().map(|(k, v)| (k.as_str(), v)).collect();
        let mut warns = Vec::new();
        let out = framexml::expand(el, &view, &mut warns);
        drop(view);
        drop(templates);
        drop(model);
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
            let model = self.model();
            let templates = model.framexml_templates.borrow();
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
            let model = self.model();
            let fonts = model.framexml_fonts.borrow();
            let view: HashMap<&str, &Element> =
                fonts.iter().map(|(k, v)| (k.as_str(), v)).collect();
            let mut warns = Vec::new();
            let merged = framexml::expand(el, &view, &mut warns);
            drop(view);
            drop(fonts);
            drop(model);
            self.report.warnings.extend(warns);
            merged
        };

        let font = font_object_from_element(&merged);
        // One model borrow, then the insert — `font_objects` and `framexml_fonts` are both on it.
        {
            let mut model = self.model();
            model
                .font_objects_by_lower
                .insert(name.to_ascii_lowercase(), font);
            // Store the merged (flattened) node so a chain rooted here reads resolved values.
            model
                .framexml_fonts
                .borrow_mut()
                .insert(name.clone(), merged);
        }
        // …and publish `_G[name]` as a real font OBJECT. In 1.12 a `<Font name="GameFontNormal">`
        // is not only a style record: it is addressable Lua, and that is how the whole addon
        // ecosystem paints text — `fs:SetFontObject(GameFontNormal)` is 3,180 of the corpus's 3,186
        // call sites, against 6 that pass a string. Registering the record without ever publishing
        // the name is why `Gratuity-2.0.lua:57` was handed nil and took five addons down at load.
        // The object and its method surface are `crate::script::font`.
        //
        // Flattening is untouched: the merge above still resolves the `inherits=` chain once, at
        // load, and publishing only gives the result a name.
        if let Err(e) = crate::script::font::publish(self.lua(), &name) {
            self.report
                .warnings
                .push(format!("<Font name=\"{name}\">: {e}"));
        }
    }

    /// Materialize one (already template-expanded) frame element into a live frame, then recurse its
    /// nested `<Frames>` and fire its `OnLoad` **after** them (bottom-up, rf26).
    ///
    /// `parent` is the enclosing frame's wrapper (`None` at top level). `parent_name` is the
    /// already-resolved name of the nearest named ancestor (or `"Top"`), used to substitute `$parent`
    /// in this frame's name and in its anchors' `relativeTo` (rf27).
    /// Returns the created frame's wrapper — `None` when `CreateFrame` refused (an unknown frame
    /// type, rf24 `0x6ee280`'s factory-table miss), which also skips the whole subtree. The
    /// `<ScrollChild>` pass is the one caller that needs the handle back.
    pub(super) fn materialize(
        &mut self,
        el: &Element,
        parent: Option<&Table>,
        parent_name: &str,
    ) -> Option<Table> {
        // ─ Step 1 and ONLY step 1 lives here: resolve the name and call CreateFrame. Everything
        //   after it is `decorate`, because the runtime template path enters at exactly that seam
        //   (`apply_template`) — its frame already exists, and re-entering here would recurse.
        // Name (with $parent substitution) — the resolved name is what CreateFrame publishes and what
        // this frame's own children substitute `$parent` against.
        let resolved_name: Option<String> = el
            .name()
            .map(|raw| framexml::resolve_name(raw, parent_name));

        // 1a · `parent="SomeFrame"` — the OTHER way an element names its parent, and the one the
        //      reference's own FrameXML uses most, because its files are flat: `<Frame name="X"
        //      parent="UIParent">` at top level rather than nested inside `<Frames>`. We only ever
        //      read the lexical parent, so **708 corpus sites across 79 addons** produced a frame
        //      with `GetParent() == nil` — no visibility inheritance, no scale or alpha
        //      inheritance, and `this:GetParent():Hide()` raising on a nil. Nothing warned, because
        //      an ignored attribute is not an error: 1205's silent-drop class again.
        //
        //      The attribute WINS over the lexical parent when both are present, which is the
        //      reference's rule and is what lets an addon nest an element for authoring
        //      convenience and still attach it elsewhere. An unresolvable name warns and falls back
        //      to the lexical parent rather than erroring — the frame is real and usable either
        //      way, and 0068's log-and-continue is the house rule for a name lookup.
        let attr_parent: Option<Table> = el.attr("parent").and_then(|raw| {
            let name = framexml::resolve_name(raw, parent_name);
            // A FRAME, not merely a global of that name. `_G` is one namespace: an addon's own
            // `MyAddon = {}` sits in it beside every frame, and the corpus really does write
            // `parent="TheoryCraft"` where the addon owns that name. Handing a plain table to
            // `CreateFrame` would raise and take the element's whole subtree with it — a name
            // collision costing a window. The identity test is RF-0023's own: a frame wrapper
            // carries its handle at `T[0]` as lightuserdata, and nothing else does.
            let hit = self
                .lua()
                .globals()
                .get::<Table>(name.as_str())
                .ok()
                .filter(|t| matches!(t.raw_get::<Value>(0), Ok(Value::LightUserData(_))));
            if hit.is_none() {
                self.report.warnings.push(format!(
                    "{}: parent=\"{name}\" names no frame — falling back to the enclosing one",
                    resolved_name.as_deref().unwrap_or(&el.tag)
                ));
            }
            hit
        });
        // `$parent` in THIS element's own anchors means its parent — so when the attribute supplied
        // one, that is the name to substitute against (rf27's rule, applied to rf27's other input).
        let attr_parent_name = attr_parent.as_ref().and_then(|_| {
            el.attr("parent")
                .map(|raw| framexml::resolve_name(raw, parent_name))
        });
        let parent_name = attr_parent_name.as_deref().unwrap_or(parent_name);
        let parent = attr_parent.as_ref().or(parent);

        // 1 · CreateFrame(kind = element tag, name, parent). An unknown frame type errors here (the
        //     factory-table miss, rf24 `0x6ee280`) — record it and skip the whole subtree.
        let create: Function = match self.lua().globals().get("CreateFrame") {
            Ok(f) => f,
            Err(e) => {
                self.report
                    .errors
                    .push(format!("CreateFrame global missing: {e}"));
                return None;
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
                    return None;
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

        self.decorate(el, &wrapper, &self_name, parent_name, &dbg_name);
        Some(wrapper)
    }

    /// Everything a frame element does to a frame that **already exists**: LoadXML attributes,
    /// `<Size>`, `<Anchors>`, `<Layers>` regions, the per-kind extras, `<Scripts>`, the nested
    /// `<Frames>` pass, and finally this frame's own `OnLoad` (bottom-up, rf26).
    ///
    /// Split out of [`Self::materialize`] because materialize's *first* step is `CreateFrame`, and
    /// the runtime template path — [`apply_template`], i.e. `CreateFrame`'s own fourth argument —
    /// is called from **inside** that binding. It cannot re-enter materialize without recursing
    /// forever; it enters here instead, and the two paths share every step that follows.
    ///
    /// The two names are not the same thing and the difference is the load-bearing one (rf27):
    /// `self_name` is what `$parent` means to this frame's **contents** (its regions, its nested
    /// frames), `parent_name` is what `$parent` means to this frame's **own** anchors.
    pub(super) fn decorate(
        &mut self,
        el: &Element,
        wrapper: &Table,
        self_name: &str,
        parent_name: &str,
        dbg_name: &str,
    ) {
        // 2 · LoadXML attributes (rf24 `0x769820`).
        self.apply_attrs(el, wrapper, dbg_name);
        // 3 · <Size> and 4 · <Anchors> (the CLayoutFrame geometry base, rf24 `0x767800`).
        self.apply_size(el, wrapper, dbg_name);
        self.apply_anchors(el, wrapper, parent_name, dbg_name);
        // 5 · <Layers> regions (rf24 `0x769d70`) — `$parent` in a region name is *this* frame.
        self.apply_layers(el, wrapper, self_name, dbg_name);
        // 5·b — a frame's **direct-child** `<FontString>` (outside `<Layers>`): the engine's special
        // font string (a ScrollingMessageFrame's line font, an EditBox's text font). Created on
        // OVERLAY so its resolved font object (ChatFontNormal — face/height/shadow) is what the
        // frame's line/text rendering reads. See [`Self::apply_special_fontstrings`].
        self.apply_special_fontstrings(el, wrapper, self_name, dbg_name);
        // 5a · <Backdrop> plate (rf24 LoadXML `0x77e6c0`): the tiled bg + 8-piece border.
        self.apply_backdrop(el, wrapper, dbg_name);
        // 5b · per-kind LoadXML extras (RF-28's typed tables) — StatusBar + Button/CheckButton;
        //      the EditBox flags/caps (RF-0082). Every one of these gates on the element's own tag,
        //      which is why `apply_template` builds its synthetic node with the kind CreateFrame was
        //      given: a Frame asked to wear a <Button> template simply skips the Button-only steps
        //      instead of calling SetNormalTexture on something that has no such method.
        self.apply_statusbar(el, wrapper, dbg_name);
        self.apply_slider(el, wrapper, self_name, dbg_name);
        self.apply_button(el, wrapper, self_name, dbg_name);
        self.apply_editbox(el, wrapper, dbg_name);
        self.apply_messageframe(el, wrapper, dbg_name);
        self.apply_simplehtml(el, wrapper, dbg_name);
        // 6 · <Scripts> handlers (rf24 `0x769ef0`); OnLoad is captured to fire bottom-up below.
        let onload = self.apply_scripts(el, wrapper, dbg_name);

        // 7 · nested <Frames> — build children (firing THEIR OnLoads) before ours (rf26). A child's
        //     `$parent` (name and anchors) resolves against this frame's name.
        for frames_el in children_named(el, "Frames") {
            for child in &frames_el.children {
                let expanded = self.expand(child);
                self.materialize(&expanded, Some(wrapper), self_name);
            }
        }

        // 7b · `<ScrollChild>` — the ScrollFrame's panning content (wow-5875-re
        //      `rf28-typed-widget-loadxml.md`: "its single child frame is instantiated via
        //      `0x6ee280`(childNode, this, status)" — the same RF-0026 path `<Frames>` uses — then
        //      "stored +0x318, flag +0x314=1", which is `SetScrollChild`).
        //
        //      **This is what gives a ScrollFrame its scroll range**, and without it
        //      `FauxScrollFrameTemplate` — the template five corpus addons instantiate and 34 more
        //      reach through `FauxScrollFrame_Update` — produces a frame whose range is 0, whose
        //      `SetVerticalScroll` clamps to 0, and whose `OnVerticalScroll` therefore fires with
        //      `arg1 = 0` forever. The list never scrolls and nothing errors.
        //
        //      An **empty** `<ScrollChild>` is an error in the reference, so it is one here.
        for sc in children_named(el, "ScrollChild") {
            let Some(child) = sc.children.first() else {
                self.report.errors.push(format!(
                    "{dbg_name}: <ScrollChild> is empty — the reference errors here, and a \
                     ScrollFrame with no child has no scroll range at all"
                ));
                continue;
            };
            let expanded = self.expand(child);
            let Some(child_wrapper) = self.materialize(&expanded, Some(wrapper), self_name) else {
                continue; // materialize already reported why
            };
            if let Err(e) = wrapper.call_method::<()>("SetScrollChild", child_wrapper) {
                self.report
                    .errors
                    .push(format!("{dbg_name}: <ScrollChild>: {e}"));
            }
        }

        // 8 · this frame's OnLoad, now that its subtree is complete (bottom-up).
        if let Some(func) = onload {
            self.fire_onload(wrapper, &func, dbg_name);
        }
    }

    /// The two names a decoration needs, read back from the frame itself through the **public
    /// object model** (`GetName`/`GetParent`) rather than the arena — the discipline this module
    /// holds to everywhere else (see the module docs). Used by [`apply_template`], whose caller
    /// hands it a wrapper and nothing else.
    ///
    /// Returns the frame's own name (`None` if it is anonymous) and the name of its nearest
    /// **named** ancestor — rf27 rule 3's walk, with [`framexml::DEFAULT_PARENT_NAME`] when there
    /// is none.
    fn frame_names(&self, wrapper: &Table) -> (Option<String>, String) {
        let own = wrapper
            .call_method::<Option<String>>("GetName", ())
            .ok()
            .flatten();
        let mut cur = wrapper.clone();
        // Bounded: the arena's parent chain is a tree, and a walk that somehow is not must still
        // return rather than hang a load.
        for _ in 0..64 {
            let Ok(Some(parent)) = cur.call_method::<Option<Table>>("GetParent", ()) else {
                break;
            };
            if let Ok(Some(name)) = parent.call_method::<Option<String>>("GetName", ()) {
                return (own, name);
            }
            cur = parent;
        }
        (own, framexml::DEFAULT_PARENT_NAME.to_string())
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
