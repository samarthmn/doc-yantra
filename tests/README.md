# doc-yantra feature tests

Two layers of feature coverage, both driven against this repo's own
`mock-files/` corpus (real docx / pptx / xlsx / csv / md / pdf documents — the
same layout DocKosha's suites use):

| Layer | Location | Runner | What it proves |
| --- | --- | --- | --- |
| **npm package** (`@samarthmn/doc-yantra`) | `tests/package-features.test.mjs` | `node --test` | Every export of the built wasm package (main + `office/`) behaves correctly on real files. |
| **Rust engine** (`pdf-core`) | `pdf-core/tests/*.rs` | `cargo test` | The crate's features, plus a real-file end-to-end pipeline. |

## Running the package suite (Node)

From the repo root:

```bash
node --test tests/
```

The suite loads the **built** package directly by path — no install, no
bundler — mirroring a Node/Vercel consumer:

```js
require("../pdf-core/wasm/pkg/pdf_core_wasm.js")        // main module
require("../pdf-core/wasm/pkg/office/pdf_core_wasm.js") // office submodule
```

so `pdf-core/wasm/pkg/` must be built first (see `pdf-core/wasm/build.sh`).

### Notes on the invocation

- The suite is ESM (`.mjs`, using `node:test`). There is **no root
  `package.json`** — the repo root intentionally has none. A tiny
  `tests/package.json` (`{"type":"module","main":"package-features.test.mjs"}`)
  exists only so the `node --test tests/` **path form** resolves on Node 24+
  (which treats a positional path as a glob and no longer scans a bare
  directory).
- Equivalent version-agnostic invocations, if you prefer them:
  ```bash
  node --test                 # from repo root: auto-discovers *.test.mjs
  node --test 'tests/*.mjs'   # explicit glob
  ```
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

Office submodule (`@samarthmn/doc-yantra/office`):

- `office_to_pdf` converts docx / pptx / xlsx (both mock variants) **warning-free**
  to a valid, countable PDF, and the result flows through downstream ops.
- Extension scope: `doc` / `ppt` / `xls` / `xlsm` / unknown all **throw** (they
  route to LibreOffice in production).
- Robustness: a renamed file (xlsx bytes sent as `docx`) is rejected **fast**
  (< 2 s), and corrupt / empty input is rejected.

## Running the Rust suite

```bash
cd pdf-core && cargo test
```

Requires Rust ≥ 1.89 and a resolvable pdfium (see the crate's pdfium setup).
Relevant to the mock corpus:

- `tests/office_test.rs` — office→PDF conversion + the extension-scope /
  fast-reject / corrupt / empty contract, all against `../mock-files`.
- `tests/mockfiles_pipeline_test.rs` — a real-file end-to-end pipeline: every
  convertible source (docx / pptx / xlsx via office2pdf, csv / md via the
  generator) is driven through **convert → watermark → merge → extract →
  page_count → raster**, asserting each stage.

The remaining `tests/*.rs` cover the individual engine modules (assembly,
watermark, meta, generate, raster, extract, redact, forms) with synthetic
fixtures and are unchanged by this corpus work.
