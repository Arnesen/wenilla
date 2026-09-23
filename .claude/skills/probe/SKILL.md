---
name: probe
description: Run the client unattended against the local vmangos server — a live probe, a capture, a leg. Use whenever a session or an agent needs the client logged in without a person at the keyboard.
---

The rules are in `docs/METHOD.md` ("The local server"). This is the how.

**Identity.** The account this checkout declares in `.probe-identity` at the tree root
(`WOW_USER=`, `WOW_PASS=`, `WOW_CHAR=`, one per line, never committed), or the same three in
the environment. A login kicks whoever holds the account, so a probe account is its own, with
the top GM level (the GM commands the probes use all work; `.modify money` is out of reach
even so), and from a checkout that declares one the client refuses the scripted login on any
other account; `WOW_ALLOW_ACCOUNT=1` overrides. `scripts/smoke.sh`, `cine.sh` and
`summon-live.sh` take the identity from `scripts/probe-identity.sh`, which applies this rule.

**Every run:** `WOW_UNATTENDED=1`, which reconnects instead of parking on a dialog and exits
non-zero on a login it cannot pass. An agent adds `WOW_NOSOUND=1`. The three scripts pass
`WOW_UNATTENDED=1`; `smoke.sh` and `summon-live.sh` add `WOW_NOSOUND=1`, and `cine.sh`
silences through a throwaway config with `MasterVolume=0` because the music path is its
subject. A capture (`WOW_CAPTURE`) implies both; a rig (`WOW_RIG`) implies unattended only.

**The body.** A probe cannot die: every world entry sends `.cheat god on`. `.die` clears it for
a death test; `WOW_GOD=off` runs unshielded. GM mode is on by default and makes reaction
colours, nameplates, threat, aggro, fall damage and breath read wrong: `WOW_GM=off` for any of
those. The preflight banner names the body and its state on every entry; read it first.

**A body that is not a level-1 human warrior:** `WOW_RIG="tauren druid 60 gear:heal-preraid-bis
spec:heal-preraid-bis at:ThunderBluff"` finds or creates it on the slot's account and applies
the lot; `WOW_RIG="gear:?"` lists the catalog. `WOW_PROBE_CHAT="<cmd>"` sends a GM command, and
the server's answer is echoed as `net: server says`.

**Wire work is proven from the trace:** `WOW_MOVE_TRACE=<path> WOW_MOVE_TRACE_TAGS="move,snd,in"
WOW_PROBE_EXIT_AT=<secs>`, then grep the file for the `rly` and `snd` lines. An unfiltered trace
in a populated scene stops being a per-frame clock.

**Keep a long run awake:** `caffeinate -u -t 2`, then `caffeinate -dis <command>`. An occluded
window runs at about 1 fps and the live-FPS line stamps `occluded_frames=`; a lock screen
throttles the same way but reads 0 with `p99_ms` near 1020, so check
`ioreg -n Root -d1 -a | grep CGSSessionScreenIsLocked` before an unattended series.

**Captures:** `scripts/visual.sh` wraps a capture run; the recipe, and why a capture goes
through `cargo run` and never a bare binary, is the header of
`crates/benilla-app/src/capture/mod.rs`. Pixel questions go through `benilla-visual crop`,
`series` and `hotspot`.
