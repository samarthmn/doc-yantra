# Tests

Every test suite and fixture in the repository lives under this folder.

| Folder | Runner | What it covers |
| --- | --- | --- |
| `node/` | `node --test tests/node/all.test.mjs` | The **built** npm packages (`pdf-core/wasm/pkg`, `office-core/wasm/pkg`, the adapter and the font pack), loaded by path with no install: every PDF export, redaction, Office conversion, font providers, workers, error envelopes and the release contract. |
| `smoke/` | `node --test tests/smoke/*.test.mjs` | Self-contained package checks that need no document fixtures. `scripts/release.sh` runs these. |
| `pdf-core/` | `cargo test` in `pdf-core/` | `pdf-core` integration tests (assembly, watermark, meta, generate, raster, extract, redaction, forms, real-file pipeline). |
| `office-core/` | `cargo test` in `office-core/` | `office-core` integration tests (DOCX/PPTX/XLSX parsing, layout, fonts, PDF emission, envelope parity). |
| `text-core/` | `cargo test` in `text-core/` | Shaping and bidi tests. |
| `fidelity/` | `cargo test --release` in `office-core/fidelity/` | Fidelity-harness parity tests against the legacy goldens. |
| `fixtures/` | — | Shared inputs: `samples/` (csv, md, pdf, docx, pptx, xlsx and image samples), `corpus/` (the fidelity reference corpus and legacy goldens) and `pdf-core/` (small engine fixtures). |

Each crate's `Cargo.toml` sets `autotests = false` and lists its integration
tests from this folder as `[[test]]` targets, so `cargo test` in the crate
directory runs them as before.

## Self-contained checks

These use committed generated packages and synthetic bytes only:

```bash
node --test tests/smoke/*.test.mjs tests/node/font-cdn-tools.test.mjs tests/node/license-metadata.test.mjs
cargo test --manifest-path pdf-core/Cargo.toml --lib --no-default-features --features wasm
cargo metadata --manifest-path office-core/Cargo.toml --locked --no-deps --format-version 1
cargo check --manifest-path office-core/Cargo.toml --locked --lib --no-default-features --features wasm
```

## Package regression suite (Node)

```bash
node --test tests/node/all.test.mjs
```

- Both committed wasm packages must exist; rebuild them with
  `pdf-core/wasm/build.sh` and `office-core/wasm/build.sh` after Rust changes.
- `tests/node/package.json` declares the PDF.js paint regression's only
  dependencies (`pdfjs-dist@3.11.174`, DocKosha's viewer, and `@napi-rs/canvas`).
  Run `npm install` inside `tests/node/` once to enable
  `office-cjk-pdfjs-paint.test.mjs`; without it that file skips loudly.
- Office tests never use a public CDN: `node/fixtures/mock-office-font-provider.cjs`
  serves pinned local OFL fonts through the production provider/cache path.
- Avoid bare `node --test`: auto-discovery runs each suite directly and again
  through `all.test.mjs`.
- Requires Node ≥ 18.

## Rust suites

```bash
cd pdf-core && cargo test        # needs pdfium; see pdf-core/README.md
cd office-core && cargo test
cd text-core && cargo test
```

## Fidelity harness

`office-core/fidelity` converts each corpus case with office-core, rasterizes
our PDF and the Word ground truth with pdfium, and scores them (SSIM plus
vector geometry). Every core case must pass:

```bash
cd office-core/fidelity
cargo run --release -- run --format docx   # also: pptx, xlsx
```
