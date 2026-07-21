#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${ROOT_DIR}/target/wasm-app/dist"
WASM_BINDGEN_VERSION="${WASM_BINDGEN_VERSION:-0.2.126}"

command -v wasm-bindgen >/dev/null 2>&1 || {
  echo "wasm-bindgen is required. Install with:"
  echo "  cargo install wasm-bindgen-cli --version ${WASM_BINDGEN_VERSION}"
  exit 1
}

cargo build \
  --manifest-path "${ROOT_DIR}/Cargo.toml" \
  --package logic-analyzer-app-web \
  --target wasm32-unknown-unknown \
  --release

rm -rf "${OUT_DIR}"
mkdir -p "${OUT_DIR}/pkg"
cp "${ROOT_DIR}"/crates/app_web/web/* "${OUT_DIR}/"

wasm-bindgen \
  "${ROOT_DIR}/target/wasm32-unknown-unknown/release/logic_conduit.wasm" \
  --target web \
  --out-dir "${OUT_DIR}/pkg"

echo "WASM app written to ${OUT_DIR}"
