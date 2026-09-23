#!/usr/bin/env bash
# cine.sh — play one cinematic on the probe and print what happened, as a timeline.
#
# Every cinematic question is the same shape — "what did the client do to the world while the
# camera was away?" — and answering it by hand costs four environment variables, a GM command, a
# move trace and a fistful of greps. Assembled by hand three times in one session (decisions
# 1701/1707/1708), it produced two WORTHLESS runs before a usable one:
#
#   * the first parked the body wherever the character was saved, which was inside Thunder Bluff's
#     geometry — the body was wedged, sent nothing, and the trace said "held" for reasons that had
#     nothing to do with the cinematic;
#   * the second read "no movement packet" as proof that input was suppressed, when the body was
#     simply in free fall and had no movement to send.
#
# Both are avoidable by construction, so this parks the body on known-flat ground first and always
# prints the control facts (did it fall? did it move?) beside the thing under test.
#
# THE SILENT-BUT-LIVE TRICK, which is the other half of why this exists: `$WOW_NOSOUND` opens no
# audio device, so `zone::start_music_stream` returns before it logs and a silent run can tell you
# NOTHING about music. Instead this points `$BENILLA_HOME` at a throwaway config with
# `MasterVolume = "0"` — the device opens, every stream starts and logs its file, and the room
# stays quiet. That is what made "the Stormwind city-intro stinger is what plays under the human
# narration" a readable fact rather than a guess.
#
#   scripts/cine.sh                 # the human intro (81), parked in Stormwind
#   scripts/cine.sh 41              # the dwarf intro
#   scripts/cine.sh 81 --key W      # …and hold W mid-shot: does the body move?
#   scripts/cine.sh 81 --at "-8913,554,94,0"
#   scripts/cine.sh 81 --keep       # keep the raw log and trace paths, printed at the end
#
# The probe identity is the checkout's `.probe-identity`, or WOW_USER/WOW_PASS/WOW_CHAR
# (scripts/probe-identity.sh).
set -euo pipefail
cd "$(dirname "$0")/.."

id=81
park="-8913,554,94,0"   # Stormwind Trade District: flat, solid, and a zone with music
key=""
keep=0
while [ $# -gt 0 ]; do
    case "$1" in
        --at)   park="$2"; shift 2 ;;
        --key)  key="$2"; shift 2 ;;
        --keep) keep=1; shift ;;
        -*)     echo "cine.sh: unknown flag $1" >&2; exit 2 ;;
        *)      id="$1"; shift ;;
    esac
done

. "$PWD/scripts/probe-identity.sh"
probe_identity cine.sh "$PWD" || exit 2
char="$PROBE_CHAR"

work="$(mktemp -d)"
cfg="$work/cfg"
mkdir -p "$cfg"
printf '[cvars]\nMasterVolume = "0"\n' > "$cfg/config.toml"
log="$work/run.log"
trace="$work/move.trace"

IFS=, read -r px py pz pmap <<< "$park"

# The schedule, in probe-clock seconds. Generous rather than tight: a cold slot streams a city
# slowly, and a measurement taken before the world arrived is the wedged-body run all over again.
park_at=8
play_at=20
key_at=$((play_at + 8))
# Shots run 25-102 s; leave room for the end, the resume and the cover to settle.
exit_at=$((play_at + 120))

echo "cine.sh: cinematic $id as $char, parked at $px $py $pz (map $pmap)"
[ -n "$key" ] && echo "cine.sh: holding $key for 3 s at t=${key_at}s"
echo "cine.sh: this takes about $((exit_at + 20)) s — the shot is played in full so the END is measured too"

env BENILLA_HOME="$cfg" WOW_UNATTENDED=1 \
    WOW_USER="$PROBE_USER" WOW_PASS="$PROBE_PASS" WOW_CHAR="$char" \
    WOW_PROBE_CHAT=".go xyz $px $py $pz $pmap;.debug play cinematic $id" \
    WOW_PROBE_CHAT_AT="$park_at" WOW_PROBE_CHAT_EVERY="$((play_at - park_at))" \
    ${key:+WOW_PROBE_KEY="$key@$key_at:3"} \
    WOW_MOVE_TRACE="$trace" WOW_MOVE_TRACE_TAGS="move,snd" \
    WOW_PROBE_EXIT_AT="$exit_at" \
    cargo run -q -p benilla > "$log" 2>&1 || true

strip() { sed -E 's/\x1b\[[0-9;]*m//g'; }

echo
echo "── timeline ────────────────────────────────────────────────────────────────"
strip < "$log" | grep -E \
    "cinematic:|screen fade:|cinematic voice:|zone music:|ambience:|loading screen:|body held|probe-key:" \
    | sed -E 's/^[0-9-]+T([0-9:.]+)Z +INFO +[a-z_:]+: /\1  /' || true

echo
echo "── the two control facts ───────────────────────────────────────────────────"
if [ -s "$trace" ]; then
    # Everything before the park is the login/teleport arc and says nothing about the cinematic.
    after=$(awk -v t="$park_at" '$1=="t=" || $2+0 >= t' "$trace" 2>/dev/null || cat "$trace")
    low=$(printf '%s\n' "$after" | grep -oE 'pos=\[[-0-9.]+,[-0-9.]+,[-0-9.]+\]' \
          | sed -E 's/.*,//; s/\]//' | sort -n | head -1)
    echo "did it FALL?  lowest z after the park: ${low:-<none>}   (park z was $pz)"
    verbs=$(printf '%s\n' "$after" | grep -oE 'snd  [A-Za-z_]+' | sort | uniq -c | tr '\n' ' ')
    echo "did it MOVE?  wire verbs sent: ${verbs:-<none>}"
    echo "              (a StartForward means the body walked; only Heartbeats means it did not)"
else
    echo "no move trace written — the run did not reach the world"
fi

echo
if [ "$keep" = 1 ]; then
    echo "log:   $log"
    echo "trace: $trace"
else
    rm -rf "$work"
fi
