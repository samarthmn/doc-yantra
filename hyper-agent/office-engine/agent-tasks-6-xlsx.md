# Agent tasks 6 — XLSX print engine

Prereq: tasks 2 & 4. Parallel-safe with task 5. New: `src/oxml/sml.rs` (stubbed in task 1), `src/xlsx/`. Target: reproduce **Excel's "Save As PDF" print output** for the workbook's sheets.

**XLSX v1 envelope.** Supported: cell values (shared strings incl. rich-text runs, inline str, numbers, booleans, errors, dates), number formats (builtin + common custom patterns below), cell styles (font/size/bold/italic/color, fill solid, borders thin|medium|thick|hair|dotted|dashed|double→rendered single with width map + `border_style_simplified` warning for double, alignment h/v, wrap text, shrink-to-fit ignored-with-warning), merged cells, explicit column widths/row heights + defaults, hidden rows/cols (skipped), page setup (paper size, orientation, scale or fitToWidth/Height, margins, header/footer text incl. `&P`/`&N`/`&A`/`&D` codes, print titles = repeated rows/cols, print area, gridlines/headings print options), multi-sheet (each sheet paginates independently; PDF concatenates in workbook order, honoring `--sheets` CLI filter). Formulas: **render the cached `<v>` value; never evaluate**; cell with formula and NO cached value → Unsupported{UnknownElement, "uncached formula"}. Unsupported (fail-closed): charts/drawings anchored on sheets (`xl/drawings` with chart/shape content → Chart/AnchoredDrawing; PICTURES via drawing are also Unsupported v1 — spreadsheets-with-images fall back), conditional formatting **when any CF rule's range intersects the printed range** (ConditionalFormatting — Excel prints CF results and we can't evaluate rules v1), pivot tables (detect `pivotCachePart` → UnknownElement "pivot"), data validation dropdowns (ignore — invisible in print), comments/notes (ignore — not printed by default), rich `dxf` table styles on `tableParts` (render plain + `table_style_partial` warning).

## T6.1 SML model — `src/oxml/sml.rs`

Parse `xl/workbook.xml` (sheet name/order/rel id/state visible|hidden, `definedNames` — capture `_xlnm.Print_Area` and `_xlnm.Print_Titles` per localSheetId), `xl/sharedStrings.xml` (plain + rich runs), `xl/styles.xml` (numFmts custom id≥164, fonts, fills patternType=solid fgColor, borders with per-edge style+color, cellXfs with applyX flags + indexed/theme/rgb colors — indexed palette: the standard 64-color legacy palette hardcoded), per-sheet `xl/worksheets/sheetN.xml`: `dimension`, `sheetFormatPr` (defaultColWidth chars default 8.43, defaultRowHeight pt default 15), `cols` (min/max/width/hidden/customWidth), `sheetData/row` (@r, @ht, @hidden, cells `c` with @r A1-ref, @s style idx, @t type, `v`/`is`), `mergeCells`, `pageMargins`, `pageSetup` (@paperSize code, @orientation, @scale, @fitToWidth/@fitToHeight — fit wins over scale when `sheetPr/pageSetUpPr @fitToPage=1`), `printOptions` (@gridLines @headings), `headerFooter` (oddHeader/oddFooter strings). A1 reference parser + rangeref parser (`Sheet1!$A$1:$D$20`) with unit tests.

## T6.2 Number format engine — `src/xlsx/numfmt.rs`

The formatted string is NOT stored in xlsx — this engine is mandatory for correctness.

1. Builtin IDs (hardcode the table): 0 `General`; 1 `0`; 2 `0.00`; 3 `#,##0`; 4 `#,##0.00`; 9 `0%`; 10 `0.00%`; 11 `0.00E+00`; 12 `# ?/?`→Unsupported detail "fraction"; 14 `m/d/yyyy`; 15 `d-mmm-yy`; 16 `d-mmm`; 17 `mmm-yy`; 18 `h:mm AM/PM`; 19 `h:mm:ss AM/PM`; 20 `h:mm`; 21 `h:mm:ss`; 22 `m/d/yyyy h:mm`; 37 `#,##0;(#,##0)`; 38 `#,##0;[Red](#,##0)`; 39/40 same with .00; 45 `mm:ss`; 46 `[h]:mm:ss`; 47 `mm:ss.0`; 48 `##0.0E+0`; 49 `@`.
2. Custom pattern parser — sections split on `;` (positive;negative;zero;text; missing sections fall back per Excel rules: 1 section = all, 2 = pos+zero / neg). Tokens: `0` `#` `?` digit placeholders (integer part fills right-to-left; decimals left-to-right, rounding half-up at the last placeholder), `.` decimal, `,` thousands separator when between digit placeholders / trailing `,` scales by 1000 per comma, `%` multiplies by 100 and appends %, `"literal"`, `\x` escaped char, `@` text placeholder, `_x` skip-width (render a space), `*x` fill (render single x — no justify v1), `[Red]`/`[Blue]`/… color modifiers (apply font color for that section), `[$-409]` locale prefixes (ignore), conditions `[>=100]` → Unsupported detail "conditional numfmt", `E+`/`E-` scientific. Date/time tokens: `yyyy/yy/mmmmm/mmmm/mmm/mm/m/dddd/ddd/dd/d/hh/h/ss/s/AM/PM/A/P` with the classic ambiguity rule: `m` adjacent (in token stream order) to `h` or `s` = minutes, else month; `[h]`/`[mm]`/`[ss]` elapsed forms. Serial date epoch 1899-12-30 (handle the phantom 1900-02-29: serial 60 renders as 1900-02-29 — reproduce Excel's bug), `date1904` workbook flag honored. `General`: integers plain; else up to 11 significant digits total display width, trailing zeros trimmed, switch to scientific when |x| ≥ 1e11 or < 1e-9 (implement Excel's 11-char rule approximately; document + calibrate against corpus case `number-formats-matrix`).
3. Unparseable custom pattern → profile reason UnknownElement detail `numFmt:<pattern>` (fail closed, per envelope).
Tests: table-driven — ≥ 60 (value, pattern, expected) rows copied from Excel-verified outputs (compute them in Excel while authoring the corpus and paste as constants).

## T6.3 Geometry: widths & heights — `src/xlsx/widths.rs`

Column width (stored in "characters" of the workbook's normal font, Calibri 11 → **MDW = 7 px** at 96dpi):
```
width_px = floor(((256 * chars + floor(128 / MDW)) / 256) * MDW)
width_pt = width_px * 72 / 96
```
defaultColWidth when absent = 8.43 chars → 64 px → 48 pt. Row height: stored directly in points (@ht), default from sheetFormatPr (15pt). Hidden rows/cols contribute 0 and are skipped entirely. If the workbook's normal font (styles.xml font id 0) is not Calibri 11, still use MDW=7 v1 with warning `nonstandard_normal_font` (calibrate later if corpus demands).

## T6.4 Print model — `src/xlsx/paginate.rs`, `src/xlsx/render.rs`

1. Printed range: `_xlnm.Print_Area` if defined for the sheet, else A1..the used range (max row/col containing a value, style-only cells with non-default fill/border also count).
2. Paper: map @paperSize codes (1=Letter 612×792pt, 5=Legal 612×1008, 8=A3 842×1191, 9=A4 595×842, 11=A5 420×595; unknown code → Letter + warning) − margins (pageMargins in inches).
3. Scaling: if fitToPage: scale = min(availW / Σcolwidths-per-page-set… ) — concretely: compute total printed width W_tot and height H_tot; scale = min(fitToWidth>0 ? (availW*fitToWidth)/W_tot : 1, fitToHeight>0 ? (availH*fitToHeight)/H_tot : 1, 1). Else scale = @scale/100 (10–400). All content (text sizes, widths, heights, borders) multiplies by scale.
4. Page breaks: columns pack left-to-right into page-width columns-sets, rows top-to-bottom (never split a row/col); manual breaks (`rowBreaks`/`colBreaks`) force cuts. Page order: down-then-over (default; @pageOrder overOver → over-then-down). Print titles: repeated rows render at top of every page (they consume height); repeated cols at left.
5. Cell rendering: background fill rect → grid lines (if printOptions@gridLines: hairline 0.25pt gray RGB 0.5 for every cell edge in range, drawn under borders) → borders (edge style map: hair 0.25pt, thin 0.5, medium 1.0, thick 1.5, dotted/dashed as PDF dash patterns [1 1]/[3 1]) → content. Text: number formats via T6.2 (numbers right-aligned by default, text left, booleans/errors centered — unless alignment overrides); wrap text = line-break within cell width (reuse linebreak); overflow WITHOUT wrap: text spills over following EMPTY cells until blocked (clip at the block; Excel behavior), clipped by the printed range edge; vertical align default bottom. Merged cells: draw as one box (top-left cell's value/style; constituent grid/border edges suppressed inside).
6. Headers/footers: parse `&L&C&R` sections; codes `&P` page, `&N` total pages, `&D` date (from docProps, deterministic), `&T` time, `&A` sheet name, `&F` file name (use the input file name passed via ConvertOptions — add `pub source_name: Option<String>` to ConvertOptions), `&"font,style"`/`&size` → warning `header_font_ignored`, render in default font 10pt. Positioned in the margin bands (header at pageMargins@header from top).
7. Multi-sheet: visible sheets in workbook order; CLI `--sheets "Sheet1,Summary"` filters by exact name (already specced in T3.8 — wire it through ConvertOptions `pub sheet_filter: Option<Vec<String>>`).

## T6.5 Profile for XLSX — extend `src/profile.rs`

Reasons per envelope (drawings/charts parts present → Chart/AnchoredDrawing; CF intersecting print range → ConditionalFormatting; uncached formulas; unparseable numFmt; pivots). Stats: sheets, printed cells, pages estimate (from T6.4 math — cheap, no rendering). Remove `format_not_implemented` for xlsx.

## T6.6 Corpus

Author the xlsx corpus cases (T4.2 list) + `overflow-spill`, `hidden-rows-cols`, `multi-sheet-order`, `header-footer-codes`, `manual-breaks`, `down-then-over`. Excel ground truths via H3. Calibrate: expected drift axes are General-format digits and fit-to-page rounding — log adjustments in LAYOUT_CALIBRATION.md.

## Local verification

1. `cargo test -p office-core` — numfmt table ≥ 60 rows green; A1/range parser; width formula (8.43 chars → 48pt exactly).
2. `cargo run --bin officecore -- convert ../mock-files/<some>.xlsx -o /tmp/sheet.pdf` — grid + numbers look right; `--sheets` filter works; `profile` on a charted workbook returns `chart`.
3. `cargo run -p fidelity -- run --format xlsx` after H3; core tier 100%.
4. wasm target check still passes.
