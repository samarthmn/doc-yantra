#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

MODE="${1:-write}"
if [[ "$MODE" != "write" && "$MODE" != "--check" ]]; then
  echo "usage: scripts/generate-rust-license-notices.sh [--check]" >&2
  exit 2
fi
command -v cargo-about >/dev/null 2>&1 || {
  echo "error: cargo-about 0.9.2 is required to generate Rust dependency notices" >&2
  exit 1
}
[[ "$(cargo about --version)" == "cargo-about 0.9.2" ]] || {
  echo "error: expected cargo-about 0.9.2; found $(cargo about --version)" >&2
  exit 1
}

mkdir -p tmp
normalize_notice() {
  local output="$1"
  local normalized="${output}.normalized"
  awk '{
    sub(/\r$/, "")
    sub(/[ \t]+$/, "")
    lines[NR] = $0
    if ($0 != "") last = NR
  }
  END {
    for (i = 1; i <= last; i++) print lines[i]
  }' "$output" > "$normalized"
  mv "$normalized" "$output"
}

generate() {
  local manifest="$1"
  local output="$2"
  cargo about generate \
    --config legal/rust/about.toml \
    --manifest-path "$manifest" \
    --no-default-features \
    --target wasm32-unknown-unknown \
    --locked \
    --fail \
    legal/rust/third-party-licenses.hbs \
    --output-file "$output"
  normalize_notice "$output"
}

if [[ "$MODE" == "--check" ]]; then
  SCRATCH="tmp/rust-license-notices-$$"
  mkdir "$SCRATCH"
  trap 'rm -rf -- "$SCRATCH"' EXIT
  generate pdf-core/wasm/Cargo.toml "$SCRATCH/pdf.txt"
  generate office-core/wasm/Cargo.toml "$SCRATCH/office.txt"
  cmp "$SCRATCH/pdf.txt" pdf-core/wasm/THIRD_PARTY_RUST_LICENSES.txt
  cmp "$SCRATCH/office.txt" office-core/wasm/THIRD_PARTY_RUST_LICENSES.txt
  echo "Rust dependency notice check passed"
else
  generate pdf-core/wasm/Cargo.toml pdf-core/wasm/THIRD_PARTY_RUST_LICENSES.txt
  generate office-core/wasm/Cargo.toml office-core/wasm/THIRD_PARTY_RUST_LICENSES.txt
  echo "Rust dependency notices regenerated"
fi
