#!/usr/bin/env bash
# Stop hook (docs/METHOD.md): tidy pass at the end of every turn — stamp this session's slot
# claim so the pool can tell a live session from a dead one, and format Rust so the fmt gate is
# green by the time anything commits. Best-effort: never block the agent from stopping (always
# exit 0). Operates on the SESSION'S checkout (worktree-aware), guarded via the shared git common-dir
# so it never formats a sibling repo (all benilla worktrees share one common-dir; wow-5875-re doesn't).
#
# It also **never formats the primary checkout**, and says so. Formatting is a write, and the primary
# belongs to no session (docs/METHOD.md "Parallel sessions"); a session sitting there is the mistake
# `guard-primary-checkout.sh` blocks at edit time, and this is the one place that notices the same
# mistake arriving by any OTHER route — a `sed -i`, a `perl -pi`, a script — since it runs every
# turn regardless of which tools were used. Warn, never block: a Stop hook that refuses to stop is a
# trap, and this one is best-effort by design.
own=$(git -C "$(dirname "$0")" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)

# ── The claim HEARTBEAT ──────────────────────────────────────────────────────────────────────────
# Stamp this session's claim marker once a turn, so "is anyone still working in that slot?" is a
# fact we hold rather than a guess. Before this, the only activity signal a dead claim could be
# judged on was its TIP COMMIT age, and `wt.sh` says why that is a poor one: a live session looks
# stale the moment it lands, so the reap window had to be 24 h on BOTH clocks to be safe. The cost
# of that safety was the director's own complaint — close a session after its work lands and the
# slot sits unusable for a day, with nothing "obviously free" for the next session to take.
#
# Deliberately BEFORE the cwd checks below, and found by SESSION ID rather than by `$PWD`: a turn
# that happened to end in the scratchpad, or in the primary, is still a turn this session was alive
# for. Best-effort like everything else here — a failed touch must never keep a session from
# stopping.
mine="${CLAUDE_CODE_SESSION_ID:-}"
if [ -n "$own" ] && [ -n "$mine" ]; then
  primary_dir=$(cd "$own/.." 2>/dev/null && pwd)
  if [ -n "$primary_dir" ]; then
    # BOTH pool roots (decision 1726): slots are allocated on the external drive and the old
    # internal root is draining. This walked only the internal one, so from the day 1726 landed the
    # heartbeat stopped stamping for every session in the new pool — and the heartbeat is the whole
    # reason `wt.sh` can tell a live claim from a dead one. A claim that never restamps looks idle
    # to `reap_idle_claims`, which is how a full `claim` takes over a slot somebody is working in.
    for pool in "${WT_POOL_ROOT:-/Volumes/SanDisk/benilla-wt}" "$(dirname "$primary_dir")/benilla-wt"; do
      for marker in "$pool"/pool-*/.wt-claimed; do
        [ -e "$marker" ] || continue
        [ "$(cut -f3 "$marker" 2>/dev/null)" = "$mine" ] || continue
        touch "$marker" 2>/dev/null || true
        break 2
      done
    done
  fi
fi

root=$(git rev-parse --path-format=absolute --show-toplevel 2>/dev/null)
cur=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
[ -n "$root" ] && [ -n "$own" ] && [ "$cur" = "$own" ] || exit 0
primary=$(cd "$own/.." 2>/dev/null && pwd)
# No pool on this machine — a plain clone — means the checkout IS the working tree, and formatting
# it is the whole job. The pool is what makes the primary off-limits (scripts/wt.sh).
pool_here() { [ -d "${WT_POOL_ROOT:-/Volumes/SanDisk/benilla-wt}" ] || [ -d "$(dirname "$1")/benilla-wt" ]; }
if [ -n "$primary" ] && [ "$root" = "$primary" ] && [ -z "${WT_ALLOW_PRIMARY:-}" ] && pool_here "$primary"; then
  echo "on-stop: this session is sitting in the PRIMARY checkout ($primary) — it belongs to no" >&2
  echo "on-stop: session and must stay clean. Not formatting it. Run './scripts/wt.sh claim <name>'" >&2
  echo "on-stop: and work in the printed slot; move anything already changed here into that slot." >&2
  exit 0
fi
cd "$root" || exit 0
cargo fmt --all >/dev/null 2>&1 || true
exit 0
