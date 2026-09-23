#!/usr/bin/env bash
# Phase-5 visual A/B render harness driver (decisions 0008 + 0010).
#
# Captures deterministic benilla screenshots (one process per scenario, server-less + clutter-off +
# pinned camera, game clock AND frame step — see the `capture` module) and diffs them with
# `benilla-visual`. `selfcheck` is the proof that "deterministic" is true. The intended
# loop: capture `baseline` on the current pipeline, make a render change, then `diff` — the diff is the
# machine check that catches a regression before the director's eye (decision 0008).
#
# Captures contain Blizzard-derived imagery, so they live under the gitignored target/visual/ and are
# NEVER committed. macOS/local only: capture opens a real window (no headless GPU path here).
#
# Usage:
#   scripts/visual.sh list                  # print scenario names
#   scripts/visual.sh capture <dir>         # capture every scenario into <dir>/
#   scripts/visual.sh baseline              # capture into target/visual/baseline/
#   scripts/visual.sh diff [--fail <mae>]   # capture into target/visual/candidate/, diff vs baseline/
#   scripts/visual.sh selfcheck             # capture the sweep TWICE from ONE build; demand identical
set -euo pipefail
cd "$(dirname "$0")/.."

VIS=target/visual

build() { cargo build -q -p benilla -p benilla-visual; }

# The scenario list is owned by the binary (capture/mod.rs), printed via WOW_CAPTURE=list — so this driver
# never drifts from the code.
scenarios() { WOW_CAPTURE=list cargo run -q -p benilla; }

capture_into() {
  local dir="$1"
  mkdir -p "$dir"
  local s
  for s in $(scenarios); do
    echo "capture $s -> $dir/$s.png"
    WOW_CAPTURE="$s" WOW_CAPTURE_OUT="$dir/$s.png" cargo run -q -p benilla
  done
}

cmd="${1:-}"
case "$cmd" in
  list)
    build
    scenarios
    ;;
  capture)
    build
    capture_into "${2:?usage: visual.sh capture <dir>}"
    ;;
  baseline)
    build
    capture_into "$VIS/baseline"
    echo "baseline written to $VIS/baseline"
    ;;
  diff)
    shift
    build
    capture_into "$VIS/candidate"
    echo "--- diff candidate vs baseline ---"
    ./target/debug/benilla-visual diff-dir "$VIS/baseline" "$VIS/candidate" --out "$VIS/diff" "$@"
    ;;
  selfcheck) # selfcheck [<mae>] — the tolerance defaults to 0 (bit-identical)
    # The harness's own tripwire (decision 0723). A golden diff is evidence ONLY if the harness is
    # deterministic, so measure that directly: capture the whole sweep twice from one unchanged
    # build and demand a bit-identical result (--fail 0 passes only at MAE exactly 0). Before 0723
    # this failed at MAE 0.009 / max delta 180 — the same band a real render change lands in, which
    # is how 0721's flame pixels ended up unreadable and how 0719 read signal out of churn. Run it
    # after touching anything the capture clock feeds, and whenever a diff looks like noise.
    build
    capture_into "$VIS/self-a"
    capture_into "$VIS/self-b"
    echo "--- selfcheck: self-a vs self-b, one build, two runs ---"
    # The bar is 0 — bit-identical — and the four blessed scenarios meet it (0817 set the bar, 1183
    # cut the sweep to two director-framed spots x two day times). Scenarios that could not hold it
    # left the sweep rather than being carried as slack in the bar (the old 0.001 was cover for two
    # overlook cells at 421 px / max delta 43).
    #
    # What makes a cell flake is a real renderer defect, not harness drift: draw order follows spawn
    # order, and spawn order varies because asset loads complete on a thread pool (0723 "Open, named"
    # -> 0815 Open -> 1182, which measured it). Both states are perfectly STABLE, so no amount of
    # settling or waiting fixes it. **Do not respond to a failure here by widening the bar or the
    # waits** — that mistake has cost several sessions. Anything above 0 is either that defect
    # surfacing in a blessed cell or a genuine harness regression; pass a number to probe.
    #
    # 1182 is also the correction to the obvious first guess: the tie is NOT always a coplanar pair.
    # In the canal cells (deleted by 1183) it was two surfaces INTERSECTING, 0.62 yd apart, tying at
    # one MSAA sample — invisible with WOW_MSAA=off. Run WOW_PICK through the differing pixels and
    # read the PERPENDICULAR gap before assuming which geometry you are chasing.
    ./target/debug/benilla-visual diff-dir "$VIS/self-a" "$VIS/self-b" --out "$VIS/self-diff" \
      --fail "${2:-0}"
    echo "selfcheck OK — the harness clock is a pure function of the build"
    ;;
  *)
    echo "usage: visual.sh {list | capture <dir> | baseline | diff [--fail <mae>] | selfcheck}" >&2
    exit 1
    ;;
esac
