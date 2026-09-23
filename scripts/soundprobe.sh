#!/usr/bin/env bash
# soundprobe.sh — start the client in MEASURING MODE (decision 1556).
#
# The output limiter (1551) was proven offline and shipped on, and the director then played a real
# session and heard no change. That gap is not settled by another fix; it is settled by a capture
# from the machine and the encounter in question. This starts a run that records one.
#
# Builds with the `play` profile on purpose — the same profile the director actually plays in
# (decision 1157). A debug build stutters, a stutter is a missed mix deadline, and a missed
# deadline is one of the mechanisms under investigation: measuring on the wrong profile would
# manufacture the very artifact the capture is meant to attribute.
#
#   scripts/soundprobe.sh              # play normally, press F9 when you hear it
#   scripts/soundprobe.py              # …then read the capture back
#
# Any extra arguments are passed through to the client.
set -euo pipefail
cd "$(dirname "$0")/.."

cat <<'BANNER'

  ── benilla · sound measuring mode ─────────────────────────────────────────
    Recording two mixes at once: what the game ASKED for (before the limiter)
    and what you actually HEARD (after it). One file cannot tell those apart;
    two can, and that is the whole question.

    ▶ PRESS F9 THE MOMENT YOU HEAR IT.
      That stamps the exact sample. Without a mark I am scanning ten minutes
      of audio guessing which second you meant; with one I read outward from
      it. Press it every time — several marks is much better than one.

    Then quit the client normally and run:  scripts/soundprobe.py
  ───────────────────────────────────────────────────────────────────────────

BANNER

exec env WOW_SOUND_PROBE=1 cargo run -q --profile play -p benilla "$@"
