#!/bin/bash
# leg.sh — the interleaved A/B leg runner (1353 law 1, mechanized).
#
# Runs N rounds of (binary A, binary B) at a pin, one slot, one body, enforcing the leg laws the
# campaign kept re-typing by hand:
#   · load guard: waits for 1-min load average < 3 before every leg (1157)
#   · lock guard: refuses to start — and stops between legs — while the screen is locked
#     (1355: a locked screen throttles drawables at occluded_frames=0; the leg is invalid)
#   · occlusion: a leg with occluded_frames != 0 is discarded and re-run, up to 2 retries (0731)
#   · the whole FPS_PROBE line is kept per leg (never a grepped subset) under $LEG_DIR
#
# Usage:
#   scripts/leg.sh <binaryA> <binaryB> [rounds] [rig]
# Env:
#   WOW_USER/WOW_PASS/WOW_CHAR  probe credentials    (default probe0/pprobe0/Probezero — pool-0)
#   LEG_RIG                     WOW_RIG spec         (default: the LBRS pin, 0731)
#   LEG_FRAMES / LEG_AT         probe window         (default 300 / 45)
#   LEG_DIR                     where leg logs land  (default: $CLAUDE_JOB_DIR/tmp or /tmp)
#
# Motion legs: export WOW_PROBE_KEY / WOW_PROBE_CAM alongside — they inherit into every leg,
# A and B alike — to measure a MOVING regime, e.g. WOW_PROBE_KEY="W@40:20"
# WOW_PROBE_CAM="30,10@40:25" with the default LEG_AT=45 puts the whole FPS window inside a
# straight walk with the camera panning (1427: parked pins cannot see the play regime the
# director reports). Streaming churn inside the window is the point, not noise — but quote
# motion legs only against motion legs.
#
# Output: one line per kept leg (cpu_ms fps sys_busy), then per-round deltas and the means.
set -euo pipefail

A="${1:?usage: leg.sh <binaryA> <binaryB> [rounds] [rig]}"
B="${2:?usage: leg.sh <binaryA> <binaryB> [rounds] [rig]}"
ROUNDS="${3:-3}"
RIG="${4:-${LEG_RIG:-at:229,24.90,-396.50,48.80}}"
USER_="${WOW_USER:-probe0}"; PASS_="${WOW_PASS:-pprobe0}"; CHAR_="${WOW_CHAR:-Probezero}"
FRAMES="${LEG_FRAMES:-300}"; AT="${LEG_AT:-45}"
DIR="${LEG_DIR:-${CLAUDE_JOB_DIR:-/tmp}/tmp}"; mkdir -p "$DIR" 2>/dev/null || DIR="/tmp"

locked() { ioreg -n Root -d1 -a | grep -A1 CGSSessionScreenIsLocked | grep -q true; }
loadwait() { until [ "$(uptime | sed -E 's/.*averages?: ([0-9]+)[.,].*/\1/')" -lt 3 ]; do sleep 10; done; }

run_leg() { # $1 binary  $2 tag  → prints "cpu_ms fps sys_busy" or fails
  local bin="$1" tag="$2" try line occ fatal
  for try in 1 2 3; do
    if locked; then echo "leg.sh: screen is LOCKED — legs are invalid (1355); aborting" >&2; exit 2; fi
    loadwait
    WOW_UNATTENDED=1 WOW_USER="$USER_" WOW_PASS="$PASS_" WOW_CHAR="$CHAR_" WOW_RIG="$RIG" \
      WOW_LIVE_FPS="$FRAMES" WOW_LIVE_FPS_AT="$AT" \
      timeout 300 "$bin" >"$DIR/leg-$tag-$try.log" 2>&1 || true
    line=$(grep -a "FPS_PROBE" "$DIR/leg-$tag-$try.log" | sed 's/\x1b\[[0-9;]*m//g' | tail -1)
    # A FATAL is the client's own verdict that the run can never measure (refused login, missing
    # character, no world by the boot deadline) — deterministic, so a retry buys the same failure.
    # Abort the sitting loudly instead of burning tries × timeout (the 1371 legs sat 3 × 300 s at
    # a login screen the account guard had already refused).
    fatal=$(grep -a "FATAL" "$DIR/leg-$tag-$try.log" | sed 's/\x1b\[[0-9;]*m//g' | tail -1)
    if [ -n "$fatal" ]; then
      echo "leg.sh: $tag ABORTING the sitting — $fatal" >&2
      echo "leg.sh: full log: $DIR/leg-$tag-$try.log" >&2
      exit 4
    fi
    [ -n "$line" ] || { echo "leg.sh: $tag try $try produced no FPS_PROBE — see $DIR/leg-$tag-$try.log" >&2; continue; }
    occ=$(sed -E 's/.*occluded_frames=([0-9]+).*/\1/' <<<"$line")
    if [ "$occ" != "0" ]; then echo "leg.sh: $tag try $try occluded ($occ) — discarded unread" >&2; continue; fi
    # Regime guard (1388 round 3): one round's legs once read 126.5 vs 60.0 fps under the same
    # uncapped present mode — Metal displaySync is off either way, but the WindowServer rails a
    # composited window's drawables at its display's rate, and only sometimes. Every campaign
    # anchor is a railed leg (1380 "fps locked 60"), so a leg that escaped the rail measures a
    # different regime and is discarded here, by machine, instead of by a human reading the
    # round's deltas sideways. The probe line's display=...@hz stamp is the rail.
    fps=$(grep -oE " fps=[0-9.]+" <<<"$line" | cut -d= -f2)
    hz=$(grep -oE " display=[^ ]*@[0-9]+" <<<"$line" | sed 's/.*@//')
    if [ -n "$fps" ] && [ -n "$hz" ] && awk -v f="$fps" -v h="$hz" 'BEGIN{exit !(f > h + 3)}'; then
      echo "leg.sh: $tag try $try FREE-RUNNING (fps=$fps above the $hz Hz rail) — discarded" >&2; continue
    fi
    echo "$line" >>"$DIR/legs-kept.log"
    grep -oE "cpu_ms=[0-9.]+" <<<"$line" | cut -d= -f2
    return 0
  done
  echo "leg.sh: $tag failed 3 tries" >&2; exit 3
}

echo "leg.sh: $ROUNDS round(s), A=$(basename "$A") B=$(basename "$B"), rig=\"$RIG\", body=$CHAR_"
# Law 6 (1351/1353): the first leg after a build — or after the machine sat idle — is a warm-up,
# not a measurement. One discarded A-leg primes every cache the rounds then share.
# The assignment (not an inline $(...) in the echo) is load-bearing: run_leg's exit runs in the
# substitution subshell, and only an assignment's status is seen by set -e — inline, a warm-up
# abort would be discarded and the rounds would start anyway.
w=$(run_leg "$A" "warmup"); echo "  warm-up (discarded): cpu_ms=$w"
declare -a AV BV
for r in $(seq 1 "$ROUNDS"); do
  a=$(run_leg "$A" "r${r}A"); AV+=("$a"); echo "  round $r  A: cpu_ms=$a"
  b=$(run_leg "$B" "r${r}B"); BV+=("$b"); echo "  round $r  B: cpu_ms=$b"
done
awk_list() { local IFS=,; echo "$*"; }
python3 - "$(awk_list "${AV[@]}")" "$(awk_list "${BV[@]}")" <<'EOF'
import sys
a = [float(x) for x in sys.argv[1].split(',')]
b = [float(x) for x in sys.argv[2].split(',')]
ma, mb = sum(a)/len(a), sum(b)/len(b)
print(f"mean A={ma:.2f}  mean B={mb:.2f}  Δ={mb-ma:+.2f}  (rounds: {['%+.2f' % (y-x) for x,y in zip(a,b)]})")
EOF
