# Agent tasks 3 — continuous sections and column blocks (file 3)

Depends on: agent-tasks-1. Independent of agent-tasks-2 (parallel-safe: different regions of `flow.rs`; coordinate the final merge on `paginate`). All work in `office-core/`. References `main` @ `4bb9efc`.

## The document and the defect

`gdocs-columns/source.docx` (file 3) has five sections, all Letter with identical margins (1800/1440 twips, header/footer 720):

| Section | ends at | `w:type` | `w:cols` | footerReference |
|---|---|---|---|---|
| 0 | 1st paragraph-level `sectPr` | *(absent → nextPage)* | 1 col | `default` → `footer1.xml` |
| 1 | 2nd | `continuous` | `num=2 equalWidth=0`, two `w:col w:w="4320" w:space="0"` | — |
| 2 | 3rd | `continuous` | 1 col | — |
| 3 | 4th | `continuous` | same 2-col | — |
| 4 | body-level final `sectPr` | `continuous` | 1 col | — |

Word ground truth: **3 pages**, the two 2-column bands flowing on the same pages as the 1-column text, columns balanced at each band's end, the footer on every page.

**Defect 1 — merge direction / type semantics.** In OOXML, a paragraph-level `sectPr` ends its own section and its `w:type` describes how *that same section begins* (the transition INTO it). Proof from this document: if `w:type` governed the break *after* a section, section 0's implicit `nextPage` would force a page break before the first 2-column band → ≥ 4 pages, contradicting the 3-page ground truth. The engine already treats the type as self-describing in one place (`flow.rs` `needs_blank` for Odd/Even uses the batch's own `sect_type`) but the batching loop does the opposite: a `Continuous` section with same geometry is merged **forward** into the *next* section's batch (`layout_docx`: `if section.sect_type == SectType::Continuous && !geometry_differs(section, next) { continue; }`), and every batch starts fresh pages. For file 3 this collapses sections 1–4 into the final 1-column batch **and** inserts a page break after section 0. Both wrong.

**Defect 2 — one column spec per batch.** `section_columns` is applied per batch, so even with correct grouping, a continuous 2-col band inside a page has nowhere to live: pagination assumes one `(column_widths, column_offsets)` per page run.

**Defect 3 — header/footer inheritance.** Only section 0 carries the `footerReference`. ECMA-376 §17.10.5/§17.10.3: a section without its own header/footer references inherits the previous section's. The engine selects from `sect.footer_refs` only (`flow.rs::render_headers_footers`/`select_hf`), so after regrouping, pages governed by sections 1–4 would lose the footer. (Today the bug is masked for single-section documents — files 2/4 keep their footer because it sits on the final `sectPr`.)

## T3.1 — Restructure `layout_docx` around section runs with correct transitions

Replace the boundary/merge loop with an explicit model:

```rust
struct SectionRun<'a> {
    blocks: &'a [BlockContent],
    sect: &'a SectPr,       // the sectPr that ENDS these blocks = this section's own props
}
// build: boundaries in document order; blocks[start..=b_k] paired with boundaries[k];
// trailing blocks pair with document.final_sect. No merging here at all.
```

Pagination walks the runs with a **persistent page cursor** (`WorkPage`, `y`, `column`) instead of per-batch page vectors:

- Transition INTO run *k* (k = 0 starts the document; no break):
  - `NextPage` → close the current page if it has content.
  - `Continuous` → keep the current page and `y`; **require** `!geometry_differs(prev.sect, run.sect)` — otherwise keep today's typed failure (`UnknownElement`, "continuous section break changes page geometry"; `profile.rs::add_section_reasons` already mirrors this — unchanged).
  - `OddPage`/`EvenPage` → close the page, then insert a blank page when the parity of the next page number demands it (port the existing `needs_blank` logic; its self-describing reading of `sect_type` was already correct).
- Each run lays out with **its own** `section_columns(&geo, &run.sect.cols)`; `build_units` per run (list counters, `active_wrap`, `prev_after` flow *across* continuous transitions — pass them through; reset `prev_after` spacing across `NextPage` like a fresh page today).

## T3.2 — Column *bands* within a page

Generalize pagination from "columns per page" to "columns per band":

1. A **band** = the vertical strip a run occupies on the current page, `[band_top, band_bottom]`, with that run's column widths/offsets. A page may stack several bands (file 3 page 1: 1-col band, then 2-col band, …).
2. Placement inside a band uses the existing column logic (`column_break_before`, fill-then-spill, `content_bottom`), with two changes:
   - the fill floor is the *page* bottom (`full_bottom`) — a band grows downward until its run's units are exhausted or the page ends; if the page ends mid-run, close the page and continue the same run in a fresh band at the next page's `content_top` (re-emitting table headers exactly as today);
   - **balancing:** when a run with `N > 1` columns *ends* and the next run is `Continuous` on the same page (or the run is the document's last and ends mid-page), balance: the existing heuristic already computes `content_bottom = top + ceil(total_height / N) + 0.5` — re-scope `total_height` to the units of *this run only* and `top` to `band_top`. Word balances continuous-section columns; keep the heuristic, calibrate at H3 (log in `LAYOUT_CALIBRATION.md` if adjusted).
3. `align_body_vertically`, footnote reservation (`full_bottom` shrink), endnote append: operate on the *page*, not the band — audit each helper for hidden per-batch assumptions (`place_footnotes` and `append_endnotes` iterate pages; they are page-scoped already; `collect_outline` maps `unit_ys` — keep unit indices unique across runs by offsetting, or key outline by `(run_idx, unit_idx)`).
4. Unequal columns: `section_columns` already honors explicit `w:col` widths (file 3: 216 pt + 216 pt, space 0). No change; add the test.

## T3.3 — Header/footer inheritance chain

1. When building `SectionRun`s, resolve effective header/footer refs: `effective_refs[k] = if run.sect.{header,footer}_refs.is_empty() { effective_refs[k-1] } else { own }` — per kind (headers and footers inherit independently; a section that defines *any* footer ref replaces the whole footer set, matching Word).
2. `render_headers_footers` runs once per page with the refs of the **section governing that page** = the section whose first band starts the page (page's first-placed run). Record `governing_run: usize` on `WorkPage` when the page opens.
3. `titlePg` and `page_num_start`/`page_num_fmt` likewise read from the governing section (existing semantics per batch → per governing section). Page numbers continue across continuous sections (only an explicit `w:pgNumType w:start` restarts — file 3 sets `start` on section 0 only).
4. `profile.rs::header_footer_refs` already unions refs across all sections — profile parity holds without change.

## T3.4 — Profile parity guard

Layout gains no *new* rejection here (the geometry rule already exists in both). But defensively: add to `profile_docx` the same preflight widening as T2.7 — run the full layout preflight when the document has **≥ 2 sections and any multi-column spec** (`sects.iter().any(|s| s.cols.is_multi())`). Cheap (file 3 lays out in ms) and guarantees `profile()` can never approve a column configuration `layout_docx` refuses.

## T3.5 — Tests

`office-core/tests/docx_layout_test.rs` (+ a focused new `docx_sections_test.rs` if the file is getting long — follow existing naming):
1. **file-3 shape (synthetic):** 5 sections, types as in the table above, A4-pinned via testkit; assert: no page break at section transitions; section-1 units place at two x-offsets (216 pt columns, offsets 0 and 216 from content left); section-2 units back to full width **below** the 2-col band; balancing puts ⌈n/2⌉ units in column 0.
2. **NextPage still breaks:** two sections, second `nextPage` (absent type) → second run starts page 2 (regression for the corrected direction).
3. **OddPage/EvenPage parity blanks** preserved (port of existing behavior — reuse its current test if present, else add).
4. **Geometry change through continuous** → typed `UnknownElement` in both profile and convert (parity assert).
5. **Footer inheritance:** footerReference on section 0 only, 3 sections → footer items present on every page (assert per-page items contain the footer text run); a section with its own footer ref *overrides* from its pages on.
6. **Mid-page band page-break:** a 2-col run too tall for the page continues balanced across pages (column fill on page 2 restarts at content_top).
7. **Full fixture:** `officecore convert 3.docx` → 3 pages (assert page_count == 3; the manifest gate re-checks at H3 with `max_page_count_delta = 0`).

## Local verification

```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
target/release/officecore profile 3.docx && target/release/officecore convert 3.docx -o /tmp/3.pdf
cargo run -p fidelity -- run --format docx    # all existing cases green; complex/ SSIM floor 0.927610 intact
```
Column-balancing constant changes (the `ceil + 0.5` fudge) belong in `LAYOUT_CALIBRATION.md` with page-level evidence from the H3 report.
