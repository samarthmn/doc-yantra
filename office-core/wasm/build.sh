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
MAX_WASM_MB=8                              # fontless engine budget; font payload is forbidden
WASM_PACK_VERSION="0.15.0"                 # generated glue is committed; upgrades are deliberate

wasmpack() {
  if command -v wasm-pack >/dev/null 2>&1; then
    if [[ "$(wasm-pack --version)" != "wasm-pack ${WASM_PACK_VERSION}" ]]; then
      echo "error: expected wasm-pack ${WASM_PACK_VERSION}; found $(wasm-pack --version)." >&2
      exit 1
    fi
    wasm-pack "$@"
  elif command -v pnpm >/dev/null 2>&1; then
    pnpm dlx "wasm-pack@${WASM_PACK_VERSION}" "$@"
  elif command -v npx >/dev/null 2>&1; then
    npx --yes "wasm-pack@${WASM_PACK_VERSION}" "$@"
  else
    echo "error: need wasm-pack ${WASM_PACK_VERSION}, pnpm, or npx on PATH" >&2
    exit 1
  fi
}

# A package-version-only change must rebuild both the wrapper's version() and
# office-core's embedded engine version instead of reusing their wasm artifacts.
if node ../../scripts/wasm-version.mjs needs-clean Cargo.toml pkg; then
  cargo clean --manifest-path Cargo.toml --release --target wasm32-unknown-unknown \
    -p office-core -p office-core-wasm
fi

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

# wasm-bindgen copies `&[u8]` arguments into wasm memory before Rust runs. Patch
# both generated entry points so the public 50 MiB contract rejects oversized
# Office input before that allocation/copy occurs.
node ./postprocess-glue.mjs \
  pkg/office_core_wasm.js \
  pkg/web/office_core_wasm.js

# Server worker API. Runtime is single-source at repo root; only engine op
# tables and public facades are package-specific.
cp ../../wasm-worker/worker-client.cjs ../../wasm-worker/worker-entry.cjs pkg/
cp worker.js worker.d.ts worker-ops.cjs index.cjs index.d.ts font-provider.cjs font-provider.d.ts fontsource-provider.cjs fontsource-provider.d.ts noto-fallback.cjs noto-fallback.d.ts pkg/
cp web-index.js web-index.d.ts pkg/web/
cp font-provider-web.js pkg/web/font-provider.js
cp font-provider-web.d.ts pkg/web/font-provider.d.ts
cp fontsource-provider-web.js pkg/web/fontsource-provider.js
cp fontsource-provider.d.ts pkg/web/fontsource-provider.d.ts

REPO_URL="git+https://github.com/${OWNER}/${REPO}.git" PKG_NAME="${PKG_NAME}" \
node -e '
  const fs = require("fs"), p = "pkg/package.json";
  const j = JSON.parse(fs.readFileSync(p, "utf8"));
  j.name = process.env.PKG_NAME;
  j.repository = { type: "git", url: process.env.REPO_URL };
  j.license = "UNLICENSED";
  j.main = "index.cjs";
  j.types = "index.d.ts";
  j.publishConfig = { registry: "https://npm.pkg.github.com" };
  j.files = Array.from(new Set([...(j.files || []), "web", "index.cjs", "index.d.ts", "font-provider.cjs", "font-provider.d.ts", "fontsource-provider.cjs", "fontsource-provider.d.ts", "noto-fallback.cjs", "noto-fallback.d.ts", "worker.js", "worker.d.ts", "worker-client.cjs", "worker-entry.cjs", "worker-ops.cjs", "build-fingerprint.json"]));
  j.exports = {
    ".": {
      types: "./index.d.ts",
      require: "./index.cjs",
      default: "./index.cjs",
    },
    "./web": {
      types: "./web/web-index.d.ts",
      import: "./web/web-index.js",
      default: "./web/web-index.js",
    },
    "./web/font-provider": { types: "./web/font-provider.d.ts", import: "./web/font-provider.js" },
    "./web/fontsource-provider": { types: "./web/fontsource-provider.d.ts", import: "./web/fontsource-provider.js" },
    "./fontsource-provider": { types: "./fontsource-provider.d.ts", require: "./fontsource-provider.cjs", default: "./fontsource-provider.cjs" },
    "./noto-fallback": { types: "./noto-fallback.d.ts", require: "./noto-fallback.cjs", default: "./noto-fallback.cjs" },
    "./web/office_core_wasm_bg.wasm": "./web/office_core_wasm_bg.wasm",
    "./worker": { types: "./worker.d.ts", require: "./worker.js", default: "./worker.js" },
    "./package.json": "./package.json",
  };
  fs.writeFileSync(p, JSON.stringify(j, null, 2) + "\n");
  console.log("stamped", j.name, j.version, "->", j.publishConfig.registry);
'

# Stamp the source fingerprint so consumers can detect a stale pkg/ build
# (doc-kosha tests/unit/package-freshness.test.ts recomputes and compares).
node ../../scripts/source-fingerprint.mjs office --out pkg/build-fingerprint.json
node ../../scripts/wasm-version.mjs verify Cargo.toml pkg

WASM_BYTES=$(wc -c < pkg/office_core_wasm_bg.wasm)
WASM_MB=$(( WASM_BYTES / 1024 / 1024 ))
echo "Sizes:"
ls -la pkg/office_core_wasm_bg.wasm pkg/web/office_core_wasm_bg.wasm | awk '{print $5, $9}'
if [ "${WASM_MB}" -ge "${MAX_WASM_MB}" ]; then
  echo "error: nodejs wasm is ${WASM_MB} MB, over the ${MAX_WASM_MB} MB budget." >&2
  echo "       investigate engine growth; font binaries are forbidden in this artifact." >&2
  exit 1
fi
echo "Done (${WASM_MB} MB < ${MAX_WASM_MB} MB). Package ready at office-core/wasm/pkg (commit it)."
