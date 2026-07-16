# Ink-weighted SSIM rollout evidence

Run date: 2026-07-16. Command:

```bash
cd office-core
cargo run -p fidelity -- run --format docx \
  --report-dir ../tmp/code-review-1-final-current-lock
```

The run used the committed font lock and its content-addressed cache. Page
counts, provenance, conversion, and per-case embedded-font contracts all
passed. No manifest threshold, frozen input hash, or golden hash was changed.

| Case | Tier | Ink-weighted | Legacy | Frozen threshold | Weighted result |
| --- | --- | ---: | ---: | ---: | --- |
| `complex` | core | 0.814679 | 0.937401 | 0.90 | fail |
| `fonts-substitution` | core | 0.894301 | 0.997443 | 0.94 | fail |
| `gdocs-anchors-toc` | core | 0.342462 | 0.907076 | 0.90 | fail |
| `gdocs-columns` | core | 0.670614 | 0.928075 | 0.90 | fail |
| `gdocs-tables-longdoc` | core | 0.450947 | 0.843417 | 0.90 | fail |
| `headers-footers` | core | 0.966729 | 0.998218 | 0.95 | pass |
| `hyperlinks` | extended | 0.960305 | 0.998946 | 0.95 | pass |
| `justify-long-text` | core | 0.708894 | 0.971370 | 0.95 | fail |
| `lists-multilevel` | extended | 0.991852 | 0.999529 | 0.93 | pass |
| `long-doc-pagination` | core | 0.921335 | 0.955218 | 0.95 | fail |
| `page-fields` | core | 0.921223 | 0.955054 | 0.95 | fail |
| `plain-paragraphs` | core | 0.944980 | 0.992634 | 0.97 | fail |
| `run-formatting-matrix` | core | 0.850072 | 0.996316 | 0.96 | fail |
| `spacing-exact-atleast` | extended | 0.979962 | 0.999168 | 0.94 | pass |
| `table-fixed` | core | 0.981586 | 0.999422 | 0.95 | pass |
| `table-merges` | core | 0.990134 | 0.999697 | 0.94 | pass |
| `table-nested` | extended | 0.995980 | 0.999851 | 0.93 | pass |
| `tabs-leaders` | extended | 0.995958 | 0.999862 | 0.94 | pass |

The AutoFit cases now pass the stricter metric, which confirms that the new
weighting detects and gates the table geometry it was introduced to cover.
Nine other cases still pass their frozen legacy threshold but fail solely
after the metric switch: `complex`, `fonts-substitution`,
`gdocs-anchors-toc`, `gdocs-columns`, `justify-long-text`,
`long-doc-pagination`, `page-fields`, `plain-paragraphs`, and
`run-formatting-matrix`. Their report images show aligned content with the
expected substituted-font outline and antialias differences; under fix spec
§3.4 these are H3 owner-judgment cases. Thresholds remain frozen pending that
decision.

`gdocs-tables-longdoc` is the one case below its threshold in both metrics in
this locked-provider run. It retains exact 44-page parity and embeds only its
declared post-substitution families. The provider path deliberately uses the
Fontsource Arimo/Carlito/Cousine/Tinos artifacts rather than the prior
machine-local font directory, so its current locked-input baseline is recorded
separately instead of being hidden by a threshold change.
