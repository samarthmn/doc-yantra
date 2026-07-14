# office-core — DocKosha's pure-Rust Office → PDF engine

Own OOXML parsing, own layout, own PDF emission. **No LibreOffice, Chromium,
pdfium, or Typst in the conversion path** — only small, permissive, pure-Rust
leaf crates. Ships as a native CLI (`officecore`) and a WebAssembly npm package
(`@samarthmn/doc-yantra-office`), mirroring `pdf-core`'s conventions.

Two contracts anchor everything (see `../hyper-agent/office-engine/README.md`):

1. **Fail-closed envelope.** `convert()` returns `Err(Unsupported{code})` for
   anything outside the documented envelope — never a silently-degraded page.
2. **DocProfile.** `profile()` predicts the same verdict without PDF emission;
   documents with multiple body anchors may run the layout preflight so profile
   and conversion cannot disagree about an overlap.

## Formats

| Format | Status | Notes |
|---|---|---|
| DOCX | validated v1 envelope | paragraphs/runs, style cascade, lists, tables (nested/merges), headers/footers, PAGE/NUMPAGES fields, inline images, bounded body anchors, hyperlinks, sections, breaks, tracked-changes final view |
| PPTX | common-deck v1 | master/layout/slide inheritance, placeholder text, rectangular fills, pictures, tables; non-rectangular geometry fails closed |
| XLSX | print v1 | number-format engine, MDW column widths, gridlines, cell fills, multi-sheet pagination |

## Envelope (summary)

`convert()` is fail-closed: anything not enumerated is `Unsupported`. The stable
`reasons[].code` strings appear in both `Err(Unsupported)` and the DocProfile
JSON. DocKosha records an out-of-envelope conversion as failed and keeps the
original asset available; it does not invoke a compatibility converter.

DOCX anchor support is deliberately bounded. The engine renders top-level body
`wp:anchor` drawings only when `simplePos` is disabled, horizontal placement is
margin-relative (offset or left/center/right alignment), vertical placement is
paragraph-relative (offset), and `allowOverlap`, `behindDoc`, and
`relativeHeight` are valid. Supported wrapping is `wrapNone`,
`wrapTopAndBottom`, rectangular `wrapSquare`, and `wrapTight` with a canonical
full-image rectangular polygon. Authored wrap distances and front/behind paint
order are preserved. A paragraph may contain at most one anchor, and multiple
body anchors must pass the layout overlap preflight.

Other anchor forms fail with the typed `anchored_drawing` code. This includes
`simplePos=true`, unsupported reference origins or side policies, vertical
alignment, `wrapThrough`, non-rectangular tight polygons, relative sizing,
hidden or overlap-disabled anchors, and anchors inside frames, table cells,
headers/footers, text boxes, or notes. The engine never substitutes no-wrap or
silently omits an out-of-envelope anchor.

Other representative envelope misses include:

- legacy binary (`.doc`/`.ppt`/`.xls`) → `legacy_format`; encrypted → `encrypted`
- feature forms outside their documented bounded subsets
- charts, SmartArt, OLE, WordArt, equations
- PPTX non-rectangular preset geometry and custom geometry
- complex-script (Arabic/Indic/Thai) and RTL; CJK without a covering font
- image crops and unsupported image containers (GIF/EMF/WMF/TIFF)
- xlsx: uncached formulas, sheet drawings/charts, conditional formatting
- packages past the decompression budgets (entry count, 64 MiB per part,
  proportional total) → `decompression_limit` — an envelope miss, not corruption
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

The server worker also accepts `extraFontDirs: string[]`. At boot it registers
every `.ttf`, `.ttc`, `.otf`, and `.otc` file found there in deterministic path
order, after the bundled faces. DocKosha can point this at the PDF package's
`fonts/` directory so CJK and emoji assets are shipped once for both engines.

The `unsupported:` message prefix and the 50 MiB pre-copy input guard are a
stable API (DocKosha parses them). Trap contract is identical to `pdf-core`:
recoverable failures are `JsError`; a panic/OOM traps and the instance must be
discarded.

## Fidelity

The `fidelity/` harness (dev-only) diffs our output against **Word's own PDF
export** with SSIM. `cargo run -p fidelity -- seed` regenerates the synthetic
corpus; committed DOCX ground truths were exported from Microsoft Word and
their producer and export-date provenance are recorded in each manifest.
`run --format docx` requires every selected ground truth, blocks on any core
failure or an extended-tier pass rate below 90%, and emits
`tmp/fidelity-report/index.html` at the repository root. Pass
`--report-dir <path>` to keep a named audit report elsewhere under the repo.
Calibration constants and their adjustments are logged in
`LAYOUT_CALIBRATION.md`.

The frozen `fidelity-corpus/docx/complex/` regression identifies both reference
files by SHA-256:

- DOCX: `8c2a8c4725e77bbfb4ddc7925674dc221611a39b876dbd7ea2efc829e70b8530`
- Word PDF: `e808b449e27436d74f81d5d5df9f9fe4daa4cc28e426ec1619bbe3fcfb55255c`

The direct converter and final WASM worker produce the same deterministic PDF
(`SHA-256 220480cc75cf26ceaa589503a7e992d90fe6ce56d29db88dc224737bdb90614d`):
six 612×792-point pages, the same front/behind layer order as Word, and image
bounds within 0.06 pt. Tight wrapping releases text on the first baseline below
the authored polygon, and table flow includes the top and bottom outer-border
half-strokes; the table edges are within 0.4 pt and the following baseline is
within 0.3 pt of Word. Per-page 8×8 luma SSIM is 0.962378, 0.972241, 0.973315,
0.971328, 0.927610, and 0.934960. The executable core gate is 0.90, so the
fixture passes with a minimum SSIM of 0.927610. PDF.js extracts zero NUL or
U+FFFE characters and finds the expected headings and body phrases. The final
package source fingerprint is
`7991fd19db7dd2599eda779e467da1c8ba7d947145cd929abc44cae58f924f95`.
Focused native PDFium and package-level PDF.js regressions preserve logical
source order, line ownership, and selection geometry for decomposed Latin,
Arabic and Hebrew mark stacks, and wrapped right-to-left text.

These measurements are a structural regression for this exact fixture, not a
universal Word-parity or pixel-identity claim. Word's PDF embeds proprietary
Calibri, while the package uses OFL-licensed, metric-compatible Carlito. Glyph
outlines and antialiasing therefore still differ.

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

- **PPTX preset geometry.** Non-rectangular presets and custom geometry fail
  closed. Full DrawingML guide-formula evaluation remains outside the supported
  fidelity envelope until a representative reference corpus exists.
- **N-API native addon** (napi-rs v3) — wasm CJS suffices for DocKosha on Vercel;
  a native addon becomes worthwhile for a self-hosted service or >50 MiB docs.
- **PDF/A-2b** output — for archival requirements.
- **Complex-script shaping** via `harfrust` — Arabic/Indic/Thai + bidi reorder.
- **Charts** — own vector renderer rather than routing to the service.
- **Slim web builds** — an in-browser font-fetch strategy instead of bundling all
  20 faces.
