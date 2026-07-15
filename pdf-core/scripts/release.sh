#!/usr/bin/env bash
#
# Cut a new patch release on the 0.0.x line.
#
# Versioning is fixed at 0.0.x (only the patch increments). This script:
#   1. finds the next x from existing git tags,
#   2. writes version = "0.0.x" into all four engine/wasm Cargo.tomls,
#   3. rebuilds both committed wasm packages so they carry the new version,
#   4. tests and commits both engines, then tags 0.0.x and pushes — the workflow
#      Action publishes to GitHub Packages on the tag.
#
# Usage:
#   ./scripts/release.sh            # auto: next patch after the highest tag
#   ./scripts/release.sh 7          # explicit: release 0.0.7
#   DRY_RUN=1 ./scripts/release.sh  # print what it would do, change nothing
#
set -euo pipefail
cd "$(dirname "$0")/.."                       # -> pdf-core/
REPO_ROOT="$(git rev-parse --show-toplevel)"

CORE_TOML="Cargo.toml"
WASM_TOML="wasm/Cargo.toml"
# office-core rides the same 0.0.x line (four Cargo.tomls in lockstep).
OFFICE_TOML="../office-core/Cargo.toml"
OFFICE_WASM_TOML="../office-core/wasm/Cargo.toml"

if [[ "$(git -C "$REPO_ROOT" branch --show-current)" != "main" ]]; then
  echo "error: releases must be cut from the local main branch." >&2
  exit 1
fi
if [[ -n "$(git -C "$REPO_ROOT" status --short)" ]]; then
  echo "error: releases require a clean worktree." >&2
  exit 1
fi

# ─── Determine the next patch (x) ──────────────────────────────────────────
# Scan both the current tag form (0.0.x) and the legacy one (wasm-v0.0.x) so
# the highest patch is found across the naming switch.
git fetch origin main --tags --quiet
if ! git -C "$REPO_ROOT" merge-base --is-ancestor origin/main HEAD; then
  echo "error: local main is behind or diverged from origin/main; update it before releasing." >&2
  exit 1
fi
highest="$(git tag --list '0.0.*' 'wasm-v0.0.*' \
  | sed -n -E 's/^(wasm-v)?0\.0\.([0-9]+)$/\2/p' \
  | sort -n | tail -1)"
highest="${highest:-0}"

if [[ "${1:-}" =~ ^[0-9]+$ ]]; then
  patch="$1"
else
  patch=$((highest + 1))
fi
version="0.0.${patch}"
tag="${version}"

if [[ "$patch" -le "$highest" ]]; then
  echo "error: ${tag} is not newer than the highest existing patch 0.0.${highest}." >&2
  echo "       Published versions are immutable — pick a higher patch." >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
  echo "error: tag ${tag} already exists. Versions are immutable; bump instead." >&2
  exit 1
fi

echo "Releasing ${version}  (highest existing: 0.0.${highest})"

if [[ -n "${DRY_RUN:-}" ]]; then
  echo "[dry-run] would set version=\"${version}\" in all four Cargo.tomls:"
  echo "          ${CORE_TOML}, ${WASM_TOML}, ${OFFICE_TOML}, ${OFFICE_WASM_TOML}"
  echo "[dry-run] would run pdf-core + office-core wasm/build.sh, commit pdf-core/ + office-core/,"
  echo "          tag ${tag}, push origin main ${tag}"
  exit 0
fi

# ─── 1. Bump all Cargo.tomls (only the first [package] version line) ────────
set_version() {
  local file="$1"
  awk -v v="$version" '
    !done && /^version[[:space:]]*=/ { sub(/"[^"]*"/, "\"" v "\""); done=1 }
    { print }
  ' "$file" > "$file.tmp" && mv "$file.tmp" "$file"
}
set_version "$CORE_TOML"
set_version "$WASM_TOML"
set_version "$OFFICE_TOML"
set_version "$OFFICE_WASM_TOML"
echo "  set version = \"${version}\" in all four Cargo.tomls"

# ─── 2. Rebuild the committed wasm packages (pkg/ carries the crate version) ──
echo "  rebuilding pdf-core wasm package ..."
( cd wasm && ./build.sh )
echo "  rebuilding office-core wasm package ..."
( cd ../office-core/wasm && ./build.sh )

# ─── 2b. Test gate (R2-Y1) ─────────────────────────────────────────────────
# GitHub publishes the committed packages without rebuilding or retesting them,
# so the supported release path must complete every gate locally before tagging.
if [[ ! -d pdfium ]]; then
  echo "error: pdf-core/pdfium/ is missing — cargo's default (pdfium) tests need it." >&2
  echo "       Run ./scripts/fetch-pdfium.sh once, then re-run the release." >&2
  exit 1
fi
echo "  running cargo tests ..."
cargo test --quiet
cargo test --quiet --no-default-features --features wasm
echo "  running office-core cargo tests ..."
( cd ../office-core && cargo test --quiet )
echo "  running fidelity harness unit tests ..."
( cd ../office-core && cargo test --quiet -p fidelity )
echo "  running blocking DOCX fidelity corpus ..."
( cd ../office-core && cargo run --quiet -p fidelity -- run --format docx )
echo "  running package tests against the rebuilt pkg ..."
# The Office suite configures its deterministic local mock provider; release
# verification must never depend on a public font CDN.
( cd "$REPO_ROOT" && node --test tests/all.test.mjs )

# ─── 3. Commit, tag, push ──────────────────────────────────────────────────
git -C "$REPO_ROOT" add pdf-core office-core
git -C "$REPO_ROOT" commit -m "wasm: v${version}"
git -C "$REPO_ROOT" tag "$tag"
git -C "$REPO_ROOT" push --atomic origin main "$tag"

echo "Done. Pushed ${tag}; publish-wasm.yml will publish ${version} to GitHub Packages."
