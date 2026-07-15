# Technical findings and solution path

## Design direction from LibreOffice

LibreOffice was used only as a behavioral and architectural reference. No
LibreOffice code or test fixture was copied.

The useful separation is:

```text
OOXML import
  -> normalized document properties
  -> logical layout in twips/points
  -> renderer-aware painting
```

DOCX tokens should disappear after import. Layout should consume normalized
list suffixes, exclusion rectangles, rows, cells, and border edges.

LibreOffice's source is MPL/LGPL/GPL licensed. Behaviors, invariants, and the
OOXML rules can inform an independent Rust implementation; functions should not
be translated line-for-line or retain LibreOffice control structure/naming.

Official repository: <https://github.com/LibreOffice/core>

## Numbering labels

The current remediation fixes the immediate defect by retaining the parsed
`LevelSuffix` and measuring the space suffix. The complete model should also
retain an explicit numbering-level tab stop when present.

Independent target behavior:

```text
effective_left = resolve paragraph/style/numbering indent cascade
first_origin = effective_left + first_line_offset
continuation_origin = effective_left

label_cursor = place measured label in the hanging area

nothing -> text starts at label_cursor
space   -> text starts after one measured numbering-font space
tab     -> text advances monotonically to the merged list/paragraph tab ruler

wrapped lines start at continuation_origin
```

Do not convert a tab suffix to an arbitrary fixed gap. A future follow-up should
parse and normalize numbering-level `w:pPr/w:tabs/w:tab`, merge it with the
paragraph ruler, and test labels that overrun the preferred stop.

LibreOffice reference files:

- <https://github.com/LibreOffice/core/blob/master/sw/source/writerfilter/dmapper/NumberingManager.cxx>
- <https://github.com/LibreOffice/core/blob/master/sw/source/core/txtnode/ndtxt.cxx>
- <https://github.com/LibreOffice/core/blob/master/sw/source/core/text/txttab.cxx>

## Top-and-bottom anchored wrapping

Treat `wrapTopAndBottom` as a full-width geometric exclusion, not paragraph
spacing and not XML source-order behavior.

```text
exclusion = object bounds plus authored distances
exclusion.left/right = flow-area edges

for each candidate line strip:
  keep a line wholly above the exclusion
  if the line intersects, jump to exclusion.bottom and retry the same text
  keep paragraph occupancy through exclusion.bottom for anchor-only paragraphs
```

The current uncommitted tests cover equal behavior for anchor-before-text and
anchor-after-text, positive vertical offsets, and anchor-only paragraphs.

LibreOffice reference files:

- <https://github.com/LibreOffice/core/blob/master/sw/source/writerfilter/dmapper/GraphicImport.cxx>
- <https://github.com/LibreOffice/core/blob/master/sw/source/core/text/txtfly.cxx>
- <https://github.com/LibreOffice/core/blob/master/sw/source/core/text/itrform2.cxx>

## Table layout and painting

The remaining longdoc evidence points to a table/page-local origin or painting
transition. The next implementation should separate logical row layout from a
canonical border-painting pass.

Logical layout:

```text
content_min = max(cell content + effective borders + padding)

exact row   -> specified height
at-least    -> max(specified height, content_min)
auto        -> content_min

row[n].top = row[n - 1].bottom
```

Canonical painting:

1. Collect visible cell edges into horizontal and vertical maps keyed by
   logical coordinates.
2. Split overlaps into segments.
3. Resolve each shared boundary once, including row spans, covered cells, RTL,
   outer edges, and border conflicts.
4. Paint a shared boundary once rather than from both adjacent cells.
5. Apply device-grid snapping only where the renderer knows the output
   transform.

For vector PDF output, do not hard-code the fidelity harness's 144 dpi grid into
document layout. That would overfit one rasterizer configuration.

LibreOffice reference files:

- <https://github.com/LibreOffice/core/blob/master/sw/source/writerfilter/dmapper/DomainMapperTableHandler.cxx>
- <https://github.com/LibreOffice/core/blob/master/sw/source/core/layout/tabfrm.cxx>
- <https://github.com/LibreOffice/core/blob/master/sw/source/core/layout/paintfrm.cxx>

## Why a global origin shift is rejected

Moving page 41 one pixel right/down raises its SSIM from 0.812452 to 0.957126,
but applying that shift globally reduces passing cases from 16/18 to 11/18.

| Case | Current | Hypothetical +1 px x/y |
| --- | ---: | ---: |
| `complex` | 0.937401 | 0.833218 |
| `gdocs-anchors-toc` | 0.689894 | 0.697140 |
| `gdocs-columns` | 0.928075 | 0.868327 |
| `gdocs-tables-longdoc` | 0.812452 | 0.835182 |
| `long-doc-pagination` | 0.955218 | 0.790665 |
| `page-fields` | 0.955054 | 0.790017 |
| `plain-paragraphs` | 0.992634 | 0.963505 |

Applying the shift only to the three 90 pt-margin fixtures also fails:
`complex` and `gdocs-columns` fall below threshold, while longdoc remains red.
The displacement must be isolated to its table/page-local cause.

## Longdoc deficit composition

On the minimum dense table page:

- blank/fill-only windows contribute about 0.1% of the SSIM deficit;
- border-containing windows contribute about 68.2%;
- border-free glyph windows contribute about 31.7%.

A border-only shift tops out near 0.893. Moving the related text and borders
together reaches 0.957 with the same Carlito glyph outlines. This is evidence
for a shared local coordinate defect, not permission to alter the score or
replace the font.

## Font findings

### Ubuntu

The anchors DOCX declares Ubuntu for most content and Ubuntu Mono once. The
wasm bundle lacks both, so exact family matching cannot occur. Adding the
official legacy fonts should be tested after the owner approves the UFL policy
change. The fixture should then assert exact-family resolution and the absence
of `font_fallback` for those families.

### Calibri

Carlito is the correct legal bundled substitute and is metric-compatible with
Calibri. It is not outline-identical. Preserve substitution warnings and runtime
font registration. Do not extract or redistribute Calibri without extended
rights.

## Recommended continuation sequence

1. Review the stopped uncommitted patch; fix review findings only.
2. Run full Rust/fmt/Clippy/wasm/Node verification and commit the remediation.
3. If approved, human-vendor Ubuntu/Ubuntu Mono with licence notices; rebuild
   and rerun anchors.
4. Add a diagnostic geometry test for the longdoc dense table pages that records
   logical table origin, row tops, cell text origins, and canonical borders.
5. Implement the normalized shared-edge painter and any proven table-local
   origin fix. Reject document-wide offsets.
6. Rerun the unchanged 18-case corpus. Both new cases must reach at least 0.90.
7. Complete H3 visual review, then H4 release and consumer pin update.
