#!/usr/bin/env bash
# check.sh — the ROUND's verify (decision 1822): fmt everywhere (cheap), clippy + test scoped to
# the crates this round's changes can actually affect — the changed crates plus everything that
# depends on them. The workspace-wide chain is `scripts/gates.sh`'s job, paid at sync→land.
#
# Why this verb exists: the fleet was paying the FULL chain per round — measured over the fortnight
# to 2026-09-01 at 3,028 `cargo test --workspace` runs (72 h of wall time) and 1,572 workspace-wide
# clippys (33 h) against only 393 actual gate moments, one session hitting the whole suite 161
# times in a day while editing a single leaf crate. 59 % of edits land in `benilla-app`, which
# nothing but the `benilla` bin depends on: the honest scope of a round is 1–3 crates, not 19.
#
# Scope rule — conservative by construction, unknown means FULL, never "probably fine":
#   crates/<dir>/**            → that crate (owner read off cargo metadata, not the dir name)
#   assets/ui/**               → benilla-app (ui_script's shipped-UI tests read it at runtime)
#   *.md                       → no gate can change (same rule as wt.sh docs_only)
#   anything else              → escalate: exec scripts/gates.sh (workspace manifests, .cargo/,
#                                rust-toolchain, scripts/, shaders outside crates, …)
# Change set = fork point vs main + staged + unstaged + untracked — the round's work, not the last
# edit. CHECK_FULL=1 skips straight to gates.sh.
#
# Same fail-fast discipline as gates.sh: never compose `cargo … | tail` pipelines by hand — the
# pipe's exit code masks the gate's (that bug bit twice before gates.sh existed).
set -uo pipefail

# Gate the tree you are STANDING IN (gates.sh learned this; same rule here).
here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$root" ] && [ -f "$root/scripts/check.sh" ] || root="$here"
cd "$root" || exit 1

if [ "${CHECK_FULL:-}" = "1" ]; then
    exec "$root/scripts/gates.sh"
fi

# ── The round's change set ───────────────────────────────────────────────────────────────────────
base="$(git merge-base HEAD main 2>/dev/null || git rev-parse HEAD)"
changed="$( { git diff --name-only "$base" HEAD 2>/dev/null
              git diff --name-only HEAD 2>/dev/null
              git ls-files --others --exclude-standard 2>/dev/null; } | sort -u )"

if [ -z "$changed" ]; then
    echo "check: no changes against main and a clean tree — nothing to verify"
    exit 0
fi

# ── Map files → crate dirs; anything unmappable escalates to the full chain ──────────────────────
dirs=""
full_reason=""
while IFS= read -r f; do
    case "$f" in
    docs/* | *.md) ;;                                 # provably gate-inert (wt.sh docs_only)
    assets/ui/*) dirs="$dirs benilla-app-DIR:crates/benilla-app" ;;
    crates/*/*) d="${f#crates/}"; dirs="$dirs DIR:crates/${d%%/*}" ;;
    *) full_reason="$f" ;;
    esac
done <<EOF
$changed
EOF

if [ -n "$full_reason" ]; then
    echo "check: '$full_reason' is outside the crate map — the whole workspace could be affected."
    echo "check: escalating to the full chain: scripts/gates.sh"
    exec "$root/scripts/gates.sh"
fi

dirlist="$(printf '%s\n' $dirs | sed 's/.*DIR://' | sort -u)"
if [ -z "$dirlist" ]; then
    echo "check: docs-only round ($(printf '%s\n' "$changed" | wc -l | tr -d ' ') file(s)) — no gate can change; fmt only"
    cargo fmt --all -- --check || exit 1
    echo "check: green (docs only)"
    exit 0
fi

# ── Changed crates → reverse-dependency closure, off cargo metadata (dir names are not package
#    names here: crates/mpq is benilla-mpq) ──────────────────────────────────────────────────────
pkgs="$(python3 - "$root" $dirlist <<'PY'
import json, subprocess, sys, os
root, dirs = sys.argv[1], set(sys.argv[2:])
meta = json.loads(subprocess.run(
    ["cargo", "metadata", "--format-version", "1", "--no-deps"],
    capture_output=True, text=True, cwd=root, check=True).stdout)
by_dir, deps = {}, {}
names = {p["name"] for p in meta["packages"]}
for p in meta["packages"]:
    by_dir[os.path.relpath(os.path.dirname(p["manifest_path"]), root)] = p["name"]
    deps[p["name"]] = {d["name"] for d in p["dependencies"] if d["name"] in names}
changed = {by_dir[d] for d in dirs if d in by_dir}
missing = [d for d in dirs if d not in by_dir]
if missing:  # a changed dir cargo doesn't know: not our map's to scope
    print("FULL " + " ".join(missing)); sys.exit(0)
scope = set(changed)
grew = True
while grew:
    grew = False
    for name, ds in deps.items():
        if name not in scope and ds & scope:
            scope.add(name); grew = True
print(" ".join(sorted(scope)))
PY
)" || { echo "check: cargo metadata failed — falling back to the full chain"; exec "$root/scripts/gates.sh"; }

case "$pkgs" in FULL*)
    echo "check: changed path(s) not in the workspace map (${pkgs#FULL }) — full chain"
    exec "$root/scripts/gates.sh" ;;
esac

pflags=""
for p in $pkgs; do pflags="$pflags -p $p"; done
echo "check: scope = $pkgs"
echo "check:   (changed crates + everything that depends on them; full chain still runs at sync→land)"

log="$(mktemp -t benilla-check)"
trap 'rm -f "$log"' EXIT
run() {
    local name="$1"; shift
    if ! "$@" >"$log" 2>&1; then
        echo "CHECK FAILED: $name"
        tail -30 "$log"
        exit 1
    fi
    echo "check ok: $name"
}

run fmt cargo fmt --all -- --check
run clippy cargo clippy $pflags --all-targets -- -D warnings
run test cargo test $pflags

echo "CHECK GREEN (scoped: $pkgs)"
echo "  (land still pays the full chain once: scripts/gates.sh — memoized, so an already-gated tree is free)"
