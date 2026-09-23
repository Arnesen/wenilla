#!/usr/bin/env bash
# PreToolUse(Edit|Write|NotebookEdit) guard (docs/METHOD.md):
# refuse to write into a checkout that is not this session's. Exit 2 blocks the call and hands
# stderr back to the agent, which then claims a slot and works there.
#
# TWO trees are not yours, and the file name only says the first (kept, deliberately: renaming it
# would churn `.claude/settings.json`, which every live session reads, for a naming tidy-up):
#
#   1 · the PRIMARY checkout — nobody's, and it stays clean (decision 0976, below);
#   2 · another SESSION's pool slot (decision 1042).
#
# It fires at the FIRST edit because that is where the fix is one `wt.sh claim` and nothing has been
# lost; unguarded, the cost lands on a BYSTANDER hours later, whose `wt.sh land` refuses on a dirty
# primary full of work they can neither see nor move. Why a hook and not more prose: decision 0976.
#
# The block is absolute and needs no allowlist — there is no legitimate agent edit here. The one
# thing that genuinely writes to the primary is a script, not a tool call (`wt.sh land` fast-forwards
# main), and it runs as Bash, which this never sees. `WT_ALLOW_PRIMARY=1` overrides, the `WOW_ALLOW_ACCOUNT` shape
# from 0677: structurally hard rather than merely forbidden, and a deliberate exception names itself.
#
# **"…which this never sees" WAS the hole, and it is why `guard-primary-bash.sh` exists.** That
# sentence assumed only scripts and humans reach the primary through Bash. An AGENT editing with
# `sed -i` or a `python3 - <<PY` heredoc does too, and none of it passes through this file: on
# 2026-08-25 a session wrote 434 lines into the primary and committed them to main without one line
# of this hook running. This one still catches the Edit/Write path at the FIRST write, which is the
# better moment; the Bash guard is the backstop that cannot be evaded by choice of tool.
#
# Scoped by the shared git common-dir, like `on-stop.sh`: a sibling repo
# (wow-5875-re) has its own, so a dispatched RE agent editing THERE is never touched by benilla's
# rule. Paths outside any git repo — the scratchpad, `~/.benilla` — are likewise none of our business.
input=$(cat)

# Edit/Write carry `file_path`; NotebookEdit carries `notebook_path`.
path=$(printf '%s' "$input" | jq -r '.tool_input.file_path // .tool_input.notebook_path // empty' 2>/dev/null)
[ -n "$path" ] || exit 0 # no target we can judge → allow

# Absolutize against the tool's own cwd — a relative path from a session sitting IN the primary is
# exactly the case this exists to catch, and it is the one that arrives without a leading slash.
cwd=$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)
cwd="${cwd:-$PWD}"
case "$path" in
  /*) ;;
  '~'/*) path="${HOME}${path#\~}" ;;
  *) path="$cwd/$path" ;;
esac

# A Write targets a file that does not exist yet, and may name directories that do not either, so
# resolve against the nearest existing ancestor rather than the path itself.
dir=$(dirname "$path")
while [ ! -d "$dir" ] && [ "$dir" != "/" ] && [ "$dir" != "." ]; do dir=$(dirname "$dir"); done
[ -d "$dir" ] || exit 0

# Same-REPO check first (this repo's worktrees all share one common-dir; other repos don't).
own=$(git -C "$(dirname "$0")" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
there=$(git -C "$dir" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
[ -n "$own" ] && [ "$there" = "$own" ] || exit 0

# The primary checkout is the common-dir's parent — the same expression `wt.sh` computes `PRIMARY`
# with, so the two can never disagree about which tree is the shared one. A linked worktree's
# toplevel is its own slot path, and falls through to the slot check below.
primary=$(cd "$own/.." 2>/dev/null && pwd) || exit 0
# No pool on this machine — a plain clone — means this checkout IS the working tree: nothing to
# guard. The pool is what makes the primary off-limits (scripts/wt.sh); without one, the rule has
# nothing to protect.
pool_here() { [ -d "${WT_POOL_ROOT:-/Volumes/SanDisk/benilla-wt}" ] || [ -d "$(dirname "$1")/benilla-wt" ]; }
pool_here "$primary" || exit 0
top=$(git -C "$dir" rev-parse --path-format=absolute --show-toplevel 2>/dev/null)
[ -n "$top" ] || exit 0

if [ "$top" = "$primary" ]; then
  [ -n "${WT_ALLOW_PRIMARY:-}" ] && exit 0 # deliberate exception, named in the transcript

  rel="${path#"$primary"/}"
  cat >&2 <<EOF
BLOCKED — that write targets the PRIMARY checkout ($primary/$rel).

The primary belongs to no session and stays clean (docs/METHOD.md hard rules; docs/METHOD.md "Parallel
sessions — one worktree each"). Editing it silently reverts other sessions' work, jams the
whole-workspace gates with a neighbour's in-flight code, and blocks their \`wt.sh land\`.

Claim a slot and work there instead:

    ./scripts/wt.sh claim <short-task-name>     # prints the slot path; cd there and carry on

If you already have a claimed slot, you are simply in the wrong directory — reissue this edit
against the slot path. A deliberate exception is \`WT_ALLOW_PRIMARY=1\`.
EOF
  exit 2
fi

# ── Another session's slot (decision 1042) ───────────────────────────────────────────────────────
# 1041 stopped a foreign session at `wt.sh land`. This stops it at the first EDIT, which is 0976's
# whole argument one level down: at the verb, the damage is already a split branch somebody has to
# unpick by hand; at the edit, the fix is one `wt.sh claim` and nothing has happened yet. On
# 2026-08-06 the gap between the two was an hour, a red `fmt --check` on files this session had
# never touched, and two decision records that reverted under it mid-write.
#
# **Agents are safe here, and that is a measured fact, not a hope**: a dispatched subagent reports
# the same `CLAUDE_CODE_SESSION_ID` as its parent (checked, 2026-08-06 — same value, same PID). So
# the bulk-edit dispatches docs/METHOD.md's "Who types what" depends on, which work in their session's
# own slot by design, pass this untouched. 1041 named that as the fact it was waiting on.
marker="$top/.wt-claimed"
[ -e "$marker" ] || exit 0                            # free slot: unclaimed, no session to wrong
owner=$(cut -f3 "$marker" 2>/dev/null)
case "$owner" in "" | "-") exit 0 ;; esac             # pre-1041 marker: no session recorded
mine="${CLAUDE_CODE_SESSION_ID:-}"
[ -n "$mine" ] || exit 0                              # nothing to compare (human shell, CI)
[ "$owner" = "$mine" ] && exit 0                      # yours — the overwhelmingly common case
[ -n "${WT_ALLOW_FOREIGN:-}" ] && exit 0              # deliberate exception, named in the transcript

holder=$(cut -f1 "$marker" 2>/dev/null)
cat >&2 <<EOF
BLOCKED — that write targets ANOTHER SESSION's worktree ($top).

    that slot is claim '$holder', held by session $owner
    you are                                   session $mine

Two sessions in one slot revert each other's edits, redden each other's gates, and split each
other's commits at land (decisions 1041, 1042). Claim your own slot:

    ./scripts/wt.sh claim <short-task-name>     # prints the slot path; cd there and carry on

\`wt.sh status\` tags the slot you own \`(yours)\` — if nothing is tagged, you have not claimed one.
A deliberate exception (recovering a dead session's work, say) is \`WT_ALLOW_FOREIGN=1\`.
EOF
exit 2
