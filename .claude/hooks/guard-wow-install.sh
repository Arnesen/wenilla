#!/usr/bin/env bash
# PreToolUse(Edit|Write|NotebookEdit) guard: refuse to write anything into the 1.12.1 install.
#
# Reads the hook JSON on stdin; exit 2 blocks the call, with stderr as the reason. The protected
# roots are WOW_DATA and the `WoW` link of the primary checkout and of the cwd.
# BENILLA_ALLOW_INSTALL_WRITE=1 lets a write through.
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

# Resolve the target through its nearest existing ancestor: a Write may name new directories too.
dir=$(dirname "$path")
while [ ! -d "$dir" ] && [ "$dir" != "/" ] && [ "$dir" != "." ]; do dir=$(dirname "$dir"); done
[ -d "$dir" ] || exit 0
real=$(cd -P "$dir" 2>/dev/null && pwd) || exit 0

# Compare physical paths (`cd -P`): `WoW` may be a symlink, and a write can name either spelling.
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
