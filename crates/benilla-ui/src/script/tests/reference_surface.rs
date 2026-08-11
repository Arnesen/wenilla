//! **The 1.12 surface is the contract** (decisions 1188 §2, 1189) — enforced here rather than
//! trusted to memory.
//!
//! benilla targets the 1.12.1 API. Anything beyond it is a *listed, justified* exception, and the
//! point of this file is that the list cannot go unknown: every global our VM exposes must either
//! be one 1.12 has, or appear below with a reason. A session that adds an Era-shaped verb has to
//! delete or extend an assertion that says why not — which is exactly what did not happen when
//! 1187 shipped eight Era globals on Era's authority and 1189 had to take them back out.
//!
//! **Why a superset is not free.** Lua branches on presence. An addon writing `if strmatch then`
//! takes a path we cannot honour, and the failure surfaces far from the cause. Extra functions are
//! harmless only if nothing feature-detects, which is not true of this ecosystem.
//!
//! The reference side is `reference/1.12-globals.tsv` — the running 1.12.1 client's own in-world
//! `_G` (see `reference/README.md`). This test reads the `engine` and `lua` rows: the surface a VM
//! is responsible for. It deliberately does **not** assert the converse — that we have everything
//! 1.12 has — because that is a multi-year backlog, not a regression gate;
//! `scripts/api-coverage.sh` is where that number is read.

use std::collections::HashSet;

use super::common::script;

/// `reference/1.12-globals.tsv`, as `name -> origin`.
fn reference() -> Vec<(String, String)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../reference/1.12-globals.tsv"
    );
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("reading {path}: {e} — regenerate with scripts/gen-reference-globals.py")
    });
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?.to_string(), f.nth(1)?.to_string()))
        })
        .collect()
}

/// Names benilla exposes that 1.12 does not, each with the reason it is allowed to stay.
///
/// **`Benilla*` and `__benilla_*` are covered by prefix**, not enumerated: the first is our host
/// bridge (verbs only our own transcribed FrameXML calls — a paperdoll model's facing, an item's
/// stat block) and the second is the tick's pushed state. Neither is an API-target claim, and
/// neither is reachable by accident from an addon that means to call a WoW function.
fn allowed_beyond_1_12() -> HashSet<&'static str> {
    [
        // ── our Lua runtime is 5.1 where 1.12's is 5.0 ────────────────────────────────────────
        // `select` is load-bearing and stays on purpose — our transcribed FrameXML uses it in 16
        // files as the 5.1 spelling of 5.0's implicit `arg` table.
        //
        // `_G` is the last one inherited rather than chosen: 1.12's base library does not export
        // it (an addon reaches the globals with `getfenv(0)`, which is what AceLibrary does), but
        // our own `getglobal`/`setglobal` are written over it. Closing it means rewriting those
        // against the registry first.
        //
        // **This list has been shrinking as the dialect got measured**: `coroutine` left in 1194
        // with the 5.1-only members of `string`/`table`/`math`; `print` and `_VERSION` left in
        // 1197, when the RE dispatch read the base library's 36-entry array and neither was in it.
        // The list only ever covered globals — the members needed `dump_globals --members` before
        // anyone could see them at all.
        "_G",
        "select",
        // ── WoW API past 1.12 — 1188 phase 5's list, and the reason that phase exists ─────────
        // Every one of these predates 1189 and is used by our own transcribed FrameXML today.
        // Resolving each means either replacing it with its 1.12 equivalent (`UnitPower` →
        // `UnitMana`, which 1.12 has and we do not) or recording why it stays.
        "CancelUnitBuff",
        "GetCursorInfo",
        "GetGossipQuestInfo",
        "GetInventoryItemID",
        "GetNumGossipQuests",
        "GetPlayerFacing",
        "GetTradePartnerName",
        "IsGossipOptionCoded",
        "SelectGossipQuest",
        "SubmitChatInput",
        "UnitAura",
        "UnitIsAFK",
        "UnitIsDND",
        "strconcat",
        "strjoin",
        "strsplit",
        "strtrim",
        "tostringall",
        "wipe",
    ]
    .into_iter()
    .collect()
}

/// Every global benilla's VM exposes is one 1.12 has, or a listed exception.
///
/// The failure message is the point: it names what is new and tells you the two ways out, so the
/// next session cannot resolve it by deleting a bare assertion.
#[test]
fn our_globals_stay_inside_the_1_12_surface() {
    let reference = reference();
    let known: HashSet<&str> = reference.iter().map(|(n, _)| n.as_str()).collect();
    assert!(
        known.len() > 19_000,
        "the reference table looks truncated ({} names) — regenerate it",
        known.len()
    );
    let allowed = allowed_beyond_1_12();

    let ours: Vec<String> = script()
        .eval(
            "local out = {} \
             for k in pairs(_G) do if type(k) == 'string' then table.insert(out, k) end end \
             return out",
        )
        .expect("dump _G");

    let mut unlisted: Vec<&str> = ours
        .iter()
        .map(String::as_str)
        .filter(|n| !known.contains(n))
        .filter(|n| !allowed.contains(n))
        .filter(|n| !n.starts_with("Benilla") && !n.starts_with("__benilla_"))
        .collect();
    unlisted.sort_unstable();

    assert!(
        unlisted.is_empty(),
        "benilla exposes {} global(s) the 1.12.1 client does not, and they are not listed as \
         exceptions:\n    {}\n\n\
         1.12 is the target (decision 1188). Either give it its 1.12 spelling, or add it to \
         `allowed_beyond_1_12` in this file WITH the reason it has to stay — an unexplained \
         superset is what 1189 had to roll back.",
        unlisted.len(),
        unlisted.join(" ")
    );
}

/// The exception list is exact — no entry outlives the global it excuses.
///
/// Without this, a removed global leaves its excuse behind, and the list slowly stops describing
/// the code. That is how a "listed, justified exception" decays into residue nobody can explain.
#[test]
fn the_exception_list_has_no_dead_entries() {
    let known: HashSet<String> = reference().into_iter().map(|(n, _)| n).collect();
    let ours: HashSet<String> = script()
        .eval::<Vec<String>>(
            "local out = {} \
             for k in pairs(_G) do if type(k) == 'string' then table.insert(out, k) end end \
             return out",
        )
        .expect("dump _G")
        .into_iter()
        .collect();

    let mut dead: Vec<&str> = allowed_beyond_1_12()
        .into_iter()
        .filter(|n| !ours.contains(*n))
        .collect();
    dead.sort_unstable();
    assert!(
        dead.is_empty(),
        "these are excused as beyond-1.12 but benilla no longer exposes them — drop them from \
         `allowed_beyond_1_12`:\n    {}",
        dead.join(" ")
    );

    // An entry that 1.12 turns out to have is a different bug: the excuse is wrong, not stale.
    let mut wrong: Vec<&str> = allowed_beyond_1_12()
        .into_iter()
        .filter(|n| known.contains(*n))
        .collect();
    wrong.sort_unstable();
    assert!(
        wrong.is_empty(),
        "these are excused as beyond-1.12 but the 1.12.1 client DOES have them — they need no \
         excuse:\n    {}",
        wrong.join(" ")
    );
}
