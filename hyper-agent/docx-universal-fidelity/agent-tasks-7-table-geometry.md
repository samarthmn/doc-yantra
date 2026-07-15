# Agent tasks 7 — the table half-point displacement (unblocks `gdocs-tables-longdoc`)

**Evidence base (handoff, 2026-07-15):** page 41 translated +1 raster px right/down (= **0.5 pt** at the harness's 144 dpi) scores 0.9571 vs 0.8125, with identical glyph outlines; borders contribute 68.2% of the deficit and border-free glyph windows 31.7%, and **they move together**; a global shift drops the corpus from 16/18 to 11/18 passes and is **forbidden**. Conclusion: a *constant, conditional* displacement of whole table blocks on specific table/page states. The fix sequence below is: measure exactly (T7.1–T7.3), fix the confirmed convention(s) (T7.4), finish the border painter (T7.5). Code references are against branch `e1ca853` — complete step 0 of `agent-tasks-6-on-demand-fonts.md` first and re-locate by symbol.

## Code-grounded hypotheses (from the current `layout/table.rs`)

The committed table layout already runs row coordinates **border-centerline to border-centerline**, insets cell content by `border_top + m_t`, collapses horizontal boundaries once via `horizontal_boundary_plan`, and reserves the outer top/bottom half-strokes by shifting `rows.first_mut()` down by `leading = max_top_stroke/2` and adding `trailing` to the last row. Given that, the candidate mechanisms — each ≈0.25 pt for the fixtures' `sz=4` (0.5 pt) borders, two compounding to the observed 0.5 pt:

| # | Hypothesis | Mechanism in code | Predicted probe signature |
|---|---|---|---|
| H1 | **Continuation fragments miss the top half-stroke reservation.** `leading` is applied only to the *table's* first row inside `layout_table_with_width`; page-slicing happens later in `flow.rs`, so the first row of a page-continuation fragment paints its top boundary centered **on** the fragment top (bleeding above `content_top`) while Word starts the fragment's boundary box *below* the page top (LO special-cases exactly this: `lcl_IsFirstRowInFollowTableWithoutRepeatedHeadlines`, #i9860/tdf#150308). | `table.rs` leading shift; `flow.rs` row-unit slicing | y-delta ≈ +half-stroke on continuation pages only; table-start pages clean |
| H2 | **No x-axis outer half-stroke reservation.** `col_x` starts at `aligned_left` = the leftmost border **centerline**; the left outer border bleeds half a stroke left of the grid, and there is no horizontal analog of `leading`/`trailing`. If Word anchors the border's **outer edge** at the table x-position, our whole block (borders + text) sits half a stroke left. | `layout_table_with_width` → `aligned_left`/`column_offsets` | x-delta ≈ +half-stroke wherever outer vertical borders exist, on all pages of bordered tables |
| H3 | **`tblInd` reference convention.** Current choice (documented in code): negative `tblInd` locates the grid edge; positive locates the leading cell *content* edge (subtracts `leading_cell_margin`). The fixtures author `-108` twips on every table. | same site | uniform x-delta = margin-or-half-border-sized on *all* bordered and borderless tables alike |
| H4 | **Row-boundary double-paint drift.** Each internal boundary is emitted twice (row N bottom at `y=height`, row N+1 top at `y=0`) relying on exact coincidence; any height rounding divergence paints two nearby lines. | `emit_horizontal_boundary` call pattern | duplicated near-coincident segments in *our* probe output |
| H5 | **Cell-margin reference edge.** Current: content top = `border_top(half) + m_t` (i.e., margins measured from the border inner edge) — believed Word-correct; verify, don't assume. | `emit_cell` content box | text-origin deltas *inside* cells while borders align |

## T7.1 — The vector geometry probe (measure, don't guess)

New fidelity subcommand: `cargo run -p fidelity -- probe --case <name> --page <n> [--json <path>]`.

1. **Input:** the case's `source.docx` converted in-process (ours) and its `ground-truth.pdf` (Word's). Both are machine-readable vector PDFs — no rasters involved.
2. **Content-stream interpreter** (new `fidelity/src/probe.rs`; parse objects with the `lopdf` re-export already available through `pdf_core`): tokenize the page content stream(s), tracking graphics state — `q/Q` stack, `cm` CTM, `w` stroke width, `Tm/Td/TD/T*` text matrices, `Tf` size. Collect:
   - **border-like elements:** stroked segments (`m … l … S|s`) and rectangles (`re` followed by `S`, `f`, or `B`) whose min dimension ≤ 2 pt (Word commonly paints table rules as thin *filled* rects; ours are stroked lines) — record as axis-aligned segments `(orientation, x0, x1, y, width)` / `(x, y0, y1, width)` in device space;
   - **text origins:** for each `Tj/TJ/'`, the device-space origin of the run plus font size; decode the first character through the font's ToUnicode when present (Word subsets carry it), else leave the byte — pairing is positional anyway.
   - Recurse one level into Form XObjects (`Do`) with the CTM composed; skip inline images and curves (`c/v/y`).
3. **Normalize** to top-left page coordinates (flip by MediaBox height), round to 0.01 pt.
4. **Pair and report:** nearest-neighbor match ours↔truth within 3 pt per element class; emit (a) a console table of unmatched elements, (b) delta clusters — `(dx, dy)` histograms split by element class (h-border / v-border / text) and by y-band (fifths of the page), (c) duplicate-segment detection within *our* set (H4). JSON out for regression pinning.
5. **Determinism/scope guard:** the probe is dev-only diagnostics (native, behind the fidelity crate) — no engine code path changes; do not gate CI on truth-side parsing quirks beyond the pages it's pointed at.

Read the interpretation directly against the hypothesis table: constant `dy` on continuation pages only → H1; constant `dx` on bordered tables → H2/H3 (borderless synthetic separates them); in-cell text deltas with aligned borders → H5; near-coincident duplicates → H4.

## T7.2 — Run the probe on the live evidence

`probe --case gdocs-tables-longdoc --page 41` (the measured worst page), plus one table-start page and one continuation page of the same table, plus `--case complex --page 6` as a passing control. Record outputs under `tmp/` and summarize the confirmed hypothesis set in the PR description before writing any fix.

## T7.3 — Synthetic bisect matrix (out-of-the-box empirical isolation)

Six tiny single-variable DOCX fixtures built via `testkit` and exported to ground truth by the owner (human task H7, same word.cloud.microsoft flow + provenance rules as H2):

| Fixture | Isolates |
|---|---|
| bordered 3×20 table, `sz=4`, starts top of page | H2/H5 baseline |
| same, preceded by filler so the table **splits across a page** | H1 (continuation edge) |
| same table with **no borders**, split across a page | separates border accounting from row stacking |
| bordered, `tblInd="-108"` vs `tblInd="0"` vs absent (3 variants) | H3 reference edge |

These live under `office-core/fidelity/diag/<name>/{source.docx, ground-truth.pdf}` — **not** under `fidelity-corpus/` (the release gate scans every `fidelity-corpus/docx/*` directory strictly; diagnostics must not join the blocking corpus). The probe accepts `--diag <name>` for these.

## T7.4 — Fix the confirmed convention(s)

Apply **only** what T7.2/T7.3 confirm; each fix lands with a geometry regression test pinning absolute coordinates measured from the Word ground truth:

- **H1 fix (fragment-aware outer edges):** move the outer half-stroke reservation from "first/last row of the table" to "first/last row of each *page fragment*". Concretely: `layout_table_with_width` exposes `leading`/`trailing` on `TableLayout` instead of baking them into row 0 / row N; `flow.rs`, where table row units are placed and sliced across pages (`is_header_row`/`table_headers` machinery), applies the leading shift to the first row unit placed on each page fragment and the trailing extension to the last unit of each fragment, matching whatever Word's fragment-edge behavior the probe measured (including whether the broken edge repaints the outer or the inside stroke — LO's follow-table special case suggests Word paints the *table* top border on continuations without repeated headers; encode what the probe shows, cite it in the code comment).
- **H2 fix (x-axis reservation):** reserve the outer vertical half-strokes symmetrically: `aligned_left += left_outer_half` before `column_offsets`, and include `left_outer_half + right_outer_half` in the width used for `free_width`/alignment math (Center/End alignment must divide the *remaining* free width — add a test for `jc=center` + thick outer borders).
- **H3 fix:** only if the matrix contradicts the current documented convention — change the `start_offset` mapping and update its comment + the Google-negative-tblInd regression test together.
- **H4 fix:** emit each internal boundary **once** (owned by the boundary index, not by both adjacent rows) — fold into T7.5.
- **H5 fix:** only per probe evidence; content-box insets change is calibration-sensitive — pair with a `LAYOUT_CALIBRATION.md` entry.

**Forbidden:** any page- or document-global offset (the handoff's own measurements refute it: global +1px drops `complex` 0.937→0.833, `long-doc-pagination` 0.955→0.791). Every fix must be expressible as "this convention now matches Word's, verified by the probe on fixture X".

## T7.5 — Finish the shared-edge border painter (vertical seams)

`horizontal_boundary_plan` already collapses horizontal boundaries (segments merged per column run, wider-then-darker conflict rule). Complete the model:

1. Build the same plan for **vertical seams**: per column boundary `c`, per row, resolve cell-left vs neighbor-right vs `insideV`/outer table border with the same `choose_boundary_border` rule; today `resolve_cell_borders` lets *both* adjacent cells paint their own strokes — conflicting widths double-paint (LO collects and paints each boundary once for exactly this reason: `SwLineRects`/`BorderLines` merge, fdo#38215).
2. Merge collinear same-stroke segments across rows (one `Item::Line` per seam run instead of per row) — content-stream size drops as a free measurable.
3. Convert internal horizontal boundaries to single-emission (H4) in the same pass.
4. Keep output **vector and unsnapped** — pixel alignment is the rasterizer's job (LO snaps only at paint time via `SwAlignRect`); the harness rasterizes ours and Word's identically through pdfium, so coordinate agreement is what scores.

## T7.6 — Gate

1. `cargo run -p fidelity -- run --case gdocs-tables-longdoc` → **min page SSIM ≥ 0.90**, 44/44 pages (handoff baseline: 0.8125; the +1px experiment bounds the available headroom at ≈0.957).
2. Full corpus green: `complex` ≥ 0.9374 observed floor, `gdocs-columns` 0.9280, `gdocs-anchors-toc` per phase-6 gate; zero manifest/threshold/golden changes (`git diff -- fidelity-corpus` empty except the phase-5 manifests).
3. Probe JSON for page 41 committed to the PR description (before/after deltas), not to the repo.

## Human task H7 (with this phase)

Export the six T7.3 synthetic fixtures to PDF via word.cloud.microsoft (same flow as H2), place under `office-core/fidelity/diag/<name>/ground-truth.pdf`. These are diagnostics — no manifests, no SSIM gates, keep out of `fidelity-corpus/`.

## Local verification

```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
cargo run -p fidelity -- probe --case gdocs-tables-longdoc --page 41   # before/after
cargo run -p fidelity -- run --format docx                              # full corpus
cd wasm && ./build.sh && cd ../.. && node --test tests/
DRY_RUN=1 pdf-core/scripts/release.sh                                   # end-to-end gate preview
```
