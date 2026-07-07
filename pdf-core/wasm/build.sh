#!/usr/bin/env bash
#
# Builds the pdf-core WebAssembly package (both targets) and stamps the npm
# metadata needed to publish it to GitHub Packages. The output (pkg/) is
# committed to the repo so consumers and CI can install without a Rust
# toolchain.
#
#   pkg/       Node/server build (CJS) — includes the `office` feature
#   pkg/web/   browser build (ESM, wasm-pack --target web) — Zone 1 only,
#              no office (Typst would bloat the browser payload)
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

wasmpack() {
  if command -v wasm-pack >/dev/null 2>&1; then
    wasm-pack "$@"
  elif command -v pnpm >/dev/null 2>&1; then
    pnpm dlx wasm-pack "$@"
  else
    echo "error: need either 'wasm-pack' or 'pnpm' on PATH" >&2
    exit 1
  fi
}

echo "Building ${PKG_NAME} (browser target, Zone 1 only) ..."
rm -rf pkg-web pkg-office
wasmpack build --release --target web --out-dir pkg-web --scope "${OWNER}"

echo "Building ${PKG_NAME} (node office module — heavy, lazy-loaded) ..."
wasmpack build --release --target nodejs --out-dir pkg-office --scope "${OWNER}" -- --features office

echo "Building ${PKG_NAME} (node main module, Zone 1 only) ..."
wasmpack build --release --target nodejs --scope "${OWNER}"

# wasm-pack writes pkg/.gitignore containing '*' — remove it so the built
# package can be committed (Vercel/CI have no Rust toolchain).
rm -f pkg/.gitignore pkg-web/.gitignore pkg-web/package.json pkg-web/README.md \
      pkg-office/.gitignore pkg-office/package.json pkg-office/README.md
mkdir -p pkg/web pkg/office
mv pkg-web/* pkg/web/
mv pkg-office/* pkg/office/
rmdir pkg-web pkg-office

# The browser build is ESM (wasm-pack --target web); mark it so resolvers that
# bypass the root "exports" conditions still parse pkg/web/*.js as modules and
# never mis-read it as CommonJS (Y-8). The office/ subtree is CJS (--target
# nodejs) and needs no marker — the package root is CJS by default.
printf '{\n  "type": "module"\n}\n' > pkg/web/package.json

# wasm-pack derives the name from the crate (@OWNER/pdf-core-wasm) and does not
# emit repository / license / publishConfig / exports; set the final metadata.
REPO_URL="git+https://github.com/${OWNER}/${REPO}.git" PKG_NAME="${PKG_NAME}" \
node -e '
  const fs = require("fs"), p = "pkg/package.json";
  const j = JSON.parse(fs.readFileSync(p, "utf8"));
  j.name = process.env.PKG_NAME;
  j.repository = { type: "git", url: process.env.REPO_URL };
  j.license = "UNLICENSED";
  j.publishConfig = { registry: "https://npm.pkg.github.com" };
  j.files = Array.from(new Set([...(j.files || []), "web", "office"]));
  j.exports = {
    ".": {
      types: "./pdf_core_wasm.d.ts",
      require: "./pdf_core_wasm.js",
      default: "./pdf_core_wasm.js",
    },
    "./office": {
      types: "./office/pdf_core_wasm.d.ts",
      require: "./office/pdf_core_wasm.js",
      default: "./office/pdf_core_wasm.js",
    },
    "./web": {
      types: "./web/pdf_core_wasm.d.ts",
      import: "./web/pdf_core_wasm.js",
      default: "./web/pdf_core_wasm.js",
    },
    "./web/pdf_core_wasm_bg.wasm": "./web/pdf_core_wasm_bg.wasm",
    "./package.json": "./package.json",
  };
  fs.writeFileSync(p, JSON.stringify(j, null, 2) + "\n");
  console.log("stamped", j.name, j.version, "->", j.publishConfig.registry);
'

echo "Sizes:"
ls -la pkg/pdf_core_wasm_bg.wasm pkg/office/pdf_core_wasm_bg.wasm pkg/web/pdf_core_wasm_bg.wasm | awk '{print $5, $9}'
echo "Done. Package ready at pdf-core/wasm/pkg (commit it)."
