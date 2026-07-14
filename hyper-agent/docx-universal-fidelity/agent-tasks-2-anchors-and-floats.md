# Agent tasks 2 — anchored drawings and floating tables (file 1)

Depends on: agent-tasks-1 (fixture must already clear the parser root causes). All work in `office-core/`. References `main` @ `4bb9efc`.

## What file 1 actually contains (verified against its XML)

| Block (of 80 top-level body blocks) | Construct | Today's verdict |
|---|---|---|
| block 16 | table with `w:tblpPr w:vertAnchor="text" w:horzAnchor="text" w:tblpX="0" w:tblpY="1" w:rightFromText="187"` — a **left-floating info table**, body text wraps to its right | converts, but renders as a full-width block (no side wrap) |
| block 47 | one paragraph: anchor A (77×77 pt image, `positionH margin/align=left`, `positionV margin/align=top`, `wrapSquare bothSides`) **plus** one inline image | anchor A → 2 violations (`positionV relativeFrom`, `positionV alignment`) |
| block 49 | one paragraph: anchor B (77×77 pt, `positionH margin/align=right`, `positionV margin/align=top`, `wrapSquare bothSides`) **plus** anchor C (20×19 pt, `positionH column/posOffset=1931670EMU≈152pt`, `positionV paragraph/posOffset≈0.55pt`, `wrapTopAndBottom`) | anchor B → 2 violations; anchor C → 1 violation (`positionH relativeFrom` ≠ margin); **plus** `"multiple anchored drawings in one paragraph"` (`profile.rs::Collector::paragraph`, `paragraph.rs` ~357) |

Word's ground truth (1.pdf, page carrying blocks 47–49): images A and B sit at the **top corners of that page**, body text flows between and beneath them; C floats within its paragraph with top-and-bottom wrap. So the envelope needs: margin-relative vertical anchoring (page-scoped floats), vertical `align`, column-relative horizontal, coexisting left+right wrap bands, and >1 anchor per paragraph when at most one is paragraph-relative.

---

## T2.1 — Widen `parse_anchor`'s positioning subset (`oxml/wml.rs`)

`AnchorProps` currently hardcodes `relative_from_h: AnchorOrigin::Margin`, `relative_from_v: AnchorOrigin::Paragraph` and rejects everything else (`parse_anchor`, ~2200: `!= Some("margin")` / `!= Some("paragraph")` → violations; vertical `align` always rejected in `parse_position`).

1. Make the origins real:
   ```rust
   // in parse_anchor
   let h_origin = match horizontal.and_then(|v| v.attribute("relativeFrom")) {
       Some("margin") => AnchorOrigin::Margin,
       Some("page") => AnchorOrigin::Page,
       Some("column") => AnchorOrigin::Paragraph, // column start == the text column origin the
                                                  // paragraph-relative machinery already uses
       other => { reject(&format!("wp:positionH relativeFrom {other:?} is outside the supported subset")); AnchorOrigin::Margin }
   };
   let v_origin = match vertical.and_then(|v| v.attribute("relativeFrom")) {
       Some("paragraph") => AnchorOrigin::Paragraph,
       Some("margin") => AnchorOrigin::Margin,
       Some("page") => AnchorOrigin::Page,
       other => { reject(&format!("wp:positionV relativeFrom {other:?} is outside the supported subset")); AnchorOrigin::Paragraph }
   };
   ```
   (`AnchorOrigin` already has Page/Margin/Paragraph variants — used by frames.) Keep `line`, `character`, `leftMargin`/`rightMargin`/`insideMargin`/`outsideMargin` as violations (typed, detailed).
2. Vertical alignment: in `parse_position`, accept for the vertical axis `top → FrameAlign::Start`, `center → Center`, `bottom → End`; keep `inside`/`outside` as violations. Delete the unconditional `if !horizontal_axis && !aligns.is_empty() { reject(…) }`.
3. `wrapSquare`/`wrapTight` `wrapText="bothSides"` acceptance currently requires `x_align ∈ {Start, End}` — extend to also accept a **margin/page-relative float** whose resolved band hugs one content edge (left or right); the layout task computes bands, so the parser check relaxes to: `bothSides` allowed when `x_align ∈ {Start,End}` **or** `h_origin != Paragraph`. `largest` stays allowed as-is.
4. Constraint move: delete the parser/profile/layout **"multiple anchored drawings in one paragraph"** rejections (`profile.rs::Collector::paragraph` `anchor_count > 1`, and `paragraph.rs` ~344–360) and replace with the layout-level rule in T2.2 step 5 (at most one *paragraph-relative* float per paragraph; any number of *page-scoped* floats). Keep "anchored drawing in a framed paragraph" and the header/footer/table-cell/note/text-box story bans exactly as they are.
5. Anchors inside **multi-column sections** are new territory — fail closed: during layout (flow), if a unit with any anchored item lands in a section whose `ColumnSpec::is_multi()`, return `Unsupported(AnchoredDrawing, "anchored drawing in a multi-column section")`. Mirror the same structural check in `profile.rs` (walk: paragraph has anchor && its governing section is multi-col) so profile/convert cannot disagree. (File 1 is single-column; file 3 has no anchors.)

## T2.2 — Page-scoped floats in layout (`layout/paragraph.rs`, `layout/flow.rs`)

Today: anchors are placed relative to their anchoring line (`AnchoredLineItem` carried per line; wrap via a single-edge `WrapExclusion` that narrows the anchor paragraph and, through `flow.rs::build_units`'s `active_wrap` chain, following paragraphs). That machinery stays for `AnchorOrigin::Paragraph` vertical. Add a second placement class:

1. **Classify at layout entry:** an anchor with `v_origin ∈ {Margin, Page}` is a *page float*; with `v_origin == Paragraph` it remains a *paragraph float* (existing path, including anchor C).
2. **Model:** add `PageFloat { image/item, w_pt, h_pt, x_spec: (h_origin, x_pt, x_align), y_spec: (v_origin, y_pt, y_align), wrap: WrapMode, distances_pt: [f64;4], behind_doc, relative_height }`, produced by `layout_paragraph*` instead of an `AnchoredLineItem` for that anchor, and carried on the paragraph's first `Unit` (`Unit` gains `page_floats: Vec<PageFloat>`).
3. **Resolve at pagination** (`flow.rs::paginate`): when the unit carrying page floats is placed on a page, resolve each float to page coordinates: x from margin/page box + offset or align (left/center/right over `geo.content_width_pt()`/`geo.width_pt`); y from `geo.content_top_pt` (Margin) or `0` (Page) + offset or align over the corresponding height. Paint via the existing underlay/overlay channels (`behind_doc`, `relative_height`) so z-order semantics are identical to paragraph floats.
4. **Two-pass wrap reflow.** Pre-laid unit heights cannot react to a float discovered at pagination, so:
   - **Pass 1:** build units with no page floats active; paginate; record for each page the resolved page-float bands `[(side, x0..x1, y0..y1)]` (side = band hugging left or right content edge; a `wrapSquare` float whose horizontal window leaves text room only on one side; `wrapTopAndBottom` bands span the full width; `wrapNone` produces no band).
   - **Pass 2:** re-run `build_units` for the whole section with a page-band schedule: a unit that pass 1 placed on page *p* lays out against page *p*'s bands (converted to `WrapExclusion`s measured from the unit's own y — extend `WrapExclusion` to carry **left and right bands simultaneously**: `left_band: Option<Band>, right_band: Option<Band>`, line window = `[left_band.end, right_band.start]`; both bands active is exactly file 1's corner-images page). Re-paginate. If any float lands on a different page than pass 1 assumed, repeat (bounded: 3 iterations); on non-convergence return `Unsupported(AnchoredDrawing, "page-anchored drawing did not converge")` — fail-closed, never a mangled page.
   - Text **above** the float on the same page: with `y_align=top`/offset-0 bands the band starts at `content_top`, so every unit on that page is inside the band range and narrows — matching Word. A float whose band would start *above* content already placed on the page in pass 2 ordering is impossible by construction (bands apply from content_top; units narrow rather than collide).
5. **Per-paragraph constraint (replaces the deleted blanket rule):** during `layout_paragraph*`, more than one *paragraph-relative* float in one paragraph → `Unsupported(AnchoredDrawing, "multiple paragraph-relative drawings in one paragraph")`. Page floats are exempt (any count; overlap between their bands handled next).
6. **Band overlap policy:** two page-float bands on the same page overlap vertically:
   - opposite sides (left+right): allowed when the middle text window ≥ 36 pt, else `Unsupported(AnchoredDrawing, "anchored drawings leave no text window")`;
   - same side or a full-width band overlapping a side band: `Unsupported(AnchoredDrawing, "overlapping anchored drawing exclusions")` (reuse the existing detail string, `paragraph.rs` ~381).
7. **Profile parity:** `profile_docx` currently runs the full layout preflight when `body_anchor_count > 1`. Change the trigger to `body_anchor_count >= 1 && any anchor is a page float — or body_anchor_count > 1` so every new failure mode above is predicted by `profile()`. (File 1 triggers either way; single-page-float docs are the new case.)

**Wrap distances:** bands extend by `distL/distR/distT/distB` exactly as the paragraph-float band math does today (`paragraph.rs` ~396–410). **Determinism:** pass-2 fixpoint is a pure function of input; no randomness introduced.

## T2.3 — Floating-table side wrap (`layout/flow.rs::build_units`, table branch)

Today (`flow.rs`, `BlockContent::Table` with `t.props.floating`): the rows are absolutely offset by `tblpX/tblpY` and pushed as one `Unit` whose `height` spans the whole table — following text resumes *below* it; nothing flows beside it.

1. Compute the floating table's footprint: width = resolved grid width (sum of `layout_table` row widths — expose it from `layout_table`'s result rather than re-deriving), height = `row_y` as today.
2. Emit the table's items as **anchored items** (underlay/overlay not needed — tables sit in text flow; keep them in `items` of a zero-height unit) positioned at `x = origin(horzAnchor) + tblpX_pt`, `y = unit top + tblpY_pt` (origins via the same `AnchorOrigin` mapping the parser already produces: `text→Paragraph`, `margin`, `page`).
3. Activate a `WrapExclusion` band exactly like a `wrapSquare` paragraph float: side = left if the table hugs the left content edge (file 1: `tblpX=0`, `horzAnchor=text`), band width = table width + `rightFromText` (and symmetrical `leftFromText` when right-hugging), band height = table height + `bottomFromText`; the existing `active_wrap` chain then narrows following paragraphs. A floating table whose band leaves < 36 pt of text window falls back to today's full-width-block behavior (documented-lossy `Warning { code: "floating_table_block_fallback" }`) rather than failing — the construct already converted before this task; do not regress it into an error.
4. `vertAnchor` ∈ {`margin`,`page`} for tables: keep today's absolute placement, no band (rare; note as TODO), i.e. only `vertAnchor="text"` gains wrap.

## T2.4 — Tests

Unit fixtures via `testkit` (one per new form) + assertions in `office-core/tests/docx_layout_test.rs` following its existing item-geometry assertion style:
1. margin/top-left 77×77 pt square-wrap float: first body line's x window starts ≥ 77pt + distL; a line below the band regains full width; the float paints at `content_top`.
2. margin/top-right float + margin/top-left float on one page: middle window `[left_band.end, right_band.start]`; both floats painted; profile preflight passes.
3. `positionH column/posOffset` + `positionV paragraph/posOffset` small float with `wrapTopAndBottom` (anchor C shape): band spans full width for its height.
4. Two paragraph-relative floats in one paragraph → typed unsupported (both in profile and convert — parity assert).
5. Anchor in a 2-column section → typed unsupported, profile == convert.
6. Floating table: following paragraph's lines narrowed by table width + 9.35 pt (`rightFromText="187"`), text resumes full width below; block-fallback warning fires for an over-wide float table.
7. Non-convergence guard: construct a float pathologically near a page boundary; assert typed error, not hang (bound = 3 passes).
8. Full-fixture smoke: `officecore convert 1.docx` succeeds after T2 (footnote markers still wrong until task 4 — fine); page count 10 ± the manifest's `max_page_count_delta` (0) is the H3 gate, not this task's.

## Local verification

```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
target/release/officecore profile 1.docx     # expect: supported (or only missing_glyph pending task 4.5 outcome)
target/release/officecore convert 1.docx -o /tmp/1.pdf && echo OK
cargo run -p fidelity -- run --format docx   # existing cases stay green (esp. `complex/` — its anchors are paragraph-relative and must not shift; SSIM floor 0.927610)
```
Log any layout-constant tweak (band margins, 36 pt window floor) in `office-core/LAYOUT_CALIBRATION.md` with evidence, per that file's invariant.
