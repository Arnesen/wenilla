#!/usr/bin/env bash
# **The two-client live summon probe** (decision 1747) — the instrument that closes the loop on
# being summoned, because nothing smaller can.
#
# The summon flow is the one confirm in the client that a single session cannot exercise at all:
# vmangos has exactly two `SendSummonRequest` callers (`Spell::EffectSummonPlayer` and
# `HandleGroupSummonCommand`), and **both skip you** — the ritual needs a warlock plus two
# clickers, and `.group summon` explicitly `continue`s past the caster. So the question can only
# be asked by somebody else, and the whole seam behind it (apply → feed → VM → drain → wire) is
# exactly the part unit tests cannot reach.
#
# What it does: a summoner client on a second account invites this checkout's probe character
# into a group and runs `.group summon`; the receiver reads its own CONFIRM_SUMMON dialog back
# through `ProbeLog`, presses Accept, and re-reads its zone once the teleport has landed. The
# receiver also writes the `summon` trace tag, so all three links report:
#
#   summon recv SMSG_SUMMON_REQUEST summoner=… zone=… delay_ms=… dead_or_ghost=false
#   summon fire CONFIRM_SUMMON summoner=… zone=…
#   summon SEND CMSG_SUMMON_RESPONSE summoner=… n=1
#
# **The summoner is a second account the shell names** — `SUMMON_USER`, `SUMMON_PASS`,
# `SUMMON_CHAR`, a GM-level test account whose login kicks nobody. The summoner's run carries
# `WOW_ALLOW_ACCOUNT=1`, because it is not the account this checkout declares; that the shell
# named it is what makes the override legitimate.
#
#   SUMMON_USER=… SUMMON_PASS=… SUMMON_CHAR=… scripts/summon-live.sh   # ~90 s, two small windows
set -uo pipefail

root="$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$root" ] && [ -f "$root/scripts/summon-live.sh" ] || root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root" || exit 1
echo "summon-live: $root"

# The receiver is this run's own identity — smoke.sh's rule (scripts/probe-identity.sh), for its
# reason: a login kicks whoever holds the account.
. "$root/scripts/probe-identity.sh"
probe_identity summon-live "$root" || exit 1
rx_user="$PROBE_USER"; rx_pass="$PROBE_PASS"; rx_char="$PROBE_CHAR"

# The summoner is the shell's to name (see the header).
if [ -z "${SUMMON_USER:-}" ] || [ -z "${SUMMON_PASS:-}" ] || [ -z "${SUMMON_CHAR:-}" ]; then
    echo "summon-live: REFUSING — the summoner needs a second account: SUMMON_USER, SUMMON_PASS"
    echo "             and SUMMON_CHAR, a GM-level test account whose login kicks nobody."
    exit 1
fi
tx_user="$SUMMON_USER"; tx_pass="$SUMMON_PASS"; tx_char="$SUMMON_CHAR"
echo "summon-live: $tx_char ($tx_user) summons $rx_char ($rx_user)"

probe_server_or_skip summon-live || exit 0

# Build once, so the two `cargo run`s below start together instead of one waiting on the other's
# build lock — the timings are wall-clock from process start (smoke.sh's build-then-time rule).
echo "summon-live: building…"
cargo build -q -p benilla || { echo "summon-live: the client did not build"; exit 1; }

work="$(mktemp -d "${TMPDIR:-/tmp}/benilla-summon.XXXXXX")"
trap '[ -n "${SUMMON_LIVE_KEEP:-}" ] || rm -rf "$work"' EXIT

# The receiver's chunk. `tostring` on every getter on purpose: a missing dialog must REPORT rather
# than raise at line 2, or a failed run says "attempt to concatenate a nil value" and names nothing.
read -r -d '' chunk <<'LUA'
ProbeLog("dialog visible=" .. tostring(StaticPopup1:IsVisible()))
ProbeLog("dialog text=[" .. tostring(StaticPopup1Text:GetText()) .. "]")
ProbeLog("summoner=[" .. tostring(GetSummonConfirmSummoner()) .. "]")
ProbeLog("area=[" .. tostring(GetSummonConfirmAreaName()) .. "]")
ProbeLog("timeleft=" .. tostring(GetSummonConfirmTimeLeft()))
ProbeLog("zone before=[" .. GetZoneText() .. "]")
StaticPopup_OnClick(StaticPopup1, 1)
ProbeLog("accept pressed; dialog visible=" .. tostring(StaticPopup1:IsVisible()))
-- The teleport is OBSERVED, not assumed: a frame that re-reads the zone ten seconds after the
-- Accept, which is the only thing that distinguishes "we sent the packet" from "we were summoned".
local watch = CreateFrame("Frame")
watch.t = 0
watch:SetScript("OnUpdate", function()
    this.t = this.t + arg1
    if this.t > 10 and not this.said then
        this.said = 1
        ProbeLog("zone after=[" .. GetZoneText() .. "]")
    end
end)
LUA

echo "summon-live: running (~90 s, opens two windows)…"
# **The receiver is parked first, and the run is worthless without it.** Its character starts
# wherever the last run left it — which, after one green run, is the destination — so a
# before/after zone compare silently stops discriminating on the second run. It did exactly that.
# Parking in Stormwind makes "before" the same on every run, whatever the previous one did.
WOW_UNATTENDED=1 WOW_NOSOUND=1 WOW_USER="$rx_user" WOW_PASS="$rx_pass" WOW_CHAR="$rx_char" \
    WOW_PROBE=partner \
    WOW_PROBE_CHAT=".go xyz -8913 554 94" WOW_PROBE_CHAT_AT=10 \
    WOW_PROBE_LUA="$chunk" WOW_PROBE_LUA_AT=34 \
    WOW_PROBE_EXIT_AT=62 \
    WOW_MOVE_TRACE="$work/rx.trace" WOW_MOVE_TRACE_TAGS=summon \
    timeout 150 cargo run -q -p benilla >"$work/rx.log" 2>&1 &
rx=$!

# `WOW_ALLOW_ACCOUNT=1` is the account guard's own escape hatch, and the shell naming the account
# is what earns it. `.group summon` rather than `.summon`: the latter teleports without ever asking.
WOW_UNATTENDED=1 WOW_NOSOUND=1 WOW_USER="$tx_user" WOW_PASS="$tx_pass" WOW_CHAR="$tx_char" WOW_ALLOW_ACCOUNT=1 \
    WOW_PROBE_CHAT="/invite $rx_char;.group summon" \
    WOW_PROBE_CHAT_AT=18 WOW_PROBE_CHAT_EVERY=10 \
    WOW_PROBE_EXIT_AT=62 \
    timeout 150 cargo run -q -p benilla >"$work/tx.log" 2>&1 &
tx=$!

wait $rx; rx_code=$?
wait $tx; tx_code=$?

strip() { sed -E 's/\x1b\[[0-9;]*m//g' "$1"; }
trace="$(cat "$work/rx.trace" 2>/dev/null)"
plog="$(strip "$work/rx.log" | sed -n 's/.*probe-log: //p')"

echo
echo "── the summoner"
strip "$work/tx.log" | grep -E "probe-chat: sending|server says —|REFUSING" | sed 's/^.*INFO [^:]*: //'
echo
echo "── the receiver: the three links"
printf '%s\n' "$trace"
echo
echo "── the receiver: what the dialog said"
printf '%s\n' "$plog"

fail() { echo; echo "SUMMON-LIVE FAILED: $1"; strip "$work/rx.log" | grep -E "ERROR" | tail -10; exit 1; }
[ $rx_code -eq 124 ] && fail "the receiver did not exit within the timeout"
[ $tx_code -eq 124 ] && fail "the summoner did not exit within the timeout"
printf '%s' "$trace" | grep -q "recv SMSG_SUMMON_REQUEST" ||
    fail "no summon request arrived — did the invite land? (see the summoner's lines above)"
printf '%s' "$trace" | grep -q "fire CONFIRM_SUMMON" || fail "the request landed but no event fired"
printf '%s' "$trace" | grep -q "SEND CMSG_SUMMON_RESPONSE" || fail "Accept sent no packet"
printf '%s' "$plog" | grep -q "wants to summon you to" || fail "the dialog text never composed"
# The two that cannot be faked by any amount of client-side bookkeeping: the character actually
# moved, and it moved to **the place the dialog named**. The second is what ties
# GetSummonConfirmAreaName to reality rather than to our own AreaTable lookup agreeing with itself.
before="$(printf '%s' "$plog" | sed -n 's/^zone before=\[\(.*\)\]$/\1/p')"
after="$(printf '%s' "$plog" | sed -n 's/^zone after=\[\(.*\)\]$/\1/p')"
area="$(printf '%s' "$plog" | sed -n 's/^area=\[\(.*\)\]$/\1/p')"
[ -n "$after" ] || fail "the receiver never re-read its zone (the teleport went unobserved)"
[ "$before" != "$after" ] ||
    fail "the packet went out but the character never moved (still in $before) — if before and \
after are BOTH the destination, the park at the top of the run did not take"
[ "$after" = "$area" ] ||
    fail "the dialog promised $area and the character landed in $after"

echo
echo "SUMMON-LIVE GREEN — asked, shown, accepted, and moved: $before → $after (the dialog's own $area)"
[ -n "${SUMMON_LIVE_KEEP:-}" ] && echo "  logs: $work"
exit 0
