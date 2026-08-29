#!/usr/bin/env bash
#
# web-setup.sh - one-time toolchain for the browser build (no sudo, nothing system-wide):
#   - rustup target wasm32-unknown-unknown
#   - wasm-bindgen-cli at EXACTLY the version Cargo.lock pins (a mismatch emits broken glue)
#   - wasi-sdk (a C sysroot for wasm32: the Lua 5.1 VM is C) unpacked into tools/wasi-sdk/
#
# Re-runnable; each step is skipped when already done. WASI_SDK_VERSION overrides the release.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

WASI_SDK_VERSION="${WASI_SDK_VERSION:-27}"
WASI_DIR="tools/wasi-sdk"

echo "== rustup target"
rustup target add wasm32-unknown-unknown

echo "== wasm-bindgen-cli (pinned to Cargo.lock)"
want=$(awk '/^name = "wasm-bindgen"$/{getline; sub(/version = /,""); gsub(/"/,""); print; exit}' Cargo.lock)
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}' || true)
if [ "${have}" != "${want}" ]; then
  cargo install wasm-bindgen-cli --version "${want}" --locked --force
else
  echo "wasm-bindgen ${want} already installed"
fi

echo "== wasi-sdk ${WASI_SDK_VERSION}"
if [ -f "${WASI_DIR}/share/wasi-sysroot/lib/wasm32-wasip1/libsetjmp.a" ]; then
  echo "already at ${WASI_DIR}"
else
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)  plat=x86_64-linux ;;
    Linux-aarch64) plat=arm64-linux ;;
    Darwin-arm64)  plat=arm64-macos ;;
    Darwin-x86_64) plat=x86_64-macos ;;
    *) echo "unsupported host $(uname -s)-$(uname -m); set WASI_SDK to an unpacked SDK instead" >&2; exit 1 ;;
  esac
  url="https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-${WASI_SDK_VERSION}/wasi-sdk-${WASI_SDK_VERSION}.0-${plat}.tar.gz"
  mkdir -p "${WASI_DIR}"
  echo "downloading ${url}"
  curl -sL "${url}" | tar xz --strip-components=1 -C "${WASI_DIR}"
fi

echo "== binaryen (wasm-opt -O3: the client is CPU-bound on its one wasm thread; +6 % measured)"
BINARYEN_VERSION="${BINARYEN_VERSION:-130}"
if [ -x tools/binaryen/bin/wasm-opt ]; then
  echo "already at tools/binaryen"
else
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)  bplat=x86_64-linux ;;
    Linux-aarch64) bplat=aarch64-linux ;;
    Darwin-arm64)  bplat=arm64-macos ;;
    Darwin-x86_64) bplat=x86_64-macos ;;
    *) bplat="" ;;
  esac
  if [ -n "${bplat}" ]; then
    mkdir -p tools/binaryen
    curl -sL "https://github.com/WebAssembly/binaryen/releases/download/version_${BINARYEN_VERSION}/binaryen-version_${BINARYEN_VERSION}-${bplat}.tar.gz" | tar xz --strip-components=1 -C tools/binaryen
  else
    echo "  no binaryen release for this host — web-build.sh will skip wasm-opt"
  fi
fi

echo "== optional: brotli/gzip for precompressed output"
command -v brotli >/dev/null || echo "  brotli not found — web-build.sh will skip .br siblings (the host then serves plain files)"
echo "ready: scripts/web-build.sh"
