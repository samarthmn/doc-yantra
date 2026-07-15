# Blockers and owner actions

> **Resolution status (2026-07-15, owner + planner):** every decision below is now resolved — see the "Resolutions" section at the end of this file. Continuation work is specified in [`../agent-tasks-6-metric-overlay.md`](../agent-tasks-6-metric-overlay.md) and [`../agent-tasks-7-table-geometry.md`](../agent-tasks-7-table-geometry.md).

## 1. Frozen fidelity gates

The release gate requires 0.90 SSIM and exact page counts. Page counts pass;
two SSIM gates do not.

### `gdocs-anchors-toc`

Current minimum: 0.6898937923 on page 3.

The page's float bounds, table widths, cell boundaries, and exclusion geometry
match Word. The source uses Ubuntu and Ubuntu Mono, but the wasm bundle contains
neither. The resolver therefore renders Ubuntu with Liberation Sans and Ubuntu
Mono with Liberation Mono. Page 3 contains opposite line-wrap differences in
different cells and a 9-line versus 10-line paragraph difference, which is
consistent with font metrics rather than a second anchor defect.

Current per-page measurements:

| Page | SSIM |
| ---: | ---: |
| 1 | 0.8973216 |
| 2 | 0.8114248 |
| 3 | 0.6898938 |
| 4 | 0.9192800 |
| 5 | 0.9221585 |
| 6 | 0.9066385 |
| 7 | 0.7909402 |
| 8 | 0.9889372 |
| 9 | 0.8495538 |
| 10 | 0.9101925 |

### `gdocs-tables-longdoc`

Current minimum: 0.8124519676 with exact 44-page parity.

The final numbering fix removes visible bullet/text overlap, but the case
minimum remains unchanged on a dense table page. Quantitative analysis shows
that this is not primarily a Calibri-versus-Carlito outline ceiling. Translating
page 41's rendered content by one raster pixel right/down raises its SSIM to
0.9571257530 without changing any glyph outline. The displacement is local to
specific table/page states; a global origin shift breaks passing fixtures.

The remaining code problem is to isolate the logical table/text origin or
painting transition that creates that local displacement. A global 0.5 pt
offset is not a valid fix.

## 2. Owner decision: Ubuntu font policy

The reference PDF embeds Ubuntu Regular/Bold/Italic/Bold Italic version 0.83
and Ubuntu Mono Regular version 0.80. Canonical publishes the matching legacy
family under the Ubuntu Font Licence 1.0.

The repository currently says bundled fonts are human-vendored, lists exactly
20 SIL-OFL faces, and forbids agents from fetching font binaries. Adding Ubuntu
therefore needs an explicit owner action:

1. Approve bundled UFL-1.0 fonts in addition to SIL-OFL fonts.
2. Have a human download the official Canonical Ubuntu Font Family 0.83 archive.
3. Vendor these eight files with Canonical's copyright and licence notices:

   ```text
   Ubuntu-R.ttf
   Ubuntu-B.ttf
   Ubuntu-RI.ttf
   Ubuntu-BI.ttf
   UbuntuMono-R.ttf
   UbuntuMono-B.ttf
   UbuntuMono-RI.ttf
   UbuntuMono-BI.ttf
   ```

4. Update `office-core/fonts/README.md` and the hard-coded 20-face test.
5. Rebuild both wasm packages and record the size change.

Expected cost: about 2.2 MB of source font files and roughly 20% growth in each
current 11 MB wasm binary. Exact size must be measured after human vendoring.

Official references:

- <https://design.ubuntu.com/font>
- <https://canonical.com/legal/font-licence>

## 3. Owner decision: Microsoft font policy

The longdoc Word PDF embeds Calibri 6.26. The package lawfully uses Carlito,
an OFL font designed to be metric-compatible with Calibri. The geometry analysis
shows that 0.90 remains achievable with Carlito, so a Calibri licence is not a
prerequisite for the current gate.

Exact Calibri pixel parity is a different product requirement. Microsoft does
not permit application redistribution under ordinary Windows/document-embedding
rights. If exact proprietary typography is required, the owner must obtain
extended redistribution/server rights and define how licensed font bytes reach
native and wasm consumers. Do not extract Calibri from the committed PDF or a
local Office installation for package distribution.

Recommended product policy:

- keep Carlito as the bundled Calibri-compatible fallback;
- keep `register_font()` for customer-authorized runtime fonts;
- surface substitution warnings in `DocProfile`;
- pursue the 0.90 gate through logical layout and painting fixes, not by
  embedding unlicensed Calibri.

Official references:

- <https://github.com/googlefonts/carlito>
- <https://learn.microsoft.com/en-us/typography/fonts/font-faq>
- <https://learn.microsoft.com/en-us/typography/font-list/calibri>

## 4. Owner review and release tasks

After both frozen cases pass:

1. Run human task H3 and inspect all three `gdocs-*` heatmaps.
2. Accept or reject the wasm size/licence change if Ubuntu is bundled.
3. Run H4 from a clean `main`: release, tag, publish, then update the DocKosha
   package pin in its separate repository.

No release action is appropriate while either 0.90 gate is red.

## 5. Product expectation for arbitrary DOCX files

LibreOffice-level breadth is a multi-release corpus program, not a one-time
proof that no document can fail. Keep the current fail-closed contract for
unimplemented visible constructs. To drive failures down:

- collect public or synthetic representatives for each new telemetry reason;
- normalize each OOXML construct into producer-independent layout data;
- add profile/convert parity and geometry tests before widening support;
- add a frozen Word ground truth only after provenance and content review;
- never replace an explicit unsupported error with silent content loss.

---

## Resolutions (2026-07-15, owner + planner — planning thread cmrki1y8p0ixg06adbyatevrz)

1. **Ubuntu font policy (§2) — RESOLVED, superseded by metric-compatibility overlays.** The owner chose NOT to vendor the TTF binaries. Instead the engine gains overlay entries carrying Ubuntu 0.83 / Ubuntu Mono 0.80 **advance widths and vertical metrics as compiled-in data** (~15 KB), applied over bundled substitute outlines — see `../agent-tasks-6-metric-overlay.md` (design rationale: with `liga` always off and `w:kern` absent from these exports, Word's wrap points are a pure function of advances). Human task H6 covers the one-time metrics extraction from Canonical's archives; **TTF vendoring per §2 steps 1–5 remains pre-approved as the contingency** if any anchors-toc page stays below 0.90 after the overlay (agent-tasks-6 T6.7).
2. **Microsoft font policy (§3) — AFFIRMED as written.** Carlito stays; no Calibri extraction; the longdoc gate is pursued as geometry, not typography. The overlay mechanism from resolution 1 additionally becomes the future lever for proprietary-family *wrap parity* (metrics-as-data), should the product ever need it.
3. **Longdoc displacement (§1) — continuation specified.** `../agent-tasks-7-table-geometry.md` adds a vector geometry probe (content-stream diff against the Word ground truth), a synthetic single-variable bisect matrix (human task H7), code-grounded hypotheses (fragment-edge half-stroke reservation, x-axis outer-border reservation, `tblInd` reference edge), the confirmed-convention fixes, and the vertical-seam completion of the shared-edge border painter. Global offsets remain forbidden, per this file's own measurements.
4. **Sequencing:** the stopped 21-file worktree patch is reviewed, verified, and committed **before** phase 6/7 work starts (step 0 in both task files).
