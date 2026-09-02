//! **The return-shape gate** — `reference/1.12-shapes.tsv` against what this client actually
//! answers (decision 1842).
//!
//! `reference_surface` compares `_G` to `_G`, so a wrong *name* cannot land. Nothing compared
//! *shapes*, and that gap produced six decisions in two days — 1818, 1819, 1830, 1834, 1836 and
//! 1840 — every one of them a right name with a wrong arity, invisible to every gate we had and
//! invisible to the chain's own files too, because a Lua caller that uses one value of two never
//! notices the second.
//!
//! The table is wow-re's `re/audit/binding-shapes.tsv`, vendored: 1722 rows over all 82 registrar
//! tables, one per registered binding, generated and differentially tested against that repo's
//! other harvester. It carries the gate rule **per row** rather than in prose, which is the whole
//! reason it can be enforced instead of remembered:
//!
//! * `arity_conf = exact` — sound. This is the column to gate on.
//! * `kinds_conf` — trustworthy only where an independent push count agreed; not gated here.
//! * `argc_conf` — **never** gate on it. It is a deliberate over-flagging superset: any binding
//!   whose direct callee reads a Lua index is marked `lower-bound`, so a `lower-bound` row says
//!   nothing about the real argument count.
//!
//! ## Two things that make this narrower than it looks, both on purpose
//!
//! **Only query-shaped names are probed.** The gate calls each binding to count what it answers,
//! and calling an arbitrary global for its arity would mean calling `AbandonQuest`, `Quit` and
//! `AcceptBattlefieldPort` for theirs. Names are filtered to the query verbs — `Get*`, `Is*`,
//! `Has*`, `Can*`, `Unit*`, `Num*` — which is a loss of coverage, never a wrong assertion.
//!
//! **A call that raises is skipped, not failed.** 53 of the 83 unit-table bindings gate their
//! arguments and raise on a nil token (1834/1836), so a no-argument probe of those is expected to
//! throw; that says nothing about arity. Only a call that *returns* is measured.
//!
//! ## The name is not a key
//!
//! **60 names are registered from more than one table and 21 differ in arity across
//! registrations** — `GetBuildInfo` is arity 5 in the glue table `0x8373b8` and 3 in the in-game
//! core `0x83de68`; `GetAddOnInfo` is 8 and 7. This VM is the in-game one, so a name carrying rows
//! from both is read at its **non-glue** row, and a name whose remaining rows still disagree is
//! skipped rather than guessed at.
//!
//! That hazard is the reason the table is keyed on `fn` upstream, and it is a live merge hazard for
//! `1.12-globals.tsv`, which is name-keyed.

/// Names the gate knows are wrong and does not yet assert — **a list that may only shrink**.
///
/// **It is empty.** It held five entries whose miss-branch *contents* the shapes table does not
/// record — four globals and `Model:GetFogColor` — and 1845 answered all five at the bytes rather
/// than guessing them. The mechanism stays because the next divergence the gate finds will want it:
/// an entry needs an arity, a reason, and a dispatched question.
const NOT_YET_ASSERTED: &[(&str, usize, &str)] = &[];

/// The widget half's own shrinking list — see [`NOT_YET_ASSERTED`], same rules. Also empty:
/// `GetFogColor` came off it with 1845, which found the fog colour was a packed `0xAARRGGBB` dword
/// all along.
const WIDGET_NOT_YET_ASSERTED: &[&str] = &[];

/// The glue registrar table. This VM is the in-game one, so a duplicated name is read at its
/// other row (see the module doc).
const GLUE_TABLE: &str = "0x8373b8";

struct Row {
    name: String,
    table_va: String,
    arity: usize,
}

fn rows() -> Vec<Row> {
    let tsv = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../reference/1.12-shapes.tsv"
    );
    let text = std::fs::read_to_string(tsv).expect("reference/1.12-shapes.tsv");
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("name\t"))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            // name fn pair_va table_va table_kind argc argc_conf arity arity_conf kinds …
            if f.len() < 9 || f[4] != "global" || f[8] != "exact" {
                return None;
            }
            Some(Row {
                name: f[0].to_string(),
                table_va: f[3].to_string(),
                arity: f[7].parse().ok()?,
            })
        })
        .collect()
}

/// Every registered global whose arity the reference states exactly, answered by this client.
#[test]
fn every_query_binding_answers_the_reference_s_return_arity() {
    let all = rows();
    assert!(
        all.len() > 800,
        "the vendored table looks wrong: {}",
        all.len()
    );

    // Resolve the duplicated names the way the module doc describes.
    let mut by_name: std::collections::HashMap<&str, Vec<&Row>> = std::collections::HashMap::new();
    for r in &all {
        by_name.entry(&r.name).or_default().push(r);
    }

    let s = benilla_ui::script::UiScript::new().expect("VM");
    let mut checked = 0usize;
    let mut mismatches: Vec<String> = Vec::new();

    for (name, rs) in &by_name {
        // Query verbs only — the gate calls what it measures.
        if !["Get", "Is", "Has", "Can", "Unit", "Num"]
            .iter()
            .any(|p| name.starts_with(p))
        {
            continue;
        }
        let candidates: Vec<&&Row> = if rs.len() == 1 {
            rs.iter().collect()
        } else {
            rs.iter().filter(|r| r.table_va != GLUE_TABLE).collect()
        };
        // Still ambiguous after dropping glue: skip rather than guess.
        let Some(first) = candidates.first() else {
            continue;
        };
        if candidates.iter().any(|r| r.arity != first.arity) {
            continue;
        }
        let want = first.arity;

        // A call that raises says nothing about arity — 53 of the unit bindings gate their
        // arguments and raise on a nil token (1834/1836).
        let probe = format!(
            "if type({name}) ~= 'function' then return -1 end \
             local ok, n = pcall(function() return select('#', {name}()) end) \
             if not ok then return -1 end return n"
        );
        let got: i64 = match s.eval(&probe) {
            Ok(n) => n,
            Err(_) => continue,
        };
        if got < 0 {
            continue;
        }
        checked += 1;
        if got as usize != want {
            if let Some((_, known, _)) = NOT_YET_ASSERTED.iter().find(|(n, ..)| n == name) {
                assert_eq!(
                    *known, want,
                    "{name} is on the not-yet-asserted list at a stale arity — the table now says \
                     {want}"
                );
                continue;
            }
            mismatches.push(format!("{name}: answers {got}, reference states {want}"));
        }
    }

    assert!(
        checked >= 40,
        "the gate measured only {checked} bindings — it has stopped covering anything"
    );
    // The list may only SHRINK. An entry that now agrees is a fix nobody deleted the note for,
    // and leaving it would let the next real divergence hide behind it.
    let stale: Vec<&str> = NOT_YET_ASSERTED
        .iter()
        .map(|(n, ..)| *n)
        .filter(|n| {
            by_name.get(n).is_some_and(|rs| {
                let want = rs[0].arity;
                s.eval::<i64>(&format!(
                    "local ok, n = pcall(function() return select('#', {n}()) end) \
                     if not ok then return -1 end return n"
                ))
                .is_ok_and(|got| got >= 0 && got as usize == want)
            })
        })
        .collect();
    assert!(
        stale.is_empty(),
        "these now answer the reference's arity and must come off NOT_YET_ASSERTED: {stale:?}"
    );
    assert!(
        mismatches.is_empty(),
        "{} of {checked} probed bindings answer the wrong number of values:\n  {}",
        mismatches.len(),
        mismatches.join("\n  ")
    );
}

/// The widget registrar tables, each mapped to a Lua expression that produces one instance.
///
/// The table address IS the widget class — the reference registers one `{name, fn}` table per
/// widget family, and a method name can appear in several of them with different arities (the
/// `name-not-unique` case). So a widget method is probed on **its own class**, never on whichever
/// object happens to answer to the name.
///
/// Identified by each table's distinctive members: `GetTexCoord` is Texture's, `GetChecked` is
/// CheckButton's, `GetHyperlinkFormat` is SimpleHTML's, `GetScrollChild` is ScrollFrame's, and so
/// on. A table with no entry here is skipped, which costs coverage and never correctness.
const WIDGET_PROBES: &[(&str, &str, &str)] = &[
    ("0x878ec0", "Frame", "PGFrame"),
    ("0x879d00", "Button", "PGButton"),
    ("0x87bf74", "CheckButton", "PGCheck"),
    ("0x87bb68", "EditBox", "PGEdit"),
    ("0x87b260", "Slider", "PGSlider"),
    ("0x87b010", "StatusBar", "PGStatus"),
    ("0x87b3c0", "ScrollFrame", "PGScroll"),
    ("0x87ba80", "SimpleHTML", "PGHtml"),
    ("0x87abb0", "ColorSelect", "PGColor"),
    ("0x87b960", "MessageFrame", "PGMessage"),
    ("0x87b5c0", "ScrollingMessageFrame", "PGScrollMsg"),
    ("0x878948", "PlayerModel", "PGModel"),
];

/// The two region classes, which are made by a frame rather than by `CreateFrame`.
const REGION_PROBES: &[(&str, &str)] = &[
    ("0x87c128", "PGFrame:CreateTexture('PGTex')"),
    ("0x87c1d8", "PGFrame:CreateFontString('PGFS')"),
];

/// **The widget half of the gate** (decision 1843) — the half that would have caught 1840's
/// `GetTexCoord` without a byte read.
///
/// Same rule as the global half: `arity_conf = exact` only, query verbs only, a call that raises is
/// skipped. The difference is the receiver: each row is probed on an instance of the class its
/// registrar table belongs to, because the same method name lives in several tables with different
/// shapes.
#[test]
fn every_widget_method_answers_the_reference_s_return_arity() {
    let tsv = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../reference/1.12-shapes.tsv"
    );
    let text = std::fs::read_to_string(tsv).expect("reference/1.12-shapes.tsv");

    let s = benilla_ui::script::UiScript::new().expect("VM");
    let mut made: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    for (table, kind, name) in WIDGET_PROBES {
        if s.run(&format!(
            "{name} = CreateFrame(\"{kind}\", \"{name}\", UIParent)"
        ))
        .is_ok()
        {
            made.insert(table, (*name).to_string());
        }
    }
    for (table, expr) in REGION_PROBES {
        let var = format!("PGR{}", made.len());
        if s.run(&format!("{var} = {expr}")).is_ok() {
            made.insert(table, var);
        }
    }
    assert!(
        made.len() >= 8,
        "only {} widget classes could be instantiated — the probe set is broken",
        made.len()
    );

    let mut checked = 0usize;
    let mut mismatches: Vec<String> = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("name\t"))
    {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 9 || f[4] != "widget" || f[8] != "exact" {
            continue;
        }
        let (name, table) = (f[0], f[3]);
        if !["Get", "Is", "Has", "Can", "Num"]
            .iter()
            .any(|p| name.starts_with(p))
        {
            continue;
        }
        let Some(obj) = made.get(table) else { continue };
        let Ok(want) = f[7].parse::<usize>() else {
            continue;
        };

        let probe = format!(
            "if type({obj}.{name}) ~= 'function' then return -1 end \
             local ok, n = pcall(function() return select('#', {obj}:{name}()) end) \
             if not ok then return -1 end return n"
        );
        let Ok(got) = s.eval::<i64>(&probe) else {
            continue;
        };
        if got < 0 {
            continue;
        }
        checked += 1;
        if got as usize != want {
            if WIDGET_NOT_YET_ASSERTED.contains(&name) {
                continue;
            }
            mismatches.push(format!(
                "{obj}:{name} answers {got}, reference states {want}"
            ));
        }
    }

    assert!(
        checked >= 60,
        "the widget gate measured only {checked} methods — it has stopped covering anything"
    );
    assert!(
        mismatches.is_empty(),
        "{} of {checked} probed widget methods answer the wrong number of values:\n  {}",
        mismatches.len(),
        mismatches.join("\n  ")
    );
}
