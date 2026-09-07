# Syncing upstream benilla

Upstream is [samwhosung/benilla](https://github.com/samwhosung/benilla). It is developed in a
private tree and published as squashed snapshots; issues and pull requests there are closed; it
runs no CI and never builds the wasm target. This fork tracks it: `main` is upstream's head plus
our carries, and the intent is to stay current while our own work keeps living alongside.

## The model

- `main` is the only long-lived branch. Upstream is merged **into** it as an ordinary merge
  commit; our carries (the browser port, the bridge, the realm service, the wasm-only fixes) are
  ordinary commits on it. Nothing is rebased onto upstream, and nothing waits on a side branch
  "until upstream fixes it": a fix we need ships from `main`, and if upstream later ships its
  own, the carry is dropped in the sync that brings it.
- A carry is anything that makes `git diff upstream/main main -- crates` non-empty outside our
  own crates. Keep every carry `#[cfg]`-gated or in a new file. The conflict cost of a sync is
  proportional to how many upstream lines our carries touch, so touch few.
- Upstream cannot fix a wasm-only problem for us, because it does not build wasm. Such fixes are
  permanent carries: the zone soundscape loading off the frame, the AudioContext resume on the
  pages, the mixer's per-target backend. Drop one only when upstream ships an equivalent — as the
  2026-09-07 sync did to half of one: decision 1920 gave upstream's owned device layer a cpal
  half, so Linux and Windows now run it and the backend split narrowed from macOS-vs-rest to
  native-vs-wasm32. Narrowing a carry onto upstream's new code is the goal; keeping the old shape
  because it still compiles is how a carry rots.

## Procedure

```bash
git fetch upstream                          # remote "upstream" = https://github.com/samwhosung/benilla
git log --oneline main..upstream/main       # what is new; empty means there is nothing to sync
git switch -c sync-upstream main
git merge upstream/main                     # a real merge; conflicts are expected in the carry files
```

Resolve each conflict by keeping our structure and taking upstream's logic inside it: the
`dispatch()` extraction in `net/io.rs` carries upstream's exact match arms; the mixer keeps the
per-target split and takes upstream's macOS arm as is. Then verify:

```bash
cargo check --workspace --all-targets
cargo check --target wasm32-unknown-unknown -p wenilla --no-default-features --features webgpu
cargo test -p wenilla-realm
scripts/web-build.sh          # then boot it in a WebGPU browser: login, world entry, sound, cross a zone line
```

**The browser boot is not optional, and it is not a formality.** `std::time::Instant::now` and
`SystemTime::now` COMPILE on wasm32 and panic when called, so a sync that adds them passes
`cargo check --target wasm32` *and* the whole test suite and still dies at boot with
`RuntimeError: unreachable` and a blank canvas. Nothing but a browser catches it. Upstream does
not build wasm, so every sync tends to bring a fresh crop: the 2026-09-07 one brought seven, two
of them on per-frame systems that aborted the client in its first seconds. Before booting, this
sweep costs nothing and finds most of them:

```bash
# every file the sync added or touched that reaches a clock through std rather than the carry
git diff --stat main..HEAD --name-only -- '*.rs' | xargs grep -ln 'use std::time::.*Instant\|std::time::\(Instant\|SystemTime\)::now'
```

`Duration` is fine — it is arithmetic, not a clock. `Instant` and `SystemTime` are not; they take
`bevy::platform::time::Instant` and `web_time::SystemTime`. Watch for the brace form
(`use std::time::{Duration, Instant}`), which a grep for the plain import misses.

Open the pull request against `Arnesen/wenilla` (`gh pr create -R Arnesen/wenilla …`) with three
sections: *what comes in*, *conflicts and how each was resolved*, *verification*. PR #12 is the
model, PR #21 the one that added the sweep above. `check.yml` runs the wasm check and the realm
tests on it — neither of which can see a wasm time panic, which is the whole point.

**Merge it with a merge commit, not squash.** A squash turns the merge back into a single-parent
content copy, and git forgets that upstream was merged (see below). The pin bot then moves prod's
pin to the merge; wenilla-realm/docs/RELEASE.md takes it from there.

## Where the conflicts will be

The list is generated, not remembered:

```bash
git diff --name-only $(git merge-base main upstream/main) main -- crates | grep -v '^crates/wenilla'
```

The recurring ones and the rule for each:

| file | our carry | resolution |
|---|---|---|
| `benilla-app/src/net/io.rs` | `dispatch()` extracted; native/wasm split around the spawn | keep the split, take upstream's arms |
| `benilla-app/src/net.rs` | `bevy::platform::time::Instant` (std's panics on wasm) | keep ours |
| `benilla-protocol/…/world/session.rs` | `recv_async().await` | keep ours, take upstream's new fields |
| `benilla-app/src/sound/mixer.rs`, `sound/mod.rs` | kira backend per target: upstream's `OutputBackend` natively, kira's own cpal backend (Web Audio) on wasm32 | keep the split |
| `benilla-app/src/sound/zone.rs`, `sound/web_load.rs` | soundscape loading off the frame on wasm | keep ours |
| `benilla-app/src/cvars.rs` | `apply_query_overrides` (wasm-only) | follow upstream's `REGISTERED` shape |
| `benilla-app/src/bindings.rs` | `BindKey::Synth`, the bridge's synthetic latch | keep ours |
| `benilla-app/src/lib.rs`, `benilla-app/Cargo.toml` | plugin registration, wasm-only deps | keep ours plus upstream's additions |

## Why a real merge matters

Git resolves a merge against the last common ancestor. PR #7 was a real merge: `c3d8c1a0` has
upstream `6356bc8` as a parent. PR #12's sync commit `a9180db7` was a single-parent content copy,
so git's merge base stayed at `6356bc8`, and the next `git merge upstream/main` would have
replayed those 23 commits and re-raised every conflict #12 resolved by hand. PR #18 repaired it
with an empty merge, `git merge -s ours upstream/main`: the tree unchanged, upstream's `a8c9bc37`
recorded as a parent. That is also the tool if it ever happens again.

## When upstream rewrites its history

Upstream publishes squashed snapshots, and may one day force-push a new root. Then
`git merge upstream/main` reports unrelated histories. Do not pass `--allow-unrelated-histories`;
with an empty base every file conflicts. Graft the new history onto the commit we last merged,
merge, then drop the graft. The merge commit still records upstream's real sha as its parent.

```bash
old=$(git rev-parse <the upstream sha the last sync merged>)   # second parent of the last sync merge
root=$(git rev-list --max-parents=0 upstream/main | head -1)   # the new snapshot's root commit
git replace --graft "$root" "$old"      # locally: the new root now descends from what we merged
git merge upstream/main                 # a normal three-way merge against $old
git replace -d "$root"                  # the graft was only for the merge computation
```

## Is upstream's fix already in?

`git merge-base --is-ancestor` answers only for real merges. For a file, compare the trees:
`git diff upstream/main main -- path/to/file`. Empty means identical; a diff consisting only of
our `#[cfg]` blocks means current, plus our carry.
