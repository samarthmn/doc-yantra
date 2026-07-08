#!/usr/bin/env bash
#
# Builds the office-core WebAssembly package (both targets) and stamps the npm
# metadata for GitHub Packages. Mirrors pdf-core/wasm/build.sh. The output
# (pkg/) is committed so consumers and CI install without a Rust toolchain.
#
#   pkg/       Node/server build (CJS)
#   pkg/web/   browser build (ESM, wasm-pack --target web)
#
# Usage:  ./build.sh
#
set -euo pipefail
cd "$(dirname "$0")"

OWNER="samarthmn"                          # GitHub owner (user/org) == npm scope
REPO="doc-yantra"                          # GitHub repo name
PKG_NAME="@samarthmn/doc-yantra-office"    # published npm package name
MAX_WASM_MB=25                             # size budget (engine + 20 bundled fonts)

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

echo "Building ${PKG_NAME} (browser target) ..."
rm -rf pkg-web
wasmpack build --release --target web --out-dir pkg-web --scope "${OWNER}"

echo "Building ${PKG_NAME} (node main module) ..."
wasmpack build --release --target nodejs --scope "${OWNER}"

# wasm-pack writes pkg/.gitignore containing '*' — remove it so pkg/ can be
# committed (Vercel/CI have no Rust toolchain).
rm -f pkg/.gitignore pkg-web/.gitignore pkg-web/package.json pkg-web/README.md
mkdir -p pkg/web
mv pkg-web/* pkg/web/
rmdir pkg-web

# The browser build is ESM; mark it so resolvers never mis-read it as CJS.
printf '{\n  "type": "module"\n}\n' > pkg/web/package.json

REPO_URL="git+https://github.com/${OWNER}/${REPO}.git" PKG_NAME="${PKG_NAME}" \
node -e '
  const fs = require("fs"), p = "pkg/package.json";
  const j = JSON.parse(fs.readFileSync(p, "utf8"));
  j.name = process.env.PKG_NAME;
  j.repository = { type: "git", url: process.env.REPO_URL };
  j.license = "UNLICENSED";
  j.publishConfig = { registry: "https://npm.pkg.github.com" };
  j.files = Array.from(new Set([...(j.files || []), "web"]));
  j.exports = {
    ".": {
      types: "./office_core_wasm.d.ts",
      require: "./office_core_wasm.js",
      default: "./office_core_wasm.js",
    },
    "./web": {
      types: "./web/office_core_wasm.d.ts",
      import: "./web/office_core_wasm.js",
      default: "./web/office_core_wasm.js",
    },
    "./web/office_core_wasm_bg.wasm": "./web/office_core_wasm_bg.wasm",
    "./package.json": "./package.json",
  };
  fs.writeFileSync(p, JSON.stringify(j, null, 2) + "\n");
  console.log("stamped", j.name, j.version, "->", j.publishConfig.registry);
'

WASM_BYTES=$(wc -c < pkg/office_core_wasm_bg.wasm)
WASM_MB=$(( WASM_BYTES / 1024 / 1024 ))
echo "Sizes:"
ls -la pkg/office_core_wasm_bg.wasm pkg/web/office_core_wasm_bg.wasm | awk '{print $5, $9}'
if [ "${WASM_MB}" -ge "${MAX_WASM_MB}" ]; then
  echo "error: nodejs wasm is ${WASM_MB} MB, over the ${MAX_WASM_MB} MB budget." >&2
  echo "       subset the bundled fonts to WinAnsi+Latin-Extended at vendor time." >&2
  exit 1
fi
echo "Done (${WASM_MB} MB < ${MAX_WASM_MB} MB). Package ready at office-core/wasm/pkg (commit it)."
