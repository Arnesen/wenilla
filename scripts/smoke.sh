#!/usr/bin/env bash
# **The fourth gate, as a command.** docs/METHOD.md asks for `fmt` · `clippy` · `test` · **a clean run**,
# and until this existed only the first three had a runner. The fourth was a paragraph in
# `docs/METHOD.md`, which is a different thing from a verb: a session that does not happen to read that
# paragraph concludes it *cannot* run the client, says so to the director, and ships a change whose
# whole subject is a live session boundary without ever having crossed one. That happened (2277).
#
# What it does: boots the real client against the local vmangos on THIS SLOT's probe account, seats
# a body in the world, `/logout`s to character select, **re-enters**, and exits — then reads the log
# back. It is the smallest run that crosses every session edge, which is exactly the set of edges
# the unit tests can only model.
#
# It is deliberately NOT wired into `gates.sh`: it wants a server, opens a window, and costs ~45 s,
# so paying it on every commit is the director's call, not this script's. Run it when a change
# touches a session boundary — and the gates print a pointer at you either way.
#
#   scripts/smoke.sh                 # this slot's probe account
#   WOW_SMOKE_KEEP=1 scripts/smoke.sh   # keep the log and print its path
set -uo pipefail

# The tree we are standing in, `gates.sh`'s rule verbatim: a session works in its own worktree and
# reaches for scripts by absolute path, so `$0`'s directory is the PRIMARY checkout and gating (or
# running) that instead is the bug that made the old gate chain lie.
root="$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$root" ] || [ ! -f "$root/scripts/smoke.sh" ]; then
    root="$(cd "$(dirname "$0")/.." && pwd)"
fi
cd "$root" || exit 1
echo "smoke: $root"

# **The smoke declares its whole environment; it does not inherit one.**
#
# Every leg below is a *run definition* — this account, this character or deliberately none, this
# smoke switch — and every one of them was previously composed against whatever `WOW_*` the calling
# shell happened to carry. The realm leg is where that stopped being theoretical: it names
# `WOW_USER`/`WOW_PASS` and pointedly NOT `WOW_CHAR`, because the walk drives *character select* and
# a seated body means there is no roster screen to drive. A session that had exported `WOW_CHAR` for
# its last probe — which is how every probe in `docs/METHOD.md` is launched — handed the leg the one
# thing it must not have, and the client dutifully entered the world while the walk waited for a
# screen that was never coming. The report was `the realm walk did not finish within the timeout`:
# 120 s, no cause, and nothing in the log that looks wrong.
#
# So the scrub is per-VARIABLE and up front, not per-leg: a gate whose result depends on the shell
# it was typed in is not a gate. Kept: `WOW_DATA` (where the install IS — a location, not a dial),
# `WOW_BG` (the director's "let me watch this one", which changes window stacking and nothing else),
# and `WOW_SMOKE_KEEP` (this script's own). Loud, because a session that meant to pass something
# should see it go rather than wonder why it did nothing.
inherited=""
for v in $(env | sed -n 's/^\(WOW_[A-Za-z0-9_]*\)=.*/\1/p'); do
    case "$v" in WOW_DATA | WOW_BG | WOW_SMOKE_KEEP) continue ;; esac
    inherited="$inherited $v"
    unset "$v"
done
[ -n "$inherited" ] &&
    echo "smoke: ignoring inherited env —$inherited (each leg names its own; a gate is not shell-dependent)"

# **The probe identity is keyed to the worktree slot** (docs/METHOD.md, "The local vmangos server"): a
# vmangos login KICKS whoever holds the account, so `one` is the director's live session and every
# other `probeN` is another session's. The slot claim is already the session mutex, so deriving the
# account from it makes probe exclusivity automatic. Refuse rather than guess.
slot="$(basename "$root" | sed -n 's/^pool-\([0-9]\)$/\1/p')"
if [ -z "$slot" ]; then
    echo "smoke: REFUSING — not in a pool worktree (\`$root\`)."
    echo "       The probe account is keyed to the slot; logging in as the default account would"
    echo "       kick the director's live session. Claim one: scripts/wt.sh claim <name>"
    exit 1
fi
# Spelled out literally, in docs/METHOD.md's own casing (`pool-0 → Probezero`) rather than derived: BSD
# sed has no `\U`, and deriving it cost a whole run to discover — the client dutifully reported
# `ProbeUzero` was not on the account, and then sat on the roster until the timeout.
names=(zero one two three four five six seven eight nine)
user="probe$slot"
pass="pprobe$slot"
char="Probe${names[$slot]}"
echo "smoke: slot $slot → $user / $char"

# The server, before the client: a refused connection reads as a client bug in the log and costs a
# full build to discover. Loud skip, never a silent pass — `gates.sh`'s posture for the install.
for port in 3724 8085; do
    if ! (exec 3<>/dev/tcp/127.0.0.1/$port) 2>/dev/null; then
        echo "smoke: SKIPPED — nothing listening on 127.0.0.1:$port (realmd 3724 / mangosd 8085)."
        echo "       The local vmangos lives at /Users/sam/dev/vmangos-deploy — \`docker compose up -d\`."
        exit 0
    fi
done

log="$(mktemp -t benilla-smoke)"
stamp="$(mktemp -t benilla-smoke-stamp)"
before="$(mktemp -t benilla-smoke-before)"
# The stamp and the file list always go; the LOG is the one a session may want to keep. Both in one
# trap so the early `fail` exits below cannot strand a temp file.
trap 'rm -f "$stamp" "$before"; [ -n "${WOW_SMOKE_KEEP:-}" ] || rm -f "$log"' EXIT

# **The install is READ-ONLY, and this is where that is measured** (decision 1486). benilla reads a
# WoW install; it never writes to one. The rule is easy to state and impossible to keep by memory —
# one `create_dir_all` under a path derived from `wow_data()` and a player's install has benilla's
# litter in it, on a folder that on this machine is SHARED with the sibling RE repo. So the fourth
# gate measures it: a full run of the real client, across a logout and a re-login, must leave the
# install byte-for-byte as it found it.
#
# Pure POSIX `find` on purpose — `stat`'s format flag is `-f` on BSD and `-c` on GNU, and a check
# that silently no-ops on the other platform is worse than none. The name list catches additions
# and deletions; `-newer` against a stamp touched just before the run catches modifications in
# place. ~7 ms over the 927-file tree.
install_root=""
if [ -n "${WOW_DATA:-}" ]; then
    install_root="$WOW_DATA"
elif [ -d "$root/WoW" ]; then
    install_root="$root/WoW"
fi
# Who ELSE might write in there. The install is shared — every pool slot symlinks the same tree,
# and the director runs the real 1.12 client against it constantly for RE comparison. That client
# writes its own WTF (Config.wtf, SavedVariables.lua, the per-character caches) while it runs, so a
# whole-tree mtime diff cannot tell "benilla wrote to the install" from "the reference client was
# open at the same time". Record it up front so the report below can name the right cause instead of
# blaming benilla for someone else's writes.
reference_client_before=""
if pgrep -f '[w]ow\.exe' >/dev/null 2>&1; then
    reference_client_before=1
fi
if [ -n "$install_root" ]; then
    find -L "$install_root" -type f 2>/dev/null | sort >"$before"
    echo "smoke: install read-only watch on $install_root ($(wc -l <"$before" | tr -d ' ') files)"
    [ -n "$reference_client_before" ] && \
        echo "smoke: NOTE — the reference client (wow.exe) is running; the install watch cannot attribute"
fi

# **Build first, THEN time the run.** The timeout below bounds the *client*, and a cold slot's
# build is minutes — folding the two together fails the gate with "the client did not exit", which
# names the wrong thing and sends the next session hunting a hang that is a compile. Build errors
# still fail here, loudly and as themselves.
if ! cargo build -q -p benilla >"$log" 2>&1; then
    echo "SMOKE FAILED: the client did not build"
    sed -E 's/\x1b\[[0-9;]*m//g' "$log" | tail -30
    exit 1
fi

echo "smoke: running the logout/re-login round trip (~45 s, opens a window)…"
# WOW_NOSOUND: the smoke is an agent's run, and `sound/mod.rs` opens no device under it — without
# it every smoke of every session played the login zone's music into the room for 45 s (2006).
WOW_UNATTENDED=1 WOW_NOSOUND=1 WOW_USER="$user" WOW_PASS="$pass" WOW_CHAR="$char" WOW_LOGOUT_SMOKE=1 \
    timeout 180 cargo run -q -p benilla >"$log" 2>&1
code=$?

# Strip ANSI once: the tracing level is a coloured field, and matching `ERROR` against the raw line
# also matches `ErrorsFrame.xml` — a false positive that would train everyone to ignore the check.
plain="$(sed -E 's/\x1b\[[0-9;]*m//g' "$log")"

fail() {
    echo "SMOKE FAILED: $1"
    [ -n "${WOW_SMOKE_KEEP:-}" ] && echo "  log: $log"
    printf '%s\n' "$plain" | tail -30
    exit 1
}

# Named before the timeout, because it IS the timeout's usual cause: an unseatable `WOW_CHAR` parks
# the client on the roster screen forever, and "did not exit" is the least useful way to say so.
printf '%s' "$plain" | grep -q "not on this account" &&
    fail "$char is not on $user — the client sat at the roster (make it, or rig it: WOW_RIG=…)"
[ $code -eq 124 ] && fail "the client did not exit within the timeout"
printf '%s' "$plain" | grep -q "logout-smoke: empty roster" &&
    fail "$user has no characters — the re-entry leg cannot run (make one, or rig it: WOW_RIG=…)"
printf '%s' "$plain" | grep -q "logout-smoke: done" ||
    fail "the round trip never completed (no 'logout-smoke: done')"

# **The second entry has to be DRIVABLE, not merely reached** (B306, decision 1542). The round trip
# crossed this boundary on every run since 2277 and only ever checked that it happened — so the
# report that arrived was a character who re-entered the world and could not move at all: vmangos
# roots you for the `/logout` countdown, we acked the grant, and the state outlived the session that
# was granted it. Nothing here could see that. Now the re-entry leg names whatever would kill WASD
# and this refuses anything but `none`.
sup="$(printf '%s\n' "$plain" | sed -n 's/.*logout-smoke: re-entered.*suppressors: \(.*\), done.*/\1/p' | tail -1)"
printf '  %-24s %s\n' "re-entry suppressors" "${sup:-<unreported>}"
[ "$sup" = "none" ] ||
    fail "the re-entered character could not be driven — suppressors: ${sup:-<unreported>} \
(1542: everything the ended session granted its mover dies with it)"

# **Did this run actually cross the ROOTED logout?** It did not, and it cannot: the probe accounts
# are `gmlevel 6` (decision 0530 — probes need GM for `.go`/`.cheat`) and the deploy sets
# `InstantLogout = 1`, so vmangos takes the instant branch of `CMSG_LOGOUT_REQUEST`
# (`MiscHandler.cpp`: resting OR taxi OR security >= the config) and returns without ever calling
# `SetRooted(true)`. B306's precondition is on the OTHER side of that `return`. So the drivable
# check above, for B306 specifically, passes without exercising it — and a check that can pass
# vacuously has to say so out loud, or a green run reads as coverage it does not have. That is the
# same blind spot as the bug: not a wrong answer, an unasked question. B306's own regression lives
# where it can be forced — the unit test at `player::wire_in::session_end_tests`.
if printf '%s\n' "$plain" | grep -q "mover mode Root granted"; then
    printf '  %-24s %s\n' "rooted logout" "yes — the drivable check above is a real pass"
else
    printf '  %-24s %s\n' "rooted logout" \
        "no (instant logout: GM probe / resting) — the check above did not exercise B306"
fi

errors="$(printf '%s\n' "$plain" | grep -cE ' ERROR ')"
panics="$(printf '%s\n' "$plain" | grep -cE 'panicked at')"
[ "$panics" -ne 0 ] && fail "$panics panic(s)"
[ "$errors" -ne 0 ] && fail "$errors ERROR line(s)"

# **The session invariant** (1290): the UI is built per login, so a two-login run must show every
# per-login step exactly twice. One occurrence means the second login re-entered the first login's
# frame tree — the "always Onewarrior" failure, which is invisible in a screenshot and silent in
# every log unless something counts.
#
# **Two counts, not one, since 2226.** This walk builds FOUR Lua states, not two: the client sits
# at the character screen on a boot VM, and the world entry now BUILDS its own rather than adopting
# that one (the reference's `0x490bd0` ↔ `0x48fbf0` pair). So a marker's expected count depends on
# which of the two things it tracks — every VM ever built, or only the VMs that loaded the in-game
# UI. Conflating them is what this loop used to do, and it was right only for as long as the entry
# adopted the glue VM.
#
# Which makes the first group a 2226 regression detector in its own right: **2 there rather than 4
# means a login inherited the character screen's session** — and with it every `VmMemo` that
# session had already spent, which is the login one-shot class (1348, B376) straight back.
sessions=2            # world entries in this walk
vms=$((sessions * 2)) # …and the Lua states they cost: a glue VM and a world VM each (2226)
for marker in "Fonts.xml loaded"; do
    n="$(printf '%s\n' "$plain" | grep -cF "$marker")"
    printf '  %-24s %s\n' "$marker" "$n"
    [ "$n" -eq "$vms" ] ||
        fail "'$marker' happened $n time(s), expected $vms — one per VM built, two per login (2226); \
$sessions would mean the entry adopted the character screen's VM and inherited its spent VmMemos"
done
# **The keybinding table is an entry-edge seed since 2241**: `seed_bindings_for_vm` runs inside
# `load_ingame_ui_on_world_entry`, beside the zone channels and the default language, so its
# marker counts once per LOGIN, not once per VM. This loop read it against `$vms` for one day —
# 2226 wrote the count on 09-14, 2241 moved the seed on 09-15 — and every smoke after that landing
# failed here on a tree whose VM count above was exactly right; found bisecting a render change
# that could not have touched it (2258's session). A marker's group is the *edge it fires on*.
for marker in "UIParent.xml loaded" "commands registered"; do
    n="$(printf '%s\n' "$plain" | grep -cF "$marker")"
    printf '  %-24s %s\n' "$marker" "$n"
    [ "$n" -eq "$sessions" ] ||
        fail "'$marker' happened $n time(s), expected $sessions — once per login: the UI rebuilt per login (1290), the keybinding table seeded on the entry edge (2241)"
done

# **The shutdown tail ran on BOTH roots** (decision 1528). One write is the `/logout`; the second is
# the exit, and the smoke now exits the way a player does — by closing the window. That is the only
# exit that tests whether the tail is reachable at all: the close button's `AppExit` is written in
# `PostUpdate`, so for as long as the shutdown systems read it in `Update` this count was 1 and
# every saved variable, every addon's file and the camera pose died with the process. Counted off
# the flat file's own line: it is only written when the UI really loaded, which is the property the
# sentinel needs. (It is no longer the tail's FIRST write — 2029 put the layout cache ahead of it,
# in the reference's own slot — but it is still the first that says the session had a UI.)
writes="$(printf '%s\n' "$plain" | grep -cF "saved variables: wrote")"
printf '  %-24s %s\n' "shutdown writes" "$writes"
[ "$writes" -eq "$sessions" ] ||
    fail "the shutdown tail wrote $writes time(s), expected $sessions — a session ended without \
saving (1528: the quit root must be observed in \`Last\`, after PostUpdate's exit_on_all_closed)"

# The read-only verdict (decision 1486). Reported by name: "something wrote to the install" is a
# rule violation somebody has to go and find, and the file that appeared is the whole lead.
if [ -n "$install_root" ]; then
    after="$(mktemp -t benilla-smoke-after)"
    find -L "$install_root" -type f 2>/dev/null | sort >"$after"
    appeared="$(comm -13 "$before" "$after")"
    vanished="$(comm -23 "$before" "$after")"
    touched="$(find -L "$install_root" -type f -newer "$stamp" 2>/dev/null)"
    rm -f "$after"
    if [ -n "$appeared" ] || [ -n "$vanished" ] || [ -n "$touched" ]; then
        if [ -n "$reference_client_before" ] || pgrep -f '[w]ow\.exe' >/dev/null 2>&1; then
            echo "SMOKE INCONCLUSIVE: the install changed, but the REFERENCE CLIENT was running."
            echo "  wow.exe writes its own WTF while it runs, and this watch is a whole-tree mtime"
            echo "  diff — it cannot tell those writes from benilla's. This is NOT evidence that"
            echo "  benilla wrote to the install, and it is not evidence that it did not."
            echo "  Close the reference client and re-run to get a real reading."
        else
            echo "SMOKE FAILED: the run WROTE TO THE INSTALL — benilla never does (decision 1486)."
            echo "  Every file benilla persists goes through \`crate::local_state\` into"
            echo "  \`benilla-config/\` beside the binary (decisions 0954/1175/1486)."
        fi
        [ -n "$appeared" ] && { echo "  added:"; printf '%s\n' "$appeared" | sed 's/^/    /'; }
        [ -n "$vanished" ] && { echo "  removed:"; printf '%s\n' "$vanished" | sed 's/^/    /'; }
        [ -n "$touched" ] && { echo "  modified:"; printf '%s\n' "$touched" | sed 's/^/    /'; }
        exit 1
    fi
    echo "  install untouched         $(wc -l <"$before" | tr -d ' ') files"
fi

# ── The realm-list leg (2069) ────────────────────────────────────────────────────────────────
#
# A second, short run, because it crosses a boundary the round trip above never touches: the realm
# list is a dialog raised **over** character select, and the IO thread has to keep serving it from
# the character park it is already sitting in. The version that ended the cycle there shipped, and
# the failure was invisible to every unit test in the workspace — the app was stranded on one
# screen while the thread walked two parks ahead, and the symptom the director saw was a client
# that hung on "Connecting" with nothing in the log at all. This drives Change Realm → Okay →
# Change Realm → Cancel → Okay against the real server and refuses anything but a completed walk.
echo "smoke: running the realm-list boundary walk (~12 s, opens a window)…"
rlog="$(mktemp -t benilla-smoke-realm)"
# **No `WOW_CHAR` here, deliberately** — an absent variable is invisible, so it is named instead.
# The walk drives character select; the fast path would seat a body and there would be no roster
# screen left to drive. The scrub at the top of this script is what makes the absence real, and
# `realm_select::smoke` refuses on its own if one reaches it anyway.
WOW_UNATTENDED=1 WOW_NOSOUND=1 WOW_USER="$user" WOW_PASS="$pass" WOW_REALM_SMOKE=1 \
    timeout 120 cargo run -q -p benilla >"$rlog" 2>&1
rcode=$?
rplain="$(sed -E 's/\x1b\[[0-9;]*m//g' "$rlog")"
[ -n "${WOW_SMOKE_KEEP:-}" ] || rm -f "$rlog"
realm_fail() {
    echo "SMOKE FAILED (realm leg): $1"
    [ -n "${WOW_SMOKE_KEEP:-}" ] && echo "  log: $rlog"
    printf '%s\n' "$rplain" | tail -30
    exit 1
}
[ $rcode -eq 124 ] && realm_fail "the realm walk did not finish within the timeout"
printf '%s' "$rplain" | grep -q "realm-smoke: FAILED" &&
    realm_fail "$(printf '%s\n' "$rplain" | sed -n 's/.*realm-smoke: FAILED — //p' | tail -1)"
printf '%s' "$rplain" | grep -q "realm-smoke: done" ||
    realm_fail "the walk never completed (no 'realm-smoke: done')"
rp="$(printf '%s\n' "$rplain" | grep -cE 'panicked at')"
[ "$rp" -ne 0 ] && realm_fail "$rp panic(s)"
printf '  %-24s %s\n' "realm walk" \
    "$(printf '%s\n' "$rplain" | sed -n 's/.*realm-smoke: done — //p' | tail -1)"

echo "SMOKE GREEN — ${sessions} logins + the realm walk, 0 errors, 0 panics, install untouched"
[ -n "${WOW_SMOKE_KEEP:-}" ] && echo "log: $log"
exit 0
