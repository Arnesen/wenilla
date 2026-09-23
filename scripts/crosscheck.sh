#!/usr/bin/env bash
# crosscheck — compile benilla for the platforms the gates cannot see.
#
# Why it exists (decision 1920, bug B356): `scripts/gates.sh` runs on the developer's Mac, so a
# file behind `cfg(not(target_os = "macos"))` is not merely unverified — it is INVISIBLE. Decision
# 1857 shipped a non-macOS audio device layer that opened nothing, every Linux and Windows build
# went silent in the 09-02 sync, and nothing in this repo could have said so, because nothing here
# had ever compiled `benilla-app` for another target. This is that missing compile.
#
#   scripts/crosscheck.sh [linux|windows|all]     (default: all)
#
# Run it whenever platform-conditional code changes — a `cfg(target_os)`, a `[target.'cfg(…)']`
# dependency, a `#[link]`, an `extern "system"`. It is NOT part of `gates.sh`: it costs a container
# and a second dependency graph, and most commits touch no platform seam. It is a land-time check
# for the ones that do.
#
# Scope is `-p benilla-app --lib`, deliberately: that is the crate every platform seam lives in and
# the one players run, and checking it pulls benilla-world/-assets/-formats/-ui/-protocol with it.
#
# What each arm needs, once:
#   linux    docker (colima is fine) — the container installs its own ALSA/udev headers.
#            The source is rsynced to ~/.benilla-crosscheck/src first: a worktree on an external
#            volume is not visible inside the VM, and $HOME is. The container is pinned to THIS
#            machine's rustc version, not to `rust-toolchain.toml`'s `stable`: the image's stable
#            ran ahead of the host's on the first run and reported eight `chunks_exact_to_as_chunks`
#            findings across four crates — a newer clippy's opinion, nothing to do with Linux. The
#            question this script asks is "does OUR toolchain build this for another platform".
#   windows  rustup target add x86_64-pc-windows-gnu   +   brew install mingw-w64
#            (blake3 assembles for the target, so `cargo check` needs the cross assembler even
#            though it never links. The *-gnu target is used rather than *-msvc for the same
#            reason: `ml64.exe` has no substitute here, and nothing we compile differs between
#            the two ABIs at the type level.)
#
# A missing prerequisite is a FAILURE, not a skip. The whole point is that a platform nobody
# compiled is a platform nobody supports; a green line for an arm that never ran would recreate
# exactly the hole this script closes.
set -uo pipefail

arm="${1:-all}"
here="$(cd "$(dirname "$0")/.." && pwd)"
root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$root" ] && [ -f "$root/scripts/crosscheck.sh" ] || root="$here"
cd "$root" || exit 1
echo "cross-checking: $root"

# ── Green-stamp memoization (decision 2331; the shape of gates.sh's, 1822) ─────────────────────
# `wt.sh land` runs this script on the landing tree whenever the diff touches a platform seam, and
# a session that already ran it on the same tree should not pay the container twice. The key is
# what decided the verdict: the WORKING TREE's true content hash (a throwaway index, so `target/`
# and the install links stay ignored), the arm, this script, and rustc. CROSSCHECK_FORCE=1 re-runs.
stamp="target/.crosscheck-green"
tree_key() {
    local tmpidx t
    tmpidx="$(mktemp -t benilla-xcheck-idx)" && rm -f "$tmpidx" || return 1
    t="$( (export GIT_INDEX_FILE="$tmpidx"
           git read-tree HEAD && git add -A . && git write-tree) 2>/dev/null )"
    rm -f "$tmpidx"
    [ -n "$t" ] || return 1
    printf '%s|%s|%s|%s' "$t" "$arm" "$(rustc -V 2>/dev/null)" \
        "$(shasum "$root/scripts/crosscheck.sh" 2>/dev/null | cut -d' ' -f1)"
}
# A docs-only delta keeps the verdict, as in gates.sh (2049): a record or a stub landing on the
# stamped tree changes nothing any compiler reads.
docs_only_delta() { # $1 = stamped tree, $2 = current tree
    git cat-file -e "$1^{tree}" 2>/dev/null && git cat-file -e "$2^{tree}" 2>/dev/null || return 1
    [ -z "$(git diff-tree -r --name-only "$1" "$2" 2>/dev/null | grep -vE '^(docs/|[^/]*\.md$)' || true)" ]
}
key_start="$(tree_key 2>/dev/null || true)"
if [ "${CROSSCHECK_FORCE:-}" != "1" ] && [ -n "$key_start" ] && [ -f "$stamp" ]; then
    old="$(cat "$stamp" 2>/dev/null)"
    memo=""
    if [ "$old" = "$key_start" ]; then
        memo="this exact tree already passed"
    elif [ "${old#*|}" = "${key_start#*|}" ] && docs_only_delta "${old%%|*}" "${key_start%%|*}"; then
        memo="this tree differs from one that passed only in top-level *.md"
    fi
    if [ -n "$memo" ]; then
        echo "CROSS-CHECK GREEN ($arm — memoized: $memo; CROSSCHECK_FORCE=1 re-runs)"
        exit 0
    fi
fi

WIN_TARGET=x86_64-pc-windows-gnu
LINUX_IMAGE=rust:1-bookworm
STAGE="$HOME/.benilla-crosscheck/src"

failed=0
note() { printf '\ncrosscheck: %s\n' "$*"; }
fail() { printf '\nCROSSCHECK FAILED: %s\n' "$*"; failed=1; }

run_windows() {
    note "windows — cargo clippy --target $WIN_TARGET"
    if ! rustup target list --installed 2>/dev/null | grep -qx "$WIN_TARGET"; then
        fail "windows: target $WIN_TARGET is not installed (rustup target add $WIN_TARGET)"
        return
    fi
    if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
        fail "windows: no x86_64-w64-mingw32-gcc (brew install mingw-w64) — blake3 needs it"
        return
    fi
    if cargo clippy -p benilla-app --lib --target "$WIN_TARGET" -- -D warnings; then
        note "windows OK"
    else
        fail "windows: clippy -D warnings"
    fi
}

run_linux() {
    note "linux — cargo clippy in $LINUX_IMAGE"
    if ! docker info >/dev/null 2>&1; then
        fail "linux: docker is not running (colima start)"
        return
    fi
    mkdir -p "$STAGE" || { fail "linux: cannot create $STAGE"; return; }
    # Anchored excludes ('/target/', not 'target/'): an unanchored pattern also matches
    # `crates/benilla-app/src/target/`, and the missing module then reports as four unrelated
    # type errors deep in bevy. Cost the first run of this script twenty minutes.
    if ! rsync -a --delete \
        --exclude '/target/' --exclude '/.git' --exclude '/WoW' --exclude '/WoW-era' \
        --exclude '/reference/' "$root"/ "$STAGE"/; then
        fail "linux: rsync to $STAGE"
        return
    fi
    local version
    version="$(rustc -V | awk '{print $2}')"
    [ -n "$version" ] || { fail "linux: could not read this machine's rustc version"; return; }
    note "linux — pinned to rustc $version (this machine's)"
    docker volume create benilla-xcheck-target >/dev/null 2>&1
    docker volume create benilla-xcheck-cargo >/dev/null 2>&1
    docker volume create benilla-xcheck-rustup >/dev/null 2>&1
    if docker run --rm \
        -v "$STAGE":/src \
        -v benilla-xcheck-target:/xtarget \
        -v benilla-xcheck-cargo:/usr/local/cargo/registry \
        -v benilla-xcheck-rustup:/usr/local/rustup \
        -w /src -e CARGO_TARGET_DIR=/xtarget -e CARGO_BUILD_JOBS="${CROSSCHECK_JOBS:-4}" \
        -e RUSTUP_TOOLCHAIN="$version" \
        "$LINUX_IMAGE" bash -c '
            set -euo pipefail
            export DEBIAN_FRONTEND=noninteractive
            apt-get update -qq >/dev/null 2>&1
            apt-get install -y -qq pkg-config libasound2-dev libudev-dev >/dev/null 2>&1
            rustup toolchain install -c clippy --profile minimal "$RUSTUP_TOOLCHAIN"
            rustc -V
            cargo clippy -p benilla-app --lib -- -D warnings
        '; then
        note "linux OK"
    else
        fail "linux: clippy -D warnings"
    fi
}

case "$arm" in
    linux) run_linux ;;
    windows) run_windows ;;
    all) run_windows; run_linux ;;
    *) echo "usage: $0 [linux|windows|all]"; exit 2 ;;
esac

if [ "$failed" -eq 0 ]; then
    # Stamp only a tree nobody edited under the run (gates.sh's rule): a changed key is no stamp.
    key_end="$(tree_key 2>/dev/null || true)"
    if [ -n "$key_end" ] && [ "$key_end" = "$key_start" ]; then
        mkdir -p target && printf '%s\n' "$key_end" > "$stamp" 2>/dev/null || true
    fi
    echo
    echo "CROSS-CHECK GREEN ($arm)"
    echo "  (compiling is not sounding: WOW_AUDIO_LIVE=1 cargo test -p benilla-app --lib sound::output::"
    echo "   runs the output layer against the machine's real device — decision 1920)"
    exit 0
fi
echo
echo "CROSS-CHECK RED"
exit 1
