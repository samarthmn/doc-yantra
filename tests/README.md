# doc-yantra feature tests

Two layers of feature coverage, both driven against this repo's own
`mock-files/` corpus (real csv / md / pdf documents — the same layout
DocKosha's suites use):

| Layer | Location | Runner | What it proves |
| --- | --- | --- | --- |
| **PDF npm package** (`@samarthmn/doc-yantra`) | `tests/package-features.test.mjs` | `node --test` | Every PDF export of the built wasm package behaves correctly on real files. |
| **Redaction package contract** | `tests/redact-package.test.mjs` | `node --test` | `redact_pdf` removes content, validates JSON, preserves recoverable errors, and guards wasm allocation. |
| **Office npm package** (`@samarthmn/doc-yantra-office`) | `tests/office.test.mjs`, `tests/office-font-provider.test.mjs`, `tests/fontsource-provider*.test.mjs`, `tests/noto-fallback.test.mjs` | `node --test` | Fontless staged/on-demand APIs, deterministic mock provider, Node/browser Fontsource resolution, fallback boot/cache recovery, telemetry, format coverage, and pre-copy guards work through the built package. |
| **Release contract** | `tests/release-contract.test.mjs` | `node --test` | Versions align and `npm pack --dry-run` proves the Office artifact contains no font files or embedded font programs. |
| **Rust engine** (`pdf-core`) | `pdf-core/tests/*.rs` | `cargo test` | The crate's features, plus a real-file end-to-end pipeline. |

## Running the package suite (Node)

From the repo root:

```bash
node --test tests/
```

The suite loads the **built** package directly by path — no install, no
bundler — mirroring a Node/Vercel consumer:

```js
require("../pdf-core/wasm/pkg")
require("../office-core/wasm/pkg")
```

Both committed packages must exist. A missing or unloadable PDF or Office
package fails this suite. A standalone source checkout that has not built
Office wasm yet may run the Rust suites, but it is not release-ready until
`office-core/wasm/build.sh` and this package suite pass.

Office tests never use a public CDN. `tests/fixtures/mock-office-font-provider.cjs`
serves pinned local OFL fixture bytes through the same provider/cache path used
by production hosts; worker tests inject it for both authored and fallback
registrations. This is the settled 6B CI strategy (mock provider + local fixture
fonts), rather than a seeded cache.

### Notes on the invocation

- The suite is ESM (`.mjs`, using `node:test`). `tests/all.test.mjs` imports
  both package suites. There is **no root
  `package.json`** — the repo root intentionally has none. A tiny
  `tests/package.json` (`{"type":"module","main":"all.test.mjs"}`)
  exists only so the `node --test tests/` **path form** resolves on Node 24+
  (which treats a positional path as a glob and no longer scans a bare
  directory).
- To run the aggregate entry point directly:
  ```bash
  node --test tests/all.test.mjs
  ```
- Avoid bare `node --test`: auto-discovery runs each suite directly and again
  through `all.test.mjs`, doubling the test run.
- Requires a Node with the stable test runner (Node ≥ 18; developed on 26).

### What the package suite covers

Main module (`@samarthmn/doc-yantra`):

- `version`, `validate_pdf`, `page_count` — including rejection of non-PDF bytes.
- `csv_to_pdf`, `markdown_to_pdf`, `text_to_pdf` — `%PDF` magic + `page_count >= 1`
  on both mock variants.
- `apply_watermark` — text (single/grid), image (real PNG), and hybrid; asserts
  the output *differs* from the input, preserves the page count, and actually
  *contains the mark* (verified by re-parsing: FlateDecode streams are inflated
  and searched for the drawn `(MARK) Tj`).
- `merge_pdfs` — with and without a watermark payload; asserts merged
  `page_count == sum of inputs`, single-input passthrough, and error on an empty
  list / an invalid member.
- `redact_pdf` — removes selected text, returns a valid PDF, rejects malformed
  JSON, prefixes fail-closed misses with `unsupported:`, and guards input before
  the wasm copy.

Office → PDF is covered through the separate
`@samarthmn/doc-yantra-office` package. Inputs outside its fail-closed envelope
fail closed; there is no LibreOffice or HTTP conversion fallback.

## Running the Rust suite

```bash
cd pdf-core && cargo test
```

Requires Rust ≥ 1.89 and a resolvable pdfium (see the crate's pdfium setup).
Relevant to the mock corpus:

- `tests/mockfiles_pipeline_test.rs` — a real-file end-to-end pipeline: every
  in-process convertible source (csv / md via the generator) is driven through
  **convert → watermark → merge → extract → page_count → raster**, asserting
  each stage.

The remaining `tests/*.rs` cover the individual engine modules (assembly,
watermark, meta, generate, raster, extract, redact, forms) with synthetic
fixtures and are unchanged by this corpus work.
