#!/usr/bin/env bash
# PreToolUse(Bash) guard: refuse to keep working when the PRIMARY checkout is dirty and this
# session holds no pool slot. Exit 2 blocks the call and hands stderr back to the agent.
#
# **This is `guard-primary-checkout.sh`'s hole, closed.** That hook is wired to `Edit|Write|
# NotebookEdit`, and it reasons — in a comment that was true when it was written — that "the three
# things that genuinely write to the primary are scripts, not tool calls ... and they run as Bash,
# which this never sees." A session editing through Bash (`sed -i`, a `python3 - <<PY` heredoc,
# `tee`) is not covered by a single line of it. On 2026-08-25 a session did exactly that: 434 lines
# across ten files, committed to main from the primary, while three neighbours were landing into the
# same checkout. Nothing fired. docs/METHOD.md said, and had said for months, that such a write is
# hook-blocked. It was not.
#
# **Why dirtiness and not the command.** The obvious fix is to scan the command for write verbs and
# resolve their arguments. That is shell parsing, and it is a losing game: the write that started
# this was a heredoc whose target path was computed inside Python, which no pattern can see. So this
# asks a question with no parsing in it at all — *is the primary dirty, and is this session the one
# with nowhere else to be?* It cannot be evaded, because it does not look at what you typed.
#
# The cost is that it fires on the command AFTER the first stray write rather than on the write
# itself. Measured against the incident it exists for, that is one command, not 434 lines: the
# session ran ~40 Bash calls over that work.
#
# **A dirty primary is never normal** (docs/METHOD.md hard rules; docs/METHOD.md "Parallel sessions"), which
# is what makes this a cheap question to ask. The primary belongs to no session and stays clean; a
# session that has claimed a slot is not answerable for dirt it did not make, so it passes.
#
# `WT_ALLOW_PRIMARY=1` overrides — in the hook's environment, or as an inline prefix on the command
# itself, because for a Bash tool call that is where a deliberate exception is actually typed.
input=$(cat)

cmd=$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)
[ -n "$cmd" ] || exit 0

# The deliberate exception, named in the transcript either way.
[ -n "${WT_ALLOW_PRIMARY:-}" ] && exit 0
case "$cmd" in *WT_ALLOW_PRIMARY=1*) exit 0 ;; esac

# The remedy must never be blocked by the thing it remedies: `wt.sh claim` is the one command this
# message asks for, and `status`/`land`/`release` are how a session gets itself unstuck.
case "$cmd" in *wt.sh*) exit 0 ;; esac

# Scoped by the shared git common-dir, like the sibling hooks: another repo (wow-5875-re) has its
# own, so a dispatched RE agent is never touched by benilla's rule.
own=$(git -C "$(dirname "$0")" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
[ -n "$own" ] || exit 0
primary=$(cd "$own/.." 2>/dev/null && pwd) || exit 0
# No pool on this machine — a plain clone — means this checkout IS the working tree: nothing to
# guard. The pool is what makes the primary off-limits (scripts/wt.sh); without one, the rule has
# nothing to protect.
pool_here() { [ -d "${WT_POOL_ROOT:-/Volumes/SanDisk/benilla-wt}" ] || [ -d "$(dirname "$1")/benilla-wt" ]; }
pool_here "$primary" || exit 0

# Clean primary -> nothing to protect, and this is the overwhelmingly common case: one `git status`
# against a warm index, ~10 ms, on a tree whose only large directory (target/) is gitignored.
dirt=$(git -C "$primary" status --porcelain 2>/dev/null | head -6)
[ -n "$dirt" ] || exit 0

mine="${CLAUDE_CODE_SESSION_ID:-}"
[ -n "$mine" ] || exit 0 # a human shell or CI — not ours to police

# Does this session already hold a slot? Then it has somewhere to be, and the dirt in the primary
# is somebody else's problem to answer for — blocking here would be a gate firing on innocent work,
# which is how a gate teaches "the gate is broken, work around it".
#
# **BOTH pool roots, because there are two** (decision 1726): slots are allocated on the external
# drive, and the old internal root beside the primary is draining but still live. This hook asked
# only the internal one — so from the day 1726 landed, a session holding an external slot answered
# "no slot" and was blocked on its NEXT Bash call after any neighbour dirtied the primary. That is
# a gate firing on exactly the innocent work the paragraph above says it must not, and it takes out
# every session at once rather than the one that made the mess. `WT_POOL_ROOT` is honoured for the
# same reason `wt.sh` honours it.
for pool in "${WT_POOL_ROOT:-/Volumes/SanDisk/benilla-wt}" "$(dirname "$primary")/benilla-wt"; do
  for marker in "$pool"/pool-*/.wt-claimed; do
    [ -e "$marker" ] || continue
    [ "$(cut -f3 "$marker" 2>/dev/null)" = "$mine" ] && exit 0
  done
done

cat >&2 <<EOF
BLOCKED — the PRIMARY checkout is dirty and this session has not claimed a slot.

    primary: $primary
$(printf '%s\n' "$dirt" | sed 's/^/      /')

The primary belongs to no session and stays clean (docs/METHOD.md hard rules; docs/METHOD.md "Parallel
sessions — one worktree each"). Working here reverts other sessions' edits, jams the
whole-workspace gates with a neighbour's in-flight code, and blocks their \`wt.sh land\`.

If those changes are YOURS — you have been editing the primary, most likely through Bash, which
\`guard-primary-checkout.sh\` cannot see:

    ./scripts/wt.sh claim <short-task-name>     # prints a slot path
    git -C $primary diff > /tmp/carry.patch     # needs WT_ALLOW_PRIMARY=1 on this call
    # …then apply it in the slot and reset the primary.

If they are NOT yours, do not clean them up — they are another session's live work. Claim a slot
and work there; say so to the director if the primary stays dirty.

If the pool is genuinely full, \`wt.sh claim\` says so and the answer is to STOP and ask the
director which slot to take over — never to carry on here.

A deliberate exception is \`WT_ALLOW_PRIMARY=1 <command>\`.
EOF
exit 2
