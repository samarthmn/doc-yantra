# Layout calibration log

Institutional memory for the fidelity-harness calibration loop (agent task
T4.5). The DOCX layout formulas in `src/layout/` contain constants tuned to
Word's behaviour. When a corpus case scores below its SSIM threshold, read the
heatmap, identify the axis, adjust the **one** relevant constant, re-run
`cargo run -p fidelity -- run --case <name>`, and log the change here.

## Method

Per failing case:
1. `cargo run -p fidelity -- run --case <name>` → open `fidelity-report/index.html`.
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

_Invariant going forward: a calibration commit gets a log entry here._
