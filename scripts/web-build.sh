#!/usr/bin/env bash
#
# web-build.sh - build the browser client into web/dist/ (index.html + wasm + glue + .br/.gz).
#
#   scripts/web-build.sh            # WebGPU backend (the world needs it: storage buffers)
#   WEB_BACKEND=webgl2 scripts/web-build.sh   # WebGL2: every browser, glue screens only
#   WEB_DEBUG=1 scripts/web-build.sh          # keep the wasm name section (symbolic stack traces)
#
# Then serve web/dist with benilla-webhost:
#   cargo run --release -p benilla-webhost -- --www web/dist --data /path/to/WoW/Data
#
# Prerequisites: scripts/web-setup.sh (rustup target, matching wasm-bindgen-cli, wasi-sdk).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

DIST=web/dist
BACKEND="${WEB_BACKEND:-webgpu}"
WASM=target/wasm32-unknown-unknown/release/benilla_web.wasm
export WASI_SDK="${WASI_SDK:-$(pwd)/tools/wasi-sdk}"

command -v wasm-bindgen >/dev/null || { echo "wasm-bindgen not found — run scripts/web-setup.sh" >&2; exit 1; }
[ -d "${WASI_SDK}" ] || { echo "wasi-sdk not at ${WASI_SDK} — run scripts/web-setup.sh (or set WASI_SDK)" >&2; exit 1; }

cargo build --release --target wasm32-unknown-unknown -p benilla-web --no-default-features --features "${BACKEND}"

mkdir -p "${DIST}"
# The name section is half the file (~170 MB -> ~90 MB) and only feeds stack-trace symbols.
strip=(--remove-name-section --remove-producers-section)
[ "${WEB_DEBUG:-0}" = 1 ] && strip=()
wasm-bindgen --target web --no-typescript "${strip[@]}" --out-dir "${DIST}" "${WASM}"
cp web/index.html web/wasi_stubs.js "${DIST}/"

# Precompressed siblings for benilla-webhost's precompressed_br()/gzip(): ~90 MB of wasm goes
# over the wire as ~15 MB without per-request CPU. brotli -q 5 is the speed/size knee.
for f in "${DIST}"/*.wasm "${DIST}"/*.js; do
  command -v brotli >/dev/null && brotli -f -q 5 "$f" -o "$f.br"
  command -v gzip >/dev/null && gzip -kf -6 "$f"
done
if command -v wasm-opt >/dev/null; then
  wasm-opt -Os "${DIST}/benilla_web_bg.wasm" -o "${DIST}/benilla_web_bg.wasm.opt" && mv "${DIST}/benilla_web_bg.wasm.opt" "${DIST}/benilla_web_bg.wasm"
fi
ls -la "${DIST}"
