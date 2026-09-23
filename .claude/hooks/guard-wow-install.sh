#!/usr/bin/env bash
# PreToolUse(Edit|Write|NotebookEdit) guard: refuse to write anything into the 1.12.1 install.
# Exit 2 blocks the call and hands stderr back to the agent.
#
# benilla reads a WoW install and never writes to one (docs/METHOD.md). The client persists
# everything through `crate::local_state` into `benilla-config/` beside the binary, and
# `scripts/smoke.sh` fails a run that leaves the install changed; this hook is the same rule for
# the tools. The install is the player's own copy of somebody else's game, and "what here is
# benilla's?" is only answerable while nothing of ours is anywhere else.
#
# Scoped by physical path, not by name: a checkout's `WoW` may be a symlink, so a write can arrive
# spelled either way and only `cd -P` collapses the two. `$WOW_DATA` is honoured because that is
# the resolver's own first step.
#
# `BENILLA_ALLOW_INSTALL_WRITE=1` overrides: a deliberate exception names itself in the transcript.
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
BLOCKED: that write targets the 1.12.1 INSTALL ($root).

    the file: $path

benilla reads a WoW install and never writes to one (docs/METHOD.md). It is the player's own copy
of somebody else's game, and the repo may never contain any of it.

Where the thing you are writing actually goes:

  · anything the client persists   → \`crate::local_state\` (\`benilla-config/\`)
  · a capture, log, or scratch file → the session scratchpad
  · a doc                          → the repo

A deliberate exception is \`BENILLA_ALLOW_INSTALL_WRITE=1\`.
EOF
      exit 2
      ;;
  esac
done
exit 0
