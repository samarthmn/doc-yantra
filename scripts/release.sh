#!/usr/bin/env bash
# Validate a lockstep DocYantra release candidate. With --publish, push the
# already-prepared version tag so GitHub Actions publishes the exact commit to
# GitHub Packages and creates the matching GitHub release.
set -euo pipefail
cd "$(dirname "$0")/.."

# This gate is deliberately preparation-only; the flag is retained as a
# searchable contract for automation and reviewers.
RELEASE_PREPARE_ONLY=1

require_tool() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: release preparation requires $1 on PATH" >&2
    exit 1
  }
}

check_versions() {
  node <<'NODE'
const fs = require("node:fs");
const json = [
  "pdf-core/wasm/pkg/package.json",
  "office-core/wasm/pkg/package.json",
  "packages/doc-yantra-fonts/package.json",
  "packages/dockosha-provider/package.json",
];
const expected = JSON.parse(fs.readFileSync(json[0], "utf8")).version;
if (!/^0\.0\.\d+$/.test(expected)) throw new Error(`release package version must stay on 0.0.x, found ${expected}`);
for (const path of json) {
  const manifest = JSON.parse(fs.readFileSync(path, "utf8"));
  if (manifest.version !== expected) throw new Error(`${path}: expected ${expected}, found ${manifest.version}`);
}
for (const path of ["pdf-core/Cargo.toml", "pdf-core/wasm/Cargo.toml", "office-core/Cargo.toml", "office-core/wasm/Cargo.toml"]) {
  const source = fs.readFileSync(path, "utf8");
  if (!new RegExp(`^version\\s*=\\s*"${expected.replaceAll(".", "\\.")}"`, "m").test(source)) {
    throw new Error(`${path}: package version is not ${expected}`);
  }
}
process.stdout.write(expected);
NODE
}

publish_tag() {
  if [[ "$(git branch --show-current)" != "main" ]]; then
    echo "error: releases must be tagged from main" >&2
    exit 1
  fi
  if [[ -n "$(git status --short)" ]]; then
    echo "error: releases require a clean worktree and index" >&2
    exit 1
  fi
  git fetch origin main --tags --quiet
  if [[ "$(git rev-parse HEAD)" != "$(git rev-parse origin/main)" ]]; then
    echo "error: local main must exactly match origin/main before release" >&2
    exit 1
  fi
  if git rev-parse -q --verify "refs/tags/${VERSION}" >/dev/null; then
    echo "error: tag ${VERSION} already exists" >&2
    exit 1
  fi

  git tag "$VERSION"
  if ! git push --atomic origin main "$VERSION"; then
    git tag -d "$VERSION" >/dev/null
    echo "error: release push failed; removed local tag ${VERSION}" >&2
    exit 1
  fi
  echo "pushed ${VERSION}; GitHub Actions will publish the four packages to GitHub Packages"
}

main() {
  if [[ $# -gt 1 || ( $# -eq 1 && "$1" != "--publish" ) ]]; then
    echo "usage: scripts/release.sh [--publish]" >&2
    exit 2
  fi
  require_tool node
  require_tool npm
  require_tool cargo-about
  VERSION="$(check_versions)"
  ./scripts/generate-rust-license-notices.sh --check
  node scripts/package-audit.mjs
  node scripts/export-public-source.mjs --check-only
  ./scripts/clean-export-smoke.sh
  echo "release preparation passed for ${VERSION}"
  if [[ "${1:-}" == "--publish" ]]; then
    publish_tag
  else
    echo "no repository or registry state was changed; use --publish to push the release tag"
  fi
}

main "$@"
