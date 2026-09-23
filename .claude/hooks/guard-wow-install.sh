#!/usr/bin/env bash
# PreToolUse(Edit|Write|NotebookEdit) guard (docs/METHOD.md hard rules; decision 1486): refuse to write
# anything into the 1.12.1 install. Exit 2 blocks the call and hands stderr back to the agent.
#
# The rule is the director's, stated 2026-08-21 while B261 was being built: *benilla never changes
# or adds anything in the WoW folder*. It has two halves and this file is the second one.
#
#   1 · the CLIENT never writes there — every persisted file resolves through
#       `crate::local_state` into `benilla-config/` beside the binary (0954/1175), and
#       `scripts/smoke.sh` now measures that a full run leaves the install byte-for-byte as it
#       found it;
#   2 · WE never write there either — which is this hook, because we already had. The install
#       currently carries `apitrace-stderr.log`, `apitrace-stderr-ring.log` and
#       `abbey-standing-frame1501.png`: three files a session dropped in the nearest folder to
#       hand. Nobody decided to; that is exactly why prose was never going to be enough (0976's
#       argument, one folder over).
#
# WHY IT MATTERS MORE HERE THAN IT LOOKS. The install is not ours to write to at all — it is the
# player's own copy of somebody else's game, and the one thing the repo may never contain
# (docs/METHOD.md). On THIS machine it is also a symlink into the sibling RE repo (`wow-5875-re/WoW`),
# so a stray write lands in another repository's working tree and shows up in their `git status`.
# And it makes "what here is benilla's?" unanswerable, which is the whole reason 0954 put our state
# in one visible folder.
#
# Scoped by PHYSICAL path, not by name: the slot's `WoW` is a symlink, so a write can arrive
# spelled either way and only `cd -P` collapses the two. `$WOW_DATA` is honoured because that is
# the resolver's own first step — a machine that points the client elsewhere protects that instead.
#
# `BENILLA_ALLOW_INSTALL_WRITE=1` overrides, the `WT_ALLOW_PRIMARY` shape: structurally hard rather
# than merely forbidden, and a deliberate exception names itself in the transcript.
input=$(cat)

path=$(printf '%s' "$input" | jq -r '.tool_input.file_path // .tool_input.notebook_path // empty' 2>/dev/null)
[ -n "$path" ] || exit 0

cwd=$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)
cwd="${cwd:-$PWD}"
case "$path" in
  /*) ;;
  '~'/*) path="${HOME}${path#\~}" ;;
  *) path="$cwd/$path" ;;
esac

[ -n "${BENILLA_ALLOW_INSTALL_WRITE:-}" ] && exit 0

# Resolve the target against its nearest EXISTING ancestor — a Write names a file, and may name
# directories, that do not exist yet.
dir=$(dirname "$path")
while [ ! -d "$dir" ] && [ "$dir" != "/" ] && [ "$dir" != "." ]; do dir=$(dirname "$dir"); done
[ -d "$dir" ] || exit 0
real=$(cd -P "$dir" 2>/dev/null && pwd) || exit 0

# The roots to protect. `$WOW_DATA` first (the resolver's own order), then the `WoW` link every
# checkout carries. Both physicalised, so the symlink and its target are one root.
own=$(git -C "$(dirname "$0")" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)
primary=$(cd "${own:-.}/.." 2>/dev/null && pwd)
for root in "${WOW_DATA:-}" "$primary/WoW" "$cwd/WoW"; do
  [ -n "$root" ] && [ -d "$root" ] || continue
  root=$(cd -P "$root" 2>/dev/null && pwd) || continue
  case "$real/" in
    "$root"/*)
      cat >&2 <<EOF
BLOCKED — that write targets the 1.12.1 INSTALL ($root).

    the file: $path

benilla READS a WoW install and never writes to one (docs/METHOD.md hard rules; decision 1486). It is
the player's own copy of somebody else's game, the repo may never contain any of it, and on this
machine that folder is a symlink into the sibling RE repo — a stray file lands in wow-5875-re's
working tree.

Where the thing you are writing actually goes:

  · anything the CLIENT persists  → \`crate::local_state\` (\`benilla-config/\`, 0954/1175)
  · a capture, log, or scratch file → the session scratchpad
  · a decision, a doc              → the repo

A deliberate exception is \`BENILLA_ALLOW_INSTALL_WRITE=1\`.
EOF
      exit 2
      ;;
  esac
done
exit 0
