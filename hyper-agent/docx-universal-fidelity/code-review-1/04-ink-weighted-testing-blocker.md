# Ink-weighted fidelity testing: sign-off blocker

Status: paused for owner review on 2026-07-16. The implementation is committed
in `a1142dc`. No SSIM threshold, frozen input hash, golden hash, or ground truth
has been changed.

## Summary

The ink-weighted calculation behaves as specified, but the existing corpus
thresholds were calibrated for a different score distribution. The old metric
gave every blank 8×8 window the same influence as a window containing text or a
table border. The new metric gives ink windows most of the influence. This
exposes sparse geometry errors, as intended, but it also magnifies harmless
glyph-outline, hinting, and antialias differences between Microsoft fonts in
the Word PDFs and the metric-compatible open-source fonts used by DocYantra.

The current locked-provider run passes 8 of 18 cases. Nine cases pass their
unchanged legacy threshold but fail the same numerical threshold under the new
metric. Fix spec §3.4 classifies these as H3 owner-judgment cases. One additional
case, `gdocs-tables-longdoc`, fails both metrics after the harness switches from
the old native fallback set to the production-equivalent Fontsource aliases.

The unresolved question is therefore a metric-baseline policy decision, not an
unknown implementation failure in the weighting formula.

## What the implementation computes

For each 8×8 window, the harness computes the existing SSIM value `s_i` and:

```text
ink_i = max(fraction of ours with luma < 250,
            fraction of truth with luma < 250)
weight_i = max(ink_i, 0.01)
page_score = sum(weight_i * s_i) / sum(weight_i)
```

The case score is the minimum page score. `ssim` contains the weighted score and
gates the case; `ssim_unweighted` reports the old mean for comparison. The
heatmap threshold remains 0.90.

Unit coverage confirms the intended edge cases:

- identical and blank page pairs score 1.0 in both metrics;
- a small black rectangle missing from an otherwise blank page scores about
  0.94 under the old mean and below 0.20 under the weighted mean;
- transparent pixels are composited over white before luma and SSIM are
  calculated;
- page-geometry mismatches fail rather than being padded into a plausible
  score.

## Observed threshold conflict

These cases pass their frozen legacy threshold but fail after the weighting
change:

| Case | Weighted | Legacy | Frozen threshold |
| --- | ---: | ---: | ---: |
| `complex` | 0.814679 | 0.937401 | 0.90 |
| `fonts-substitution` | 0.894301 | 0.997443 | 0.94 |
| `gdocs-anchors-toc` | 0.342462 | 0.907076 | 0.90 |
| `gdocs-columns` | 0.670614 | 0.928075 | 0.90 |
| `justify-long-text` | 0.708894 | 0.971370 | 0.95 |
| `long-doc-pagination` | 0.921335 | 0.955218 | 0.95 |
| `page-fields` | 0.921223 | 0.955054 | 0.95 |
| `plain-paragraphs` | 0.944980 | 0.992634 | 0.97 |
| `run-formatting-matrix` | 0.850072 | 0.996316 | 0.96 |

Their page counts, frozen-file hashes, provenance, conversions, and embedded
font contracts pass. The most extreme case, `gdocs-anchors-toc`, retains exact
10-page parity, embeds Carlito, Cousine, Ubuntu, and Ubuntu Mono as declared,
and reproduces its earlier 0.907076 legacy score. Its weighted score falls to
0.342462 because most influential windows contain substituted-font glyphs.

This is not cache drift. A cold run downloaded and hash-validated the committed
font lock. A second run completed with all network proxies pointed at an
unreachable address. Both runs produced identical reports.

The full per-case comparison is in
[`03-ink-weighted-rollout.md`](./03-ink-weighted-rollout.md).

## Why matching layout can still score poorly

SSIM compares local means, variances, and covariance. Within an 8×8 text
window, a one-pixel change in a glyph edge can change most non-white pixels in
that window. Ink weighting then gives that window much more influence than the
surrounding white page.

Word's PDFs embed proprietary families such as Calibri, Arial, Times New Roman,
and Courier New. The fontless package lawfully uses Carlito, Arimo, Tinos, and
Cousine. These fonts target compatible advances and wrapping, but their glyph
outlines and raster hinting are not pixel-identical. The new aggregation treats
those expected typography differences as strongly as a displaced table border.

This explains why the three AutoFit fixtures now pass—the metric correctly
detects their repaired table edges—while several text-heavy, visually aligned
cases fall far below thresholds that the old blank-dominated mean could pass.

## The separate `gdocs-tables-longdoc` result

`gdocs-tables-longdoc` scores 0.450947 weighted and 0.843417 legacy against a
0.90 threshold. It has exact 44-page parity and embeds only its declared
families: Arimo, Carlito, Cousine, and Tinos.

The minimum occurs on page 5, a typography demonstration page that explicitly
uses Times New Roman, Arial, and Courier New. The production-equivalent provider
maps those requests to Tinos, Arimo, and Cousine. The earlier 0.918 result used
the native bundled fallback set, including Liberation faces, so it was not a
score for the same font inputs. The current raster remains structurally aligned
with Word, but the changed glyph programs reduce page 5 to 0.450947 weighted and
0.843417 legacy. Other pages retain legacy scores from 0.925648 to 0.989439.

This case needs a separate product decision: keep the production Fontsource
aliases and approve a new locked-input baseline, or change the production alias
policy and recalibrate it across package and native lanes. A threshold-only
change would hide that decision.

## Conflicting completion requirements

The current review text requires all three of these conditions:

1. §3.3 makes ink-weighted `ssim` the gating metric.
2. §3.4 freezes every existing threshold and assigns visually correct failures
   to an H3 owner decision.
3. The re-verification block requires the complete corpus to be green.

The locked-provider evidence cannot satisfy all three at once. Keeping the
specified formula and the old numerical thresholds leaves ten cases red.
Making the run green requires an explicit ruling on the score baseline, the
metric, or the font inputs.

## Decisions that would unblock sign-off

### Option A — approve separate weighted baselines (recommended)

Keep the existing legacy thresholds as a regression check, add a separately
reviewed weighted threshold for each case, and freeze both. This preserves the
new sparse-geometry sensitivity without pretending that weighted and
unweighted SSIM share the same numerical scale. The owner must authorize the
new baseline because §3.4 currently forbids threshold changes.

### Option B — normalize typography before weighted SSIM

Blur, binarize, or use a small edge-distance tolerance before calculating
window SSIM. This could preserve table-geometry sensitivity while reducing
glyph-outline noise, but it changes `s_i`, requires a new specification, and
needs adversarial tests proving that it cannot hide one-pixel table errors.

### Option C — run fidelity with licensed Microsoft fonts

Supplying the same font programs as Word would reduce typography noise. This
conflicts with the fontless, redistributable package policy and would make fresh
checkout reproduction depend on licensed external assets. The repository must
not extract or redistribute fonts from the Word PDFs.

### Option D — keep weighted SSIM informational during migration

Continue gating the frozen legacy thresholds while collecting weighted scores.
This avoids an immediate baseline change, but it does not satisfy §3.3, which
currently makes the new metric blocking.

The owner decision should also state whether `gdocs-tables-longdoc` must retain
the Arimo/Tinos/Cousine production aliases or return to a Liberation-based
mapping before any weighted baseline is approved.

## Reproduction

From the repository root:

```bash
./pdf-core/scripts/fetch-pdfium.sh  # first run on a machine only
cd office-core
cargo test -p fidelity
cargo run -p fidelity -- missing-ground-truth
cargo run -p fidelity -- run --format docx \
  --report-dir ../tmp/ink-weighted-review
```

The final command intentionally exits non-zero because the current core gate is
red. Its HTML report is `tmp/ink-weighted-review/index.html`; `report.json`
contains both metrics and the embedded font sets.

## Resume note: separate provider-parity defect

While preparing this handoff, the audit found one unrelated B1 follow-up. The
shipped JavaScript provider tries `request.family` before `request.altFamilies`,
but `fidelity/src/resolver.rs::mapped_id` currently checks alternatives first.
The alias tables match, yet their precedence can diverge when both names map.
On resume, add a regression in which `family = Ubuntu` and `altFamilies =
[Calibri]`, then make the Rust resolver choose Ubuntu before Calibri. This issue
does not explain the weighted-score scale conflict above.
