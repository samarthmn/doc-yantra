# office-core — DocKosha's pure-Rust Office → PDF engine

Own OOXML parsing, own layout, own PDF emission. **No LibreOffice, Chromium,
pdfium, or Typst in the conversion path** — only small, permissive, pure-Rust
leaf crates. Ships as a native CLI (`officecore`) and a WebAssembly npm package
(`@samarthmn/doc-yantra-office`), mirroring `pdf-core`'s conventions.

Two contracts anchor everything (see `../hyper-agent/office-engine/README.md`):

1. **Fail-closed envelope.** `convert()` returns `Err(Unsupported{code})` for
   anything outside the documented envelope — never a silently-degraded page.
2. **DocProfile.** `profile()` predicts the same verdict *without* layout, so the
   DocKosha router can decide before converting.

## Formats

| Format | Status | Notes |
|---|---|---|
| DOCX | full v1 | paragraphs/runs, style cascade, lists, tables (nested/merges), headers/footers, PAGE/NUMPAGES fields, inline images, hyperlinks, sections, breaks, tracked-changes final view |
| PPTX | common-deck v1 | master/layout/slide inheritance, placeholder text, fills, pictures, tables (bounding-box geometry — see Deferred) |
| XLSX | print v1 | number-format engine, MDW column widths, gridlines, cell fills, multi-sheet pagination |

## Envelope (summary)

`convert()` is fail-closed: anything not enumerated is `Unsupported`. The stable
`reasons[].code` strings appear in both `Err(Unsupported)` and the DocProfile
JSON. Out of envelope (routed to the LibreOffice fallback):

- legacy binary (`.doc`/`.ppt`/`.xls`) → `legacy_format`; encrypted → `encrypted`
- footnotes/endnotes, text boxes, floating/anchored drawings, multi-column
- charts, SmartArt, OLE, WordArt, equations
- complex-script (Arabic/Indic/Thai) and RTL; CJK without a covering font
- image crops and unsupported image containers (GIF/EMF/WMF/TIFF)
- xlsx: uncached formulas, sheet drawings/charts, conditional formatting
- anything genuinely unknown → `unknown_element`

## Quick start

**CLI** (native):

```sh
officecore convert report.docx -o report.pdf     # exit 3 on Unsupported
officecore profile report.docx | jq .            # DocProfile JSON
```

**npm** (`@samarthmn/doc-yantra-office`, Node/Vercel + browser):

```js
const engine = require("@samarthmn/doc-yantra-office");
const json = engine.office_profile(bytes, "docx");     // never throws for valid input
const pdf  = engine.office_to_pdf(bytes, "docx");       // throws "unsupported:<code>: …" off-envelope
engine.register_font(brandFontTtf);                     // optional brand fonts
```

The `unsupported:` message prefix and the 50 MiB pre-copy input guard are a
stable API (DocKosha parses them). Trap contract is identical to `pdf-core`:
recoverable failures are `JsError`; a panic/OOM traps and the instance must be
discarded.

## Fidelity

The `fidelity/` harness (dev-only) diffs our output against **Word's own PDF
export** with SSIM. `cargo run -p fidelity -- seed` regenerates the synthetic
corpus; a human exports ground truths (plan human task H3); `run --format docx`
scores and emits `fidelity-report/index.html`. Calibration constants and their
adjustments are logged in `LAYOUT_CALIBRATION.md`.

## Design notes

- **Determinism.** Byte-identical output for identical input: object numbering by
  emission order, `/ID` = first 16 bytes of `SHA-256(input ‖ version)`,
  CreationDate/ModDate from `docProps/core.xml` (else a fixed literal), streams
  FlateDecode-compressed.
- **Fonts.** Bundled metric-compatible OFL fonts (Carlito↔Calibri,
  Caladea↔Cambria, Liberation↔Arial/Times/Courier), OS/2 Windows metrics for
  line-height parity, subset Type0/CIDFontType2 embedding with ToUnicode.
- **wasm-first.** The engine compiles to `wasm32-unknown-unknown` from day one;
  CI enforces it. `--no-default-features --features wasm,bundled-fonts`.

## Deferred (record, don't build)

- **PPTX preset geometry.** The full DrawingML guide-formula evaluator and
  `custGeom` path rendering need ECMA-376's `presetShapeDefinitions.xml` (a large
  vendored asset, like the OFL fonts). v1 renders shape fills against their
  bounding box; drop the XML in and swap the geometry backend.
- **N-API native addon** (napi-rs v3) — wasm CJS suffices for DocKosha on Vercel;
  a native addon becomes worthwhile for a self-hosted service or >50 MiB docs.
- **PDF/A-2b** output — for archival requirements.
- **Complex-script shaping** via `harfrust` — Arabic/Indic/Thai + bidi reorder.
- **Charts** — own vector renderer rather than routing to the service.
- **Slim web builds** — an in-browser font-fetch strategy instead of bundling all
  20 faces.
