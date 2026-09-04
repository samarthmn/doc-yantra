#!/usr/bin/env bash
#
# Builds the pdf-core WebAssembly package (both targets) and stamps the npm
# metadata needed to publish it to the public npm registry. The output (pkg/) is
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

# Keep debug/source paths deterministic and avoid leaking a builder's checkout
# or Cargo home into the committed WASM bytes.
REPO_ROOT="$(cd ../.. && pwd -P)"
CARGO_ROOT="${CARGO_HOME:-${HOME}/.cargo}"
RUSTFLAGS="${RUSTFLAGS:+${RUSTFLAGS} }--remap-path-prefix=${REPO_ROOT}=/doc-yantra --remap-path-prefix=${CARGO_ROOT}=/cargo"
export RUSTFLAGS

# ─── Package identity ────────────────────────────────────────────────────────
OWNER="samarthmn"
REPO="doc-yantra"
PKG_NAME="@samarthmn/doc-yantra"
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

# The node and browser builds contain the same engine bytes. Keep the root
# binary as the one package copy and point the browser glue at it.
if ! cmp -s pkg/pdf_core_wasm_bg.wasm pkg/web/pdf_core_wasm_bg.wasm; then
  echo "error: node and browser PDF wasm binaries differ; refusing to discard one." >&2
  exit 1
fi
rm -f pkg/web/pdf_core_wasm_bg.wasm
rm -rf pkg/fonts

# Server worker API. Runtime is single-source at repo root; only engine op
# tables and public facades are package-specific.
cp ../../wasm-worker/worker-client.cjs ../../wasm-worker/worker-entry.cjs pkg/
cp worker.js worker.d.ts worker-ops.cjs font-provider.cjs font-provider.d.ts pkg/
cp ../../packages/doc-yantra-fonts/fonts/manifest.json pkg/font-manifest.json
cp ../../LICENSE ../../COMMERCIAL-LICENSING.md ../../THIRD_PARTY_NOTICES.md pkg/
cp THIRD_PARTY_RUST_LICENSES.txt pkg/
cp ../assets/cmaps/LICENSE-adobe-cmap-resources.md pkg/NOTICE-ADOBE-CMAP.md

# wasm-pack derives the name from the crate and does not emit repository,
# license, publishConfig, or exports; set the final metadata.
REPO_URL="git+https://github.com/${OWNER}/${REPO}.git" PKG_NAME="${PKG_NAME}" \
node -e '
  const fs = require("fs"), p = "pkg/package.json";
  const j = JSON.parse(fs.readFileSync(p, "utf8"));
  j.name = process.env.PKG_NAME;
  j.repository = { type: "git", url: process.env.REPO_URL };
  j.license = "AGPL-3.0-only";
  j.publishConfig = { registry: "https://npm.pkg.github.com" };
  j.files = Array.from(new Set([...(j.files || []), "web", "worker.js", "worker.d.ts", "worker-client.cjs", "worker-entry.cjs", "worker-ops.cjs", "font-provider.cjs", "font-provider.d.ts", "font-manifest.json", "build-fingerprint.json", "LICENSE", "COMMERCIAL-LICENSING.md", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_RUST_LICENSES.txt", "NOTICE-ADOBE-CMAP.md"]));
  j.peerDependencies = { ...(j.peerDependencies || {}), "@samarthmn/doc-yantra-fonts": j.version };
  j.peerDependenciesMeta = { ...(j.peerDependenciesMeta || {}), "@samarthmn/doc-yantra-fonts": { optional: true } };
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
    "./pdf_core_wasm_bg.wasm": "./pdf_core_wasm_bg.wasm",
    "./web/pdf_core_wasm_bg.wasm": "./pdf_core_wasm_bg.wasm",
    "./font-provider": { types: "./font-provider.d.ts", require: "./font-provider.cjs", default: "./font-provider.cjs" },
    "./font-manifest.json": "./font-manifest.json",
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
ls -la pkg/pdf_core_wasm_bg.wasm | awk '{print $5, $9}'
echo "Done. Package ready at pdf-core/wasm/pkg (commit it)."
