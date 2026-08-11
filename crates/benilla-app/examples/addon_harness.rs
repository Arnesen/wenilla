//! `addon_harness` — load a folder of addons, one per VM, and print what happened.
//!
//! ```text
//! cargo run -q -p benilla-app --example addon_harness -- <folder of addons> [--verbose] [--why <substr>]
//! ```
//!
//! The instrument decision 1188 phase 6 asks for: *"which addons work" is a number that can be
//! re-read on any day*. The mechanics, and what the numbers are and are not worth, are in
//! [`benilla_app::addon_harness`]'s module doc — read it before quoting anything from here.
//!
//! **Expect a long tail and do not chase 100 %** (1188's own instruction). The report is a
//! distribution, not a pass/fail: a handful of addons will always want features we have not built.
use benilla_app::addon_harness;

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
    // `--why <substring>` — the addons behind one blocker row, with their verbatim first errors.
    // The ranked table collapses quoted names by design (1193); this is the read-back, and two of
    // this arc's decisions came from doing it by hand (1206, 1210).
    let why = rest
        .iter()
        .position(|a| a == "--why")
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

    println!("\n  most-wanted missing globals (addons wanting each):");
    for (name, count) in addon_harness::demand(&reports).into_iter().take(30) {
        println!("    {count:>4}  {name}");
    }

    if let Some(pattern) = &why {
        let hits = addon_harness::blocked_by(&reports, pattern);
        println!(
            "\n  addons whose first error matches {pattern:?} ({}):",
            hits.len()
        );
        for (name, err) in &hits {
            println!("    {name}");
            // Two lines, not one. The first is the message; the SECOND is mlua's first traceback
            // frame, and for the row this instrument is most often pointed at that frame is the
            // whole answer — `in local '(for generator)'` is what tells a generic-for
            // (decision 1202) apart from any other call of a table value.
            for line in err.lines().take(2) {
                println!("        {}", line.trim());
            }
        }
        if hits.is_empty() {
            println!("    (none — the pattern is matched against the NORMALISED row as printed)");
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
            println!(
                "    {:<28} iface={:<12} {:<8} missing={:<4} {}",
                r.name,
                if iface.is_empty() { "-".into() } else { iface },
                if r.loaded { "loaded" } else { "ERRORS" },
                r.missing_globals.len(),
                r.errors.first().map(String::as_str).unwrap_or("")
            );
        }
    }
    println!();
}
