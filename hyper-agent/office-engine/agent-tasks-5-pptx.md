# Agent tasks 5 — PPTX engine

Prereq: tasks 2 & 4 (fonts/profile + harness). Reuses `oxml/dml.rs`, `oxml/theme.rs`, `text/*`, `fonts/*`, `emit/pdf.rs` unchanged. New: `src/oxml/pml.rs` (task 1 stubbed it), `src/pptx/`.

**PPTX v1 envelope.** Supported: slide size/orientation; master→layout→slide placeholder inheritance; text bodies with paragraph levels, bullets, alignment, autofit via STORED values; shapes with preset geometry (full evaluator — all presets), solid/linear-gradient/picture(stretch) fills, solid outlines, rotation/flip; pictures (PNG/JPEG); tables; slide background (solid/gradient/picture); hyperlinks. Ignored-with-warning (cosmetic-lossy, documented): effects (`a:effectLst` — shadows/reflections/glow) dropped with `effects_dropped`; transitions/animations/notes (never print). Unsupported (fail-closed): charts, SmartArt (`p:graphicFrame` with chart/diagram uri), OLE/media, WordArt (`a:prstTxWarp` other than `textNoShape`), 3D (`a:sp3d`/`a:scene3d`), custom geometry `a:custGeom` **with** formulas we can't evaluate (plain path custGeom IS supported — same path renderer), `normAutofit` missing when text overflows (NeedsAutofitRecompute), pattern/texture fills, radial/path gradients (v1 linear only).

## T5.1 PML model — `src/oxml/pml.rs`

Parse: `presentation.xml` (slide size `p:sldSz` cx/cy EMU + slide id list → rel ids), `slideMaster<N>.xml` (+ its `p:txStyles`: titleStyle/bodyStyle/otherStyle with per-level `a:lvl1pPr..lvl9pPr`), `slideLayout<N>.xml`, `slide<N>.xml`. Shape tree (`p:spTree`) elements: `p:sp` (shape: `p:nvSpPr` placeholder info `p:ph @type @idx`, `p:spPr` geometry/fill/line/xfrm, `p:txBody`), `p:pic`, `p:graphicFrame` (table via `a:tbl`, else inspect graphic uri → chart/diagram/ole → the mapped Unsupported), `p:grpSp` (group: recurse with composed transform), `p:cxnSp` (connector: treat as shape with line only).

`a:xfrm`: `a:off` (EMU x,y), `a:ext` (cx,cy), `@rot` (60000ths of a degree, clockwise), `@flipH/@flipV`. Group transforms: `chOff`/`chExt` define child space — compose scale+offset when recursing.

## T5.2 Inheritance — `src/pptx/inherit.rs`

For a slide shape with `p:ph type=T idx=I`:
1. Geometry/xfrm: use the shape's own if present, else the matching placeholder (same idx first, then same type) on the LAYOUT, else on the MASTER.
2. Text properties resolve per paragraph level L (0-based from `a:pPr @lvl`): merge chain = master `txStyles` bucket (title→titleStyle; body/subTitle→bodyStyle; else otherStyle) level L → layout placeholder `a:lstStyle` level L → slide shape `lstStyle`/direct `a:pPr`/`a:rPr`. Same `merge(base)` pattern as WML styles.
3. Background: slide `p:bg` else layout's else master's; `p:bgRef` resolves into the theme fill styles list (`a:fmtScheme/fillStyleLst`, idx 1-based; ≥1000 → bgFillStyleLst[idx−1000]).
4. Colors: `a:schemeClr` resolves through the slide→layout→master `p:clrMapOvr`/`p:clrMap` indirection into theme slots, THEN transforms (T1.7 `resolve_color`).

## T5.3 Preset geometry evaluator — `src/pptx/geometry.rs`

Implement the DrawingML guide-formula language once; it renders ALL preset shapes:

1. Vendor the preset definitions: create `office-core/src/pptx/preset_geometries.rs` containing the preset shape definitions transcribed from ECMA-376 Part 1 (5th ed.) `presetShapeDefinition.xml` as a `const &str` XML blob (`include_str!("presetShapeDefinitions.xml")` with the file committed under `src/pptx/`). Source it from the published ECMA-376 annex archive; commit it verbatim with a header comment citing ECMA-376. Parse it lazily once (OnceLock) into `HashMap<String, PresetDef { av_defaults, gd_list, path_list }>`.
2. Formula ops (`a:gd @fmla`, space-separated tokens; all arithmetic on f64): `val a`; `*/ a b c`=a·b/c; `+- a b c`=a+b−c; `+/ a b c`=(a+b)/c; `?: a b c`=a>0?b:c; `abs`; `min`/`max`; `mod a b c`=√(a²+b²+c²); `pin a b c`=clamp(b,a,c); `sqrt`; `at2 a b`=atan2(b,a); `cat2 a b c`=a·cos(atan2(c,b)); `sat2 a b c`=a·sin(atan2(c,b)); `sin a b`=a·sin(b); `cos`/`tan` likewise. **Angles are in 60000ths of a degree** wherever they appear (convert inside sin/cos/tan/at2 results: at2 returns 60000ths).
3. Built-in variables per evaluation: `w`,`h` (shape ext in EMU), `ss`=min(w,h), `ls`=max(w,h), `wd2..wd10`=w/n, `hd2..hd10`=h/n, `ssd2..ssd32`, `t`=0,`l`=0,`b`=h,`r`=w, `cd2`=10800000, `cd4`=5400000, `cd8`=2700000, `3cd4`, `3cd8`, `5cd8`, `7cd8` (angle constants in 60000ths). Adjust values: preset `avLst` defaults overridden by the shape's `a:avLst`.
4. Paths (`a:path @w @h @fill @stroke`): commands moveTo/lnTo/quadBezTo/cubicBezTo/arcTo/close; coordinates scale by (shape_ext / path @w,@h) when @w/@h present. `arcTo (wR hR stAng swAng)`: ellipse arc from current point — convert to ≤4 cubic Béziers (standard midpoint algorithm; angles in 60000ths). Multiple `a:path` elements layer (some fill-only, some stroke-only via @fill="none"/@stroke="0").
5. Output: `Vec<PathSegment>` in pt, pre-transformed by xfrm (offset, scale, rotation about shape center, flips). Emitter extension (small, in `emit/pdf.rs`): a `Path { segments, fill, stroke }` Item variant — m/l/c/re/h + f/S/B operators, gradient fills via axial shading pattern (`/Shading` dict, `Coords` from gradient angle mapped across the shape bbox, stops from `a:gsLst` with resolved colors).
6. `a:custGeom`: same path renderer; its own `a:gdLst`/`a:avLst` feed the same evaluator.

Tests: evaluate `rect`, `roundRect` (adj), `ellipse`, `triangle`, `rightArrow`, `star5`, `pie` against hand-computed control points (write expected values in test comments with the math shown); arcTo→Bézier accuracy (endpoint within 0.1pt of exact ellipse point).

## T5.4 Text bodies — `src/pptx/textbody.rs`

`a:bodyPr`: insets (default 91440/45720 EMU l-r/t-b), `anchor` (t/ctr/b vertical anchoring in the shape box), `@wrap` (square|none), `a:normAutofit @fontScale @lnSpcReduction` (thousandths of a percent) — **apply the stored values** (scale font sizes; reduce line spacing) and lay out; if the result STILL overflows the box by > 2%, or `a:spAutoFit`/missing autofit with overflow → profile reason NeedsAutofitRecompute (fail-closed rather than recompute Word's iteration v1). Paragraph properties: `algn`, `marL`/`indent` (EMU), `buChar`/`buAutoNum`/`buNone` bullets (buChar font via `buFont` → T2.4 mapping; buAutoNum formats arabicPeriod, alphaLcPeriod, romanLcPeriod etc. → reuse T1.6 formatter), `lnSpc`/`spcBef`/`spcAft` as `spcPct` (percent of natural, val in thousandths) or `spcPts` (hundredths of a point). Runs: `a:rPr` (sz in hundredths of a point, b/i/u, `a:latin/@typeface` + `+mj-lt`/`+mn-lt` theme fonts, solidFill color, hyperlink `a:hlinkClick r:id`). Reuse the paragraph line-builder from `layout/paragraph.rs` — refactor it to take a generic "props" view if needed rather than duplicating (acceptable shared trait: `LineSource`).

## T5.5 Pictures, tables, background — `src/pptx/render.rs`

- `p:pic`: `a:blipFill/a:blip r:embed` → image part; `a:stretch` = fit to xfrm ext (only mode v1; `a:tile` → Unsupported); srcRect → ImageCrop reason (shared with docx).
- `a:tbl` inside graphicFrame: `a:tblGrid/gridCol @w` EMU widths (fixed layout ALWAYS — PowerPoint tables don't autofit); rows `a:tr @h` minimums; cells `a:tc` with `gridSpan`/`rowSpan`/`hMerge`/`vMerge`, `a:tcPr` fills/borders (lnL/lnR/lnT/lnB), text via T5.4. Table styles (`a:tblPr @firstRow @bandRow` + `tableStyles.xml` GUID ref): v1 resolve ONLY the explicit tcPr formatting + a warning `table_style_partial` when a style GUID is referenced (banding colors will differ → the harness decides whether cases pass; if they don't, implement first-row/banding fills from the referenced style — structure the code so this is a contained follow-up).
- Slide render order: background → spTree in document order (z-order); groups recurse.
- `convert()` for Pptx: one PDF page per slide, page size = sldSz (EMU→pt, landscape default 13.333×7.5in for 16:9).

## T5.6 Profile for PPTX — extend `src/profile.rs`

Walk all slides+layouts+masters: reasons from the envelope table (chart/diagram/ole via graphicFrame uri: `.../chart`→Chart, `.../diagram`→SmartArt, `.../ole`→OleObject; media `p:video`/`p:audio`→OleObject detail "media"; prstTxWarp→WordArt; sp3d/scene3d→detail "3d"; tile fills, radial gradients, custGeom-with-unevaluable-formula→UnknownElement). Stats: slides, shapes, images, fontsUsed/Missing. `format_not_implemented` is removed for pptx by this task.

## T5.7 Corpus + calibration

Author the pptx corpus cases from T4.2, plus: `autofit-stored-fontscale`, `group-transform`, `rotation-flip`, `theme-color-transforms`. Human H3 exports ground truths (PowerPoint for Mac, same export option); run the harness; calibrate (line spacing `spcPct` interpretation and gradient angle mapping are the two expected drift axes — log in LAYOUT_CALIBRATION.md).

## Local verification

1. `cargo test -p office-core` (geometry evaluator tests, inheritance tests using a 2-master fixture, autofit stored-value test).
2. `cargo run --bin officecore -- convert ../mock-files/<some>.pptx -o /tmp/deck.pdf` — visually sane; page size 960×540pt for 16:9.
3. `cargo run -p fidelity -- run --format pptx` after ground truths exist; core tier 100%.
4. wasm target check still passes.
