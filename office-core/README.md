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
| DOCX | validated v1 envelope | paragraphs/runs (including paragraph-mark `rPr`), style cascade, lists, tables (nested/merges), headers/footers, sequential footnotes/endnotes, cross-paragraph field results, PAGE/NUMPAGES fields, tab leaders, internal link destinations, inline images, bounded page/paragraph anchors (including multiple anchors), floating-table side wrap, continuously balanced multi-column section bands, decimal measurements, sections, breaks, tracked-changes final view |
| PPTX | common-deck v1 | master/layout/slide inheritance, placeholder text, rectangular fills, pictures, tables; non-rectangular geometry fails closed |
| XLSX | print v1 | number-format engine, MDW column widths, gridlines, cell fills, multi-sheet pagination |

## Envelope (summary)

`convert()` is fail-closed: anything not enumerated is `Unsupported`. The stable
`reasons[].code` strings appear in both `Err(Unsupported)` and the DocProfile
JSON. DocKosha records an out-of-envelope conversion as failed and keeps the
original asset available; it does not invoke a compatibility converter.

DOCX anchor support is deliberately bounded. The engine renders top-level body
`wp:anchor` drawings with horizontal page, margin, or column origins and
vertical page, margin, or paragraph origins. It supports authored offsets plus
left/center/right and top/center/bottom alignment where the selected origin has
a stable page box. `simplePos` must be disabled, and `allowOverlap`, `behindDoc`,
and `relativeHeight` must be valid. Supported wrapping is `wrapNone`,
`wrapTopAndBottom`, rectangular `wrapSquare`, and `wrapTight` with a canonical
full-image rectangular polygon. Authored wrap distances and front/behind paint
order are preserved. Multiple body anchors are supported, with at most one
paragraph-relative anchor per paragraph; their exclusion bands must pass the
layout overlap and minimum-text-window preflight. Text-relative floating tables
can occupy a side band when at least 36 pt remains for text.

Other anchor forms fail with the typed `anchored_drawing` code. This includes
`simplePos=true`, inside/outside alignment, line/character reference origins,
`wrapThrough`, non-rectangular tight polygons, relative sizing, anchors in
multi-column sections, overlapping same-side exclusion bands, layouts that
leave no text window, non-convergent page-float placement, hidden or
overlap-disabled anchors, and anchors inside frames, table cells,
headers/footers, text boxes, or notes. The engine never substitutes no-wrap or
silently omits an out-of-envelope anchor.

Supported conversions may report additive warning codes without changing the
profile/convert verdict: `bullet_substituted` for a deterministic fallback
bullet and `floating_table_block_fallback` when a side-wrapped table leaves less
than the 36 pt text window and is laid out as a block instead. Bundled faces
cover the supported superscript code points in the U+207x block as actual
glyphs; an uncovered superscript fails closed with `missing_glyph` rather than
being synthesized.

Other representative envelope misses include:

- legacy binary (`.doc`/`.ppt`/`.xls`) → `legacy_format`; encrypted → `encrypted`
- feature forms outside their documented bounded subsets
- charts, SmartArt, OLE, WordArt, equations
- PPTX non-rectangular preset geometry and custom geometry
- complex-script (Arabic/Indic/Thai) and RTL; CJK without a covering font
  (hosts may register the pan-CJK Noto Sans CJK faces — see CONSUMING.md)
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
const provider = new engine.FontsourceProvider();
const result = await engine.convertOnDemand(bytes, "docx", {
  provider,                                      // allowlisted Fontsource aliases
  mode: "compatible",                          // or "strict"
  persistentCache: new engine.NodeFontCache(cacheDirectory),
});
const pdf = result.pdf;

// Advanced staged flow:
engine.office_font_requests(bytes, "docx");     // metadata only; never text
engine.office_register_font(fontBytes, expectationJson);
engine.office_profile(bytes, "docx");
engine.office_to_pdf(bytes, "docx");
```

The server worker also accepts `extraFontDirs: string[]`. At boot it registers
every `.ttf`, `.ttc`, `.otf`, and `.otc` file found there in deterministic path
order. This is an explicit host-authorized asset path; the production wasm
package itself contains no fonts. `convertOnDemand` and `profileOnDemand`
resolve missing faces through the configured provider and caches.

For the isolated Node worker, `FontsourceProvider` maps Office families to
fetchable metric-compatible faces (for example Calibri → Carlito and Courier
New → Cousine). Ubuntu and Ubuntu Mono use the verified Fontsource 5.2.8
artifacts whose internal versions are 0.83 and 0.80; other catalog families use
the documented `latest` policy. In compatible mode the worker also boot-loads
six SHA-256-pinned Noto Sans/Symbols faces from DocKosha's first-party origin.
They are persisted through the same cache, re-registered after worker
replacement, and selected only after normal resolution fails. Strict mode
never fetches or considers this fallback. CI injects the local mock provider
and disables fallback networking; package tests never call a public CDN.

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
Normal corpus runs resolve the document's fonts automatically from the
hash-pinned `fidelity/fonts.lock`, populating the ignored, content-addressed
`fidelity/fonts-cache/` on the first run and reusing verified cached bytes
thereafter. The cache needs network access only when a required entry is cold.
Do not pass host font directories to `run`; repeatable `--font-dir <path>` is
available only on `probe` for host-font diagnostics. Run
`cargo run -p fidelity -- lock-fonts` only when deliberately refreshing the
committed font lock.
Calibration constants and their adjustments are logged in
`LAYOUT_CALIBRATION.md`.

The frozen `fidelity-corpus/docx/complex/` regression identifies both reference
files by SHA-256:

- DOCX: `8c2a8c4725e77bbfb4ddc7925674dc221611a39b876dbd7ea2efc829e70b8530`
- Word PDF: `e808b449e27436d74f81d5d5df9f9fe4daa4cc28e426ec1619bbe3fcfb55255c`

The direct converter and final WASM worker produce the same deterministic PDF
(`SHA-256 a3629b6aae822d56127cba7ab07ea6c0fc3ab51910b72bad0e0db301cdde3e1a`):
six 612×792-point pages, the same front/behind layer order as Word, and image
bounds within 0.06 pt. Tight wrapping releases text on the first baseline below
the authored polygon, and table flow includes the top and bottom outer-border
half-strokes; the table edges are within 0.4 pt and the following baseline is
within 0.3 pt of Word. The executable core gate is 0.90, and the rebuilt
six-page fixture passes with a minimum SSIM of 0.9374006584. PDF.js extracts zero NUL or
U+FFFE characters and finds the expected headings and body phrases. The final
package source fingerprint is
`add0e1fe34024cc8b5494cb4c2bd99dc31c05702c50127b7b1abf632cfdc962d`.
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
- **Fonts.** Native default builds bundle metric-compatible OFL fonts for
  hermetic fidelity. Production wasm is fontless and accepts validated
  provider/host-supplied SFNT faces; output uses subset Type0/CIDFontType2
  embedding with ToUnicode (or policy-bounded whole-font embedding when
  `fsType` forbids subsetting).
- **wasm-first.** The engine compiles to `wasm32-unknown-unknown` from day one;
  CI enforces it. Production: `--no-default-features --features wasm`.

## Deferred (record, don't build)

- **PPTX preset geometry.** Non-rectangular presets and custom geometry fail
  closed. Full DrawingML guide-formula evaluation remains outside the supported
  fidelity envelope until a representative reference corpus exists.
- **N-API native addon** (napi-rs v3) — wasm CJS suffices for DocKosha on Vercel;
  a native addon becomes worthwhile for a self-hosted service or >50 MiB docs.
- **PDF/A-2b** output — for archival requirements.
- **Complex-script shaping** via `harfrust` — Arabic/Indic/Thai + bidi reorder.
- **Charts** — own vector renderer rather than routing to the service.
