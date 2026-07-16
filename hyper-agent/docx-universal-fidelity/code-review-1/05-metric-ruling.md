# Ruling — ink-weighted metric blocker (review 1, §3 amended)

Date: 2026-07-16. Responds to [`04-ink-weighted-testing-blocker.md`](./04-ink-weighted-testing-blocker.md). The blocker's analysis is accepted in full: the weighting formula is implemented correctly, the AutoFit cases prove it detects the geometry it was built for, and the nine typography failures are a **metric-design conflict, not an implementation failure**. The conflicting completion requirements (§3.3 gating + §3.4 frozen thresholds + all-green) are resolved below by amending §3.3 — not by touching a single threshold, golden, or hash.

## The ruling: Option E — split the two questions the score conflates

A raster scalar cannot simultaneously be sensitive to sparse geometry errors and tolerant of lawful glyph substitution — the evidence table proves it (`gdocs-anchors-toc`: geometry-perfect at 10/10 pages with exact Ubuntu embedded, weighted 0.342 because dot-leader and substituted-glyph windows dominate the ink mass). So we stop asking one number to answer two questions:

1. **"Is the geometry right?" → the vector probe becomes a blocking per-case gate.** It compares border segments and text-run *origins* against the ground truth in vector space — **immune to glyph outlines, hinting, and antialiasing by construction**, and (because our substitutes are metric-compatible) immune to lawful substitution too. It is also immune to the sparse-page dilution that motivated ink-weighting: a table at the wrong width is a huge origin delta no matter how blank the page is. Both failure classes that started all this (AutoFit widths, the 0.5pt longdoc displacement) are exactly what it measures — the second one it *already caught once*.
2. **"Does the raster look right, given accepted substitutions?" → the frozen legacy mean-SSIM thresholds keep gating**, unchanged. They were calibrated on this score distribution and encode the accepted-substitution baseline. §3.4 is fully honored — zero threshold edits.
3. **Ink-weighted SSIM stays in the reports as a diagnostic** (both metrics + heatmaps). It is the best *pointer* to where differences live; it is not a gate until/unless per-case weighted baselines are ever deliberately reviewed and frozen (not required for sign-off; Option A remains available later without rework).

### Geometry gate specification (replaces §3.3's gating clause)

- `run` executes the existing probe machinery per case on every scored page (vector parse, no extra rasterization). Per page it computes, separately for **border segments** and **text-run origins**: paired median Δx/Δy, p95 Δ, and the unmatched-element fraction.
- Manifest schema (additive, defaults apply when absent):
  `geometry_median_pt = 0.6`, `geometry_p95_pt = 1.5`, `geometry_max_unmatched_pct = 5.0`.
- A case fails when any page exceeds its geometry bounds, with a note naming the page, axis, and element class (`"page 3 borders median dx +4.31pt"`) — sparse pages can no longer hide, and failures are self-diagnosing.
- Calibrate the three defaults against the currently-green corpus (they must pass all 18 as-is; the probe's own longdoc evidence suggests comfortable margins — pre-fix medians were +0.55pt and the fix brought them to +0.05pt). Record the calibration run in `LAYOUT_CALIBRATION.md`.
- Adversarial tests (required): the +1px translated-page fixture must FAIL the geometry gate (median 0.5pt hits the 0.6 median bound via p95 1.5 — verify and tune bounds so it fails); the pre-AutoFit `table-fixed` output must FAIL; a substituted-font page with identical wraps must PASS.

### Completion requirements (amended)

Sign-off re-verification = fresh checkout, two consecutive runs (cold fetch, then offline): **all 18 cases green under (a) frozen legacy SSIM thresholds AND (b) the geometry gate**, with both SSIM variants reported. The nine typography cases return to green automatically under this ruling — no owner re-baselining, no H3 batch.

## `gdocs-tables-longdoc` — a real regression, ruled separately (owner decision D2)

The blocker is right that this is a product decision, and right not to hide it. Facts: 0.918 legacy under the old *Liberation* fallback set; 0.843 legacy under the production-equivalent *Arimo/Tinos/Cousine* fetch, minimum on the typography-demo page 5. Before any decision, the implementer must run two checks: (a) **Fontsource subset coverage** on page 5 — if characters fell outside the fetched `latin` subsets, glyphs fell back to other faces entirely; fetching the missing subsets may recover most of the delta cheaply; (b) **probe page 5** — if text origins align (expected: Arimo/Tinos are metric-compatible), the residue is pure outline/hinting raster noise and the geometry gate passes regardless.

Then the owner picks:

- **D2-i (recommended): register Liberation as app-owned worker assets in production.** Liberation is OFL and *already ships in the pdf package's `fonts/` directory* — no fetch, no repo commit, no license issue; alias `Arial/Times New Roman/Courier New → Liberation *` restores the calibrated 0.918-class output in both production and harness, coherently. Cost: ~3 MB of host assets (bends "minimal local fonts" slightly — host-side, never in the office package).
- **D2-ii: keep the pure-fetch Arimo/Tinos/Cousine policy** and accept a documented, owner-authorized lower legacy threshold for this case (a deliberate manifest change with the geometry gate as the compensating control). Honest, but it codifies a visible fidelity drop for the most common proprietary fonts.

Until D2 is decided, `gdocs-tables-longdoc` remains red-by-policy; do not adjust anything to green it.

## How Word itself handles this part of the problem (verified 2026-07-16)

Researched against Microsoft's cloud-fonts documentation and the [MS-OE376] §2.8 implementer notes — Word's missing-font ladder, in order:

1. **Embedded fonts first** (§2.8: one embedded font per font name; `embedRegular/Bold/Italic/BoldItalic` collapse to a single face — a laxer contract than ours).
2. **On-demand cloud fetch**: modern Word silently downloads missing fonts from Microsoft's font service (`fs.microsoft.com`) into a per-user persistent cache (`%LOCALAPPDATA%\Microsoft\FontCache\4\CloudFonts\`), including "a variety of popular open-source fonts … provided for document compatibility purposes" — which is precisely how the Word-online export of our ground truths had **Ubuntu 0.83** to embed. *Word's answer to a missing font is discover → fetch → cache → register → lay out — the 6B pipeline is a faithful clone of Word's own mechanism, with Fontsource/jsDelivr standing in for `fs.microsoft.com` and `fonts-cache/` for `FontCache\4\CloudFonts`.*
3. **Metadata-guided substitution** only when fetch is unavailable: Word selects the closest installed font using exactly the `fontTable.xml` "font substitution elements" the spec names — `altName`, `panose1`, `charset`, `family`, `pitch`, `sig` — weighting charset/pitch/family/PANOSE distance. Substitution is **display-only** (the document keeps the original name; File → Options → Advanced → Font Substitution offers "Convert Permanently"), and **Word reflows with the substitute's true metrics** — it makes no attempt at wrap preservation. Documents repaginating on font-poor machines is accepted Word behavior.
4. **Word maintains no raster self-fidelity gate.** Its fidelity promise is *have the font* (cloud/embed); it does not promise pixel identity even across its own platforms (Windows/Mac/web rasterize differently).

**Consequences adopted here:** (a) the on-demand architecture is validated at the highest possible authority; (b) our metric-compatible substitution is *stricter than Word's own* — Word reflows where we preserve wraps — so gating our substituted output on raster identity would hold us to a bar Word does not hold itself to, which independently supports this ruling's geometry/appearance split; (c) Word's use of the PANOSE/family metadata motivates required fix 4 below.

## Additional required fixes surfaced by this round

1. **Resolver precedence parity** (implementer's own resume note): `resolver.rs::mapped_id` must try `request.family` before `alt_families`, matching the JS provider; add the `family=Ubuntu, altFamilies=[Calibri]` regression both sides.
2. **Georgia alias is wrong**: `gdocs-anchors-toc` embeds *Carlito* (a sans) for Georgia (a serif). Google Fonts carries **Gelasio — metric-compatible with Georgia by design**. Add `Georgia → gelasio` to both alias tables (+ lock entries). Expect anchors-toc raster scores to improve; geometry gate unaffected.
3. Keep the blocker's locked-provider discipline (cold hash-verified fetch + offline re-run parity) as a permanent regression test if not already one.
4. **PANOSE/family-guided substitute classing (Word-inspired, generalizes the Georgia fix):** when a requested family has no alias-table entry, choose the substitute class from the `fontTable.xml` metadata that 6A already parses (`oxml/font_table.rs`: `panose1` family-kind/serif-style digits, `w:family`, `w:pitch`) — serif→`tinos`/`gelasio`, sans→`arimo`/`carlito`, mono→`cousine` — exactly the signal Word's own mapper weights. This turns Georgia-class mistakes (serif rendered with a sans) into a structural impossibility instead of a per-family patch. Emit the existing substitution warning with the classing basis in the detail.

## Why not the other options

- **A (weighted baselines now):** workable but brittle — weighted scores are typography-dominated, so every font-pack evolution churns 18 baselines; and it would have gated geometry *indirectly* when we own a direct instrument. Remains available later; not needed for sign-off.
- **B (blur/binarize normalization):** hand-rolling a typography-invariant raster metric needs an experimental campaign to prove it can't hide 1px table errors — the probe gives the same invariance in vector space with no experiment.
- **C (licensed MS fonts):** correctly rejected in the blocker; conflicts with the fontless, redistributable policy.
- **D (informational only):** alone, it abandons the sparse-geometry gate the owner demanded. Under this ruling the *probe* carries that gate instead, so D's spirit survives as item 3 above.
