#!/usr/bin/env bash
#
# Cut a new patch release on the fixed 0.0.x line for every published package.
#
# The engines release in lockstep: one version, one commit, one 0.0.x tag; the
# publish-wasm.yml Action publishes the committed packages to GitHub Packages
# when the tag lands. GitHub never rebuilds or retests them, so this script is
# the supported local build-and-gate path (R2-Y1).
#
# Usage:
#   scripts/release.sh            # auto: next patch after the highest tag
#   scripts/release.sh 7          # explicit: release 0.0.7
#   DRY_RUN=1 scripts/release.sh  # print what it would do, change nothing
#
# Extending the release
#   A released package is a directory that follows these conventions:
#     <pkg>/Cargo.toml        engine crate manifest (version bumped in lockstep)
#     <pkg>/wasm/Cargo.toml   wasm crate manifest   (version bumped in lockstep)
#     <pkg>/wasm/build.sh     rebuilds the committed <pkg>/wasm/pkg/ artifact
#   To add one: append its directory to PACKAGES, and register any
#   package-specific test gates in run_gates.
#
set -euo pipefail
cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"

PACKAGES=(pdf-core office-core)
WASM_PACK_VERSION="0.15.0"
CARGO_LOCKS=(
  pdf-core/Cargo.lock
  pdf-core/wasm/Cargo.lock
  office-core/Cargo.lock
  office-core/wasm/Cargo.lock
  text-core/Cargo.lock
)

version=""
tag=""

# ─── Preflight: releases cut only from a clean, up-to-date local main ────────
preflight_git() {
  if [[ "$(git branch --show-current)" != "main" ]]; then
    echo "error: releases must be cut from the local main branch." >&2
    exit 1
  fi
  if [[ -n "$(git status --short)" ]]; then
    echo "error: releases require a clean worktree." >&2
    exit 1
  fi
  git fetch origin main --tags --quiet
  if ! git merge-base --is-ancestor origin/main HEAD; then
    echo "error: local main is behind or diverged from origin/main; update it before releasing." >&2
    exit 1
  fi
}

preflight_tools() {
  if ! command -v wasm-pack >/dev/null 2>&1; then
    echo "error: releases require a preinstalled wasm-pack ${WASM_PACK_VERSION}." >&2
    echo "       Refusing a release-time network installer." >&2
    exit 1
  fi
  if [[ "$(wasm-pack --version)" != "wasm-pack ${WASM_PACK_VERSION}" ]]; then
    echo "error: expected wasm-pack ${WASM_PACK_VERSION}; found $(wasm-pack --version)." >&2
    exit 1
  fi
  if ! command -v cargo-audit >/dev/null 2>&1; then
    echo "error: releases require cargo-audit on PATH." >&2
    exit 1
  fi
}

# ─── Version: next patch on 0.0.x, derived from existing tags ────────────────
# Scans both the current tag form (0.0.x) and the legacy one (wasm-v0.0.x) so
# the highest patch is found across the naming switch.
pick_version() {
  local highest
  highest="$(git tag --list '0.0.*' 'wasm-v0.0.*' \
    | sed -n -E 's/^(wasm-v)?0\.0\.([0-9]+)$/\2/p' \
    | sort -n | tail -1)"
  highest="${highest:-0}"

  local patch
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
}

manifests_for() {
  local package="$1"
  echo "${package}/Cargo.toml ${package}/wasm/Cargo.toml"
}

dry_run_report() {
  local package
  echo "[dry-run] would set version=\"${version}\" in the lockstep Cargo.tomls:"
  for package in "${PACKAGES[@]}"; do
    echo "          $(manifests_for "$package")"
  done
  echo "[dry-run] would run wasm/build.sh for: ${PACKAGES[*]}"
  echo "[dry-run] would run every test gate, commit ${PACKAGES[*]},"
  echo "          tag ${tag}, push origin main ${tag}"
}

# ─── Bump: write the new version into every lockstep Cargo.toml ──────────────
# Only the first `version =` line of each manifest (the [package] version).
# Cargo.locks follow during the builds; each pkg/package.json follows via its
# package's build.sh.
set_version() {
  local file="$1"
  awk -v v="$version" '
    !done && /^version[[:space:]]*=/ { sub(/"[^"]*"/, "\"" v "\""); done=1 }
    { print }
  ' "$file" > "$file.tmp" && mv "$file.tmp" "$file"
}

bump_versions() {
  local package manifest
  for package in "${PACKAGES[@]}"; do
    for manifest in $(manifests_for "$package"); do
      set_version "$manifest"
    done
  done
  echo "  set version = \"${version}\" in the lockstep Cargo.tomls"
}

# ─── Build: regenerate every committed wasm package ──────────────────────────
build_packages() {
  local package
  for package in "${PACKAGES[@]}"; do
    echo "  rebuilding ${package} wasm package ..."
    ( cd "${package}/wasm" && ./build.sh )
  done
}

# ─── Gates (R2-Y1): every release-blocking test, run locally ─────────────────
gate_security() {
  echo "  auditing Rust dependency locks ..."
  local lockfile
  for lockfile in "${CARGO_LOCKS[@]}"; do
    # ttf-parser is marked unmaintained with no patched release. Keep this
    # single documented exception while failing on every vulnerability and
    # every other informational advisory discovered at release time.
    cargo audit --file "${lockfile}" --ignore RUSTSEC-2026-0192
  done
}

gate_pdf_core() {
  if [[ ! -d pdf-core/pdfium ]]; then
    echo "error: pdf-core/pdfium/ is missing — cargo's default (pdfium) tests need it." >&2
    echo "       Run pdf-core/scripts/fetch-pdfium.sh once, then re-run the release." >&2
    exit 1
  fi
  echo "  running pdf-core cargo tests ..."
  ( cd pdf-core && cargo test --quiet )
  ( cd pdf-core && cargo test --quiet --no-default-features --features wasm )
}

gate_office_core() {
  echo "  running office-core cargo tests ..."
  ( cd office-core && cargo test --quiet )
  echo "  running fidelity harness unit tests ..."
  ( cd office-core && cargo test --quiet -p fidelity )
  echo "  running blocking DOCX fidelity corpus ..."
  ( cd office-core && cargo run --quiet -p fidelity -- run --format docx )
}

gate_package_suite() {
  echo "  running package tests against the rebuilt pkgs ..."
  # The Office suite configures its deterministic local mock provider; release
  # verification must never depend on a public font CDN.
  node --test tests/all.test.mjs
}

run_gates() {
  gate_security
  gate_pdf_core
  gate_office_core
  gate_package_suite
  # New packages: add a gate_<package> above and call it here.
}

# ─── Ship: one commit and one tag cover every package ────────────────────────
ship() {
  git add "${PACKAGES[@]}"
  git commit -m "wasm: v${version}"
  git tag "$tag"
  git push --atomic origin main "$tag"
  echo "Done. Pushed ${tag}; publish-wasm.yml will publish ${version} to GitHub Packages."
}

main() {
  preflight_git
  preflight_tools
  pick_version "$@"
  if [[ -n "${DRY_RUN:-}" ]]; then
    dry_run_report
    exit 0
  fi
  bump_versions
  build_packages
  run_gates
  ship
}

main "$@"
