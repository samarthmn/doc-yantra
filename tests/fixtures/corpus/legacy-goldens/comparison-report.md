# Legacy golden comparison report

Audit date: 2026-07-13; dispositions refreshed 2026-07-15 after the DOCX
remediation. Oracle: all 34 digest-frozen `e0ff8fb` manifest rows.
No golden was regenerated. Runners use local Rust/Node engines and test-side
pdfium; they never invoke LibreOffice, Gotenberg, Chromium, or PyMuPDF.

| Truth category | Count | Meaning |
|---|---:|---|
| Green rows | 12 | Every newly required assertion passes. |
| Allowlisted rows | 0 | No row is green solely through an allowlist. One URI serialization metric is allowlisted, but that row is independently red on visual parity. |
| Red rows | 22 | A typed known failure exists and remains visible in runner output. |

## What changed

1. **Frozen bytes now authenticate themselves.** The 34-row legacy manifest
   already carried `input_sha256` and `golden_sha256`, but no comparison runner
   checked them. Every legacy runner now verifies both bytes before conversion or
   comparison. Each of the 14 Word-fidelity TOMLs now also freezes its
   `source.docx` and `ground-truth.pdf` with those fields, and the visual runner
   verifies them before scoring. Node verifies all 68 legacy paths.
2. **Page counts come from the oracle.** The owned Rust/Node comparisons parse
   the golden PDF. The frozen long-Markdown golden has 17 pages. Current Rust
   source emits 17; the committed Node wasm emits 14 and is tracked red as
   `NODE_MD_PAGE_COUNT`. `pdf-core/tests/golden_comparison_test.rs:49` still pins
   the current output to `14`; that file is owned by another worker and must be
   changed there to compare against the parsed golden count.
3. **Words must remain on their original page.** Every Markdown, CSV, and Office
   row now compares the complete normalized ordered word vector on page *i* with
   page *i* of the golden. Global word coverage no longer hides cross-page drift.
4. **Office parity now includes pixels and geometry.** The office legacy runner
   adds fixed-DPI raster MAD, regional MAD, golden-derived color probes, word-box
   positions, exact image counts and bounds, exact page dimensions, and URI
   target sets. The former text/object-count-only claim marked every office row
   green; the visual evidence now keeps 20 office rows red while the exact
   `headers-footers` and `tabs-leaders` comparisons pass.
5. **Redaction now matches the oracle exactly.** The former 80% (`file2`) and
   30% (`pdf_ppt-file1`) survivor narratives and their 75%/25% floors are gone.
   Whole-text-object deletion was replaced by glyph/run-level rewriting, so all
   three rows reach 100% removal *and* 100% survivor parity, compared as exact
   page-word equality against the PyMuPDF oracle — including the partial-word
   fragments the oracle itself leaves when a rectangle bisects a word
   (`slightly` → `slig`). The allowlist contains no over-redaction tolerance and
   no redaction entry of any kind.
6. **Disposition is explicit.** `legacy-parity-allowlist.json` separates typed
   `allowlisted` metrics from typed `known_failures`. Tests reject empty fixture
   ids, reasons, user effects, or tracking notes; known failures count as red,
   never as covered green.

## Comparison thresholds

All office rasters use pdfium at **144 DPI**. Both PDFs use the same renderer in
the same process, so renderer variance is not part of the budget.

| Metric | Fixed gate | Rationale |
|---|---:|---|
| Page count | exact golden equality | Missing or extra pages change output semantics. |
| Page dimensions | max 0.05pt per axis | Covers decimal PDF serialization only; observed maximum is 0.0239pt. |
| Per-page ordered words | exact vector equality | Prevents text loss, insertion, reorder, and migration across pages. |
| Raster global MAD | ≤ 0.020 normalized RGB | A 2% average channel error allows antialiasing noise but catches broad missing fills, images, or moved content. |
| Raster regional MAD | ≤ 0.120 in every cell of a 12×12 grid | Catches a localized missing block that a mostly white page could hide in global MAD. |
| Golden color probes | ≥ 95% match; each probe MAD ≤ 0.120 | Each 32×32-pixel cell contributes its strongest non-white golden point. Exact coordinates make fills, borders, colors, and moved marks visible. |
| Word-box position | p95 ≤ 2.0pt and max ≤ 5.0pt | Allows sub-glyph/font rounding while rejecting visibly moved lines or blocks. Boxes come from pdfium character extraction in both PDFs. |
| Image count | exact | Presence-only checks hid duplicate or missing images. |
| Image bounds | max 0.5pt per edge | Allows sub-point matrix rounding while catching moved/resized images. All current oracle rows contain zero images, so the exact zero-count assertion is still exercised. |
| URI navigation targets | exact normalized set | Root-only `http(s)` trailing slashes normalize; path, query, fragment, and non-HTTP URIs remain exact. |

No existing threshold was reduced. New thresholds were fixed before measuring
the current engine. Red measurements were registered; thresholds were not moved.

## Office measurements

Every office row has exact page count, per-page ordered words, orientation, and
image count (0 → 0). `Color` is matched/total golden-derived probes followed by
the worst page ratio. `Position` is p95/max Euclidean word-box center delta.
`Raster` is worst-page global MAD / worst 12×12-region MAD.

| Fixture | Pages | Dim max pt | Raster MAD / region | Color | Position p95 / max pt | URI raw | Verdict |
|---|---:|---:|---:|---:|---:|---|---|
| office/docx/file1.docx | 1→1 | 0.0000 | 0.004435 / 0.085678 | 4/54; 0.0741 | 16.3001 / 17.0316 | exact |
| office/docx/file2.docx | 1→1 | 0.0000 | 0.004563 / 0.092588 | 9/58; 0.1552 | 15.1406 / 15.1406 | exact |
| office/pptx/file1.pptx | 1→1 | 0.0142 | 0.026554 / 0.288994 | 81/173; 0.4682 | 185.9211 / 185.9633 | exact |
| office/pptx/file2.pptx | 1→1 | 0.0142 | 0.026947 / 0.288994 | 80/173; 0.4624 | 185.9633 / 185.9633 | exact |
| office/xlsx/file1.xlsx | 1→1 | 0.0239 | 0.002301 / 0.139261 | 9/44; 0.2045 | 30.8490 / 30.8578 | exact |
| office/xlsx/file2.xlsx | 1→1 | 0.0239 | 0.001051 / 0.047380 | 16/50; 0.3200 | 2.6556 / 2.6832 | exact |
| office/xlsm/file1.xlsm | 1→1 | 0.0239 | 0.002402 / 0.130832 | 9/43; 0.2093 | 29.1064 / 29.1435 | exact |
| office/xlsm/file2.xlsm | 1→1 | 0.0239 | 0.001050 / 0.049895 | 15/40; 0.3750 | 2.6556 / 2.6832 | exact |
| office/fidelity-docx/fonts-substitution | 1→1 | 0.0103 | 0.000654 / 0.040622 | 0/16; 0.0000 | 53.3766 / 53.3766 | exact |
| office/fidelity-docx/headers-footers | 1→1 | 0.0103 | 0.000214 / 0.015693 | 52/52; 1.0000 | 0.4685 / 0.4753 | exact |
| office/fidelity-docx/hyperlinks | 1→1 | 0.0103 | 0.000105 / 0.010744 | 10/12; 0.8333 | 2.0082 / 2.0082 | {"https://example.com/"}->{"https://example.com"} |
| office/fidelity-docx/justify-long-text | 1→1 | 0.0103 | 0.006333 / 0.077720 | 169/232; 0.7284 | 360.8760 / 434.0718 | exact |
| office/fidelity-docx/lists-multilevel | 1→1 | 0.0103 | 0.000127 / 0.009740 | 16/19; 0.8421 | 1.0552 / 1.0552 | exact |
| office/fidelity-docx/long-doc-pagination | 8→8 | 0.0103 | 0.060531 / 0.167926 | 2842/8580; 0.3262 | 265.0898 / 293.3913 | exact |
| office/fidelity-docx/page-fields | 11→11 | 0.0103 | 0.061015 / 0.167926 | 4295/12970; 0.3235 | 265.0898 / 293.3913 | exact |
| office/fidelity-docx/plain-paragraphs | 1→1 | 0.0103 | 0.005946 / 0.086914 | 157/222; 0.7072 | 265.1500 / 293.3913 | exact |
| office/fidelity-docx/run-formatting-matrix | 1→1 | 0.0103 | 0.001026 / 0.051714 | 3/18; 0.1667 | 66.7819 / 66.7819 | exact |
| office/fidelity-docx/spacing-exact-atleast | 1→1 | 0.0103 | 0.000748 / 0.027069 | 39/54; 0.7222 | 7.0461 / 7.0461 | exact |
| office/fidelity-docx/table-fixed | 1→1 | 0.0103 | 0.005735 / 0.140882 | 7/100; 0.0700 | 2.2920 / 2.2920 | exact |
| office/fidelity-docx/table-merges | 1→1 | 0.0103 | 0.002496 / 0.055994 | 1/62; 0.0161 | 7.7238 / 7.7238 | exact |
| office/fidelity-docx/table-nested | 1→1 | 0.0103 | 0.001634 / 0.047822 | 1/44; 0.0227 | 2.0921 / 2.0921 | exact |
| office/fidelity-docx/tabs-leaders | 1→1 | 0.0103 | 0.000020 / 0.001574 | 8/8; 1.0000 | 0.4410 / 0.4410 | exact |

These measurements caught a real defect, not just a threshold miss. The office
emitter paints glyphs as artifacts and carries a separate invisible (`Tr 3`)
`/ActualText` layer for extraction — and that layer adjusted every cluster by its
*full* glyph width, which cancels the advance a TJ number is supposed to leave.
Every character therefore reported the same origin: pages rendered perfectly while
character boxes (text selection, search highlighting, accessibility) collapsed onto
a point. The layer now consumes each cluster's painted advance, scaled onto the
width the line actually paints so justification cannot push it past the line end.
`office/docx/file1` fell from p95 253.2pt to 16.3pt and `office/xlsx/file2` from
4.04pt to 2.66pt.

What remains is honest and still red: the invisible layer distributes a line's
painted width across its clusters proportionally rather than carrying each
cluster's exact painted advance, so justified and multi-line rows still deviate
(`justify-long-text` 360.9pt, `plain-paragraphs` 265.2pt). PPTX is a separate,
genuinely divergent case — it fails the raster gate too (mean-abs 0.0266 against a
0.020 limit), so its slide layout really does differ from LibreOffice. The
color-probe metric point-samples exact golden pixel coordinates, so a sub-pixel
shift in antialiased text flips a probe; treat it as a change detector, not as
proof of visible difference, and read the raster MAD columns alongside it.

## Markdown and CSV measurements

The Rust runner parses both page counts from PDFs and compares complete ordered
word vectors per corresponding page. The Node package independently parses the
same golden page count before checking its output.

| Fixture | Pages golden→Rust / Node | Per-page ordered words | Verdict |
|---|---:|---|---|
| markdown/file1 | 1→1 / not separately generated | exact | green |
| markdown/file2 | 1→1 / not separately generated | exact | green |
| markdown/unicode-mixed | 2→2 / not separately generated | p1: golden 110/current 98, first mismatch 29; p2: 30/30, first mismatch 24 | red: `MDCSV_PAGE_WORD_ORDER` |
| markdown/gfm-constructs | 2→2 / not separately generated | p1 exact; p2: golden 91/current 93, first mismatch 0 | red: `MDCSV_PAGE_WORD_ORDER` |
| markdown/long-pagination | 17→17 / 17 | Rust and packaged Node page-local words exact | green |
| csv/file1 | 1→1 / not separately generated | exact | green |
| csv/file2 | 1→1 / not separately generated | exact | green |
| csv/latin1 | 1→1 / not separately generated | exact | green |
| csv/unicode | 1→1 / not separately generated | exact | green |

## Redaction status

No allowlist entry permits over-redaction. In particular, the former `file2`
whole-object over-removal tolerance is gone. The old report's 80% and 30%
survivor results are historical red evidence, not parity. Current exact
survivor parity is below.

| Fixture | Current survivor parity | Verdict |
|---|---|---|
| redaction/file1 | 8/8 removed; 13/13 survivors (1.0000) | green: exact PyMuPDF page-word equality |
| redaction/file2 | 7/7 removed; 10/10 survivors (1.0000) | green: exact PyMuPDF page-word equality; no over-removal tolerance |
| redaction/pdf_ppt-file1 | 11/11 removed; 10/10 survivors (1.0000) | green: exact PyMuPDF page-word equality |

## Full 34-row disposition audit

“Assertions” names the real runner evidence. A mark in Allowlisted records a
typed metric exception; any independent red keeps the row in the red count.

| # | Fixture | Green | Allowlisted (typed) | Red (tracked) | Assertions |
|---:|---|:---:|---|---|---|
| 1 | office/docx/file1.docx |  |  | `OFFICE_VISUAL_PARITY` | digest; pages; page words; 0.05pt dimensions; raster; regions; color probes; word boxes; image count/bounds; URIs |
| 2 | office/docx/file2.docx |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set |
| 3 | office/pptx/file1.pptx |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set |
| 4 | office/pptx/file2.pptx |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set |
| 5 | office/xlsx/file1.xlsx |  | `OFFICE_PAGE_DIMENSIONS`: LO-era golden fixed A4; engine now applies the ECMA-376 default (paperSize 1, Letter) when pageSetup omits it | `OFFICE_VISUAL_PARITY` | same office assertion set |
| 6 | office/xlsx/file2.xlsx |  | `OFFICE_PAGE_DIMENSIONS`: LO-era golden fixed A4; engine now applies the ECMA-376 default (paperSize 1, Letter) when pageSetup omits it | `OFFICE_VISUAL_PARITY` | same office assertion set |
| 7 | office/xlsm/file1.xlsm |  | `OFFICE_PAGE_DIMENSIONS`: LO-era golden fixed A4; engine now applies the ECMA-376 default (paperSize 1, Letter) when pageSetup omits it | `OFFICE_VISUAL_PARITY` | same office assertion set |
| 8 | office/xlsm/file2.xlsm |  | `OFFICE_PAGE_DIMENSIONS`: LO-era golden fixed A4; engine now applies the ECMA-376 default (paperSize 1, Letter) when pageSetup omits it | `OFFICE_VISUAL_PARITY` | same office assertion set |
| 9 | office/fidelity-docx/fonts-substitution |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set; font-run text |
| 10 | office/fidelity-docx/headers-footers | ✓ |  |  | same office assertion set; header/footer text |
| 11 | office/fidelity-docx/hyperlinks |  | `OFFICE_LINK_URI_SERIALIZATION`: golden `https://example.com/`, current `https://example.com`, normalized target exact | `OFFICE_VISUAL_PARITY` | same office assertion set; raw and normalized URI sets |
| 12 | office/fidelity-docx/justify-long-text |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set; justified prose |
| 13 | office/fidelity-docx/lists-multilevel |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set; list order |
| 14 | office/fidelity-docx/long-doc-pagination |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set on all 8 pages |
| 15 | office/fidelity-docx/page-fields |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set on all 11 pages; PAGE/NUMPAGES text |
| 16 | office/fidelity-docx/plain-paragraphs |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set |
| 17 | office/fidelity-docx/run-formatting-matrix |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set; formatted runs |
| 18 | office/fidelity-docx/spacing-exact-atleast |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set; line spacing |
| 19 | office/fidelity-docx/table-fixed |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set; borders/cells |
| 20 | office/fidelity-docx/table-merges |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set; merged cells |
| 21 | office/fidelity-docx/table-nested |  |  | `OFFICE_VISUAL_PARITY` | same office assertion set; nested cells |
| 22 | office/fidelity-docx/tabs-leaders | ✓ |  |  | same office assertion set; tab positions |
| 23 | markdown/file1 | ✓ |  |  | digest; golden-derived page count; exact page-local ordered words |
| 24 | markdown/file2 | ✓ |  |  | same Markdown/CSV assertion set |
| 25 | markdown/unicode-mixed |  |  | `MDCSV_PAGE_WORD_ORDER` | same Markdown/CSV assertion set; both pages red |
| 26 | markdown/gfm-constructs |  |  | `MDCSV_PAGE_WORD_ORDER` | same Markdown/CSV assertion set; both pages red |
| 27 | markdown/long-pagination | ✓ |  |  | digest; golden-derived page count 17/17 in both the Rust engine and the packaged Node build; exact per-page ordered words |
| 28 | csv/file1 | ✓ |  |  | digest; golden-derived page count; exact page-local ordered words |
| 29 | csv/file2 | ✓ |  |  | same Markdown/CSV assertion set |
| 30 | csv/latin1 | ✓ |  |  | same Markdown/CSV assertion set |
| 31 | csv/unicode | ✓ |  |  | same Markdown/CSV assertion set |
| 32 | redaction/file1 | ✓ |  |  | digest; exact oracle page-word equality; 8/8 oracle removals absent; 13/13 oracle survivors present (1.0000); decoded-graph probes; coverage rects |
| 33 | redaction/file2 | ✓ |  |  | digest; exact oracle page-word equality; 7/7 oracle removals absent; 10/10 oracle survivors present (1.0000); decoded-graph probes; coverage rects |
| 34 | redaction/pdf_ppt-file1 | ✓ |  |  | digest; exact oracle page-word equality; 11/11 oracle removals absent; 10/10 oracle survivors present (1.0000); decoded-graph probes; coverage rects |

## Engine gaps for routing

- **Remaining red office fixtures:** each fails one or more fixed visual gates;
  the worst current text-position p95 is 178.9689pt against 2pt. Golden color
  probe ratios range from 0% to 84.21% against 95%.
- **Raster-specific office gaps:** both PPTX fixtures exceed global and regional
  MAD; XLSX `file1`, XLSM `file1`, `table-fixed`, `long-doc-pagination`, and
  `page-fields` exceed regional MAD, with the two long DOCX cases also exceeding
  global MAD.
- **Markdown Unicode/GFM:** page-local ordered vectors differ even when global
  logical text/run checks pass.
- **Distributed long Markdown:** committed Node wasm emits 14 pages; oracle and
  current Rust source emit 17.
- **Redaction:** all three fixtures match the oracle exactly (see above).
