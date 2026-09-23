#!/usr/bin/env bash
# PreToolUse(EnterWorktree | Agent) guard (docs/METHOD.md):
# refuse the HARNESS worktree tool. `scripts/wt.sh` is this repo's only worktree mechanism.
#
# Two doors, one room. `EnterWorktree` is the named tool; `Agent` with `isolation: "worktree"`
# is the same worktree by another door — the subagent is spawned into a fresh
# `.claude/worktrees/agent-<id>` checkout that no pool verb knows about (decision 2287: one was
# found under the primary on 2026-09-16, created by a session whose method text still said to
# use it). For `Agent` the block is conditional on that one field; every other Agent call
# passes untouched.
#
# Why a guard for a tool that sounds like exactly the right thing: it IS the natural reach. The
# session reads "every session works in its own git worktree", claims a slot, and then meets a
# built-in tool named EnterWorktree — the name matches the rule word for word. The pull is
# strongest right after a correct `wt.sh claim`, because the Bash tool resets cwd to the primary
# between calls, so the printed slot path looks unusable and this looks like how you "enter" it.
#
# Both of its forms are wrong here, for different reasons:
#   `name` -> creates a worktree under `.claude/worktrees/`. That is precisely what the pool
#             replaced: ownerless agent worktrees, no sweeper, each paying a cold ~4.5-min dep
#             build, ~60 GB of them at the 2026-07-17 disk incident (decisions 0192, 0433 —
#             sweep step 2 still exists to reap the strays).
#   `path` -> points at a pool slot with a second tracker that has its own lifecycle opinions,
#             including removal prompts. A pool slot must never be `git worktree remove`d; its
#             warm target/ is the entire point (0192), and 0520 is the incident from a session
#             holding the wrong idea of which slot it was in.
# So the block is absolute and needs no allowlist — same shape as guard-primary-checkout.sh.
# There is no legitimate call here: the pool's verbs are Bash, which this never sees.
#
# Unscoped by design, unlike the primary-checkout guard. That one allows sibling repos because a
# dispatched RE agent legitimately edits in wow-5875-re; this one has no such case — wow-re runs
# its own isolated-worktree rule under its own law (docs/METHOD.md, "The cross-repo RE workflow"), and
# an agent reaching for the harness tool there is making the same mistake in someone else's tree.
input=$(cat)

# A deliberate exception names itself, the WT_ALLOW_PRIMARY / WOW_ALLOW_ACCOUNT shape (0677).
[ -n "${WT_ALLOW_HARNESS_WORKTREE:-}" ] && exit 0

# No pool on this machine — a plain clone — means there is no pool to prefer: the harness tool is
# then an ordinary worktree, and nothing here has an opinion about it (scripts/wt.sh).
own=$(git -C "$(dirname "$0")" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
primary=$(cd "${own:-/nonexistent}/.." 2>/dev/null && pwd) || exit 0
pool_here() { [ -d "${WT_POOL_ROOT:-/Volumes/SanDisk/benilla-wt}" ] || [ -d "$(dirname "$1")/benilla-wt" ]; }
pool_here "$primary" || exit 0

tool=$(printf '%s' "$input" | jq -r '.tool_name // empty' 2>/dev/null)
if [ "$tool" = "Agent" ]; then
  isolation=$(printf '%s' "$input" | jq -r '.tool_input.isolation // empty' 2>/dev/null)
  [ "$isolation" = "worktree" ] || exit 0
  door="Agent isolation: worktree"
else
  door="EnterWorktree"
fi

target=$(printf '%s' "$input" | jq -r '.tool_input.path // .tool_input.name // empty' 2>/dev/null)

cat >&2 <<EOF
BLOCKED — $door is not how this repo does worktrees${target:+ (asked for: $target)}.

\`scripts/wt.sh\` is the only worktree mechanism here (docs/METHOD.md, "Parallel sessions — one
worktree each"). The harness tool builds worktrees under \`.claude/worktrees/\` — ownerless, with
no sweeper and a cold ~4.5-min dependency build each; ~60 GB of those strays were the 2026-07-17
disk incident (decisions 0192/0433). Pointing it at a pool slot instead is no better: a slot must
never be removed, its warm target/ is the whole point.

If you have NOT claimed a slot yet:

    ./scripts/wt.sh claim <short-task-name>     # prints the slot path on stdout

If you HAVE claimed one, you do not need this tool — you need the path. The Bash tool resets cwd
to the primary between calls; that is expected, not a problem to solve. Two habits cover it
(a dispatched agent inherits the session's slot and works there the same way — it never needs a
tree of its own; a diagnostic that genuinely needs a clean tree is the parent's to run, on a
synced slot):

    Bash        cd /path/to/benilla-wt/pool-N && cargo test ...    # every call, prefix it
    Read/Edit   /path/to/benilla-wt/pool-N/crates/...              # absolute, under the slot

The gates, the hooks and the runtime asset reads all follow the slot on their own.
EOF
exit 2
