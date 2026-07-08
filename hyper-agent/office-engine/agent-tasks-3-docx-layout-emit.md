# Agent tasks 3 — DOCX layout engine, PDF emission, CLI

Prereq: tasks 1–2. This is the moat: `src/layout/` computes page geometry from the WML model; `src/emit/pdf.rs` writes the PDF. Calibration against Word ground truth happens in task 4 — build to the formulas here, expect the harness to drive refinements.

**DOCX v1 envelope (single source of truth).** Supported: paragraphs/runs with the full RunProps set from T1.4; style cascade; bullet/numbered lists (formats from T1.6); tables incl. nested, gridSpan, vMerge, per-cell borders/shading/margins, header-row repeat; inline images (PNG/JPEG); hyperlinks (external + internal anchors); headers/footers (default/first/even); multiple sections when every section is single-column (size/margins/orientation changes apply at section break); explicit line/page breaks; PAGE/NUMPAGES/HYPERLINK fields; sdt unwrapping; tracked changes rendered as final view. Everything in the UnsupportedCode list stays fail-closed. Ignored-with-warning (documented lossy): paragraph `shading` patterns other than solid fill (render solid), `w:highlight` rendered as solid rect behind text, softHyphen dropped.

## T3.1 Layout data model — `src/layout/mod.rs`

```rust
pub struct LaidOutDocument { pub pages: Vec<Page> }
pub struct Page { pub width_pt: f64, pub height_pt: f64, pub items: Vec<Positioned> }
pub struct Positioned { pub x_pt: f64, pub y_pt: f64, pub item: Item }   // y = distance from page TOP
pub enum Item {
    Glyphs { face_id: FaceId, size_pt: f64, color: Rgba, glyphs: Vec<PlacedGlyph>, // x-advances resolved
             underline: Option<LineDecor>, strike: Option<LineDecor>, rise_pt: f64 /*super/sub*/ },
    Rect { w_pt: f64, h_pt: f64, fill: Option<Rgba>, stroke: Option<Stroke> },     // borders, shading, highlights
    Line { x2_pt: f64, y2_pt: f64, stroke: Stroke },
    Image { w_pt: f64, h_pt: f64, image_id: ImageId },
    LinkRect { w_pt: f64, h_pt: f64, target: LinkTarget /*Url(String)|Page(u32)*/ },
}
```
Layout emits ONLY these primitives; the emitter has zero WML knowledge. Coordinates are top-left-origin here; the emitter flips to PDF's bottom-left.

## T3.2 Line building — `src/layout/paragraph.rs`

Algorithm (per paragraph, given effective props from StyleResolver and available content width `W` = page width − margins − indents):

1. Resolve indents: `left`, `right`, `first_line` (positive) or `hanging` (negative first-line, and the hang defines where list labels sit — see T3.5). Line 1 width = `W − left − right − first_line_offset`; other lines `W − left − right`.
2. Shape each run (T2.5) producing glyph clusters; concatenate into an item stream with break opportunities from `linebreak.rs`. `Tab` items resolve against tab stops: explicit stops from `ParaProps.tabs` (pos, alignment left/center/right/decimal, leader none/dot/hyphen/underscore), else default grid every `settings.default_tab_stop_pt` from the left indent. Tab advance = distance to next stop ≥ current x (center/right stops position the FOLLOWING text span accordingly — implement by measuring the span to the next tab/break); leader fills the gap with the leader char repeated at its advance.
3. Greedy line breaking: pack items until the next break opportunity would overflow; trailing whitespace at line end is measured as zero. A word longer than the line breaks at glyph granularity (Word does emergency-break too). Mandatory breaks (`Break(Line)`, newline chars) end lines.
4. **Line height**: `natural = max over runs on the line of (usWinAscent+usWinDescent)/upem * size_pt` (fallback face metrics per T2.5); apply `LineSpacing`: `Auto(m)` → `m * natural`; `AtLeast(v)` → `max(v, natural)`; `Exact(v)` → `v` (clip nothing; baseline = v − descent). Baseline position within line = ascent portion. Add paragraph `spacing.before/after` between paragraphs; when `contextual_spacing` and adjacent paragraph shares the style, suppress the between-space; collapse rule: space between paras = `max(after_prev, before_next)` — Word's default "don't add space between paragraphs of the same style" applies only via contextualSpacing.
5. **Justify** (`Jc::Both`): extra = line width deficit distributed equally across expandable space glyphs (U+0020 clusters) on the line; last line of the paragraph and lines ending in a mandatory break stay start-aligned. Center/right: shift line origin.
6. Super/sub (`vert_align`): size × 0.65, rise ±0.35 × size (calibrate in task 4).
7. Underline/strike: decor lines using face underline metrics (post table) scaled; emit with the Glyphs item.
8. Paragraph borders/shading: after lines are placed, emit `Rect` behind the paragraph block (shading) and border `Line`s (between-borders between consecutive bordered paragraphs of the same style).

## T3.3 Tables — `src/layout/table.rs`

Column width resolution (deterministic; calibrated later):
1. Grid base widths from `tblGrid/gridCol` (twips→pt). Cell preferred widths `tcW` override per column where type=dxa; pct resolves against available width; `auto`/absent = grid value.
2. `tblLayout w:type="fixed"` (or tblW type=dxa with all tcW dxa): use resolved widths as-is, scaled proportionally iff total > available width.
3. Autofit (default): compute per-column `min` (widest unbreakable cluster over the column's cells) and `max` (widest cell content laid on one line, capped at available). If Σmax ≤ available → widths = max, then distribute leftover proportionally to max iff tblW=pct/auto says fill. Else widths = min + (available − Σmin) × (max−min)/Σ(max−min), floored at min. gridSpan cells contribute their min/max spread equally across spanned columns.
4. Cell layout: recurse `Vec<BlockContent>` into the cell box (width − cell margins; defaults 108 twips left/right, 0 top/bottom, overridable by tblCellMar/tcMar). Row height = max cell content height, raised to `RowHeight::AtLeast`, forced by `Exact`. vMerge Continue cells render nothing; the Restart cell's box spans the merged rows' total height. Vertical alignment positions cell content within the final row height.
5. Borders: effective border per edge = cell `tcBorders` if set, else row/table `insideH/insideV` for interior edges, else table `tblBorders` for outer edges. Conflict between adjacent cells: wider `sz` wins; tie → the border whose color is darker (sum of RGB lower); `nil`/`none` beats nothing but loses to any visible border. Draw each shared edge exactly once. Border widths are eighths of a point (T1.3); `single` line style only v1 — other `w:val` styles render as single with a `border_style_simplified` warning.
6. Row split across pages: a row taller than remaining page space moves to the next page whole (`cantSplit` semantics as default v1 — simpler and safe); if it exceeds a FULL page, split its cells' content at line granularity. Header rows (`tblHeader`) repeat at the top of each continuation page.

## T3.4 Pagination & sections — `src/layout/flow.rs`, `src/layout/page.rs`

1. Content area per section: page − margins; header box top at `margins.header`, footer box bottom at `margins.footer`; if measured header content is taller than (margin.top − margin.header), push content top down (Word grows the top margin).
2. Fill lines/blocks into the content area; `keep_lines` moves the whole paragraph if it would split; `keep_next` binds a paragraph to the first line of the next; widow/orphan (default ON per Word) forbids 1-line stragglers — move 2 lines when needed; `page_break_before` and explicit `Break(Page)` force page starts.
3. Section breaks: `sect_break` on a paragraph ends the section; types nextPage/oddPage/evenPage start pages (odd/even insert a blank page when parity requires — blank page still shows that section's header/footer); `continuous` with identical page geometry continues flow, otherwise treat as nextPage with warning `continuous_section_as_page`.
4. Headers/footers: per section select by kind — first page (`title_pg`), even (`even_and_odd_headers`), else default; sections inherit the previous section's refs when they define none. Lay them out with the same paragraph/table engine (images allowed; fields allowed).
5. **PAGE/NUMPAGES fields (two-pass):** pass 1 lays out with placeholder digits ("88"/"888" by expected magnitude) to fix pagination; pass 2 re-runs layout with real values (page numbers respect `page_num_start`). Two full passes, always, whenever any PAGE/NUMPAGES field exists — deterministic and simple. If pass-2 pagination differs from pass-1 (rare oscillation), accept pass-2 and emit warning `page_field_reflow`.
6. Field instructions: parse ` PAGE `, ` NUMPAGES `, ` HYPERLINK "url" ` (render cached runs as link); any other instruction word → render the cached result runs verbatim with warning `field_cached_result` (TOC therefore renders as-is with stale numbers — acceptable, Word-cached); no cached result → Unsupported{FieldUnsupported}.

## T3.5 Lists — `src/layout/numbering_layout.rs`

Counters keyed by `(num_id, ilvl)`: start at `lvl.start`, increment per numbered paragraph; deeper-level counters reset when a shallower level increments (standard Word restart behavior; `lvlOverride startOverride` honored). Label text = `lvlText` with `%n` placeholders replaced by formatted counters (T1.6). Label is placed in the hanging indent: label at `left − hanging`, text at `left`; `suff` tab → advance to `left` (or next tab stop if label overruns), space → single space, nothing → abut. Label run props come from the lvl `rPr` merged over the paragraph's effective run props (bullet chars map via T2.4).

## T3.6 Inline images — `src/layout/objects.rs`

`wp:inline` drawings: size from `wp:extent` (EMU→pt). Decode via `image` crate to get intrinsic px only when extent is missing (rare) at 96dpi. Images participate in line layout as a big glyph (baseline-aligned, line height grows to fit). JPEG bytes pass through untouched (emitter uses DCTDecode); PNG decodes to RGB8/RGBA8 (alpha → SMask). Unsupported image containers (GIF/EMF/WMF/TIFF) → Unsupported{UnknownElement, "image/<kind>"} — detected at profile time from the part's content type.

## T3.7 PDF emission — `src/emit/pdf.rs`

Use `pdf-writer` 0.15 (same crate/version pdf-core uses — copy idioms from `pdf-core/src/generate.rs` where helpful, but this is a fresh module; do NOT import pdf-core).

- One `Type0` font per used (face, subset) with `CIDFontType2` descendant, Identity-H, `W` array from subset widths, `CIDToGIDMap /Identity`, FontFile2 = subset bytes (T2.6), ToUnicode CMap (standard 4-byte bfchar template). Font resources deduped document-wide.
- Content streams per page: flip y (`pdf_y = page_h − y`); text via BT/Tf/Td/TJ with rise for super/sub; kerning is already baked into glyph x-positions — emit runs of glyphs with identical (face,size,color) as single TJ arrays using position deltas; rects `re f`/`re S`; lines with `w`, RG/rg. Link annotations from LinkRect (URI action or GoTo page dest). Outline (bookmarks) from paragraphs whose effective style has `outline_lvl` 0–2 (Heading 1–3).
- Compress every stream with `miniz_oxide` FlateDecode.
- **Determinism:** PDF trailer /ID = first 16 bytes of `sha2::Sha256(input bytes ‖ engine version)` for both array entries; CreationDate/ModDate from `docProps/core.xml` created/modified when parseable, else the fixed literal `D:20000101000000Z`; no other environmental data. Object numbering purely by emission order. Add test: convert same fixture twice → byte-identical output.
- `ConvertOutput { pdf, page_count, warnings }` — page_count from the layout, never re-parsed.

## T3.8 CLI — `src/bin/officecore.rs`

Mirror `pdfcore`'s clap style:
```
officecore convert <in.docx> [-o out.pdf] [--font-path DIR]...   # exit 3 on Unsupported, printing the JSON profile to stderr
officecore profile <in.docx>                                     # prints DocProfile JSON to stdout
```
Extension→format via `OfficeFormat::from_extension`; unknown → exit 2.

## T3.9 Tests

`tests/docx_layout_test.rs` (fixtures via testkit + real files in `mock-files/`):
- line breaking: known text at known width → expected break points (compute expected advances from Carlito metrics in-test);
- justify distributes exactly the deficit across spaces (assert glyph x positions);
- two-pass PAGE field: 3-page doc renders "Page 1 of 3".."3 of 3" in footer;
- table: gridSpan+vMerge fixture → assert drawn border segment count and positions;
- keep_next / widow control: construct a doc that would strand lines, assert the paragraph moved;
- nested table renders inside cell box;
- determinism test (T3.7);
- every Unsupported fixture from task 1 makes `convert()` return the SAME code `profile()` predicted (contract lock: add a loop test over all fixtures asserting `profile.supported == convert().is_ok()` and reason codes match error codes).

## Local verification

1. `cargo test -p office-core` all green; `cargo clippy -- -D warnings`; `fmt --check`.
2. wasm check: `cargo check --no-default-features --features wasm,bundled-fonts --target wasm32-unknown-unknown`.
3. Manual smoke: `cargo run --bin officecore -- convert ../mock-files/<some>.docx -o /tmp/out.pdf` and open the PDF; text selectable (ToUnicode works: copy-paste text out of the PDF); `profile` prints valid JSON (`| jq .` succeeds).
