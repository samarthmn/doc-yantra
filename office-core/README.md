# office-core — DocKosha's pure-Rust Office → PDF engine

Own OOXML parsing, own layout, own PDF emission. **No LibreOffice, Chromium,
pdfium, or Typst in the conversion path** — first-party code is under
AGPL-3.0-only and upstream dependencies retain their own terms; only small,
pure-Rust
leaf crates. Ships as a native CLI (`officecore`) and a WebAssembly npm package
(`@samarthmn/doc-yantra-office`), mirroring `pdf-core`'s conventions.

Two contracts anchor everything: a fail-closed conversion envelope and the
predictive `DocProfile` router contract.

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
  (hosts register the pan-CJK Noto Sans/Serif CJK faces — serif lineages such
  as SimSun/MingLiU/Mincho/Batang substitute the Serif sibling, bold CJK runs
  render with synthetic stroke emboldening, and `w:autoSpaceDE`/`DN` East
  Asian auto-spacing is applied — see CONSUMING.md)
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
resolve missing faces through the DocYantra font resolver and cache the
checked, hash-pinned assets.

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

## Verification

Self-contained package checks live in `smoke-tests/`; run those checks before
sharing a package. Owner-supplied golden-document
comparison is a separate scheduled/manual gate and requires the approved
rights and provenance intake described in the repository's top-level
`FIXTURE_PROVENANCE.md`. Package smoke checks do not depend on that owner-only gate.

## Design notes

- **Determinism.** Byte-identical output for identical input: object numbering by
  emission order, `/ID` = first 16 bytes of `SHA-256(input ‖ version)`,
  CreationDate/ModDate from `docProps/core.xml` (else a fixed literal), streams
  FlateDecode-compressed.
- **Fonts.** Native default builds bundle metric-compatible OFL fonts for
  hermetic fidelity. Production wasm is fontless and accepts validated
  provider/host-supplied SFNT faces; output uses subset Type0/CIDFontType2
  embedding with ToUnicode (or policy-bounded whole-font embedding when
  `fsType` forbids subsetting). OpenType/CFF faces (e.g. the pan-CJK Noto
  OTFs) embed as Type0/CIDFontType0 with a bare-CFF `FontFile3`
  (`CIDFontType0C`) and an embedded `/Encoding` CMap. That CMap lists **only
  content-reachable codes, painted codes allocated lowest**: pdf.js resolves
  CIDFontType0C glyphs by inverting the CMap per CID (lowest charcode wins),
  so any extra alias — an identity prelude over the subset glyph ids, or a
  raw glyph-id fallback code — silently blanks the code the content stream
  actually uses while extraction stays perfect. Paint-only glyphs in mixed
  CFF runs draw as vector outlines instead of raw codes; a visible glyph
  needing two painted codes, or a whole-embedded CFF whose charset is not the
  `CID == GID` identity, fails closed (`missing_glyph`). `CIDFontType0C` +
  `FontFile3` alone do not prove viewer compatibility — the paint regression
  in `tests/office-cjk-pdfjs-paint.test.mjs` renders through pdf.js 3.11.174
  and asserts actual ink.
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


### Legacy spreadsheet Notes

`legacyDrawing` is resolved separately from OLE objects and controls. A bounded
legacy Note envelope supports plain comment text, canonical rectangular VML Note
shapes, internal VML/comments relationships, cell anchors with pixel offsets,
point dimensions, yellow or white fill, a black 1pt border, and default Tahoma 9pt
text with wrapping. Textbox margins use the VML defaults (7.2pt horizontal and
3.6pt vertical); see [Microsoft's inset specification](https://learn.microsoft.com/en-us/windows/win32/vml/msdn-online-vml-inset-attribute).

Only visible, printable Notes appear when `pageSetup cellComments="asDisplayed"`.
Hidden Notes and `PrintObject=False` remain absent, and the default `none` print
mode omits Notes. `atEnd` printing, styled/rich comment runs, custom shapes,
decorations, unsupported styles, external/missing relationships, mismatched
comment anchors, overflowing text, and page-crossing Notes fail closed. Profile
and conversion share this validation and the same layout check. This does not
provide general VML, ActiveX, OLE, or threaded-comment support.
