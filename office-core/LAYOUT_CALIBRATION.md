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

_No adjustments yet — awaiting Word ground truths (human task H3) before the
first calibration pass._
