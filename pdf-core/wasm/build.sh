#!/usr/bin/env bash
#
# Builds the pdf-core WebAssembly package (both targets) and stamps the npm
# metadata needed to publish it to GitHub Packages. The output (pkg/) is
# committed to the repo so consumers and CI can install without a Rust
# toolchain.
#
#   pkg/       Node/server build (CJS) — Zone 1
#   pkg/web/   browser build (ESM, wasm-pack --target web) — Zone 1
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
WASM_PACK_VERSION="0.15.0"           # generated glue is committed; upgrades are deliberate
# ────────────────────────────────────────────────────────────────────────────

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

# Cargo can reuse a wasm artifact when only Cargo.toml's package version moved,
# even though version() embeds CARGO_PKG_VERSION. Invalidate the two local
# crates when the committed package or embedded engine reports another version.
if node ../../scripts/wasm-version.mjs needs-clean Cargo.toml pkg; then
  cargo clean --manifest-path Cargo.toml --release --target wasm32-unknown-unknown \
    -p pdf-core -p pdf-core-wasm
fi

echo "Building ${PKG_NAME} (browser target, Zone 1) ..."
rm -rf pkg-web
wasmpack build --release --target web --out-dir pkg-web --scope "${OWNER}"

echo "Building ${PKG_NAME} (node main module, Zone 1) ..."
wasmpack build --release --target nodejs --scope "${OWNER}"

# wasm-pack writes pkg/.gitignore containing '*' — remove it so the built
# package can be committed (Vercel/CI have no Rust toolchain).
rm -f pkg/.gitignore pkg-web/.gitignore pkg-web/package.json pkg-web/README.md
mkdir -p pkg/web
mv pkg-web/* pkg/web/
rmdir pkg-web

# The browser build is ESM (wasm-pack --target web); mark it so resolvers that
# bypass the root "exports" conditions still parse pkg/web/*.js as modules and
# never mis-read it as CommonJS (Y-8). The package root is CJS by default.
printf '{\n  "type": "module"\n}\n' > pkg/web/package.json

# wasm-bindgen copies `&[u8]` arguments into linear memory before Rust runs.
# Guard runtime font registration and redaction PDFs in both public JS entry
# points before those allocations, while retaining Rust-side checks.
node ./postprocess-glue.mjs \
  pkg/pdf_core_wasm.js \
  pkg/web/pdf_core_wasm.js

# Runtime fonts stay outside the wasm binary. Consumers load these package
# assets and register the faces they need during adapter initialization.
rm -rf pkg/fonts
mkdir -p pkg/fonts
cp \
  ../../office-core/fonts/liberation/LiberationSans-Regular.ttf \
  ../../office-core/fonts/liberation/LiberationSans-Bold.ttf \
  ../../office-core/fonts/liberation/LiberationMono-Regular.ttf \
  pkg/fonts/
cp fonts/* pkg/fonts/

# Server worker API. Runtime is single-source at repo root; only engine op
# tables and public facades are package-specific.
cp ../../wasm-worker/worker-client.cjs ../../wasm-worker/worker-entry.cjs pkg/
cp worker.js worker.d.ts worker-ops.cjs pkg/

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
  j.files = Array.from(new Set([...(j.files || []), "web", "fonts", "worker.js", "worker.d.ts", "worker-client.cjs", "worker-entry.cjs", "worker-ops.cjs", "build-fingerprint.json"]));
  j.exports = {
    ".": {
      types: "./pdf_core_wasm.d.ts",
      require: "./pdf_core_wasm.js",
      default: "./pdf_core_wasm.js",
    },
    "./web": {
      types: "./web/pdf_core_wasm.d.ts",
      import: "./web/pdf_core_wasm.js",
      default: "./web/pdf_core_wasm.js",
    },
    "./web/pdf_core_wasm_bg.wasm": "./web/pdf_core_wasm_bg.wasm",
    "./worker": { types: "./worker.d.ts", require: "./worker.js", default: "./worker.js" },
    "./package.json": "./package.json",
  };
  fs.writeFileSync(p, JSON.stringify(j, null, 2) + "\n");
  console.log("stamped", j.name, j.version, "->", j.publishConfig.registry);
'

# Stamp the source fingerprint so consumers can detect a stale pkg/ build
# (doc-kosha tests/unit/package-freshness.test.ts recomputes and compares).
node ../../scripts/source-fingerprint.mjs pdf --out pkg/build-fingerprint.json
node ../../scripts/wasm-version.mjs verify Cargo.toml pkg

echo "Sizes:"
ls -la pkg/pdf_core_wasm_bg.wasm pkg/web/pdf_core_wasm_bg.wasm | awk '{print $5, $9}'
echo "Done. Package ready at pdf-core/wasm/pkg (commit it)."
