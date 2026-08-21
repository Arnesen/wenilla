//! **Where interfaces come from** — the addon folder, and the walk that loads what is in it
//! (decision 1184, building 1178 step 2).
//!
//! 1178 step 1 made benilla's own interface load from a `.toc`. This is the other half of the
//! claim: the same parser, the same loader, and the same walk now run over folders that are not
//! ours. An [`Addon`] is a name, a parsed manifest, and a [`Source`] its files come from — and
//! benilla's own interface is simply the one whose source is the compiled-in tree.
//!
//! ## Where we look — ONE folder
//!
//! **`<benilla-config>/AddOns/<Name>/<Name>.toc`**, and nothing else (decision 1185). 1184 also
//! searched the WoW install's own `Interface/AddOns/` and merged the two; the director's call is
//! one root, changeable in future *by choice* but never both at once. Resolved through
//! [`crate::local_state`], the only module allowed to compute that path (0954), which is already
//! `None` under `$WOW_CAPTURE` — so a capture discovers nothing and a deterministic baseline
//! cannot depend on what somebody has installed (0008).
//!
//! What the install's folder actually holds, checked rather than assumed: twelve `Blizzard_*`
//! folders containing **only a `.pub` signature file** — their real code is inside the MPQs, which
//! we do not read for addons at all — plus whatever the machine's owner has put there. Nothing was
//! being gained by searching it.
//!
//! ## Two kinds of file in a manifest
//!
//! A `.toc` lists `.lua` **and** `.xml`, and they load differently: Lua is executed as a chunk in
//! the shared global state, FrameXML is parsed and materialized. See [`Addon::load_files`].
//!
//! ## The load order is the reference's, not a topological sort
//!
//! `AddOn_Load 0x51f240` is **recursive**, and wow-5875-re has it byte-verified
//! (`system/ui/ui.md`, the SavedVariables §5 quad): for each addon, in order —
//! **OptionalDeps first (failures ignored)** → **RequiredDeps (a failure ABORTS this addon's
//! load)** → **its own `.toc`-listed files, in listed order**. We match that shape, because a
//! pre-sorted flat list cannot express "a missing hard dependency drops exactly this addon and
//! nothing else".
//!
//! **Our own interface is not an addon for lifecycle purposes.** The same source records that
//! *FrameXML does not go through `AddOn_Load`, so it gets no `ADDON_LOADED`* — and `benilla.toc`
//! is excluded exactly as FrameXML is. Structurally, not by a filter: it loads through
//! [`super::manifest::load_ingame_ui`], while [`Walk`] only ever walks what [`discover`] found.
//! It shares the loader; it does not share the event.
//!
//! ## Per-addon `Bindings.xml`
//!
//! Loaded right after the `.toc` files and before the saved variables (`0x51f400`) — 1188 phase 4,
//! in [`Walk::load`]. Parsed by [`benilla_ui::bindings_xml`] and registered into the same
//! key-binding table the Key Bindings window edits, so an addon's binding is a row like any
//! other; the app's dispatch runs its Lua body ([`crate::bindings`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bevy::prelude::*;

use benilla_ui::script::{ScriptValue, UiScript};
use benilla_ui::toc::Toc;

use super::content;

/// The folder name under each root, and the reference's own spelling.
const ADDONS_DIR: &str = "AddOns";

/// The world-entry load's VM instruction bound, per addon (decision 1306) — and the addon
/// harness's survey bound, one number for both so the measurement stays shared. MEASURED, not
/// guessed (e463649e): across the 218-addon corpus, 214 addons execute under 1M VM instructions
/// and the heaviest legitimate one (Enchantrix) reaches 4M — this is ~50x that. An addon that
/// crosses it is not slow, it is not coming back; the raise fails that addon with a distinctive
/// message and the walk loads everyone else, where before 1306 the client sat frozen on the
/// loading screen with zero diagnostics (B271's class).
pub(crate) const LOAD_INSTRUCTION_BUDGET: u64 = 200_000_000;

/// Where one addon's files come from.
///
/// The two arms are the same question asked of two stores, and keeping them one enum is what lets
/// [`Addon::load`] be written once: our own interface and a third-party folder differ in where the
/// bytes live, not in how they load.
enum Source {
    /// benilla's own interface — the compiled-in tree, which a dev build shadows with `assets/ui`
    /// on disk so editing FrameXML costs no recompile ([`content`], 1175).
    Builtin,
    /// **The AddOns root**, not this addon's own folder (decision 1186). Paths handed to
    /// [`Addon::read`] are relative to it, so `Bagnon/src/main.xml` and the
    /// `BagBrother/core/core.xml` it includes are both expressible — which is the point, since a
    /// shared library addon exists to be reached from its dependents.
    Dir(PathBuf),
}

/// One loadable interface: a name, its parsed manifest, and where its files come from.
pub(super) struct Addon {
    /// The addon's folder name — and, for the builtin, `"benilla"`. This is what
    /// `GetAddOnInfo`/`IsAddOnLoaded` will key on (1188 phase 2) and what `ADDON_LOADED` carries.
    pub(super) name: String,
    /// The parsed `.toc`: the ordered file list plus every directive.
    pub(super) toc: Toc,
    source: Source,
}

impl Addon {
    /// benilla's own interface. Always present — it is in the binary (1175), so unlike a
    /// discovered addon it cannot be missing.
    pub(super) fn builtin() -> Self {
        let toc = content::read(super::manifest::MANIFEST)
            .map(|t| Toc::parse(&t))
            .unwrap_or_else(|| {
                error!(
                    "ui_script: {} is not in the shipped UI — no interface will load",
                    super::manifest::MANIFEST
                );
                Toc::default()
            });
        Addon {
            name: "benilla".to_string(),
            toc,
            source: Source::Builtin,
        }
    }

    /// The text of one file, by a path already resolved into the **source's** path space — the
    /// AddOns root for a `Dir`, the flat shipped tree for the builtin.
    ///
    /// The caller does the relative-path arithmetic ([`benilla_ui::loader::join_ref`]); this only
    /// reads. That split is decision 1186's: only the loader knows the include tree, and only the
    /// source knows what a resolved path is allowed to reach.
    ///
    /// **The sandbox is the AddOns folder, not one addon's subfolder.** `..` that walks above the
    /// root survives `join_ref` as a leading `..`, and [`read_under`] refuses it — so an addon can
    /// reach a sibling library addon (which is how `Bagnon` reaches `BagBrother`, and what 1184's
    /// per-addon guard wrongly blocked) but cannot reach the machine.
    ///
    /// The **builtin alone** keeps a basename fallback: its tree is flat, and a transcription that
    /// writes a Blizzard-style directory path should still find the file. A `Dir` source must not
    /// have it — a basename fallback silently rescues exactly the escaping path the guard just
    /// refused.
    /// Returns **bytes**, not text (decision 1193). A `.lua` chunk is handed to Lua as it sits on
    /// disk, and only an XML/`.toc` parse decodes — because a `read_to_string` here did not make a
    /// cp1252 locale file lose a glyph, it made the file *not exist*.
    fn read(&self, req: &str) -> Option<Vec<u8>> {
        match &self.source {
            Source::Builtin => {
                let norm = req.replace('\\', "/");
                let base = norm.rsplit('/').next().unwrap_or(&norm).to_string();
                content::read(&norm)
                    .or_else(|| content::read(&base))
                    .map(String::into_bytes)
            }
            Source::Dir(root) => read_under(root, req),
        }
    }

    /// This addon's own folder in its source's path space — the `base` its manifest entries are
    /// relative to. `""` for the builtin's flat tree, the addon's folder name for a `Dir`.
    fn prefix(&self) -> &str {
        match &self.source {
            Source::Builtin => "",
            Source::Dir(_) => &self.name,
        }
    }

    /// Load this addon's `.toc`-listed files into the VM, in listed order. Returns per-file
    /// errors, each tagged `"<Addon>/<file>: <error>"`.
    ///
    /// Errors are logged as they happen *and* returned: the app ignores the value (it has already
    /// been logged) while the tests assert it empty, which is the shape 1178 step 1 established
    /// for the builtin and the reason a broken entry can no longer reach a run behind a log line.
    fn load(&self, script: &UiScript) -> Vec<String> {
        self.load_files(script, &self.toc.files)
    }

    /// [`Addon::load`] over an explicit slice — the builtin's two-phase boot split (1051) is the
    /// only caller that needs less than the whole manifest.
    ///
    /// **A manifest lists two kinds of file and they load differently** (decision 1185). `.lua` is
    /// executed as a chunk in the shared global state; anything else is FrameXML — parsed, then
    /// materialized. 1184 sent every entry through the XML parser, which was invisible only
    /// because `benilla.toc` happens to list nothing but `.xml`: the reference's own `FrameXML.toc`
    /// opens with `GlobalStrings.lua`, and a bare Lua file is not a document.
    pub(super) fn load_files(&self, script: &UiScript, files: &[String]) -> Vec<String> {
        let mut failures = Vec::new();
        // The `<Include>` / `<Script file=>` seam. The loader hands us paths it has already
        // resolved against the including file's directory (1186), so this only reads — and the
        // sandbox lives in `read`, at the source, rather than being spread over call sites.
        let provider = |req: &str| -> Option<Vec<u8>> { self.read(req) };
        for file in files {
            // A manifest entry is relative to the addon's own folder; `read` and the loader both
            // work in the source's path space, so resolve once here and use it for both.
            let path = benilla_ui::loader::join_ref(self.prefix(), file);
            let Some(bytes) = self.read(&path) else {
                let e = format!("{}/{file}: not found", self.name);
                // Severity follows whose manifest lied. For the builtin that is us — a client
                // bug, and the boot tests assert none. For a player's addon it is the package
                // (the director's AtlasLoot copy lists `Bossnames\BossNames.xml`; no such folder
                // ships in it), and the reference client silently skips a missing toc entry — so
                // a broken addon must not read as a client ERROR, which is gate-fatal to
                // `smoke.sh`'s zero-ERROR count (1450).
                match self.source {
                    Source::Builtin => error!("ui_script: {e}"),
                    Source::Dir(_) => warn!("ui_script: {e}"),
                }
                // Retained where the player can read it (1495). This is the single commonest way
                // an addon "doesn't work" with nothing on screen — the director's own AtlasLoot
                // copy lists `Bossnames\BossNames.xml` and ships no such folder.
                script.report_load_failure(&e);
                failures.push(e);
                continue;
            };
            if is_lua(file) {
                // The same execution `<Script file=>` gets (`loader::mod.rs`): one chunk, run in
                // the one global state, in manifest order. The reference does exactly this —
                // `AddOn_Load 0x51f240` hands each listed file to `0x6edb90` regardless of kind.
                match script.run_chunk_named(
                    &bytes,
                    &benilla_ui::script::addon_chunk_name(&self.name, file),
                ) {
                    Ok(()) => info!("ui_script: {}/{file} ran", self.name),
                    Err(e) => {
                        let e = format!("{}/{file}: {e}", self.name);
                        error!("ui_script: {e}");
                        // A file-scope failure is a script error, and script errors reach the
                        // player (1305): queued for the Lua error handler — `_ERRORMESSAGE`'s
                        // red dialog once BasicControls has run — never only a terminal line.
                        script.report_script_error(&e);
                        failures.push(e);
                    }
                }
                continue;
            }
            let doc = match benilla_ui::framexml::parse(&benilla_ui::source::decode(&bytes)) {
                Ok(d) => d,
                Err(e) => {
                    let e = format!("{}/{file}: {e}", self.name);
                    error!("ui_script: parsing {e}");
                    // Still log-only as far as the *dialog* goes — the reference answers an
                    // unparseable document with a FrameXML.log line and silence, and 1495 does not
                    // change that. What it changes is that the silence is no longer total.
                    script.report_load_failure(&e);
                    failures.push(e);
                    continue;
                }
            };
            // The document's OWN directory is what its relative references resolve against — a
            // manifest listing `src\main.xml` means `<Include file="templates.xml">` inside it is
            // `<Addon>/src/templates.xml`, and `..\..\Lib\lib.xml` is a sibling addon (1186). The
            // loader takes the path and does that itself, so it can also name the file a raise
            // came from (1217).
            let report = benilla_ui::loader::load_in(script, &doc, &path, &provider);
            for w in &report.warnings {
                warn!("ui_script({}/{file}): {w}", self.name);
            }
            for e in &report.errors {
                error!("ui_script({}/{file}): {e}", self.name);
                // Same contract as the Lua arm above: a `<Script file=>` chunk that raised or an
                // OnLoad that errored is a script error the player gets to see (1305).
                // `_ERRORMESSAGE`'s own IsVisible guard shows a burst's FIRST failure only,
                // which is the reference's behaviour too. Document *parse* failures stay
                // log-only, like the reference's FrameXML.log.
                script.report_script_error(&format!("{}/{file}: {e}", self.name));
                failures.push(format!("{}/{file}: {e}", self.name));
            }
            info!(
                "ui_script: {}/{file} loaded ({} frames materialized)",
                self.name, report.frames
            );
        }
        failures
    }
}

/// Is this manifest entry a Lua chunk rather than a FrameXML document?
///
/// By extension, case-insensitively, and `\` is a path separator like `/` — a `.toc` is written
/// for a Windows client and `Libs\LibStub\LibStub.lua` is the normal spelling.
fn is_lua(entry: &str) -> bool {
    entry
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(entry)
        .rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("lua"))
}

/// `root/rel`, refusing to escape `root`.
///
/// `rel` has already been lexically resolved by [`benilla_ui::loader::join_ref`], so a path that
/// stays inside is free of `.`/`..` and one that escapes carries a **leading `..`** — which is
/// exactly what this rejects, along with anything absolute or non-UTF-8-normal. The check is
/// lexical and happens before any filesystem call, so a symlinked addon folder still works and a
/// traversal never opens a file at all.
///
/// `root` is the **AddOns folder** (1186), so a sibling addon is reachable and the machine is not.
fn read_under(root: &Path, rel: &str) -> Option<Vec<u8>> {
    let rel = Path::new(rel);
    if rel
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    std::fs::read(root.join(rel)).ok()
}

/// **The** addon folder — `<benilla-config>/AddOns/` — or `None` when there is none to read.
///
/// **One root, never two** (decision 1185, the director's call). 1184 searched the WoW install's
/// `Interface/AddOns/` as well and merged the results. Two roots means an addon's identity depends
/// on which folder won, a name can be shadowed, and "where is this addon loaded from" stops having
/// one answer. If the install's folder ever becomes reachable it will be *instead of* this one and
/// by explicit choice, not alongside it.
///
/// It is ours for the same reason 1180 moved our state out of the install: benilla reads a WoW
/// install, it does not live in one. Resolved through [`crate::local_state`], the only module
/// allowed to compute that path (0954) — which is already `None` under `$WOW_CAPTURE`, so a
/// deterministic baseline cannot depend on what somebody has installed (0008).
///
/// `pub(crate)` since 1322: the texture side needs the same folder — the sprite decoder's
/// `Interface\AddOns\` loose-file resolve and the `SetTexture` probe both map that virtual prefix
/// onto this root (`ui_script::lifecycle::install_texture_resolvers`), so there is exactly one
/// answer to "where do addons live" however the question is asked.
pub(crate) fn root() -> Option<PathBuf> {
    root_from(crate::local_state::home())
}

/// [`root`] with its one environment fact passed in, so it is testable without touching the
/// process environment.
///
/// Not tidiness: `install::candidates_from` exists for exactly this reason (1175's own flake — a
/// test that *sets* an env var poisons every other test running concurrently in the same process,
/// and the failure then moves around with scheduling).
fn root_from(home: Option<PathBuf>) -> Option<PathBuf> {
    home.map(|h| h.join(ADDONS_DIR))
}

/// Every third-party addon we can see, in load order: alphabetical by folder name.
///
/// Alphabetical is **our** choice, not a fidelity claim — the reference enumerates its own record
/// list and we have not established that order. What matters is that it is deterministic, so two
/// runs on one machine load the same interface. Real ordering constraints between addons are
/// expressed by `## Dependencies:` and honoured by [`load_third_party`]'s recursion, which is the
/// mechanism the reference actually uses.
fn discover() -> Vec<Addon> {
    let Some(root) = root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new(); // no addon folder is the normal case, not an error
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|name| {
            let dir = root.join(&name);
            // A folder with no `<Name>.toc` is not an addon. The reference ignores these too (a
            // stray `Backup/` folder is the common case), so this is not a warning.
            let text = manifest_in(&dir, &name)?;
            Some(Addon {
                name,
                toc: Toc::parse(&text),
                // The ROOT, not `dir` — every path this addon resolves is relative to the AddOns
                // folder so a sibling library addon is reachable (1186).
                source: Source::Dir(root.clone()),
            })
        })
        .collect()
}

/// `<dir>/<name>.toc`, matched case-insensitively.
///
/// The reference is a Windows client on a case-insensitive filesystem, so a folder named `MyAddon`
/// containing `myaddon.toc` is an addon there and has to be one here — on Linux an exact-name
/// probe would silently not find it.
/// A `.toc` that is not valid UTF-8 is still a manifest (decision 1193): before it was decoded
/// rather than `read_to_string`'d, five addons in a 218-addon corpus were **invisible to
/// discovery** — not broken, not reported, simply not addons — because a German `## Notes:` line
/// held one cp1252 byte.
fn manifest_in(dir: &Path, name: &str) -> Option<String> {
    let want = format!("{name}.toc");
    let entry = std::fs::read_dir(dir).ok()?.flatten().find(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|f| f.eq_ignore_ascii_case(&want))
    })?;
    let bytes = std::fs::read(entry.path()).ok()?;
    Some(benilla_ui::source::decode(&bytes).into_owned())
}

/// Build the AddOn API's registry row for one discovered addon — every `.toc` directive the
/// eleven verbs can be asked about, read once here rather than re-parsed per call.
fn info_for(addon: &Addon) -> benilla_ui::script::AddOnInfo {
    info_from_toc(&addon.name, &addon.toc)
}

/// [`info_for`]'s body, over the two things it actually reads.
///
/// Split out because `addon_harness` needs the same row and must not build its own: a survey that
/// seats a DIFFERENT registry shape from production is measuring a client nobody runs, which is the
/// fault 1193 was written to fix. One converter, two callers — the harness's own `Toc` goes through
/// this, so a directive added here reaches the survey for free.
pub(crate) fn info_from_toc(name: &str, toc: &Toc) -> benilla_ui::script::AddOnInfo {
    benilla_ui::script::AddOnInfo {
        name: name.to_owned(),
        title: toc.directive("Title").map(str::to_owned),
        notes: toc.directive("Notes").map(str::to_owned),
        url: toc.directive("URL").map(str::to_owned),
        // `## Secure: 1` is Blizzard's own marker; nothing a player installs carries it honestly,
        // and we do not treat it as granting anything — it only picks the glue's icon.
        secure: toc.directive("Secure").map(str::trim) == Some("1"),
        load_on_demand: toc.load_on_demand(),
        dependencies: toc.dependencies().into_iter().map(str::to_owned).collect(),
        directives: toc.directives.clone(),
        files: toc.files.clone(),
        saved_variables: toc
            .list("SavedVariables")
            .into_iter()
            .map(str::to_owned)
            .collect(),
        saved_variables_per_character: toc
            .list("SavedVariablesPerCharacter")
            .into_iter()
            .map(str::to_owned)
            .collect(),
        enabled: true, // an addon nobody has disabled is enabled; the file below overrides
        saved_enabled: true, // re-stamped from `enabled` at registration — see `register_addons`
        loaded: false,
        // The version gate's dword (decision 1292) — the client's own parse: leading integer,
        // 0 when the manifest is silent (and 0 is out of date, not "unknown").
        interface: toc.interface_version(),
    }
}

/// Parse the enable-state file — one `<AddOnName>: enabled|disabled` per line, the reference's own
/// `AddOns.txt` format (confirmed against a real 1.12 install, not remembered).
///
/// Unknown names are ignored rather than dropped-and-rewritten: a file listing an addon that is
/// not installed right now belongs to an addon that will be again, and silently forgetting the
/// player's choice on every uninstall is the behaviour nobody wants. Only `disabled` disables — a
/// malformed line leaves the addon enabled, which is the safe direction.
fn parse_enable_state(text: &str) -> Vec<(String, bool)> {
    text.lines()
        .filter_map(|line| {
            let (name, state) = line.split_once(':')?;
            let name = name.trim();
            (!name.is_empty()).then(|| {
                (
                    name.to_string(),
                    !state.trim().eq_ignore_ascii_case("disabled"),
                )
            })
        })
        .collect()
}

/// Render the enable state back out in the reference's format.
fn render_enable_state(states: &[(String, bool)]) -> String {
    let mut out = String::new();
    for (name, enabled) in states {
        out.push_str(name);
        out.push_str(if *enabled {
            ": enabled\n"
        } else {
            ": disabled\n"
        });
    }
    out
}

/// Where this character's enable state lives, or `None` with no identity yet / no install.
pub(crate) fn enable_state_path(identity: Option<&(String, String)>) -> Option<PathBuf> {
    let (realm, character) = identity?;
    crate::local_state::addons_state_path(realm, character)
}

/// One installed addon as the **AddOns screens** need it — no VM, no load (decision 1197).
///
/// The glue screen runs before any UI VM has addons in it (`load_third_party` is a world-entry
/// step), so the AddOns list cannot ask `GetAddOnInfo`. It asks the same *folder* instead, through
/// the same discovery and the same manifest parser, so the two views cannot disagree about what is
/// installed. Same shape as the reference's `GetAddOnInfo` return, minus what only a live session
/// knows (`loaded`).
#[derive(Clone, Debug)]
pub(crate) struct InstalledAddOn {
    pub(crate) name: String,
    pub(crate) title: Option<String>,
    pub(crate) notes: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) dependencies: Vec<String>,
    /// `## Interface` as the version gate reads it (decision 1292): the leading integer, `0`
    /// when absent — and now ENFORCED by the load walk when `checkAddonVersion` is on,
    /// superseding 1191 §6's report-only interim (the RE answer it was waiting for landed:
    /// wow-re `addon-version-gate.md`).
    pub(crate) interface: u32,
    /// `## LoadOnDemand: 1` — shown as a status hint rather than a checkbox state, because a
    /// LoadOnDemand addon is not "off", it is waiting for a `LoadAddOn` call (1191 §6).
    /// Read by the char-select AddOns screen's rows (`char_select::addons`).
    pub(crate) load_on_demand: bool,
    /// From this character's enable file; an addon the file never mentions is **enabled** (1191 §7).
    pub(crate) enabled: bool,
}

impl InstalledAddOn {
    /// The list's display name — `## Title` when it has one, else the folder name. The reference's
    /// own `if (title) … else name` (`AddonList_Update`).
    ///
    /// (An `out_of_date()` convenience lived here too until 1293 moved the glue screen onto the
    /// gate — the version compare has exactly one home now, `addon_gate::can_load`'s check 6,
    /// and a screen-side copy is the per-asker drift 1292 §Rejected names.)
    pub(crate) fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.name)
    }
}

/// Every installed addon, in load order, with `character`'s enable state applied.
///
/// The AddOns screens' whole data source. `identity` is `(realm, character)`; `None` reads the
/// folder with nobody's enable file, which is the "no character picked yet" case and shows
/// everything as enabled.
pub(crate) fn installed(identity: Option<&(String, String)>) -> Vec<InstalledAddOn> {
    let disabled = disabled_set(identity);
    discover()
        .into_iter()
        .map(|addon| InstalledAddOn {
            enabled: !disabled.contains(&addon.name.to_ascii_lowercase()),
            title: addon.toc.directive("Title").map(str::to_owned),
            notes: addon.toc.directive("Notes").map(str::to_owned),
            url: addon.toc.directive("URL").map(str::to_owned),
            dependencies: addon
                .toc
                .dependencies()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            interface: addon.toc.interface_version(),
            load_on_demand: addon.toc.load_on_demand(),
            name: addon.name,
        })
        .collect()
}

/// The lowercased names this character has turned off.
fn disabled_set(identity: Option<&(String, String)>) -> HashSet<String> {
    enable_state_path(identity)
        .and_then(|p| std::fs::read(p).ok())
        .map(|b| {
            parse_enable_state(&benilla_ui::source::decode(&b))
                .into_iter()
                .filter(|(_, on)| !on)
                .map(|(n, _)| n.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

/// Write a character's enable state from the AddOns screen (decision 1197).
///
/// **Merges rather than replaces**, for the reason [`parse_enable_state`] documents: a name in the
/// file that is not installed right now belongs to an addon that will be again, and rewriting the
/// file from the installed list alone would forget the player's choice on every uninstall.
pub(crate) fn write_enable_state(identity: Option<&(String, String)>, states: &[(String, bool)]) {
    let Some(path) = enable_state_path(identity) else {
        return; // no character picked, or no state folder — nothing to write to
    };
    let mut merged: Vec<(String, bool)> = std::fs::read(&path)
        .ok()
        .map(|b| parse_enable_state(&benilla_ui::source::decode(&b)))
        .unwrap_or_default();
    for (name, on) in states {
        match merged
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
        {
            Some(row) => row.1 = *on,
            None => merged.push((name.clone(), *on)),
        }
    }
    match crate::local_state::write_atomic(&path, &render_enable_state(&merged)) {
        Ok(()) => info!("addons: wrote {} ({} rows)", path.display(), merged.len()),
        Err(e) => warn!("addons: cannot write {}: {e}", path.display()),
    }
}

/// Write the enable state back — the reference's own last shutdown step (`0x490bd0`'s tail,
/// after the saved-variables files), so a `DisableAddOn` from Lua survives the session.
pub(super) fn save_enable_state(script: &UiScript, identity: Option<&(String, String)>) {
    let states = script.addon_enable_states();
    if states.is_empty() {
        return; // nothing was ever registered — a glue-only run or a capture; an empty write is a wipe
    }
    let Some(path) = enable_state_path(identity) else {
        return;
    };
    match crate::local_state::write_atomic(&path, &render_enable_state(&states)) {
        Ok(()) => info!(
            "ui_script: wrote {} ({} addons)",
            path.display(),
            states.len()
        ),
        Err(e) => warn!("ui_script: cannot write {}: {e}", path.display()),
    }
}

/// Write every loaded addon's declared saved variables — the reference's own shutdown step
/// (`0x490c83`, right after the flat file and just before `AddOns.txt`).
///
/// **There is no autosave and no dirty bit**, deliberately: the reference has neither (`ds:0xb4b3f4`
/// has three references image-wide, and the write gate is the record's *loaded* byte). An addon
/// that never loaded this session has no globals to write, and writing it would blank a file it
/// never read.
///
/// An addon declaring nothing gets no file. The reference *deletes* one in that case; we simply do
/// not write, because our folder is a place a player looks and a stale file there is confusing
/// either way — recorded rather than silently different.
pub(super) fn save_addon_variables(script: &mut UiScript, identity: Option<&(String, String)>) {
    let account = crate::local_state::addon_saved_account_dir();
    let character = identity.and_then(|(r, c)| crate::local_state::addon_saved_character_dir(r, c));
    for (name, account_names, character_names) in script.addon_saved_variable_sets() {
        for (dir, names) in [(&account, &account_names), (&character, &character_names)] {
            if names.is_empty() {
                continue;
            }
            let Some(dir) = dir else { continue };
            let body = script.saved_variables_text_for(names);
            let path = dir.join(format!("{name}.lua"));
            match crate::local_state::write_atomic(&path, &format!("{SAVED_HEADER}{body}")) {
                Ok(()) => info!("ui_script: wrote {}", path.display()),
                Err(e) => warn!("ui_script: cannot write {}: {e}", path.display()),
            }
        }
    }
    for w in script.take_warnings() {
        warn!("ui_script: saved variables: {w}");
    }
}

/// The per-addon file's header. The reference writes none (its files open with a bare blank line
/// from the `"\r\n"`-per-variable prefix); ours says what the file is, because a visible folder
/// invites a look — the same call [`crate::ui_saved`] made for the flat file.
const SAVED_HEADER: &str = "\
-- benilla per-addon saved variables (decision 1188 phase 3).
-- Written at logout/exit from the live globals; executed as a Lua chunk at addon load.
";

/// Load every discovered third-party addon, honouring `## Dependencies:` the way the reference
/// does. Returns every load error, tagged by addon.
///
/// Called at world entry, after the built-in interface's own files — the reference loads addons
/// from `UI_Init 0x48fbf0` → `0x51f600`, which is the same seam our in-game UI materializes at
/// (1051).
///
/// **`&mut` is what lets `ADDON_LOADED` fire in the right place.** [`UiScript::fire_event`] needs
/// it, and the alternative — collecting names and firing them all after the walk — is a different
/// behaviour, not a shortcut: the reference fires each addon's event before the *next* addon's
/// files run, so a deferred batch would let B's file-scope code run ahead of A's handler.
pub(super) fn load_third_party(
    script: &mut UiScript,
    identity: Option<&(String, String)>,
    version_check: bool,
) -> Vec<String> {
    let addons = discover();
    // Register the AddOn API's view even when the list is empty: `GetNumAddOns()` must answer 0
    // rather than answer whatever a previous session left, and an addon that asks before any
    // exist is asking a real question.
    let mut infos: Vec<_> = addons.iter().map(info_for).collect();
    if let Some(text) = enable_state_path(identity).and_then(|p| std::fs::read_to_string(p).ok()) {
        for (name, enabled) in parse_enable_state(&text) {
            if let Some(i) = infos
                .iter_mut()
                .find(|i| i.name.eq_ignore_ascii_case(&name))
            {
                i.enabled = enabled;
            }
        }
    }
    let disabled_names: HashSet<String> = infos
        .iter()
        .filter(|i| !i.enabled)
        .map(|i| i.name.to_ascii_lowercase())
        .collect();
    script.register_addons(
        infos,
        root(),
        crate::local_state::addon_saved_account_dir(),
        identity.and_then(|(r, c)| crate::local_state::addon_saved_character_dir(r, c)),
    );
    if addons.is_empty() {
        return Vec::new();
    }
    info!(
        "ui_script: {} addon(s) found: {}",
        addons.len(),
        addons
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut state = Walk {
        disabled: disabled_names,
        version_check,
        ..Walk::default()
    };
    for addon in &addons {
        // `## LoadOnDemand: 1` means "not at startup" — the reference's `0x51f600` loads only
        // records whose LoadOnDemand byte is 0. Without `LoadAddOn()` (1178 step 4) that means
        // never, which is why it is said out loud rather than silently skipped.
        if addon.toc.load_on_demand() {
            info!(
                "ui_script: {} is LoadOnDemand — not loaded (no LoadAddOn() yet)",
                addon.name
            );
            continue;
        }
        // Re-arm the load bound PER ADDON (decision 1306): the budget is a per-addon fact (the
        // corpus was measured per addon), and without the reset one runaway would spend the
        // whole allowance and fail every addon after it for somebody else's loop. A dependency
        // chain loads under its dependent's arming, which is the same accounting the harness's
        // per-survey arming gives it.
        script.set_instruction_budget(LOAD_INSTRUCTION_BUDGET);
        let _ = state.load(script, &addons, &addon.name);
        let spent = script.instructions_used();
        if spent > 1_000_000 {
            // Heavy is worth a line: this counter is how the budget was chosen, and a report
            // like B271's needs exactly this number without a harness run.
            info!(
                "ui_script: {} spent {spent} VM instructions loading",
                addon.name
            );
        }
    }
    state.failures
}

/// The recursive load's bookkeeping — see [`Walk::load`].
#[derive(Default)]
struct Walk {
    loaded: HashSet<String>,
    /// Addons whose load already failed, so a second dependent gets the same answer without
    /// re-running (and without re-reporting) the failure.
    failed: HashSet<String>,
    /// The current dependency chain, for cycle detection and for naming the cycle when it happens.
    loading: Vec<String>,
    failures: Vec<String>,
    /// Addons the player has turned off, lowercased. **Passed in rather than read back out of the
    /// VM**: the walk's inputs stay explicit, so recursion cannot depend on registration having
    /// happened first, and an empty set means "everything enabled" — which is exactly what an
    /// absent enable-state file means too.
    disabled: HashSet<String>,
    /// The `checkAddonVersion` gate (decision 1292), resolved by the caller from the persisted
    /// CVar — passed in for the same explicitness reason as `disabled`. When on, an addon whose
    /// `## Interface` is not exactly the client's is skipped like a disabled one (the
    /// reference's `AddOn_CanLoad` check 6, before the dependency loop), and a dependent gets
    /// `Err` — its `DEP_INTERFACE_VERSION`.
    version_check: bool,
}

impl Walk {
    /// Load one addon by name, its dependencies first. `Err` means "this addon did not load", and
    /// is what a hard dependency's failure propagates.
    ///
    /// The order inside is `AddOn_Load 0x51f240`'s, byte-verified in wow-5875-re (`system/ui/ui.md`):
    ///
    /// > OptionalDeps (failures ignored) → RequiredDeps (a failure aborts) → **this addon's own
    /// > `.toc`-listed files, in listed order** (`0x51f3fa`) → `Bindings.xml` (`0x51f400`) → the
    /// > account SavedVariables file (`0x51f4b5`) → the per-character file (`0x51f53b`) →
    /// > **`ADDON_LOADED` (event 429) at `0x51f5ad`** → the reverse-`LoadWith` dependents.
    ///
    /// All of it is built (`Bindings.xml` is 1188 phase 4, the two saved files phase 3), and the
    /// two middle steps are why the event is fired at the very end rather than beside the load.
    /// That position is the mechanism, not a detail: a saved value overwrites the addon's own
    /// file-scope default, and `ADDON_LOADED` handlers are specified to see the *restored* value.
    fn load(&mut self, script: &mut UiScript, all: &[Addon], name: &str) -> Result<(), ()> {
        // Resolve to the addon's OWN spelling before anything is keyed on it. A `.toc` may name a
        // dependency in any case (`## Dependencies: probeaddon` against a folder `ProbeAddon`),
        // and lookup here is case-insensitive — so keying the sets on the caller's spelling would
        // let two differently-cased dependents each miss the "already loaded" check and load the
        // same addon twice, running every one of its files a second time.
        let Some(addon) = all.iter().find(|a| a.name.eq_ignore_ascii_case(name)) else {
            return Err(()); // not installed — the caller decides whether that is fatal
        };
        let key = addon.name.as_str();
        if self.loaded.contains(key) {
            return Ok(());
        }
        if self.failed.contains(key) {
            return Err(());
        }
        // **Disabled is a player's choice, not a failure** — it is skipped silently and nothing is
        // pushed onto `failures`. A dependent still gets `Err`, which is the reference's own
        // `DEP_DISABLED`: an addon whose hard dependency the player turned off cannot load either.
        if self.disabled.contains(&key.to_ascii_lowercase()) {
            info!("ui_script: {key} is disabled — not loaded");
            self.failed.insert(addon.name.clone());
            return Err(());
        }
        // **The version gate** (decision 1292; `AddOn_CanLoad` check 6, in the checks' own order —
        // after the enable state, before the dependency loop). Exact `==` against the client's
        // build; a missing `## Interface` parses as 0 and is out of date. Not a `failures` entry
        // for the same reason disabled is not: the state is the player's to see on the AddOns
        // screens (`ADDON_INTERFACE_VERSION`), and the *Load out of date AddOns* checkbox — the
        // `checkAddonVersion` CVar this flag carries — is the reference's own escape.
        if self.version_check
            && addon.toc.interface_version() != benilla_ui::script::addon_gate::CLIENT_INTERFACE
        {
            info!(
                "ui_script: {key} is out of date (## Interface: {}, client {}) — not loaded \
                 (the AddOns screen's 'Load out of date AddOns' loads it anyway)",
                addon.toc.interface_version(),
                benilla_ui::script::addon_gate::CLIENT_INTERFACE
            );
            self.failed.insert(addon.name.clone());
            return Err(());
        }
        if self.loading.iter().any(|n| n == key) {
            let chain = self.loading.join(" → ");
            let e = format!("{key}: dependency cycle ({chain} → {key})");
            error!("ui_script: {e}");
            script.report_load_failure(&e);
            self.failures.push(e);
            self.failed.insert(addon.name.clone());
            return Err(());
        }
        self.loading.push(addon.name.clone());

        // Optional dependencies first, and a failure is genuinely ignored — that is what makes
        // them optional. It still orders the load when the dependency IS present.
        for dep in addon.toc.optional_dependencies() {
            let _ = self.load(script, all, dep);
        }
        // Hard dependencies. A failure aborts THIS addon and nothing else.
        let mut blocked = None;
        for dep in addon.toc.dependencies() {
            if self.load(script, all, dep).is_err() {
                blocked = Some(dep.to_string());
                break;
            }
        }

        self.loading.pop();
        if let Some(dep) = blocked {
            let e = format!(
                "{}: required dependency {dep} is missing or failed",
                addon.name
            );
            error!("ui_script: {e}");
            // The one failure a player can usually FIX themselves — install the dependency — and
            // until 1495 the only place it was said was the terminal.
            script.report_load_failure(&e);
            self.failures.push(e);
            self.failed.insert(addon.name.clone());
            return Err(());
        }

        self.failures.extend(addon.load(script));
        // ── `Bindings.xml` (1188 phase 4) ── the verified position `0x51f400`: after this addon's
        // own files (whose functions a binding body calls), before its saved variables. Read
        // through the addon's own reader, so the AddOns-root sandbox (1186) covers it like every
        // other file it loads; absent is the normal case and silent.
        let bindings_xml = benilla_ui::loader::join_ref(addon.prefix(), "Bindings.xml");
        if let Some(bytes) = addon.read(&bindings_xml) {
            match benilla_ui::bindings_xml::parse(&benilla_ui::source::decode(&bytes)) {
                Ok(bindings) => script.register_addon_bindings(&addon.name, &bindings),
                Err(e) => {
                    let e = format!("{}/Bindings.xml: {e}", addon.name);
                    error!("ui_script: {e}");
                    script.report_load_failure(&e);
                    self.failures.push(e);
                }
            }
        }
        // The two saved-variables files, account then per-character — the verified position
        // (`0x51f4b5`, `0x51f53b`): after the addon's own files assigned their defaults, before
        // `ADDON_LOADED` whose handlers must see the restored value.
        script.load_addon_saved_variables(&addon.name);
        self.loaded.insert(addon.name.clone());
        script.mark_addon_loaded(&addon.name);
        // `ADDON_LOADED`, `arg1` = the addon's own folder name — the spelling the addon knows
        // itself by, not the caller's (a dependent may name it in any case). Marked loaded first,
        // because a handler is free to ask `IsAddOnLoaded` about itself and the honest answer is
        // yes: the reference sets `[rec+0x18]` before `0x51f5ad`.
        //
        // **The builtin cannot reach this**, which is the rule rather than a filter: FrameXML does
        // not go through `AddOn_Load` and gets no `ADDON_LOADED` (same source), and our
        // `benilla.toc` is loaded by `manifest::load_ingame_ui` while this walk only ever sees
        // what `discover()` found under the AddOns root.
        script.fire_event("ADDON_LOADED", vec![ScriptValue::Str(addon.name.clone())]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an addon over a temp AddOns root, so the walk can be tested without an install.
    fn dir_addon(root: &Path, name: &str, toc: &str) -> Addon {
        let folder = root.join(name);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(format!("{name}.toc")), toc).unwrap();
        Addon {
            name: name.to_string(),
            toc: Toc::parse(toc),
            source: Source::Dir(root.to_path_buf()),
        }
    }

    /// **The sandbox is the AddOns folder** (decision 1186): a sibling addon is reachable, the
    /// machine is not.
    ///
    /// 1184 drew the line at each addon's own folder, which reads as the safer choice and breaks
    /// the single most common structure in the ecosystem — a shared library addon exists precisely
    /// to be included by its dependents (`Bagnon/src/main.xml` reaches
    /// `..\..\BagBrother\core\core.xml`, and BagBrother's own `.toc` lists no files at all).
    ///
    /// The escape cases go through [`benilla_ui::loader::join_ref`] first, exactly as the loader
    /// feeds them, because that is what turns an escape into the leading `..` the guard sees.
    #[test]
    fn the_sandbox_is_the_addons_root_not_one_addon() {
        use benilla_ui::loader::join_ref;
        let tmp =
            std::env::temp_dir().join(format!("benilla-addon-escape-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("AddOns");
        std::fs::create_dir_all(root.join("Probe/src")).unwrap();
        std::fs::create_dir_all(root.join("ProbeLib/core")).unwrap();
        std::fs::write(tmp.join("secret.txt"), "no").unwrap();
        std::fs::write(root.join("Probe/src/own.txt"), "yes").unwrap();
        std::fs::write(root.join("ProbeLib/core/lib.xml"), "sibling").unwrap();
        let addon = Addon {
            name: "Probe".into(),
            toc: Toc::default(),
            source: Source::Dir(root),
        };

        // Its own file, and a sibling library addon reached the way a real addon reaches one.
        assert_eq!(
            addon.read(&join_ref("Probe/src", "own.txt")).as_deref(),
            Some(&b"yes"[..])
        );
        assert_eq!(
            addon
                .read(&join_ref("Probe/src", "..\\..\\ProbeLib\\core\\lib.xml"))
                .as_deref(),
            Some(&b"sibling"[..]),
            "a shared library addon must be reachable — this is what 1184 wrongly blocked"
        );

        // Above the AddOns root is refused: the escape survives `join_ref` as a leading `..` and
        // `read_under` will not join it.
        assert!(addon
            .read(&join_ref("Probe/src", "../../../secret.txt"))
            .is_none());
        assert!(addon.read(&join_ref("Probe", "..\\secret.txt")).is_none());
        // A leading `/` is not "the filesystem root" — it re-roots at AddOns, which is the only
        // root a FrameXML path has. So this looks for `<AddOns>/etc/hosts` and finds nothing.
        assert_eq!(join_ref("", "/etc/hosts"), "etc/hosts");
        assert!(addon.read(&join_ref("", "/etc/hosts")).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A missing hard dependency drops exactly the dependent, and an optional one drops nothing.
    ///
    /// This is the behaviour a flat topological sort cannot express, and the reason the walk is
    /// recursive like `AddOn_Load 0x51f240` rather than a pre-sort.
    #[test]
    fn a_missing_required_dep_drops_only_its_dependent() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-addon-deps-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let all = vec![
            dir_addon(&tmp, "Alone", "## Interface: 11200\n"),
            dir_addon(&tmp, "NeedsGhost", "## Dependencies: Ghost\n"),
            dir_addon(&tmp, "WantsGhost", "## OptionalDeps: Ghost\n"),
        ];
        let mut script = UiScript::new().unwrap();
        let mut w = Walk::default();
        for a in &all {
            let _ = w.load(&mut script, &all, &a.name);
        }
        assert!(w.loaded.contains("Alone"), "an independent addon loads");
        assert!(
            w.loaded.contains("WantsGhost"),
            "a MISSING OPTIONAL dependency must not block its dependent"
        );
        assert!(
            !w.loaded.contains("NeedsGhost"),
            "a missing REQUIRED dependency must block its dependent"
        );
        assert_eq!(
            w.failures.len(),
            1,
            "and reports exactly that one: {:?}",
            w.failures
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// **1178's falsifier, as far as step 2 can carry it**: a real third-party addon — a `.toc`, a
    /// frame in XML, a second file reached by `<Include>`, and Lua that calls a client-API global
    /// our own interface uses — loads from a folder, with no Rust written for it.
    ///
    /// This is the check the unit tests above structurally cannot give. They exercise the walk
    /// with empty manifests; this one goes through `framexml::parse` → `loader::load` → the VM, so
    /// it is the first thing that would fail if per-addon file scoping, the `<Include>` provider,
    /// or the shared global namespace were wrong. `ADDON_LOADED` is covered by
    /// [`addon_loaded_carries_the_addons_own_name_and_fires_after_its_files`]; what remains of the
    /// falsifier is saved variables (1188 phase 3).
    #[test]
    fn a_third_party_addon_loads_from_a_folder_with_no_rust() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-addon-e2e-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dir = tmp.join("ProbeAddon");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(tmp.join("ProbeLib/core")).unwrap();
        // **Bagnon's shape, deliberately** — the manifest names ONE file in a subfolder with a
        // backslash path, and that file reaches a sibling by bare name and a shared library addon
        // by `..\..`. Every part of this is what the real addon does, and every part of it failed
        // before 1186.
        std::fs::write(
            dir.join("ProbeAddon.toc"),
            "## Interface: 11200\n## Title: Probe\nsrc\\main.xml\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("src/main.xml"),
            r#"<Ui>
  <Include file="templates.xml"/>
  <Include file="..\..\ProbeLib\core\lib.xml"/>
  <Script file="core.lua"/>
  <Frame name="ProbeAddonFrame" parent="UIParent">
    <Size><AbsDimension x="100" y="50"/></Size>
    <Anchors><Anchor point="CENTER"/></Anchors>
    <Scripts><OnLoad>ProbeAddonLoaded = GetTime() ~= nil and ProbeAddonGreeting == 'hello'</OnLoad></Scripts>
  </Frame>
</Ui>"#,
        )
        .unwrap();
        std::fs::write(dir.join("src/core.lua"), "ProbeAddonGreeting = 'hello'\n").unwrap();
        std::fs::write(
            dir.join("src/templates.xml"),
            "<Ui><Script>ProbeAddonInclude = true</Script></Ui>",
        )
        .unwrap();
        // The sibling library addon, reached across the AddOns root — and it includes a file of
        // its OWN by bare name, so the base has to follow the include down a level rather than
        // staying on the includer's.
        std::fs::write(
            tmp.join("ProbeLib/core/lib.xml"),
            "<Ui><Include file=\"deep.xml\"/><Script>ProbeLibLoaded = true</Script></Ui>",
        )
        .unwrap();
        std::fs::write(
            tmp.join("ProbeLib/core/deep.xml"),
            "<Ui><Script>ProbeLibDeep = true</Script></Ui>",
        )
        .unwrap();

        let addon = Addon {
            name: "ProbeAddon".into(),
            toc: Toc::parse(&std::fs::read_to_string(dir.join("ProbeAddon.toc")).unwrap()),
            source: Source::Dir(tmp.clone()),
        };
        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        let failures = addon.load(&script);
        assert!(failures.is_empty(), "addon load errors: {failures:#?}");

        assert_eq!(
            script.eval::<bool>("return ProbeAddonInclude == true").ok(),
            Some(true),
            "a bare-name <Include> resolved against the INCLUDING FILE's directory (src/), not \
             the addon root — Bagnon's `templates.xml` missed entirely before 1186"
        );
        assert_eq!(
            script.eval::<bool>("return ProbeLibLoaded == true").ok(),
            Some(true),
            "`..\\..\\ProbeLib\\core\\lib.xml` reached a SIBLING addon — the shared-library \
             pattern 1184's per-addon sandbox blocked"
        );
        assert_eq!(
            script.eval::<bool>("return ProbeLibDeep == true").ok(),
            Some(true),
            "and the sibling's own bare-name <Include> resolved against ITS folder, so the base \
             follows the include tree down rather than staying on the includer"
        );
        assert_eq!(
            script
                .eval::<bool>("return ProbeAddonGreeting == 'hello'")
                .ok(),
            Some(true),
            "a <Script file=> ran, resolved the same relative way as an <Include>"
        );
        assert_eq!(
            script.eval::<bool>("return ProbeAddonLoaded == true").ok(),
            Some(true),
            "the frame's OnLoad ran, reached a client-API global (GetTime), AND saw the value the \
             manifest's earlier .lua file set — so the two kinds load into one shared state, in \
             manifest order"
        );
        assert_eq!(
            script
                .eval::<bool>("return getglobal('ProbeAddonFrame') ~= nil")
                .ok(),
            Some(true),
            "the addon's frame materialized under the same global namespace ours use"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// An addon named in two different cases by two dependents loads **once**.
    ///
    /// Dependency lookup is case-insensitive (a `.toc` may spell a dependency however it likes),
    /// so the loaded/failed sets have to be keyed on the addon's own spelling. Keyed on the
    /// caller's, each differently-cased dependent misses the "already loaded" check and runs every
    /// one of the shared addon's files again — re-registering its templates and re-materializing
    /// its frames on top of themselves.
    #[test]
    fn a_dependency_named_in_another_case_loads_once() {
        let tmp = std::env::temp_dir().join(format!(
            "benilla-addon-case-key-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // Shared.xml increments a counter, so "loaded twice" is observable rather than inferred.
        let shared = tmp.join("Shared");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("Shared.toc"), "## Interface: 11200\nBump.xml\n").unwrap();
        std::fs::write(
            shared.join("Bump.xml"),
            "<Ui><Script>SharedLoads = (SharedLoads or 0) + 1</Script></Ui>",
        )
        .unwrap();
        let all = vec![
            Addon {
                name: "Shared".into(),
                toc: Toc::parse(&std::fs::read_to_string(shared.join("Shared.toc")).unwrap()),
                source: Source::Dir(tmp.clone()),
            },
            dir_addon(&tmp, "UpperDep", "## Dependencies: SHARED\n"),
            dir_addon(&tmp, "LowerDep", "## Dependencies: shared\n"),
        ];
        let mut script = UiScript::new().unwrap();
        let mut w = Walk::default();
        for a in &all {
            let _ = w.load(&mut script, &all, &a.name);
        }
        assert!(w.failures.is_empty(), "no failures: {:?}", w.failures);
        assert_eq!(
            script.eval::<i64>("return SharedLoads").ok(),
            Some(1),
            "the shared dependency's files ran exactly once"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A dependency cycle is reported once and does not recurse forever.
    #[test]
    fn a_dependency_cycle_is_caught() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-addon-cycle-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let all = vec![
            dir_addon(&tmp, "Ping", "## Dependencies: Pong\n"),
            dir_addon(&tmp, "Pong", "## Dependencies: Ping\n"),
        ];
        let mut script = UiScript::new().unwrap();
        let mut w = Walk::default();
        for a in &all {
            let _ = w.load(&mut script, &all, &a.name);
        }
        assert!(w.loaded.is_empty(), "neither side of a cycle loads");
        assert!(
            w.failures.iter().any(|f| f.contains("cycle")),
            "the cycle is named: {:?}",
            w.failures
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// There is exactly ONE addon root and it is ours (decision 1185) — and a capture has none,
    /// so a baseline cannot depend on what is installed on the machine that runs it (0008).
    ///
    /// The capture arm is `local_state::home()`'s, already hermetic under `$WOW_CAPTURE`, which is
    /// why passing `None` here IS the capture case rather than a stand-in for it.
    #[test]
    fn there_is_one_addon_root_and_it_is_ours() {
        assert_eq!(
            root_from(Some(PathBuf::from("/state"))),
            Some(PathBuf::from("/state/AddOns"))
        );
        assert_eq!(root_from(None), None, "a capture run has no addon root");
    }

    /// A manifest's `.lua` entries run as chunks; everything else is FrameXML.
    ///
    /// The classifier, not the load — `\` is a path separator in a `.toc` written for Windows, and
    /// the extension compare is case-insensitive for the same reason discovery's is.
    #[test]
    fn lua_entries_are_told_apart_from_framexml() {
        assert!(is_lua("Core.lua"));
        assert!(is_lua("Libs\\LibStub\\LibStub.LUA"));
        assert!(is_lua("deep/nested/file.Lua"));
        assert!(!is_lua("Frames.xml"));
        assert!(!is_lua("Bindings.XML"));
        assert!(!is_lua("README"));
        assert!(!is_lua("weird.lua.xml"));
    }

    /// An addon folder with no matching `.toc` is not an addon, and one whose `.toc` differs only
    /// in case still is — the reference is a case-insensitive filesystem and a real addon shipped
    /// as `MyAddon/myaddon.toc` has to load on Linux too.
    #[test]
    fn discovery_matches_the_manifest_case_insensitively() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-addon-case-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("Cased")).unwrap();
        std::fs::create_dir_all(tmp.join("Bare")).unwrap();
        std::fs::write(tmp.join("Cased/cased.TOC"), "## Interface: 11200\n").unwrap();
        std::fs::write(tmp.join("Bare/notes.txt"), "not an addon").unwrap();
        assert!(manifest_in(&tmp.join("Cased"), "Cased").is_some());
        assert!(manifest_in(&tmp.join("Bare"), "Bare").is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ───────────────────────────── lifecycle events (1188 phase 1) ─────────────────────────────

    /// A frame that appends `"<event>:<arg1>"` to a global for every event it is registered for —
    /// the witness the lifecycle tests read. Written as an addon file rather than injected into
    /// the VM, so the events travel the same `<OnEvent>` path a real addon's would.
    fn recorder_xml(events: &[&str]) -> String {
        let registers: String = events
            .iter()
            .map(|e| format!("this:RegisterEvent(\"{e}\");"))
            .collect();
        format!(
            r#"<Ui>
  <Frame name="EventProbeFrame" parent="UIParent">
    <Scripts>
      <OnLoad>{registers}</OnLoad>
      <OnEvent>table.insert(EventLog, event .. ":" .. tostring(arg1));</OnEvent>
    </Scripts>
  </Frame>
</Ui>"#
        )
    }

    /// Read the witness back as a `Vec<String>`.
    fn event_log(script: &UiScript) -> Vec<String> {
        script
            .eval::<Vec<String>>("return EventLog")
            .expect("EventLog")
    }

    /// **`ADDON_LOADED` reaches a third-party addon, carries its own name, and fires after its
    /// files** — 1188 phase 1's acceptance test, and the half of 1178's falsifier that said
    /// *"today it loads and reads; it does not receive"*.
    ///
    /// The `arg1` half is not cosmetic: every addon in the ecosystem opens with
    /// `if arg1 == "MyAddon" then` and does nothing at all otherwise, so a wrong or missing arg1
    /// is indistinguishable from the event never arriving.
    #[test]
    fn addon_loaded_carries_the_addons_own_name_and_fires_after_its_files() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-addon-loaded-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dir = tmp.join("EventProbe");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("EventProbe.toc"),
            "## Interface: 11200\nprobe.lua\nprobe.xml\n",
        )
        .unwrap();
        // File-scope Lua, which the reference runs BEFORE the event. Recording the marker here and
        // asserting it below is what proves the ordering rather than merely the delivery.
        std::fs::write(
            dir.join("probe.lua"),
            "EventLog = {}\ntable.insert(EventLog, \"files-ran\")\n",
        )
        .unwrap();
        std::fs::write(dir.join("probe.xml"), recorder_xml(&["ADDON_LOADED"])).unwrap();

        let all = vec![Addon {
            name: "EventProbe".into(),
            toc: Toc::parse(&std::fs::read_to_string(dir.join("EventProbe.toc")).unwrap()),
            source: Source::Dir(tmp.clone()),
        }];
        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        let mut w = Walk::default();
        let _ = w.load(&mut script, &all, "EventProbe");
        assert!(w.failures.is_empty(), "load errors: {:?}", w.failures);

        assert_eq!(
            event_log(&script),
            vec!["files-ran", "ADDON_LOADED:EventProbe"],
            "the addon's own files run first, THEN ADDON_LOADED with its own folder name as arg1 \
             (`AddOn_Load 0x51f240`: files 0x51f3fa, event 0x51f5ad)"
        );
    }

    /// **`benilla` never appears in an `ADDON_LOADED`.**
    ///
    /// FrameXML does not go through `AddOn_Load` and gets no such event (wow-5875-re
    /// `system/ui/ui.md`), and our own interface is FrameXML's counterpart. An addon that watches
    /// `ADDON_LOADED` to detect *another* addon would otherwise see a name no reference client
    /// ever sends.
    ///
    /// The guard is structural — the builtin loads through [`super::manifest`] and never enters
    /// [`Walk`] — so this asserts the structure holds rather than that a filter fires: it loads
    /// the builtin's own manifest entries the way production does, alongside a real walk.
    #[test]
    fn the_builtin_interface_never_fires_addon_loaded() {
        let tmp = std::env::temp_dir().join(format!(
            "benilla-addon-builtin-silent-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let dir = tmp.join("EventProbe");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("EventProbe.toc"),
            "## Interface: 11200\nprobe.xml\n",
        )
        .unwrap();
        std::fs::write(dir.join("probe.xml"), recorder_xml(&["ADDON_LOADED"])).unwrap();

        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        script.run("EventLog = {}").unwrap();

        let all = vec![Addon {
            name: "EventProbe".into(),
            toc: Toc::parse(&std::fs::read_to_string(dir.join("EventProbe.toc")).unwrap()),
            source: Source::Dir(tmp.clone()),
        }];
        let mut w = Walk::default();
        let _ = w.load(&mut script, &all, "EventProbe");
        // The builtin, loaded the way production loads it — through its own manifest, not the walk.
        let builtin = Addon::builtin();
        let _ = builtin.load_files(&script, builtin.toc.files.get(..1).unwrap_or_default());

        let log = event_log(&script);
        assert!(
            !log.iter().any(|e| e.contains("benilla")),
            "benilla must never appear in an ADDON_LOADED: {log:?}"
        );
        assert_eq!(
            log,
            vec!["ADDON_LOADED:EventProbe"],
            "exactly the one third-party addon announced itself"
        );
    }

    /// **The three UI-init events fire in the reference's order:** every addon's `ADDON_LOADED`,
    /// then `VARIABLES_LOADED`, then `PLAYER_LOGIN`.
    ///
    /// Byte-verified straight-line order inside `UI_Init 0x48fbf0` — `0x4900a3` loads the addons
    /// (each `0x51f5ad`), `0x4900b2` fires `VARIABLES_LOADED`, `0x490168` enters the cascade that
    /// fires `PLAYER_LOGIN`. This drives the production functions in the production order rather
    /// than re-stating it: [`super::load_third_party`] then [`super::super::finish_ui_load`].
    ///
    /// It is the ordering an addon depends on — restore state on `ADDON_LOADED`, and by
    /// `PLAYER_LOGIN` everything saved is in place.
    #[test]
    fn the_ui_init_events_fire_in_the_reference_order() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tmp =
            std::env::temp_dir().join(format!("benilla-addon-order-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("benilla-config");
        let dir = home.join("AddOns").join("EventProbe");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("EventProbe.toc"),
            "## Interface: 11200\nprobe.xml\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("probe.xml"),
            recorder_xml(&["ADDON_LOADED", "VARIABLES_LOADED", "PLAYER_LOGIN"]),
        )
        .unwrap();

        // Hermetic: point the whole state folder at the tempdir, so discovery finds exactly this
        // addon and the saved-variables read cannot touch the machine's real file.
        let _c = crate::local_state::test_env::EnvGuard::unset("WOW_CAPTURE");
        let _h =
            crate::local_state::test_env::EnvGuard::set("BENILLA_HOME", home.to_str().unwrap());

        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        script.run("EventLog = {}").unwrap();
        let failures = load_third_party(&mut script, None, true);
        assert!(failures.is_empty(), "load errors: {failures:?}");
        crate::ui_script::finish_ui_load(&mut script);

        assert_eq!(
            event_log(&script),
            vec![
                "ADDON_LOADED:EventProbe",
                "VARIABLES_LOADED:nil",
                "PLAYER_LOGIN:nil",
            ],
            "every non-LoadOnDemand addon's ADDON_LOADED precedes VARIABLES_LOADED, which \
             precedes PLAYER_LOGIN"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ────────────────────────── the AddOn API + enable state (1188 phase 2) ──────────────────────

    /// Write a whole AddOns root under a temp `benilla-config`, and point `BENILLA_HOME` at it.
    /// Returns the guards, which must be held for the duration of the test.
    fn hermetic_root(tag: &str) -> (PathBuf, crate::local_state::test_env::EnvGuard) {
        // **The pid is load-bearing**, not decoration: two `benilla_app` test binaries can run at
        // once (a concurrent session, or `--all-targets`), and a fixed path plus the
        // `remove_dir_all` below means each wipes the other's tree mid-test. `ui_saved.rs` keys
        // its temp the same way for the same reason.
        let tmp =
            std::env::temp_dir().join(format!("benilla-addon-api-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("benilla-config");
        std::fs::create_dir_all(home.join("AddOns")).unwrap();
        let guard =
            crate::local_state::test_env::EnvGuard::set("BENILLA_HOME", home.to_str().unwrap());
        (home, guard)
    }

    /// **The version gate holds the startup walk, and force-load opens it** (decision 1292):
    /// the byte-verified `AddOn_CanLoad` check 6 — exact `==`, missing `## Interface` = 0 = out
    /// of date, a dependent of a gated addon blocked like a dependent of a disabled one — and
    /// the `checkAddonVersion` flag (the *Load out of date AddOns* checkbox inverted) loading
    /// the very same folder in full.
    #[test]
    fn the_version_gate_holds_the_walk_and_force_load_opens_it() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (home, _guard) = hermetic_root("version-gate");
        write_addon(
            &home,
            "Fresh",
            "## Interface: 11200\nmain.lua\n",
            &[("main.lua", "FreshRan = true\n")],
        );
        write_addon(
            &home,
            "Old",
            "## Interface: 11100\nmain.lua\n",
            &[("main.lua", "OldRan = true\n")],
        );
        write_addon(
            &home,
            "Silent",
            "main.lua\n", // no ## Interface at all — parses as 0, out of date (not "unknown")
            &[("main.lua", "SilentRan = true\n")],
        );
        write_addon(
            &home,
            "NeedsOld",
            "## Interface: 11200\n## Dependencies: Old\nmain.lua\n",
            &[("main.lua", "NeedsOldRan = true\n")],
        );

        let mut script = UiScript::new().unwrap();
        let failures = load_third_party(&mut script, None, true);
        assert!(
            failures.iter().any(|f| f.contains("NeedsOld")),
            "the gated dependency is the dependent's failure: {failures:?}"
        );
        assert_eq!(
            script.eval::<bool>("return FreshRan == true").ok(),
            Some(true)
        );
        assert_eq!(
            script
                .eval::<bool>("return OldRan == nil and SilentRan == nil")
                .ok(),
            Some(true),
            "out-of-date and interface-less addons are held by the gate"
        );
        assert_eq!(
            script.eval::<bool>("return NeedsOldRan == nil").ok(),
            Some(true),
            "…and so is their dependent (DEP_INTERFACE_VERSION territory)"
        );

        // Force-load: the same folder, the checkbox's other state — everything loads.
        let mut open = UiScript::new().unwrap();
        let failures = load_third_party(&mut open, None, false);
        assert!(failures.is_empty(), "force-load load errors: {failures:?}");
        assert_eq!(
            open.eval::<bool>(
                "return OldRan == true and SilentRan == true and NeedsOldRan == true"
            )
            .ok(),
            Some(true),
            "'Load out of date AddOns' loads the very same folder in full"
        );
    }

    fn write_addon(home: &Path, name: &str, toc: &str, files: &[(&str, &str)]) {
        let dir = home.join("AddOns").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.toc")), toc).unwrap();
        for (f, body) in files {
            std::fs::write(dir.join(f), body).unwrap();
        }
    }

    /// [`write_addon`] with **raw bytes**, because the files this crate must survive are not text.
    fn write_addon_bytes(home: &Path, name: &str, toc: &[u8], files: &[(&str, &[u8])]) {
        let dir = home.join("AddOns").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.toc")), toc).unwrap();
        for (f, body) in files {
            std::fs::write(dir.join(f), body).unwrap();
        }
    }

    /// **An addon whose files are not valid UTF-8 loads anyway** (decision 1193) — the corpus's
    /// single largest blocker, and one nobody would have guessed.
    ///
    /// Three separate failures, all of which used to be silent-ish and all of which are here:
    ///
    /// 1. a **cp1252 `.toc`** made the addon *invisible to discovery* — `manifest_in` read it with
    ///    `read_to_string`, got `None`, and a folder with no readable manifest is not an addon. 5
    ///    of a real 218-addon corpus vanished this way, and the harness scored them as clean
    ///    passes because an unparsed manifest lists no files to fail on.
    /// 2. a **BOM'd `.lua`** reached the lexer with `EF BB BF` in front and died on
    ///    `unexpected symbol`. 160 corpus files carry one.
    /// 3. a **cp1252 `.lua`** read as absent, so the loader reported "not found" for a file that
    ///    was right there. `AceAddon-2.0.lua` is one, embedded in ~30 addons.
    ///
    /// The literal survives as **bytes**, not as decoded text: Lua 5.0 strings are byte strings
    /// and the reference hands `luaL_loadbuffer` the file unmodified, so `string.len` on a cp1252
    /// literal must answer what it answers there. That assertion is the one that would fail if
    /// somebody "helpfully" transcoded chunks to UTF-8 later.
    #[test]
    fn an_addon_whose_files_are_not_utf8_still_loads() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (home, _guard) = hermetic_root("encoding");
        // `## Notes:` holds a cp1252 a-umlaut (0xE4) — one byte, and the whole addon disappeared.
        let mut toc = b"## Interface: 11200\n## Title: Sch\xE4tze\nlocale.lua\nboot.lua\n".to_vec();
        toc.splice(0..0, [0xEFu8, 0xBB, 0xBF]); // ...and a BOM on the manifest too (14 in corpus).
        write_addon_bytes(
            &home,
            "Umlaut",
            &toc,
            &[
                // A cp1252 locale file: the German/French half of the vanilla ecosystem.
                ("locale.lua", &b"UmlautWord = \"Sch\xE4tze\"\n"[..]),
                // A BOM'd chunk: valid UTF-8, three bytes the lexer cannot start on.
                (
                    "boot.lua",
                    &b"\xEF\xBB\xBFUmlautLoaded = true\nUmlautLen = string.len(UmlautWord)\n"[..],
                ),
            ],
        );

        let mut script = UiScript::new().unwrap();
        let failures = load_third_party(&mut script, None, true);
        assert!(failures.is_empty(), "load errors: {failures:?}");

        // Discovery saw it at all — the `.toc` decoded rather than read as absent.
        assert_eq!(script.eval::<i64>("return GetNumAddOns()").ok(), Some(1));
        assert_eq!(
            script
                .eval::<String>("return GetAddOnMetadata('Umlaut', 'Title')")
                .ok(),
            Some("Sch\u{e4}tze".to_string()),
            "the manifest's cp1252 title decoded to the glyph its author typed, not to nothing"
        );
        // Both chunks ran.
        assert_eq!(
            script.eval::<bool>("return UmlautLoaded == true").ok(),
            Some(true),
            "a BOM'd .lua ran — the three-byte mark was stripped, not handed to the lexer"
        );
        // ...and the cp1252 literal is still BYTES, which is the reference's semantics.
        assert_eq!(
            script.eval::<i64>("return UmlautLen").ok(),
            Some(7),
            "`Sch\\xE4tze` is 7 bytes in the file and must be 7 bytes in Lua — transcoding the \
             chunk to UTF-8 would make it 8 and silently move every string.sub in the addon"
        );
    }

    /// The AddOns screen's write **merges** rather than replaces (decision 1197).
    ///
    /// A name in the file that is not installed right now belongs to an addon that will be again.
    /// Rewriting the file from the installed list alone forgets the player's choice on every
    /// uninstall — the same rule `parse_enable_state` already documents for the read side, and the
    /// one that has to hold on the write side or the read's carefulness is wasted.
    #[test]
    fn the_addons_screen_write_merges_with_what_is_already_on_disk() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (home, _guard) = hermetic_root("screenwrite");
        let id = ("Realm".to_string(), "Char".to_string());
        let path = enable_state_path(Some(&id)).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "Gone: disabled\nStays: enabled\n").unwrap();

        write_enable_state(Some(&id), &[("Stays".into(), false), ("New".into(), true)]);

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("Gone: disabled"),
            "an uninstalled addon's choice survives — {written:?}"
        );
        assert!(
            written.contains("Stays: disabled"),
            "the edit applied — {written:?}"
        );
        assert!(
            written.contains("New: enabled"),
            "a new row appended — {written:?}"
        );
        let _ = home;
    }

    /// **`GetAddOnInfo` returns the manifest's own `## Title` and `## Notes`** — 1188 phase 2's
    /// first acceptance test, and the reason the registry carries directives rather than a name.
    ///
    /// The return shape is the **in-game** one — `name, title, notes, enabled, loadable, reason,
    /// security`, SEVEN values — because that is the VM this runs in.
    ///
    /// **The client registers `GetAddOnInfo` twice, as two different functions**, and this test
    /// used to assert the wrong one. It was written off `Interface\GlueXML\AddonList.lua`, which
    /// describes glue's `0x46d460`: eight values, `url` at slot 4, `newVersion` at 8. The in-game
    /// binding `0x48e390` answers seven, with **`enabled`** at slot 4 and no `url` at all
    /// (wow-re `system/ui/scratch/addon-version-gate.md`).
    ///
    /// Its own doc comment named the failure it then failed to catch — *"a row that shifts by one
    /// puts an addon's notes in its URL field, which nothing would notice"*. The row was shifted at
    /// slot 4 the whole time, and the test asserted the shift. That is what a falsifier copied from
    /// the wrong reference buys: it pins the bug.
    ///
    /// What it cost: `local name, _, _, enabled, loadable = GetAddOnInfo(major)` is AceLibrary's
    /// and AceAddon's shape, reached by 70 corpus folders. They read `url` as `enabled`, and since
    /// `## URL` is rare that is nil for nearly every addon, so `if enabled and loadable` refused to
    /// load the dependency. Silent — a nil where a flag belongs is legal Lua.
    ///
    /// The whole tuple is still asserted rather than the fields the phase names, for the original
    /// and still-correct reason.
    #[test]
    fn get_addon_info_returns_the_manifests_own_title_and_notes() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _c = crate::local_state::test_env::EnvGuard::unset("WOW_CAPTURE");
        let (home, _h) = hermetic_root("info");
        write_addon(
            &home,
            "Probe",
            "## Interface: 11200\n## Title: Probe Title\n## Notes: What it does\n## URL: http://example\n## Version: 1.2\n",
            &[],
        );
        let mut script = UiScript::new().unwrap();
        let _ = load_third_party(&mut script, None, true);

        assert_eq!(script.eval::<i64>("return GetNumAddOns()").ok(), Some(1));
        assert_eq!(
            script
                .eval::<Vec<String>>(
                    "local n,t,no,e,l,r,s,extra = GetAddOnInfo(1) \
                     return { n, t, no, tostring(e), tostring(l), tostring(r), s, \
                              tostring(extra) }"
                )
                .ok(),
            Some(vec![
                "Probe".into(),
                "Probe Title".into(),
                "What it does".into(),
                "1".into(), // enabled — slot 4 in-game, NOT url (which glue's binding returns)
                "1".into(), // loadable
                "nil".into(), // no reason — it loads
                "INSECURE".into(),
                // The in-game binding returns SEVEN values, so there is no eighth. Asserted as
                // absent rather than trimmed off the query: glue's `newVersion` sat here, and a
                // future edit that re-adds an eighth return has to face this line.
                "nil".into(),
            ])
        );
        // AceLibrary's and AceAddon's literal shape, which is how 70 corpus folders reach this
        // verb: `local name, _, _, enabled, loadable = GetAddOnInfo(major)`. Asserted as the
        // GUARD they actually write, not as a tuple, because the guard is what silently failed —
        // with `url` in slot 4 and no `## URL` in the manifest, `enabled` was nil and Ace declined
        // to load its own dependency without erroring.
        assert_eq!(
            script
                .eval::<bool>(
                    "local _, _, _, enabled, loadable = GetAddOnInfo('Probe') \
                     if enabled and loadable then return true else return false end"
                )
                .ok(),
            Some(true),
            "Ace's `if enabled and loadable` must pass for an enabled, loadable addon"
        );

        // Both spellings of the argument, and the raw directives.
        assert_eq!(
            script
                .eval::<String>("return (GetAddOnInfo('probe'))")
                .ok()
                .as_deref(),
            Some("Probe"),
            "index OR name, case-insensitively — the reference's verbs all take either"
        );
        assert_eq!(
            script
                .eval::<String>("return GetAddOnMetadata(1, 'Version')")
                .ok()
                .as_deref(),
            Some("1.2")
        );
        assert_eq!(
            script
                .eval::<bool>("return IsAddOnLoaded('Probe') == 1")
                .ok(),
            Some(true)
        );
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// **Disabling an addon in the enable-state file stops it loading** — phase 2's second
    /// acceptance test.
    ///
    /// The file is the reference's own `AddOns.txt` format, `<Name>: enabled|disabled` per line,
    /// confirmed against a real 1.12 install. It also checks the direction that matters more in
    /// practice: an addon the file does not mention loads, so dropping a folder in just works.
    #[test]
    fn a_disabled_addon_does_not_load_and_an_unlisted_one_does() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _c = crate::local_state::test_env::EnvGuard::unset("WOW_CAPTURE");
        let (home, _h) = hermetic_root("disable");
        write_addon(
            &home,
            "Off",
            "## Interface: 11200\nran.lua\n",
            &[("ran.lua", "OffRan = true")],
        );
        write_addon(
            &home,
            "On",
            "## Interface: 11200\nran.lua\n",
            &[("ran.lua", "OnRan = true")],
        );
        // "On" is deliberately absent from the file.
        std::fs::create_dir_all(home.join("addons")).unwrap();
        std::fs::write(home.join("addons/Realm-Char.txt"), "Off: disabled\n").unwrap();

        let mut script = UiScript::new().unwrap();
        let id = ("Realm".to_string(), "Char".to_string());
        let failures = load_third_party(&mut script, Some(&id), true);

        assert!(
            failures.is_empty(),
            "a disabled addon is a player's choice, never a load failure: {failures:?}"
        );
        assert_eq!(
            script.eval::<bool>("return OffRan == nil").ok(),
            Some(true),
            "the disabled addon's files must not have run"
        );
        assert_eq!(
            script.eval::<bool>("return OnRan == true").ok(),
            Some(true),
            "an addon the file never mentions is enabled — a dropped-in folder just works"
        );
        // And the API agrees with the loader about what happened.
        assert_eq!(
            script
                .eval::<Vec<String>>(
                    "local _,_,_,_,l,r = GetAddOnInfo('Off') return { tostring(l), tostring(r) }"
                )
                .ok(),
            Some(vec!["nil".into(), "DISABLED".into()]),
            "not loadable, with the reference's own reason token"
        );
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// **A LoadOnDemand addon loads on `LoadAddOn` and not before** — phase 2's third acceptance
    /// test, and the one that needs the loader to run from inside a Lua binding.
    ///
    /// `LoadAddOn` is called *from Lua*, synchronously, and its side effects are asserted on the
    /// next line — exactly how `UIParentLoadAddOn` uses it. A deferred implementation would fail
    /// here, which is the point.
    #[test]
    fn a_load_on_demand_addon_loads_only_when_asked() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _c = crate::local_state::test_env::EnvGuard::unset("WOW_CAPTURE");
        let (home, _h) = hermetic_root("lod");
        write_addon(
            &home,
            "Demand",
            "## Interface: 11200\n## Title: Demand\n## LoadOnDemand: 1\nlate.lua\nlate.xml\n",
            &[
                ("late.lua", "DemandRan = true"),
                (
                    "late.xml",
                    "<Ui><Frame name=\"DemandFrame\" parent=\"UIParent\"><Scripts>\
                     <OnEvent>DemandEvent = arg1</OnEvent></Scripts></Frame>\
                     <Script>DemandFrame:RegisterEvent(\"ADDON_LOADED\")</Script></Ui>",
                ),
            ],
        );
        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        let _ = load_third_party(&mut script, None, true);

        // Discovered and described, but NOT run.
        assert_eq!(script.eval::<i64>("return GetNumAddOns()").ok(), Some(1));
        assert_eq!(
            script
                .eval::<bool>("return IsAddOnLoadOnDemand(1) == 1")
                .ok(),
            Some(true)
        );
        assert_eq!(
            script.eval::<bool>("return DemandRan == nil").ok(),
            Some(true),
            "a LoadOnDemand addon must not run at startup"
        );
        assert_eq!(
            script
                .eval::<bool>("return IsAddOnLoaded('Demand') == nil")
                .ok(),
            Some(true)
        );

        // ...then loads synchronously, from Lua, and is usable on the very next statement.
        assert_eq!(
            script
                .eval::<Vec<String>>(
                    "local loaded, reason = LoadAddOn('Demand') \
                     return { tostring(loaded), tostring(reason), tostring(DemandRan), \
                              tostring(getglobal('DemandFrame') ~= nil) }"
                )
                .ok(),
            Some(vec!["1".into(), "nil".into(), "true".into(), "true".into()]),
            "LoadAddOn returns loaded=1 and its files have ALREADY run when it returns — the \
             reference's UIParentLoadAddOn uses the addon's frames on the next line"
        );
        assert_eq!(
            script
                .eval::<bool>("return IsAddOnLoaded('Demand') == 1")
                .ok(),
            Some(true)
        );
        // A demand load fires ADDON_LOADED too, at the same position: after the files.
        assert_eq!(
            script.eval::<String>("return DemandEvent").ok().as_deref(),
            Some("Demand"),
            "the addon's own frame, registered by its own XML, saw its own ADDON_LOADED"
        );
        // A second load is a no-op that still answers success, as the reference does.
        assert_eq!(
            script
                .eval::<String>("local l = LoadAddOn('Demand') return tostring(l)")
                .ok()
                .as_deref(),
            Some("1")
        );
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// The reason tokens are the reference's own, for the cases a caller branches on.
    ///
    /// They are not free-form strings: the reference splices them into
    /// `getglobal("ADDON_"..reason)` to find a localized label, so a token we invent renders as a
    /// nil global in somebody's addon manager.
    #[test]
    fn load_addon_answers_with_the_references_reason_tokens() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _c = crate::local_state::test_env::EnvGuard::unset("WOW_CAPTURE");
        let (home, _h) = hermetic_root("reasons");
        write_addon(&home, "Plain", "## Interface: 11200\n", &[]);
        write_addon(
            &home,
            "Off",
            "## Interface: 11200\n## LoadOnDemand: 1\n",
            &[],
        );
        std::fs::create_dir_all(home.join("addons")).unwrap();
        std::fs::write(home.join("addons/Realm-Char.txt"), "Off: disabled\n").unwrap();

        let mut script = UiScript::new().unwrap();
        let id = ("Realm".to_string(), "Char".to_string());
        let _ = load_third_party(&mut script, Some(&id), true);

        for (call, want) in [
            ("LoadAddOn('NoSuchAddon')", "MISSING"),
            ("LoadAddOn('Off')", "DISABLED"),
            // Loaded at startup because it is not LoadOnDemand, so this is the success case; the
            // NOT_DEMAND_LOADED arm needs an addon that is neither loaded nor LoadOnDemand, which
            // cannot happen once it is enabled — asserted through the disabled one above instead.
            ("LoadAddOn('Plain')", "nil"),
        ] {
            assert_eq!(
                script
                    .eval::<String>(&format!("local _, r = {call} return tostring(r)"))
                    .ok()
                    .as_deref(),
                Some(want),
                "{call}"
            );
        }
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// Enable state survives the session: `DisableAddOn` from Lua is written back in the
    /// reference's own `AddOns.txt` format, and read back as a disable next time.
    #[test]
    fn the_enable_state_round_trips_through_the_file() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _c = crate::local_state::test_env::EnvGuard::unset("WOW_CAPTURE");
        let (home, _h) = hermetic_root("roundtrip");
        write_addon(&home, "Keep", "## Interface: 11200\n", &[]);
        write_addon(
            &home,
            "Drop",
            "## Interface: 11200\nran.lua\n",
            &[("ran.lua", "DropRan = true")],
        );
        let id = ("Realm".to_string(), "Char".to_string());

        let mut script = UiScript::new().unwrap();
        let _ = load_third_party(&mut script, Some(&id), true);
        script.run("DisableAddOn('Drop')").unwrap();
        save_enable_state(&script, Some(&id));

        let written = std::fs::read_to_string(home.join("addons/Realm-Char.txt")).unwrap();
        assert_eq!(
            written, "Drop: disabled\nKeep: enabled\n",
            "the reference's own one-line-per-addon format"
        );

        // A fresh session reads it back and the addon stays off.
        let mut next = UiScript::new().unwrap();
        let _ = load_third_party(&mut next, Some(&id), true);
        assert_eq!(
            next.eval::<bool>("return DropRan == nil").ok(),
            Some(true),
            "the disable survived the session"
        );
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    // ───────────────────────────── saved variables (1188 phase 3) ─────────────────────────────

    /// **An addon sets a saved variable, the session ends, a new session sees it** — 1188 phase 3's
    /// acceptance test, end to end through the production path.
    ///
    /// It also pins the two orderings that make the mechanism work, both byte-verified:
    /// *account file then per-character file* (`0x51f4b5`, `0x51f53b`), so a per-character value
    /// wins; and *files → saved variables → `ADDON_LOADED`* (`0x51f5ad`), so the addon's own
    /// file-scope default is assigned first, overwritten second, and read by the handler third.
    /// Reverse either and the saved value can never win — which is a bug that looks exactly like
    /// "settings do not persist".
    #[test]
    fn an_addons_saved_variables_survive_the_session() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _c = crate::local_state::test_env::EnvGuard::unset("WOW_CAPTURE");
        let (home, _h) = hermetic_root("saved");
        write_addon(
            &home,
            "Keeper",
            "## Interface: 11200\n## SavedVariables: KeeperDB\n\
             ## SavedVariablesPerCharacter: KeeperChar\nkeeper.lua\nkeeper.xml\n",
            &[
                // File-scope defaults, exactly where the reference puts them.
                (
                    "keeper.lua",
                    "KeeperDB = { count = 0 }\nKeeperChar = 'default'\n",
                ),
                (
                    "keeper.xml",
                    "<Ui><Frame name=\"KeeperFrame\"><Scripts>\
                     <OnEvent>KeeperSawAtEvent = KeeperDB.count</OnEvent></Scripts></Frame>\
                     <Script>KeeperFrame:RegisterEvent(\"ADDON_LOADED\")</Script></Ui>",
                ),
            ],
        );
        let id = ("Realm".to_string(), "Char".to_string());

        // ── session one: defaults, then the addon changes them ──
        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        let failures = load_third_party(&mut script, Some(&id), true);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(
            script.eval::<i64>("return KeeperSawAtEvent").ok(),
            Some(0),
            "first run: no file yet, so ADDON_LOADED sees the file-scope default"
        );
        script
            .run("KeeperDB.count = 7 KeeperDB.note = 'hi' KeeperChar = 'mine'")
            .unwrap();
        crate::ui_script::shutdown_ui_state(&mut script, Some(&id));

        // ── session two: a fresh VM reads them back ──
        let mut next = UiScript::new().unwrap();
        next.set_screen_size(1024.0, 768.0);
        let failures = load_third_party(&mut next, Some(&id), true);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(
            next.eval::<i64>("return KeeperDB.count").ok(),
            Some(7),
            "the saved value overwrote the addon's own file-scope default"
        );
        assert_eq!(
            next.eval::<String>("return KeeperDB.note").ok().as_deref(),
            Some("hi"),
            "a table round-trips whole, not just the scalar that changed"
        );
        assert_eq!(
            next.eval::<String>("return KeeperChar").ok().as_deref(),
            Some("mine"),
            "the per-character file loaded too"
        );
        assert_eq!(
            next.eval::<i64>("return KeeperSawAtEvent").ok(),
            Some(7),
            "ADDON_LOADED handlers see the RESTORED value — the whole reason the event is last"
        );
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// The written bytes are the recorded grammar (decision 1128's, itself byte-verified against
    /// `0x7043f0`/`0x704480`): `NAME = value`, bracketed keys, TAB indent, a trailing comma on
    /// every entry, and the file split by scope.
    ///
    /// Asserted as bytes rather than by re-reading, because "it round-trips through our own
    /// loader" is exactly the check that passes for a private format. The reference's own client
    /// has to be able to read this.
    #[test]
    fn the_saved_file_bytes_match_the_recorded_grammar() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _c = crate::local_state::test_env::EnvGuard::unset("WOW_CAPTURE");
        let (home, _h) = hermetic_root("grammar");
        write_addon(
            &home,
            "Gram",
            "## Interface: 11200\n## SavedVariables: GramDB\n\
             ## SavedVariablesPerCharacter: GramChar\ngram.lua\n",
            &[("gram.lua", "GramDB = {}\nGramChar = 0\n")],
        );
        let id = ("Realm".to_string(), "Char".to_string());
        let mut script = UiScript::new().unwrap();
        let _ = load_third_party(&mut script, Some(&id), true);
        script
            .run("GramDB = { ['on'] = true, ['n'] = 2, ['s'] = 'a\\\"b', ['t'] = { 1 } } GramChar = 5")
            .unwrap();
        crate::ui_script::shutdown_ui_state(&mut script, Some(&id));

        let account = std::fs::read_to_string(home.join("saved/Gram.lua")).unwrap();
        let body = account.lines().skip(2).collect::<Vec<_>>().join("\n");
        assert_eq!(
            body,
            "GramDB = {\n\
             \t[\"n\"] = 2,\n\
             \t[\"on\"] = true,\n\
             \t[\"s\"] = \"a\\\"b\",\n\
             \t[\"t\"] = {\n\
             \t\t[1] = 1,\n\
             \t},\n\
             }",
            "keys always bracketed and quoted, TAB indent per level, trailing comma on every \
             entry, `\\\"` escaped — the recorded grammar\nGOT:\n{account}"
        );
        // Scope split: the per-character global is in the per-character file and nowhere else.
        assert!(!account.contains("GramChar"), "account file: {account}");
        let per_char = std::fs::read_to_string(home.join("saved/Realm-Char/Gram.lua")).unwrap();
        assert!(
            per_char.ends_with("GramChar = 5\n"),
            "per-char file: {per_char}"
        );
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// `PLAYER_LOGOUT` fires **before** the writes — an addon's last chance to mutate a saved
    /// global, and worthless if it fires after.
    ///
    /// The reference's own tail is `PLAYER_LEAVING_WORLD` → `PLAYER_LOGOUT` → the writes
    /// (`0x490c2a` before `0x490c7e`/`0x490c83`). This asserts the observable consequence rather
    /// than the call order: a value set *in the handler* has to reach the file.
    #[test]
    fn player_logout_fires_before_the_write() {
        let _l = crate::local_state::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _c = crate::local_state::test_env::EnvGuard::unset("WOW_CAPTURE");
        let (home, _h) = hermetic_root("logout");
        write_addon(
            &home,
            "Last",
            "## Interface: 11200\n## SavedVariables: LastDB\nlast.lua\nlast.xml\n",
            &[
                ("last.lua", "LastDB = 'unset'"),
                (
                    "last.xml",
                    "<Ui><Frame name=\"LastFrame\"><Scripts>\
                     <OnEvent>if event == \"PLAYER_LOGOUT\" then LastDB = 'written at logout' end</OnEvent>\
                     </Scripts></Frame>\
                     <Script>LastFrame:RegisterEvent(\"PLAYER_LOGOUT\")</Script></Ui>",
                ),
            ],
        );
        let id = ("Realm".to_string(), "Char".to_string());
        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        let _ = load_third_party(&mut script, Some(&id), true);
        crate::ui_script::shutdown_ui_state(&mut script, Some(&id));

        let written = std::fs::read_to_string(home.join("saved/Last.lua")).unwrap();
        assert!(
            written.contains("LastDB = \"written at logout\""),
            "the value the PLAYER_LOGOUT handler set must reach the file — if the write ran \
             first this reads 'unset':\n{written}"
        );
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }
}
