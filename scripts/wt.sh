#!/usr/bin/env bash
# wt.sh — the session-worktree pool (decisions 0192 + 0433).
#
# Sessions used to `git worktree add` a fresh path per task and `git worktree remove` it after.
# Every fresh path pays a cold dependency build (~4.5 min at full CPU: ~139 dep compiles embed
# worktree-specific OUT_DIR paths, so sccache can't help) — measured at ~6 h of saturated cores
# per fortnight, and the single biggest source of machine lag while the director is testing.
# A *reused* path keeps its warm `target/`, so re-pointing it at a new branch costs only the
# workspace-crate rebuild (~25-60 s). So: a fixed pool of worktree paths, claimed and released.
#
# WHERE THE POOL LIVES (decision 1726, 2026-08-30). Slots are created on the EXTERNAL drive
# ($POOL_DIR, default /Volumes/SanDisk/benilla-wt); the old internal root ($LEGACY_POOL_DIR) is
# kept and still fully worked by every verb, but never allocated from again — it DRAINS as its
# sessions land, and sweep retires each slot once free. `status` tags those DRAINING. If the drive
# is not mounted, every verb that would create a slot REFUSES rather than silently building the
# path on the boot volume (pool_root_ready). WT_POOL_ROOT overrides the root for one command.
#
#   scripts/wt.sh claim <name>   -> claims a free slot, checks out `sess/<name>` at main,
#                                   links WoW data, prints the slot path on stdout and how to use
#                                   it on stderr. THIS IS THE ONLY WORKTREE MECHANISM — no harness
#                                   worktree tool, no hand-rolled `git worktree add` (decision
#                                   1040). ONE slot per session: a session already holding a claim
#                                   — or a dispatched agent, which shares its parent's session id —
#                                   is refused and told which slot it holds (decision 1756;
#                                   WT_ALLOW_SECOND_SLOT=1 for the rare deliberate case). A full
#                                   pool self-heals twice before giving up: it takes
#                                   over ABANDONED claims (heartbeat silent $WT_IDLE_MINS, clean,
#                                   landed — reap_idle_claims), then runs `sweep`, retrying after
#                                   each. If it still fails, the answer is to STOP and ask.
#   scripts/wt.sh reclaim <name> [path] -> re-attach a slot that is on a DETACHED HEAD (what you
#                                   are on if you kept working after `land`) onto sess/<name> at
#                                   its current HEAD, keeping orphaned commits AND a dirty tree.
#                                   `claim` is not the recovery: it skips a dirty slot and hands
#                                   you a different one, stranding the work. See reclaim().
#   scripts/wt.sh sync <name> [path]    -> rebase onto main now. A conflict confined to the generated
#                                   docs/MAP.md is resolved by regenerating it.
#                                   OPTIONAL since 2049 — `land` rebases and gates on its own;
#                                   `sync` + a `gates.sh` run beforehand only make that land
#                                   instant (the gate memo, gates.sh).
#   scripts/wt.sh land <name> [path]    -> THE session ending, all of it (decision 2049). Work that is
#                                   docs only lands at once under the MAIN LOCK. Work that touches
#                                   code goes to a detached WORKER that waits its turn in the LAND
#                                   QUEUE, rebases onto main, regenerates the map, runs the full
#                                   gate chain on the tree that will land (plus crosscheck.sh when
#                                   the diff touches a platform seam — 2331), fast-forwards + pushes
#                                   main and releases the slot — while no other code land can
#                                   move main, so a green gate is never stale. Prints the worker's
#                                   log and exits with its verdict; RUN IT IN THE BACKGROUND, and
#                                   a re-run re-attaches to a land already in flight.
#   scripts/wt.sh release <name> [path] -> abandon without landing: verifies clean + landed,
#                                   detaches, deletes the sess branch, returns the slot (warm
#                                   target intact — unless it grew past $WT_TARGET_CAP_GB, then
#                                   reset: the claimed-slot sediment bound, decision 0522)
#   scripts/wt.sh sweep [--dry-run] -> the janitor (also run by a full `claim`; step 5 bounds the
#                                   pool's TOTAL under the disk floor, not just each slot): reap dead
#                                   claims (clean + landed + claim AND tip both older than $WT_STALE_HOURS), remove
#                                   dead non-pool worktrees (agent/legacy leftovers), bound
#                                   unclaimed slots' target/ sediment at $WT_TARGET_CAP_GB, and
#                                   retire slots at/above $WT_MAX_SLOTS once free + clean + landed.
#                                   It also REPORTS the one dead claim it must never touch — see
#                                   orphaned_claim() and decision 1400.
#   scripts/wt.sh status         -> per-slot: holder, claim age, branch, dirtiness, target size,
#                                   LANDING on a slot whose land worker is running, the land queue,
#                                   a STALE tag on reapable claims, ORPHANED on a dead one no verb
#                                   can reap, RETIRING above $WT_MAX_SLOTS, STUCK on a free+dirty
#                                   slot (unclaimable AND unreapable — the silent capacity leak);
#                                   non-pool worktrees; the two big consumers that belong to no slot
#                                   (the PRIMARY's target/ and the scratchpad root); disk free;
#                                   main's unpushed gap (the backstop for a failed/bypassed push)
#
# Every verb that writes main also PUBLISHES it (decision 0570) — pushing is not a thing a session
# has to remember. See push_main() for why it is non-fatal and never forced.
#
# Claims are a `.wt-claimed` marker in the slot — `name<TAB>claimed-at<TAB>session-id`, created
# with noclobber so racing sessions can't double-claim. Dead sessions used to need hand surgery
# (rm .wt-claimed) — the 2026-07-17 disk
# incident (3 GB free: 3 dead claims, 5 dead legacy worktrees, ~60 GB of dead agent worktrees,
# 74 GB sediment targets) turned that into `sweep`, run automatically when `claim` finds the
# pool full. Every reap re-verifies clean+landed in-process and deletes branches with `-d`
# (never `-D`), so nothing unlanded can ever be swept. sweep bounds only UNCLAIMED sediment,
# though: a long-lived CLAIM keeps growing its target/ out of sweep's reach (a 2026-07-18 slot
# hit 101 GB, dropping the disk under its floor again) — so release/land reset a slot whose
# target passed $TARGET_CAP_GB at session end, and status tags it BLOATED before then (0522).
# Under the disk floor, sweep step 5 stops honouring the per-slot cap and reclaims unclaimed
# sediment largest-first until the floor clears — the sum bound this script spent three disk
# incidents without (decision 1569). A CLAIMED slot is still untouchable there.
#
# Every slot verb (sync/land/release) takes the caller's CLAIM NAME and verifies it against
# the slot's marker before acting (decision 0520). $PWD alone is not identity: on 2026-07-18 a
# fresh session ran `reserve` with its shell still parked in ANOTHER session's slot — the stub
# was stamped with the bystander's branch and the bystander's tree was silently rebased
# mid-session (a clean-window fluke away from yanking edits out from under it; a wrong-cwd
# `land` would have shipped the bystander's half-done branch to main).
#
# The claim NAME is not identity either (decision 1041). A name check passes whenever the names
# match — including when they match because a SECOND session adopted the first's name, read out of
# the marker or inherited with a stale cwd. On 2026-08-06 two sessions ran as `auraslots` in
# pool-2: the second's `land` pushed the first's decision records to main and stripped its code
# onto a `preserved/` branch, leaving main citing two decisions with nothing behind them. So the
# marker carries a third field — the claiming SESSION's id, which a name-copier cannot copy — and
# every verb compares it (`own_slot`). `status` tags the one slot you own `(yours)`; finding none
# is the same collision one step earlier. Escape hatch: `WT_ALLOW_FOREIGN=1`.
#
# Dials (env overrides):
#   WT_MAX_SLOTS      (8)  slots the pool claims (pool-0..N-1) — it may only LOWER the pool. 8 is a
#                     HARD ceiling ($POOL_CEILING) and a value above it is REFUSED, not clamped;
#                     lowering retires the slots above via sweep step 4, as each falls free — never
#                     under a live session
#   WT_IDLE_MINS      (30) minutes since the claim marker's last HEARTBEAT before a clean+landed
#                     claim counts as abandoned and `claim` may take it over. Only ever applied to
#                     a marker that has actually been heartbeaten (see heartbeat_seen) — a claim
#                     from before the heartbeat existed keeps the old WT_STALE_HOURS rule
#   WT_STALE_HOURS    (24) claim age AND tip-commit age before a clean+landed one counts as dead
#   WT_TARGET_CAP_GB  (100) target/ size that counts as sediment: sweep resets an unclaimed slot
#                     past it, release/land reset a slot past it at session end (decision 0522).
#                     Raised 40 -> 60 on 2026-08-22 (director's call): at 40 a normal session was
#                     tripping the reset and paying a cold full-workspace rebuild on its next
#                     claim, which is the cost this cap exists to AVOID, not to cause. Raised
#                     60 -> 100 on 2026-09-01 (director's call): the pool lives on the external
#                     drive now (1726; 931 G, its own budget), a wiped slot's cold gate chain was
#                     just measured at 27 min vs 80 s warm, and warmest-first claiming (1822)
#                     deliberately steers sessions INTO the fattest targets — a low cap and that
#                     rule fight each other. Read it with the SUM caveat below — this bounds one
#                     slot; the disk floor is what protects the drive.
#   WT_DISK_FLOOR_GB  (60) free-disk floor under which status/claim warn loudly
#   WT_ALLOW_FOREIGN  (0)  =1 skips the claim's session check (decision 1041) — for the one real
#                     case, a session resumed under a new id that must land its own old slot
#   WT_ALLOW_SECOND_SLOT (0) =1 lets `claim` hand this session a second slot (decision 1756) — for
#                     a genuine deliberate case only; a dispatched agent inheriting its parent's id
#                     is exactly what the refusal exists to stop
#   WT_SCRATCH_ROOT   (derived) where the harness keeps sessions' scratchpads; `status` REPORTS its
#                     size and nothing more (decision 1400)
set -euo pipefail

PRIMARY="$(cd "$(git rev-parse --path-format=absolute --git-common-dir)/.." && pwd)"
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"

# ALWAYS RUN MAIN'S COPY (decision 2049). The land queue is a PROTOCOL: one stale copy of this script
# — a slot branched before the queue existed, running `cd $slot && ./scripts/wt.sh land` as docs/METHOD.md
# says to — fast-forwards main straight past everyone's tickets, and the queue protects nobody. The
# primary checkout is always at main and always clean (docs/METHOD.md), so its scripts/wt.sh IS the
# current protocol; a copy that differs re-executes it, once, and says so. WT_NO_REEXEC=1 runs the
# copy you invoked — for a session that is changing this script and needs to test its own edits,
# and for the detached land worker, which must keep the copy it started with while main moves.
if [ "${WT_NO_REEXEC:-}" != "1" ] && [ -f "$PRIMARY/scripts/wt.sh" ] && ! cmp -s "$SELF" "$PRIMARY/scripts/wt.sh"; then
  echo "wt.sh: this copy ($SELF) differs from main's — running $PRIMARY/scripts/wt.sh instead (2049; WT_NO_REEXEC=1 to insist)" >&2
  WT_NO_REEXEC=1 exec bash "$PRIMARY/scripts/wt.sh" "$@"
fi
# The pool's home moved to the external drive on 2026-08-30 (decision 1726): eight slots of warm
# target/ was ~196 G of a 926 G internal disk, and a measured cold build there costs 4% over the
# NVMe (507s vs 486s) with warm rebuilds identical, so the space is nearly free to move.
#
# TWO roots, because a live session must never have the ground moved under it. NEW slots are
# created in POOL_DIR alone; LEGACY_POOL_DIR is the old internal root, which every verb still
# RECOGNIZES — status lists it, sweep and the idle reap work it, land and release act on it — and
# which try_claim never allocates from. It drains on its own: each legacy slot keeps its session
# until that session lands, and sweep retires the slot once it is free, clean and landed.
POOL_DIR="${WT_POOL_ROOT:-/Volumes/SanDisk/benilla-wt}"
LEGACY_POOL_DIR="$(dirname "$PRIMARY")/benilla-wt"
# Where the harness keeps sessions' scratchpad directories. `status` prints its size; NOTHING here
# deletes by it (decision 1400). 24 G had accumulated there unnoticed by 2026-08-17 — 17 G of it one
# dead session's cargo target — because no rule of ours has ever looked outside the repo. Reporting
# is the right amount of authority for a path that is an OBSERVED convention rather than a contract
# we own: a wrong guess would be a wrong `rm` outside the repo, and a stale one would silently match
# nothing while looking like coverage. A human reads the number and decides.
SCRATCH_ROOT="${WT_SCRATCH_ROOT:-/private/tmp/claude-$(id -u)/$(printf '%s' "$PRIMARY" | tr '/' '-')}"
# Slots the pool will claim: pool-0 .. pool-$((MAX_SLOTS-1)). Ten slots × a ~35 G target/ is ~350 G
# of warm build cache, and TARGET_CAP_GB bounds each slot but nothing bounded the SUM — on
# 2026-08-05 every slot sat just under the 40 G cap, so the sediment reset never fired once and the
# disk hit 3.9 G free. The 2026-08-22 raise to 60 G widened that same worst case to ~480 G (the
# 2026-09-01 raise to 100 G widens it to ~800 G of the external drive's 931 G), and it
# recurred on 2026-08-23: six free slots holding 236 G, every one under the cap, `status` saying
# DISK LOW — run 'wt.sh sweep' and `sweep` answering "nothing to do". **sweep step 5 bounds the sum
# now** — below WT_DISK_FLOOR_GB the per-slot cap stops being the rule and unclaimed slots are
# reset largest-first until the floor clears (decision 1569). This dial stays a per-slot bound;
# the floor is what actually protects the disk. LOWERING this number does not remove the slots
# above it: sweep step 4 retires them, once each is free, clean and landed.
#
# POOL_CEILING is a HARD cap, not a dial (decision 1037). The pool may never exceed it. A full pool
# used to end in "wait for a land, or grow MAX_SLOTS", and on 2026-08-06 a session read that closing
# clause as an instruction: `WT_MAX_SLOTS=9 wt.sh claim` created pool-8 and paid the cold build, on
# the one machine whose disk floor is the entire reason the number is 8. Growing is never the answer
# to a full pool — eight is the concurrency the disk budget was sized for, so a ninth session waits
# for a land or takes a slot over by the director's word. WT_MAX_SLOTS may still LOWER the pool
# (the drain path sweep step 4 serves); raising it is refused, loudly.
POOL_CEILING=8
MAX_SLOTS="${WT_MAX_SLOTS:-$POOL_CEILING}"
STALE_HOURS="${WT_STALE_HOURS:-24}"
IDLE_MINS="${WT_IDLE_MINS:-30}"
TARGET_CAP_GB="${WT_TARGET_CAP_GB:-100}"
DISK_FLOOR_GB="${WT_DISK_FLOOR_GB:-60}"
# How far ABOVE the floor sweep's sum bound (step 5) climbs before it stops resetting slots.
# Stopping exactly at the floor would leave the next claim's cold build straight back under it,
# and sweep would eat another slot an hour later; a little room makes the reclaim settle.
FLOOR_HEADROOM_GB="${WT_FLOOR_HEADROOM_GB:-40}"

die() { echo "wt.sh: $*" >&2; exit 1; }

# The ceiling is enforced here, before any verb runs — refused rather than silently clamped, so a
# session that asked for a bigger pool learns the pool does not grow instead of quietly getting the
# same eight slots back and a confusing "all slots claimed".
case "$MAX_SLOTS" in '' | *[!0-9]*) die "WT_MAX_SLOTS must be a whole number (got '$MAX_SLOTS')" ;; esac
[ "$MAX_SLOTS" -ge 1 ] || die "WT_MAX_SLOTS must be at least 1 (got $MAX_SLOTS)"
[ "$MAX_SLOTS" -le "$POOL_CEILING" ] || die \
  "WT_MAX_SLOTS=$MAX_SLOTS is above the hard ceiling of $POOL_CEILING — the pool never grows." \
  "A full pool means: wait for a land, or ask the director which slot to take over."

# Is the pool's root actually THERE? An external volume that is unplugged, asleep or not yet
# mounted leaves /Volumes/<name> as an ordinary directory ON THE BOOT VOLUME, and `git worktree
# add` would cheerfully create the slot inside it: every message would say "external" while a 50 G
# target/ filled the very disk this move exists to empty. Silent until "why is the disk full
# again". So every verb that creates a slot asks first.
#
# The test is the mount, not the path — a real mount point's device id differs from its parent's.
# A root inside the boot volume (the legacy root, or a WT_POOL_ROOT override) needs no check.
pool_root_ready() {
  case "$POOL_DIR" in /Volumes/*) ;; *) return 0 ;; esac
  local rest="${POOL_DIR#/Volumes/}" vol
  vol="/Volumes/${rest%%/*}"
  [ -d "$vol" ] || return 1
  [ "$(stat -f %d "$vol" 2>/dev/null || echo same)" != "$(stat -f %d /Volumes 2>/dev/null || echo same)" ]
}

pool_root_or_die() {
  pool_root_ready && return 0
  die "the pool's drive is not mounted — $POOL_DIR is not on a mounted volume." \
      "Nothing was created. A slot made now would land on the BOOT volume under a path that says" \
      "otherwise, and quietly refill the disk the pool was moved off (decision 1726)." \
      "Plug the drive in and re-run, or run the pool from the internal disk for this command:" \
      "WT_POOL_ROOT=$LEGACY_POOL_DIR scripts/wt.sh ..."
}

# Every slot the pool KNOWS, new root first — a different question from where a new slot may be
# CREATED (try_claim walks POOL_DIR alone). status, sweep, the idle reap and reclaim all have to
# see a slot wherever it lives; without this a legacy slot goes invisible the moment the root
# moves — unlistable, unreapable, and still holding 50 G.
all_slots() {
  local root slot seen=""
  for root in "$POOL_DIR" "$LEGACY_POOL_DIR"; do
    case " $seen " in *" $root "*) continue ;; esac # the two coincide under a WT_POOL_ROOT override
    seen="$seen $root"
    [ -d "$root" ] || continue
    for slot in "$root"/pool-*; do
      [ -e "$slot" ] || continue
      printf '%s\n' "$slot"
    done
  done
}

# A slot in the draining root: recognized by every verb, allocated by none, retired once free.
legacy_slot() {
  [ "$LEGACY_POOL_DIR" != "$POOL_DIR" ] || return 1
  case "$1" in "$LEGACY_POOL_DIR"/pool-*) return 0 ;; *) return 1 ;; esac
}

# Uncommitted state in a worktree, with the claim marker itself excluded — the marker is also in
# .gitignore, but correctness here must not hang on that line staying put.
slot_dirty() { git -C "$1" status --porcelain 2>/dev/null | grep -qv '^?? \.wt-claimed$'; }

# The claiming SESSION's own id — the conversation's, stable across its compactions, and NOT
# derived from anything inside the slot. `-` when there is none (a human shell, CI, a script).
# That last part is the whole point: a session that reads its claim name out of `.wt-claimed`
# copies the name but cannot copy this, so [`own_slot`] can tell the two apart.
session_id() { printf '%s' "${CLAUDE_CODE_SESSION_ID:--}"; }

# The ownership check (decision 0520, widened by 1041): does this slot belong to the caller?
#
# TWO questions, because the 2026-08-06 incident answered the first one yes and was still wrong.
#
# 1 · **The claim NAME** (0520). `sess/` is stripped from both sides (claim normalizes it away too,
#     but pre-0520 markers may carry it). Catches a wrong-cwd invocation: a session running its own
#     verb while parked in a bystander's slot.
#
# 2 · **The claim's SESSION** (1041). 0520's check passes whenever the names match — including when
#     they match because a *second* session adopted the first one's name (read out of the marker, or
#     inherited with a stale cwd). That is not hypothetical: on 2026-08-06 two sessions ran as
#     `auraslots` in pool-2, and the second one's `land` shipped the first's decision records to main
#     while its code was stripped onto a `preserved/` branch — main claiming two decisions with no
#     implementation behind them. The name was never the identity; the session is.
#
# A marker with no session field (claimed before 1041) verifies on the name alone, exactly as
# before — live claims keep working and age out at their next land.
own_slot() { # $1 = slot path, $2 = the caller's claim name
  local slot="$1" name="${2#sess/}"
  [ -n "$name" ] || die "this verb needs your claim name (the <name> you passed to 'wt.sh claim')"
  [ -e "$slot/.wt-claimed" ] || die "$slot has no claim"
  local holder; holder="$(cut -f1 "$slot/.wt-claimed")"; holder="${holder#sess/}"
  [ "$holder" = "$name" ] || die "$slot belongs to claim '$holder', not '$name' — your shell is in the wrong worktree; cd into your own slot"

  local owner mine; owner="$(cut -f3 "$slot/.wt-claimed")"; mine="$(session_id)"
  case "$owner" in "" | "-") return 0 ;; esac  # pre-1041 marker: name-only, as before
  [ "$mine" != "-" ] || return 0               # no session id to compare (human shell, CI)
  [ "${WT_ALLOW_FOREIGN:-}" != "1" ] || return 0
  [ "$owner" = "$mine" ] || die "$(printf '%s\n' \
    "$slot is claim '$holder' — but that claim belongs to a DIFFERENT SESSION." \
    "    claimed by session $owner" \
    "    you are      session $mine" \
    "Two sessions in one slot revert each other's edits and split each other's commits: on" \
    "2026-08-06 a land shipped one session's decision records to main with the other's code" \
    "stripped onto a preserved/ branch (decision 1041)." \
    "Claim your own slot instead:  ./scripts/wt.sh claim <short-task-name>" \
    "If you really are that session under a new id, re-run with WT_ALLOW_FOREIGN=1.")"
}

link_wow() { # a fresh slot lacks the gitignored game-data symlinks; runs silently miss without them
  local slot="$1"
  local link
  for link in WoW WoW-era; do # WoW-era: the Era install (0068 §7) — api-coverage.sh reads addons from it
    [ -e "$slot/$link" ] || { [ -e "$PRIMARY/$link" ] && ln -s "$(readlink "$PRIMARY/$link")" "$slot/$link"; }
  done
  # The EXE-DIR install link too: the resolver's ladder is $WOW_DATA, then Data/ and WoW/Data
  # BESIDE THE BINARY (benilla-formats/src/install.rs — the drop-in-a-folder convention), so a
  # direct target/debug/benilla run (every leg wrapper) resolves through target/debug/WoW. The
  # 40G sediment reset deletes it with the rest of target/ and nothing re-made it: one night's
  # leg sittings died at startup ("no WoW install found") on the first post-reset wrapper run
  # (2026-08-18). Idempotent, and created even before the first build (mkdir -p).
  #
  # BOTH profile dirs, and the PRIMARY as well as the slot (decision 1451). `cargo play` builds
  # into target/play, and the exe-dir ladder is the ONLY one a `--no-default-features` binary has
  # — install.rs candidate 2 (the project folder) is `dev`-only on purpose, so the player build a
  # gate is about cannot fall back on the source tree. Without a link beside *that* binary,
  # `cargo play --no-default-features` from the primary — the director's own way of looking at
  # what a player gets — starts with no world at all.
  local root prof
  for root in "$slot" "$PRIMARY"; do
    for prof in debug play; do
      [ -e "$root/target/$prof/WoW" ] || { [ -e "$PRIMARY/WoW" ] && mkdir -p "$root/target/$prof" && ln -s "$(readlink "$PRIMARY/WoW")" "$root/target/$prof/WoW"; }
    done
  done
  # The addon corpus too (decision 2329): `wow-addons-vanilla` is the second piece of external
  # data the tests read — third-party, never in the repo, and it lives BESIDE the primary, not in
  # it. Its resolver (benilla-formats/src/install.rs `addon_corpus`) has one rung, the project
  # folder, so the link goes into the slot AND the primary, exactly as `WoW` does. Before this,
  # five test files each walked up to the primary's *parent* to find it; from a slot on the
  # external drive that walk names nothing, and thirty tests skipped at every land for three
  # weeks (2026-08-30 → 09-22) with libtest swallowing the skip line.
  local corpus="$(dirname "$PRIMARY")/wow-addons-vanilla"
  if [ -d "$corpus" ]; then
    for root in "$slot" "$PRIMARY"; do
      [ -e "$root/wow-addons-vanilla" ] || ln -s "$corpus" "$root/wow-addons-vanilla"
    done
  fi
  return 0
}

# Hours since an ISO-8601 UTC stamp (`%FT%TZ`, the claim-marker format); unparseable -> 0 (young).
age_hours() {
  local ts="$1" then
  then="$(date -j -u -f "%Y-%m-%dT%H:%M:%SZ" "$ts" +%s 2>/dev/null || echo 0)"
  [ "$then" -gt 0 ] || { echo 0; return; }
  echo $(( ($(date +%s) - then) / 3600 ))
}

# Hours since a file's mtime; missing file -> 0 (young — err on the safe side).
mtime_age_hours() {
  local m; m="$(stat -f %m "$1" 2>/dev/null || echo 0)"
  [ "$m" -gt 0 ] || { echo 0; return; }
  echo $(( ($(date +%s) - m) / 3600 ))
}

# Hours since the slot HEAD's tip commit — the recent-activity signal a claim's own age can't
# give: a multi-day session looks "stale" the moment it lands (clean + landed + old claim), but
# its tip was committed minutes ago. Reaping such a slot hands it to the next claimant while the
# session is still working in it — the 0435 collision (two sessions interleaving one tree).
# Missing/unreadable -> 0 (young — err on the safe side).
head_age_hours() {
  local t; t="$(git -C "$1" log -1 --format=%ct 2>/dev/null || echo 0)"
  [ "$t" -gt 0 ] || { echo 0; return; }
  echo $(( ($(date +%s) - t) / 3600 ))
}

# ── The claim heartbeat (2026-08-25) ─────────────────────────────────────────────────────────────
# `.claude/hooks/on-stop.sh` touches the owning session's marker once a TURN, so the marker's mtime
# is "when that session was last alive". Everything below reads that clock.
#
# Why it was worth adding a clock at all: the only activity signal a dead claim could be judged on
# was head_age_hours, the TIP COMMIT age — and as that function's own comment says, a live session
# looks stale the moment it lands. Being safe against the 0435 collision therefore cost 24 h on BOTH
# clocks, and that is what the director hit: a session closed after its work landed leaves a clean,
# landed slot holding NOTHING, and the next session sees nothing obviously free for a full day.
#
# A heartbeat measures the right thing directly, so the window can be minutes instead of a day — and
# the 0435 collision is impossible by construction, because a session still working there stamps the
# marker every turn.
marker_epoch() { # $1 = marker path -> the claim's creation time (field 2), or 0
  local ts; ts="$(cut -f2 "$1" 2>/dev/null || true)"
  date -j -u -f "%Y-%m-%dT%H:%M:%SZ" "$ts" +%s 2>/dev/null || echo 0
}

# **Has this marker ever been heartbeaten?** mtime meaningfully newer than the creation stamp.
#
# This is the migration safety, and it is load-bearing rather than tidy: on the day the heartbeat
# landed, three live sessions held markers whose mtime WAS their creation time, hours old. Judged on
# the idle window alone, `claim` would have reaped a slot out from under a working session — the
# exact failure the 24 h rule existed to prevent. So an un-heartbeaten marker is never idle-reapable
# and keeps the old rule; the slack absorbs filesystem timestamp granularity and the seconds between
# `set -C` creating the marker and the turn that stamps it.
heartbeat_seen() { # $1 = marker path
  local created mtime
  created="$(marker_epoch "$1")"; [ "$created" -gt 0 ] || return 1
  mtime="$(stat -f %m "$1" 2>/dev/null || echo 0)"
  [ "$mtime" -gt $((created + 60)) ]
}

# Minutes since the last heartbeat; missing/unreadable -> 0 (young — err on the safe side).
idle_minutes() { # $1 = marker path
  local m; m="$(stat -f %m "$1" 2>/dev/null || echo 0)"
  [ "$m" -gt 0 ] || { echo 0; return; }
  echo $(( ($(date +%s) - m) / 60 ))
}

# **An ABANDONED claim: safe for `claim` to take over.** Heartbeaten (so the clock means something),
# silent for $IDLE_MINS, clean, and landed — a slot whose session is gone and whose work is on main,
# holding nothing but a marker. All four, because the last two are what make taking it over lossless
# no matter how wrong the first two are.
idle_claim() { # $1 = slot
  [ -e "$1/.wt-claimed" ] || return 1
  heartbeat_seen "$1/.wt-claimed" || return 1
  [ "$(idle_minutes "$1/.wt-claimed")" -ge "$IDLE_MINS" ] || return 1
  ! slot_dirty "$1" || return 1
  landed_on_main "$1"
}

# Is the worktree's HEAD fully contained in main? (True for a landed branch OR a detached head
# parked on/behind main.) The safety predicate every reap re-checks right before acting.
landed_on_main() { git -C "$1" merge-base --is-ancestor HEAD main 2>/dev/null; }

# The dead claim NO verb can reap: stale by both clocks and clean — sweepable in every respect —
# except that its HEAD is not contained in main, so step 1 skips it and `release`'s `-d` refuses it.
# Forever, silently, holding one of eight slots.
#
# `sess/guild` sat in pool-2 like that for 103 h (2026-08-13..17, decision 1400). Its work WAS on
# main: cb65bf342, the same 33 files, the same author date, committed a day later — replayed there
# from somewhere else while this slot kept the pre-replay hashes and never went through `land`
# (which would have released it). Four days of `sweep` printed "nothing to do" over 38 G.
#
# This only ever REPORTS, and that is the whole design. Ancestry stays the delete rule: a patch-id
# match is not proof of landing — guild's own commit read `+` to `git cherry` after 532 commits of
# context drift — and a false positive here deletes work, while a false negative costs a slot. What
# was missing was never authority. It was a line saying "this one is stuck".
orphaned_claim() { # $1 = slot
  [ -e "$1/.wt-claimed" ] || return 1
  [ "$(age_hours "$(cut -f2 "$1/.wt-claimed")")" -ge "$STALE_HOURS" ] || return 1
  [ "$(head_age_hours "$1")" -ge "$STALE_HOURS" ] || return 1
  ! slot_dirty "$1" || return 1
  ! landed_on_main "$1"
}

# Can the commits main gained since this branch forked change a GATE outcome? Only if they touch
# something cargo compiles or runs. A top-level `*.md` is not: nothing `include_str!`s
# them, no test opens one, and `build.rs` watches git refs rather than files (all checked
# 2026-08-05). So a docs-only advance leaves fmt/clippy/test provably where they were, and demanding
# a re-run for it is pure double-pay — which matters because that is what main mostly IS: of 377
# commits in the three days to 08-05, **254 were docs-only**, 113 of them `reserve` stubs this very
# script pushes. Anything else — code, shaders, scripts, `.claude/` — is treated as gate-affecting;
# the set stays deliberately narrow, because the cost of being wrong here is a red gate reaching
# main and the cost of being conservative is one extra gate run.
# $1 = newline-separated paths (empty ⇒ nothing arrived ⇒ trivially safe).
docs_only() { [ -z "$(printf '%s' "$1" | grep -vE '^(docs/|[^/]*\.md$)' || true)" ]; }

# ── The platform seam tripwire (2331) ───────────────────────────────────────────────────────────
# A seam is what docs/METHOD.md "Gates" lists: a `cfg(target_os …)` (and its `windows` / `unix` /
# `target_family` spellings), a `[target.'cfg(…)']` dependency table, a `#[link]`, an
# `extern "system"`. A changed file sits behind one in two ways, and the second is the one that
# bit: the file CONTAINS a seam, or the `mod` line declaring it in its parent does. The crash
# injector was dead on Linux and Windows for six days because the seam sat in `perf/mod.rs`
# (`#[cfg(target_os = "macos")] mod stall;`), the edit in `stall.rs`, and neither the file nor
# the diff carried a `cfg` — six green macOS gate runs later, `crosscheck.sh` was the first thing
# to look. So the land worker asks this of every code land and runs the cross-check when it says
# yes; `crosscheck.sh` memoizes on the tree, so a session that already ran it pays nothing.
SEAM_RE='cfg\((not|any|all)?\(?[^)]*(target_os|target_family|windows|unix)|\[target\.|#\[link|extern "system"'
seam_touched() { # $1 = slot, $2 = branch → 0 (and the file, on stdout) when the diff touches a seam
  local slot="$1" branch="$2" f name dir parent
  for f in $(git -C "$slot" diff --name-only "main...$branch" -- '*.rs' '*.toml'); do
    [ -f "$slot/$f" ] || continue # deleted: nothing left to compile
    if grep -qE "$SEAM_RE" "$slot/$f"; then echo "$f"; return 0; fi
    case "$f" in *.rs) ;; *) continue ;; esac
    dir="$(dirname "$f")"; name="$(basename "$f" .rs)"
    if [ "$name" = mod ]; then name="$(basename "$dir")"; dir="$(dirname "$dir")"; fi
    for parent in "$dir/mod.rs" "$dir.rs" "$dir/lib.rs" "$dir/main.rs"; do
      [ -f "$slot/$parent" ] || continue
      # The `mod name;` line, and the three lines above it (where its attributes sit).
      if SEAM_RE="$SEAM_RE" awk -v m="$name" '
            { buf[NR % 4] = $0 }
            $0 ~ ("^[[:space:]]*(pub(\\([a-z]+\\))? )?mod " m ";") {
              for (i = 0; i <= 3 && i < NR; i++) if (buf[(NR - i) % 4] ~ ENVIRON["SEAM_RE"]) found = 1 }
            END { exit !found }' "$slot/$parent"; then
        echo "$f (declared behind a seam in $parent)"; return 0
      fi
    done
  done
  return 1
}

# ── Landing, one at a time, gated on the tree that lands (decision 2049) ────────────────────────
#
# THE RACE THIS ENDS. The recipe used to be sync → gates → land with nothing serialising the eight
# slots: every session gated its own tree in parallel, `land` then rebased onto whatever main had
# become, and if code had arrived during the gate it refused — so the whole chain ran again on the
# new base, with main free to move again meanwhile. Measured over the five days to 2026-09-06 from
# every session transcript on this machine: 270 lands, 190 forced re-gates, 156 rebase conflicts,
# 73 of them on the generated files alone. Main takes ~50 lands a day, the median gap
# between two lands is 18 min and 23 % of gaps are under 10 — one full gate chain. Sessions had
# started writing their own retry loops around this script.
#
# The fix is the shape every merge queue has (bors, GitHub's merge queue, Zuul): a change is gated
# on EXACTLY the tree that will land, under mutual exclusion, and the tool does the whole thing.
#
#   · The MAIN LOCK — a mkdir lock held for the seconds a rebase + fast-forward + push take — sits
#     around every write to main: every land's last metres.
#   · A branch whose changes are DOCS ONLY (`docs_only`: top-level *.md — nothing
#     any gate reads, 0979) lands at once under the main lock. No queue, no gate, seconds. Docs land
#     while somebody else's gate is running.
#   · A branch that TOUCHES CODE goes to a detached WORKER (`land_worker`, its own process session,
#     so no tool timeout, closed terminal or stopped agent kills it mid-cargo — the log is in the
#     slot's private git dir). It takes a ticket in the LAND QUEUE and waits until it holds the
#     oldest live one; then rebases onto main, regenerates the map, runs the full chain on that
#     tree (`gates.sh` — memoized, so a tree the session already gated, or one that differs from it
#     only in docs, is instant), and, under the main lock, rebases once more (only docs can have
#     arrived: the queue held every other code land), fast-forwards main, pushes, releases the
#     slot. The foreground `land` follows the log and exits with the verdict; a re-run re-attaches.
#   · The GENERATED FILES are the land's business, never the session's: a rebase conflict confined
#     to docs/MAP.md is resolved by regenerating it, and the worker regenerates
#     it once more on the rebased tree before landing — main's map is always the map of main's
#     code, and two sessions can no longer conflict on a derived file.
#
# What stays with the session: writing the code, `check.sh` per round, committing. What it no
# longer does: sync-gate-land loops, hand rebases over the map, `genmap.sh` + a map commit.
LAND_QUEUE="$PRIMARY/.git/wt-land-queue"
MAIN_LOCK="$PRIMARY/.git/wt-main.lock"
TICKET="" # this process's queue ticket, if it holds one — removed at exit whatever happens

ts() { date +%H:%M:%S; }
say() { echo "$(ts) $*"; }

# The slot's PRIVATE git dir (…/.git/worktrees/pool-N): the land log, the worker's pid and its
# verdict live there — outside the working tree, so a land in flight never makes the slot dirty.
slot_gitdir() { git -C "$1" rev-parse --absolute-git-dir; }

# Is a rebase stopped in this slot? (both backends' state dirs)
rebase_in_progress() { local g; g="$(slot_gitdir "$1")"; [ -d "$g/rebase-merge" ] || [ -d "$g/rebase-apply" ]; }

# THE GENERATED-FILE CONFLICT. docs/MAP.md is written by scripts/genmap.sh from the tree and never by
# hand — it moves on every land that changes the structure, so two sessions landing near each
# other conflicted on it with certainty, and on
# nothing else: not a conflict of intent, both sides derived the same output from different trees.
# The resolution is mechanical and this does it: at each stop whose conflicts are confined to that
# file, regenerate it from the tree AS IT STANDS at that commit (genmap reads the crates,
# never its own output, so the markers in them are not an input) and continue; a
# commit left with nothing to say — a stale regeneration and nothing else — is dropped, which is
# what git does with a commit that rebases to empty. ANY other conflicted path hands the rebase back
# untouched (still in progress, so the caller can name the files before aborting): the set is
# deliberately this one name and not "generated-looking", because the cost of resolving a real
# conflict by machine is a wrong tree on main. Returns 0 with the rebase finished, 1 otherwise.
# (Written in pool-1 by `sess/m2views` on 2026-09-05, rescued uncommitted the day after; 2049.)
GENERATED_FILES='docs/MAP.md'
resolve_generated_conflicts() { # $1 = slot
  local slot="$1" g conflicted other before after rounds=0
  g="$(slot_gitdir "$slot")"
  rebase_in_progress "$slot" || return 1 # the rebase failed outright — not a stop to resolve
  while rebase_in_progress "$slot"; do
    conflicted="$(git -C "$slot" diff --name-only --diff-filter=U)"
    [ -n "$conflicted" ] || return 1 # stopped, but not on a conflict — not ours to guess at
    other="$(printf '%s\n' "$conflicted" | grep -vxF "$GENERATED_FILES" || true)"
    [ -z "$other" ] || return 1
    (cd "$slot" && ./scripts/genmap.sh >/dev/null 2>&1) || return 1
    git -C "$slot" add -- $GENERATED_FILES || return 1
    # Progress guard: the stop must advance, or this is a loop no one asked for.
    before="$(cat "$g/rebase-merge/msgnum" 2>/dev/null || echo 0)"
    if git -C "$slot" diff --cached --quiet; then
      git -C "$slot" rebase --skip >/dev/null 2>&1 || true
    else
      GIT_EDITOR=true git -C "$slot" rebase --continue >/dev/null 2>&1 || true
    fi
    if rebase_in_progress "$slot"; then
      after="$(cat "$g/rebase-merge/msgnum" 2>/dev/null || echo 0)"
      [ "$after" -gt "$before" ] || return 1
    fi
    rounds=$((rounds + 1)); [ "$rounds" -lt 1000 ] || return 1
  done
  return 0
}

# Regenerate the map on the slot's tree and commit it if it changed. Done by the land, on the
# rebased tree, so main's docs/MAP.md always describes main's code — and so no
# session ever has to run genmap.sh and commit its output again (the doc rule, docs/METHOD.md).
regen_map() { # $1 = slot, $2 = branch
  local slot="$1"
  [ -x "$slot/scripts/genmap.sh" ] || return 0
  (cd "$slot" && ./scripts/genmap.sh >/dev/null 2>&1) || { echo "wt.sh: WARNING — genmap.sh failed; landing the map as committed" >&2; return 0; }
  if [ -n "$(git -C "$slot" status --porcelain -- $GENERATED_FILES)" ]; then
    git -C "$slot" add -- $GENERATED_FILES
    git -C "$slot" commit -q -m "map: regenerate — $2 landing" -- $GENERATED_FILES
    say "regenerated docs/MAP.md on the rebased tree (one commit)"
  fi
}

# THE MAIN LOCK. A mkdir is atomic on one filesystem; the holder's pid inside lets a crashed holder
# be recognised and its lock broken; and the wait is bounded because nothing legitimately holds it
# for more than the seconds a rebase + fast-forward + push take — the gate chain runs OUTSIDE it,
# serialised by the queue instead.
main_lock() {
  local waited=0 pid
  until mkdir "$MAIN_LOCK" 2>/dev/null; do
    pid="$(cat "$MAIN_LOCK/pid" 2>/dev/null || true)"
    if [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null; then
      rm -rf "$MAIN_LOCK"; continue # the holder died with the lock — break it
    fi
    sleep 1; waited=$((waited + 1))
    [ "$waited" -lt 120 ] || die "main has been locked for 2 minutes by pid ${pid:-?} ($MAIN_LOCK) —" \
      "nothing holds it that long. Look at that process; if it is gone, rm -rf the lock and retry."
  done
  echo $$ > "$MAIN_LOCK/pid"
}
main_unlock() { [ "$(cat "$MAIN_LOCK/pid" 2>/dev/null || true)" = "$$" ] && rm -rf "$MAIN_LOCK"; return 0; }

# THE LAND QUEUE. One ticket per code-lane land, named `<epoch>.<pid>.<name>` (content: the slot
# path). Whoever holds the OLDEST LIVE ticket may gate-and-land; everyone else waits. A ticket is
# live while its pid is a running land worker — a dead one (killed, or the machine rebooted under
# it) is pruned by the next reader, so nobody ever waits on a corpse.
ticket_pid() { local t="${1##*/}"; t="${t#*.}"; echo "${t%%.*}"; }
ticket_name() { local t="${1##*/}"; t="${t#*.}"; echo "${t#*.}"; }
ticket_alive() { local p; p="$(ticket_pid "$1")"; kill -0 "$p" 2>/dev/null && ps -o command= -p "$p" 2>/dev/null | grep -q '__land-worker'; }
queue_tickets() { ls "$LAND_QUEUE" 2>/dev/null | sort || true; } # oldest first
queue_prune() { local t; for t in $(queue_tickets); do ticket_alive "$t" || rm -f "$LAND_QUEUE/$t"; done; }
queue_enter() { # $1 = name, $2 = slot
  # The ticket is a FILE named after the session, so the name may not contain a path separator.
  # `claim` has stripped a habitual `sess/` prefix since 0520; `land`/`sync`/`release` did not, so
  # `wt.sh land sess/foo` built `.../<epoch>.<pid>.sess/foo` and the worker died on its first line
  # with "No such file or directory" — after printing that it had started, and exiting 0. All four
  # verbs strip it now; the guard below is the belt for whatever else ends up in a name.
  mkdir -p "$LAND_QUEUE"
  TICKET="$LAND_QUEUE/$(date +%s).$$.${1//\//-}"
  printf '%s\n' "$2" > "$TICKET"
}
queue_wait_turn() {
  local head last="" pos ahead
  while :; do
    queue_prune
    head="$(queue_tickets | head -1)"
    [ "$head" != "$(basename "$TICKET")" ] || return 0
    pos="$(queue_tickets | grep -nxF "$(basename "$TICKET")" | cut -d: -f1 || true)"
    if [ -z "$pos" ]; then # somebody removed the queue dir under us — re-enter
      mkdir -p "$LAND_QUEUE"; printf '%s\n' "$(cat "$TICKET" 2>/dev/null || true)" > "$TICKET"; sleep 1; continue
    fi
    if [ "$pos" != "$last" ]; then
      ahead="$(queue_tickets | head -n $((pos - 1)) | while read -r t; do ticket_name "$t"; done | tr '\n' ' ')"
      say "waiting in the land queue — position $pos (ahead: ${ahead% })"
      last="$pos"
    fi
    sleep 3
  done
}
cleanup_on_exit() { [ -z "$TICKET" ] || rm -f "$TICKET"; main_unlock; }
trap cleanup_on_exit EXIT

# Is a land worker running for this slot? $1 = the slot's git dir.
worker_alive() { local p; p="$(cat "$1/wt-land.pid" 2>/dev/null || true)"; [ -n "$p" ] && kill -0 "$p" 2>/dev/null; }

# Rebase the slot onto main. Echoes a one-line verdict. Returns 0 when what arrived cannot affect a
# gate (nothing, or docs only — 0979), 1 when code arrived (a gate verdict from before this rebase is
# stale), 2 when the rebase conflicted on something not generated (aborted; the branch is as it was,
# and the verdict names the files). A conflict confined to the generated files is resolved in place.
rebase_onto_main() { # $1 = slot, $2 = branch
  local slot="$1" branch="$2" behind arrived
  behind="$(git -C "$slot" rev-list --count "$branch..main")"
  if [ "$behind" -eq 0 ]; then
    echo "already on the latest main"
    return 0
  fi
  # Compute BEFORE the rebase — afterwards there is nothing left in the range to look at. Three-dot:
  # what MAIN changed since the fork point, not what this branch did.
  arrived="$(git -C "$slot" diff --name-only "$branch...main")"
  if ! git -C "$slot" rebase main >/dev/null 2>&1; then
    if resolve_generated_conflicts "$slot"; then
      echo "wt.sh: a conflict confined to the generated docs/MAP.md was resolved by regenerating it" >&2
    else
      local files; files="$(git -C "$slot" diff --name-only --diff-filter=U 2>/dev/null | tr '\n' ' ' || true)"
      git -C "$slot" rebase --abort 2>/dev/null || true
      echo "rebase onto main conflicts on: ${files:-(unknown)}"
      return 2
    fi
  fi
  if docs_only "$arrived"; then
    echo "rebased onto main ($behind commit(s), docs only — gate results still stand)"
    return 0
  fi
  echo "rebased onto main ($behind commit(s), touching code)"
  return 1
}

# A slot's target/ size in whole GB (0 if absent) — the sediment metric that sweep, release, and
# status all measure against $TARGET_CAP_GB.
target_gb() { local kb; kb="$(du -sk "$1/target" 2>/dev/null | cut -f1 || echo 0)"; echo $(( kb / 1024 / 1024 )); }

# Free space on the volume holding $1 (default: the POOL's root, which since 1726 is usually not
# the primary's). Walks up to the first path that exists, so it works before the root is created.
disk_free_gb() {
  local p="${1:-$POOL_DIR}"
  while [ ! -d "$p" ] && [ "$p" != "/" ]; do p="$(dirname "$p")"; done
  df -g "$p" | awk 'NR==2 {print $4}'
}

# Publish main. Every verb that WRITES main (land) calls this, so a session never has to
# remember `git push` — the 2026-07-20 audit found main 2 commits behind origin here and 11 in
# wow-re, published only because a repo-health pass happened to look (decision 0570).
#
# Deliberately NON-FATAL and never forced: the commits are already safe in the local main, so a
# push that can't happen (offline, auth, a race with another session's push) must not fail the
# land — it degrades to a loud warning, and the next verb's push carries both commits. `--no-verify`
# is NOT used and no refspec is forced: this only ever fast-forwards origin/main.
push_main() {
  if git -C "$PRIMARY" push -q origin main 2>/dev/null; then
    echo "pushed origin/main ($(git -C "$PRIMARY" rev-parse --short main))"
  else
    echo "wt.sh: WARNING — 'git push origin main' failed; main is safe locally but origin still serves the old view. Push it when you can." >&2
  fi
}

# main's unpushed gap, for status's warning line (0 when origin/main is unknown — a fresh clone
# with no upstream is not a drift to nag about).
unpushed_count() { git -C "$PRIMARY" rev-list --count origin/main..main 2>/dev/null || echo 0; }

disk_note() {
  local free; free="$(disk_free_gb)"
  if [ "$free" -lt "$DISK_FLOOR_GB" ]; then
    echo "wt.sh: DISK LOW — ${free}G free (< ${DISK_FLOOR_GB}G floor); run 'wt.sh sweep'" >&2
  fi
}

# The janitor. Idempotent, safe on a busy pool: only clean + fully-landed + old things are
# touched, each re-verified immediately before acting, and branch deletes use `-d` (refuses
# unlanded work) as the hard backstop. `--dry-run` previews.
sweep() {
  local dry=0; [ "${1:-}" = "--dry-run" ] && dry=1
  local acted=0 noted=0

  # 1 · Dead pool claims: clean + landed + claim older than $STALE_HOURS -> release the claim
  #     (target/ stays warm — that's the pool's point).
  local slot
  while IFS= read -r slot; do
    [ -e "$slot/.git" ] && [ -e "$slot/.wt-claimed" ] || continue
    local who ts age
    who="$(cut -f1 "$slot/.wt-claimed")"; ts="$(cut -f2 "$slot/.wt-claimed")"
    age="$(age_hours "$ts")"
    [ "$age" -ge "$STALE_HOURS" ] || continue
    [ "$(head_age_hours "$slot")" -ge "$STALE_HOURS" ] || continue # tip is fresh -> live session between lands
    ! slot_dirty "$slot" || continue
    if ! landed_on_main "$slot"; then
      # Dead by every clock, clean, and unreapable by construction (see orphaned_claim). Say so —
      # the four silent days this closes cost a slot and 38 G — and hand over the two commands,
      # because the choice between them needs a human looking at the branch.
      local obranch; obranch="$(git -C "$slot" symbolic-ref --short -q HEAD || echo HEAD)"
      noted=1
      echo "sweep: NOTE — $slot ($who, ${age}h dead, clean) is STUCK: $obranch is not contained in main,"
      echo "sweep:        so no verb here can reap it. Land it, or confirm its work reached main and drop it:"
      echo "sweep:          cd $slot && ./scripts/wt.sh land $who"
      echo "sweep:          git -C $slot checkout --detach main && git -C $slot branch -D $obranch && WT_ALLOW_FOREIGN=1 ./scripts/wt.sh release $who $slot"
      continue
    fi
    acted=1
    if [ "$dry" = 1 ]; then echo "would reap claim: $slot ($who, ${age}h old, clean, landed)"; continue; fi
    local branch; branch="$(git -C "$slot" symbolic-ref --short -q HEAD || true)"
    git -C "$slot" checkout -q --detach main
    if [ -n "$branch" ] && ! git -C "$slot" branch -d "$branch" >/dev/null 2>&1; then
      git -C "$slot" checkout -q "$branch" # unlanded after all — restore and leave it alone
      echo "sweep: $slot ($who) looked dead but $branch would not delete cleanly — left as is" >&2
      continue
    fi
    rm "$slot/.wt-claimed"
    echo "reaped claim: $slot ($who, ${age}h old — branch was landed; target/ stays warm)"
  done < <(all_slots)

  # 2 · Dead non-pool worktrees (agent `.claude/worktrees/*` leftovers, legacy paths): clean +
  #     landed + no git activity for $STALE_HOURS -> remove whole worktree + its landed branch.
  local wt
  while IFS= read -r wt; do
    [ -n "$wt" ] || continue
    case "$wt" in "$PRIMARY") continue ;; "$POOL_DIR"/pool-*) continue ;; esac
    ! legacy_slot "$wt" || continue
    [ -d "$wt" ] || continue
    local gitdir; gitdir="$(git -C "$wt" rev-parse --absolute-git-dir 2>/dev/null || true)"
    [ -n "$gitdir" ] || continue
    local wage; wage="$(mtime_age_hours "$gitdir/HEAD")"
    [ "$wage" -ge "$STALE_HOURS" ] || continue
    ! slot_dirty "$wt" || continue
    landed_on_main "$wt" || continue
    acted=1
    local wsize; wsize="$(du -sh "$wt" 2>/dev/null | cut -f1)"
    if [ "$dry" = 1 ]; then echo "would remove worktree: $wt ($wsize, ${wage}h idle, clean, landed)"; continue; fi
    local wbranch; wbranch="$(git -C "$wt" symbolic-ref --short -q HEAD || true)"
    git -C "$PRIMARY" worktree remove "$wt" || continue # refuses dirty — a second backstop
    [ -z "$wbranch" ] || git -C "$PRIMARY" branch -d "$wbranch" >/dev/null 2>&1 || true
    echo "removed dead worktree: $wt ($wsize freed)"
  done < <(git -C "$PRIMARY" worktree list --porcelain | awk '/^worktree /{print substr($0,10)}')

  # 3 · Sediment bound: an UNCLAIMED slot's target/ past $TARGET_CAP_GB is stale-artifact
  #     buildup (dep bumps never evict; one hit 142 GB) — reset it. The next claim of that slot
  #     pays a cold build (~5 min): rare and bounded beats unbounded. (A CLAIMED slot can't be
  #     reset here — a live build would be corrupted — so release() bounds it at session end, the
  #     deterministic idle moment sweep never gets: decision 0522.)
  while IFS= read -r slot; do
    [ -e "$slot/.git" ] && [ ! -e "$slot/.wt-claimed" ] && [ -d "$slot/target" ] || continue
    local gb; gb="$(target_gb "$slot")"
    [ "$gb" -ge "$TARGET_CAP_GB" ] || continue
    acted=1
    if [ "$dry" = 1 ]; then echo "would reset target: $slot (${gb}G > ${TARGET_CAP_GB}G cap)"; continue; fi
    rm -rf "$slot/target"
    echo "reset target: $slot (${gb}G > ${TARGET_CAP_GB}G cap — next claim builds cold)"
  done < <(all_slots)

  # 4 · Retired slots: pool-N with N >= MAX_SLOTS is unreachable from try_claim (the pool was
  #     shrunk under it, or a slot was created above the ceiling by hand — this is the backstop for
  #     both). NOTHING else reaps these — step 2 skips every pool-* path by design — so
  #     without this rule a shrink makes disk use *worse*: the slot squats forever, unclaimable and
  #     unswept, holding a full target/. Retired only when free + clean + landed, the same bar step
  #     1 uses; a claimed slot keeps its session until that session lands, then this reaps it.
  while IFS= read -r slot; do
    [ -e "$slot/.git" ] || continue
    local idx="${slot##*/pool-}"
    case "$idx" in '' | *[!0-9]*) continue ;; esac # not a numbered slot — leave it alone
    # TWO reasons a slot retires now. It sits above a lowered MAX_SLOTS (the original rule), or
    # it is in the draining legacy root (1726): try_claim can never hand either one out again, so
    # a warm target/ kept for it is pure sediment on the disk the move exists to empty.
    local why=""
    if legacy_slot "$slot"; then why="legacy root — the pool now lives at $POOL_DIR"
    elif [ "$idx" -ge "$MAX_SLOTS" ]; then why="N >= MAX_SLOTS=$MAX_SLOTS"
    else continue; fi
    [ ! -e "$slot/.wt-claimed" ] || continue # live session — it lands before it retires
    ! slot_dirty "$slot" || continue
    landed_on_main "$slot" || continue
    acted=1
    local rsize; rsize="$(du -sh "$slot" 2>/dev/null | cut -f1)"
    if [ "$dry" = 1 ]; then echo "would retire slot: $slot ($rsize, $why)"; continue; fi
    local rbranch; rbranch="$(git -C "$slot" symbolic-ref --short -q HEAD || true)"
    git -C "$PRIMARY" worktree remove "$slot" || continue # refuses dirty — a second backstop
    [ -z "$rbranch" ] || git -C "$PRIMARY" branch -d "$rbranch" >/dev/null 2>&1 || true
    echo "retired slot: $slot ($rsize freed — $why)"
  done < <(all_slots)

  # 5 · The SUM bound. Steps 3 and 4 bound each slot; nothing bounds the total, and that is not a
  #     hypothetical — this script has said so about itself since the pool was written ("nothing in
  #     this script watches the sum"). The failure it produces is worse than the disk use: status
  #     says "DISK LOW — run 'wt.sh sweep'" and sweep answers "nothing to do", because eight slots
  #     of 45 G sediment are each *under* a 60 G cap. That is the same lie decision 1400 already
  #     had to fix once at a stuck claim, in a second place.
  #
  #     So: below the floor, the cap stops being the rule. Reset unclaimed slots' target/
  #     LARGEST FIRST until free clears the floor with a little room — largest first because it
  #     buys the most disk per cold build, and stopping at the floor because a warm target/ is the
  #     pool's whole point and this should take no more than it must. A CLAIMED slot is still
  #     untouchable: a live build reads those files (2026-08-23, a slot was claimed between this
  #     sweep's dry run and its real one, and this check is why that session's cache survived).
  local free_now; free_now="$(disk_free_gb)"
  if [ "$free_now" -lt "$DISK_FLOOR_GB" ]; then
    local target_free=$(( DISK_FLOOR_GB + FLOOR_HEADROOM_GB ))
    # Unclaimed slots holding a target/, biggest first. `free_now` is tracked rather than
    # re-measured on every pass so a --dry-run previews the SAME set a real run would reset —
    # a preview that kept re-reading an unchanged disk would list the whole pool.
    while read -r gb slot; do
      [ "$free_now" -lt "$target_free" ] || break
      [ -n "$slot" ] && [ "$gb" -gt 0 ] || continue
      acted=1
      if [ "$dry" = 1 ]; then
        echo "would reset target: $slot (${gb}G — sum bound, disk under ${DISK_FLOOR_GB}G floor)"
        free_now=$(( free_now + gb ))
        continue
      fi
      rm -rf "$slot/target"
      free_now="$(disk_free_gb)"
      echo "reset target: $slot (${gb}G — sum bound, under the ${DISK_FLOOR_GB}G floor; next claim builds cold)"
    done < <(
      while IFS= read -r slot; do
        [ -e "$slot/.git" ] && [ ! -e "$slot/.wt-claimed" ] && [ -d "$slot/target" ] || continue
        echo "$(target_gb "$slot") $slot"
      done < <(all_slots) | sort -rn
    )
    # Still low with the pool wrung out? Then the sediment is somewhere sweep does not own, and
    # "nothing to do" would be the lie again. Name both places and how to clear them.
    if [ "$free_now" -lt "$DISK_FLOOR_GB" ]; then
      noted=1
      local pgb sgb
      pgb="$(target_gb "$PRIMARY")"
      sgb="$(du -sk "$SCRATCH_ROOT" 2>/dev/null | cut -f1 || echo 0)"; sgb=$(( sgb / 1024 / 1024 ))
      echo "sweep: NOTE — the pool is wrung out and disk is still under the floor. What is left is"
      echo "sweep:        not sweep's to reap:"
      echo "sweep:          primary target/   ${pgb}G  — rust-analyzer's, owned by no session."
      echo "sweep:            rm -rf $PRIMARY/target"
      echo "sweep:            for p in debug play; do mkdir -p $PRIMARY/target/\$p \\"
      echo "sweep:              && ln -s \"\$(readlink $PRIMARY/WoW)\" $PRIMARY/target/\$p/WoW; done"
      echo "sweep:            (the relink is not optional — decision 1451: target/{debug,play}/WoW is"
      echo "sweep:             the exe-dir install ladder, and a reset without it kills every wrapper run.)"
      echo "sweep:          scratchpads       ${sgb}G  — dead sessions' leftovers under $SCRATCH_ROOT."
      echo "sweep:            NOT safe to bulk-delete: a live session's directory can carry a stale"
      echo "sweep:            mtime, and one of them holds a registered git worktree. Check first."
    fi
  fi

  # "nothing to do" was the exact line four days of sweeps printed at a stuck slot — it has to stop
  # meaning "nothing is wrong" the moment a note above says otherwise (decision 1400).
  if [ "$acted" != 1 ]; then
    if [ "$noted" = 1 ]; then echo "sweep: nothing reapable — see the note(s) above"; else echo "sweep: nothing to do"; fi
  fi
  disk_note
}

try_claim() {
  local name="$1" branch="sess/$1"
  pool_root_or_die
  mkdir -p "$POOL_DIR"
  # WARMEST slot first, not lowest index (decision 1822). Index order handed a session the one
  # slot whose target/ had been reset (4.9G) while 59G sat free two indexes up — and its land-gate
  # chain then rebuilt the workspace's four artifact universes cold: a 27-minute gate run that a
  # warm slot does in ~5. Existing slots are tried by target/ size descending (a cold existing slot
  # still beats creating a new path — the worktree already exists); indices with no slot yet come
  # last, ascending. Every skip below is re-checked per candidate exactly as before, and the
  # noclobber marker still decides races.
  local order i
  order="$( { for i in $(seq 0 $((MAX_SLOTS - 1))); do
                [ -d "$POOL_DIR/pool-$i" ] && echo "$(target_gb "$POOL_DIR/pool-$i") $i"
              done | sort -rn | awk '{print $2}'
              for i in $(seq 0 $((MAX_SLOTS - 1))); do
                [ -d "$POOL_DIR/pool-$i" ] || echo "$i"
              done; } )"
  for i in $order; do
    local slot="$POOL_DIR/pool-$i"
    # While the legacy root drains (1726), pool-$i exists in BOTH roots — and the slot INDEX is an
    # identity, not just a name: docs/METHOD.md keys the live-probe login off it (pool-4 -> probe4), and
    # a vmangos login KICKS whoever holds the account. Handing out external pool-0 while internal
    # pool-0 is live would put two sessions on probe0 kicking each other mid-probe. So an index
    # still claimed in the legacy root is skipped here. Self-resolving: the last legacy slot to
    # land frees its index for good.
    if [ -e "$LEGACY_POOL_DIR/pool-$i/.wt-claimed" ] && legacy_slot "$LEGACY_POOL_DIR/pool-$i"; then
      continue
    fi
    if [ ! -d "$slot" ]; then
      # A slot path that doesn't exist yet: create it (bootstrap, or after a shrink+regrow within
      # the ceiling — the loop bound makes pool-$POOL_CEILING and up unreachable from here). First
      # build in a fresh path is cold; later claims of the same path are warm.
      git -C "$PRIMARY" worktree add --detach "$slot" main >/dev/null
    elif [ -e "$slot/.wt-claimed" ] || [ ! -e "$slot/.git" ]; then
      continue # claimed, or a non-pool directory
    elif slot_dirty "$slot"; then
      continue # unclaimed but dirty (dead session?) — never hand a session foreign edits
    fi
    # Atomic claim: noclobber create fails if another session got here first. Field 3 is the
    # claiming session's id ([`own_slot`], decision 1041) — the thing a name alone cannot say.
    if (set -C; printf '%s\t%s\t%s\n' "$name" "$(date -u +%FT%TZ)" "$(session_id)" > "$slot/.wt-claimed") 2>/dev/null; then
      git -C "$slot" checkout -q -B "$branch" main
      link_wow "$slot"
      echo "$slot" # stdout is the PATH ALONE, so `cd "$(wt.sh claim foo)"` keeps working
      # ...and stderr is how to use it, because "cd there" is human advice that an agent session
      # cannot follow: its shell cwd resets between tool calls, which makes the bare path look
      # unusable and sends it reaching for the harness's EnterWorktree tool (blocked — decision
      # 1040, .claude/hooks/guard-harness-worktree.sh). Said at the one moment it is needed.
      cat >&2 <<EOF
wt.sh: slot claimed. Work there by PATH — do not "enter" it with any other worktree tool:
wt.sh:   Bash       cd $slot && <cmd>      (each call; cwd resetting to the primary is expected)
wt.sh:   Read/Edit  $slot/crates/...       (absolute, under the slot)
wt.sh: gates, hooks and runtime asset reads all follow the slot on their own.
EOF
      return 0
    fi
  done
  return 1
}

# Release every ABANDONED claim (see idle_claim) and say which. `claim`'s first self-heal, ahead of
# the 24 h sweep, because it is the case the director actually hits: a session closed once its work
# landed, leaving a clean, landed, silent slot that no verb would touch for a day.
#
# The reap is sweep step 1's, verbatim in its safety: re-check clean+landed in-process, detach onto
# main, and delete the branch with `-d` (NEVER `-D`), restoring the branch and leaving the slot alone
# if git refuses. So the worst case of a wrong idleness verdict is a slot that stays claimed — never
# a lost commit. target/ stays warm, which is the pool's whole point.
reap_idle_claims() {
  local slot reaped=0
  while IFS= read -r slot; do
    [ -e "$slot/.git" ] || continue
    idle_claim "$slot" || continue
    local who mins branch
    who="$(cut -f1 "$slot/.wt-claimed")"; mins="$(idle_minutes "$slot/.wt-claimed")"
    branch="$(git -C "$slot" symbolic-ref --short -q HEAD || true)"
    git -C "$slot" checkout -q --detach main
    if [ -n "$branch" ] && ! git -C "$slot" branch -d "$branch" >/dev/null 2>&1; then
      git -C "$slot" checkout -q "$branch" # unlanded after all — restore and leave it alone
      continue
    fi
    rm "$slot/.wt-claimed"
    reaped=1
    echo "wt.sh: took over $slot — claim '$who' went silent ${mins}m ago, clean and landed." >&2
  done < <(all_slots)
  [ "$reaped" = 1 ]
}

claim() {
  local name="${1:-}"; name="${name#sess/}" # a habitual `claim sess/foo` must not become sess/sess/foo
  [ -n "$name" ] || die "usage: wt.sh claim <short-task-name>"
  git -C "$PRIMARY" show-ref --verify --quiet "refs/heads/sess/$name" && die "branch sess/$name already exists"

  # ONE slot per session (decision 1756). Nothing used to say so, and the sessions that took more
  # were not being greedy — a dispatched agent INHERITS its parent's session id, so an orchestrator
  # running `claim` took a second slot the pool could not tell from the first. On 2026-08-31 one
  # session held pool-2, pool-6 and pool-7 with work in only pool-2, and six live sessions read as
  # a full eight-slot pool. The marker already carries the session id; this is the one verb where
  # comparing it stops the leak at the source.
  local mine held; mine="$(session_id)"
  if [ "$mine" != "-" ] && [ "${WT_ALLOW_SECOND_SLOT:-}" != "1" ]; then
    while IFS= read -r held; do
      [ -e "$held/.wt-claimed" ] || continue
      [ "$(cut -f3 "$held/.wt-claimed" 2>/dev/null)" = "$mine" ] || continue
      die "$(printf '%s\n' \
        "this session already holds $held (claim '$(cut -f1 "$held/.wt-claimed")') — one slot per session." \
        "Work there by path; a second claim is almost never yours to make:" \
        "  - a dispatched agent shares its parent's session id AND its parent's slot — an agent" \
        "    that needs a clean tree gets isolation:worktree, never a pool claim" \
        "  - wow-re work gets a wow-re worktree via wow-re's own scripts/wt.sh new (docs/METHOD.md," \
        "    'The cross-repo RE workflow'), never a benilla slot" \
        "If this session genuinely needs a second slot, re-run with WT_ALLOW_SECOND_SLOT=1 (1756).")"
    done < <(all_slots)
  fi
  disk_note
  if ! try_claim "$name"; then
    # Self-heal, cheapest first. An ABANDONED claim (heartbeat silent, clean, landed) is the common
    # case and needs no 24 h wait; only then fall through to the full sweep (decision 0433).
    if reap_idle_claims; then
      try_claim "$name" && return 0
    fi
    echo "wt.sh: all $MAX_SLOTS slots claimed — sweeping stale ones" >&2
    sweep >&2
    # NOT "or grow MAX_SLOTS" — that closing clause was read as an instruction once, and the pool
    # grew past the ceiling the disk budget is sized for (decision 1037). The only two answers to a
    # genuinely full pool are to wait and to take one over, and the second is the director's call.
    try_claim "$name" || die "all $MAX_SLOTS slots are genuinely live — the pool does not grow." \
      "Wait for a land, or ask the director which slot to take over (\`wt.sh status\` shows claim ages)." \
      "STOP HERE. Do NOT carry on in the primary checkout: it belongs to no session, and working" \
      "there reverts other sessions' edits and blocks their land (docs/METHOD.md hard rules). No slot" \
      "means no work, and asking the director is the next step, not a fallback."
  fi
}

# The recovery verb for the ONE way a session loses its branch: `land` ends by detaching the slot
# and deleting sess/<name>, and a session that keeps working after its land is then committing onto
# a DETACHED HEAD. Those commits are real and reachable, but no verb can see them — `land` and
# `sync` both `die "$slot is detached"` — so they sit outside main until someone notices.
#
# That happened three times in one arc (2026-08-10..12). Twice it cost a hand cherry-pick; once the
# "fix" was `git push origin HEAD:main`, which advanced ORIGIN but left the PRIMARY's main ref
# behind — the ref rebase_onto_main reads — and the next session measured a phantom REGRESSION off
# the stale tree and nearly recorded it as a real one. A lost commit is cheap; a lost commit that
# comes back as a false measurement is not.
#
# `claim` is NOT the recovery, which is the trap this verb exists to close: try_claim SKIPS a dirty
# slot ("never hand a session foreign edits"), so a claim run from a detached slot with live edits
# silently hands you a DIFFERENT slot and strands the work in this one. reclaim re-attaches the slot
# you are standing in, at its current HEAD (`checkout -B` with no start point touches no file), so
# both the orphaned commits and an uncommitted tree survive.
reclaim() {
  local name="${1:-}"; name="${name#sess/}"
  local slot="${2:-$PWD}"
  [ -n "$name" ] || die "usage: wt.sh reclaim <short-task-name> [path]"
  slot="$(cd "$slot" 2>/dev/null && pwd)" || die "no such path: ${2:-$PWD}"
  [ -e "$slot/.git" ] || die "$slot is not a worktree"
  case "$slot" in
    "$POOL_DIR"/pool-*) ;;
    *) legacy_slot "$slot" || die "reclaim is for pool slots only, and $slot is not one." \
           "The primary checkout belongs to no session and is never re-attached (docs/METHOD.md)." ;;
  esac

  local cur; cur="$(git -C "$slot" symbolic-ref --short -q HEAD || true)"
  [ -z "$cur" ] || die "$slot is already on $cur — nothing to re-attach."

  # A marker held by ANOTHER session is the 1041 hazard; own_slot refuses for us. No marker at all
  # is the normal post-land state, and we write a fresh one below.
  [ ! -e "$slot/.wt-claimed" ] || own_slot "$slot" "$name"

  # -B would MOVE an existing branch, so an occupied name is refused rather than hijacked.
  ! git -C "$PRIMARY" show-ref --verify --quiet "refs/heads/sess/$name" \
    || die "branch sess/$name already exists — reclaim under a different name."

  git -C "$slot" checkout -q -B "sess/$name" # no start point: keeps the commits AND the dirty tree
  [ -e "$slot/.wt-claimed" ] || \
    (set -C; printf '%s\t%s\t%s\n' "$name" "$(date -u +%FT%TZ)" "$(session_id)" > "$slot/.wt-claimed")
  link_wow "$slot"

  local ahead; ahead="$(git -C "$slot" rev-list --count main..HEAD 2>/dev/null || echo 0)"
  echo "wt.sh: reclaimed $slot on sess/$name — $ahead commit(s) ahead of main, working tree kept." >&2
  echo "wt.sh: run the gates, then land as usual:  ./scripts/wt.sh land $name" >&2
}

release() {
  local name="${1:-}" slot="${2:-$PWD}"; name="${name#sess/}"
  slot="$(cd "$slot" && pwd)"
  [ -e "$slot/.wt-claimed" ] || die "$slot has no claim to release"
  own_slot "$slot" "$name"
  ! slot_dirty "$slot" || die "$slot is dirty — commit or discard first"
  local branch; branch="$(git -C "$slot" symbolic-ref --short -q HEAD || true)"
  git -C "$slot" checkout -q --detach main
  if [ -n "$branch" ]; then
    # -d (not -D): refuses if the branch isn't landed on main — releasing must not lose work.
    git -C "$slot" branch -d "$branch" || {
      git -C "$slot" checkout -q "$branch"
      die "$branch is not merged into main — land it first (or delete the branch yourself)"
    }
  fi
  rm "$slot/.wt-claimed"
  # Sediment bound at the session boundary (decision 0522): a claimed slot's target/ grows
  # unbounded (cargo never evicts; a session hit 101 GB, invisible to sweep — which only caps
  # UNCLAIMED slots). The slot is idle the instant the claim is gone, so this is the safe,
  # deterministic point to reset an oversized one — no live build to corrupt. Under the cap it
  # stays warm (the pool's whole point).
  local tgb; tgb="$(target_gb "$slot")"
  if [ "$tgb" -ge "$TARGET_CAP_GB" ]; then
    rm -rf "$slot/target"
    echo "released $slot (target/ was ${tgb}G > ${TARGET_CAP_GB}G cap — reset; next claim builds cold)"
  else
    echo "released $slot (target/ stays warm)"
  fi
}

# ── sync · land · the worker (decisions 0433, 0979, 2049) ─────────────────────────────────────

# `sync`: rebase onto main now. Optional since 2049 — `land` rebases and gates on its own — but a
# sync followed by a `gates.sh` run makes the land's own gate a memo hit (gates.sh's stamp, 1822),
# so the worker lands in seconds instead of holding the queue for a chain.
sync() {
  local name="${1:-}" slot="${2:-$PWD}"; name="${name#sess/}"
  slot="$(cd "$slot" && pwd)"
  [ -e "$slot/.wt-claimed" ] || die "$slot has no claim — nothing to sync"
  own_slot "$slot" "$name"
  # Clean only, like land: an autostash rebase reorders someone's in-flight edits under them, and a
  # session that has not committed yet has nothing to gate anyway.
  ! slot_dirty "$slot" || die "$slot is dirty — commit first, then sync"
  local branch; branch="$(git -C "$slot" symbolic-ref --short -q HEAD)" || die "$slot is detached"
  local verdict rc
  verdict="$(rebase_onto_main "$slot" "$branch")" && rc=0 || rc=$?
  case "$rc" in
    2) die "$verdict — rebase by hand in $slot (git rebase main; resolve; git rebase --continue), then 'wt.sh land $name'" ;;
    1) echo "$verdict — 'scripts/gates.sh' now makes 'wt.sh land $name' instant; land gates on its own either way" ;;
    *) echo "$verdict — 'wt.sh land $name' when ready" ;;
  esac
}

# The landing itself — the last metres, ALWAYS under the main lock: rebase (a generated-file
# conflict resolved, anything else refused), regenerate the map, fast-forward main from the
# primary, push. Prints what it did. Returns 0 landed; 1 refused,
# with the reason printed; 3 (code lane only) when code arrived on main since the gate — which the
# queue makes impossible unless something landed outside wt.sh — so the worker gates again.
land_now() { # $1 = slot, $2 = branch, $3 = lane (docs | code)
  local slot="$1" branch="$2" lane="$3" verdict rc
  [ -z "$(git -C "$PRIMARY" status --porcelain)" ] || { echo "the PRIMARY checkout is dirty — that violates the worktree rule; investigate before landing"; return 1; }
  verdict="$(rebase_onto_main "$slot" "$branch")" && rc=0 || rc=$?
  case "$rc" in
    2) echo "$verdict"
       echo "rebase by hand in $slot: git rebase main → resolve → git rebase --continue, then 'wt.sh land' again"
       return 1 ;;
    1) if [ "$lane" = code ]; then echo "$verdict — code reached main outside the queue; gating again"; return 3; fi ;;
  esac
  [ "$verdict" = "already on the latest main" ] || echo "$verdict"
  regen_map "$slot" "$branch"

  git -C "$PRIMARY" merge --ff-only "$branch" >/dev/null || { echo "ff-merge failed — main moved under the lock; run 'wt.sh land' again"; return 1; }
  echo "landed $branch on main ($(git -C "$PRIMARY" rev-parse --short main))"
  push_main
  return 0
}

# The detached land worker (2049): runs `__land-worker <name> <slot>` from a stable copy of this
# script in the slot's private git dir, in its own process session, with the slot's land log as its
# stdout/stderr. Its verdict is the exit code it writes to wt-land.status; the ticket it holds is
# removed by the EXIT trap however it ends, so the queue never waits on a finished worker.
#
# However the worker ends — a verdict, a `die` inside release, a signal — its EXIT trap leaves a
# status behind (the exit code; anything unexpected is a failure) and drops the pid file, so the
# follower always gets an answer; the shared cleanup drops its ticket and any lock it held.
WORKER_GD=""
worker_exit() {
  local rc=$?
  cleanup_on_exit
  if [ -n "$WORKER_GD" ]; then
    [ -e "$WORKER_GD/wt-land.status" ] || echo "${rc:-1}" > "$WORKER_GD/wt-land.status"
    rm -f "$WORKER_GD/wt-land.pid"
  fi
}
worker_finish() { echo "$2" > "$1/wt-land.status"; exit "$2"; } # $1 = gitdir, $2 = verdict
land_worker() { # $1 = name, $2 = slot
  local name="$1" slot="$2" gd branch verdict rc round
  gd="$(slot_gitdir "$slot")"; WORKER_GD="$gd"
  trap worker_exit EXIT
  echo $$ > "$gd/wt-land.pid"
  rm -f "$gd/wt-land.status"
  say "land worker $$ — $name in $slot"
  branch="$(git -C "$slot" symbolic-ref --short -q HEAD)" || { say "FAILED: $slot is detached"; worker_finish "$gd" 1; }
  queue_enter "$name" "$slot"
  queue_wait_turn
  say "my turn — no other code land moves main until this one is on it"
  rc=1
  for round in 1 2 3; do
    # 1 · Rebase onto main. From here on the queue holds every other code land back, so what this
    #     tree is now is what lands (bar docs, which cannot change a gate).
    verdict="$(rebase_onto_main "$slot" "$branch")" && rc=0 || rc=$?
    if [ "$rc" = 2 ]; then
      say "FAILED: $verdict"
      say "rebase by hand in $slot: git rebase main → resolve → git rebase --continue, then 'wt.sh land $name' again"
      worker_finish "$gd" 1
    fi
    say "$verdict"
    # 2 · The map, on the rebased tree.
    regen_map "$slot" "$branch"
    # 3 · The full chain, on the tree that will land. Memoized: a tree the session already gated
    #     (or one that differs from it only in docs) is a stamp read, not a chain (gates.sh).
    say "gates: $slot/scripts/gates.sh"
    if ! (cd "$slot" && ./scripts/gates.sh); then
      say "FAILED: the gates are red on the rebased tree — fix, commit, then 'wt.sh land $name' again"
      worker_finish "$gd" 1
    fi
    # 3b · The platform seam tripwire (2331). The chain above runs on macOS, so code behind
    #      another target's cfg is invisible to it (1920): when the landing diff touches a seam
    #      (seam_touched), the other two platforms are compiled here, on the landing tree, before
    #      main moves. A missing prerequisite (colima down, no mingw target) is a failure, as
    #      crosscheck.sh says — a platform nobody compiled is a platform nobody supports.
    local seam
    if seam="$(seam_touched "$slot" "$branch")"; then
      say "crosscheck: the diff touches a platform seam ($seam) — $slot/scripts/crosscheck.sh"
      if ! (cd "$slot" && ./scripts/crosscheck.sh); then
        say "FAILED: the cross-check is red on the rebased tree — fix (or start the prerequisite it names), commit, then 'wt.sh land $name' again"
        worker_finish "$gd" 1
      fi
    fi
    # 4 · The last metres, under the main lock.
    main_lock
    land_now "$slot" "$branch" code && rc=0 || rc=$?
    main_unlock
    case "$rc" in
      0) break ;;
      3) say "round $round: gating again"; continue ;;
      *) say "FAILED: not landed — see above"; worker_finish "$gd" 1 ;;
    esac
  done
  if [ "$rc" != 0 ]; then
    say "FAILED: code kept reaching main outside the queue (3 rounds) — something is landing without wt.sh"
    worker_finish "$gd" 1
  fi
  release "$name" "$slot"
  worker_finish "$gd" 0
}

# Follow a worker's log until its verdict, and exit with it. $1 = the slot's git dir.
land_follow() {
  local gd="$1" n=0 lines i
  for i in $(seq 1 20); do # the worker writes its pid within a second of starting
    { [ -e "$gd/wt-land.pid" ] || [ -e "$gd/wt-land.status" ]; } && break
    sleep 0.5
  done
  while :; do
    lines="$(wc -l < "$gd/wt-land.log" 2>/dev/null | tr -d ' ' || echo 0)"
    if [ "${lines:-0}" -gt "$n" ]; then sed -n "$((n + 1)),${lines}p" "$gd/wt-land.log"; n="$lines"; fi
    if [ -e "$gd/wt-land.status" ]; then
      lines="$(wc -l < "$gd/wt-land.log" 2>/dev/null | tr -d ' ' || echo 0)"
      if [ "${lines:-0}" -gt "$n" ]; then sed -n "$((n + 1)),${lines}p" "$gd/wt-land.log"; fi
      exit "$(cat "$gd/wt-land.status")"
    fi
    if ! worker_alive "$gd"; then
      sleep 1
      [ -e "$gd/wt-land.status" ] && continue
      echo "wt.sh: the land worker died without a verdict — its log: $gd/wt-land.log" >&2
      exit 1
    fi
    sleep 2
  done
}

# `land` — the one-command session ending (0433), the whole of it (2049). Two lanes, see the
# header above `LAND_QUEUE`: docs land here and now under the main lock; code goes to the worker
# and this call follows its log. Either way it ends with the slot released, or a verdict saying why
# not. Idempotent: a land already in flight for this slot is re-attached, not started twice.
land() {
  local name="${1:-}" slot="${2:-$PWD}"; name="${name#sess/}"
  slot="$(cd "$slot" && pwd)"
  [ -e "$slot/.wt-claimed" ] || die "$slot has no claim — nothing to land"
  own_slot "$slot" "$name"
  local gd; gd="$(slot_gitdir "$slot")"
  if worker_alive "$gd"; then
    echo "wt.sh: a land for $name is already in flight (worker $(cat "$gd/wt-land.pid")) — attaching to its log" >&2
    land_follow "$gd"
  fi
  if rebase_in_progress "$slot"; then
    die "a rebase is stopped in $slot — finish it (git rebase --continue) or 'git -C $slot rebase --abort', then land again"
  fi
  ! slot_dirty "$slot" || die "$slot is dirty — commit first"
  local branch; branch="$(git -C "$slot" symbolic-ref --short -q HEAD)" || die "$slot is detached"
  [ -z "$(git -C "$PRIMARY" status --porcelain)" ] || die "the PRIMARY checkout is dirty — that violates the worktree rule; investigate before landing"

  # THE DOCS LANE: nothing this branch changes is read by any gate, so there is nothing to wait for.
  if docs_only "$(git -C "$slot" diff --name-only "main...$branch")"; then
    echo "docs only — landing now (no gate can be affected, so no queue)"
    local rc
    main_lock
    land_now "$slot" "$branch" docs && rc=0 || rc=$?
    main_unlock
    [ "$rc" = 0 ] || exit 1
    release "$name" "$slot"
    return 0
  fi

  # THE CODE LANE: hand it to the worker and follow.
  : > "$gd/wt-land.log"
  rm -f "$gd/wt-land.status"
  # A STABLE COPY of this script: the worker's own rebase may rewrite scripts/wt.sh in the slot, and
  # bash reads a script as it runs it. WT_NO_REEXEC: the worker keeps this copy while main moves.
  cp "$SELF" "$gd/wt-land.sh"
  (cd "$slot" && WT_NO_REEXEC=1 perl -MPOSIX -e 'POSIX::setsid(); exec @ARGV or die "exec: $!"' -- \
      bash "$gd/wt-land.sh" __land-worker "$name" "$slot" </dev/null >>"$gd/wt-land.log" 2>&1 &)
  cat >&2 <<EOF
wt.sh: code lane — a detached worker queues, rebases, regenerates the map, runs the gates on the
wt.sh: tree that lands, fast-forwards + pushes main and releases the slot. Following its log
wt.sh: ($gd/wt-land.log); a re-run of 'wt.sh land $name' re-attaches.
EOF
  land_follow "$gd"
}

status() {
  [ -n "$(all_slots)" ] || { echo "no pool at $POOL_DIR"; return 0; }
  pool_root_ready || echo "POOL DRIVE NOT MOUNTED — $POOL_DIR is absent; claim will refuse (1726)"
  local slot
  while IFS= read -r slot; do
    [ -e "$slot/.git" ] || continue
    local who="free" dirty="" stale="" bloat="" retire="" landing="" branch tsize
    if [ -e "$slot/.wt-claimed" ]; then
      local name ts age
      name="$(cut -f1 "$slot/.wt-claimed")"; ts="$(cut -f2 "$slot/.wt-claimed")"
      age="$(age_hours "$ts")"
      who="claimed: $name (${age}h)"
      # `(yours)` on the one slot this session owns (decision 1041) — and its ABSENCE is the
      # reading that matters: a session that finds no `(yours)` anywhere is about to work in
      # somebody else's slot, which is exactly the 2026-08-06 collision seen one step earlier.
      [ "$(cut -f3 "$slot/.wt-claimed")" = "$(session_id)" ] && who="$who (yours)" || true
      if [ "$age" -ge "$STALE_HOURS" ] && [ "$(head_age_hours "$slot")" -ge "$STALE_HOURS" ] \
        && ! slot_dirty "$slot" && landed_on_main "$slot"; then
        stale=" STALE(sweepable)"
      elif idle_claim "$slot"; then
        # Abandoned: `claim` will take it over on the next full pool, without waiting for STALE_HOURS.
        stale=" IDLE($(idle_minutes "$slot/.wt-claimed")m silent, clean+landed — claim may take it)"
      elif orphaned_claim "$slot"; then
        # Same deadness, opposite outcome: sweepable in every respect but the one that matters, so
        # it needs its own word. Reading STALE and ORPHANED as the same thing is what let pool-2
        # hide — it was tagged neither (decision 1400).
        stale=" ORPHANED(dead ${age}h, not in main — no verb can reap it; 'wt.sh sweep' says how)"
      fi
    fi
    branch="$(git -C "$slot" symbolic-ref --short -q HEAD || echo detached)"
    # A FREE slot that is dirty is the silent capacity leak (2026-08-05): try_claim skips it
    # ("never hand a session foreign edits") and sweep step 1 only reaps CLAIMED slots, so a stray
    # uncommitted file strands the slot permanently — two were found holding 33 G, one of them
    # parking a revert of landed work. Unclaimable is a different state from merely dirty; say so.
    if slot_dirty "$slot"; then
      [ -e "$slot/.wt-claimed" ] && dirty=" DIRTY" || dirty=" STUCK(free+dirty: unclaimable, needs checkout/commit)"
    fi
    # A slot above the current MAX_SLOTS is on its way out; sweep step 4 takes it once it is free.
    local sidx="${slot##*/pool-}"
    case "$sidx" in '' | *[!0-9]*) ;; *) [ "$sidx" -ge "$MAX_SLOTS" ] && retire=" RETIRING(>=MAX_SLOTS=$MAX_SLOTS)" ;; esac
    # DRAINING: a slot in the old internal root (1726). It works exactly as before for the session
    # holding it — that is the whole point of keeping two roots — but try_claim will never hand it
    # out again, and sweep retires it the moment it is free. The tag is how a human sees the move
    # finishing without having to diff two paths by eye.
    legacy_slot "$slot" && retire="$retire DRAINING(legacy root; retires when free)" || true
    tsize="$(du -sh "$slot/target" 2>/dev/null | cut -f1 || echo 0)"
    # BLOATED: target/ past the cap. On a free slot sweep/next-release reaps it; on a CLAIMED slot
    # nothing can until it ends — so the tag is the early warning sweep can't give (decision 0522).
    [ "$(target_gb "$slot")" -ge "$TARGET_CAP_GB" ] && bloat=" BLOATED(>${TARGET_CAP_GB}G)"
    # LANDING: this slot's land worker is running (2049) — queued or gating; the queue below says which.
    worker_alive "$(slot_gitdir "$slot")" && landing=" LANDING" || true
    echo "$slot  $who  [$branch]$dirty$stale$retire$landing  target=$tsize$bloat"
  done < <(all_slots)
  # The land queue (2049): the oldest live ticket is the one gating; the rest wait their turn.
  queue_prune
  local t i=0 tage
  for t in $(queue_tickets); do
    i=$((i + 1)); tage=$(( ($(date +%s) - ${t%%.*}) / 60 ))
    if [ "$i" = 1 ]; then echo "land queue:  1. $(ticket_name "$t")  ($(cat "$LAND_QUEUE/$t" 2>/dev/null || true)) — landing, ${tage}m in the queue"
    else echo "             $i. $(ticket_name "$t")  ($(cat "$LAND_QUEUE/$t" 2>/dev/null || true)) — waiting, ${tage}m"; fi
  done
  if [ -d "$MAIN_LOCK" ]; then echo "main lock:   held by pid $(cat "$MAIN_LOCK/pid" 2>/dev/null || echo '?') ($MAIN_LOCK)"; fi
  # non-pool worktrees (agent runs, legacy paths) — sized, so drift is visible before it hurts
  local wt
  while IFS= read -r wt; do
    [ -n "$wt" ] || continue
    case "$wt" in "$PRIMARY") continue ;; "$POOL_DIR"/pool-*) continue ;; esac
    ! legacy_slot "$wt" || continue
    [ -d "$wt" ] || continue
    echo "non-pool: $wt  ($(du -sh "$wt" 2>/dev/null | cut -f1))"
  done < <(git -C "$PRIMARY" worktree list --porcelain | awk '/^worktree /{print substr($0,10)}')
  # The two big consumers of this disk that belong to no slot, so nothing above can show them, and
  # nothing anywhere bounds them (decision 1400): at the 2026-08-17 audit they were 55 G and 24 G,
  # against a 60 G floor. Shown, never touched. The primary's target/ is the EDITOR's build cache —
  # rust-analyzer had 22 G of it back within ten minutes of a wipe that day — so a size rule here
  # would only fight the director's editor and cost it a re-index; age is the only honest signal,
  # and a profile that idles long enough to qualify is one a human can see on this line and delete.
  if [ -d "$PRIMARY/target" ]; then
    echo "primary target: $(du -sh "$PRIMARY/target" 2>/dev/null | cut -f1)  (rust-analyzer's; belongs to no session, bounded by nothing)"
  fi
  if [ -d "$SCRATCH_ROOT" ]; then
    echo "scratchpads:    $(du -sh "$SCRATCH_ROOT" 2>/dev/null | cut -f1)  ($SCRATCH_ROOT — dead sessions' leftovers are never swept)"
  fi
  echo "disk: $(disk_free_gb)G free on the pool's volume ($POOL_DIR)"
  echo "      $(disk_free_gb "$PRIMARY")G free on the primary's ($PRIMARY)"
  # The PRIMARY's own state. It belongs to no session and must stay clean; when it isn't, every
  # session's `land` refuses — on a condition none of them can see from their own slot,
  # and which the offending session is usually long past noticing. Reported here so the step-back
  # checkpoint catches it while it is still cheap, instead of a bystander discovering it at land
  # time (the 08-05 incident behind `.claude/hooks/guard-primary-checkout.sh`).
  if [ -n "$(git -C "$PRIMARY" status --porcelain 2>/dev/null)" ]; then
    echo "⚠ PRIMARY $PRIMARY is DIRTY — it belongs to no session and blocks every land:"
    git -C "$PRIMARY" status --porcelain | sed 's/^/    /'
    echo "    a session is working there instead of in a slot; that work belongs in a claimed slot"
  fi
  local gap; gap="$(unpushed_count)"
  [ "$gap" = "0" ] || echo "⚠ main is $gap commit(s) ahead of origin/main — a push failed or a commit bypassed wt.sh; run 'git -C $PRIMARY push origin main'"
  disk_note
}

case "${1:-}" in
  claim)   shift; claim "$@" ;;
  sync)    shift; sync "$@" ;;
  land)    shift; land "$@" ;;
  __land-worker) shift; land_worker "$@" ;; # internal: what `land` detaches (see land_worker)
  reclaim) shift; reclaim "$@" ;;
  release) shift; release "$@" ;;
  sweep)   shift; sweep "$@" ;;
  status)  status ;;
  *) die "usage: wt.sh claim <name> | reclaim <name> [path] | sync <name> [path] | land <name> [path] | release <name> [path] | sweep [--dry-run] | status" ;;
esac
