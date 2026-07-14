# Agent tasks 1 — parser root causes (P0: files 1–3 enter the envelope)

All work in `office-core/`. Self-contained: everything an implementer needs is in this file plus the repo. Fixture pairs live in the planning thread until human-tasks H1 commits them under `fidelity-corpus/docx/gdocs-*` — for tasks in THIS file you only need `source.docx` bytes locally (any path works; nothing here depends on the corpus layout). Line references are against `main` @ `4bb9efc`; re-locate by symbol if drifted.

---

## T1.0 — Reproduce and freeze the baseline (do this first)

**Why:** the plan's failure analysis is a static trace; lock it against reality before changing code, and capture file 2's suspected `missing_glyph`.

1. Build the CLI: `cd office-core && cargo build --release` → `target/release/officecore`.
2. For each of the three fixtures: `officecore profile <n>.docx | jq .reasons` and `officecore convert <n>.docx -o /tmp/<n>.pdf; echo $?`.
3. Record the exact `reasons[]` JSON for 1/2/3 in the PR description. Expected from the plan's trace:
   - all three: `unknown_element` with detail `"w:rPr in w:pPr"` (deduped — one reason regardless of count);
   - file 1 additionally: `anchored_drawing` ×3 (`positionV relativeFrom`, `positionV alignment`, `positionH relativeFrom` details) and `field_unsupported` (`"field without cached result: TOC …"`);
   - file 2 additionally (to confirm): `missing_glyph` naming U+2076/U+2079/U+207B;
   - file 3: nothing beyond the universal reason.
4. If reality disagrees with the trace, STOP and reconcile with the plan author before proceeding — the remaining tasks assume this failure matrix.

**Acceptance:** baseline JSON committed in the PR description (not in the repo).

---

## T1.1 — Parse the paragraph-mark run properties (`w:pPr/w:rPr`)

**Root cause:** `office-core/src/oxml/wml.rs::parse_para_props`, the multi-name arm that currently reads:

```rust
"suppressLineNumbers"
| "suppressAutoHyphens"
| ...
| "textboxTightWrap"
| "rPr" => props
    .visible_unsupported
    .push(format!("w:{} in w:pPr", c.tag_name().name())),
```

`w:rPr` here is the **paragraph mark's** run properties (ECMA-376 §17.3.1.29, CT_PPr). Google Docs emits it on every paragraph — files 2/3 as a literal empty `<w:rPr/>` — so a single arm fails entire documents over invisible bookkeeping.

**Change 1 — model.** Add to `ParaProps` (same struct that carries `visible_unsupported`; keep field-order conventions):

```rust
/// Run properties of the paragraph mark (`w:pPr/w:rPr`). Affects the height
/// of an empty paragraph's line and the pilcrow; never the runs' own props.
pub mark_rpr: Option<RunProps>,
```

In `ParaProps::merge_over` (the merge at ~wml.rs:690–770), merge like the other optionals: `mark_rpr: self.mark_rpr.clone().or_else(|| base.mark_rpr.clone())`.

**Change 2 — parse.** In `parse_para_props`, remove `"rPr"` from the unsupported arm and add its own arm:

```rust
"rPr" => props.mark_rpr = Some(parse_run_props(c)),
```

Notes that keep this fail-closed:
- `parse_run_props` already routes unknown children of the mark rPr into `RunProps.visible_unsupported` via its catch-all — **but** those land on the *mark's* RunProps, which nothing currently surfaces. Surface them: in `profile.rs::Collector::paragraph` (right after the existing `eff_para.visible_unsupported` loop) add:
  ```rust
  if let Some(mark) = &eff_para.mark_rpr {
      for detail in &mark.visible_unsupported {
          profile.add_reason_deduped(UnsupportedCode::UnknownElement, detail.clone());
      }
  }
  ```
  This keeps `w:rPr/w:del` (deleted paragraph mark = paragraphs merge in final view — a *visible* semantic we do not implement) and any exotic child failing closed exactly as before, with a precise detail string.
- `parse_para_props` is reused by `styles.rs` for style-level `pPr`; a style's nested `pPr/rPr` now parses instead of poisoning every styled paragraph — same correctness argument.

**Change 3 — use it for empty-paragraph line height.** In `office-core/src/layout/paragraph.rs`, the empty-paragraph path currently derives line height from the effective run props of an empty run set (locate the branch where `para.runs` yields no atoms; it builds a line from the paragraph's effective `RunProps` via `doc.styles.run_props(&para.props, &RunProps::default())`). Change the *empty-paragraph case only* to overlay the mark:

```rust
let mark = eff_para.mark_rpr.clone().unwrap_or_default();
let eff_run = doc.styles.run_props(&para.props, &mark);
```

Word sizes an empty paragraph's line by the mark's formatting (this is why file 1's spacer paragraphs carry `w:sz` on the mark). Do NOT apply `mark_rpr` to non-empty paragraphs' runs, and do NOT wire it into numbering-label props in this task (calibration risk; revisit only if H3 review shows label-size drift — log in `LAYOUT_CALIBRATION.md` if so).

**Tests** (extend `office-core/tests/wml_test.rs` + profile tests in `src/profile.rs`):
1. `pPr` with empty `<w:rPr/>` → profile `supported`, zero reasons.
2. `pPr/rPr` with `<w:sz w:val="56"/>` on an empty paragraph → layout line height greater than an 11pt default line (assert via the existing layout-test pattern in `tests/docx_layout_test.rs`).
3. `pPr/rPr` containing `<w:del/>` → profile NOT supported, `unknown_element` detail `"w:del in w:rPr"` (fail-closed boundary held).
4. Existing corpus: `cargo run -p fidelity -- run --format docx` — no regression on the 14 existing cases (empty-paragraph height change can shift `plain-paragraphs` et al. only if those fixtures carry mark rPr — they don't).

**Acceptance:** `officecore profile 3.docx` (gdocs-columns source) reports `supported: true`; file 2 reports either supported or only `missing_glyph` reasons; file 1's reasons no longer include `unknown_element "w:rPr in w:pPr"`.

---

## T1.2 — Complex fields that span paragraphs (file 1's TOC)

**Root cause:** `wml.rs::inline_level` owns the field state machine locally per paragraph (`let mut field: Option<FieldBuild> = None;` … flush at paragraph end), so a `fldChar begin`/`separate` in one paragraph and `end` 17 paragraphs later (Google's TOC-in-`w:sdt`) yields a `ComplexFieldRegion` with a non-empty instruction and **empty** `cached_runs` → `profile.rs::Collector::paragraph`:

```rust
if f.cached_runs.is_empty() && !f.instruction.trim().is_empty() { … FieldUnsupported … }
```

Also: `w:hyperlink` between `separate` and `end` takes `inline_level`'s hyperlink branch and never reaches `cached_runs`, so even a single-paragraph TOC with hyperlink entries would starve the region.

**Design — field segments with shared liveness** (implement exactly this):

1. **Field content becomes inline content.** In `wml.rs`, change:
   ```rust
   pub struct Field {
       pub instruction: String,
       pub content: Vec<InlineContent>,   // was: cached_runs: Vec<Run>
       /// True only on the terminal segment of a field whose whole span
       /// produced no visible result content.
       pub starved: bool,
   }
   ```
   `FieldBuild` mirrors it: `instruction: String, content: Vec<InlineContent>, in_result: bool, any_content: bool`.
2. **State lives on the parser.** Move the `Option<FieldBuild>` into `WmlParser` (`self.field`). `inline_level` uses `self.field`; **save/restore** it (`std::mem::take` + restore) around the `w:txbxContent` recursion in `wml_drawing`/`vml_shapes` so a text-box story never inherits the body's open field. Each fresh `WmlParser` (headers, footers, notes are parsed with their own parser instance — see `parse_hdr_ftr_with_flags`, `parse_notes`) starts clean, so story isolation elsewhere is automatic.
3. **Routing while a field is open.** In `inline_level`/`run_into_field`:
   - `begin`: flush any open field as a terminal segment (`starved = !any_content`), then open a new one (innermost-wins, unchanged).
   - not-yet-`separate`: accumulate `instrText` (unchanged).
   - after `separate` (`in_result`): push **every** InlineContent produced — runs via `self.run(r)?`, and route the `w:hyperlink` / `w:sdt` / `mc:AlternateContent` branches into `fb.content` instead of `out` (one shared `push_inline(&mut self, item)` helper that appends to the open field's `content` when `in_result`, else to `out`, keeps every branch consistent). Set `fb.any_content = true` when pushing anything non-empty.
   - `end`: emit `ComplexFieldRegion(Field { instruction, content, starved: !any_content })` and clear.
   - **paragraph end with field still open:** emit a *non-terminal* segment carrying whatever `content` accumulated in this paragraph (`starved: false` always for non-terminal segments), clear `fb.content`, keep `fb` (instruction + flags) alive for the next paragraph. The begin-paragraph therefore contributes a `(instruction, [])` non-terminal segment — harmless, renders nothing.
   - **story end** (`parse_document` after `block_level`, and the same point in `parse_hdr_ftr_with_flags`/`parse_notes`): flush an open field as terminal.
4. **Profile check** (`profile.rs::Collector::paragraph`) becomes:
   ```rust
   if f.starved && !f.instruction.trim().is_empty() {
       … FieldUnsupported, format!("field without cached result: {}", f.instruction.trim()) …
   }
   ```
   and the walker recurses `f.content` as inline content (runs, hyperlinks — reuse the existing InlineContent match instead of the runs-only loop).
5. **Layout** (`paragraph.rs::field_runs` and the atom scan at ~580/631/669): PAGE/NUMPAGES substitution logic is unchanged (those fields are single-paragraph; their segment carries the cached run). For every other instruction, render `f.content` exactly as ordinary inline content (this is Word's "final view" of a field: show the cached result). Update `flow.rs::blocks_contain_anchors` and `profile.rs::paragraph_anchor_count` to walk `f.content` recursively (hyperlink runs inside field content count).
6. **No nested-field support**: a `begin` inside an open result region flushes the outer as terminal (rule 3a). Word nests fields (e.g. TOC contains PAGEREF); Google's TOC does not. A nested `begin` mid-result therefore renders outer-then-inner segments sequentially — acceptable final-view v1; add a `// TODO(nested fields)` note.

**Tests** (`wml_test.rs` + `docx_layout_test.rs` + profile tests):
1. Three-paragraph field: ¶1 `begin` + `instrText " TOC \h"` + `separate`, ¶2 a `w:hyperlink` with a run "Heading 1\t3", ¶3 `end` → profile supported; layout produces the entry text; no `FieldUnsupported`.
2. Footer `fldSimple PAGE` + complex PAGE field regression: existing `page-fields` corpus case and `fidelity` run stay green (PAGE substitution path untouched — assert page numbers still render).
3. Unterminated `begin` at story end → terminal starved segment → `field_unsupported` (fail-closed held).
4. A field whose result is only whitespace runs → starved (assert `any_content` requires non-empty text or non-text content).
5. Text-box containing its own field while body field is open (save/restore test).

**Acceptance:** `officecore profile 1.docx` no longer reports `field_unsupported`; the TOC paragraphs render their cached entries (visual check happens at H3).

---

## T1.3 — Tolerate decimal measurement values (Google writes non-integer twips)

**Root cause:** `oxml/mod.rs::parse_i64` (`s.trim().parse::<i64>()`) returns `None` for `"192.00000000000003"`. Measurements silently vanish (file 1: `w:ind w:firstLine`/`w:hanging` ×14, `numbering.xml` `w:hanging="935.9999999999995"`). `TblWidth::parse` already tolerates decimals (f64) — the rest of the measurement surface must match it.

**Change.** Add to `oxml/mod.rs`:

```rust
/// Parses a signed measurement attribute that mainstream producers may write
/// with a fractional part (Google Docs: `w:w="-108.0"`, `w:firstLine=
/// "192.00000000000003"`). Counts and ids stay on `parse_i64`.
pub fn parse_measure(v: Option<&str>) -> Option<f64> {
    v.and_then(|s| s.trim().parse::<f64>().ok()).filter(|f| f.is_finite())
}
/// `parse_measure` in twips → points.
pub fn measure_twip_to_pt(v: Option<&str>) -> Option<f64> {
    parse_measure(v).map(|t| t / 20.0)
}
```

Swap call sites that read **lengths** (leave ids/counts — `numId`, `ilvl`, `abstractNumId`, note `id`, `start`, `gridSpan`, `cols@num`, `relativeHeight`, `compatibilityMode`, `outlineLvl`, band sizes — on `parse_i64`):

| Site (wml.rs unless noted) | Attribute(s) |
|---|---|
| `parse_spacing` | `w:before`, `w:after`, `w:line` (keep the `lineRule` branch: exact/atLeast → twips→pt; auto → value/240.0) |
| `parse_indent` | `left/start`, `right/end`, `firstLine`, `hanging` |
| `parse_tabs` | `w:pos` |
| `parse_row_props` → `trHeight` | `w:val` |
| `parse_margins` | all seven |
| `sect_pr` → `pgSz` | `w:w`, `w:h` |
| `parse_cols` | `w:space`, `w:col@w`, `w:col@space` |
| `table()` → `gridCol` | `w:w` (currently `parse_i64(...).unwrap_or(0)` — a decimal gridCol silently collapses a column to 0pt today) |
| `parse_frame_props` | the `twips` closure (uses raw `parse::<i64>` today) |
| `parse_run_props` | `w:spacing` (letter-spacing twips), `w:kern`, `w:sz`, `w:szCs`, `w:position` if/when parsed |
| `parse_anchor` | `posOffset` EMU (`parse::<i64>` → f64, `emu` value /12700.0), wrap distances |
| `oxml/settings.rs` | `defaultTabStop` |
| `oxml/numbering.rs::parse_level_indent` | all four |

Precision: parse to f64 and convert directly (no rounding to integer twips) — layout is f64 end-to-end and f64 parsing is deterministic, so output determinism is preserved. Integer inputs parse to identical values, so **existing fixtures cannot shift**.

**Tests:** unit tests per helper (`"192.00000000000003"` → 9.6000…pt; `"-108.0"` → −5.4pt; `"abc"`/`"NaN"`/`"inf"` → None); a fixture with decimal `gridCol` asserting column widths are non-zero; the existing `tbl_width_parsing` test untouched.

**Acceptance:** file 1's list indents match ground truth to within the SSIM gate at H3 (indent step 9.6pt now honored); zero `cargo test` regressions.

---

## T1.4 — Google-Docs conformance battery (regression armor)

**Why:** the producer idioms that broke files 1–3 must never regress, independent of the big fixtures.

1. Add a builder to `office-core/src/testkit.rs`: `pub fn gdocs_paragraph_docx() -> Vec<u8>` producing a minimal package whose body exercises, in one document: `pPr/rPr` (one empty, one with `color`+`sz`+`vertAlign w:val="baseline"`), explicit `w:val="0"` toggles (`b`,`i`,`smallCaps`,`strike`,`u w:val="none"`,`rtl`,`tblHeader`,`cantSplit`), `w:jc w:val="left"`, a table with `tblGridChange` inside `tblGrid` + `tblInd w:w="-108.0" w:type="dxa"` + decimal `gridCol`, `w:ind` with decimal `firstLine`, `sectPr` with `pgNumType` and a `footerReference` (plus the footer part), and an empty `<w:tcPr/>`.
2. Profile test: `gdocs_paragraph_docx()` → `supported: true`, `reasons: []`; stats count 1 table.
3. Convert test: emits ≥1 page; extracted text (`pdf_core::extract::extract_text`, dev-dependency already used by the fidelity roundtrip tests) contains the body probes.
4. Negative controls stay red: `w:ptab`, `w:rPr/w:del` mark deletion, `mc:AlternateContent` without fallback — each still `unknown_element`.

**Acceptance:** battery green; negative controls red.

---

## Local verification (cannot run in the planning sandbox — run all)

```bash
cd office-core
cargo test                                   # unit + integration incl. new battery
cargo clippy --all-targets -- -D warnings
cargo build --release && target/release/officecore profile /path/to/1.docx   # repeat 2,3 — compare with T1.0 baseline
cargo run -p fidelity -- run --format docx   # existing 14 cases must stay green
cd wasm && ./build.sh && cd ../.. && node --test tests/    # committed pkg + package suite
```
