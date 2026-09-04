#!/usr/bin/env bash
# Verify a fresh public candidate in isolation. The destination is created by
# this script and removed by its trap; an existing path is always rejected.
set -euo pipefail
cd "$(dirname "$0")/.."

DEST="${1:-tmp/public-candidate-smoke-$$}"
case "$DEST" in
  tmp/public-candidate-smoke-*) ;;
  *) echo "error: destination must be a task-owned tmp/public-candidate-smoke-* path" >&2; exit 2 ;;
esac
if [[ -e "$DEST" ]]; then
  echo "error: destination already exists; refusing to delete it: $DEST" >&2
  exit 2
fi
trap 'rm -rf -- "$DEST"' EXIT

node scripts/export-public-source.mjs --out-dir "$DEST"
(cd "$DEST" && \
  node scripts/export-public-source.mjs --check-only && \
  node scripts/package-audit.mjs && \
  node --test public-tests/*.test.mjs tests/font-cdn-tools.test.mjs tests/license-metadata.test.mjs && \
  cargo test --manifest-path pdf-core/Cargo.toml --lib --no-default-features --features wasm && \
  cargo metadata --manifest-path office-core/Cargo.toml --locked --no-deps --format-version 1 >/dev/null && \
  cargo check --manifest-path office-core/Cargo.toml --locked --lib --no-default-features --features wasm)
