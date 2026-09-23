#!/usr/bin/env bash
# vkleg — run the client on VULKAN before it ships: a Linux container, Mesa's software Vulkan
# driver (lavapipe), a virtual display, and a handful of capture scenes (decision 2344).
#
# Why it exists (2258, B390): the gates run on Metal, and Metal has no pipeline-statistics query
# set, so a render-pass shape wgpu's Vulkan validation refuses ran clean on every gate and
# aborted every Linux and Windows-on-Vulkan player at their first world frame. lavapipe offers
# `pipelineStatisticsQuery`, so the same validation runs here — and this script, pointed at the
# pre-fix commit 4404112d6, reproduces Thalyn's report verbatim ("Query 10 was started while
# query 9 was already active") in its first world frame, while main runs all four scenes clean.
#
#   scripts/vkleg.sh [<commit>]        build <commit> (default HEAD) and run the scenes
#   scripts/vkleg.sh --check [<commit>]  exit 0 if <commit>'s tree already passed, 1 if not
#
# It builds COMMITTED content (`git archive`), never a working tree: the question it answers is
# "does this commit run on Vulkan" — the one question the macOS gates cannot ask. `/eod` runs it
# on the commit the day's post covers and drafts no post over a tree that has not passed (2344).
#
# What it needs, once: colima + docker (`brew install colima docker`). It runs in its OWN colima
# profile, `vkleg` (6 CPU, 14 GiB), started on demand and stopped afterwards: a debug client on
# a software rasterizer holds ~2 GB resident and the build wants more, which the default
# profile's 8 GiB — shared with the local servers — cannot spare, and resizing that profile would restart the servers. The
# docker context is put back to what it was. VKLEG_KEEP=1 leaves the VM up for a second run.
#
# A failure is: a scene exits non-zero, writes no PNG, or logs `wgpu error` / `panicked at`.
# Memoized on the commit's tree (a docs-only delta keeps the verdict, as in gates.sh), in
# ~/.benilla-vkleg/green — outside every checkout, so a slot's verdict serves the primary's
# identical tree. VKLEG_FORCE=1 re-runs.
set -uo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(git -C "$here" rev-parse --show-toplevel 2>/dev/null || echo "$here")"
cd "$root" || exit 1

check_only=0
if [ "${1:-}" = "--check" ]; then check_only=1; shift; fi
rev="${1:-HEAD}"
commit="$(git rev-parse --verify -q "$rev^{commit}")" || { echo "vkleg: no such commit: $rev"; exit 2; }
tree="$(git rev-parse "$commit^{tree}")"

HOME_DIR="$HOME/.benilla-vkleg"
GREEN="$HOME_DIR/green"
PROFILE=vkleg
CTX="colima-$PROFILE"
BUILD_IMAGE=rust:1-bookworm
RUN_IMAGE=debian:trixie
SCENES=(${VKLEG_SCENES:-glue-login ui-unitframes water-night inn-interior})

# ── The memo ─────────────────────────────────────────────────────────────────────────────────
docs_only_delta() { # $1 $2 = trees
    [ -z "$(git diff-tree -r --name-only "$1" "$2" 2>/dev/null | grep -vE '^(docs/|[^/]*\.md$)' || true)" ]
}
passed() {
    [ -f "$GREEN" ] || return 1
    local t
    while read -r t _; do
        [ -n "$t" ] || continue
        if [ "$t" = "$tree" ]; then return 0; fi
        git cat-file -e "$t^{tree}" 2>/dev/null && docs_only_delta "$t" "$tree" && return 0
    done < "$GREEN"
    return 1
}
if [ "$check_only" = 1 ]; then
    if passed; then echo "vkleg: $(git rev-parse --short "$commit") — its tree passed"; exit 0; fi
    echo "vkleg: $(git rev-parse --short "$commit") — its tree has not passed"; exit 1
fi
if [ "${VKLEG_FORCE:-}" != "1" ] && passed; then
    echo "VKLEG GREEN ($(git rev-parse --short "$commit") — memoized; VKLEG_FORCE=1 re-runs)"
    exit 0
fi

# ── Prerequisites: a missing one is a FAILURE, never a skip (crosscheck.sh's rule) ──────────
command -v colima >/dev/null || { echo "VKLEG FAILED: no colima (brew install colima docker)"; exit 1; }
command -v docker >/dev/null || { echo "VKLEG FAILED: no docker CLI"; exit 1; }
data="${WOW_DATA:-}"
if [ -z "$data" ]; then
    install="$(cd "$root/WoW" 2>/dev/null && pwd -P)" || { echo "VKLEG FAILED: no WoW install link at $root/WoW (or set WOW_DATA to its Data folder)"; exit 1; }
    data="$install/Data"
fi
[ -d "$data" ] || { echo "VKLEG FAILED: $data is not a directory"; exit 1; }
case "$data" in "$HOME"/*) ;; *) echo "VKLEG FAILED: $data is outside \$HOME, which is all colima mounts"; exit 1 ;; esac
version="$(rustc -V | awk '{print $2}')"
[ -n "$version" ] || { echo "VKLEG FAILED: cannot read this machine's rustc version"; exit 1; }

# ── The VM, on demand ────────────────────────────────────────────────────────────────────────
prev_ctx="$(docker context show 2>/dev/null || true)"
started=0
if ! colima status "$PROFILE" >/dev/null 2>&1; then
    echo "vkleg: starting colima profile '$PROFILE'"
    colima start "$PROFILE" --cpu 6 --memory 14 --disk 80 --vm-type vz --mount-type virtiofs \
        >/dev/null 2>&1 || { echo "VKLEG FAILED: colima start $PROFILE"; exit 1; }
    started=1
fi
# `colima start` switches the docker context to the new profile; put the caller's back.
[ -n "$prev_ctx" ] && docker context use "$prev_ctx" >/dev/null 2>&1
finish() {
    if [ "$started" = 1 ] && [ "${VKLEG_KEEP:-}" != "1" ]; then
        colima stop "$PROFILE" >/dev/null 2>&1 || true
    fi
    [ -n "$prev_ctx" ] && docker context use "$prev_ctx" >/dev/null 2>&1
}
trap finish EXIT

# ── Stage the commit (committed content only) ────────────────────────────────────────────────
stage="$HOME_DIR/src"
out="$HOME_DIR/out"
rm -rf "$stage" "$out" && mkdir -p "$stage" "$out" || { echo "VKLEG FAILED: cannot stage in $HOME_DIR"; exit 1; }
git archive "$commit" | tar -x -C "$stage" || { echo "VKLEG FAILED: git archive $commit"; exit 1; }
echo "vkleg: $(git rev-parse --short "$commit") on lavapipe — scenes: ${SCENES[*]}"

for v in target cargo rustup; do docker --context "$CTX" volume create "benilla-vkleg-$v" >/dev/null; done
log="$out/vkleg.log"
# Two containers: BUILD on bookworm (the rust image; the target volume stays warm across runs),
# RUN on trixie. The run image is the driver: bookworm's Mesa 22.3.6 lavapipe grows the client by
# ~14 MB a frame until the OOM killer takes it (7.3 GB after 450 frames of ui-unitframes, 13.6 GB
# and killed on water-night); the same binary under trixie's Mesa 25.0.7 holds flat at 1.9 GB and
# runs twice as fast. Metal holds the same scene at 1.3 GB, so the growth was never ours (2344).
# A bookworm-built binary runs on trixie (older glibc, newer loader).
{
docker --context "$CTX" run --rm \
    -v "$stage":/src:ro \
    -v benilla-vkleg-target:/xtarget \
    -v benilla-vkleg-cargo:/usr/local/cargo/registry \
    -v benilla-vkleg-rustup:/usr/local/rustup \
    -w /src -e CARGO_TARGET_DIR=/xtarget -e CARGO_BUILD_JOBS=6 \
    -e CARGO_PROFILE_DEV_DEBUG=0 -e RUSTUP_TOOLCHAIN="$version" \
    "$BUILD_IMAGE" bash -c '
        set -uo pipefail
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq >/dev/null 2>&1
        apt-get install -y -qq pkg-config libasound2-dev libudev-dev libx11-dev libxkbcommon-dev \
            libwayland-dev >/dev/null 2>&1 || { echo "apt failed"; exit 1; }
        rustup toolchain install --profile minimal "$RUSTUP_TOOLCHAIN" >/dev/null 2>&1
        # Debug info off: the full-debuginfo benilla-app compile was OOM-killed at 8 GiB.
        cargo build -p benilla 2>&1 | grep -v -E "^\s*(Compiling|Downloaded|Downloading)" | tail -5
        test -x /xtarget/debug/benilla || { echo "BUILD FAILED"; exit 1; }
    ' || { echo "BUILD FAILED"; exit 1; }
docker --context "$CTX" run --rm \
    -v "$stage":/src:ro -v "$data":/wowdata:ro -v "$out":/out \
    -v benilla-vkleg-target:/xtarget:ro \
    -w /src -e SCENES="${SCENES[*]}" \
    "$RUN_IMAGE" bash -c '
        set -uo pipefail
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq >/dev/null 2>&1
        apt-get install -y -qq xvfb xauth mesa-vulkan-drivers libvulkan1 libxcursor1 libxrandr2 \
            libxi6 libxkbcommon-x11-0 libasound2t64 libudev1 procps >/dev/null 2>&1 \
            || { echo "apt failed"; exit 1; }
        cp /xtarget/debug/benilla /tmp/benilla || { echo "no binary"; exit 1; }
        fail=0
        for s in $SCENES; do
            echo "== scene $s"
            ( WOW_DATA=/wowdata WOW_CAPTURE="$s" WOW_CAPTURE_OUT="/out/$s.png" \
                WOW_CAPTURE_DEADLINE=900 WOW_NOSOUND=1 WOW_UNATTENDED=1 WGPU_BACKEND=vulkan \
                timeout 1200 xvfb-run -a -s "-screen 0 1280x800x24" /tmp/benilla > "/out/$s.log" 2>&1
              echo $? > /tmp/rc ) &
            # Resident memory, sampled: on a software rasterizer the GPU memory is RAM too, and a
            # scene that outgrows the VM dies by the OOM killer (exit 137) — say so with a number.
            peak=0; rm -f /tmp/rc; : > "/out/$s.rss"
            while [ ! -f /tmp/rc ]; do
                sleep 2
                r=$(ps -C benilla -o rss= 2>/dev/null | head -1 | tr -d " ")
                [ -n "$r" ] && echo "$r" >> "/out/$s.rss" && [ "$r" -gt "$peak" ] && peak=$r
            done
            rc=$(cat /tmp/rc)
            echo "   peak resident $((peak / 1024)) MB"
            if [ "$rc" != 0 ]; then echo "   FAILED: exit $rc"; fail=1; fi
            if [ ! -s "/out/$s.png" ]; then echo "   FAILED: no image"; fail=1; fi
            if grep -q -E "wgpu error|panicked at" "/out/$s.log"; then
                echo "   FAILED:"; grep -A8 -E "wgpu error|panicked at" "/out/$s.log" | head -14 | sed "s/^/     /"
                fail=1
            fi
            [ "$rc" = 0 ] && echo "   ok"
        done
        exit $fail
    '
} 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}

if [ "$rc" = 0 ]; then
    mkdir -p "$HOME_DIR" && printf '%s %s %s\n' "$tree" "$(git rev-parse --short "$commit")" "$(date +%F)" >> "$GREEN"
    echo
    echo "VKLEG GREEN ($(git rev-parse --short "$commit"); logs and images in $out)"
    exit 0
fi
echo
echo "VKLEG RED ($(git rev-parse --short "$commit"); per-scene logs in $out)"
exit 1
