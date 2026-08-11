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
//! ## Running it
//!
//! ```text
//! cargo run -q -p benilla-app --example addon_harness -- <folder of addons>
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use benilla_ui::script::UiScript;
use benilla_ui::toc::Toc;

/// What one addon did.
#[derive(Debug, Clone)]
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
    /// Kept **separate from [`Self::loaded`] on purpose.** Folding these in would silently change
    /// what the headline means and make every number in every past decision record incomparable
    /// (1209's whole subject). `loaded` still means exactly "no LOAD errors"; this is a second,
    /// stricter column beside it.
    pub session_errors: Vec<String>,
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
    names
        .iter()
        .map(|n| survey_one(root, n, &installed))
        .collect()
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

fn survey_one(root: &Path, name: &str, installed: &BTreeSet<String>) -> AddonReport {
    let Some(toc_path) = manifest_path(root, name) else {
        return AddonReport {
            name: name.to_string(),
            interface: Vec::new(),
            loaded: false,
            errors: vec!["no manifest".into()],
            missing_globals: Vec::new(),
            missing_deps: Vec::new(),
            missing_templates: Vec::new(),
            missing_inherits: Vec::new(),
            missing_tables: Vec::new(),
            session_errors: Vec::new(),
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
                loaded: false,
                errors: vec![format!("VM: {e}")],
                missing_globals: Vec::new(),
                missing_deps,
                missing_templates: Vec::new(),
                missing_inherits: Vec::new(),
                missing_tables: Vec::new(),
                session_errors: Vec::new(),
            }
        }
    };
    script.set_screen_size(1024.0, 768.0);
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
    load_dependencies(&script, root, &toc, installed, &mut BTreeSet::new());
    let known = globals_of(&script);

    let errors = load_addon_files(&script, root, name, &toc);
    let (missing_globals, missing_tables) = missing_calls(root, name, &toc, &known);
    // AFTER the addon's files: a template it declares in its OWN XML is registered by then, so it
    // is not missing. The check asks the VM's live registry, not a name list.
    let missing_templates = missing_templates(&script, root, name, &toc);
    let missing_inherits = missing_inherits(&script, root, name, &toc);
    let session_errors = drive_session_start(&mut script, name);

    AddonReport {
        name: name.to_string(),
        interface: toc.interface_versions(),
        loaded: errors.is_empty(),
        errors,
        missing_globals,
        missing_deps,
        missing_templates,
        missing_inherits,
        missing_tables,
        session_errors,
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
fn drive_session_start(script: &mut UiScript, name: &str) -> Vec<String> {
    let before = script.errors().len();
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
    script.errors().split_off(before)
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
        load_dependencies(script, root, &dep_toc, installed, seen);
        let _ = load_addon_files(script, root, dep, &dep_toc);
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
    script.set_realm_name("Harness");
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
            if let Err(e) = script.run_chunk(&bytes) {
                errors.push(format!("{file}: {e}"));
            }
            continue;
        }
        match benilla_ui::framexml::parse(&benilla_ui::source::decode(&bytes)) {
            Ok(doc) => {
                let base = path.rfind('/').map_or("", |i| &path[..i]);
                let report = benilla_ui::loader::load_in(script, &doc, base, &provider);
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
) -> (Vec<String>, Vec<String>) {
    let mut called = BTreeSet::new();
    let mut indexed = BTreeSet::new();
    let mut defined = BTreeSet::new();
    for path in source_files(root, name, toc) {
        if let Some(text) = read_text(root, &path) {
            scan_lua(&text, &mut called, &mut indexed, &mut defined);
        }
    }
    let absent = |set: BTreeSet<String>| -> Vec<String> {
        set.into_iter()
            .filter(|n| !known.contains(n) && !defined.contains(n))
            .collect()
    };
    // Kept as two lists, not one, for 1207's reason: two shapes ranked together mis-rank. A missing
    // FUNCTION is a verb to write in Rust; a missing FRAME or TABLE is FrameXML to transcribe, and
    // the two queues go to different places.
    (absent(called), absent(indexed))
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

/// Blank out Lua comments and string literals, preserving line structure.
///
/// **This is not tidying — it is the difference between a signal and a list of names.** Without
/// it the ranked demand is topped by `Author`, `Iriel`, `Tekkub` and `Knight`: words inside
/// `-- credits` comments and `"..."` messages that happen to sit before a `(`. Measured on the
/// vanilla corpus, stripping removes over a thousand phantom call targets, and every one of the
/// top four was one.
fn strip_lua_noise(text: &str) -> String {
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
            while i + 1 < src.len() && !(src[i] == ']' && src[i + 1] == ']') {
                if src[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(src.len());
            continue;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            while i < src.len() && src[i] != quote {
                if src[i] == '\\' {
                    i += 1; // an escaped quote does not close the literal
                }
                i += 1;
            }
            i = (i + 1).min(src.len());
            out.push_str("\"\""); // keep it an expression, drop its contents
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Collect API-shaped call sites and the file's own top-level definitions.
fn scan_lua(
    text: &str,
    called: &mut BTreeSet<String>,
    indexed: &mut BTreeSet<String>,
    defined: &mut BTreeSet<String>,
) {
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
            let qualified = i > 0 && (bytes[i - 1] == '.' || bytes[i - 1] == ':');
            let (word, next) = ident(i);
            let mut j = next;
            while j < bytes.len() && bytes[j] == ' ' {
                j += 1;
            }
            if !qualified && j < bytes.len() && bytes[j] == '(' {
                let first = word.chars().next().unwrap_or('_');
                if first.is_uppercase() {
                    called.insert(word.clone());
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
            if !qualified && j < bytes.len() && (bytes[j] == '.' || bytes[j] == ':') {
                let first = word.chars().next().unwrap_or('_');
                if first.is_uppercase() {
                    indexed.insert(word.clone());
                }
            }
            if word == "function" {
                let mut k = next;
                while k < bytes.len() && bytes[k] == ' ' {
                    k += 1;
                }
                if k < bytes.len() && (bytes[k].is_alphabetic() || bytes[k] == '_') {
                    let (fname, _) = ident(k);
                    defined.insert(fname);
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
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in reports {
        for g in &r.missing_globals {
            *counts.entry(g.as_str()).or_default() += 1;
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
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in reports {
        for t in &r.missing_templates {
            *counts.entry(t.as_str()).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// [`demand`] over [`AddonReport::missing_tables`] — the frames and tables an addon indexes and we
/// do not have. Its own list because a missing frame is FrameXML to transcribe, not a Rust verb.
pub fn table_demand(reports: &[AddonReport]) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in reports {
        for g in &r.missing_tables {
            *counts.entry(g.as_str()).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// [`template_demand`] over [`AddonReport::missing_inherits`] — the same ranking, the other axis.
pub fn inherits_demand(reports: &[AddonReport]) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in reports {
        for t in &r.missing_inherits {
            *counts.entry(t.as_str()).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
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
/// `pattern` is a plain substring match against the NORMALISED row, so it is the row as printed.
pub fn blocked_by(reports: &[AddonReport], pattern: &str) -> Vec<(String, String)> {
    reports
        .iter()
        .filter_map(|r| {
            // A failed addon is read back through its LOAD error; one that loaded clean is read
            // back through what its handlers raised at session start. Both, because the two tables
            // now share a normalisation and a reader asking "which addons?" means whichever table
            // they were looking at. The label says which.
            let (first, label) = if r.loaded {
                (r.session_errors.first()?, "session")
            } else {
                (r.errors.first()?, "load")
            };
            normalise_error(first)
                .contains(pattern)
                .then(|| (format!("{} [{label}]", r.name), first.clone()))
        })
        .collect()
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
        let mut called = BTreeSet::new();
        let mut indexed = BTreeSet::new();
        let mut defined = BTreeSet::new();
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
            &mut called,
            &mut indexed,
            &mut defined,
        );
        let missing: Vec<&str> = called
            .iter()
            .filter(|n| !defined.contains(*n))
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
        let mut called = BTreeSet::new();
        let mut indexed = BTreeSet::new();
        let mut defined = BTreeSet::new();
        scan_lua(
            "local GetTime = GetTime\n\
             local UnitName, UnitClass = UnitName, UnitClass\n\
             local MyCache = MyCache or {}\n\
             local Helper = function() end\n\
             GetTime() UnitName('player') UnitClass('player') MyCache() Helper()\n",
            &mut called,
            &mut indexed,
            &mut defined,
        );
        let mut missing: Vec<&str> = called
            .iter()
            .filter(|n| !defined.contains(*n))
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
            interface: vec![],
            loaded,
            errors,
            missing_globals: vec![],
            missing_deps: vec![],
            missing_templates: vec![],
            missing_inherits: vec![],
            missing_tables: vec![],
            session_errors: vec![],
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
        let mut called = BTreeSet::new();
        let mut indexed = BTreeSet::new();
        let mut defined = BTreeSet::new();
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
            &mut called,
            &mut indexed,
            &mut defined,
        );
        let live: Vec<&str> = indexed
            .iter()
            .filter(|n| !defined.contains(*n))
            .map(String::as_str)
            .collect();
        assert_eq!(
            live,
            vec!["ColorPickerFrame", "GameTooltip"],
            "the addon's own MyAddon and its local Cache are its own; `self` is lowercase and \
             qualified reads never count"
        );
        assert_eq!(
            called.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["UnitName"],
            "and the two lists do not bleed into each other"
        );
    }

    /// `blocked_by` matches the row **as printed**, not the raw error — which is the whole point,
    /// because the printed row is what a reader has in front of them when they ask "which addons?".
    ///
    /// The subtlety it pins: a quoted name is already `'X'` by then, so searching for the real name
    /// finds nothing and searching for the collapsed row finds everything. Both halves asserted.
    #[test]
    fn blocked_by_reads_back_through_the_normalised_row() {
        let report = |name: &str, loaded: bool, first: &str| AddonReport {
            name: name.into(),
            interface: vec![],
            loaded,
            errors: vec![first.into()],
            missing_globals: vec![],
            missing_deps: vec![],
            missing_templates: vec![],
            missing_inherits: vec![],
            missing_tables: vec![],
            session_errors: vec![],
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
        assert!(
            blocked_by(&reports, "tinsert").is_empty(),
            "the quoted name is already 'X' in the row the reader is holding"
        );
        // ...and the verbatim error comes back, which is the reason to run it at all.
        assert!(wall[0].1.contains("tinsert"));
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
}
