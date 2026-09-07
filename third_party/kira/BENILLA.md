# benilla's `kira` fork — what differs from upstream, and how to check

Upstream: [`tesselode/kira`](https://github.com/tesselode/kira), MIT OR Apache-2.0, version
`0.12.1` — the version the workspace lock resolves to. Wired in through `[patch.crates-io]` in
the workspace root, the way `lua-src` is. The crate is copied from the cargo registry as
published (`src/`, `Cargo.toml`, `README.md`) plus the two licence files from the repository;
upstream files are CRLF and are left so, except the one file patched.

## Why a fork exists at all

The mixer runs on benilla's own audio render thread (`sound/output`), and with a raid buffing
itself — a dozen live voices, most of them spatial — that thread cost **~1.5 ms of every 60 Hz
frame** on the M2 Air (decision 1945: a sampled profile of the crowd rig's Blackrock leg with
sound on, `Track::process` two thirds of it). Upstream's sub-track `process` spatializes **every
frame**: two vector normalizes, a length, the attenuation curve and a dB→amplitude `powf` per
frame per spatial track, then two more `powf`s per frame for the volume tween and the fade. At
48 kHz × 12 tracks that is ~1.7 M `powf`s a second plus the vector math, for gains that change
imperceptibly across a 10 ms chunk. FMOD 3 — the mixer the real client delegates to — updates
pan and attenuation per mixer update, not per sample.

## The one patch — `src/track/sub.rs`

`Track::process` evaluates the spatial gains (attenuation, left/right ear gain, the mono fold)
and the volume × fade amplitude at the chunk's two ends and interpolates them linearly across
the chunk. `SpatialData::spatial_gains` is `spatialize` factored into a value; `spatialize`
itself is kept, unused, so the factoring can be diffed against it. A chunk is the output
device's buffer — 512 frames, ~10.7 ms, `sound/output::DEVICE_BUFFER_FRAMES`.

What changes audibly: nothing a listener can name. A source crossing an ear's axis within one
chunk pans linearly in gain over 10 ms instead of following the dot product per sample; a
volume tween ramps linearly in amplitude across each chunk instead of in decibels. Both are
below the granularity the reference itself updates at.

## How to check

`git diff --no-index <registry>/kira-0.12.1/src/track/sub.rs third_party/kira/src/track/sub.rs`
(after `sed 's/\r$//'` on the upstream copy) shows the patch and nothing else; every other file
is byte-identical to the registry crate. The mixer's own tests (`cargo test -p benilla-app
sound::`) run against the fork.
