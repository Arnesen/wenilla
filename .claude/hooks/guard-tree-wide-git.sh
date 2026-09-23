#!/usr/bin/env bash
# PreToolUse(Bash) guard (docs/METHOD.md "Who types what": *Parallel agents share one working tree —
# whole-tree git ops are forbidden*): refuse the handful of git verbs that sweep a tree — or the
# SHARED STASH STACK — rather than a path.
#
# Why a hook, on 2026-08-14: a session reaching for a one-command "revert my file, run the test,
# put it back" repro typed `git stash push -- <path> -q`. The trailing `-q` parsed as a pathspec,
# the push failed — and the follow-up `git stash pop` then popped a DIFFERENT session's entry
# (`sess/death-arc`'s) into this worktree. It conflicted on docs/MAP.md and aborted, so nothing was
# lost; the near-miss is the point. **Stashes are per-REPOSITORY, not per-worktree** — every pool
# slot shares one stack — so `stash pop` is a whole-tree op wearing a local-looking name, and the
# blast radius is another session's uncommitted hours. The earlier incident docs/METHOD.md records (a
# six-agent fan-out losing hours to one agent's diagnostic `git stash`) is the same mechanism.
#
# The rule the prose already had, made structural — 0976's argument for the primary-checkout guard,
# one verb over: at the command, the fix is one different command and nothing has happened; after
# it, somebody unpicks a stash stack by hand and hopes the conflict preserved their work.
#
# **The block list is deliberately SHORT.** A gate that fires on legitimate work teaches "the gate
# is broken, work around it" (the 2026-07-20 incident), so
# this blocks only forms with no path-scoped reading:
#
#   git stash [push|save|pop|apply|drop|clear|branch|create|store]   — the shared stack
#   git reset --hard                                                 — sweeps the tree
#   git clean -f…                                                    — deletes untracked files
#   git checkout . | git checkout -- . | git restore . | …           — the sweep-everything forms
#
# Everything else passes, including the ones this repo's own work depends on: `git stash list`/
# `show` (read-only), `git checkout HEAD -- <path>` (the correct way to do the repro above),
# `git checkout --detach`/`-B <branch>` (what `wt.sh` itself runs), `git restore --source=X -- path`,
# and a soft/mixed `git reset` (which moves HEAD but leaves the working tree alone).
#
# Scoped by the shared git common-dir like every other hook here: a sibling repo (wow-5875-re) has
# its own rules and falls through untouched. Note the hook only ever sees the agent's own command
# text, so git run INSIDE our scripts (`wt.sh`) is never its business.
#
# `WT_ALLOW_TREE_OP=1` overrides, the `WOW_ALLOW_ACCOUNT`/`WT_ALLOW_PRIMARY` shape (0677, 0976):
# structurally hard rather than merely forbidden, and a deliberate exception names itself.
input=$(cat)
cmd=$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)
[ -n "$cmd" ] || exit 0
[ -n "${WT_ALLOW_TREE_OP:-}" ] && exit 0 # deliberate exception, named in the transcript

# Per SEGMENT of a (possibly compound) command, so a `git stash` riding behind a `&&` is judged
# on its own rather than by the line's first verb.
verdict=""
while IFS= read -r seg; do
  # Strip a leading `cd … &&`-style residue and normalise whitespace for matching.
  s=$(printf '%s' "$seg" | sed 's/^[[:space:]]*//; s/[[:space:]]\{1,\}/ /g')
  # Only `git …` invocations, with or without a leading `-C <dir>`.
  case "$s" in
    git\ *) ;;
    *) continue ;;
  esac
  # Drop `-C <dir>` and other pre-verb globals so the verb is the next token.
  v=$(printf '%s' "$s" | sed -E 's/^git( +-[A-Za-z-]+( +[^ ]+)?)* +//')
  case "$v" in
    # ── the shared stash stack (read-only subcommands excepted) ──────────────────────────────
    stash\ list* | stash\ show*) ;;
    stash | stash\ *)
      verdict="git stash — the stash stack is shared by EVERY worktree of this repo, so this can
capture, or restore, another live session's uncommitted work. It has already come within one
merge conflict of doing exactly that (2026-08-14).

For the common case — 'revert one file, run something, put it back' — copy it aside and use the
path-scoped restore instead:

    cp <file> \"\$SCRATCH/<file>.mine\"
    git checkout HEAD -- <file>      # path-scoped: touches nothing else, no shared state
    <run the thing>
    cp \"\$SCRATCH/<file>.mine\" <file>

To park work properly, commit it on your session branch — that is what the branch is for."
      break
      ;;
    # ── whole-tree resets / sweeps ───────────────────────────────────────────────────────────
    reset*--hard*)
      verdict="git reset --hard — discards every uncommitted change in this worktree, including
work belonging to a dispatched agent running in this session's slot right now.

Undo one path with \`git checkout HEAD -- <path>\`; move HEAD without touching files with
\`git reset --soft\`."
      break
      ;;
    clean\ *-*f* | clean\ -*f*)
      verdict="git clean -f — deletes untracked files outright, and untracked is exactly what a
neighbouring agent's not-yet-added new file looks like.

Delete the specific files you mean by name, or list first with \`git clean -n\`."
      break
      ;;
    checkout\ . | checkout\ --\ . | checkout\ -- | restore\ . | restore\ --\ . | checkout\ -f | checkout\ -f\ *)
      verdict="a pathless/sweep-everything checkout — it reverts the WHOLE tree, not the file you
are looking at.

Name the path: \`git checkout HEAD -- <path>\` (or \`git restore -- <path>\`)."
      break
      ;;
  esac
done <<EOF
$(printf '%s' "$cmd" | tr ';&|\n' '\n\n\n\n')
EOF

[ -n "$verdict" ] || exit 0

# Same-REPO check LAST — it costs a git call, and the overwhelming majority of commands never
# reach it. The cwd resolution: a tree-wide op is judged
# by the tree it lands in, and a `cd <slot> && …` is how every session writes one.
cwd=$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)
cwd="${cwd:-$PWD}"
cd_dir=$(printf '%s' "$cmd" | tr ';&|' '\n' \
  | sed -n 's/^[[:space:]]*cd[[:space:]]\{1,\}\([^[:space:]]\{1,\}\)[[:space:]]*$/\1/p' | tail -n 1)
if [ -n "$cd_dir" ]; then
  case "$cd_dir" in
    /*) cwd="$cd_dir" ;;
    '~'*) cwd="${HOME}${cd_dir#\~}" ;;
    *) cwd="$cwd/$cd_dir" ;;
  esac
fi
[ -d "$cwd" ] || cwd=$PWD
own=$(git -C "$(dirname "$0")" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
there=$(git -C "$cwd" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
[ -n "$own" ] && [ "$there" = "$own" ] || exit 0 # another repo's rules are its own
primary=$(cd "$own/.." 2>/dev/null && pwd) || exit 0
# No pool on this machine — a plain clone — means this checkout IS the working tree, shared with
# nobody: nothing to guard. The pool is what makes the tree shared (scripts/wt.sh).
pool_here() { [ -d "${WT_POOL_ROOT:-/Volumes/SanDisk/benilla-wt}" ] || [ -d "$(dirname "$1")/benilla-wt" ]; }
pool_here "$primary" || exit 0

cat >&2 <<EOF
BLOCKED — tree-wide git op in a SHARED working tree.

$verdict

(docs/METHOD.md, "Who types what": parallel agents share one working tree, so whole-tree git ops are
forbidden — diagnostics use path-scoped commands only. A genuinely deliberate exception is
WT_ALLOW_TREE_OP=1; if you need a clean tree of your own, take a worktree instead.)
EOF
exit 2
