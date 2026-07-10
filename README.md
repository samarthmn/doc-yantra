# doc-yantra — DocKosha's native PDF engine (Rust + WebAssembly)

This repo is DocKosha's **proprietary PDF engine** and its distributable
**npm package**. It contains:

| Path | What it is |
| --- | --- |
| `pdf-core/` | The Rust engine: merge, split, watermark, metadata, CSV/Markdown/text/image → PDF (Zone 1), plus pdfium-backed raster/extract/redact/forms (Zone 2, native-only). See [`pdf-core/README.md`](pdf-core/README.md). |
| `pdf-core/wasm/` | The **`@samarthmn/doc-yantra`** npm package (built output committed at `pdf-core/wasm/pkg`; consumers never need Rust). Two modules: main (Node CJS — watermark/merge/csv/md, 1.7MB), `./web` (browser ESM — Zone 1 for client-side previews, 1.7MB). Office→PDF is provided by the separate Office package below; out-of-envelope files use the compatibility service. |
| `office-core/` | The pure-Rust **Office → PDF** engine: DOCX/PPTX/XLSX layout + PDF emission, fail-closed envelope, DocProfile router contract. No LibreOffice/Chromium/pdfium in the conversion path. See [`office-core/README.md`](office-core/README.md). |
| `office-core/wasm/` | The **`@samarthmn/doc-yantra-office`** npm package (Node CJS + browser ESM). `office_profile()` / `office_to_pdf()` / `register_font()`. DocKosha routes office conversions here engine-first, LibreOffice as fallback. |
| `.github/workflows/publish-wasm.yml` | Rebuilds and tests both engines/packages, runs the blocking DOCX fidelity gate, validates the tag version, then publishes **both** packages to **GitHub Packages** for a `0.0.*` tag. |
| `.github/workflows/office-core-ci.yml` | Lints/tests office-core + proves wasm-compilability on every change. |

> **Not in this repo:** the DocYantra HTTP service (NestJS + Gotenberg —
> Office→PDF, HTML→PDF, redaction endpoints). That still lives in the
> DocKosha app repo at `doc-kosha/doc-yantra/` and keeps serving the
> features that need LibreOffice/Chromium/pdfium. This repo is only the
> ported, in-process engine.

## Quick start (consume the package)

Full guide: **[CONSUMING.md](CONSUMING.md)**. The package is published to GitHub
Packages; consume the registry version (this is how DocKosha ships today):

```jsonc
// app package.json — needs @samarthmn:registry in .npmrc + a read:packages token
"@samarthmn/doc-yantra": "0.0.7",
"@samarthmn/doc-yantra-office": "0.0.7"
```

> Releases stay on the **0.0.x** line. On `0.0.x`, a caret pins the exact patch
> (`^0.0.7` resolves only `0.0.7`), so pin the exact version and bump it in the
> consumer each time a new patch is published.

For local engine iteration you can temporarily `link:` a sibling checkout
(`link:../doc-yantra/pdf-core/wasm/pkg`), but switch back to the registry range
before committing — see CONSUMING.md § "Local development".

```ts
import * as pdf from "@samarthmn/doc-yantra"; // server-side only (CJS)

const merged = pdf.merge_pdfs(
  [new Uint8Array(a), new Uint8Array(b)],
  JSON.stringify({ pattern: "diagonal_grid", lines: ["CONFIDENTIAL"] }),
  undefined,
);
```

The watermark payload is byte-compatible with the DocYantra service's
`/docyantra/pdf/merge` endpoint, so apps can switch between the HTTP service
and this package without reshaping data.

## Developing the engine

```bash
cd pdf-core
./scripts/fetch-pdfium.sh   # one-time: pdfium dylib for native Zone 2
cargo test                  # full engine suite (Zone 1 + Zone 2)
cargo build --release       # CLI at target/release/pdfcore

# rebuild the npm package after Rust changes (updates pkg/ — commit it)
cd wasm && ./build.sh
```

Requires Rust ≥ 1.89 (`rust-version` is pinned in both Cargo.tomls). The wasm build compiles Zone 1 only
(`--no-default-features --features wasm`); Zone 2 needs native pdfium and is
served by the DocYantra HTTP service instead.

## Releasing

Releases stay on the **0.0.x** line — only the patch increments. From a clean
`main`, one script picks the next `x`, bumps all four engine/wasm manifests,
rebuilds and tests both packages, runs the blocking DOCX fidelity corpus, then
commits, tags, and atomically pushes `main` + `0.0.x`. The tag workflow repeats
the gates before publishing to GitHub Packages:

```bash
pdf-core/scripts/release.sh            # auto: next patch, commit, tag, push
pdf-core/scripts/release.sh 7          # explicit: release 0.0.7
DRY_RUN=1 pdf-core/scripts/release.sh  # preview only

# then, in a follow-up, bump DocKosha's package.json pin to the new 0.0.x + pnpm install
```

Published versions are immutable — the script refuses to reuse or go below an
existing tag; always bump.
