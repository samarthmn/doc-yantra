# doc-yantra — DocKosha's native PDF engine (Rust + WebAssembly)

This repo is DocKosha's **proprietary PDF engine** and its distributable
**npm package**. It contains:

| Path | What it is |
| --- | --- |
| `pdf-core/` | The Rust engine: merge, split, watermark, metadata, CSV/Markdown/text/image → PDF (Zone 1), plus pdfium-backed raster/extract/redact/forms (Zone 2, native-only). See [`pdf-core/README.md`](pdf-core/README.md). |
| `pdf-core/wasm/` | Zone 1 compiled to WebAssembly for Node — the **`@samarthmn/doc-yantra`** npm package. Built output is committed at `pdf-core/wasm/pkg` so consumers never need a Rust toolchain. |
| `.github/workflows/publish-wasm.yml` | Publishes the package to **GitHub Packages** when a `wasm-v*` tag is pushed. |

> **Not in this repo:** the DocYantra HTTP service (NestJS + Gotenberg —
> Office→PDF, HTML→PDF, redaction endpoints). That still lives in the
> DocKosha app repo at `doc-kosha/doc-yantra/` and keeps serving the
> features that need LibreOffice/Chromium/pdfium. This repo is only the
> ported, in-process engine.

## Quick start (consume the package)

Full guide: **[CONSUMING.md](CONSUMING.md)**. Short version — until this repo
is pushed/published, consume it directly from the folder:

```jsonc
// app package.json (sibling checkout)
"@samarthmn/doc-yantra": "link:../doc-yantra/pdf-core/wasm/pkg"
```

After publishing to GitHub Packages:

```jsonc
"@samarthmn/doc-yantra": "^0.0.1"
```

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
cargo test                  # 162 tests
cargo build --release       # CLI at target/release/pdfcore

# rebuild the npm package after Rust changes (updates pkg/ — commit it)
cd wasm && ./build.sh
```

Requires Rust ≥ 1.88. The wasm build compiles Zone 1 only
(`--no-default-features --features wasm`); Zone 2 needs native pdfium and is
served by the DocYantra HTTP service instead.

## Releasing

```bash
# bump version in pdf-core/wasm/Cargo.toml, rebuild, commit pkg/
git tag wasm-v0.1.1 && git push origin main wasm-v0.1.1   # CI publishes
```
