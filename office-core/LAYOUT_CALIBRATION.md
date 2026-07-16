# Layout calibration log

Institutional memory for the fidelity-harness calibration loop (agent task
T4.5). The DOCX layout formulas in `src/layout/` contain constants tuned to
Word's behaviour. When a corpus case scores below its SSIM threshold, read the
heatmap, identify the axis, adjust the **one** relevant constant, re-run
`cargo run -p fidelity -- run --case <name>`, and log the change here.

## Method

Per failing case:
1. `cargo run -p fidelity -- run --case <name>` → open
   `../tmp/fidelity-report/index.html`, or pass `--report-dir <path>`.
2. Read the heatmap:
   - line-height drift → error compounds down the page → `paragraph.rs` line metrics.
   - column mis-alignment → `table.rs` width resolution.
   - spacing errors at paragraph boundaries → `flow.rs` spacing collapse.
   - super/sub position → `paragraph.rs` `SCRIPT_SCALE` / `SCRIPT_RISE`.
3. Adjust one constant; re-run the case; commit when it passes without
   regressing the full `run --format docx`.

## Current calibration constants (starting values, pre-ground-truth)

| Location | Constant | Value | Notes |
|---|---|---|---|
| `paragraph.rs` | `SCRIPT_SCALE` | 0.65 | super/subscript size factor |
| `paragraph.rs` | `SCRIPT_RISE` | 0.35 | super/subscript baseline rise (× size) |
| `paragraph.rs` | line height | OS/2 winAscent+winDescent | the primary parity lever (T2.5) |
| `table.rs` | `DEFAULT_CELL_MARGIN_PT` | 5.4 | 108 twips default cell side margin |
| `table.rs` | `AUTOFIT_CELL_MARGIN_PT` | 0.5 | bare AutoFit table side inset; stored-grid tables keep 5.4pt |
| `flow.rs` | paragraph gap | `max(after_prev, before_next)` | Word collapse rule |
| `flow.rs` | `MIN_TEXT_WINDOW_PT` | 36.0 | minimum useful middle/side wrap interval |
| `flow.rs` | `PAGE_FLOAT_REFLOW_LIMIT` | 3 | deterministic page-float pagination fixpoint bound |

## Log

### 2026-07-09 — kerning gated on `w:kern` (line-break drift)

- **Axis:** line-break drift compounding down the page (dense-text SSIM collapse).
- **Symptom:** with A4 page size and line height already matching Word exactly,
  the four dense-text cases still failed — `long-doc-pagination` and `page-fields`
  at SSIM 0.64, `plain-paragraphs` 0.938, `justify-long-text` 0.933. Page counts
  matched, so the error was within-page: office-core packed more words per line
  than Word, and every wrap point drifted from there down.
- **Root cause:** `text/shape.rs` applied pair kerning to every run. Word only
  kerns runs carrying `w:kern` (off by default). Kerning narrows text, so lines
  ran long and wrapped later.
- **Change:** parse `w:kern` into `RunProps.kern_min_pt`; apply pair kerning only
  when set and the size clears the threshold. Per format: docx gated on `w:kern`,
  pptx always (PowerPoint kerns by default), xlsx never (Excel does not kern).
- **Result:** the four cases rose to 0.955 / 0.953 / 0.993 / 0.971; the docx
  corpus went fully green. Commit `91ac3f3`.
- **Note:** this is a global correctness fix, not a per-case constant — it
  changes how every document wraps, so it belongs here rather than in the
  constants table.

### 2026-07-14 — tight wrap releases by text baseline

- **Axis:** page 3 tight-wrapped text stayed narrow for one line too many.
- **Root cause:** tight wrapping tested the whole line box against the polygon,
  so ascender space above a baseline that had already cleared the shape kept the
  exclusion active. Square wrapping correctly needs line-box intersection; tight
  wrapping follows the text baseline against its polygon.
- **Change:** wrap bands retain their mode. Tight bands test the baseline while
  square bands continue to test the full line box.
- **Evidence:** the shape bottom plus `distB` is 385.0568 pt. Baselines at
  363.755 and 379.197 pt stay wrapped; the 394.639 pt baseline returns to the
  full text width. Page 3 SSIM rose from about 0.958 to 0.973315.

### 2026-07-14 — outer table border half-strokes participate in flow

- **Axis:** the page 6 table and all following text were vertically high.
- **Root cause:** row content heights excluded the outward halves of the top and
  bottom boundary strokes. The border painted correctly, but the block consumed
  too little vertical space.
- **Change:** shift the first row by half the maximum top boundary stroke and add
  that amount to its height; add half the maximum bottom boundary stroke to the
  last row height. Internal row geometry and center-to-center distances stay
  unchanged.
- **Evidence:** the downstream baseline is 664.385 pt versus Word's 664.660 pt;
  table top/bottom edges are within 0.4 pt. Page 6 SSIM rose from about 0.890 to
  0.934960.

### 2026-07-14 — frozen complex DOCX gate

- **Corpus:** `fidelity-corpus/docx/complex/` contains the 6-page source and the
  Microsoft Word PDF export with provenance and frozen hashes.
- **Result:** all six pages pass the 0.90 core threshold; per-page SSIM is
  0.962378 / 0.972241 / 0.973315 / 0.971328 / 0.927610 / 0.934960 (minimum
  0.927610). This is a fixture-specific regression, not a pixel-identity claim;
  Word uses Calibri and the package uses Carlito.

### 2026-07-14 — bounded page-float reflow and minimum text window

- **Axis:** page-scoped anchored drawings and text-relative floating tables can
  change line widths after their carrier's page is known.
- **Constants:** reflow is capped at 3 rebuild/re-paginate passes; simultaneous
  edge bands and side-wrapped floating tables require at least 36pt of text
  width. Non-convergence fails closed. An over-wide floating table keeps the
  previous block layout and emits `floating_table_block_fallback`.
- **Evidence:** the focused pathological fixture moves its page-float carrier
  through pages 0 → 2 → 1 → 0 and reaches the typed non-convergence guard after
  3 passes. The dual 77pt corner fixture leaves a 296pt middle interval and
  converts; the 230pt-per-side fixture leaves less than 36pt and fails closed.
  A 430pt table plus 9.35pt clearance leaves about 28.65pt on the default 468pt
  content box and takes the documented block fallback. All 15 pre-existing
  manifested DOCX fidelity cases remain green; `complex` retains its frozen
  0.927610 minimum SSIM and 6/6 page count.

### 2026-07-15 — continuous-column balancing and authored band extent

- **Axis:** a continuous section change must balance only the units in that
  section band, then resume full-width flow below the deepest column.
- **Change:** derive the balance target from the authored band extent, preserve
  the band's leading gap in every column, carry explicit page breaks across the
  band boundary, and place following content at the deepest occupied column.
  No fixture-specific offset or tunable fudge is used.
- **Evidence:** `gdocs-columns` retains exact 3/3 page parity. Per-page SSIM in
  the final full-corpus run is 0.918623 / 0.959255 / 0.926355 (minimum
  0.918623), clearing the 0.90 core gate.

### 2026-07-15 — long-table continuation and list geometry

- **Axis:** split tables lost their collapsed top boundary at continuation
  pages, negative decimal `tblInd` values were truncated, and hanging-indent
  first lines used the continuation-line right edge.
- **Change:** every row owns its collapsed top boundary so a continuation page
  repaints it; decimal twip measurements remain fractional through parsing and
  layout; hanging indents narrow continuation lines without expanding the first
  line; a keep-next heading binds to the first table row.
- **Evidence:** `gdocs-tables-longdoc` retains exact 44/44 page parity. The
  final measured run has a 0.803000 minimum SSIM; pages 30, 33, 36, 37, 41,
  42, and 43 remain below the 0.90 core floor, so this case is still a blocking
  fidelity failure rather than a claimed calibration win.

### 2026-07-15 — shaped tab-leader advances

- **Axis:** TOC leaders must fill the authored tab interval without changing
  the tab stop or overrunning the following page number.
- **Change:** leader repetition uses the selected font's shaped glyph advance;
  zero-width or non-finite advances are rejected, and the final glyph is kept
  within the measured interval. This introduces no fixed leader-width constant.
- **Evidence:** focused tab-leader metric tests preserve the authored right tab
  stop and keep every repeated leader within its interval. The frozen legacy
  `tabs-leaders` comparison is now green (8/8 color probes, 0.441 pt text
  position p95), so its obsolete known-failure entry was removed. The larger
  `gdocs-anchors-toc` fixture retains exact 10/10 page parity with a 0.669644
  minimum SSIM but remains below the core floor, so it is not cited as passing
  calibration evidence.

### 2026-07-15 — footer-distance bottom anchoring

- **Axis:** Word's `w:pgMar/@w:footer` measures from the physical page bottom
  to the footer block's bottom, not to the block's top edge.
- **Change:** footer layout now measures its complete block and subtracts that
  height from the authored bottom anchor; header layout retains its top-edge
  convention.
- **Evidence:** `footer_distance_bottom_anchors_a_one_line_footer` pins the
  authored 0.5-inch bottom distance. The frozen `headers-footers` comparison is
  now green: 52/52 color probes, 0.4685 pt text-position p95, and 0.000214
  global raster MAD.

### 2026-07-15 — fragment-aware table edges and shared vertical seams

- **Axis:** `gdocs-tables-longdoc` table blocks were about 0.5pt left on every
  page and about 0.5pt high on continuation pages; adjacent cells also emitted
  the same vertical seam twice.
- **Probe evidence:** before the fix, page 40 (table start) measured median
  truth-minus-ours deltas of roughly `(+0.55, +0.13)` for vertical borders and
  `(+0.60, +0.09)` for text. Page 41 (continuation) measured roughly
  `(+0.55, +0.61)` and `(+0.60, +0.58)`. The output contained 112 and 71 exact
  duplicate vertical segments respectively. The passing `complex` page-6
  control already had an x delta near +0.05pt for its unindented start table.
- **Change:** table layout exposes top/bottom outer half-strokes to pagination,
  which reserves the top half on every fragment and the bottom half in fragment
  fit accounting. Explicit negative `tblInd` anchors the painted outer edge;
  default-start tables without `tblInd` retain Word's centerline convention,
  while center/end/floating placement accounts for the complete painted
  footprint. Vertical borders use the same wider-then-darker shared-edge
  resolver as horizontal borders and merge collinear same-stroke row segments
  after page slicing. Cell top margins remain measured from the border inner
  edge; the probe did not support an H5 inset change.
- **Result:** the final page-41 probe clusters table text around +0.05pt x and
  near-zero y, with zero duplicate segments. `gdocs-tables-longdoc` has exact
  44/44 page parity and 0.918135 minimum SSIM (required 0.90, prior 0.812452).
  The `complex` control retains its exact 0.937401 observed floor.

### 2026-07-15 — hhea line gap participates in baseline placement

- **Axis:** with the verified Ubuntu 0.83 files registered, line breaks and
  advances matched Word but text origins remained roughly 0.3pt high. On the
  dense TOC page this held `gdocs-anchors-toc` at 0.886703 minimum SSIM.
- **Root cause:** natural line height included hhea `lineGap`, but the line-box
  baseline offset used only the face ascent, leaving the complete gap below the
  descent. Word reserves the hhea gap above the glyph ascent. Ubuntu's 28/1000
  gap is 0.308pt at 11pt; Liberation Sans' 67/2048 gap is 0.393pt at 12pt,
  matching the vector-probe deltas in both fixtures.
- **Change:** retain the same natural line height and pagination, but use
  `ascent + lineGap` for DOCX line-box and numbering-label baseline placement.
  Glyph metrics, script rise, and PDF font programs are unchanged.
- **Result:** `gdocs-anchors-toc` passes 10/10 pages at 0.907076 minimum; its
  dense TOC page rises from 0.886703 to 0.935346. `complex` stays above the
  required 0.937401 floor and page 6 rises to 0.949921. The 44-page table case
  remains green at 0.918135 minimum.

### 2026-07-16 — shaped min/max AutoFit for unqualified tables

- **Axis:** the three synthetic table fixtures omitted `tblW`, `tcW`, and
  `tblLayout`, so Word ignored their 130pt stored grid columns and fitted the
  grid to shaped cell content. The engine continued to paint three 130pt
  columns.
- **Gate:** AutoFit runs only when layout is not fixed, table width is absent,
  `auto`, or `nil`, and no cell has a `dxa`/`pct` width. The calibrated Google
  fixtures (`tblLayout=fixed`) and `complex` (explicit `tcW`) retain the stored
  grid path.
- **Change:** paragraph runs are shaped once into minimum and maximum intrinsic
  widths; cell spans distribute deficits over their columns, flexible columns
  interpolate between min/max at the available width, and nested tables recurse.
  A bare AutoFit table uses Word's measured 0.5pt implicit side inset; its grid
  is border-center to border-center, so the stroke is not added to that width.
  Stored-grid tables retain the existing 5.4pt `TableNormal` default.
- **Probe evidence:** `table-fixed` moved from 130.000pt columns to 23.988pt;
  Word's edges are 24.000pt apart. Final truth-minus-ours vertical-edge deltas
  are 0.02–0.06pt. `table-merges` edges are within 0.09pt and `table-nested`
  within 0.08pt.
- **Result before ink weighting:** SSIM is 0.999422 (`table-fixed`), 0.999697
  (`table-merges`), and 0.999851 (`table-nested`), with exact page counts.

_Invariant going forward: a calibration commit gets a log entry here._
