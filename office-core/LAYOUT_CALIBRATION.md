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
| `flow.rs` | paragraph gap | `max(after_prev, before_next)` | Word collapse rule |

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

_Invariant going forward: a calibration commit gets a log entry here._
