# doc-yantra — DocKosha's native PDF engine (Rust + WebAssembly)

This repo is DocKosha's **AGPL-3.0-only PDF engine** and its distributable
**npm package**. It contains:

| Path                                 | What it is                                                                                                                                                                                                                                                                                     |
| ------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `pdf-core/`                          | The Rust engine: merge, split, watermark, metadata, CSV/Markdown/text/image → PDF, and pure-Rust redaction (Zone 1), plus pdfium-backed raster/extract/forms and legacy redaction cross-validation (Zone 2, native-only). See [`pdf-core/README.md`](pdf-core/README.md).                      |
| `pdf-core/wasm/`                     | The **`@samarthmn/doc-yantra`** npm package (built output committed at `pdf-core/wasm/pkg`; consumers never need Rust). Two modules: main (Node CJS), `./web` (browser ESM); both expose Zone 1, including fail-closed redaction. Office→PDF is provided by the separate Office package below. |
| `office-core/`                       | The pure-Rust **Office → PDF** engine: DOCX/PPTX/XLSX layout + PDF emission, fail-closed envelope, DocProfile router contract. No LibreOffice/Chromium/pdfium in the conversion path. See [`office-core/README.md`](office-core/README.md).                                                    |
| `office-core/wasm/`                  | The **`@samarthmn/doc-yantra-office`** npm package (Node CJS + browser ESM). `office_profile()` / `office_to_pdf()` / `register_font()`. DocKosha routes all office conversions here; envelope misses fail closed (no LibreOffice anywhere).                                                   |
| `packages/dockosha-provider/`        | The **`@samarthmn/dockosha-provider-docyantra`** TypeScript adapter. Its committed `dist/` and exact engine pins release in lockstep with both wasm engines.                                                                                                                                   |
| `packages/doc-yantra-fonts/`         | The optional **`@samarthmn/doc-yantra-fonts`** offline pack. It owns the one canonical copy of all 17 hash-pinned distribution fonts and their licences; neither engine package contains font binaries.                                                                                        |
| `.github/workflows/publish-wasm.yml` | PR/manual readiness checks: rebuilds and audits the four lockstep packages without publishing.                                                                                                                                                                                                 |
| `.github/workflows/publish-npm.yml`  | Owner-triggered public npm publication after immutable version and tarball checks.                                                                                                                                                                                                             |

The first-party source and wrapper code is licensed under the AGPL-3.0-only
terms in [`LICENSE`](LICENSE). Enterprise licensing is available only through a
separate signed agreement; see [`COMMERCIAL-LICENSING.md`](COMMERCIAL-LICENSING.md).
See
[`CONTRIBUTING.md`](CONTRIBUTING.md), [`SECURITY.md`](SECURITY.md),
[`SUPPORT.md`](SUPPORT.md), and [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).

> **History note:** the DocYantra HTTP service (NestJS + Gotenberg) has been
> retired and deleted from the DocKosha app repo. These packages are the only
> conversion path; anything the engines cannot prove fails closed.

## Quick start (consume the package)

Full guide: **[CONSUMING.md](CONSUMING.md)**. Consume the public npm registry
version:

```jsonc
// app package.json — public npm registry
"@samarthmn/doc-yantra": "0.0.22",
"@samarthmn/doc-yantra-office": "0.0.22"
```

> Releases stay on the **0.0.x** line. On `0.0.x`, a caret pins the exact patch
> (`^0.0.7` resolves only `0.0.7`), so pin the exact version and bump it in the
> consumer each time a new patch is published.

For local engine iteration, build and pack the workspace packages into a
disposable consumer project; keep application manifests on public registry
versions.

The default install is fontless. Unicode text operations resolve only the
required exact faces, in this order: memory cache, persistent cache, installed
offline font pack, then immutable first-party HTTPS assets. Font-independent
merge, redaction, validation, and page counting perform no font I/O. Install
`@samarthmn/doc-yantra-fonts` at the same exact version only when the deployment
must work fully offline.

```ts
import * as pdf from "@samarthmn/doc-yantra"; // server-side only (CJS)

const merged = pdf.merge_pdfs(
  [new Uint8Array(a), new Uint8Array(b)],
  JSON.stringify({ pattern: "diagonal_grid", lines: ["CONFIDENTIAL"] }),
  undefined,
);
```

The watermark payload keeps the exact shape of the retired DocYantra
service's `/docyantra/pdf/merge` endpoint, so consumers migrated without
reshaping data. One deliberate refinement: when a grid payload omits
`xSpacingPt`/`ySpacingPt`, the default spacing now scales with each page's
box relative to US-Letter (Letter output is byte-identical to the historical
fixed 320pt lattice; slide-sized and other non-Letter pages get the same
visual stamp density instead of one sparse row). Explicit spacing values are
honored exactly as before.

## Developing the engine

```bash
cd pdf-core
PDFIUM_SHA256_LINUX_X64=<reviewed-sha256> ./scripts/fetch-pdfium.sh
cargo test                  # full engine suite (Zone 1 + Zone 2)
cargo build --release       # CLI at target/release/pdfcore

# rebuild the npm package after Rust changes (updates pkg/)
cd wasm && ./build.sh

# rebuild the fontless Office package after office-core changes
cd ../../office-core/wasm && ./build.sh
```

Requires Rust ≥ 1.89 (`rust-version` is pinned in both Cargo.tomls). The wasm build compiles Zone 1 only
(`--no-default-features --features wasm`); Zone 2 needs native pdfium and stays
out of the package. Its legacy redactor exists only for native
cross-validation. Zone-1 redaction has no HTTP fallback.

The Office wasm package is also built with `--no-default-features --features wasm`.
It contains no fonts: hosts discover canonical requests with
`office_font_requests()`, supply validated SFNT bytes with
`office_register_font()`, then profile and convert. Native Office builds retain
the default `bundled-fonts` feature for local engine tests.

### Clean and rebuild Rust artifacts

Cargo stores compiled dependencies and build artifacts in `target/` directories.
From the repository root, remove all DocYantra `target/` directories with:

```bash
find pdf-core office-core text-core -type d -name target -prune -exec rm -rf {} +
```

Recreate the active crates' artifacts when needed by rebuilding them:

```bash
for manifest in \
  pdf-core/Cargo.toml pdf-core/wasm/Cargo.toml \
  office-core/Cargo.toml office-core/wasm/Cargo.toml \
  text-core/Cargo.toml; do
  cargo build --manifest-path "$manifest"
done
```

`uv sync` restores Python dependencies only; it does not recreate Cargo's Rust
build artifacts.

## Public verification surface

The exported candidate contains only self-contained checks in `public-tests/`.
Run them from its root with `node --test public-tests/*.test.mjs`, then follow
the PDF and Office build/test commands in `public-tests/README.md`. Owner-only
golden-document validation is a separate scheduled/manual gate and requires
the provenance-approved intake described in `FIXTURE_PROVENANCE.md`; it is not
part of the public candidate.

## Releasing

Releases stay on the **0.0.x** line — only the patch increments. The release
script is a side-effect-free preparation gate: it validates lockstep manifests,
audits every `npm pack`, enforces core/font byte budgets, and checks the public
source candidate without changing repository or registry state:

```bash
scripts/release.sh            # validate the 0.0.22 candidate
```

Published versions are immutable. The owner-only npm workflow performs the
public-registry reuse check and publishes only after all four audited tarballs
are prepared; this checkout does not publish.
