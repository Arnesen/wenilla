//! `addon_harness` — load a folder of addons, one per VM, and print what happened.
//!
//! ```text
//! cargo run -q -p benilla-app --example addon_harness -- <folder> [--verbose] [--why <substr>] [--status <file>] [--diff <file>]
//! ```
//!
//! The instrument decision 1188 phase 6 asks for: *"which addons work" is a number that can be
//! re-read on any day*. The mechanics, and what the numbers are and are not worth, are in
//! [`benilla_app::addon_harness`]'s module doc — read it before quoting anything from here.
//!
//! **Expect a long tail and do not chase 100 %** (1188's own instruction). The report is a
//! distribution, not a pass/fail: a handful of addons will always want features we have not built.
use benilla_app::addon_harness;

/// How much traceback `--why` prints per addon. Two lines gave the message and mlua's first
/// frame, which was enough to RANK a row and never enough to FIND one: chasing a `SetTexture`
/// failure to its call site needed the frames below it, and the addon/file/line only appear there.
const WHY_TRACEBACK_LINES: usize = 8;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(root) = args.next() else {
        eprintln!(
            "usage: addon_harness <folder of addons> [--verbose] [--why <blocker substring>]"
        );
        std::process::exit(2);
    };
    let rest: Vec<String> = args.collect();
    let verbose = rest.iter().any(|a| a == "--verbose");
    // `--why <substring>` — the addons behind one row, with their verbatim errors. The
    // ranked table collapses quoted names by design (1193); this is the read-back, and two of this
    // arc's decisions came from doing it by hand (1206, 1210). It matches the normalised row AND
    // the raw text, so `--why <a row>` and `--why <a name>` both work — the second did not, and
    // that is what made the whole widget-method class unreadable here.
    //
    // It reads back through the **method demand tables** too, which nothing did: every ranking here
    // is built by scanning, three of them have opened with fiction (1210, 1218, 1227), and the rule
    // that came out of that is to open the corpus line before quoting a row. Verifying the per-kind
    // table's own first head cost a temporary debug print and a second corpus run for want of this.
    let why = rest
        .iter()
        .position(|a| a == "--why")
        .and_then(|i| rest.get(i + 1))
        .cloned();
    // `--status` writes the per-addon ok/fail roster; `--diff <file>` compares against one.
    //
    // **The instrument this survey was missing, and the reason it is missing is instructive.**
    // Every column here is a TOTAL, so a landing that fixes one addon and breaks another reads as
    // a clean zero — indistinguishable from a change that did nothing. That happened: the shadow
    // accessors (`7824c154`) gained FuBar_NavigatorFu and held the column at 107, so something was
    // lost, and there was no way to ask what. Every "zero delta" recorded in this arc before now
    // could have been hiding the same swap.
    // `--status <file>` WRITES the roster, rather than printing it. It printed at first, and the
    // obvious `--status > roster.txt` then captured the whole report — 53 report lines parsed as
    // addon rows, because `--diff` splits on the last space and almost anything satisfies that. An
    // instrument whose output needs a `grep` incantation to be usable is one that will be used
    // wrong; the file is the artefact, so the tool writes the file.
    let status = rest
        .iter()
        .position(|a| a == "--status")
        .and_then(|i| rest.get(i + 1))
        .cloned();
    let diff = rest
        .iter()
        .position(|a| a == "--diff")
        .and_then(|i| rest.get(i + 1))
        .cloned();
    let root = std::path::PathBuf::from(root);

    let reports = addon_harness::survey(&root);
    if reports.is_empty() {
        eprintln!(
            "no addons under {} — is that an AddOns folder?",
            root.display()
        );
        std::process::exit(1);
    }

    let loaded = reports.iter().filter(|r| r.loaded).count();
    let clean = reports
        .iter()
        .filter(|r| r.loaded && r.missing_globals.is_empty())
        .count();
    let blocked = reports
        .iter()
        .filter(|r| !r.missing_deps.is_empty())
        .count();

    println!("\n{} addon(s) under {}", reports.len(), root.display());
    // Which VM the survey ran against. Without an install there is no GlobalStrings.lua, ~5,000
    // globals are missing, and every number below is worse for a reason that has nothing to do
    // with the client — say so rather than letting two machines' numbers be compared in silence.
    println!(
        "  VM: our whole FrameXML + a seated session{}\n",
        if addon_harness::seated_with_global_strings() {
            " + the real GlobalStrings.lua"
        } else {
            "  ** no install found: GlobalStrings absent, these numbers are NOT comparable **"
        }
    );
    // The tree these numbers came from. Two runs are comparable only if this matches: in a dev
    // build `assets/ui` is read from the SOURCE TREE, so anything else editing the checkout moves
    // the headline with no rebuild. Quoting a delta across two different digests is how a wrong
    // attribution got into a decision record (1209).
    println!(
        "  FrameXML digest                    : {}",
        addon_harness::framexml_digest()
    );
    println!(
        "  loaded without a single load error : {loaded}/{}",
        reports.len()
    );
    println!(
        "  ...and calling nothing we lack     : {clean}/{}",
        reports.len()
    );
    println!("  with a dependency not installed    : {blocked}");
    // The stricter column, and the one that answers what the survey is really asking. Every other
    // number here is LOAD-time; this one drives the client's own session start
    // (ADDON_LOADED -> VARIABLES_LOADED -> PLAYER_LOGIN -> PLAYER_ENTERING_WORLD, then a second of
    // ticks) and reports what the addon's HANDLERS raised. Four decision records in this arc end
    // with "the headline cannot see this"; this is the number that can.
    let survived = reports
        .iter()
        .filter(|r| r.loaded && r.session_errors.is_empty())
        .count();
    println!(
        "  ...and survived a session start    : {survived}/{}",
        reports.len()
    );
    // The UI-probe column: of those, how many survive having their OVERRIDES actually invoked.
    // The director found this blind spot by playing the game — an addon that replaces
    // `ToggleBackpack` and is never called looked identical to one that works.
    let probed = reports
        .iter()
        .filter(|r| r.loaded && r.session_errors.is_empty() && r.probe_errors.is_empty())
        .count();
    println!(
        "  ...and survived a UI probe         : {probed}/{}",
        reports.len()
    );
    // **The render column** — the only one here that asks whether anything was DRAWN. Every number
    // above it asks whether something raised, and the director's Bagnon report raised nothing at
    // all: a window with a title, a gold line and no bag slots, scoring a clean pass on all four.
    // `addon_harness::render`'s header is the design and its honest bounds.
    let drew = reports
        .iter()
        .filter(|r| r.render.drew() != addon_harness::Drew::Nothing)
        .count();
    println!(
        "  ...and DREW something on screen    : {drew}/{}",
        reports.len()
    );
    let (own, overlay) = (
        reports
            .iter()
            .filter(|r| r.render.drew() == addon_harness::Drew::Own)
            .count(),
        reports
            .iter()
            .filter(|r| r.render.drew() == addon_harness::Drew::Overlay)
            .count(),
    );
    println!("      of those: {own} drew a window of their own, {overlay} painted onto ours");
    // The actionable list, and the reason this column exists: an addon that loads clean, survives a
    // session start AND a UI probe, and still puts nothing on screen. Nothing else here can name
    // one. Bagnon was on this list.
    let silent: Vec<&str> = reports
        .iter()
        .filter(|r| {
            r.loaded
                && r.session_errors.is_empty()
                && r.probe_errors.is_empty()
                && r.render.drew() == addon_harness::Drew::Nothing
        })
        .map(|r| r.name.as_str())
        .collect();
    if !silent.is_empty() {
        println!(
            "\n  CLEAN ON EVERY OTHER COLUMN AND DREW NOTHING ({}) — read this list first:",
            silent.len()
        );
        for chunk in silent.chunks(4) {
            println!("    {}", chunk.join("  "));
        }
    }

    // **The use column** — the only one here that TOUCHES anything. Every number above it asks
    // whether something ran or appeared; the director drew the line by hovering Bagnon's freshly
    // drawn bag slots and getting a wall of `attempt to call global
    // 'ContainerFrameItemButton_OnEnter'` while all five columns above scored it a pass.
    //
    // Printed as THREE numbers, never one, because "nothing raised" and "nothing was touched" are
    // different answers and a probe that quietly drove zero targets reporting "clean" is this
    // instrument's own oldest failure mode (`addon_harness::use_probe`'s header).
    let (survived_use, raised_use, untouched_use) = (
        reports
            .iter()
            .filter(|r| r.used.verdict() == addon_harness::Used::Survived)
            .count(),
        reports
            .iter()
            .filter(|r| r.used.verdict() == addon_harness::Used::Raised)
            .count(),
        reports
            .iter()
            .filter(|r| {
                r.render.drew() != addon_harness::Drew::Nothing
                    && r.used.verdict() == addon_harness::Used::Untouched
            })
            .count(),
    );
    println!(
        "  ...and SURVIVED BEING USED         : {survived_use}/{}",
        reports.len()
    );
    println!(
        "      of the {drew} that drew: {} raised on hover/click/drag, {survived_use} came \
         through clean, {untouched_use} had nothing a pointer can reach",
        raised_use
    );
    println!(
        "      (input driven: hover in+out, left click, right click, drag — up to {} of the \
         frames each addon painted itself)",
        addon_harness::MAX_USE_TARGETS
    );
    // The list this column exists to be able to print: an addon whose UI is fully on screen and
    // falls over the moment anyone uses it.
    let inert: Vec<String> = reports
        .iter()
        .filter(|r| r.used.verdict() == addon_harness::Used::Raised)
        .map(|r| format!("{}({})", r.name, r.used.errors.len()))
        .collect();
    if !inert.is_empty() {
        println!(
            "\n  DREW SOMETHING AND IS DEAD TO THE TOUCH ({}) — (n) = raises, `--why <name>` for \
             the text:",
            inert.len()
        );
        for chunk in inert.chunks(4) {
            println!("    {}", chunk.join("  "));
        }
    }
    // ...and what the input broke, ranked like the session-start table above it.
    let mut used_rows: std::collections::BTreeMap<String, usize> = Default::default();
    for r in &reports {
        if let Some(e) = r.used.errors.first() {
            *used_rows.entry(addon_harness::normalise(e)).or_default() += 1;
        }
    }
    if !used_rows.is_empty() {
        let mut rows: Vec<(String, usize)> = used_rows.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        println!("\n  what broke when the UI was USED (first error each):");
        for (err, count) in rows.into_iter().take(12) {
            println!("    {count:>4}  {err}");
        }
    }

    // What the session start broke, ranked the way `blockers` ranks load failures.
    let mut session: std::collections::BTreeMap<String, usize> = Default::default();
    for r in reports.iter().filter(|r| r.loaded) {
        if let Some(e) = r.session_errors.first() {
            *session.entry(addon_harness::normalise(e)).or_default() += 1;
        }
    }
    if !session.is_empty() {
        let mut rows: Vec<(String, usize)> = session.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        println!("\n  what broke at SESSION START (addons that loaded clean, first error each):");
        for (err, count) in rows.into_iter().take(12) {
            println!("    {count:>4}  {err}");
        }
    }

    // The distribution, because a mean would hide the shape.
    let mut buckets = [0usize; 5];
    for r in &reports {
        let n = r.missing_globals.len();
        buckets[match n {
            0 => 0,
            1..=2 => 1,
            3..=5 => 2,
            6..=15 => 3,
            _ => 4,
        }] += 1;
    }
    println!("\n  missing-global count per addon:");
    for (label, n) in ["0", "1-2", "3-5", "6-15", "16+"].iter().zip(buckets) {
        println!("    {label:>5}  {n:>4}  {}", "#".repeat(n.min(60)));
    }

    // What actually STOPPED them — the ranked first error. Read this before the demand list: a
    // wall 60 addons hit is worth more than a verb 60 addons would like (decision 1193).
    println!("\n  what stopped them (addons whose FIRST load error was each):");
    for (err, count) in addon_harness::blockers(&reports).into_iter().take(12) {
        println!("    {count:>4}  {err}");
    }

    // Templates an addon names in `CreateFrame(..., "Template")` that we have never declared
    // (decision 1203). Its own list because it is invisible to every other number here: an
    // unresolved template raises no load error, so the addon scores as a pass and paints nothing.
    let templates = addon_harness::template_demand(&reports);
    if !templates.is_empty() {
        println!("\n  most-wanted missing TEMPLATES (addons naming each in CreateFrame):");
        for (name, count) in templates.into_iter().take(12) {
            println!("    {count:>4}  {name}");
        }
    }

    // The same question over the OTHER axis, and the one that actually moves the headline: a
    // template named in an addon's own XML `inherits=`. Unlike the list above this failure is
    // usually LOUD — the element's `<OnLoad>` fires at load and its first line is normally
    // `getglobal(this:GetName().."Text")` — which is why transcribing the reference's shared kit
    // was worth twelve addons while the CreateFrame list predicted none of them. Printed second
    // and separately because merging the two would hide exactly that difference.
    let inherits = addon_harness::inherits_demand(&reports);
    if !inherits.is_empty() {
        println!("\n  most-wanted missing TEMPLATES (addons naming each in an XML inherits=):");
        for (name, count) in inherits.into_iter().take(12) {
            println!("    {count:>4}  {name}");
        }
    }

    // Frames and tables, ranked separately: a missing function is a Rust verb to write, a missing
    // frame is FrameXML to transcribe, and the two queues go to different people. The scan was
    // blind to this whole shape until 2026-08-11 — a window 86 addons reach scored 0.
    let tables = addon_harness::table_demand(&reports);
    if !tables.is_empty() {
        println!("\n  most-wanted missing FRAMES/TABLES (addons indexing each):");
        for (name, count) in tables.into_iter().take(16) {
            println!("    {count:>4}  {name}");
        }
    }

    // **Widget METHODS** — the third queue, and the one this report was blind to while the arc
    // spent a day building exactly these (GetTexture, SetShadowColor, SetNonSpaceWrap, GetBackdrop
    // all landed within hours). A method is not a global and not an indexed table, so neither list
    // above could ever hold one; the only trace a missing method left was an error row reading
    // `attempt to call method 'X' (a nil value)`, with the name collapsed away by the very
    // normalisation that makes the row readable.
    //
    // Resolved against the live `__index` dispatcher, not a name list — so a whole *kind* left
    // unwired shows up here as loudly as a verb never written. It over-reports (a scanner cannot
    // type the receiver of a `:` call); `AddonReport::missing_methods` states exactly how far, and
    // `--why <name>` now reads any row back by name.
    let methods = addon_harness::method_demand(&reports);
    if !methods.is_empty() {
        println!("\n  most-wanted missing METHODS (addons calling each as obj:Name(), no widget has it):");
        for (name, count) in methods.into_iter().take(16) {
            println!("    {count:>4}  {name}");
        }
    }

    // **Per KIND** — the question the table above structurally cannot ask. It resolves a name
    // against every probe and stops at the first hit, so a verb wired to one class and forgotten on
    // its sibling comes back present: `MessageFrame:AddMessage` scored zero for as long as it
    // existed, answered by the ScrollingMessageFrame probe, while three corpus addons had
    // `UIErrorsFrame:AddMessage` as their FIRST load error (decision 1228). These rows are the
    // call sites whose receiver the survey could TYPE — from a `CreateFrame("Kind", …)` local, or
    // from the kind our own arena publishes that name as — asked against that kind alone.
    let by_kind = addon_harness::kind_method_demand(&reports);
    if !by_kind.is_empty() {
        println!(
            "\n  most-wanted missing METHODS BY KIND (receiver typed from the call site; \"on X\" \
             = the sibling that does answer it):"
        );
        for (name, count) in by_kind.into_iter().take(16) {
            println!("    {count:>4}  {name}");
        }
    }

    // ...and the residue: a receiver nothing could type, on a name whose answer DEPENDS on the
    // kind. An upper bound, printed rather than swallowed — resolving these against the whole probe
    // set and calling them present is the exact blindness above. The length of this table measures
    // the ATTRIBUTOR, not the widget surface: every receiver shape the scan learns to type moves
    // rows out of it.
    let ambiguous = addon_harness::ambiguous_method_demand(&reports);
    if !ambiguous.is_empty() {
        println!(
            "\n  ...and names an UNTYPABLE receiver may or may not have ({} in all — an upper \
             bound, not a queue; NARROWEST first, because a name only a minority of kinds answer \
             is the AddMessage shape and one every kind but the two regions answers is not):",
            ambiguous.len()
        );
        for (name, count) in ambiguous.into_iter().take(12) {
            println!("    {count:>4}  {name}");
        }
    }

    // The other half of the same scan, and never merged into it: methods addons call **behind a
    // feature test**, so nobody is stuck on one. `if sliderFrame.SetTopLevel then` is Dewdrop's,
    // and `SetTopLevel` is a real 1.12 widget method 60 corpus addons quietly do without. Building
    // one of these fixes no error and improves N addons' behaviour, which is a different decision
    // from the list above — so it is a different table.
    let optional = addon_harness::optional_method_demand(&reports);
    if !optional.is_empty() {
        println!("\n  ...and methods they FEATURE-TEST and work around (not blockers):");
        for (name, count) in optional.into_iter().take(8) {
            println!("    {count:>4}  {name}");
        }
    }

    println!("\n  most-wanted missing globals (addons wanting each):");
    for (name, count) in addon_harness::demand(&reports).into_iter().take(30) {
        println!("    {count:>4}  {name}");
    }

    if let Some(pattern) = &why {
        let hits = addon_harness::blocked_by(&reports, pattern);
        // "first error" was a lie in two directions and both cost this instrument a whole class:
        // the match ran against the NORMALISED row only, where every quoted name is already `'X'`,
        // so `--why GetBackdrop` answered `(none)` while an addon was dying on exactly that; and
        // it read only the first error, while a session start keeps firing handlers after one dies.
        // A row with no `#` is the addon's first error — the one the tables above rank — so the
        // index-less rows still count out to the ranked row exactly.
        println!(
            "\n  errors matching {pattern:?} ({}) — a row without a '#' is the FIRST error, i.e. \
             the one the tables above rank:",
            hits.len()
        );
        for (name, err) in &hits {
            println!("    {name}");
            // Two lines, not one. The first is the message; the SECOND is mlua's first traceback
            // frame, and for the row this instrument is most often pointed at that frame is the
            // whole answer — `in local '(for generator)'` is what tells a generic-for
            // (decision 1202) apart from any other call of a table value.
            for line in err.lines().take(WHY_TRACEBACK_LINES) {
                println!("        {}", line.trim());
            }
        }
        if hits.is_empty() {
            println!(
                "    (none — no load or session error of any addon contains that text, and no \
                 normalised row does either)"
            );
        }
        // ...and the same question of the DEMAND tables, which no read-back reached until now.
        // Every ranking above is a scan, and this arc has found fiction at the top of three of
        // them (1210, 1218, 1227); the rule that came out of it is "open the corpus line before
        // quoting the row", and this is what makes that one command instead of a code edit.
        let rows = addon_harness::method_rows_matching(&reports, pattern);
        println!(
            "\n  method-table rows matching {pattern:?} ({}) — which addons carry the row, and \
             from which table:",
            rows.len()
        );
        for (name, row) in &rows {
            println!("    {name:<36} {row}");
        }
        if rows.is_empty() {
            println!("    (none — no addon's method tables contain that text)");
        }
    }

    if verbose {
        println!("\n  per addon:");
        for r in &reports {
            let iface = r
                .interface
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            // The SESSION column, not just the load one. `loaded` means "no LOAD errors" and
            // always has (1213); an addon that loads clean and dies in its PLAYER_LOGIN handler
            // read as a pass here, which is the exact overstatement 1213 measured at four to one.
            // "does this addon actually work" is the two columns together, so print both.
            let session = match r.session_errors.first() {
                None if r.loaded => "session=ok".to_string(),
                None => "session=-".to_string(),
                Some(e) => format!("session: {}", e.lines().next().unwrap_or(e)),
            };
            // The render verdict, with the quad count and the frames it was charged to — the row
            // that turns "this addon is fine" into "this addon is fine AND you can see it".
            let drew = format!(
                "drew={}({})",
                r.render.drew().word(),
                r.render.own_quads + r.render.overlay_quads
            );
            // The use verdict, ALWAYS with the target count beside it — a bare `used=ok` would be
            // unreadable exactly where it matters, because it reads the same whether the probe
            // drove eight of the addon's frames or none at all.
            let used = format!(
                "used={}({}/{})",
                r.used.verdict().word(),
                r.used.driven,
                r.used.touchable
            );
            println!(
                "    {:<28} iface={:<12} {:<8} missing={:<4} {:<11} {drew:<16} {used:<20} {} {}",
                r.name,
                if iface.is_empty() { "-".into() } else { iface },
                if r.loaded { "loaded" } else { "ERRORS" },
                r.missing_globals.len(),
                session,
                r.render.frames.join(","),
                r.errors.first().map(String::as_str).unwrap_or("")
            );
        }
    }
    // The per-addon roster: `<name> ok|fail`, sorted, one per line — a shape `--diff` can read back
    // and a human can eyeball. `ok` is the SESSION column (loaded AND no session error), because
    // that is the one this arc treats as "works" (1213).
    let roster: Vec<(String, bool)> = {
        let mut v: Vec<(String, bool)> = reports
            .iter()
            .map(|r| (r.name.clone(), r.loaded && r.session_errors.is_empty()))
            .collect();
        v.sort();
        v
    };
    if let Some(path) = &status {
        let mut out = format!(
            "# per-addon status ({} ok / {} fail) — digest {}\n",
            roster.iter().filter(|(_, ok)| *ok).count(),
            roster.iter().filter(|(_, ok)| !*ok).count(),
            addon_harness::framexml_digest()
        );
        for (name, ok) in &roster {
            out.push_str(&format!("{name} {}\n", if *ok { "ok" } else { "fail" }));
        }
        match std::fs::write(path, out) {
            Ok(()) => println!("\n  wrote roster to {path} ({} addons)", roster.len()),
            Err(e) => println!("\n  --status {path}: {e}"),
        }
    }
    if let Some(path) = &diff {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                // **The digest gate.** `--status` stamps the roster's header with the FrameXML
                // digest it was taken at, and a delta is only attributable when this run's matches:
                // in a dev build `assets/ui` is read from the SOURCE TREE, so any *other* session
                // landing an interface change moves the headline under you with no rebuild of
                // yours. The stamp was written for this and then never checked, so the check was
                // the reader's to remember — which is exactly how 1209's wrong attribution reached
                // a decision record, and it nearly happened again on the run that added this gate
                // (a rebase pulled in a neighbour's interface change between the baseline and the
                // measurement).
                //
                // So this REFUSES rather than warns. A warning printed beside a `net +0` still
                // leaves the number sitting there to be quoted; withholding the number is the only
                // form that cannot be misread.
                let base_digest = text
                    .lines()
                    .find(|l| l.trim_start().starts_with("# per-addon status"))
                    .and_then(|l| l.split_whitespace().last());
                let now = addon_harness::framexml_digest().to_string();
                if let Some(was) = base_digest.filter(|d| **d != now) {
                    println!("\n  DIFF vs {path}: REFUSED — the baseline is at a different tree");
                    println!("    baseline digest {was}   this run {now}");
                    println!(
                        "    A delta across two digests is not attributable to your change \
                         (decision 1209).\n    Re-take the baseline on this tree: \
                         --status <file>, then re-run --diff."
                    );
                    return;
                }
                let base: std::collections::BTreeMap<&str, bool> = text
                    .lines()
                    .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
                    .filter_map(|l| l.rsplit_once(' '))
                    .map(|(n, v)| (n.trim(), v.trim() == "ok"))
                    .collect();
                let (mut gained, mut lost, mut appeared) = (Vec::new(), Vec::new(), Vec::new());
                for (name, ok) in &roster {
                    match base.get(name.as_str()) {
                        Some(was) if *was != *ok => {
                            if *ok {
                                gained.push(name)
                            } else {
                                lost.push(name)
                            }
                        }
                        None => appeared.push(name),
                        _ => {}
                    }
                }
                let vanished: Vec<&&str> = base
                    .keys()
                    .filter(|n| !roster.iter().any(|(m, _)| m == **n))
                    .collect();
                println!("\n  DIFF vs {path}");
                println!(
                    "    net {:+}  (gained {}, lost {})",
                    gained.len() as i64 - lost.len() as i64,
                    gained.len(),
                    lost.len()
                );
                // Both lists always print, even when empty. A silent "lost" section would let a
                // regression read as a clean run, which is the fault this whole mode exists for.
                for (label, list) in [("GAINED", &gained), ("LOST", &lost)] {
                    println!("    {label} ({}):", list.len());
                    for n in list.iter() {
                        println!("      {n}");
                    }
                }
                if !appeared.is_empty() || !vanished.is_empty() {
                    println!(
                        "    (roster changed: {} not in the baseline, {} baseline rows absent — \
                         the two runs surveyed different folders)",
                        appeared.len(),
                        vanished.len()
                    );
                }
            }
            Err(e) => println!("\n  --diff {path}: {e}"),
        }
    }
    println!();
}
