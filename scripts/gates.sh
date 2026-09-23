#!/usr/bin/env bash
# The commit gates (docs/METHOD.md "Gates"), one vetted runner: fmt-check →
# clippy -D warnings → workspace tests (skips refused where the data is) → doc-links →
# pass-span-lint → the player build and its own tests → the enforcer. Each is introduced where it
# runs, below. Fail-fast, nonzero exit on ANY failure, the failing log's tail printed.
#
# Exists because ad-hoc gate chains keep masking failures: `cargo … | tail` reports the PIPE's
# exit code, and `grep -c` exits nonzero on a zero count — both bit real sessions (phase-1a of
# the social arc, then again at the 6a land). Run this instead of composing pipelines.
set -uo pipefail

# Gate the tree you are STANDING IN, not the one this file happens to live in. A machine with
# several checkouts reaches for the gates by absolute path — `scripts/gates.sh` resolved from
# `$0`, which is another checkout. That silently gated the wrong one: fmt-red work in one
# worktree reported ALL GATES GREEN, because the other was clean.
#
# So: the git toplevel of $PWD, as long as it is a checkout of THIS repo (it has this script) — a
# stray invocation from a sibling repo like wow-5875-re falls back rather than gating that repo with
# benilla's gates. And print the tree either way: the silence is what made the old bug invisible.
here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$root" ] && [ -f "$root/scripts/gates.sh" ] || root="$here"
cd "$root" || exit 1
echo "gating: $root"

# ── Green-stamp memoization (decision 1822) ──────────────────────────────────────────────────────
# A green chain stamps target/.gates-green with a key of exactly what determined the verdict: the
# WORKING TREE's true content hash (untracked files included — hashed through a throwaway index, the
# real one untouched, so `target/` and `WoW` stay excluded by the ignore rules), the
# toolchain, this script itself, and where the install resolver points. Re-running on an unchanged
# tree is then instant instead of ~5–8 min — which is what a land after an already-gated sync, or a
# "once more to be sure", actually costs the machine. GATES_FORCE=1 runs the chain regardless.
# The install path is compared lazily (its resolver is a `cargo run`, instant only on a warm
# target): a tree that doesn't match skips that probe entirely.
stamp="target/.gates-green"
tree_key() {
    local tmpidx t
    tmpidx="$(mktemp "${TMPDIR:-/tmp}/benilla-gates-idx.XXXXXX")" && rm -f "$tmpidx" || return 1
    t="$( (export GIT_INDEX_FILE="$tmpidx"
           git read-tree HEAD && git add -A . && git write-tree) 2>/dev/null )"
    rm -f "$tmpidx"
    [ -n "$t" ] || return 1
    printf '%s|%s|%s' "$t" "$(rustc -V 2>/dev/null)" \
        "$(shasum "$root/scripts/gates.sh" 2>/dev/null | cut -d' ' -f1)"
}
resolve_wow() { cargo run -q -p benilla-formats --example where 2>/dev/null || true; }
# **A docs-only delta keeps the verdict** (decision 2049). The stamped tree and the current one are
# both real tree objects (write-tree puts them in the object store), so git can say exactly what
# differs. If every differing path is under `docs/` or a top-level `*.md` — nothing compiles,
# tests, formats or `include_str!`s those, checked — then the chain's outcome on this tree IS the
# stamped one, by construction rather than by optimism. This is what lets a landing that only
# regenerated the map after a rebase go through without paying the chain again.
# A stamped tree that the object store has since pruned simply fails the check, and the chain runs.
docs_only_delta() { # $1 = stamped tree, $2 = current tree
    git cat-file -e "$1^{tree}" 2>/dev/null && git cat-file -e "$2^{tree}" 2>/dev/null || return 1
    [ -z "$(git diff-tree -r --name-only "$1" "$2" 2>/dev/null | grep -vE '^(docs/|[^/]*\.md$)' || true)" ]
}
key_start="$(tree_key || true)"
if [ "${GATES_FORCE:-}" != "1" ] && [ -n "$key_start" ] && [ -f "$stamp" ]; then
    old="$(cat "$stamp" 2>/dev/null)"
    old_key="${old%|*}" # tree|rustc|script — the part that must match (bar the tree, see below)
    memo=""
    if [ "$old_key" = "$key_start" ]; then
        memo="this exact tree already passed"
    elif [ "${old_key#*|}" = "${key_start#*|}" ] && docs_only_delta "${old_key%%|*}" "${key_start%%|*}"; then
        memo="this tree differs from one that passed only in top-level *.md, which no gate reads (0979/2049)"
    fi
    if [ -n "$memo" ] && [ "${old##*|}" = "$(resolve_wow)" ]; then
        echo "ALL GATES GREEN (memoized — $memo; GATES_FORCE=1 re-runs)"
        [ -d target ] && printf '%s\t%s\t%s\t%s\t%s\n' "$(date -u +%FT%TZ)" \
            "$(git rev-parse --short HEAD 2>/dev/null || echo '-')" chain 0 memo >>target/.gates-timing 2>/dev/null
        echo "  (a clean run is the fourth gate: scripts/smoke.sh — live logout/re-login round trip)"
        exit 0
    fi
fi

log="$(mktemp "${TMPDIR:-/tmp}/benilla-gates.XXXXXX")"
skips="$(mktemp "${TMPDIR:-/tmp}/benilla-gates-skips.XXXXXX")"
trap 'rm -f "$log" "$skips"' EXIT

# **How hollow was that green?** The data-gated tests pass without asserting on a machine that
# lacks the install or the addon corpus, and libtest swallows the skip line — so `install.rs`'s
# `skipped` appends one to `$BENILLA_SKIP_LOG` instead, and this reads the file back after each
# test rung. A clone without the data sees the number rather than a green it cannot weigh
# (docs/CONTRIBUTING.md, "Setting up"). Where the data is, `BENILLA_REQUIRE_DATA` has already
# made every skip a failure and the file stays empty.
report_skips() { # $1 = rung name
    [ -s "$skips" ] || return 0
    echo "  $1: $(wc -l <"$skips" | tr -d ' ') data-gated tests SKIPPED on this machine —"
    sort "$skips" | uniq -c | sort -rn | sed 's/^ *\([0-9]*\) \(.*\)/    \1 × \2/'
    echo "    (they run where the data is: docs/CONTRIBUTING.md, \"Setting up\")"
    : >"$skips"
}

# ── Timing (decision 2265 §C1) ──────────────────────────────────────────────────────────────────
# Every gate's wall seconds, printed beside its verdict and appended to `target/.gates-timing`
# (gitignored with the rest of `target/`; one tab-separated line per gate per run: UTC time, tree
# short-sha, gate, seconds, verdict). 1822 measured the chain as the machine's single biggest load
# and scoped the per-round verify; its successor question — WHICH gate is the eleven minutes a
# land costs, and how that moves as the tree grows — could not be asked, because the only log was
# `rm -f`'d on exit and `run()` printed no duration. A memo hit and a skipped gate are recorded
# too, at 0 s, so a run's shape reads off the file whole.
timing="target/.gates-timing"
chain_t0=$SECONDS
tree_sha="$(git rev-parse --short HEAD 2>/dev/null || echo '-')"
note_timing() { # $1 = gate, $2 = seconds, $3 = verdict
    [ -d target ] || return 0
    printf '%s\t%s\t%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "$tree_sha" "$1" "$2" "$3" >>"$timing" 2>/dev/null || true
}

run() {
    local name="$1"
    shift
    local t0=$SECONDS
    if ! "$@" >"$log" 2>&1; then
        echo "GATE FAILED: $name ($((SECONDS - t0)) s)"
        note_timing "$name" "$((SECONDS - t0))" "FAILED"
        tail -30 "$log"
        exit 1
    fi
    echo "gate ok: $name ($((SECONDS - t0)) s)"
    note_timing "$name" "$((SECONDS - t0))" "ok"
}

run fmt cargo fmt --all -- --check
run clippy cargo clippy --workspace --all-targets -- -D warnings

# **Skips are refused where the data is** (decision 2329). `wow_data_or_skip!` and
# `addon_corpus_or_skip!` pass without asserting on a machine that lacks the install or the addon
# corpus, and libtest swallows a passing test's stderr — so on THIS machine a resolver that has
# drifted reads as green, which is how thirty corpus tests skipped at every land for three weeks
# after the pool moved drives. Where both resolve, `BENILLA_REQUIRE_DATA=1` turns a skip into the
# failure it is. Both, not either: with the install alone every corpus test would fail for the
# honest reason. Ask the resolver for the install (the same `where` the player-tests rung and the
# enforcer use); the corpus is the `wow-addons-vanilla` link at the tree's root.
wow_data="$(resolve_wow)"
require_data=""
if [ -n "$wow_data" ] && [ -d "$root/wow-addons-vanilla" ]; then
    require_data=1
else
    echo "gates: NOTE — install or addon corpus not found here (install='${wow_data:-none}'," \
         "corpus=$([ -d "$root/wow-addons-vanilla" ] && echo yes || echo no)):" \
         "data-gated tests skip silently on this machine"
fi
run test env ${require_data:+BENILLA_REQUIRE_DATA=1} BENILLA_SKIP_LOG="$skips" cargo test --workspace
report_skips test

# test-no-install: the same suite as a clone without client data runs it. A test that reads the
# install without opening with `wow_data_or_skip!` passes where the data is and panics everywhere
# else; `WOW_DATA=` (set, empty) is the resolver's "no install", so this run is that machine. No
# build variable reads it, so the binaries are reused and only the run time is paid.
run test-no-install env -u BENILLA_REQUIRE_DATA WOW_DATA= cargo test --workspace

# **doc-links** (decision 1925) — the docs are the knowledge base, so a doc link pointing at a
# DELETED item is rot, and nothing else here runs rustdoc. Deliberately narrow: it fails only on a
# `crate::`/`super::`/`self::`/`Self::` path whose leaf exists nowhere in the workspace. The broad
# `-D rustdoc::broken_intra_doc_links` was measured and rejected — 818 of its warnings are
# conventions this codebase writes on purpose (docs naming private internals by path, one crate
# naming another's module). The script's own header carries that measurement. ~22 s warm.
run doc-links scripts/doc-links.py

# **pass-span-lint** (decision 2258, bug B390) — one `pass_span` per render pass at a time. bevy's
# pass span is a pipeline-statistics query as well as a timestamp pair, wgpu allows one active at
# a time, and only Vulkan exposes the feature: Metal (this machine) and DX12 (the lab laptop's
# pinned backend) run a nested span as a silent no-op, which is how the 09-15 sync aborted every
# Linux and Vulkan-on-Windows player on their first world frame with every gate green. The runtime
# fact no compile or macOS run can reach, read off the source in a tenth of a second.
run pass-span-lint scripts/pass-span-lint.py

# **The player build** (decision 1173's deliverable; built in 1174). `benilla` is the binary a
# player would run, and `--no-default-features` drops the `dev` feature — so the debug panel, the
# perf HUD, the inspector, the capture harness and the probe fleet are not compiled at all. It
# fails the moment anything outside those roots names one of them.
#
# This line IS the mechanism, not the module boundary above it. 0026 adopted "add no new
# gameplay→dev coupling" in June 2026 with nothing to fail; by August there were 24 references
# across 12 files, because a rule with no failing build is a wish (1173). Same lesson as the
# enforcer below: a tripwire nobody trips is not a tripwire.
#
# `build`, not `test`: the seam's claim is that the player binary LINKS without the instruments.
# The tests are dev-side by construction (they drive fixtures and probes) and stay on the default
# feature set, where `cargo test --workspace` above already runs them.
run player-build cargo build -p benilla --no-default-features

# **The player build's own unit tests** (decision 1175). A build proves the seam LINKS; it cannot
# prove the player configuration BEHAVES. The two facts 1175 rests on are both `#[cfg(not(feature =
# "dev"))]` — the install resolver looks nowhere inside the source tree, and the state folder lands
# beside the binary — so `cargo test --workspace` above, which runs with default features, never
# executes either of them. A falsifier that never runs is 1173's wish with a `#[test]` on it.
#
# `--lib` only: the integration tests and examples are dev-side by construction. Measured at ~7 s
# warm, because these two crates' unit tests are pure.
#
# **`$WOW_DATA` is handed in, and that is not a loophole.** Decision 1751 began sourcing FrameXML
# off the player's own patch chain, so the shipped-UI tests in this same `--lib` set now need an
# install — while the whole point of `--no-default-features` is that the resolver's *source-tree*
# rung is compiled out. The two collided silently: 27 tests here failed for a missing
# `Interface\FrameXML\ContainerFrame.xml` on every machine whose shell did not happen to export
# the variable, and the gate had been red for that reason rather than for anything a commit did.
# `$WOW_DATA` is rung ONE of the ladder, not the dev rung, so both facts 1175 asks this gate to
# falsify still hold: nothing here reads the source tree, and `install.rs`'s own tests take their
# environment as arguments (that is why they do) and cannot see this at all.
#
# Ask the resolver, don't re-derive it — the same rule, and the same `where`, as the enforcer
# below; `$wow_data` was resolved once, ahead of the test gate. No install found: run it bare,
# and let the client-data tests say so themselves (`BENILLA_REQUIRE_DATA` is deliberately NOT
# handed down: rung 2 is compiled out here, and what this rung falsifies is the player ladder,
# not the resolver — a skip here would be the honest one).
run player-tests env ${wow_data:+WOW_DATA="$wow_data"} BENILLA_SKIP_LOG="$skips" \
    cargo test -p benilla-formats -p benilla-app --no-default-features --lib
report_skips player-tests

# **The enforcer** (decision 1160's second binary, made into a gate by 1164). `benilla-worldview`
# boots the engine plugin set with no server, no login, no UI and no player, and any system whose
# parameters need a gameplay resource reports itself. It is the only check for coupling that
# crosses NO SYMBOL — a `Res<player::Player>` read is invisible to the compiler and to the API wall
# test, and shows up only at runtime.
#
# It is here because it rotted: the binary sat broken on main, failing on frame one, for as long as
# nobody happened to run it. A tripwire nobody trips is not a tripwire.
#
# ~15 s, and it opens a small cornered window (`bgwin`'s no-pixel rule) — it reads the error log,
# not the framebuffer. Skipped, loudly, without the 1.12.1 install: the gate cannot demand an asset
# tree the repo is forbidden to contain.
# **Ask the resolver, don't re-derive it** (decision 1175). This used to read
# `[ -n "$WOW_DATA" ] || [ -d WoW/Data ]` — a second, hand-rolled copy of the rule for where the
# install is, which is the exact duplication that record exists to end. A gate that disagrees with
# the client skips on a machine where the client would have run, or runs where it cannot.
if cargo run -q -p benilla-formats --example where >/dev/null 2>&1; then
    run enforcer env WOW_WORLDVIEW_CHECK=10 cargo run -q -p benilla-worldview
else
    echo "gate SKIPPED: enforcer (no WoW install found — the engine boot check needs one;"
    echo "             \`cargo run -p benilla-formats --example where\` says where it looked)"
    note_timing enforcer 0 SKIPPED
fi

# **The enforcer again, with NO install** (decision 1451) — the boot every player who unzips
# benilla into the wrong folder takes, and the one nothing on this machine could take: a dev build
# finds the project folder, so `wow_data()` never returned `None` in any run we make. Nobody had
# to *break* that path; it simply drifted, and by August a `Startup` that inserts a resource only
# when there is client data faced an `Update` that took it as a hard `Res`. Bevy's default handler
# panics on a parameter that cannot validate, so the client died on frame one with a system name it
# could not even print. `WOW_DATA=` (set, empty) is `benilla_formats::install`'s spelling of "there
# is no install", and the check's error handler collects every such fault instead of dying on the
# first — so this run names them all, the day they land.
#
# No `if`: this gate is the one that needs no install. ~5 s (nothing to stream — every system is
# offered to the executor, which is what validates its parameters, within the first frames), and
# the same small cornered window as above. It covers the engine crate, which is where the
# world-shaped resources live; the client's own no-install boot is still eyes-on.
run enforcer-no-install env WOW_DATA= WOW_WORLDVIEW_CHECK=5 cargo run -q -p benilla-worldview

echo "ALL GATES GREEN ($((SECONDS - chain_t0)) s; per-gate seconds in $timing)"
note_timing chain "$((SECONDS - chain_t0))" ok

# Stamp the verdict — but only if the tree is still the one the chain actually ran on: parallel
# agents share this working tree (docs/METHOD.md, "Who types what"), and a stamp for a half-edited tree
# would memoize a green nothing verified. A changed key just means no stamp; nothing fails.
key_end="$(tree_key || true)"
if [ -n "$key_end" ] && [ "$key_end" = "$key_start" ] && [ -d target ]; then
    printf '%s|%s\n' "$key_end" "$wow_data" > "$stamp" 2>/dev/null || true
fi

# **The fourth gate has a runner too, and it is not this script** (2277). docs/METHOD.md asks for
# `fmt` · `clippy` · `test` · *a clean run*, and for a long time only the first three were a
# command — so "a clean run" was a paragraph in docs/METHOD.md, which a session can simply not have
# read. One did, decided it could not reach the server at all, and said so to the director while
# shipping a change whose entire subject was a session boundary.
#
# Not a rung: it wants the local vmangos, opens a window, and costs ~45 s, so paying it per commit
# is the director's call. A pointer costs nothing and makes the verb impossible to not know about.
echo "  (a clean run is the fourth gate: scripts/smoke.sh — live logout/re-login round trip)"
