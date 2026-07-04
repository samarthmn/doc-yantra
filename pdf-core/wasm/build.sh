#!/usr/bin/env bash
#
# Builds the pdf-core WebAssembly package and stamps the npm metadata needed to
# publish it to GitHub Packages. The output (pkg/) is committed to the repo so
# consumers and CI can install it without a Rust toolchain.
#
# Usage:  ./build.sh
#
set -euo pipefail
cd "$(dirname "$0")"

# ─── EDIT THESE IF YOUR GITHUB OWNER / REPO / PACKAGE NAME DIFFER ────────────
# For GitHub Packages, the package's npm scope MUST equal the GitHub owner
# (user or org) that hosts the repo, or the publish is rejected. PKG_NAME is
# therefore scoped to OWNER.
OWNER="samarthmn"                    # GitHub owner (user/org) == npm scope
REPO="doc-yantra"                    # GitHub repo name
PKG_NAME="@samarthmn/doc-yantra"     # published npm package name
# ────────────────────────────────────────────────────────────────────────────

echo "Building ${PKG_NAME} ..."
pnpm dlx wasm-pack build --release --target nodejs --scope "${OWNER}"

# wasm-pack writes pkg/.gitignore containing '*' — remove it so the built
# package can be committed (Vercel/CI have no Rust toolchain).
rm -f pkg/.gitignore

# wasm-pack derives the name from the crate (@OWNER/pdf-core-wasm) and does not
# emit repository / license / publishConfig; set the final name + metadata so
# the package publishes to GitHub Packages and links to this repo.
REPO_URL="git+https://github.com/${OWNER}/${REPO}.git" PKG_NAME="${PKG_NAME}" \
node -e '
  const fs = require("fs"), p = "pkg/package.json";
  const j = JSON.parse(fs.readFileSync(p, "utf8"));
  j.name = process.env.PKG_NAME;
  j.repository = { type: "git", url: process.env.REPO_URL };
  j.license = "UNLICENSED";
  j.publishConfig = { registry: "https://npm.pkg.github.com" };
  fs.writeFileSync(p, JSON.stringify(j, null, 2) + "\n");
  console.log("stamped", j.name, j.version, "->", j.publishConfig.registry);
'

echo "Done. Package ready at pdf-core/wasm/pkg (commit it)."
