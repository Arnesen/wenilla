//! **The addon corpus harness** (1188 phase 6) — load every addon in a folder, one at a time, and
//! report what happened as numbers that can be re-read on any day.
//!
//! 1188 asks for a harness rather than a one-off, and the reason is in its own closing line:
//! *"that harness plus phase 0's coverage script are this arc's instruments, and they are what
//! make the remaining work a list instead of an argument."* `scripts/api-coverage.sh` answers
//! *what surface do we present*; this answers *what does that surface actually carry*.
//!
//! ## One VM per addon, deliberately
//!
//! Every addon is surveyed in a **fresh** [`UiScript`] with our own FrameXML loaded underneath it.
//! That costs a full UI load per addon and buys the only property that makes the report readable:
//! one addon's failure cannot be another's. Loading them all into one VM means the first addon to
//! leave a global in a bad state gets blamed for the next twenty, and the distribution 1188 asks
//! for stops meaning anything.
//!
//! **What one VM per addon costs, stated because it bounds every number here.** The real client
//! loads every addon into ONE Lua state, so a library embedded in *any* addon's `Libs\` is global
//! for every addon that loads after it. Here it is not. An addon that ships no libraries and relies
//! on a sibling's copy — `FuBar_CustomMenuFu` ships one Lua file and calls
//! `AceLibrary("Tablet-2.0")`, which no addon it can reach provides — fails in this survey and
//! would work in a real session. **So the headline is a floor, not an estimate**, and a
//! `Cannot find a library instance of X` row is this limitation before it is a gap of ours. The
//! isolation is still the right trade (see above: one addon's failure must not be another's); it
//! is the reporting that has to say so. Pinned by
//! [`dependency_tests::a_sibling_addons_embedded_library_is_invisible`].
//!
//! The FrameXML underneath is not optional either — an addon calls `UIDropDownMenu_Initialize` and
//! `GameTooltip_SetDefaultAnchor` as readily as it calls `UnitName`, and roughly half of what looks
//! like "the WoW API" is Lua the client ships (decision 1190: 1,100 engine functions vs 1,075
//! FrameXML ones). Surveying against a bare VM would report most of FrameXML as missing.
//!
//! ## What "missing" means here, and what it does not
//!
//! [`AddonReport::missing_globals`] is a **static** read: the names the addon's own source calls
//! like functions, minus everything the loaded VM has, minus what the addon defines itself. It is
//! deliberately not a runtime trace, because an addon's API calls overwhelmingly happen in
//! handlers that only fire in a live session — a load-time trace would report almost nothing and
//! read as success.
//!
//! The cost of that choice, stated rather than hidden: it over-reports. A name reached only on a
//! path the addon never takes still counts, and a name built at runtime (`getglobal("Unit"..verb)`)
//! is invisible to it. So the list is a **prioritisation signal**, exactly like
//! `api-coverage.sh`'s — read the ranked aggregate, not any single row, and never quote it as a
//! pass rate.
//!
//! There are **three** such lists and they rank three different queues, never merged (1207):
//! [`AddonReport::missing_globals`] is functions to write in Rust, [`AddonReport::missing_tables`]
//! is FrameXML to transcribe, and [`AddonReport::missing_methods`] is widget bindings. The third
//! arrived last and had been invisible the longest — a method is not a global, so nothing here
//! could rank one, and the error row that carried them collapsed the name away.
//!
//! The method queue then turned out to be **two** questions, not one (decision 1228):
//! `missing_methods` asks whether *any* widget answers a name, which is blind to a verb wired to
//! one class and forgotten on its sibling — `MessageFrame:AddMessage` scored zero on it while three
//! corpus addons had exactly that call as their first load error.
//! [`AddonReport::kind_missing_methods`] asks the question the addon asked, by typing the receiver
//! of the call site; [`AddonReport::ambiguous_methods`] is what the typing could not reach, printed
//! rather than swallowed.
//!
//! ## Running it
//!
//! ```text
//! cargo run -q -p benilla-app --example addon_harness -- <folder of addons>
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use benilla_ui::script::UiScript;
use benilla_ui::toc::Toc;

/// The render column — the one question every column here was blind to: *did this addon put
/// anything on screen?* Its own module because it is its own concern (and this file is well over
/// the size budget); its header is the design and the honest bounds.
pub mod render;
#[cfg(test)]
mod render_tests;
/// The use column — *does the thing it drew survive being **touched**?* Its own module for the
/// same two reasons `render` is; its header is the design, the bounds and the four-deep history
/// that made it necessary.
mod use_probe;
#[cfg(test)]
mod use_probe_tests;

use render::{measure_render, RenderBaseline};
pub use render::{Drew, RenderReport};
use use_probe::measure_use;
pub use use_probe::{UseReport, Used, MAX_USE_TARGETS};

/// What one addon did.
///
/// `Default` is derived so the two error returns in [`survey_one`] and the unit tests can name only
/// the fields they mean — a report grew from nine fields to fourteen over this arc, and a
/// positional wall of `Vec::new()`s is where a new column silently gets filled with the wrong one.
#[derive(Debug, Clone, Default)]
pub struct AddonReport {
    pub name: String,
    /// `## Interface`, as written. 1.12 is `11200`; the corpus is full of older values, and we
    /// deliberately do not refuse them (decision 1191 §6).
    pub interface: Vec<u32>,
    /// Did every file in its manifest load without an error?
    pub loaded: bool,
    /// Load errors, verbatim, tagged by file.
    pub errors: Vec<String>,
    /// Names it calls that the VM does not have — see the module doc on what this is worth.
    pub missing_globals: Vec<String>,
    /// Dependencies named in its `.toc` that are not in the folder.
    pub missing_deps: Vec<String>,
    /// Templates it names in `CreateFrame(kind, name, parent, "Template")` that the VM has never
    /// declared (decision 1203).
    ///
    /// **A blind spot by construction until it was measured.** `CreateFrame`'s fourth argument was
    /// ignored outright until today; now it is honoured, and the survey's headline did not move at
    /// all — because an unresolved template produces *no load error*, so `loaded` cannot see it and
    /// `missing_globals` does not either (a template is not a global). An addon gets a bare frame,
    /// loads clean, and paints nothing.
    ///
    /// Static, like `missing_globals`, and with the same caveat: read the ranked aggregate.
    pub missing_templates: Vec<String>,
    /// Templates it names in an XML `inherits=` that the VM has never declared.
    ///
    /// [`Self::missing_templates`]'s twin, added because the first was measuring the wrong axis.
    /// 1203 built `CreateFrame`'s fourth argument and ranked what it could not resolve; the
    /// transcription that followed then moved the headline by twelve addons, and **not one of
    /// those twelve came through `CreateFrame`** — every one failed on an `inherits=` in its own
    /// XML. The two are ranked separately rather than merged because they behave differently:
    /// an unresolved `CreateFrame` template is silent (1203 §2), while an unresolved `inherits=`
    /// is usually loud, because the very next line of the element's `<OnLoad>` is
    /// `getglobal(this:GetName().."Text")` and the loader fires that `<OnLoad>` immediately.
    ///
    /// Same static caveat, plus one of its own: `inherits=` spans two namespaces — a `<FontString
    /// inherits="GameFontNormal">` names a FONT — so a name registered as either is resolved.
    pub missing_inherits: Vec<String>,
    /// Errors raised **after** the files loaded, while the session start was driven —
    /// `ADDON_LOADED` → `VARIABLES_LOADED` → `PLAYER_LOGIN` → `PLAYER_ENTERING_WORLD`, then a few
    /// ticks to drain `OnUpdate` and anything scheduled.
    ///
    /// **This is the survey's answer to its own oldest blind spot.** Every number beside it is
    /// load-time, and this arc has now written the same sentence into four decision records —
    /// 1203, 1205, 1211 and the state-texture pass all end with *"the headline cannot see this"*,
    /// because an addon whose file scope runs clean scores as a pass no matter what its handlers
    /// do. The handlers are where addons actually live: an `OnEvent` that fires on `PLAYER_LOGIN`
    /// is the single most common shape in the corpus.
    ///
    /// Names it INDEXES (`Foo.bar`, `Foo:baz()`) that the VM does not have — frames and tables,
    /// where [`Self::missing_globals`] is functions.
    ///
    /// Two lists because the two queues go to different places: a missing function is a verb to
    /// write in Rust, a missing frame or table is FrameXML to transcribe. And ranking them together
    /// would mis-rank, which is 1207's lesson.
    ///
    /// **The scan was blind to this shape until 2026-08-11.** `ColorPickerFrame` — a window
    /// **86 corpus addons reach** — scored exactly 0 on the most-wanted list, because the corpus
    /// spells it `ColorPickerFrame.func` and `ColorPickerFrame:SetColorRGB`, never `ColorPickerFrame(`.
    /// The same blindness hid `GameTooltip`, `WorldFrame`, `ChatFrame1` and every other FrameXML
    /// frame global. Third instrument correction of this arc, after 1209 and 1210.
    pub missing_tables: Vec<String>,
    /// Names it calls as `obj:Name(...)` that **no widget we ship provides** — the widget-method
    /// class, which nothing else here could see.
    ///
    /// [`Self::missing_globals`] is functions and [`Self::missing_tables`] is frames; **a method is
    /// neither, and never lands in either.** `BuffCheck2/BuffCheck2.lua:448` died on
    /// `attempt to call method 'GetBackdrop' (a nil value)` while `GetBackdrop` scored exactly zero
    /// on every ranking this harness printed — and the arc was working precisely this class by hand
    /// at the time (`GetTexture`, `SetShadowColor`, `SetNonSpaceWrap`, `GetBackdrop` all landed
    /// within hours of each other). Six corpus addons' FIRST error is this shape today, and the
    /// ranked row that carries them reads `attempt to call method 'X' (a nil value)`: the name —
    /// the only part anyone can act on — is exactly what the normalisation deletes.
    ///
    /// **Resolved by ASKING THE VM, never by a name list.** One instance of every `CreateFrame`
    /// kind plus a Texture and a FontString are stood up and each wanted name is looked up through
    /// the real `__index` dispatcher — the same path the addon's own call takes — so this cannot
    /// drift from what we actually implement the way a hand-kept list would.
    ///
    /// **It resolves against ANY probe, so it is blind to a method on the WRONG kind** (decision
    /// 1228), and it stays that way **on purpose**: this is the number four decision records quote,
    /// and redefining it would make every one of them incomparable (1209). A name survives here
    /// only if *no* widget answers it; a verb we implemented on one class and forgot on its sibling
    /// resolves and scores zero. This doc once claimed the opposite — that an unwired
    /// `<MessageFrame>` was exactly what the list would catch, with `AddMessage` topping the
    /// ranking because of it — and the run underneath it never agreed: `AddMessage` sat at zero all
    /// along, answered by `ScrollingMessageFrame`, while three addons died on
    /// `UIErrorsFrame:AddMessage`.
    ///
    /// So this list ranks **verbs nobody has**; [`Self::kind_missing_methods`] ranks **kinds nobody
    /// wired**, by typing the receiver of the call site instead of asking the probe set at large,
    /// and [`Self::ambiguous_methods`] carries the residue it cannot type. Read all three; they are
    /// three different jobs, and merging them would lose exactly the distinction that cost this
    /// instrument a whole class.
    ///
    /// **It OVER-reports, deliberately**, and this is the field where that trade bites hardest: a
    /// scanner cannot know the receiver of a `:` call, so every OO library method whose definition
    /// is not in the addon's own source counts here too (the whole Ace2/FuBar `self:Foo()` family
    /// when it is reached through a *dependency* rather than an embedded copy). What is subtracted
    /// is what can be seen: names the addon's own files bind as `function T:N`, `function T.N`, and
    /// `T.N = …`, and the same three read off every dependency the VM loaded under it. A method
    /// bound as `T["N"] = …` cannot be subtracted at all — `strip_lua_noise` blanks the string
    /// before the scan sees it — and that is stated rather than hidden.
    ///
    /// **The residue in the ranking's head is the one-VM limitation, not a gap of ours** — the same
    /// bound the module doc already puts on `Cannot find a library instance of X`, showing up on a
    /// new axis. `FuBar:RegisterPlugin` sits at 8 because eight addons call a library they never
    /// declare, so nothing loads it for them; on the real client a sibling's copy is global. And
    /// like every list here it counts a name on a branch the addon never takes:
    /// `instance:RegisterTabCompletion` is 51 addons' copy of one AceConsole line guarded by
    /// `elseif major == "AceTab-2.0"`, for a library no corpus addon ships. Read the ranked
    /// aggregate, and read the row's call sites before building it (1214).
    pub missing_methods: Vec<String>,
    /// Methods it calls **and feature-tests first** (`if f.SetTopLevel then f:SetTopLevel(1) end`)
    /// that no widget we ship provides — [`Self::missing_methods`]'s other half, printed beside it
    /// rather than folded into it.
    ///
    /// These are not blockers: the addon has already written the branch it takes without them, so
    /// counting them in the ranking a session builds from would put a name nobody is stuck on above
    /// one somebody is. But they are not nothing either — **`SetTopLevel` is a real 1.12 widget
    /// method 60 corpus addons work around**, and implementing it silently improves every one of
    /// them. Dropping the class outright is exactly the silent under-report this instrument keeps
    /// being caught in; it gets its own row instead.
    pub optional_methods: Vec<String>,
    /// `Kind:Name (on …)` — a method called on a receiver this survey could **type**, that that
    /// kind does not answer. [`Self::missing_methods`]'s answer to the bound 1228 recorded.
    ///
    /// `missing_methods` asks the probe set *"does ANY widget answer this name"*, and a verb we
    /// implemented on one class and never wired to its sibling answers yes. That is not a corner
    /// case: `MessageFrame:AddMessage` scored **zero** on that list for as long as it existed —
    /// answered by the ScrollingMessageFrame probe — while `EasyCopy`, `QuestHistory` and
    /// `QuestItem` all had `UIErrorsFrame:AddMessage` as their FIRST load error. This list asks the
    /// question the addon actually asked: *does the kind it called it on answer it*.
    ///
    /// The row says which kinds do answer, because that is the difference between two jobs: `(on no
    /// kind)` is a verb to write, `(on ScrollingMessageFrame)` is a verb we already wrote and
    /// forgot to wire.
    ///
    /// **Bounded by the attributor, and only the attributor.** A receiver is typed when the file
    /// binds it from a widget factory (`local f = CreateFrame("MessageFrame", …)`,
    /// `f:CreateTexture()`) or when it is a **published name** whose kind the live arena knows
    /// (`UIErrorsFrame` → `MessageFrame`, via [`UiScript::widget_kind`]). `self:Foo()`,
    /// `this:Foo()`, `a.b:Foo()` and `getglobal(n):Foo()` are not typable and are reported in
    /// [`Self::ambiguous_methods`] instead of being silently counted as fine.
    pub kind_missing_methods: Vec<String>,
    /// `Name (only on …)` — a name the addon calls on a receiver nothing could type, where the
    /// answer **depends on the kind**: some widgets we ship answer it and some do not.
    ///
    /// The row this whole per-kind pass exists to stop swallowing. Before it, such a name resolved
    /// against the probe set and vanished; now the reader is told the answer is conditional and on
    /// what. It is an **upper bound on purpose** — the call may well be on one of the kinds that
    /// does answer — and the honest way to read it is as a measure of how much the *attributor*
    /// still cannot see: every receiver shape [`scan_receivers`] learns to type moves rows out of
    /// here and into a real answer.
    ///
    /// A name **no** kind answers is not here — it is a plain miss and it is in
    /// [`Self::missing_methods`].
    pub ambiguous_methods: Vec<String>,
    /// Kept **separate from [`Self::loaded`] on purpose.** Folding these in would silently change
    /// what the headline means and make every number in every past decision record incomparable
    /// (1209's whole subject). `loaded` still means exactly "no LOAD errors"; this is a second,
    /// stricter column beside it.
    pub session_errors: Vec<String>,
    /// What the addon's own UI OVERRIDES raised when actually invoked — see [`drive_ui_probe`].
    ///
    /// A NEW column beside `session_errors`, never folded into it (1213's rule): this asks a
    /// different question, and redefining an existing number would make every past run
    /// incomparable.
    pub probe_errors: Vec<String>,
    /// **What it actually PUT ON SCREEN** — see [`render`], whose header is the design.
    ///
    /// The column every other one here was blind to. `loaded`, `session_errors` and `probe_errors`
    /// all ask whether something *raised*; the director's Bagnon report raised nothing at all and
    /// drew nothing either, and scored a clean pass on all three. A new column beside them,
    /// never folded in — 1213's rule, for the third time.
    pub render: RenderReport,
    /// **What happened when the UI it drew was actually USED** — see [`use_probe`], whose header is
    /// the design and the four-deep history behind it.
    ///
    /// [`Self::render`] closed "did anything appear"; this closes "is what appeared alive". The
    /// director drew the line themselves: Bagnon draws sixteen bag slots and they are dead to the
    /// touch — hovering one raised `attempt to call global 'ContainerFrameItemButton_OnEnter'` —
    /// while `render` scored it a full pass, because a frame that paints and does nothing paints
    /// exactly like one that works.
    ///
    /// A new column beside the others, never folded in — 1213's rule, for the fourth time. Read
    /// [`UseReport::driven`] with every verdict: a clean row that touched nothing is
    /// [`Used::Untouched`], which is not a pass.
    pub used: UseReport,
}

/// Survey every addon folder under `root`.
///
/// `root` is an AddOns folder — one subfolder per addon, each with a `<Name>.toc`. Anything else
/// is skipped in silence, exactly as discovery does, because a stray `Backup/` is the common case.
pub fn survey(root: &Path) -> Vec<AddonReport> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .filter(|n| manifest_path(root, n).is_some())
        .collect();
    names.sort();

    let installed: BTreeSet<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();

    // THE ADDON REGISTRY, built once and seated into every VM below.
    //
    // Until this existed the survey never called `register_addons` at all, so every VM ran with an
    // EMPTY registry: `GetNumAddOns()` answered 0 and `GetAddOnInfo(...)` answered nothing, for all
    // 218. That is a state the real client cannot produce — the same fault as 1193 (dependencies
    // unloaded) and 1212 (`OptionalDeps` unwalked), and it hid the whole AddOn API from the survey.
    //
    // The cost was not theoretical. AceAddon and AceLibrary — the two most replicated files in the
    // corpus — find their dependencies with `local name, _, _, enabled, loadable =
    // GetAddOnInfo(major)`, and against an empty registry every one of those took the "nothing is
    // installed" path. It is also why fixing `GetAddOnInfo`'s slot 4 (`4666b708`) moved the
    // headline by exactly zero: the instrument could not see the verb at all.
    //
    // Every addon is registered ENABLED, which is the survey's own premise — it asks what an addon
    // does when the player has it on. `loaded` stays false: the flag means "already loaded in this
    // session", and each VM loads exactly one addon plus its declared dependencies.
    let registry: Vec<benilla_ui::script::AddOnInfo> = names
        .iter()
        .filter_map(|n| {
            let toc = Toc::parse(&benilla_ui::source::decode(
                &std::fs::read(manifest_path(root, n)?).unwrap_or_default(),
            ));
            Some(crate::ui_script::addons::info_from_toc(n, &toc))
        })
        .collect();

    // Folder → the method names its source DEFINES, memoised across the whole survey.
    //
    // A dependency's definitions have to be read once per *folder*, not once per dependent: the
    // corpus's library folders are declared by dozens of addons each (83 declare `FuBar`), and
    // re-scanning `FuBar`'s source 83 times is the survey's runtime for nothing.
    let mut defined_methods: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    names
        .iter()
        .map(|n| survey_one(root, n, &installed, &registry, &mut defined_methods))
        .collect()
}

/// The method names one addon folder's source **defines** — `function T:N`, `function T.N`,
/// `T.N = …` — memoised by folder.
///
/// This is the method-side answer to a question `known` already answers for globals. `known` is
/// read off the VM *after* the dependency chain has run, so a global a dependency defines is
/// correctly not missing; there is no equivalent read for methods, because a method lives on an
/// object the survey cannot name, so the source is the only available proxy.
///
/// **Skipping this made the first widget-method ranking useless**, and in the most instructive way:
/// seven of its top eleven rows were `FuBar:RegisterPlugin`, `FuBar:GetNumPanels`,
/// `FuBar:IsChangingProfile` and their siblings, at 75-78 addons each. Every one is
/// `function FuBar:<Name>` in the **`FuBar` addon's own source** — a dependency 83 corpus addons
/// declare, which `load_dependencies` duly loads into their VM. The methods exist at runtime; only
/// the scanner could not see them, because it read the dependent's folder and stopped. Under them,
/// at 61 and 60, sat `SetDesaturated` and `SetTopLevel` — two widget methods we genuinely do not
/// have. That is 1210's lesson a third time: a ranking built by grepping source is worth exactly
/// what its subtractions are worth.
fn methods_defined_by<'a>(
    root: &Path,
    folder: &str,
    memo: &'a mut BTreeMap<String, BTreeSet<String>>,
) -> &'a BTreeSet<String> {
    if !memo.contains_key(folder) {
        let mut scan = Scan::default();
        if let Some(toc) = manifest_path(root, folder)
            .and_then(|p| std::fs::read(p).ok())
            .map(|b| Toc::parse(&benilla_ui::source::decode(&b)))
        {
            for path in source_files(root, folder, &toc) {
                if let Some(text) = read_text(root, &path) {
                    scan_source(&path, &text, &mut scan);
                }
            }
        }
        memo.insert(folder.to_string(), scan.defined_methods);
    }
    &memo[folder]
}

/// `<root>/<name>/<name>.toc`, matched case-insensitively — a real 1.12 addon may ship
/// `MyAddon/myaddon.toc`, and on a case-sensitive filesystem an exact probe would not find it.
fn manifest_path(root: &Path, name: &str) -> Option<PathBuf> {
    let want = format!("{name}.toc");
    std::fs::read_dir(root.join(name))
        .ok()?
        .flatten()
        .find(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|f| f.eq_ignore_ascii_case(&want))
        })
        .map(|e| e.path())
}

/// The per-addon VM-instruction bound (see its use in [`survey_one`]). Measured: the heaviest
/// legitimate corpus addon runs 4M, and 214 of 218 run under 1M.
const ADDON_INSTRUCTION_BUDGET: u64 = 200_000_000;

fn survey_one(
    root: &Path,
    name: &str,
    installed: &BTreeSet<String>,
    registry: &[benilla_ui::script::AddOnInfo],
    defined_methods: &mut BTreeMap<String, BTreeSet<String>>,
) -> AddonReport {
    let Some(toc_path) = manifest_path(root, name) else {
        return AddonReport {
            name: name.to_string(),
            errors: vec!["no manifest".into()],
            ..Default::default()
        };
    };
    // Decoded, not `read_to_string`'d: five of the corpus's 218 manifests are cp1252 and would
    // otherwise parse as an empty `.toc` — an addon with no files and no dependencies, which reads
    // as a clean pass (decision 1193).
    let toc = Toc::parse(&benilla_ui::source::decode(
        &std::fs::read(&toc_path).unwrap_or_default(),
    ));
    let missing_deps: Vec<String> = toc
        .dependencies()
        .into_iter()
        .filter(|d| !installed.contains(&d.to_ascii_lowercase()))
        .map(str::to_owned)
        .collect();

    // A VM with our whole interface under it — see the module doc on why per-addon and why loaded.
    let mut script = match UiScript::new() {
        Ok(s) => s,
        Err(e) => {
            return AddonReport {
                name: name.to_string(),
                interface: toc.interface_versions(),
                errors: vec![format!("VM: {e}")],
                missing_deps,
                ..Default::default()
            }
        }
    };
    // **A time bound, so one non-terminating addon cannot take the whole survey with it.**
    //
    // 1247 met the failure this exists for: `date("*t")` returned a string where Lua returns a
    // table, so an addon's `while` never ended and the 218-addon run simply stopped finishing —
    // no roster to diff, no column to compare, no error row to read, and the cause found by
    // bisecting the corpus BY HAND. A wrong number is recoverable; an instrument that does not
    // return is not.
    //
    // The budget is MEASURED, not guessed. Across the corpus at the time of writing, 214 of 218
    // addons execute fewer than 1M VM instructions in a whole survey (the counter's resolution);
    // the heaviest legitimate one is Enchantrix at 4M, then FuBar_CTRaid and BigWigs at 1M. So
    // this is ~50x the heaviest real addon — far enough out that a fixture growing new seats
    // cannot drift into it, and near enough that a runaway reports in about a second.
    script.set_instruction_budget(ADDON_INSTRUCTION_BUDGET);
    script.set_screen_size(1024.0, 768.0);
    // Before anything runs: the AddOn API must answer for the whole installed set, not for nothing.
    // `None` roots because a survey must never read or write the director's real saved variables
    // (the same reason `drive_session_start` is not `finish_ui_load` — 1213 §4).
    script.register_addons(registry.to_vec(), None, None, None);
    seat_a_session(&mut script);
    let _ = crate::ui_script::load_default_ui(&script);
    // The addon's DEPENDENCIES, first and recursively — `AddOn_Load 0x51f240`'s own first two
    // steps (1191 §2, byte-verified). Surveying an addon without them is surveying a state the
    // real client never presents: `FuBar_Aspect` declares `## Dependencies: Ace, FuBar` and opens
    // with `ace:LoadTranslation(...)`, so in isolation it fails on a global its dependency was
    // always going to define. Fifteen corpus addons failed that way, on us rather than on
    // themselves.
    //
    // Loaded BEFORE `globals_of` below, so a name a dependency provides does not also count as a
    // missing global — the same double-count the FrameXML-underneath decision avoided.
    let mut dep_order: Vec<String> = Vec::new();
    load_dependencies(
        &script,
        root,
        &toc,
        installed,
        &mut BTreeSet::new(),
        &mut dep_order,
    );
    let known = globals_of(&script);
    // The METHODS the dependency chain defines, gathered from the same folders whose files were
    // just loaded above. `known` covers their globals because the VM actually ran them; a method
    // has no such read (it lives on an object the survey cannot name), so its source stands in.
    let dep_methods: BTreeSet<String> = dep_order
        .iter()
        .flat_map(|d| methods_defined_by(root, d, defined_methods).clone())
        .collect();

    // THE RENDER BASELINE — every widget that exists before this addon has run a single line.
    // Everything that appears after it was created by this addon (or by a handler of its own that
    // it installed), which is what makes the render column an attribution rather than a guess.
    // Taken here, after the dependency chain, so a library's frames are not charged to its
    // consumer — the same rule `load_dependencies` applies to errors.
    let baseline = RenderBaseline::of(&script);

    let errors = load_addon_files(&script, root, name, &toc);
    let wants = missing_calls(root, name, &toc, &known, &dep_methods);
    // AFTER the addon's files: a template it declares in its OWN XML is registered by then, so it
    // is not missing. The check asks the VM's live registry, not a name list.
    let missing_templates = missing_templates(&script, root, name, &toc);
    let missing_inherits = missing_inherits(&script, root, name, &toc);
    let session_errors = drive_session_start(&mut script, name, &dep_order);
    let probe_errors = drive_ui_probe(&mut script);
    // AFTER the UI probe and BEFORE the method oracle, and both halves of that are load-bearing.
    // After, because the probe leaves the addon fully driven — and this pass re-OPENS what the
    // probe's second toggle closed, which is why it is a separate probe rather than a read at the
    // end of that one. Before, because `unresolved_widget_methods` stands up one frame of every
    // kind: measuring after it would charge sixteen widgets of the harness's own to the addon and
    // score every single row as having drawn.
    let (render, painted) = measure_render(&mut script, &baseline);
    // ...and then USE what it drew. Same seam, same two reasons: after every other column so no
    // number beside it can be perturbed by input this probe invented, and before the method oracle
    // so the sixteen widgets that pass stands up can never become an addon's input targets.
    //
    // It consumes `render`'s own attribution rather than re-deriving it — "which pixels are this
    // addon's" is one question with one answer, and two instruments computing it separately is how
    // they start disagreeing about the same addon.
    let used = measure_use(&mut script, &baseline, &painted);
    // LAST, and the ordering is deliberate: standing up the probe widgets writes frames and one
    // global into the VM, and this is the only point at which no addon code will ever observe
    // them. Every other number above is therefore provably unperturbed by this list existing —
    // which is what an instrument addition has to be able to claim (1225's measurement rule).
    //
    // ONE oracle call for both question sets and for the per-kind pass: it stood the whole probe
    // set up twice for the same answer before, and the per-kind pass would have made that three.
    let asked: BTreeSet<String> = wants
        .wanted_methods
        .union(&wants.tested_methods)
        .cloned()
        .collect();
    eprintln!("INSTRMEASURE\t{name}\t{}", script.instructions_used());
    eprintln!("INSTRM	{name}	{}", script.instructions_used());
    let oracle = widget_method_kinds(&script, &asked);
    let missing_methods = unresolved_from(&oracle, &wants.wanted_methods);
    let optional_methods = unresolved_from(&oracle, &wants.tested_methods);
    // The receiver-typed half. It reads `widget_kind` off the same VM, which is the point: the kind
    // an addon's call actually lands on is the kind OUR object graph publishes, not the one the
    // reference documents.
    let (kind_missing_methods, ambiguous_methods) = per_kind_rows(&script, &oracle, &wants);

    AddonReport {
        name: name.to_string(),
        interface: toc.interface_versions(),
        loaded: errors.is_empty(),
        errors,
        missing_globals: wants.missing_globals,
        missing_deps,
        missing_templates,
        missing_inherits,
        missing_tables: wants.missing_tables,
        missing_methods,
        optional_methods,
        kind_missing_methods,
        ambiguous_methods,
        session_errors,
        probe_errors,
        render,
        used,
    }
}

/// **Which widget KINDS answer each wanted name** — the oracle every method table here is read off.
/// `None` means the oracle could not run at all.
///
/// **It asks the live `__index` dispatcher, one probe per widget kind, rather than consulting a
/// list of names.** The method tables live in the Lua registry behind a Rust `__index` and are not
/// enumerable from Lua at all, so the alternative was a hand-kept mirror of every
/// `REG_*_METHODS` table — a list that would go stale the first time a kind was added and would
/// then report a whole widget class as missing, or (worse) as present. Standing up one of each
/// kind and indexing it is the *same path the addon's own call takes*, which is the only oracle
/// that cannot disagree with what we ship.
///
/// **The answer is kept PER KIND, and that is the change decision 1228 asked for.** It used to be
/// collapsed to "did anything answer", which is why `MessageFrame:AddMessage` was unfindable while
/// three addons died on it: the ScrollingMessageFrame probe answered the name, so the name was not
/// missing, so there was no row anywhere in this report.
///
/// A kind `CreateFrame` refuses is skipped rather than fatal (`pcall`), so this survives a kind
/// being renamed. But **an oracle that finds no probes at all reports everything as missing**, not
/// nothing: a silent empty answer would read as "this addon calls no method we lack", which is the
/// one thing this instrument exists to stop being invisible. `None` is how that travels; every
/// reader of it ([`unresolved_from`], [`per_kind_rows`]) turns it into the loudest answer it can.
/// Pinned by `the_method_oracle_fails_loudly_when_it_cannot_probe` and
/// `the_per_kind_census_fails_loudly_when_it_cannot_probe`.
///
/// One call per addon serves all three tables. It used to be two (`wanted`, then `tested`), which
/// stood the whole probe set up twice for the same answer.
fn widget_method_kinds(script: &UiScript, wanted: &BTreeSet<String>) -> Option<MethodOracle> {
    if wanted.is_empty() {
        return Some(MethodOracle::default());
    }
    // Safe to interpolate unquoted: these names came out of the scanner's identifier reader, so
    // they are letters, digits and underscores — a quote or a backslash cannot be in one.
    let names = wanted
        .iter()
        .map(|n| format!("\"{n}\""))
        .collect::<Vec<_>>()
        .join(",");
    let kinds = PROBE_FRAME_KINDS
        .iter()
        .map(|k| format!("\"{k}\""))
        .collect::<Vec<_>>()
        .join(",");
    let chunk = format!(
        r#"
        local kinds, probes = {{}}, {{}}
        -- Every kind `frame_kind_from_str` accepts. A kind that is refused is simply skipped: this
        -- list going stale must never turn into a wrong ANSWER, only into a narrower probe set.
        for _, kind in ipairs({{{kinds}}}) do
            local ok, f = pcall(CreateFrame, kind)
            if ok and type(f) == "table" then
                table.insert(kinds, kind)
                table.insert(probes, f)
            end
        end
        -- The two REGION leaves. They carry their own metatable (the "tag" table), so a
        -- FontString's SetFont and a Texture's SetTexCoord are unreachable from any frame probe —
        -- and region methods are a large slice of what the corpus calls.
        if probes[1] then
            local okt, tex = pcall(function() return probes[1]:CreateTexture() end)
            if okt and type(tex) == "table" then
                table.insert(kinds, "Texture") table.insert(probes, tex)
            end
            local okf, fs = pcall(function() return probes[1]:CreateFontString() end)
            if okf and type(fs) == "table" then
                table.insert(kinds, "FontString") table.insert(probes, fs)
            end
        end
        if table.getn(probes) == 0 then error("no widget probes could be created") end
        -- Row 1 is the ROSTER: which kinds actually stood up. Without it the caller has to infer
        -- the probe count from the answers, and "how many kinds exist" then depends on whether
        -- some name happened to be answered by all of them — a number that silently shrinks with
        -- the question set.
        local out = {{"*=" .. table.concat(kinds, ",")}}
        for _, name in ipairs({{{names}}}) do
            local have = ""
            for i = 1, table.getn(probes) do
                if type(probes[i][name]) == "function" then
                    if have == "" then have = kinds[i] else have = have .. "," .. kinds[i] end
                end
            end
            table.insert(out, name .. "=" .. have)
        end
        return out
    "#
    );
    let rows = script.eval::<Vec<String>>(&chunk).ok()?;
    let mut oracle = MethodOracle::default();
    for (name, have) in rows.iter().filter_map(|row| row.split_once('=')) {
        let list: Vec<String> = have
            .split(',')
            .filter(|k| !k.is_empty())
            .map(str::to_owned)
            .collect();
        if name == "*" {
            oracle.probes = list;
        } else {
            oracle.answered.insert(name.to_string(), list);
        }
    }
    Some(oracle)
}

/// What the live `__index` dispatcher answered, per kind — [`widget_method_kinds`]'s result.
#[derive(Default)]
struct MethodOracle {
    /// The kinds that actually stood up, in probe order. Carried explicitly so "how many kinds are
    /// there" never has to be inferred from the answers.
    probes: Vec<String>,
    /// Wanted name → the kinds that answer it (empty = no widget we ship has it).
    answered: BTreeMap<String, Vec<String>>,
}

/// The names in `wanted` that **no** kind answers — [`AddonReport::missing_methods`]'s rule,
/// unchanged since 1227 so its numbers stay comparable (1209).
///
/// "Found nothing" must stay distinguishable from "ran nothing": an oracle that could not run
/// reports the whole wanted set, which is loud and wrong in the direction that gets noticed, rather
/// than empty and wrong in the direction that does not. A name the oracle somehow did not answer
/// for falls the same way.
fn unresolved_from(oracle: &Option<MethodOracle>, wanted: &BTreeSet<String>) -> Vec<String> {
    let Some(oracle) = oracle else {
        return wanted.iter().cloned().collect();
    };
    wanted
        .iter()
        .filter(|n| oracle.answered.get(*n).is_none_or(Vec::is_empty))
        .cloned()
        .collect()
}

/// The two per-kind lists: what the addon calls **on a kind that cannot answer it**, and what it
/// calls on a receiver nothing could type where the answer *depends* on the kind.
///
/// This is 1228's bound, closed as far as a static scan honestly can — and the residue is printed
/// rather than swallowed. Three outcomes for a wanted name:
///
/// | the call site | the verdict |
/// |---|---|
/// | receiver typed, the kind answers | silent — this is the ordinary case |
/// | receiver typed, the kind does not | `Kind:Name` — a real blocker, whoever else answers it |
/// | receiver untypable, some kinds answer and some do not | `Name (only on …)` — an upper bound |
///
/// **It over-reports and never under-reports, deliberately.** The ambiguous list counts a name once
/// per addon that calls it on *any* untypable receiver, which is a call that might be perfectly
/// fine — `self:SetTexture()` inside a texture wrapper class is not a bug. The alternative is to
/// swallow it as present because *something* answers it, which is precisely how `AddMessage` scored
/// zero for a fortnight while three addons died on it. Its size is therefore a reading of the
/// **attributor's** reach, not of ours: every shape [`scan_receivers`] learns to type moves rows out
/// of it.
///
/// The blocking/feature-tested split is honoured here as it is above (a guarded call is not a
/// blocker), and so is the definition subtraction — `f.Update = function … f:Update()` on a frame
/// the addon made is the corpus's most common idiom, and its receiver is genuinely a widget, so
/// without the subtraction every one of those would be a `Frame:Update` row.
fn per_kind_rows(
    script: &UiScript,
    oracle: &Option<MethodOracle>,
    wants: &Wants,
) -> (Vec<String>, Vec<String>) {
    let empty = Vec::new();
    let answered = |name: &str| -> &[String] {
        oracle
            .as_ref()
            .and_then(|o| o.answered.get(name))
            .unwrap_or(&empty)
            .as_slice()
    };
    // Every typed call site, as `name → the kinds it was called on`.
    let mut typed: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (kind, name) in &wants.kind_calls {
        if wants.wanted_methods.contains(name) {
            typed.entry(name).or_default().insert(kind);
        }
    }
    // ...plus the published names, resolved against the live arena rather than guessed.
    let mut untyped_by_name: BTreeSet<&str> = BTreeSet::new();
    for (receiver, name) in &wants.global_calls {
        if !wants.wanted_methods.contains(name) {
            continue;
        }
        match script.widget_kind(receiver) {
            Some(kind) => {
                typed.entry(name).or_default().insert(kind);
            }
            // Nothing publishes that name, so the receiver is a runtime value this scan cannot
            // follow — the same standing as `self:Foo()`.
            None => {
                untyped_by_name.insert(name);
            }
        }
    }

    let mut kind_missing: Vec<String> = Vec::new();
    for (name, called_on) in &typed {
        let have = answered(name);
        for kind in called_on {
            if !have.iter().any(|k| k == kind) {
                kind_missing.push(format!("{kind}:{name} ({})", elsewhere(have)));
            }
        }
    }

    // A name is ambiguous only if it has an UNTYPED call site in this addon (a name every one of
    // whose call sites was typed has already been answered above) and the answer actually depends
    // on the kind. A name no kind answers is not ambiguous — it is missing, and it is already in
    // `missing_methods`.
    let probes: &[String] = oracle.as_ref().map_or(&[], |o| o.probes.as_slice());
    let mut ambiguous: Vec<String> = Vec::new();
    for name in wants
        .wanted_methods
        .iter()
        .filter(|n| wants.loose_methods.contains(*n) || untyped_by_name.contains(n.as_str()))
    {
        let have = answered(name);
        if !have.is_empty() && have.len() < probes.len() {
            ambiguous.push(format!("{name} ({})", only_on(have, probes)));
        }
    }
    kind_missing.sort();
    (kind_missing, ambiguous)
}

/// `(on ScrollingMessageFrame)` / `(on no kind)` — who *does* answer a name the called kind does not.
///
/// The distinction is the whole point of the row: "on no kind" is a verb to write, "on
/// ScrollingMessageFrame" is a verb we already wrote and forgot to wire to its sibling, and those
/// are different jobs.
fn elsewhere(have: &[String]) -> String {
    match have.len() {
        0 => "on no kind".to_string(),
        1..=4 => format!("on {}", have.join(", ")),
        n => format!("on {} +{} more", have[..3].join(", "), n - 3),
    }
}

/// `(only on Texture)` / `(missing on Texture, FontString)` — whichever side of the split is
/// shorter, so a reader can triage the row at a glance. Deterministic, because it is part of a
/// ranking key.
fn only_on(have: &[String], probes: &[String]) -> String {
    if have.len() * 2 <= probes.len() {
        // Never truncated, unlike the other side: the length of this list IS the row's rank
        // ([`ambiguous_method_demand`] counts its separators), and it is bounded by construction —
        // this branch only runs when at most half the kinds answer.
        format!("only on {}", have.join(", "))
    } else {
        let lack: Vec<&str> = probes
            .iter()
            .map(String::as_str)
            .filter(|k| !have.iter().any(|h| h == k))
            .collect();
        match lack.len() {
            0..=4 => format!("missing on {}", lack.join(", ")),
            n => format!("missing on {} +{} more", lack[..3].join(", "), n - 3),
        }
    }
}

/// Drive the client's own session-start sequence over a loaded addon and report what its HANDLERS
/// raised — the errors no other number here can see.
///
/// The order is the reference's, byte-verified inside `UI_Init 0x48fbf0` and already pinned by
/// `ui_script::addons`' own test: every addon's `ADDON_LOADED`, then `VARIABLES_LOADED`, then
/// `PLAYER_LOGIN`; `PLAYER_ENTERING_WORLD` follows in the cascade. Then a few ticks, because a
/// great deal of addon code runs from `OnUpdate` or from something scheduled on the first one.
///
/// **Deliberately not `ui_script::finish_ui_load`**, which is the production path: that also runs
/// `load_saved_variables`, which reads the machine's real `BENILLA_HOME`. A survey must not depend
/// on — or write to — the director's own saved variables, so the events are fired directly.
///
/// **Ten ticks of 0.1 s, and the bound is the point.** An addon with a busy `OnUpdate` would
/// otherwise run for as long as we let it, and 218 VMs multiply whatever that is. One simulated
/// second reaches the common `ScheduleEvent(..., 0)`/`(..., 0.05)` shapes and Ace's own one-second
/// `AceEvent_FullyInitialized` timer; it does not reach a ten-second self-heal, and that
/// under-report is stated rather than hidden.
fn drive_session_start(script: &mut UiScript, name: &str, deps: &[String]) -> Vec<String> {
    // **Every addon in the VM gets its OWN `ADDON_LOADED`, in load order** — the client fires one
    // per loaded addon with that addon's folder in `arg1`, and a dependency's initialiser is
    // almost always gated on exactly that:
    //
    //     -- Atlas.lua:326
    //     if (event == "ADDON_LOADED" and arg1 == "Atlas") then Atlas_Init(); end
    //
    // `Atlas_Init` is what assigns `AtlasOptions` (l.199). Firing only the SURVEYED addon's name
    // meant that guard never passed for a dependency, so `FuBar_AtlasFu` met an Atlas that had
    // loaded its files and never initialised — and died at `AtlasButton.lua:30` on a nil
    // `AtlasOptions`, a fault the survey then recorded against FuBar_AtlasFu. The state the real
    // client presents was never reachable.
    //
    // Fired BEFORE the error mark, deliberately: a dependency's own handler raising is the
    // dependency's row, not its consumers'. That is this module's stated rule for load errors
    // (`load_dependencies`) and there is no reason session errors should differ — charging it here
    // would count one library's fault once per addon that embeds it, which is the whole reason
    // one-VM-per-addon exists.
    //
    // **Attribution is by the RAISING CHUNK now, not by which event window the raise fell in**
    // (decision 1226 — recorded when this was still window-based, fixed here). The window proxy
    // broke on the shape it was written for: AceAddon drains its ENTIRE `nextAddon` queue on any
    // `ADDON_LOADED` it sees (`AceAddon-2.0.lua:104-105`) and calls each consumer's
    // `OnInitialize` there (`:230`). So the SURVEYED addon's own code runs inside a DEPENDENCY's
    // window, and firing deps outside the mark charged those raises to nobody — the whole
    // FuBar/Ace family was silently OVER-reported as surviving. `Region:SetParent` landing made
    // FuBar_FuXPFu flip `ok -> fail` and that looked like a regression; it was this.
    //
    // Every `ADDON_LOADED` fires inside the mark now, and the dependency exemption is applied
    // afterwards by asking WHOSE FILE raised. Chunk names have been truthful since 1217
    // (`@Interface\AddOns\<Folder>\<File>`), which is what makes the honest key available at all.
    let before = script.errors().len();
    for dep in deps {
        script.fire_event(
            "ADDON_LOADED",
            vec![benilla_ui::script::ScriptValue::Str(dep.clone())],
        );
    }
    script.fire_event(
        "ADDON_LOADED",
        vec![benilla_ui::script::ScriptValue::Str(name.to_string())],
    );
    for event in ["VARIABLES_LOADED", "PLAYER_LOGIN", "PLAYER_ENTERING_WORLD"] {
        script.fire_event(event, Vec::new());
    }
    for _ in 0..10 {
        script.tick(0.1);
    }
    let raised = script.errors().split_off(before);
    raised
        .into_iter()
        .filter(|e| !raised_inside_a_dependencys_own_file(e, name, deps))
        .collect()
}

/// Does this raise belong to a DEPENDENCY's file rather than the surveyed addon's?
///
/// The rule `load_dependencies` already states for load errors, now enforceable for session errors
/// too: *a library that fails is its own row; blaming its consumers would count one fault once per
/// addon that embeds it.* Only the FIRST line is consulted — that is the raise site; the frames
/// below it are the call path, and a consumer calling into a library that then raises is still the
/// library's fault, exactly as it is at load time.
///
/// **Conservative on purpose.** A chunk that names no addon folder at all — our own FrameXML, an
/// `[string "Frame:OnEvent"]` handler — is KEPT, because the surveyed addon is what drove it. Only
/// a chunk that positively names one of this addon's own dependencies is dropped. Getting that
/// backwards would swing the column the other way, and the whole point of 1226 is that a proxy
/// which errs silently in one direction is how the number drifted in the first place.
fn raised_inside_a_dependencys_own_file(err: &str, name: &str, deps: &[String]) -> bool {
    let Some(first) = err.lines().next() else {
        return false;
    };
    // The surveyed addon's own folder always wins, even when a dependency's name is a substring of
    // a path inside it.
    if first.contains(&format!("\\{name}\\")) || first.starts_with(&format!("{name}\\")) {
        return false;
    }
    deps.iter()
        .any(|d| first.contains(&format!("\\{d}\\")) || first.starts_with(&format!("{d}\\")))
}

/// Invoke the reference's own UI entry points, so an addon's OVERRIDES actually execute.
///
/// **The blind spot this closes was found by the director, not by any number here.** They installed
/// Bagnon, saw it listed in the AddOns window, and still got the stock bags: it had replaced
/// `ToggleBackpack` and nothing in the client ever called that name. Every column in this survey
/// said the addon was fine, because the survey loads addons and fires events and **never clicks
/// anything**. An override that is installed and never invoked is indistinguishable here from one
/// that works.
///
/// So this drives a small set of entry points that real corpus addons are known to replace:
///
/// | entry point | who replaces it |
/// |---|---|
/// | `ToggleBackpack` | Bagnon (`Bagnon_Core/core/Overrides.lua`) |
/// | `UnitFrame_OnEnter`/`OnLeave` | TipBuddy (`TipBuddy.lua:2770`) |
/// | `ActionButton_Update` | zBar, zBarEx, CT_BarMod |
///
/// **Bounded on purpose, like the ten ticks above.** These three families are the ones with a
/// measured corpus hooker; driving the whole UI would be a different program, and an unbounded
/// probe over 218 VMs is how a survey stops finishing. Each call is guarded on the global existing
/// and wrapped so a raise is RECORDED rather than aborting the probe — the error is the finding.
///
/// What it still cannot see: whether the override produced the right *result*. It proves the body
/// ran, not that the bags look right. That remains the director's eye.
fn drive_ui_probe(script: &mut UiScript) -> Vec<String> {
    let before = script.errors().len();
    // `this` is set explicitly for the `this`-shaped entry points, because the reference's contract
    // reads it and an addon's replacement will too.
    let _ = script.run(
        r#"
        -- The pcall CAPTURES the error; it does not discard it. Written the obvious way first
        -- (`pcall(fn)` alone) this probe could never report anything at all — it drove the
        -- overrides correctly and swallowed every raise, so the corpus showed a spotless column
        -- forever. `the_ui_probe_records_what_an_override_raises` is the test that caught it, and
        -- it exists because a silent instrument is worse than no instrument.
        BENILLA_PROBE_ERRORS = {}
        local function try(fn)
            if type(fn) == "function" then
                local ok, err = pcall(fn)
                if not ok then
                    table.insert(BENILLA_PROBE_ERRORS, tostring(err))
                end
            end
        end
        -- Open, then close: a toggle left open would leak state into nothing here, but the second
        -- call is what exercises an override's close path.
        try(ToggleBackpack); try(ToggleBackpack)
        if PlayerFrame then
            this = PlayerFrame
            try(UnitFrame_OnEnter); try(UnitFrame_OnLeave)
        end
        if ActionButton1 then
            this = ActionButton1
            try(ActionButton_Update)
        end
        -- HOVER. Nothing above puts a tooltip on screen, and a whole class of addon only runs
        -- there: hooks on GameTooltip's OnShow/OnHide, the FrameXML globals an addon replaces
        -- (GameTooltip_SetDefaultAnchor), and the ones that SCRAPE the line regions
        -- (`GameTooltipTextLeft1:GetText()`). Decision 1220 fixed a raise squarely in that class
        -- and this column could not see it, which is the reason this block exists.
        --
        -- Each call is guarded on the global being a FUNCTION, not merely non-nil: an unguarded
        -- call to a missing FrameXML global would land in every addon's probe column as that
        -- addon's fault, which is exactly the mis-attribution 1209 was written about.
        if GameTooltip and UIParent then
            if type(GameTooltip_SetDefaultAnchor) == "function" then
                try(function() GameTooltip_SetDefaultAnchor(GameTooltip, UIParent) end)
            end
            try(function()
                GameTooltip:SetOwner(UIParent, "ANCHOR_NONE")
                GameTooltip:SetText("benilla probe")
                GameTooltip:Show()
            end)
            try(function() GameTooltip:Hide() end)
        end
        this = nil
    "#,
    );
    let mut out = script.errors().split_off(before);
    // The captured raises, in the order they happened.
    if let Ok(n) = script.eval::<i64>("return table.getn(BENILLA_PROBE_ERRORS)") {
        for i in 1..=n {
            if let Ok(e) = script.eval::<String>(&format!("return BENILLA_PROBE_ERRORS[{i}]")) {
                out.push(e);
            }
        }
    }
    out
}

/// Load an addon's declared dependencies into the VM, depth-first, each at most once.
///
/// The reference's own order (`AddOn_Load 0x51f240`, 1191 §2): **OptionalDeps first, failures
/// ignored; RequiredDeps next, a failure aborts the dependent's load; then its own files.** Both
/// halves are walked, in that order; the harness does not reproduce the *abort*, only the **state**
/// a dependent's file scope actually meets, and it reports a missing required dependency separately
/// ([`AddonReport::missing_deps`]) while an absent optional one is silent, as the client's is.
///
/// This doc claimed the same thing from 1193 onward while the code read `dependencies()` only —
/// the third doc-vs-code lie this arc has found in its own instruments (the others: a module doc
/// claiming it printed which mode a run was in, and a comment claiming the leading-capital filter
/// covered Lua locals). **A sentence describing behaviour is a claim, and an unverified claim in a
/// comment is worse than none, because it stops the next reader checking.**
///
/// Errors inside a dependency are **not** attributed to the dependent. A library that fails is its
/// own row in this survey; blaming its consumers would count one fault N times, which is the
/// mistake the one-VM-per-addon rule exists to prevent.
fn load_dependencies(
    script: &UiScript,
    root: &Path,
    toc: &Toc,
    installed: &BTreeSet<String>,
    seen: &mut BTreeSet<String>,
    loaded: &mut Vec<String>,
) {
    // OPTIONAL first, then required — the reference's own order, and the half this walk was
    // missing. The doc above has claimed since 1193 that the two are "folded together"; only
    // `dependencies()` was ever read, so `## OptionalDeps: FuBar, Ace2` did nothing and the survey
    // met a state the real client never produces. That is the exact bug `load_dependencies` exists
    // to prevent, in the half nobody checked. **130 corpus addons declare optional deps**, and it
    // is how the whole FuBar family gets AceLibrary: `FuBar_BagFu`'s `.toc` lists
    // `FuBarPlugin-2.0.lua` BEFORE `AceLibrary.lua`, and FuBarPlugin raises
    // "FuBarPlugin-2.0 requires AceLibrary." unless the `Ace2` addon loaded first.
    let deps: Vec<&str> = toc
        .optional_dependencies()
        .into_iter()
        .chain(toc.dependencies())
        .collect();
    for dep in deps {
        let key = dep.to_ascii_lowercase();
        if !installed.contains(&key) || !seen.insert(key) {
            continue; // not installed (already reported), or already in this VM
        }
        let Some(dep_toc) = manifest_path(root, dep)
            .and_then(|p| std::fs::read(p).ok())
            .map(|b| Toc::parse(&benilla_ui::source::decode(&b)))
        else {
            continue;
        };
        load_dependencies(script, root, &dep_toc, installed, seen, loaded);
        let _ = load_addon_files(script, root, dep, &dep_toc);
        // Recorded AFTER its own files, and after its own dependencies — the order the client
        // fires `ADDON_LOADED` in, which is the order `drive_session_start` replays.
        loaded.push(dep.to_string());
    }
}

/// Template names the addon passes to `CreateFrame` that the VM cannot resolve.
///
/// Scanned rather than traced, for `missing_globals`' reason: the calls overwhelmingly happen in
/// handlers a load-time survey never fires. The pattern is narrow — a **string literal** in the
/// fourth argument position — so `CreateFrame(t, n, p, someVariable)` is invisible here, which is
/// the honest under-report to make. It is also not comment-stripped: a `CreateFrame` inside a
/// comment counts, and that over-report is the cheaper error of the two.
fn missing_templates(script: &UiScript, root: &Path, name: &str, toc: &Toc) -> Vec<String> {
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    for path in source_files(root, name, toc) {
        let Some(text) = read_text(root, &path) else {
            continue;
        };
        for (i, _) in text.match_indices("CreateFrame") {
            let rest = &text[i + "CreateFrame".len()..];
            let Some(open) = rest.find('(') else { continue };
            let Some(close) = rest[open..].find(')') else {
                continue;
            };
            let Some(fourth) = rest[open + 1..open + close].split(',').nth(3) else {
                continue;
            };
            let f = fourth.trim();
            // A quoted literal, either quote style. `inherits` is a comma-separated LIST in
            // FrameXML and `CreateFrame` takes the same shape, so split it.
            let lit = f
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| f.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')));
            if let Some(lit) = lit {
                wanted.extend(
                    lit.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                );
            }
        }
    }
    wanted
        .into_iter()
        .filter(|name| !script.has_framexml_template(name))
        .collect()
}

/// Template names the addon names in an XML `inherits=` that the VM cannot resolve.
///
/// [`missing_templates`]'s twin over the *other* way a template is asked for, and the one that
/// turned out to carry the weight (see [`AddonReport::missing_inherits`]). Same discipline as
/// every scanner here: a plain attribute read, not comment-stripped, over the addon's XML only.
///
/// Two deliberate rules, both of which the CreateFrame scanner does not need:
///
/// - **A name registered as a FONT resolves.** `inherits=` is one attribute over two namespaces,
///   and `<FontString inherits="GameFontNormal">` is the single most common use of it in any
///   corpus. Asking only the template registry would bury the real answer under font names.
/// - **`virtual="true"` elements the addon declares itself are already in the registry**, because
///   this runs after its files have loaded — the same ordering `missing_templates` relies on.
fn missing_inherits(script: &UiScript, root: &Path, name: &str, toc: &Toc) -> Vec<String> {
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    for path in source_files(root, name, toc) {
        if !path.to_ascii_lowercase().ends_with(".xml") {
            continue;
        }
        let Some(text) = read_text(root, &path) else {
            continue;
        };
        for (i, _) in text.match_indices("inherits=") {
            let rest = &text[i + "inherits=".len()..];
            let quote = match rest.chars().next() {
                Some(q @ ('"' | '\'')) => q,
                _ => continue,
            };
            let Some(end) = rest[1..].find(quote) else {
                continue;
            };
            wanted.extend(
                rest[1..1 + end]
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            );
        }
    }
    wanted
        .into_iter()
        .filter(|n| !script.has_framexml_template(n) && !script.has_font_object(n))
        .collect()
}

/// Every source file an addon reaches — its manifest entries plus the `<Script file=>`/`<Include>`
/// tree hanging off them. An addon's real Lua often hangs off its XML rather than its `.toc`, the
/// same trap the 1.12 corpus set in decision 1190.
fn source_files(root: &Path, name: &str, toc: &Toc) -> Vec<String> {
    let mut pending: Vec<String> = toc
        .files
        .iter()
        .map(|f| benilla_ui::loader::join_ref(name, f))
        .collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    while let Some(path) = pending.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        if let Some(text) = read_text(root, &path) {
            let base = path.rfind('/').map_or("", |i| &path[..i]);
            for m in refs_in_xml(&text) {
                pending.push(benilla_ui::loader::join_ref(base, &m));
            }
        }
        out.push(path);
    }
    out
}

/// The FrameXML digest of the interface this survey loaded (`crate::ui_script::framexml_digest`).
///
/// **Print it beside every number.** A survey run is only comparable to another survey run that
/// loaded the same interface, and in a dev build `assets/ui` is read from the source tree — so an
/// edit by anything sharing the checkout moves the headline with no rebuild and no announcement.
pub fn framexml_digest() -> String {
    crate::ui_script::framexml_digest()
}

/// The real `GlobalStrings.lua`, read once off the install's patch chain.
///
/// **~5,000 globals the surveyed VM would otherwise not have**, and the difference between an
/// instrument that models a session and one that models a blank slate. `FACTION_ALLIANCE`,
/// `PLAYER_OF_REALM`, `LEVEL`, every `ERR_*` — an addon reads them at file scope constantly, and
/// AceDB-2.0 alone builds its per-realm key out of `FACTION_ALLIANCE` before anything else runs.
///
/// Read once per process rather than per addon: it is a megabyte of Lua and the survey stands up
/// 218 VMs. `None` when there is no install, in which case the survey still runs and the numbers
/// are simply worse — and this says which mode a run was in, so two numbers taken on different
/// machines are never quietly compared.
pub fn seated_with_global_strings() -> bool {
    global_strings().is_some()
}

/// The real `GlobalStrings.lua`, or `None` with no install to read it from.
fn global_strings() -> Option<&'static str> {
    use std::sync::OnceLock;
    static SRC: OnceLock<Option<String>> = OnceLock::new();
    SRC.get_or_init(|| {
        let data = benilla_formats::wow_data()?;
        let mut chain = benilla_formats::open_chain(&data).ok()?;
        let bytes = chain
            .read_file("Interface\\FrameXML\\GlobalStrings.lua")
            .ok()?;
        Some(benilla_ui::source::decode(&bytes).into_owned())
    })
    .as_deref()
}

/// Put a **player and a realm** in the VM before the addon loads (decision 1195).
///
/// Not decoration, and not optimism: the reference runs `AddOn_Load` from inside `UI_Init`, which
/// is *after* the world is entered, so an addon's file scope always sees a real character. A bare
/// VM answers `UnitName("player")` with nil, and the corpus's single most common opening line is
///
/// ```lua
/// local charID = string.format(PLAYER_OF_REALM, UnitName("player"), GetRealmName())
/// ```
///
/// — AceDB-2.0's, embedded in a large slice of the ecosystem. Without a seated session that is 24
/// addons failing on a condition that cannot occur in a real client, which would make the harness
/// pessimistic in exactly the way §4 of 1193 caught it being optimistic. The numbers are only
/// worth quoting if the VM is shaped like the session an addon will actually meet.
///
/// Deliberately minimal — a name, a realm, a level, a class — because everything beyond that is
/// state an addon reads *in handlers*, which this survey never fires.
fn seat_a_session(script: &mut UiScript) {
    // The reference boots FrameXML with this file FIRST; so does our app. Before the survey did,
    // an addon's `FACTION_ALLIANCE` was nil and the failure looked like our bug.
    if let Some(src) = global_strings() {
        let _ = script.run(src);
    }
    // The shipped CVar table, before the realm below writes into it — the survey never registered
    // any, so `GetCVar` answered nil for every name the client actually ships. Same class as the
    // empty addon registry: a state the real client cannot be in.
    script.register_cvars(crate::cvars::REGISTERED.iter().copied());
    script.set_realm_name("Harness");
    // THE BIND POINT. `GetBindLocation()` answered `""` in every VM, and a logged-in character with
    // no hearth location is not a state one is in — the server sends `SMSG_BINDPOINTUPDATE` at
    // login, before any addon runs. Same argument as the purse and the nil faction group.
    //
    // Three corpus addons read it, in three separate files (FuBar_TransporterFu, Necrosis,
    // _LazyPig), and one of them CONCATENATES the result — so an empty seat is the difference
    // between exercising their path and not.
    script.set_bind_location("Stormwind City");
    script.set_unit(
        "player",
        Some(benilla_ui::script::UnitState {
            exists: true,
            name: Some("Harness".into()),
            health: 100,
            max_health: 100,
            level: 60,
            power_type: 0,
            power: 100,
            max_power: 100,
            race: Some("Human".into()),
            race_file: Some("Human".into()),
            class: Some("Warrior".into()),
            class_file: Some("WARRIOR".into()),
            sex: 2,
            is_player: true,
            // `UnitFactionGroup("player")` — nil here is not "no faction", it is a state a real
            // player character cannot be in. AceDB-2.0 builds its per-realm key as
            // `realm .. " - " .. faction` at file scope, so a nil faction is 24 addons stopping on
            // `attempt to concatenate local 'faction'`. Every playable race has a side.
            faction_group: Some("Alliance".into()),
            ..Default::default()
        }),
    );

    // ── A POPULATED world, not merely an inhabited one ──────────────────────────────────────
    //
    // The columns above answer "did it raise"; the render column answers "did it draw". Until
    // now the survey seated a player into an EMPTY world — no buffs, no target, no live
    // cooldown — so a buff bar, a target frame or a cooldown-text addon drew nothing and landed
    // in the drew-nothing list beside the pure libraries. `CT_BuffMod` declares 29 frames with
    // none hidden at birth and `ElkBuffBar` 3: neither was failing to build, both had nothing to
    // put in them.
    //
    // That made "nothing to draw" and "failed to draw" share a row, and the second is the one
    // worth finding. Seating content separates them — and, more usefully, exposes the addons
    // that only fail WHEN there is something to draw, which no empty-world column can reach.
    //
    // Deliberately minimal and deliberately ordinary: one buff, one target, one occupied action
    // with a running cooldown. Not a stress fixture — a Tuesday.
    script.set_auras(
        "player",
        Some(vec![benilla_ui::script::AuraState {
            spell_id: 1243,
            name: Some("Power Word: Fortitude".into()),
            icon: Some("Interface\\Icons\\Spell_Holy_WordFortitude".into()),
            count: 1,
            helpful: true,
            cancelable: true,
            ..Default::default()
        }]),
    );
    script.set_unit(
        "target",
        Some(benilla_ui::script::UnitState {
            exists: true,
            name: Some("Target Dummy".into()),
            health: 80,
            max_health: 100,
            level: 60,
            power_type: 0,
            power: 50,
            max_power: 100,
            class: Some("Warrior".into()),
            class_file: Some("WARRIOR".into()),
            ..Default::default()
        }),
    );
    script.set_action(
        1,
        Some(benilla_ui::script::ActionSlot {
            texture: Some("Interface\\Icons\\Ability_SteelMelee".into()),
            kind: 0x00,
            action: 100,
            count: 0,
        }),
    );
    script.set_action_state(
        1,
        Some(benilla_ui::script::ActionState {
            usable: true,
            // (startTime ms, duration ms, enable) — a cooldown with time left on it, which is
            // what a cooldown-text addon (OmniCC, CooldownCount) needs before it draws anything.
            cooldown: Some((1, 30_000, true)),
            ..Default::default()
        }),
    );
    // A SPELLBOOK. A level-60 warrior with an empty one is a state no real character is in — the
    // same argument this fixture already makes for a nil `UnitFactionGroup` and an empty CVar
    // table, and it costs an addon the same way: `TheoryCraftEngine.lua:306` is
    // `name, texture, offset, numSpells = GetSpellTabInfo(1)` then `for i=1, numSpells`, so an
    // empty book is `'for' limit must be a number` and the addon dies at session start. The verb
    // was never the problem — `GetSpellTabInfo` already returns all four values — the FIXTURE was.
    //
    // Deliberately modest, and the reason is 1209's: a fixture that seats everything stops
    // resembling a session anyone has, and every row it lights up is one nobody can attribute. Two
    // tabs and four spells is a Tuesday. `Attack` is slot 1 because it is on every warrior's, and
    // three corpus addons look for exactly that name.
    // THE QUEST LOG. `GetNumQuestLogEntries()` answered `0, 0`, so every quest addon's walk ran
    // zero times — the same shape as the 0-copper purse below and the empty spellbook: not a bug
    // anywhere, just a path nothing ever entered. Five corpus addons walk this API (AtlasQuest,
    // EQL3, FuBar_QuestsFu, QuestHistory, QuestItem).
    //
    // A HEADER AND TWO QUESTS, and each piece is here to be a shape the API distinguishes rather
    // than to be plausible scenery:
    //  · a zone HEADER row, because `GetQuestLogTitle`'s `isHeader` is a different row kind and an
    //    addon that indexes rows without checking it walks straight into one;
    //  · an IN-PROGRESS quest with two objectives, one finished and one not — so a leaderboard walk
    //    sees both states, and `%d/%d` progress is mid-way rather than 0 or complete;
    //  · a COMPLETE quest (`complete = 1`), the other end of `isComplete`'s 1/-1/nil.
    // Not seated: a FAILED quest (-1) and a TIMED one. Both are real states, and both are states a
    // character is only briefly in — 1209's rule that a row nobody can attribute is worth less than
    // one nobody lit applies to fixtures too.
    {
        use benilla_ui::script::{QuestLogEntryView, QuestLogObjectiveView, QuestLogState};
        let objective = |text: &str, cur: u32, req: u32| QuestLogObjectiveView {
            text: text.into(),
            kind: "monster".into(),
            finished: cur >= req,
            cur,
            req,
        };
        let header = QuestLogEntryView {
            quest_id: 0,
            title: "Elwynn Forest".into(),
            is_header: true,
            ..Default::default()
        };
        let in_progress = QuestLogEntryView {
            quest_id: 62,
            title: "The Fargodeep Mine".into(),
            level: 10,
            objectives: vec![
                objective("Kobold Miner slain: 8/12", 8, 12),
                objective("Kobold Vermin slain: 6/6", 6, 6),
            ],
            ..Default::default()
        };
        let done = QuestLogEntryView {
            quest_id: 176,
            title: "Kobold Candles".into(),
            level: 8,
            complete: 1,
            ..Default::default()
        };
        script.set_quest_log(QuestLogState {
            entries: vec![header, in_progress, done],
            num_quests: 2,
            detail: None,
        });
    }

    // THE PURSE. `GetMoney()` answered **0 copper** for every addon in every VM — and a level-60
    // with literally no money is not a state a character is in; it is the same "state the real
    // client cannot be in" the nil `UnitFactionGroup` below is seated for. Money addons are a whole
    // genre in this corpus (Accountant, the CT_* expense trackers, every auction and vendor addon),
    // and they all compute with this number.
    //
    // **12_345_678 copper — 1234g 56s 78c — and the digits are the point.** A round figure is what
    // a fixture reaches for and it is exactly the value that hides bugs: any of `gold`, `silver`
    // and `copper` being zero lets a broken coin-format or a `mod`/`floor` slip read as correct.
    // All three non-zero means a formatter that drops a field, or divides in the wrong order, has
    // nowhere to hide. Non-zero also matters on its own: an addon that guards `if GetMoney() > 0`
    // never ran its body here.
    script.set_money(12_345_678);

    // EQUIPPED GEAR. Every `GetInventoryItem*("player", slot)` answered the empty shape, and a
    // level-60 with no equipment at all is a state no character reaches — the same argument the
    // spellbook and the backpack landed on.
    //
    // Three slots, not nineteen: head, chest and main hand. A fully-geared doll would be a state a
    // real session presents, but it would also light up every path at once, and 1209's rule is
    // that a row nobody can attribute is worth less than one nobody lit. Three is enough for an
    // addon that WALKS the slots to reach real items, empty ones, and the ammo slot's absence.
    {
        let mut slots: benilla_ui::script::InventorySlots = Default::default();
        slots[1] = Some(equip_slot(12640, "Lionheart Helm"));
        slots[5] = Some(equip_slot(11726, "Bloodmail Hauberk"));
        slots[16] = Some(equip_slot(871, "Flurry Axe"));
        script.set_inventory_slots(slots);
    }
    // THE BACKPACK. `GetContainerNumSlots(0)` answered 0 — and a character with no backpack has
    // never existed: bag 0 is 16 slots from level 1, before a single bag is bought. Same argument
    // as the spellbook above and the nil `UnitFactionGroup` below it, and the same expectation:
    // seating a state every character is in should LOSE rows, because a path nothing walked is a
    // path nothing tested.
    //
    // Backpack only, and deliberately: bags 1..4 are equipped bags, which a fresh character does
    // NOT have, so seating them would manufacture a state rather than expose one. Two items in
    // sixteen slots — the rest empty, which is itself a shape bag addons must handle.
    {
        let mut slots = std::collections::HashMap::new();
        slots.insert(1, bag_slot(6948, "Hearthstone", 1));
        slots.insert(5, bag_slot(2589, "Linen Cloth", 12));
        script.set_container(
            0,
            Some(benilla_ui::script::ContainerState {
                name: Some("Backpack".into()),
                num_slots: 16,
                slots,
            }),
        );
    }
    script.set_spellbook(benilla_ui::script::SpellBookState {
        tabs: vec![
            benilla_ui::script::SpellTabView {
                name: "General".into(),
                texture: Some("Interface\\Icons\\INV_Misc_QuestionMark".into()),
                offset: 0,
                num_spells: 2,
            },
            benilla_ui::script::SpellTabView {
                name: "Arms".into(),
                texture: Some("Interface\\Icons\\Ability_Rogue_Eviscerate".into()),
                offset: 2,
                num_spells: 2,
            },
        ],
        slots: vec![
            spell_slot(6603, "Attack", None),
            spell_slot(78, "Heroic Strike", Some("Rank 1")),
            spell_slot(100, "Charge", Some("Rank 1")),
            spell_slot(772, "Rend", Some("Rank 1")),
        ],
    });
}

/// One seated equipment slot — full durability, so no alert region lights up.
fn equip_slot(item_id: u32, name: &str) -> benilla_ui::script::InvSlotView {
    benilla_ui::script::InvSlotView {
        item_id,
        icon: Some("Interface\\Icons\\INV_Misc_QuestionMark".into()),
        count: 1,
        quality: 2,
        name: Some(name.to_string()),
        // FULL durability on purpose. The setter recomputes the eleven alert statuses and fires
        // UPDATE_INVENTORY_ALERTS, so a worn item would light DurabilityFrame's regions — a
        // VISIBLE change, and this fixture's job is to present a plausible session, not to drive
        // the alert law. Undamaged gear is the ordinary case anyway.
        durability: Some((100, 100)),
        link: Some(format!("|cff1eff00|Hitem:{item_id}:0:0:0|h[{name}]|h|r")),
        ..Default::default()
    }
}

/// One seated backpack slot.
fn bag_slot(item_id: u32, name: &str, count: u32) -> benilla_ui::script::ContainerSlot {
    benilla_ui::script::ContainerSlot {
        texture: Some("Interface\\Icons\\INV_Misc_QuestionMark".into()),
        count,
        quality: Some(1),
        item_id,
        link: Some(format!("|cffffffff|Hitem:{item_id}:0:0:0|h[{name}]|h|r")),
        ..Default::default()
    }
}

/// One seated spellbook slot — the fields a book reader actually reads, defaulted otherwise.
fn spell_slot(spell_id: u32, name: &str, rank: Option<&str>) -> benilla_ui::script::SpellSlotView {
    benilla_ui::script::SpellSlotView {
        spell_id,
        name: name.to_string(),
        rank: rank.map(str::to_string),
        texture: Some("Interface\\Icons\\INV_Misc_QuestionMark".into()),
        ..Default::default()
    }
}

/// Every string key in the VM's `_G`.
fn globals_of(script: &UiScript) -> BTreeSet<String> {
    script
        .eval::<Vec<String>>(
            "local out = {} \
             for k in pairs(_G) do if type(k) == 'string' then table.insert(out, k) end end \
             return out",
        )
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// Run the addon's manifest through the same two arms the real loader uses — `.lua` as a chunk,
/// anything else as FrameXML — with the AddOns root as the provider's path space (decision 1186).
fn load_addon_files(script: &UiScript, root: &Path, name: &str, toc: &Toc) -> Vec<String> {
    let provider = |req: &str| -> Option<Vec<u8>> { read_under(root, req) };
    let mut errors = Vec::new();
    for file in &toc.files {
        let path = benilla_ui::loader::join_ref(name, file);
        let Some(bytes) = read_under(root, &path) else {
            errors.push(format!("{file}: not found"));
            continue;
        };
        if is_lua(file) {
            // Named as the client names it, so the survey sees what a player would: an addon
            // that PARSES a traceback for its own folder (the whole FuBar family) needs the real
            // `Interface\AddOns\<Folder>\<File>` shape, not mlua's Rust-caller default.
            if let Err(e) =
                script.run_chunk_named(&bytes, &benilla_ui::script::addon_chunk_name(name, file))
            {
                errors.push(format!("{file}: {e}"));
            }
            continue;
        }
        match benilla_ui::framexml::parse(&benilla_ui::source::decode(&bytes)) {
            Ok(doc) => {
                let report = benilla_ui::loader::load_in(script, &doc, &path, &provider);
                errors.extend(report.errors.into_iter().map(|e| format!("{file}: {e}")));
            }
            Err(e) => errors.push(format!("{file}: {e}")),
        }
    }
    errors
}

fn is_lua(entry: &str) -> bool {
    entry
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(entry)
        .rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("lua"))
}

/// `root/rel`, refusing to escape — the same lexical AddOns-root sandbox the loader applies.
///
/// **Bytes, like the loader's** (decision 1193). Until then this function carried a private
/// lossy-UTF-8 + BOM-strip of its own, so the harness could survey files the *client* refused to
/// load — an instrument reporting on a world its host could not reach, which is the wrong way
/// round. The client reads bytes now, so the harness can simply read bytes too, and the one place
/// that still needs text ([`read_text`]) says so.
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

/// [`read_under`] for the **source scanner**, which greps text rather than running it.
fn read_text(root: &Path, rel: &str) -> Option<String> {
    read_under(root, rel).map(|b| benilla_ui::source::decode(&b).into_owned())
}

/// Names the addon calls like functions that the VM does not have.
///
/// Two subtractions matter and both are easy to get wrong: the addon's **own** definitions (a
/// helper it declares and calls is not a missing API), and Lua's **locals** — `local Foo = function`
/// then `Foo()`. The leading-capital filter alone does not cover the second, because a local can be
/// capitalised, and for a long time this list's top five rows were exactly that mistake (see
/// [`scan_lua`]'s assignment pass). Both `local function Foo` and `local Foo = …` are credited now.
fn missing_calls(
    root: &Path,
    name: &str,
    toc: &Toc,
    known: &BTreeSet<String>,
    dep_methods: &BTreeSet<String>,
) -> Wants {
    let mut scan = Scan::default();
    for path in source_files(root, name, toc) {
        if let Some(text) = read_text(root, &path) {
            scan_source(&path, &text, &mut scan);
        }
    }
    let Scan {
        called,
        indexed,
        methods,
        defined,
        defined_methods,
        tested_fields,
        kind_calls,
        global_calls,
        loose_methods,
    } = scan;
    let absent = |set: BTreeSet<String>| -> Vec<String> {
        set.into_iter()
            .filter(|n| !known.contains(n) && !defined.contains(n))
            .collect()
    };
    // The METHOD candidates are returned raw rather than resolved here: `known` is a set of
    // globals and a method is not one, so the only honest oracle is the VM itself
    // ([`widget_method_kinds`]). Subtracted here is the half a *scanner* can answer — the
    // names this addon's own source binds as fields on its own tables.
    // Split, not filtered away: a name the addon feature-tests is not a blocker, but losing it
    // entirely would be the silent under-report this instrument keeps getting caught in — 60
    // corpus addons guard `SetTopLevel`, which is a real widget method we do not have.
    let (wanted_methods, tested_methods): (BTreeSet<String>, BTreeSet<String>) = methods
        .into_iter()
        .filter(|n| !defined_methods.contains(n) && !dep_methods.contains(n))
        .partition(|n| !tested_fields.contains(n));
    // Kept as separate lists, not one, for 1207's reason: shapes ranked together mis-rank. A
    // missing FUNCTION is a verb to write in Rust; a missing FRAME or TABLE is FrameXML to
    // transcribe; a missing METHOD is a widget binding — three queues, three places.
    Wants {
        missing_globals: absent(called),
        missing_tables: absent(indexed),
        wanted_methods,
        tested_methods,
        kind_calls,
        global_calls,
        loose_methods,
    }
}

/// What one addon's source scan wants — [`missing_calls`]' result.
///
/// A struct rather than the tuple this was: the per-kind census took it from four members to seven,
/// and four of those are `BTreeSet<String>`-shaped, which is how the wrong one gets passed at a
/// call site and nobody sees it. Same reasoning as [`Scan`]'s own.
struct Wants {
    missing_globals: Vec<String>,
    missing_tables: Vec<String>,
    /// Method names the addon calls and does not define — the census's question set.
    wanted_methods: BTreeSet<String>,
    /// ...and the half it feature-tests first, which is not a blocker.
    tested_methods: BTreeSet<String>,
    kind_calls: BTreeSet<(String, String)>,
    global_calls: BTreeSet<(String, String)>,
    loose_methods: BTreeSet<String>,
}

/// `<Script file=>` / `<Include file=>` targets.
fn refs_in_xml(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, _) in text.match_indices("file=\"") {
        let rest = &text[i + 6..];
        if let Some(end) = rest.find('"') {
            out.push(rest[..end].to_string());
        }
    }
    out
}

/// Blank out `<!-- … -->`, preserving line structure — run over an XML file **before**
/// [`strip_lua_noise`], which cannot see them.
///
/// An unterminated `<!--` swallows the rest of the file, which is what a real XML parser does with
/// it too: the document is malformed and there is no honest way to guess where the comment meant
/// to stop.
fn strip_xml_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 4..];
        match after.find("-->") {
            Some(close) => {
                // Keep the newlines so anything reporting a line still reports a real one.
                out.extend(after[..close].chars().filter(|c| *c == '\n'));
                rest = &after[close + 3..];
            }
            None => {
                out.extend(after.chars().filter(|c| *c == '\n'));
                return out;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Blank out Lua comments and string literals, preserving line structure.
///
/// **This is not tidying — it is the difference between a signal and a list of names.** Without
/// it the ranked demand is topped by `Author`, `Iriel`, `Tekkub` and `Knight`: words inside
/// `-- credits` comments and `"..."` messages that happen to sit before a `(`. Measured on the
/// vanilla corpus, stripping removes over a thousand phantom call targets, and every one of the
/// top four was one.
fn strip_lua_noise(text: &str) -> String {
    strip_lua(text, false)
}

/// [`strip_lua_noise`] with the string **contents kept** — comments still go.
///
/// Exactly one question needs this, and it is the one the per-kind census is built on: the widget
/// kind in `CreateFrame("MessageFrame", …)` lives *inside* a string literal, which the ordinary
/// stripper blanks before anything can read it. Reading the raw file instead would count a
/// commented-out `CreateFrame` — the mistake 1218 is about, where five words of GPL boilerplate
/// topped a ranking — so the comments go and only the literals stay.
///
/// It is never used for the *call-site* scan ([`scan_lua`], [`attribute_calls`]): a name inside a
/// string would read as an identifier there, which is the same fault the other way round.
fn strip_lua_comments_only(text: &str) -> String {
    strip_lua(text, true)
}

fn strip_lua(text: &str, keep_strings: bool) -> String {
    let src: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        // Long bracket `[[ … ]]` (a string, or a `--[[ … ]]` comment): both end the same way.
        let long_open = c == '[' && i + 1 < src.len() && src[i + 1] == '[';
        let line_comment = c == '-' && i + 1 < src.len() && src[i + 1] == '-';
        if line_comment && i + 3 < src.len() && src[i + 2] == '[' && src[i + 3] == '[' {
            i += 4;
            while i + 1 < src.len() && !(src[i] == ']' && src[i + 1] == ']') {
                if src[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(src.len());
            continue;
        }
        if line_comment {
            while i < src.len() && src[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if long_open {
            i += 2;
            if keep_strings {
                out.push_str("[[");
            }
            while i + 1 < src.len() && !(src[i] == ']' && src[i + 1] == ']') {
                if keep_strings || src[i] == '\n' {
                    out.push(src[i]);
                }
                i += 1;
            }
            i = (i + 2).min(src.len());
            if keep_strings {
                out.push_str("]]");
            }
            continue;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            let start = i;
            while i < src.len() && src[i] != quote {
                if src[i] == '\\' {
                    i += 1; // an escaped quote does not close the literal
                }
                i += 1;
            }
            if keep_strings {
                out.push(quote);
                out.extend(src[start..i.min(src.len())].iter());
                out.push(quote);
            } else {
                out.push_str("\"\""); // keep it an expression, drop its contents
            }
            i = (i + 1).min(src.len());
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// [`scan_lua`] over one source file, stripped according to what kind of file it is.
///
/// An XML file is scanned as Lua on purpose — its `<Script>` CDATA and its `<OnLoad>` handler
/// bodies ARE Lua, and skipping the file would lose them. But `strip_lua_noise` only knows `--` and
/// `[[ ]]`, so an `<!-- … -->` header survived it whole. XML attribute values happen to be blanked
/// already (they are `"…"`, which the Lua string rule eats) and tag names are never followed by
/// `(`/`.`/`:` — the comment was the entire hole, and it was enough to put five license-boilerplate
/// words at the top of the frames/tables ranking (decision 1218).
fn scan_source(path: &str, text: &str, scan: &mut Scan) {
    let text: std::borrow::Cow<'_, str> = if is_lua(path) {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(strip_xml_comments(text))
    };
    scan_lua(&text, scan);
    // The receiver pass is per FILE, never per addon: a `local f = CreateFrame("MessageFrame")` in
    // one file says nothing about an `f` in the next, and pretending otherwise is how an
    // attribution becomes fiction.
    scan_receivers(&text, scan);
}

/// Every kind `CreateFrame` accepts, in `frame_kind_from_str`'s own spelling — the probe set of the
/// widget-method census, and the vocabulary [`UiScript::widget_kind`] answers in.
///
/// A kind that is refused is simply skipped by the oracle, so this list going stale narrows the
/// probe set rather than producing a wrong answer.
const PROBE_FRAME_KINDS: &[&str] = &[
    "Frame",
    "Button",
    "CheckButton",
    "EditBox",
    "StatusBar",
    "Slider",
    "ScrollFrame",
    "Model",
    "MessageFrame",
    "ScrollingMessageFrame",
    "ColorSelect",
    "SimpleHTML",
    "MovieFrame",
    "GameTooltip",
    "Minimap",
    "Cooldown",
];

/// **Type the receiver of a `:` call where the file says what it is** — the whole basis of the
/// per-kind census.
///
/// The census's oracle used to ask "does *any* widget answer this name", and decision 1228 is the
/// record of what that cost: `MessageFrame:AddMessage` scored **zero** for as long as it existed,
/// because the ScrollingMessageFrame probe answered the name, while three corpus addons had
/// `UIErrorsFrame:AddMessage` as their first load error. A verb wired to one class and forgotten on
/// its sibling is exactly the shape an ANY-kind answer cannot see. The only fix is to ask the
/// question per kind, and that needs a receiver type.
///
/// A static scan cannot type an arbitrary expression, so this types the two shapes that carry the
/// corpus and says nothing about the rest:
///
/// - a **local (or plain global) bound from a widget factory** — `local f = CreateFrame("Frame")`,
///   `local t = f:CreateTexture()`, `local fs = f:CreateFontString()`;
/// - a **published name**, resolved later against the live arena by [`UiScript::widget_kind`] —
///   `UIErrorsFrame:AddMessage(…)` is a MessageFrame call because our `UIErrorsFrame` *is* one.
///   That is deliberately our object graph, not the reference's: if we declared it a plain
///   `<Frame>`, the addon's call really would land on a plain Frame, and the census should say so.
///
/// Everything else — `self:Foo()`, `this:Foo()`, `a.b:Foo()`, `getglobal(n):Foo()` — is left
/// **untyped on purpose** and reported as such ([`AddonReport::ambiguous_methods`]), because
/// swallowing it as "some kind has it, so fine" is the exact blindness this is here to end.
fn scan_receivers(text: &str, scan: &mut Scan) {
    // Pass 1 needs the string literal inside `CreateFrame("MessageFrame")`, which the ordinary
    // stripper blanks; pass 2 must NOT see identifiers inside strings. Two texts, one file.
    let typed = local_widget_kinds(&strip_lua_comments_only(text));
    attribute_calls(&strip_lua_noise(text), &typed, scan);
}

/// Identifiers this file binds to a widget of a known kind — `Some(kind)`, or `None` for a name the
/// file also binds to something else (and therefore cannot be typed).
///
/// **Every binding of a name must agree, and a name that is ever a loop variable or a function
/// parameter is out.** Without that, one `local frame = CreateFrame("Frame")` at file top types
/// every `frame:Method()` in the file — including the `frame` that is a *parameter* of a helper
/// three functions down, holding whatever the caller passed. That is how an attribution pass
/// invents rows, and the corpus is full of exactly that naming.
fn local_widget_kinds(text: &str) -> BTreeMap<String, Option<&'static str>> {
    let mut out: BTreeMap<String, Option<&'static str>> = BTreeMap::new();
    let mut bind = |name: &str, kind: Option<&'static str>| match out.get(name) {
        Some(prev) if *prev == kind => {}
        Some(_) => {
            out.insert(name.to_string(), None);
        }
        None => {
            out.insert(name.to_string(), kind);
        }
    };
    for line in text.lines() {
        let Some(eq) = assignment_at(line) else {
            continue;
        };
        let lhs = line[..eq].trim();
        let lhs = lhs.strip_prefix("local ").unwrap_or(lhs).trim();
        if !is_ident(lhs) {
            continue; // a comma list, a dotted field, an index — not a name we can follow
        }
        bind(lhs, widget_kind_of_expression(&line[eq + 1..]));
    }
    // The POISON pass, over the whole file rather than line by line: every name that is ever a
    // `for` variable or a function parameter is struck out, wherever it was bound. Without it one
    // `local frame = CreateFrame("Frame")` at file top types every `frame:Method()` below —
    // including the `frame` that is a *parameter* of a helper three functions down, holding
    // whatever its caller passed. An untyped name costs a row in the ambiguous table; a wrongly
    // typed one is fiction in the ranking, which is the error this whole arc keeps paying for.
    for name in binder_names(text) {
        bind(&name, None);
    }
    out
}

/// The byte offset of the line's first real `=` (never `==`, `<=`, `>=`, `~=`), or `None`.
fn assignment_at(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    (0..b.len()).find(|&i| {
        b[i] == b'='
            && b.get(i + 1) != Some(&b'=')
            && !matches!(
                i.checked_sub(1).map(|p| b[p]),
                Some(b'=' | b'<' | b'>' | b'~')
            )
    })
}

/// Every identifier the file binds through a `for` header or a `function` parameter list.
fn binder_names(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let b: Vec<char> = text.chars().collect();
    let word_at = |i: usize, w: &str| -> bool {
        let n = w.chars().count();
        i + n <= b.len()
            && b[i..i + n].iter().copied().eq(w.chars())
            && (i == 0 || !(b[i - 1].is_alphanumeric() || b[i - 1] == '_'))
            && b.get(i + n)
                .is_none_or(|c| !(c.is_alphanumeric() || *c == '_'))
    };
    let take = |out: &mut BTreeSet<String>, s: &str| {
        for name in s.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if is_ident(name) {
                out.insert(name.to_string());
            }
        }
    };
    for i in 0..b.len() {
        if word_at(i, "for") {
            // `for i = 1, n do` and `for k, v in pairs(t) do` — the header ends at the `=`/`in`.
            let rest: String = b[i + 3..].iter().take(200).collect();
            let head = rest
                .find(" in ")
                .map(|p| &rest[..p])
                .or_else(|| assignment_at(&rest).map(|p| &rest[..p]))
                .unwrap_or(&rest);
            take(&mut out, head);
        }
        if word_at(i, "function") {
            let rest: String = b[i + 8..].iter().take(400).collect();
            if let Some(open) = rest.find('(') {
                if let Some(close) = rest[open..].find(')') {
                    take(&mut out, &rest[open + 1..open + close]);
                }
            }
        }
    }
    out
}

/// The widget kind a right-hand side **provably** produces, or `None`.
///
/// Only the factory calls, and only with a literal kind: `CreateFrame(kind, …)` with a variable
/// first argument is exactly as untypable as `getglobal(n)` and is treated the same.
fn widget_kind_of_expression(rhs: &str) -> Option<&'static str> {
    // This is the ONE pass that reads un-blanked string literals, so it is also the one pass that
    // has to check it is not standing *inside* one: `local x = "CreateFrame('Button')"` types
    // nothing. An odd number of quotes before the match is the cheap, deterministic test.
    let outside_string =
        |i: usize| rhs[..i].chars().filter(|c| *c == '"' || *c == '\'').count() % 2 == 0;
    if let Some(i) = rhs
        .match_indices("CreateFrame")
        .find(|(i, _)| outside_string(*i))
    {
        let i = i.0;
        // Not `lib.CreateFrame(...)` — a qualified call is some other function of the same name.
        let qualified = rhs[..i].trim_end().ends_with(['.', ':']);
        let after = &rhs[i + "CreateFrame".len()..];
        if !qualified && after.trim_start().starts_with('(') {
            let arg = after[after.find('(')? + 1..].trim_start();
            let quote = arg.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let lit = &arg[1..arg[1..].find(quote)? + 1];
            return PROBE_FRAME_KINDS
                .iter()
                .find(|k| k.eq_ignore_ascii_case(lit))
                .copied();
        }
        return None;
    }
    // The two region leaves have factories of their own, and they are worth typing: a region method
    // called on a region is the single biggest source of "present on some kinds only" noise, and
    // every one of these moves a row OUT of the ambiguous table by answering it properly.
    for (call, kind) in [
        (":CreateTexture", "Texture"),
        (":CreateFontString", "FontString"),
    ] {
        if rhs.match_indices(call).any(|(i, _)| outside_string(i)) {
            return Some(kind);
        }
    }
    None
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

/// Walk `obj:Name(` call sites and file each under the receiver we can (or cannot) type.
fn attribute_calls(text: &str, typed: &BTreeMap<String, Option<&'static str>>, scan: &mut Scan) {
    let src: Vec<char> = text.chars().collect();
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut i = 0;
    while i < src.len() {
        if src[i] != ':' || (i + 1 < src.len() && src[i + 1] == ':') {
            i += 1;
            continue;
        }
        // The METHOD name, right of the colon.
        let mut m = i + 1;
        while m < src.len() && src[m] == ' ' {
            m += 1;
        }
        let start = m;
        while m < src.len() && is_word(src[m]) {
            m += 1;
        }
        let name: String = src[start..m].iter().collect();
        let mut p = m;
        while p < src.len() && src[p] == ' ' {
            p += 1;
        }
        // Same guards as `scan_lua`'s method arm, so the two lists cannot disagree about what a
        // method call is: it must be called, and it must be capitalised.
        if name.is_empty()
            || p >= src.len()
            || src[p] != '('
            || !name.starts_with(|c: char| c.is_uppercase())
        {
            i += 1;
            continue;
        }
        // The RECEIVER, left of the colon: a bare identifier and nothing else. `a.b:C()`,
        // `getglobal(n):C()` and `t[1]:C()` are all real corpus shapes and none of them is typable.
        let mut r = i;
        while r > 0 && src[r - 1] == ' ' {
            r -= 1;
        }
        let end = r;
        while r > 0 && is_word(src[r - 1]) {
            r -= 1;
        }
        let receiver: String = src[r..end].iter().collect();
        let plain =
            !receiver.is_empty() && (r == 0 || !matches!(src[r - 1], '.' | ':' | ']' | ')'));
        if !plain {
            scan.loose_methods.insert(name);
        } else {
            match typed.get(&receiver) {
                Some(Some(kind)) => {
                    scan.kind_calls.insert(((*kind).to_string(), name));
                }
                // The file demonstrably rebinds this name, so it is untypable HERE — and it must
                // not fall through to the published-name lookup either, or a local called `Minimap`
                // would be answered by the global of that name.
                Some(None) => {
                    scan.loose_methods.insert(name);
                }
                // The receiver might still be a PUBLISHED widget — but only the live arena knows,
                // so the name travels to Rust rather than being guessed here.
                None => {
                    scan.global_calls.insert((receiver, name));
                }
            }
        }
        i = p;
    }
}

/// What one pass of [`scan_lua`] found.
///
/// **A struct rather than a row of `&mut BTreeSet` out-parameters**, which is what this was: the
/// scan grew a fourth and fifth set when the widget-method class was made rankable, and five
/// positionally-identical sets in one signature is how the wrong one gets passed and nobody sees it
/// — the exact class of silent instrument fault this arc keeps writing records about.
#[derive(Default)]
struct Scan {
    /// `Foo(` — API-shaped call sites. Missing ones become [`AddonReport::missing_globals`].
    called: BTreeSet<String>,
    /// `Foo.bar` / `Foo:baz` — names the addon *indexes*. → [`AddonReport::missing_tables`].
    indexed: BTreeSet<String>,
    /// `obj:Name(` — method calls, receiver unknowable. → [`AddonReport::missing_methods`].
    methods: BTreeSet<String>,
    /// Globals the addon binds itself, in any of the shapes below.
    defined: BTreeSet<String>,
    /// FIELDS the addon binds itself — `function T:N`, `function T.N`, `T.N = …`. These are the
    /// method definitions a scanner *can* see, and subtracting them is what keeps an embedded OO
    /// library's own `self:Foo()` calls out of the widget-method ranking.
    defined_methods: BTreeSet<String>,
    /// Fields the addon READS without calling — `if self.OnMouseUp then`. A feature-tested method
    /// is one the addon has already written a fallback for, so it is not a blocker; see the arm
    /// that fills this for the two library idioms that made the rule necessary.
    tested_fields: BTreeSet<String>,
    /// `(kind, method)` — call sites [`scan_receivers`] could TYPE from the file itself, because
    /// the receiver was bound by a widget factory: `local f = CreateFrame("MessageFrame")` then
    /// `f:AddMessage(…)`. → [`AddonReport::kind_missing_methods`].
    kind_calls: BTreeSet<(String, String)>,
    /// `(receiver name, method)` — the receiver is a bare identifier the file does not bind, so it
    /// may be a **published** widget. Only the live arena can say ([`UiScript::widget_kind`]), so
    /// the pair travels to Rust unresolved.
    global_calls: BTreeSet<(String, String)>,
    /// Methods called on a receiver nothing can type — `self:Foo()`, `a.b:Foo()`,
    /// `getglobal(n):Foo()`. The size of this set is the honest measure of how much the per-kind
    /// census is *guessing about*, and it is why [`AddonReport::ambiguous_methods`] exists.
    loose_methods: BTreeSet<String>,
}

/// Collect API-shaped call sites and the file's own top-level definitions.
fn scan_lua(text: &str, scan: &mut Scan) {
    let Scan {
        called,
        indexed,
        methods,
        defined,
        defined_methods,
        tested_fields,
        ..
    } = scan;
    let text = &strip_lua_noise(text);
    let bytes: Vec<char> = text.chars().collect();
    let ident = |start: usize| -> (String, usize) {
        let mut i = start;
        while i < bytes.len() && (bytes[i].is_alphanumeric() || bytes[i] == '_') {
            i += 1;
        }
        (bytes[start..i].iter().collect(), i)
    };
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_alphabetic() || bytes[i] == '_' {
            // A qualified name (`self.foo`, `string.format`) is not a global call site — skip the
            // whole chain, except the `C_Thing.Verb` shape which IS one.
            let prev = if i > 0 { bytes[i - 1] } else { '\0' };
            let qualified = prev == '.' || prev == ':';
            let (word, next) = ident(i);
            let mut j = next;
            while j < bytes.len() && bytes[j] == ' ' {
                j += 1;
            }
            let capitalised = word.chars().next().is_some_and(char::is_uppercase);
            if !qualified && j < bytes.len() && bytes[j] == '(' && capitalised {
                called.insert(word.clone());
            }
            // **`obj:Name(` — the widget-method class**, and the third thing an addon can want
            // that we might not have. It is neither a global nor an indexed table, so neither arm
            // above can see it, and until this existed the only trace a missing method left was a
            // ranked error row reading `attempt to call method 'X' (a nil value)` — with the name
            // collapsed away by the normalisation.
            //
            // The receiver is deliberately ignored: a static scan cannot type `obj`. The same
            // leading-capital guard as the other two arms is the only filter, and it is doing the
            // same work — every widget method in the 1.12 surface is capitalised, while an
            // addon's private `obj:doThing()` helpers usually are not.
            if prev == ':' && j < bytes.len() && bytes[j] == '(' && capitalised {
                methods.insert(word.clone());
            }
            if prev == '.' {
                let assigned = bytes.get(j) == Some(&'=') && bytes.get(j + 1) != Some(&'=');
                if assigned {
                    // `T.Name = …` — a field the addon binds on a table it already has. `f.Update
                    // = function` is the corpus's other way of writing a method, and without
                    // crediting it every subsequent `f:Update()` would read as a widget binding we
                    // lack.
                    defined_methods.insert(word.clone());
                } else if bytes.get(j) != Some(&'(') {
                    // **A field the source READS rather than calls is a feature test, and a
                    // feature-tested method is by definition not a blocker** — the addon has
                    // already written the branch it takes when the method is absent.
                    //
                    // This is not a heuristic dressed as a rule; it is the corpus's dominant idiom,
                    // verbatim, in the two libraries that between them owned ten of this ranking's
                    // top sixteen rows:
                    //
                    //     if type(self.OnMouseUp) == "function" then self:OnMouseUp(arg1) end
                    //         -- FuBarPlugin-2.0.lua:768
                    //     elseif type(self) == "table" and self.CompareTo then
                    //         return self:CompareTo(other) == 0
                    //         -- AceOO-2.0.lua:353
                    //
                    // Neither `OnMouseUp` nor `CompareTo` is defined anywhere in the corpus: they
                    // are optional callbacks a *plugin author* may supply, dispatched by hand.
                    // They are not widget methods, they are not missing, and no amount of
                    // definition-scanning could ever have subtracted them — the definition does not
                    // exist. What does exist, one line up, is the addon saying it can live without.
                    //
                    // A dotted CALL (`MyLib.Helper()`) is neither read nor test and lands in
                    // neither set: it is a plain function call through a table, already the
                    // `indexed` arm's business.
                    tested_fields.insert(word.clone());
                }
            }
            // **A name the addon INDEXES is a surface it expects too**, and this scan was blind to
            // every one of them. `ColorPickerFrame.func = …`, `GameTooltip:AddLine(…)`,
            // `ChatFrame1:AddMessage(…)` — a FRAME or a TABLE global, never followed by `(`, so the
            // call arm above cannot see it. A window **86 corpus addons reach** scored exactly 0 on
            // the most-wanted list until this arm existed, and the same blindness hid every other
            // FrameXML frame global.
            //
            // Same two guards as the call arm (unqualified, capitalised) and the same `defined`
            // subtraction, so an addon's own `MyAddon = {}` namespace stays its own.
            if !qualified && j < bytes.len() && (bytes[j] == '.' || bytes[j] == ':') && capitalised
            {
                indexed.insert(word.clone());
            }
            if word == "function" {
                let mut k = next;
                while k < bytes.len() && bytes[k] == ' ' {
                    k += 1;
                }
                if k < bytes.len() && (bytes[k].is_alphabetic() || bytes[k] == '_') {
                    let (fname, mut end) = ident(k);
                    defined.insert(fname);
                    // `function T:N(`, `function T.N(`, `function A.B.C:D(` — every name after the
                    // first is a FIELD bound on a table the addon already has, not a global. This
                    // is how an addon and every embedded Ace/FuBar library declares its methods,
                    // and it is the single biggest subtraction keeping the widget-method ranking
                    // from being a list of `AceEvent-2.0`'s verbs.
                    while end < bytes.len() && (bytes[end] == '.' || bytes[end] == ':') {
                        let (part, next_end) = ident(end + 1);
                        if part.is_empty() {
                            break;
                        }
                        defined_methods.insert(part);
                        end = next_end;
                    }
                }
            }
            i = next;
            continue;
        }
        i += 1;
    }
    // Assignments — `Foo = …`, the other way an addon defines a global it later calls, AND
    // `local Foo = …`, which is not a global at all and is the reason this scan exists in the
    // shape it does.
    //
    // **The local arm is a correction, and it was worth five wrong rows at the top of `demand`.**
    // The doc above claims locals are handled by only counting leading-capital names; they are
    // not, because a local can be capitalised. `local CheckShow = function(self, panelId)` in
    // `FuBarPlugin-2.0.lua` is called four lines later as `CheckShow(...)`, and the scanner read
    // that as a missing API in **74 addons** — the corpus's most-wanted global, ahead of anything
    // real. `DropDownList1_Show`, `WorldFrame_OnMouseDown`, `WorldFrame_OnMouseUp` (60 each) and
    // `ColorPickerOkayButton_OnClick` (51) are the same `local X = <expr>` shape in Dewdrop-2.0
    // and AceConsole-2.0. Five rows, 305 addon-mentions, all phantom; the first true row was
    // `SendAddonMessage` at 24.
    //
    // **The trade, stated:** `defined` is addon-wide while a `local` is file-scoped, so an addon
    // that shadows a real API name in one file now suppresses that name in all of them. That is an
    // under-report, which is the error this instrument's module doc already chooses to prefer —
    // and it is bounded by the addon, where the over-report was unbounded.
    for line in text.lines() {
        let t = line.trim_start();
        let body = t.strip_prefix("local ").unwrap_or(t);
        let Some(eq) = body.find('=') else { continue };
        let (lhs, rhs) = body.split_at(eq);
        for name in lhs.split(',') {
            let name = name.trim();
            if name.is_empty()
                || !name.chars().all(|c| c.is_alphanumeric() || c == '_')
                || name.chars().next().is_some_and(|c| c.is_ascii_digit())
            {
                continue;
            }
            // **A self-localisation is not a definition.** `local GetTime = GetTime` — and its
            // `local a, b = a, b` and `local X = X or {}` cousins — binds the local from the
            // GLOBAL of the same name, so the global really is demanded and hiding it is exactly
            // the under-report this pass was supposed to avoid. **143 corpus sites, 41 distinct
            // names**, and the shape is the performance idiom every library writes at file top, so
            // it is concentrated on precisely the APIs an addon uses most.
            if rhs
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|tok| tok == name)
            {
                continue;
            }
            defined.insert(name.to_string());
        }
    }
}

/// The aggregate 1188 asks for: how many addons want each missing name, most-wanted first.
///
/// **This is the number phase 5 is prioritised by** — one addon wanting a verb is a curiosity,
/// forty wanting it is the next thing to build.
pub fn demand(reports: &[AddonReport]) -> Vec<(String, usize)> {
    rank(reports, |r| &r.missing_globals)
}

/// **How many ADDONS want each name**, most-wanted first, ties broken alphabetically — the one
/// shape every ranking here has, factored out when the fifth arrived.
///
/// The unit is deliberately the addon, never the call site: a library file replicated across sixty
/// folders would otherwise decide the whole ranking by itself (1207), and the queue this feeds is
/// "how many players notice if we build it", which counts addons.
fn rank(
    reports: &[AddonReport],
    pick: impl Fn(&AddonReport) -> &Vec<String>,
) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in reports {
        for n in pick(r) {
            *counts.entry(n.as_str()).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// [`demand`]'s twin for **templates** (decision 1203) — how many addons name each template we
/// have never declared, most-wanted first.
///
/// The list `CreateFrame`'s fourth argument working made measurable: honouring it moved the
/// headline by exactly zero, because an unresolved template was never a load error in the first
/// place. This is what actually stands between an addon and a painted window.
pub fn template_demand(reports: &[AddonReport]) -> Vec<(String, usize)> {
    rank(reports, |r| &r.missing_templates)
}

/// [`demand`] over [`AddonReport::missing_tables`] — the frames and tables an addon indexes and we
/// do not have. Its own list because a missing frame is FrameXML to transcribe, not a Rust verb.
pub fn table_demand(reports: &[AddonReport]) -> Vec<(String, usize)> {
    rank(reports, |r| &r.missing_tables)
}

/// [`demand`] over [`AddonReport::missing_methods`] — **the widget-method queue**, and the axis
/// this survey was blind to while the arc spent a day building exactly these.
///
/// Its own list for the same reason as the other three (1207): the three queues go to different
/// places. A missing global is a Rust verb to write; a missing frame is FrameXML to transcribe; a
/// missing method is a widget **binding** — and sometimes not a verb at all but a whole *kind* left
/// unwired, which is what the head of this list said the first time it was run.
///
/// Read [`AddonReport::missing_methods`] before quoting a row: this one over-reports by
/// construction and the doc there says exactly how far.
pub fn method_demand(reports: &[AddonReport]) -> Vec<(String, usize)> {
    rank(reports, |r| &r.missing_methods)
}

/// [`method_demand`] over [`AddonReport::optional_methods`] — the methods addons **work around**.
///
/// Ranked apart from the blocking list on purpose, and never merged into it: nobody is stuck on
/// these, so mixing them in would rank a name nobody needs above one somebody does. Read as
/// "implementing this improves N addons silently", not as "N addons are broken".
pub fn optional_method_demand(reports: &[AddonReport]) -> Vec<(String, usize)> {
    rank(reports, |r| &r.optional_methods)
}

/// [`demand`] over [`AddonReport::kind_missing_methods`] — **the per-kind widget-method queue**,
/// and the one row shape this whole survey was structurally unable to print until it existed.
///
/// Read it before [`method_demand`], not after: a row here names a kind an addon's call actually
/// lands on, so it is a blocker whether or not some *other* widget answers the same verb. The
/// `(on …)` tail says which job it is — `(on no kind)` is a verb to write, `(on
/// ScrollingMessageFrame)` is a verb we already wrote and forgot to wire to its sibling, which is
/// the `MessageFrame:AddMessage` shape decision 1228 is about.
pub fn kind_method_demand(reports: &[AddonReport]) -> Vec<(String, usize)> {
    rank(reports, |r| &r.kind_missing_methods)
}

/// [`demand`] over [`AddonReport::ambiguous_methods`] — the names whose answer **depends on the
/// kind** at a call site nothing could type.
///
/// An **upper bound**, and it says so in every row: the receiver may well be one of the kinds that
/// answers. It is printed anyway because the alternative — resolving it against the whole probe set
/// and calling it present — is exactly how `AddMessage` scored zero while three addons died on it.
/// Its length is a reading of the *attributor's* reach, not of ours.
///
/// **NARROWEST first, then by demand** — the only table here that is not ranked by count alone, and
/// the ordering is the row's whole usefulness rather than a preference.
///
/// Measured on the corpus, demand order puts `StopMovingOrSizing` (122 addons, absent only on the
/// two region leaves) at the top: an untyped receiver being a Texture *there* is not a thing anyone
/// writes, so the row is noise by construction — and with 139 rows and a printed head, noise at the
/// top is the same as hiding the rest. The fewer kinds answer a name, the likelier an untyped
/// receiver is one of the kinds that does not, and two-kind rows are literally the
/// `MessageFrame:AddMessage` shape (`AddMessage` is answered by exactly the two message frames).
/// So the key is the size of the answering set, ascending, then demand.
///
/// Nothing is filtered — the wide rows follow in their own demand order, and the printed header
/// says which half is which.
pub fn ambiguous_method_demand(reports: &[AddonReport]) -> Vec<(String, usize)> {
    let mut rows = rank(reports, |r| &r.ambiguous_methods);
    // The row TEXT is the sort key, so the ordering and the printed row can never disagree about
    // how narrow a name is: `only on` is [`only_on`]'s own word for "at most half the kinds", and
    // that branch never truncates its list, so counting separators recovers the set size exactly.
    rows.sort_by_key(|(row, count)| {
        let narrow = row.contains("(only on ");
        (
            !narrow,
            if narrow { row.matches(", ").count() } else { 0 },
            std::cmp::Reverse(*count),
        )
    });
    rows
}

/// [`template_demand`] over [`AddonReport::missing_inherits`] — the same ranking, the other axis.
pub fn inherits_demand(reports: &[AddonReport]) -> Vec<(String, usize)> {
    rank(reports, |r| &r.missing_inherits)
}

/// The **first** error of every addon that failed to load, normalised and ranked (decision 1193).
///
/// [`demand`]'s twin, and the more useful of the two for a while. `demand` answers *what would an
/// addon like to call*; this answers **what actually stopped it**, and those are different
/// questions with different top entries. The first error is the load-bearing one because a chunk
/// stops at its first raise: everything after it in that file never ran, so ranking all errors
/// would count the same root cause once per victim.
///
/// Normalisation is deliberately crude and deliberately stated: quoted names (`'setn'`,
/// `'GetMouseFocus'`) collapse to `'X'`, source positions are dropped, and the `<file>: ` prefix
/// goes. That turns 60 different-looking lines into one row reading `runtime error: 'X' is
/// obsolete`, which is what made the Lua 5.0/5.1 dialect gap visible at all — before this the
/// only view was per-addon, where it read as sixty unrelated failures.
pub fn blockers(reports: &[AddonReport]) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for r in reports.iter().filter(|r| !r.loaded) {
        if let Some(e) = r.errors.first() {
            *counts.entry(normalise_error(e)).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// The addons **behind** one [`blockers`] row, with their verbatim first errors.
///
/// The ranked table collapses every quoted name to `'X'` on purpose (1193) — that collapse is what
/// made the Lua-dialect gap readable as one wall instead of sixty. The cost is that reading back
/// *through* it has been a manual grep every time, and twice this arc that manual step was where
/// the finding actually was: decision **1206** came from noticing that eleven of fifteen
/// `bad argument #1 to 'X' (table expected, got nil)` rows were one missing table, and **1210** came
/// from asking which addons were behind a 74-count row (none: it was a Lua local).
///
/// `pattern` is a plain substring match against **the normalised row *or* the raw error text**, and
/// it is checked against **every** error, not only the ranked one.
///
/// **Both halves are corrections, and the first one is why this read-back was blind to an entire
/// class.** It used to match the normalised row alone — and step 2 of [`normalise_error`] replaces
/// every quoted name with `'X'`. So the one substring a reader would ever type was the one
/// substring the matcher deleted before comparing. `--why GetBackdrop` answered `(none)` while
/// `BuffCheck2/BuffCheck2.lua:448` was dying on `attempt to call method 'GetBackdrop' (a nil
/// value)`; `--why AddMessage` answered `(none)` with three addons behind it. The instrument even
/// printed *"matched against the NORMALISED row"* underneath, which reads as an explanation and is
/// exactly why nobody chased it (1212: a comment that says why a known problem is already handled
/// is the dangerous kind).
///
/// This is not a widget-method bug. **No name of any kind was findable** — not `setn`, not
/// `SendAddonMessage`, not a file path. The method class is simply the one that is *only* ever
/// identified by its quoted name, so it is where the hole became impossible to miss.
///
/// The second half: a chunk stops at its first raise, but a session start does not — an addon's
/// handlers keep firing after one of them dies, so the error naming the method can be the fourth in
/// the list. Searching only the first is the same blindness one level down.
///
/// **The ranked correspondence is preserved, which is what makes the read-back trustworthy.** A hit
/// on the error the tables actually counted (a failed addon's first LOAD error; a loaded addon's
/// first SESSION error) is labelled `[load]`/`[session]` with no index — so counting the
/// index-less rows reproduces the ranked row's count exactly. Anything else is `[load #3]`,
/// `[session #2]`: real, findable, and visibly not part of that count.
pub fn blocked_by(reports: &[AddonReport], pattern: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for r in reports {
        // A failed addon is ranked through its LOAD error; one that loaded clean is ranked through
        // what its handlers raised at session start. Both lists are searched for every addon,
        // because a reader asking "which addons?" means whichever table they were looking at — and
        // because an addon that failed to load still runs a session start.
        //
        // The USE column is searched too, and its first error IS ranked (its table counts every
        // addon's first, with no `loaded` filter) — without this, `--why` could not read back the
        // one column whose rows a human most wants the text of. The UI probe ranks nothing, so its
        // rows are always indexed.
        let ranked_kind = if r.loaded { "session" } else { "load" };
        for (list, kind) in [
            (&r.errors, "load"),
            (&r.session_errors, "session"),
            (&r.probe_errors, "probe"),
            (&r.used.errors, "used"),
        ] {
            for (i, e) in list.iter().enumerate() {
                if !(e.contains(pattern) || normalise_error(e).contains(pattern)) {
                    continue;
                }
                let ranked = i == 0 && (kind == ranked_kind || kind == "used");
                let label = if ranked {
                    kind.to_string()
                } else {
                    format!("{kind} #{}", i + 1)
                };
                out.push((format!("{} [{label}]", r.name), e.clone()));
            }
        }
    }
    out
}

/// The addons behind one **method-table** row — [`blocked_by`]'s twin for the rankings that are
/// built by scanning rather than by catching an error.
///
/// `blocked_by` reads back through error text; nothing read back through a demand row, and the cost
/// of that was paid immediately: verifying the per-kind table's own top row (`EditBox:SetFontObject`,
/// 63 addons) meant patching a debug print into the example and re-running the corpus, because
/// there was no way to ask *which* 63. "Read the line before ranking" (1207, 1210, 1214, 1227 — four
/// records in one arc, three of which found fiction at the top of a ranking) is the standing rule
/// here, and a rule whose only tool is a temporary code edit gets skipped.
///
/// A plain substring match against the row as printed, over all four method tables, each hit
/// labelled with the table it came from — so `--why "EditBox:SetFontObject"` names the addons and
/// `--why SetFontObject` finds every table that mentions it.
/// Which addons carry `pattern` in one of the three demand lists — the read-back those rankings
/// never had.
///
/// Every ranked table above is a **count**, and until this existed nothing could ask which addons a
/// count was made of. That is not a theoretical gap: `GetChannelList` ranked 4 while exactly one
/// corpus addon's source names it, and establishing that took a hand-rolled grep across a corpus
/// whose entries are symlinks (so `grep -r` silently reads none of them). A number you cannot open
/// is a claim rather than a measurement — the same lesson `--why`'s error read-back learned in
/// 1210/1218/1227, applied to the other three tables.
///
/// Matched case-insensitively on a substring, exactly like the error read-back, so a half-remembered
/// name still finds its row.
pub fn wanters(
    reports: &[AddonReport],
    pattern: &str,
    pick: impl Fn(&AddonReport) -> &Vec<String>,
) -> Vec<(String, String)> {
    let needle = pattern.to_ascii_lowercase();
    let mut out = Vec::new();
    for r in reports {
        for n in pick(r) {
            if n.to_ascii_lowercase().contains(&needle) {
                out.push((r.name.clone(), n.clone()));
            }
        }
    }
    out
}

pub fn method_rows_matching(reports: &[AddonReport], pattern: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for r in reports {
        for (list, table) in [
            (&r.kind_missing_methods, "by-kind"),
            (&r.missing_methods, "missing"),
            (&r.ambiguous_methods, "ambiguous"),
            (&r.optional_methods, "feature-tested"),
        ] {
            for row in list.iter().filter(|row| row.contains(pattern)) {
                out.push((format!("{} [{table}]", r.name), row.clone()));
            }
        }
    }
    out
}

/// [`normalise_error`], public so the report can rank SESSION-start errors with the same collapse
/// the load-time table uses — one wall must read as one row on both sides of the load boundary.
pub fn normalise(raw: &str) -> String {
    normalise_error(raw)
}

/// One load error with everything addon-specific removed, so two addons hitting the same wall
/// produce the same string. See [`blockers`] for why the crudeness is the point.
fn normalise_error(raw: &str) -> String {
    // 1 · Keep from the LAST `error: ` on, which drops every `<file>: <Script file="…">: ` prefix
    //     without having to know their shapes — and cut mlua's `stack traceback:` tail, which is
    //     per-addon detail that would split one wall into a dozen rows.
    let core = raw.rfind("error: ").map_or(raw, |i| &raw[i..]);
    let core = core
        .split_once("stack traceback:")
        .map_or(core, |(head, _)| head)
        .trim();

    // 2 · Every quoted name becomes `'X'` — the name is what varies between two addons that hit
    //     the same wall, and it is already ranked by `demand`. Both quote kinds, because mlua
    //     writes a chunk name as `[string "MyFrame:OnLoad"]`.
    let squashed = core.replace('"', "'");
    let mut collapsed = String::with_capacity(squashed.len());
    let mut rest = squashed.as_str();
    while let Some(open) = rest.find('\'') {
        collapsed.push_str(&rest[..open]);
        collapsed.push_str("'X'");
        match rest[open + 1..].find('\'') {
            Some(close) => rest = &rest[open + 1 + close + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    collapsed.push_str(rest);

    // 3 · Source positions carry no information once the name is gone.
    collapsed
        .split_whitespace()
        .filter(|t| !is_position(t))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Is this whitespace-separated token a source position rather than words?
///
/// `crates/benilla-ui/src/script/mod.rs:406:305:`, `[string "X"]:2:`, `MyAddon.lua:12:` — all of
/// them a `<where>:<line>[:<col>]:` tail, which is the only shape mlua emits.
fn is_position(tok: &str) -> bool {
    if tok == "[string" {
        return true; // the opening half of mlua's `[string "…"]:N:` chunk name
    }
    let t = tok.strip_suffix(':').unwrap_or(tok);
    let mut tail = t.rsplit(':');
    let last = tail.next().unwrap_or("");
    if last.is_empty() || !last.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // `<where>:<line>` is enough; a third field just means a column was included.
    tail.next().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two addons hitting one wall must produce **one** row — that collapse is the whole value of
    /// [`blockers`], and it is what made the Lua-dialect gap readable as `61` rather than as sixty
    /// unrelated-looking lines (decision 1193).
    #[test]
    fn one_wall_is_one_row_however_it_was_reported() {
        let same = [
            "libs\\AceLibrary\\AceLibrary.lua: runtime error: crates/benilla-ui/src/script/mod.rs:406:305: 'setn' is obsolete",
            "Libs/AceLibrary.lua: runtime error: crates/benilla-ui/src/script/mod.rs:406:301: 'setn' is obsolete",
            "embeds.xml: <Script file=\"AceLibrary.lua\">: runtime error: crates/benilla-ui/src/loader/mod.rs:218:13: 'setn' is obsolete",
        ];
        let normalised: BTreeSet<String> = same.iter().map(|e| normalise_error(e)).collect();
        assert_eq!(
            normalised.into_iter().collect::<Vec<_>>(),
            vec!["error: 'X' is obsolete"],
            "the file, the source position and the quoted name are all per-addon noise"
        );
    }

    /// mlua's `[string "Frame:OnLoad"]:2:` chunk name is a position, not words.
    #[test]
    fn a_chunk_name_position_is_not_mistaken_for_the_message() {
        assert_eq!(
            normalise_error(
                "Outfitter: OnLoad: runtime error: [string \"OutfitterShowMinimapButton:OnLoad\"]:2: attempt to index a nil value"
            ),
            "error: attempt to index a nil value"
        );
    }

    /// **An XML license header is not a list of frames**, and for a while it was the top of one.
    ///
    /// `.xml` files are scanned as Lua deliberately — `<Script>` CDATA and `<OnLoad>` bodies are
    /// Lua and skipping the file loses them. But `strip_lua_noise` only knows `--` and `[[ ]]`, so
    /// a GPL header in an `<!-- … -->` came through whole, and `PURPOSE.  See the` reads exactly
    /// like a table index. Five of the frames/tables ranking's top rows were this boilerplate
    /// (decision 1218).
    ///
    /// The other half of the claim matters as much: the Lua INSIDE the file must survive, or the
    /// fix trades a wrong ranking for a blind one.
    #[test]
    fn an_xml_comment_is_not_scanned_but_the_script_body_is() {
        let xml = "<Ui>\n\
                   <!-- MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the\n\
                        GNU General Public License. $Id: Thing.xml 1 2006 $ -->\n\
                   <Script><![CDATA[\n\
                   GameTooltip:AddLine(\"x\")\n\
                   MyHelper()\n\
                   ]]></Script>\n\
                   </Ui>";
        let mut s = Scan::default();
        scan_lua(&strip_xml_comments(xml), &mut s);
        for noise in ["PURPOSE", "License", "Id"] {
            assert!(
                !s.indexed.contains(noise) && !s.called.contains(noise),
                "{noise} is license boilerplate, not a surface the addon expects"
            );
        }
        assert!(
            s.indexed.contains("GameTooltip"),
            "the script body must still be scanned: {:?}",
            s.indexed
        );
        assert!(
            s.called.contains("MyHelper"),
            "the script body must still be scanned: {:?}",
            s.called
        );
        // ...and the METHOD arm obeys the same stripper. `See the` in that header is a capitalised
        // word after a word — harmless — but `GameTooltip:AddLine(` inside the CDATA is the shape
        // the method scan lives on, so both halves are asserted on the same file (1218's rule: a
        // noise fix that goes blind is worse than the noise).
        assert!(
            s.methods.contains("AddLine"),
            "the script body's method call must be scanned: {:?}",
            s.methods
        );
    }

    /// An unterminated `<!--` takes the rest of the file, exactly as a real XML parser does — the
    /// document is malformed and guessing where the comment meant to stop would invent structure.
    #[test]
    fn an_unterminated_xml_comment_swallows_the_rest() {
        let out = strip_xml_comments("Kept.Alpha\n<!-- Dropped.Beta\nDropped.Gamma\n");
        assert!(out.contains("Kept"));
        assert!(
            !out.contains("Beta") && !out.contains("Gamma"),
            "got: {out:?}"
        );
        // Line structure survives, so anything counting lines still counts real ones.
        assert_eq!(out.matches('\n').count(), 3);
    }

    /// **A capitalised Lua local is not a missing API**, and for a long time this instrument said
    /// it was — loudly, at the top of its own most-wanted list.
    ///
    /// The shape is `FuBarPlugin-2.0.lua`'s, verbatim: a `local` bound to a function expression and
    /// called a few lines down. `local function Foo` was already credited; `local Foo = function`
    /// was not, and the leading-capital filter that was supposed to cover it cannot, because
    /// nothing stops a local from being capitalised. Five rows and 305 addon-mentions of the
    /// vanilla corpus's `demand` table were this.
    #[test]
    fn a_capitalised_local_is_not_a_missing_global() {
        let mut s = Scan::default();
        scan_lua(
            "local CheckShow = function(self, panelId) end\n\
             local DropDownList1_Show = DropDownList1.Show\n\
             local A, B = 1, 2\n\
             local function Direct() end\n\
             Global = function() end\n\
             CheckShow(self, 1)\n\
             DropDownList1_Show(DropDownList1)\n\
             A() B() Direct() Global()\n\
             UnitName(\"player\")\n",
            &mut s,
        );
        let missing: Vec<&str> = s
            .called
            .iter()
            .filter(|n| !s.defined.contains(*n))
            .map(String::as_str)
            .collect();
        assert_eq!(
            missing,
            vec!["UnitName"],
            "every capitalised name the file binds itself is the file's, however it binds it"
        );
    }

    /// ...**but a self-localisation is not a definition.** `local GetTime = GetTime` is the
    /// performance idiom every library writes at file top, and it binds the local from the GLOBAL,
    /// so the global is still demanded. 143 corpus sites over 41 names — concentrated, by its
    /// nature, on the APIs an addon leans on hardest.
    ///
    /// This is a correction to the fix one commit up: crediting every `local X =` line hid exactly
    /// the names most worth ranking.
    #[test]
    fn localising_a_global_still_demands_it() {
        let mut s = Scan::default();
        scan_lua(
            "local GetTime = GetTime\n\
             local UnitName, UnitClass = UnitName, UnitClass\n\
             local MyCache = MyCache or {}\n\
             local Helper = function() end\n\
             GetTime() UnitName('player') UnitClass('player') MyCache() Helper()\n",
            &mut s,
        );
        let mut missing: Vec<&str> = s
            .called
            .iter()
            .filter(|n| !s.defined.contains(*n))
            .map(String::as_str)
            .collect();
        missing.sort_unstable();
        assert_eq!(
            missing,
            vec!["GetTime", "MyCache", "UnitClass", "UnitName"],
            "self-localisation in every shape — single, comma list, and the `or` form — keeps \
             the demand; only the genuinely-new `Helper` is the file's own"
        );
    }

    /// A "not found" has no `error: ` marker and must survive whole — it is a *different* wall
    /// (a missing file) and collapsing it into the runtime errors would hide it.
    #[test]
    fn a_missing_file_stays_its_own_row() {
        assert_eq!(
            normalise_error("..\\..\\FrameXML\\Fonts.xml: not found"),
            "..\\..\\FrameXML\\Fonts.xml: not found"
        );
    }

    /// Only the FIRST error counts, and only from addons that failed — a chunk stops at its first
    /// raise, so everything after it is a consequence rather than a cause.
    #[test]
    fn only_the_first_error_of_a_failed_addon_is_counted() {
        let report = |name: &str, loaded: bool, errors: Vec<String>| AddonReport {
            name: name.into(),
            loaded,
            errors,
            ..Default::default()
        };
        let ranked = blockers(&[
            report(
                "A",
                false,
                vec![
                    "x.lua: runtime error: 'setn' is obsolete".into(),
                    "y.lua: runtime error: 'other' is obsolete".into(),
                ],
            ),
            report(
                "B",
                false,
                vec!["z.lua: runtime error: 'setn' is obsolete".into()],
            ),
            report("C", true, vec![]),
        ]);
        assert_eq!(ranked, vec![("error: 'X' is obsolete".to_string(), 2)]);
    }
}

#[cfg(test)]
mod dependency_tests {
    use super::*;

    /// **A dependency is loaded before its dependent** — `AddOn_Load 0x51f240`'s own first step,
    /// and the difference between surveying a real session and surveying a state that cannot occur.
    ///
    /// The corpus case this is drawn from: `FuBar_Aspect` declares `## Dependencies: Ace, FuBar`
    /// and its very first line is `ace:LoadTranslation("FuBar_Aspect")`. Surveyed alone it fails
    /// on a global its dependency was always going to define — fifteen addons failed that way, on
    /// us rather than on themselves.
    #[test]
    fn a_dependency_runs_before_the_addon_that_declares_it() {
        let tmp = std::env::temp_dir().join(format!("benilla-harness-deps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let write = |name: &str, toc: &str, file: &str, body: &str| {
            let dir = tmp.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{name}.toc")), toc).unwrap();
            std::fs::write(dir.join(file), body).unwrap();
        };
        // A library, a middle layer that depends on it, and a leaf that depends on the middle —
        // so the walk has to be depth-first, not one level.
        write(
            "Lib",
            "## Interface: 11200\nlib.lua\n",
            "lib.lua",
            "LibReady = 1\n",
        );
        write(
            "Mid",
            "## Interface: 11200\n## Dependencies: Lib\nmid.lua\n",
            "mid.lua",
            "MidReady = LibReady + 1\n",
        );
        write(
            "Leaf",
            "## Interface: 11200\n## Dependencies: Mid\nleaf.lua\n",
            "leaf.lua",
            "LeafReady = MidReady + 1\n",
        );

        let reports = survey(&tmp);
        let leaf = reports.iter().find(|r| r.name == "Leaf").unwrap();
        assert!(
            leaf.loaded,
            "the leaf loaded because its chain ran first: {:?}",
            leaf.errors
        );
        assert!(leaf.missing_deps.is_empty());

        // ...and an addon whose dependency is NOT installed still reports it, unchanged.
        write(
            "Orphan",
            "## Interface: 11200\n## Dependencies: Nowhere\norphan.lua\n",
            "orphan.lua",
            "OrphanReady = 1\n",
        );
        let reports = survey(&tmp);
        let orphan = reports.iter().find(|r| r.name == "Orphan").unwrap();
        assert_eq!(orphan.missing_deps, vec!["Nowhere".to_string()]);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// **A frame is a surface too**, and the scan could not see one until this arm existed.
    ///
    /// The corpus spells the colour picker `ColorPickerFrame.func` and
    /// `ColorPickerFrame:SetColorRGB` — never `ColorPickerFrame(` — so a window **86 addons reach**
    /// scored exactly 0 on the most-wanted list. Same blindness for `GameTooltip`, `WorldFrame`,
    /// `ChatFrame1` and every other FrameXML frame global.
    ///
    /// The two lists stay separate (1207): a missing function is a Rust verb, a missing frame is
    /// FrameXML to transcribe.
    #[test]
    fn an_indexed_frame_is_a_missing_surface_not_a_missing_function() {
        let mut s = Scan::default();
        scan_lua(
            "ColorPickerFrame.func = function() end\n\
             ColorPickerFrame:SetColorRGB(1, 0, 0)\n\
             GameTooltip:AddLine('hi')\n\
             MyAddon = {}\n\
             MyAddon.thing = 1\n\
             local Cache = {}\n\
             Cache.x = 1\n\
             UnitName('player')\n\
             self.wrong = 1\n",
            &mut s,
        );
        let live: Vec<&str> = s
            .indexed
            .iter()
            .filter(|n| !s.defined.contains(*n))
            .map(String::as_str)
            .collect();
        assert_eq!(
            live,
            vec!["ColorPickerFrame", "GameTooltip"],
            "the addon's own MyAddon and its local Cache are its own; `self` is lowercase and \
             qualified reads never count"
        );
        assert_eq!(
            s.called.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["UnitName"],
            "and the lists do not bleed into each other"
        );
        // `ColorPickerFrame.func = function` is a FIELD the file binds, so `func` is credited as a
        // definition — while `SetColorRGB` and `AddLine`, which it only calls, are demanded.
        assert_eq!(
            s.methods.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["AddLine", "SetColorRGB"],
            "a `:` call is a method demand and never a global one"
        );
        assert!(
            s.defined_methods.contains("func"),
            "a dotted assignment is the addon binding its own field: {:?}",
            s.defined_methods
        );
    }

    /// `blocked_by` matches the row **as printed** *and* the raw error text — and the second half
    /// is a correction to what this test used to assert.
    ///
    /// It pinned `blocked_by(&reports, "tinsert").is_empty()` as *correct*, on the reasoning that
    /// "the quoted name is already `'X'` in the row the reader is holding". That reasoning describes
    /// the mechanism accurately and draws the wrong conclusion from it: the normalisation exists so
    /// sixty lines rank as one **row**, not so a reader is forbidden from searching by **name**.
    /// Searching by name is in fact the only thing anyone ever does with this — and it silently
    /// returned nothing every time. `--why GetBackdrop` said `(none)` while an addon was dying on
    /// `attempt to call method 'GetBackdrop' (a nil value)`.
    ///
    /// A test can encode a bug as an invariant, and this one did, which is why the assertion below
    /// is inverted rather than deleted.
    #[test]
    fn blocked_by_reads_back_by_row_and_by_name() {
        let report = |name: &str, loaded: bool, first: &str| AddonReport {
            name: name.into(),
            loaded,
            errors: vec![first.into()],
            ..Default::default()
        };
        let reports = [
            report(
                "A",
                false,
                "a.lua: runtime error: bad argument #1 to 'tinsert' (table expected, got nil)",
            ),
            report(
                "B",
                false,
                "b.xml: runtime error: bad argument #1 to 'tremove' (table expected, got nil)",
            ),
            report(
                "C",
                false,
                "c.lua: runtime error: attempt to call a table value",
            ),
            report("D", true, ""),
        ];
        let wall = blocked_by(&reports, "table expected");
        let hits: Vec<&str> = wall.iter().map(|(n, _)| n.as_str()).collect();
        // The `[load]`/`[session]` tag is part of the row (c91cd11a: "the label says which") —
        // both of these failed to LOAD, so both read back through the load table.
        assert_eq!(
            hits,
            vec!["A [load]", "B [load]"],
            "two different verbs, one wall"
        );
        // The half that was inverted: the NAME the normalisation collapsed is findable again, and
        // finds exactly the addon that used it — not the whole wall.
        let by_name: Vec<String> = blocked_by(&reports, "tinsert")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            by_name,
            vec!["A [load]"],
            "a quoted name is what a reader types, and it must reach the addon behind it"
        );
        // ...and the verbatim error comes back, which is the reason to run it at all.
        assert!(wall[0].1.contains("tinsert"));
    }

    /// **A later error is findable, and says so** — the second half of the read-back's blindness.
    ///
    /// A chunk stops at its first raise, but a session start does not: handlers keep firing, so the
    /// error naming the method can be the third in the list. Searching only the first is the same
    /// hole one level down.
    ///
    /// The labelling is the load-bearing part. A hit on the error the tables actually **ranked**
    /// carries no index, so counting index-less rows reproduces the ranked row's count exactly; a
    /// later one is `#N` and visibly outside that count. Without that, making the search complete
    /// would have made the read-back disagree with the table above it, and a reader would have no
    /// way to tell which was wrong.
    #[test]
    fn a_later_error_is_found_and_labelled_as_not_the_ranked_one() {
        let reports = [AddonReport {
            name: "Late".into(),
            loaded: true,
            session_errors: vec![
                "runtime error: attempt to call global 'Foo' (a nil value)".into(),
                "runtime error: attempt to index a nil value".into(),
                "runtime error: attempt to call method 'GetBackdrop' (a nil value)".into(),
            ],
            ..Default::default()
        }];
        let ranked: Vec<String> = blocked_by(&reports, "attempt to call global")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            ranked,
            vec!["Late [session]"],
            "no index: this is the row the table counted"
        );

        let late: Vec<String> = blocked_by(&reports, "GetBackdrop")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            late,
            vec!["Late [session #3]"],
            "the method that killed a handler was the addon's THIRD error, and it must still be \
             reachable by the only name anyone would search for"
        );
    }

    /// **A clean load is not a working addon**, and this is the column that can tell the
    /// difference — the survey's answer to the blind spot four decision records end on.
    ///
    /// Three addons, one shape each: one that raises only from its `PLAYER_LOGIN` handler, one that
    /// raises only from `OnUpdate` (so it needs a tick, not just an event), and one that is clean
    /// throughout. All three must report `loaded == true`, because **`loaded` still means exactly
    /// "no LOAD errors"** — changing that would make every number in every past decision record
    /// incomparable, which is 1209's whole subject.
    #[test]
    fn a_clean_load_is_not_a_working_addon() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-harness-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let write = |name: &str, body: &str| {
            let dir = tmp.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("{name}.toc")),
                "## Interface: 11200\na.lua\n",
            )
            .unwrap();
            std::fs::write(dir.join("a.lua"), body).unwrap();
        };
        write(
            "LoginBreaker",
            "local f = CreateFrame('Frame')\n\
             f:RegisterEvent('PLAYER_LOGIN')\n\
             f:SetScript('OnEvent', function() error('boom at login') end)\n",
        );
        write(
            "TickBreaker",
            "local f = CreateFrame('Frame')\n\
             f:SetScript('OnUpdate', function() error('boom on update') end)\n",
        );
        write(
            "Fine",
            "local f = CreateFrame('Frame')\n\
             f:RegisterEvent('PLAYER_LOGIN')\n\
             f:SetScript('OnEvent', function() FineRan = 1 end)\n",
        );

        let reports = survey(&tmp);
        let get = |n: &str| reports.iter().find(|r| r.name == n).unwrap();

        for n in ["LoginBreaker", "TickBreaker", "Fine"] {
            assert!(
                get(n).loaded,
                "{n}: `loaded` is LOAD errors only and must not change meaning: {:?}",
                get(n).errors
            );
        }
        assert!(
            !get("LoginBreaker").session_errors.is_empty(),
            "a handler that raises on PLAYER_LOGIN is exactly what no other column can see"
        );
        assert!(
            !get("TickBreaker").session_errors.is_empty(),
            "and an OnUpdate needs the ticks, not just the events"
        );
        assert!(
            get("Fine").session_errors.is_empty(),
            "{:?}",
            get("Fine").session_errors
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// **A library another addon embeds is invisible here, and that is the isolation's price.**
    ///
    /// The real client loads everything into one Lua state; this harness stands up one VM per
    /// addon so a failure cannot be attributed to the wrong party (the module doc's first
    /// section). The cost is exactly this: an addon that ships no libraries and leans on a
    /// sibling's copy fails here and would work in a real session.
    ///
    /// Drawn from `FuBar_CustomMenuFu`, which ships one Lua file, declares
    /// `## OptionalDeps: Ace2, FuBar`, and calls `AceLibrary("Tablet-2.0")` — a library neither
    /// installed addon provides. Five corpus addons sit behind that row, and none of them is a gap
    /// of ours.
    /// **Every addon in the VM gets its own `ADDON_LOADED`, with its OWN name in `arg1`.**
    ///
    /// The survey fired exactly one, carrying the SURVEYED addon's folder. A dependency's
    /// initialiser is almost always gated on its own name — `Atlas.lua:326` is
    /// `if (event == "ADDON_LOADED" and arg1 == "Atlas") then Atlas_Init(); end`, and `Atlas_Init`
    /// is what assigns `AtlasOptions` (l.199) — so a dependency loaded its files and never
    /// initialised. `FuBar_AtlasFu` then died at `AtlasButton.lua:30` on a nil `AtlasOptions`, and
    /// the survey recorded that against FuBar_AtlasFu. **The state was one the real client never
    /// produces**, and it cost 11 addons of the session-start column.
    ///
    /// The second assertion is the half that keeps the rule this module already states: a
    /// dependency's OWN handler raising is the dependency's row, never its consumers'. Those
    /// events fire outside the error-capture window, so one library's fault cannot be counted once
    /// per addon that embeds it.
    /// **A consumer's OWN code raising inside a DEPENDENCY's window is the consumer's row.**
    ///
    /// 1226's finding, now enforced. `AceAddon-2.0.lua:104-105` drains its entire `nextAddon` queue
    /// on ANY `ADDON_LOADED` it sees and calls each consumer's `OnInitialize` there (`:230`). So
    /// the surveyed addon's own file runs inside a dependency's window — and while attribution was
    /// by window, those raises were charged to nobody and the whole FuBar/Ace family read as
    /// surviving when it was not.
    ///
    /// Attribution is by the raising CHUNK now, which 1217 made truthful. The fixture is AceAddon's
    /// shape reduced: the library drains a queue on the first ADDON_LOADED it sees, whoever it
    /// names, and the consumer's callback raises from the consumer's own file.
    ///
    /// The second half is the half that must not regress: the LIBRARY's own raise, in the same
    /// window, still belongs to the library.
    #[test]
    fn a_consumers_raise_inside_a_dependency_window_is_the_consumers_row() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-harness-attrib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let write = |name: &str, toc: &str, file: &str, body: &str| {
            let dir = tmp.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{name}.toc")), toc).unwrap();
            std::fs::write(dir.join(file), body).unwrap();
        };
        // AceAddon's shape: drain every queued consumer on the FIRST ADDON_LOADED, whoever it names.
        write(
            "QueueLib",
            "## Interface: 11200\nlib.lua\n",
            "lib.lua",
            "QueueLibQueue = {}\n\
             QueueLibFrame = CreateFrame(\"Frame\")\n\
             QueueLibFrame:RegisterEvent(\"ADDON_LOADED\")\n\
             QueueLibFrame:SetScript(\"OnEvent\", function()\n\
             while table.getn(QueueLibQueue) > 0 do\n\
             local f = table.remove(QueueLibQueue, 1) f()\n\
             end\n\
             end)\n",
        );
        // The consumer queues a callback that raises from ITS OWN file.
        write(
            "QueueUser",
            "## Interface: 11200\n## Dependencies: QueueLib\nuse.lua\n",
            "use.lua",
            "table.insert(QueueLibQueue, function() error(\"consumer init blew up\") end)\n",
        );

        let reports = survey(&tmp);
        let of = |n: &str| reports.iter().find(|r| r.name == n).unwrap();

        assert!(
            of("QueueUser")
                .session_errors
                .iter()
                .any(|e| e.contains("consumer init blew up")),
            "the consumer's own file raised — window or not, it is the consumer's row: {:?}",
            of("QueueUser").session_errors
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn each_loaded_addon_gets_its_own_addon_loaded_event() {
        let tmp = std::env::temp_dir().join(format!(
            "benilla-harness-addonloaded-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let write = |name: &str, toc: &str, file: &str, body: &str| {
            let dir = tmp.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{name}.toc")), toc).unwrap();
            std::fs::write(dir.join(file), body).unwrap();
        };
        // Atlas's shape, reduced: a library that initialises ONLY on its own ADDON_LOADED.
        write(
            "TheLib",
            "## Interface: 11200\nlib.lua\n",
            "lib.lua",
            "TheLibFrame = CreateFrame(\"Frame\")\n\
             TheLibFrame:RegisterEvent(\"ADDON_LOADED\")\n\
             TheLibFrame:SetScript(\"OnEvent\", function()\n\
             if event == \"ADDON_LOADED\" and arg1 == \"TheLib\" then TheLibOptions = {} end\n\
             end)\n",
        );
        // The consumer reads the library's initialised state on a LATER event, as Atlas's button does.
        write(
            "TheUser",
            "## Interface: 11200\n## Dependencies: TheLib\nuse.lua\n",
            "use.lua",
            "TheUserFrame = CreateFrame(\"Frame\")\n\
             TheUserFrame:RegisterEvent(\"PLAYER_LOGIN\")\n\
             TheUserFrame:SetScript(\"OnEvent\", function() local _ = TheLibOptions.anything end)\n",
        );
        // A library whose own handler blows up — its consumers must not wear it.
        write(
            "BadLib",
            "## Interface: 11200\nbad.lua\n",
            "bad.lua",
            "BadLibFrame = CreateFrame(\"Frame\")\n\
             BadLibFrame:RegisterEvent(\"ADDON_LOADED\")\n\
             BadLibFrame:SetScript(\"OnEvent\", function()\n\
             if event == \"ADDON_LOADED\" and arg1 == \"BadLib\" then error(\"lib init blew up\") end\n\
             end)\n",
        );
        write(
            "BadUser",
            "## Interface: 11200\n## Dependencies: BadLib\nquiet.lua\n",
            "quiet.lua",
            "QuietGlobal = 1\n",
        );

        let reports = survey(&tmp);
        let of = |n: &str| reports.iter().find(|r| r.name == n).unwrap();

        assert!(
            of("TheUser").session_errors.is_empty(),
            "the dependency must have received its OWN ADDON_LOADED and initialised: {:?}",
            of("TheUser").session_errors
        );
        assert!(
            of("BadUser").session_errors.is_empty(),
            "a dependency's own handler raising is ITS row, not its consumer's: {:?}",
            of("BadUser").session_errors
        );
        assert!(
            of("BadLib")
                .session_errors
                .iter()
                .any(|e| e.contains("lib init blew up")),
            "...and it must still be reported against the library itself: {:?}",
            of("BadLib").session_errors
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_sibling_addons_embedded_library_is_invisible() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-harness-sibling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let write = |name: &str, toc: &str, file: &str, body: &str| {
            let dir = tmp.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{name}.toc")), toc).unwrap();
            std::fs::write(dir.join(file), body).unwrap();
        };
        // One addon embeds a library. Another uses it, and declares no relationship at all —
        // which on the real client is fine, because they share a Lua state.
        write(
            "Embedder",
            "## Interface: 11200\nembedded.lua\n",
            "embedded.lua",
            "SharedLibGlobal = 1\n",
        );
        write(
            "Freeloader",
            "## Interface: 11200\nuse.lua\n",
            "use.lua",
            "if not SharedLibGlobal then error('needs the sibling library') end\n",
        );

        let reports = survey(&tmp);
        assert!(
            reports
                .iter()
                .find(|r| r.name == "Embedder")
                .unwrap()
                .loaded,
            "the addon that ships it is fine"
        );
        let free = reports.iter().find(|r| r.name == "Freeloader").unwrap();
        assert!(
            !free.loaded,
            "and the one that borrows it fails HERE while working on the real client — the \
             isolation's price, not a gap in the API surface"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// **An OPTIONAL dependency loads too, and first** — `AddOn_Load`'s own order (1191 §2).
    ///
    /// Drawn from the shape that exposed it: `FuBar_BagFu` declares `## OptionalDeps: FuBar, Ace2`
    /// and then lists `FuBarPlugin-2.0.lua` in its manifest BEFORE `AceLibrary.lua`. That only
    /// works because the `Ace2` addon went first and left `AceLibrary` global; surveyed without it,
    /// FuBarPlugin raises "requires AceLibrary" and the addon dies on a state the real client never
    /// produces. Ten corpus addons sat behind that row.
    ///
    /// Also asserts the half that must NOT change: an optional dependency that is not installed is
    /// skipped in silence and never appears in `missing_deps`, which is the required-only list.
    #[test]
    fn an_optional_dependency_loads_first_and_a_missing_one_is_silent() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-harness-optdeps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let write = |name: &str, toc: &str, file: &str, body: &str| {
            let dir = tmp.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{name}.toc")), toc).unwrap();
            std::fs::write(dir.join(file), body).unwrap();
        };
        // The library addon, and a dependent whose OWN file order needs it to have gone first.
        write(
            "TheLib",
            "## Interface: 11200\nlib.lua\n",
            "lib.lua",
            "TheLibGlobal = 1\n",
        );
        write(
            "Dependent",
            "## Interface: 11200\n## OptionalDeps: TheLib, NotInstalled\nuse.lua\n",
            "use.lua",
            "if not TheLibGlobal then error('Dependent requires TheLib') end\nDependentReady = 1\n",
        );

        let reports = survey(&tmp);
        let r = reports.iter().find(|r| r.name == "Dependent").unwrap();
        assert!(
            r.loaded,
            "the optional dependency ran first: {:?}",
            r.errors
        );
        assert!(
            r.missing_deps.is_empty(),
            "an uninstalled OPTIONAL dep is silent — missing_deps is the required-only list: {:?}",
            r.missing_deps
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// **The `inherits=` census sees the axis `template_demand` cannot**, and its two exclusions
    /// hold: a FONT name is not a missing template, and neither is a virtual the addon declares
    /// itself.
    ///
    /// Written from the shape that produced the finding — twelve corpus addons whose whole failure
    /// was an `inherits=` in their own XML, none of which appeared in the `CreateFrame` ranking.
    #[test]
    fn the_inherits_census_counts_templates_and_not_fonts() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-harness-inherits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dir = tmp.join("Inheritor");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Inheritor.toc"), "## Interface: 11200\nui.xml\n").unwrap();
        std::fs::write(
            dir.join("ui.xml"),
            r#"<Ui>
                <Button name="InheritorOwnTemplate" virtual="true"/>
                <Frame name="InheritorRoot">
                    <Layers><Layer level="ARTWORK">
                        <FontString name="$parentLabel" inherits="GameFontNormal" text="hi"/>
                    </Layer></Layers>
                    <Frames>
                        <Button name="$parentMine" inherits="InheritorOwnTemplate"/>
                        <Button name="$parentReal" inherits="UIPanelButtonTemplate"/>
                        <Button name="$parentGone" inherits="NoSuchTemplate"/>
                    </Frames>
                </Frame>
            </Ui>
"#,
        )
        .unwrap();

        let reports = survey(&tmp);
        let r = reports.iter().find(|r| r.name == "Inheritor").unwrap();
        assert_eq!(
            r.missing_inherits,
            vec!["NoSuchTemplate".to_string()],
            "GameFontNormal is a font, InheritorOwnTemplate is the addon's own, and \
             UIPanelButtonTemplate is one we now ship"
        );
        assert!(
            r.missing_templates.is_empty(),
            "and none of it is visible to the CreateFrame scanner — the point of the twin"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
    /// **The widget-method census finds a method that does not exist** — the can-it-fail proof, and
    /// the reason this test is written against `survey` rather than against the scanner.
    ///
    /// A census that returns an empty list is indistinguishable from one that never ran, and this
    /// instrument has already shipped once in the second state (`drive_ui_probe`'s own comment).
    /// So the fixture calls an **invented** method that no widget can ever provide, and it must
    /// come back — while four shapes that are *not* gaps must not:
    ///
    /// | called | why it must not appear |
    /// |---|---|
    /// | `f:SetWidth` | a frame method we ship — if this appeared, the oracle answered "everything" |
    /// | `f:CreateTexture` | ditto, and it is what produces the region probe |
    /// | `t:SetTexCoord` | a **region** method: reachable only from the Texture probe, never a frame |
    /// | `Probe:OwnMethod` | the addon declares it — `function T:N` |
    /// | `f:Hooked` | the addon declares it the other way — `T.N = function` |
    /// | `MethodicalLib:LibOnly` | a **dependency** declares it, and the VM loaded that dependency |
    /// | `f:OptionalHook` | the addon **feature-tests** it one line up, so it is not a blocker |
    ///
    /// Both failure directions are therefore pinned by one assertion pair: a silent-empty oracle
    /// loses `NoSuchWidgetMethod`, a blanket-report oracle gains `SetWidth`.
    ///
    /// The last two rows are the ones that were learned from the corpus rather than reasoned out,
    /// and each was worth most of a screen of fiction:
    ///
    /// - without the dependency subtraction the top seven rows were `FuBar:RegisterPlugin` and its
    ///   siblings at 75-78 addons each — every one `function FuBar:<Name>` in a dependency 83
    ///   corpus addons declare and the VM duly loads;
    /// - without the feature-test subtraction the next ten were FuBarPlugin's optional callbacks
    ///   (`OnMouseUp`, `OnReceiveDrag`) and AceOO's duck-typed operators (`CompareTo`, `Equals`,
    ///   `Divide`), none of which is *defined* anywhere in the corpus at all.
    #[test]
    fn the_method_census_finds_a_method_no_widget_provides() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-harness-methods-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let lib = tmp.join("MethodicalLib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(
            lib.join("MethodicalLib.toc"),
            "## Interface: 11200\nlib.lua\n",
        )
        .unwrap();
        std::fs::write(
            lib.join("lib.lua"),
            "MethodicalLib = {}\nfunction MethodicalLib:LibOnly() end\n",
        )
        .unwrap();
        let dir = tmp.join("Methodical");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Methodical.toc"),
            "## Interface: 11200\n## Dependencies: MethodicalLib\na.lua\n",
        )
        .unwrap();
        // Wrapped in a function that is never called: the census is STATIC, so it sees these
        // whether or not the path runs — which is the whole reason it is a scan and not a trace
        // (the module doc's first section). Keeping it unrun also keeps `loaded` clean, so a
        // failure here can only be the census's.
        std::fs::write(
            dir.join("a.lua"),
            "local f = CreateFrame(\"Frame\")\n\
             Probe = {}\n\
             function Probe:OwnMethod() end\n\
             f.Hooked = function() end\n\
             function Methodical_Never()\n\
             f:SetWidth(10)\n\
             local t = f:CreateTexture()\n\
             t:SetTexCoord(0, 1, 0, 1)\n\
             f:NoSuchWidgetMethod(1)\n\
             Probe:OwnMethod()\n\
             f:Hooked()\n\
             MethodicalLib:LibOnly()\n\
             if type(f.OptionalHook) == \"function\" then f:OptionalHook() end\n\
             end\n",
        )
        .unwrap();

        let reports = survey(&tmp);
        let r = reports.iter().find(|r| r.name == "Methodical").unwrap();
        assert!(r.loaded, "the fixture must load clean: {:?}", r.errors);
        assert_eq!(
            r.missing_methods,
            vec!["NoSuchWidgetMethod".to_string()],
            "the invented method must be found and nothing else may be"
        );
        // The feature-tested one is not a blocker — and it is not lost either. Dropping the class
        // outright would have taken `SetTopLevel` (60 corpus addons, a real widget method) with it.
        assert_eq!(
            r.optional_methods,
            vec!["OptionalHook".to_string()],
            "a guarded call is its own row, never a silent omission"
        );
        // ...and it reaches the ranking a session actually reads. The library's own row is empty:
        // it calls nothing.
        assert_eq!(
            method_demand(&reports),
            vec![("NoSuchWidgetMethod".to_string(), 1)]
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// **The per-kind census, end to end over a real addon folder** — the can-it-fail proof for the
    /// whole path, not just the oracle.
    ///
    /// The fixture is 1228's own case, both ways round, so a pass here cannot be a pass by
    /// accident:
    ///
    /// | the call | what must happen |
    /// |---|---|
    /// | `smf:SetInsertMode()` on a `CreateFrame("ScrollingMessageFrame")` local | a row — the verb is real, the sibling has it, this kind does not |
    /// | `mf:SetInsertMode()` on a `CreateFrame("MessageFrame")` local | **no** row — the control that stops "report everything" passing |
    /// | `UIParent:AddMessage()` | a row via the PUBLISHED-name path: `UIParent` is a plain Frame in our arena |
    /// | `UIErrorsFrame:AddMessage()` | **no** row — ours really is a `<MessageFrame>` (1228) |
    /// | `self:SetInsertMode()` | not a row, an AMBIGUOUS one: nothing can type `self` |
    ///
    /// And the two blindness guards this arc keeps paying for, both written so they can only pass
    /// for the right reason: `ghost` is typed **only** inside a `--` comment and `faker` **only**
    /// inside a string literal, both as `Button` — a kind nothing else in the fixture uses — so a
    /// stripper that leaked would mint a `Button:AddMessage` row that is impossible otherwise
    /// (1218: five words of GPL boilerplate once topped a ranking, and this pass is the first here
    /// that reads un-blanked string literals at all).
    ///
    /// `missing_methods` must stay **empty** throughout: every name here is one some widget answers,
    /// which is exactly why the old table could not see any of it.
    #[test]
    fn the_per_kind_census_finds_a_verb_wired_to_the_wrong_kind() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-harness-perkind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dir = tmp.join("PerKind");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("PerKind.toc"), "## Interface: 11200\na.lua\n").unwrap();
        // Never executed, like the census fixture above: the scan is static, and an unrun body
        // keeps `loaded` clean so a failure here can only be the census's.
        std::fs::write(
            dir.join("a.lua"),
            "local smf = CreateFrame(\"ScrollingMessageFrame\")\n\
             local mf = CreateFrame(\"MessageFrame\")\n\
             -- local ghost = CreateFrame(\"Button\")\n\
             local faker = \"CreateFrame('Button')\"\n\
             function PerKind_Never(self)\n\
             smf:SetInsertMode(\"TOP\")\n\
             mf:SetInsertMode(\"TOP\")\n\
             UIParent:AddMessage(\"x\")\n\
             UIErrorsFrame:AddMessage(\"x\")\n\
             self:SetInsertMode(\"TOP\")\n\
             ghost:AddMessage(\"x\")\n\
             faker:AddMessage(\"x\")\n\
             end\n",
        )
        .unwrap();

        let reports = survey(&tmp);
        let r = reports.iter().find(|r| r.name == "PerKind").unwrap();
        assert!(r.loaded, "the fixture must load clean: {:?}", r.errors);
        assert!(
            r.missing_methods.is_empty(),
            "every name here IS answered by some widget — which is the whole reason the ANY-kind \
             table cannot see this class: {:?}",
            r.missing_methods
        );
        assert_eq!(
            r.kind_missing_methods,
            vec![
                "Frame:AddMessage (on MessageFrame, ScrollingMessageFrame)".to_string(),
                "ScrollingMessageFrame:SetInsertMode (on MessageFrame)".to_string(),
            ],
            "the typed call sites, and only the ones whose kind cannot answer — no Button row, so \
             neither the comment nor the string literal was read as code"
        );
        // `self`, `ghost` and `faker` are all untypable, so the NAME is reported as conditional
        // rather than swallowed as present.
        assert_eq!(
            r.ambiguous_methods,
            vec![
                "AddMessage (only on MessageFrame, ScrollingMessageFrame)".to_string(),
                "SetInsertMode (only on MessageFrame)".to_string(),
            ],
            "an untypable receiver is a stated unknown, never a silent pass"
        );
        // ...and it reaches the rankings a session reads.
        assert_eq!(
            kind_method_demand(&reports),
            vec![
                (
                    "Frame:AddMessage (on MessageFrame, ScrollingMessageFrame)".to_string(),
                    1
                ),
                (
                    "ScrollingMessageFrame:SetInsertMode (on MessageFrame)".to_string(),
                    1
                ),
            ]
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// **A name the file rebinds cannot be typed at all** — the rule that stops the attributor
    /// inventing rows, which is the failure mode every demand ranking in this arc has already had.
    ///
    /// One `local frame = CreateFrame("MessageFrame")` at file top would otherwise type every
    /// `frame:Method()` below it, including the `frame` that is a **parameter** of a helper, or a
    /// `for` variable, holding whatever the caller passed. Three shapes, one assertion: the typed
    /// receiver keeps its kind, and each rebound one loses it.
    #[test]
    fn a_rebound_name_is_not_typed() {
        let kinds = local_widget_kinds(
            "local kept = CreateFrame(\"MessageFrame\")\n\
             local shadowed = CreateFrame(\"MessageFrame\")\n\
             local shadowed = SomethingElse()\n\
             local looped = CreateFrame(\"MessageFrame\")\n\
             for _, looped in ipairs(t) do end\n\
             local passed = CreateFrame(\"MessageFrame\")\n\
             local function helper(passed) end\n",
        );
        assert_eq!(kinds.get("kept"), Some(&Some("MessageFrame")));
        for rebound in ["shadowed", "looped", "passed"] {
            assert_eq!(
                kinds.get(rebound),
                Some(&None),
                "{rebound} is bound twice, so nothing here can say what it holds"
            );
        }
    }

    /// A VM shaped like the survey's, for the oracle tests below.
    fn seated_vm() -> UiScript {
        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        seat_a_session(&mut script);
        let _ = crate::ui_script::load_default_ui(&script);
        script
    }

    /// **The session fixture is LIVE**, asserted through the same Lua surface an addon sees.
    ///
    /// A fixture that silently fails to seat reads EXACTLY like a fixture that exposed nothing —
    /// both are "net +0" — and this arc has already been fooled once by a measurement that
    /// returned an empty answer for a tooling reason (1242's note on `grep`). The backpack seat
    /// produced no corpus delta at all; this is what makes that a finding rather than a
    /// possibility.
    ///
    /// It is also a guard: delete a seat and this fails, instead of the columns quietly shifting
    /// and the next A/B attributing the move to whatever landed beside it.
    #[test]
    fn the_seated_session_is_visible_from_lua() {
        let s = seated_vm();

        // The backpack — 16 slots from level 1, two of them filled. Exposed nothing, verifiably.
        assert_eq!(s.eval::<i64>("return GetContainerNumSlots(0)").unwrap(), 16);
        assert_eq!(
            s.eval::<String>("return GetBagName(0)").unwrap(),
            "Backpack"
        );
        assert_eq!(
            s.eval::<i64>("local _, c = GetContainerItemInfo(0, 5) return c")
                .unwrap(),
            12,
            "the Linen Cloth stack"
        );
        // Bags 1..4 are deliberately NOT seated: a fresh character has no equipped bags, and
        // seating them would manufacture a state rather than expose one.
        assert_eq!(s.eval::<i64>("return GetContainerNumSlots(1)").unwrap(), 0);

        // The quest log — asserted through the API an addon actually walks, not the state struct.
        // `GetNumQuestLogEntries` returns (rows, quests): THREE rows but TWO quests, because the
        // header is a row and not a quest. An addon that conflates the two indexes off the end.
        assert_eq!(
            s.eval::<(i64, i64)>("return GetNumQuestLogEntries()")
                .unwrap(),
            (3, 2),
            "three rows, two quests — the header is a row"
        );
        assert_eq!(
            s.eval::<String>("local t = GetQuestLogTitle(1) return t")
                .unwrap(),
            "Elwynn Forest"
        );
        assert!(
            s.eval::<bool>("local _,_,_,h = GetQuestLogTitle(1) return h and true or false")
                .unwrap(),
            "row 1 is the header"
        );
        // Both ends of isComplete: nil for in-progress, 1 for complete.
        assert!(s
            .eval::<Option<i64>>("local _,_,_,_,_,c = GetQuestLogTitle(2) return c")
            .unwrap()
            .is_none());
        assert_eq!(
            s.eval::<i64>("local _,_,_,_,_,c = GetQuestLogTitle(3) return c")
                .unwrap(),
            1
        );
        // A leaderboard walk sees a finished objective AND an unfinished one — the pair a
        // one-objective fixture cannot show.
        s.run("SelectQuestLogEntry(2)").unwrap();
        assert_eq!(
            s.eval::<i64>("return GetNumQuestLeaderBoards()").unwrap(),
            2
        );
        assert_eq!(
            s.eval::<(bool, bool)>(
                "local _,_,f1 = GetQuestLogLeaderBoard(1)                  local _,_,f2 = GetQuestLogLeaderBoard(2) return f1, f2"
            )
            .unwrap(),
            (false, true),
            "one objective outstanding, one done"
        );

        // The bind point — a plain string, and the reason it is seated is that an addon
        // CONCATENATES it (`Necrosis.lua:1089`), where an empty answer reads as no hearth at all.
        assert_eq!(
            s.eval::<String>("return GetBindLocation()").unwrap(),
            "Stormwind City"
        );

        // The purse — and asserted as its three coin fields, not as one number, because that is the
        // whole reason the value is 1234g 56s 78c rather than something round. A seat that reads
        // back as 12345678 but breaks down wrong is the bug this shape exists to catch.
        assert_eq!(s.eval::<i64>("return GetMoney()").unwrap(), 12_345_678);
        assert_eq!(
            s.eval::<(i64, i64, i64)>(
                "local m = GetMoney()                  return floor(m / 10000), mod(floor(m / 100), 100), mod(m, 100)"
            )
            .unwrap(),
            (1234, 56, 78),
            "gold/silver/copper are each non-zero, so no field can be dropped unnoticed"
        );

        // Equipped gear — head, chest, main hand, with the rest empty. Also exposed nothing.
        assert_eq!(
            s.eval::<String>(r#"return GetInventoryItemTexture("player", 16)"#)
                .unwrap(),
            "Interface\\Icons\\INV_Misc_QuestionMark"
        );
        assert!(
            s.eval::<String>(r#"return GetInventoryItemLink("player", 5)"#)
                .unwrap()
                .contains("Bloodmail Hauberk"),
            "the chest slot's link"
        );
        assert_eq!(
            s.eval::<i64>(r#"return GetInventoryItemQuality("player", 1)"#)
                .unwrap(),
            2
        );
        // An EMPTY slot still answers the absent shape — the case an addon walking 1..19 hits most.
        assert!(s
            .eval::<bool>(r#"return GetInventoryItemLink("player", 10) == nil"#)
            .unwrap());
        // The slot ids come from the same table `GetInventorySlotInfo` serves, so a walk that
        // resolves names to ids lands on the seated items rather than past them.
        assert_eq!(
            s.eval::<i64>(r#"return GetInventorySlotInfo("MainHandSlot")"#)
                .unwrap(),
            16
        );

        // The spellbook, as the positive control — this seat DID move the columns (+2).
        assert_eq!(
            s.eval::<String>(r#"return GetSpellName(1, "spell")"#)
                .unwrap(),
            "Attack"
        );
        assert_eq!(
            s.eval::<String>(r#"local _, r = GetSpellName(1, "spell") return r"#)
                .unwrap(),
            "",
            "and its rank is the empty string, not nil"
        );
    }

    fn wanted(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    /// **An oracle that cannot probe reports EVERYTHING missing, never nothing.**
    ///
    /// [`widget_method_kinds`] asks the live `__index` dispatcher, so it depends on
    /// `CreateFrame` still being callable — and an addon is perfectly free to replace it. The
    /// failure has to land in the direction that gets noticed: a blanket over-report is loud and
    /// obviously wrong, while a silent empty answer reads as "this addon calls no method we lack",
    /// which is the exact sentence this whole census exists to stop being invisible.
    ///
    /// The first assertion is what stops the fix being trivially satisfied by always over-reporting.
    #[test]
    fn the_method_oracle_fails_loudly_when_it_cannot_probe() {
        let script = seated_vm();
        let want = wanted(&["SetWidth", "SetTexCoord"]);
        assert!(
            unresolved_from(&widget_method_kinds(&script, &want), &want).is_empty(),
            "a frame method and a region method we both ship must resolve"
        );

        script
            .run("CreateFrame = function() error('no frames for you') end")
            .unwrap();
        assert_eq!(
            unresolved_from(&widget_method_kinds(&script, &want), &want),
            vec!["SetTexCoord".to_string(), "SetWidth".to_string()],
            "with no probes there is no answer, and the honest report of no answer is the whole \
             wanted set"
        );
    }

    /// **The PER-KIND pass fails loudly too** — the same rule one level down, and it needs its own
    /// test because the per-kind pass has a second way to answer nothing: no *attributed* call
    /// sites.
    ///
    /// With the oracle dead, every typed call site is reported against its kind and the ambiguous
    /// table is empty (a name whose answer we cannot compute is not "conditional", it is unknown,
    /// and it is already in the loudest bucket — `missing_methods`).
    #[test]
    fn the_per_kind_census_fails_loudly_when_it_cannot_probe() {
        let script = seated_vm();
        let wants = Wants {
            missing_globals: Vec::new(),
            missing_tables: Vec::new(),
            wanted_methods: wanted(&["SetWidth", "SetInsertMode"]),
            tested_methods: BTreeSet::new(),
            kind_calls: [("Frame".to_string(), "SetWidth".to_string())]
                .into_iter()
                .collect(),
            global_calls: BTreeSet::new(),
            loose_methods: wanted(&["SetInsertMode"]),
        };
        let (missing, ambiguous) = per_kind_rows(
            &script,
            &widget_method_kinds(&script, &wanted(&["SetWidth", "SetInsertMode"])),
            &wants,
        );
        assert!(
            missing.is_empty(),
            "a live oracle answers SetWidth on a Frame: {missing:?}"
        );
        assert_eq!(
            ambiguous,
            vec!["SetInsertMode (only on MessageFrame)".to_string()],
            "...and the untypable one is reported as conditional, with the kind named"
        );

        // Now break it. `None` is the oracle saying it could not run.
        let (missing, ambiguous) = per_kind_rows(&script, &None, &wants);
        assert_eq!(
            missing,
            vec!["Frame:SetWidth (on no kind)".to_string()],
            "with no answer at all, every typed call site is reported — loud, not silent"
        );
        assert!(
            ambiguous.is_empty(),
            "and nothing is called merely conditional when nothing is known: {ambiguous:?}"
        );
    }

    /// **A method on a sibling kind is invisible to the ANY-kind oracle, and the PER-KIND pass
    /// finds it** — decision 1228's bound, and the fix for it, asserted together.
    ///
    /// 1228 landed this test to pin a bound it deliberately left open: the census resolved a name
    /// against every probe and stopped at the first `function`, so a name survived only if *no*
    /// widget answered it. `MessageFrame:AddMessage` therefore scored zero for as long as it
    /// existed — the ScrollingMessageFrame probe answered it — while `EasyCopy`, `QuestHistory`
    /// and `QuestItem` all died on `UIErrorsFrame:AddMessage`.
    ///
    /// Both halves are still true, and both are asserted here, because they are now *different
    /// tables*: `missing_methods` still answers ANY-kind and still cannot see this (its numbers
    /// have to stay comparable — 1209), while `kind_missing_methods` asks the kind the addon
    /// actually called it on. `SetInsertMode` is the probe: a MessageFrame binding and only a
    /// MessageFrame binding (1228 §3's table), so a ScrollingMessageFrame calling it is exactly the
    /// shape that used to be unfindable.
    ///
    /// The control below it is what stops the pass being satisfied by reporting everything: the
    /// same verb on the kind that *does* answer it produces no row at all.
    #[test]
    fn a_method_on_a_sibling_kind_is_invisible_to_any_and_found_per_kind() {
        let script = seated_vm();
        let want = wanted(&["SetInsertMode"]);
        let oracle = widget_method_kinds(&script, &want);
        assert!(
            unresolved_from(&oracle, &want).is_empty(),
            "the ANY-kind table answers `some widget has it`, so a per-kind gap is still not what \
             IT finds — and that number must not move"
        );

        let called_on = |kind: &str| {
            per_kind_rows(
                &script,
                &oracle,
                &Wants {
                    missing_globals: Vec::new(),
                    missing_tables: Vec::new(),
                    wanted_methods: want.clone(),
                    tested_methods: BTreeSet::new(),
                    kind_calls: [(kind.to_string(), "SetInsertMode".to_string())]
                        .into_iter()
                        .collect(),
                    global_calls: BTreeSet::new(),
                    loose_methods: BTreeSet::new(),
                },
            )
            .0
        };
        assert_eq!(
            called_on("ScrollingMessageFrame"),
            vec!["ScrollingMessageFrame:SetInsertMode (on MessageFrame)".to_string()],
            "the row 1228 could not print: the sibling that DOES answer it is named, because \
             `wire the kind` and `write the verb` are different jobs"
        );
        assert!(
            called_on("MessageFrame").is_empty(),
            "...and the kind that answers it produces nothing — the pass is not just over-reporting"
        );
    }

    /// **The receiver of a published name is typed off the LIVE ARENA**, which is what lets the
    /// census see `UIErrorsFrame:AddMessage` as a MessageFrame call at all.
    ///
    /// Both directions are pinned, because the attribution is only worth what its source is worth:
    /// our `UIErrorsFrame` really is a `<MessageFrame>` (1228 converted it), `UIParent` is a plain
    /// `<Frame>`, `GameTooltipTextLeft1` is a **region** — and a name nothing publishes is `None`
    /// rather than a guess.
    #[test]
    fn a_published_name_carries_its_kind() {
        let script = seated_vm();
        assert_eq!(script.widget_kind("UIErrorsFrame"), Some("MessageFrame"));
        assert_eq!(script.widget_kind("UIParent"), Some("Frame"));
        assert_eq!(
            script.widget_kind("ChatFrame1"),
            Some("ScrollingMessageFrame")
        );
        assert_eq!(script.widget_kind("GameTooltip"), Some("GameTooltip"));
        assert_eq!(
            script.widget_kind("GameTooltipTextLeft1"),
            Some("FontString"),
            "the region leaves publish into their own name table, and the corpus scrapes them"
        );
        assert_eq!(script.widget_kind("NoSuchFrameAnywhere"), None);
    }

    /// **The UI probe actually INVOKES an addon's override**, which is the only thing that makes it
    /// different from every other column here.
    ///
    /// Earned two ways rather than trusted: a hook is installed the way Bagnon installs one, the
    /// probe is driven, and the hook's own counter is read back. A probe that silently found no
    /// entry point would report a clean run forever and be worse than not having it — which is
    /// exactly the failure mode the corpus's first run looked like (zero addons failing it).
    #[test]
    fn the_ui_probe_reaches_an_addon_override() {
        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        seat_a_session(&mut script);
        let _ = crate::ui_script::load_default_ui(&script);

        // Bagnon's idiom, on the verb Bagnon actually replaces.
        script
            .run("BENILLA_PROBE_HITS = 0 ToggleBackpack = function() BENILLA_PROBE_HITS = BENILLA_PROBE_HITS + 1 end")
            .unwrap();

        let errs = drive_ui_probe(&mut script);
        assert!(errs.is_empty(), "the probe must not raise here: {errs:?}");
        assert_eq!(
            script.eval::<i64>("return BENILLA_PROBE_HITS").ok(),
            Some(2),
            "the probe drives ToggleBackpack twice (open, then the override's close path)"
        );
    }

    /// And it RECORDS a raise rather than swallowing it — the error is the finding.
    #[test]
    fn the_ui_probe_records_what_an_override_raises() {
        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        seat_a_session(&mut script);
        let _ = crate::ui_script::load_default_ui(&script);
        script
            .run("ToggleBackpack = function() error('addon blew up') end")
            .unwrap();

        let errs = drive_ui_probe(&mut script);
        assert!(
            errs.iter().any(|e| e.contains("addon blew up")),
            "a raising override must land in the probe column: {errs:?}"
        );
    }

    /// **The HOVER arm reaches an addon's tooltip hook, and reports what it raises.**
    ///
    /// Nothing else in the probe puts a tooltip on screen, so an addon that hooks `GameTooltip`'s
    /// `OnShow` or scrapes its line regions was invisible to this column — the gap decision 1220
    /// named after fixing a raise squarely inside it.
    ///
    /// The assertion is deliberately the RAISE, not the call count. When the hover arm landed the
    /// column did not move at all, and "found nothing" has to be distinguishable from "ran
    /// nothing" — this instrument has already shipped once in the second state
    /// (see `drive_ui_probe`'s own comment), and a probe that cannot fail is not a probe.
    #[test]
    fn the_ui_probe_hovers_and_records_what_a_tooltip_hook_raises() {
        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        seat_a_session(&mut script);
        let _ = crate::ui_script::load_default_ui(&script);
        // The corpus shape: hook the tooltip's own OnShow, which only runs if something shows it.
        script
            .run(
                "GameTooltip:SetScript(\"OnShow\", function() error(\"tooltip hook blew up\") end)",
            )
            .unwrap();

        let errs = drive_ui_probe(&mut script);
        assert!(
            errs.iter().any(|e| e.contains("tooltip hook blew up")),
            "the hover arm must actually show the tooltip, and report the hook's raise: {errs:?}"
        );
    }

    /// ...and the hover arm raises NOTHING on a clean VM with no addon loaded.
    ///
    /// Without this, a probe arm that is simply broken would charge its own failure to all 218
    /// addons — the mis-attribution 1209 exists about, arriving through the instrument instead of
    /// through a claim.
    #[test]
    fn the_ui_probe_is_silent_on_a_clean_vm() {
        let mut script = UiScript::new().unwrap();
        script.set_screen_size(1024.0, 768.0);
        seat_a_session(&mut script);
        let _ = crate::ui_script::load_default_ui(&script);

        let errs = drive_ui_probe(&mut script);
        assert!(
            errs.is_empty(),
            "no addon is loaded — every probe error here would be charged to somebody else: {errs:?}"
        );
    }
}
