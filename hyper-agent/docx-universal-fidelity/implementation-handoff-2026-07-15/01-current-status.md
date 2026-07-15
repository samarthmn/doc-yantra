# Current implementation status

## Committed phases

Phases 1 through 4 passed their implementation and independent review gates.
Phase 5's corpus/gate scaffolding is committed, but its two SSIM gates remain
red.

| Phase | Commits | State |
| --- | --- | --- |
| Parser/root causes | `c6901b5`, `84907a7` | complete |
| Anchors/floats | `ec54d19`, `5f1a8a6`, `139ab5c`, `a0e67e0` | complete |
| Columns/sections | `060ff86`, `924909f`, `23cc60b`, `dab9931` | complete |
| Fidelity/robustness | `02508d1`, `cedd043`, `d8bb0c3` | complete |
| Corpus/gates | `69525d0` | implementation committed; frozen SSIM gate incomplete |

The committed work adds Google Docs parser compatibility, cross-paragraph field
results, decimal measurements, page-scoped floats, floating-table reflow,
continuous section/column layout, inline note markers, internal destinations,
tab leaders, glyph substitution boundaries, provenance matching, package tests,
and exact fixture page-count guards.

## Preserved uncommitted remediation

The stopped worktree contains 21 modified tracked files, approximately 1,733
insertions and 254 deletions. It has no deletions, new font binaries, manifest
changes, golden changes, or threshold changes.

Main changes:

- `wrapTopAndBottom` uses a paragraph-coordinate geometric exclusion band.
- Numbering retains `tab`, `space`, and `nothing` suffix behavior instead of
  dropping the parsed suffix.
- Inline footnote/endnote markers stay on the note text line.
- Keep-next grouping uses a bounded fitting path and an oversized fallback.
- Exact profile/convert error-code parity covers every error variant.
- Paragraph shading, right borders, hanging indents, right/decimal tabs,
  nested vertical merges, section bands, table continuations, footer placement,
  note reserve sizing, and font line metrics have focused fixes.
- Superscript/subscript offsets use font metrics when available.
- The fictional `superscript_synthesized` warning was removed from docs and
  allowlists; uncovered glyphs still fail closed.
- The fidelity scorer now composites RGBA over white before grayscale scoring
  and heatmap rendering. This is a valid defensive fix, but current pdfium
  rasters are already opaque, so it does not raise corpus scores.

Modified files at stop time:

```text
CONSUMING.md
office-core/README.md
office-core/fidelity/src/ssim.rs
office-core/src/fonts/metrics.rs
office-core/src/layout/flow.rs
office-core/src/layout/numbering_layout.rs
office-core/src/layout/paragraph.rs
office-core/src/layout/table.rs
office-core/src/oxml/wml.rs
office-core/src/profile.rs
office-core/src/testkit.rs
office-core/tests/docx_layout_test.rs
office-core/tests/docx_sections_test.rs
office-core/tests/docx_wave3_test.rs
office-core/tests/envelope_parity_test.rs
office-core/tests/gdocs_fixtures_test.rs
office-core/tests/wml_test.rs
office-core/wasm/pkg/build-fingerprint.json
office-core/wasm/pkg/office_core_wasm_bg.wasm
office-core/wasm/pkg/web/office_core_wasm_bg.wasm
tests/office.test.mjs
```

## Verification completed on the final source

- `docx_layout_test`: 84/84 passed.
- Six geometric top/bottom-wrap cases passed, including source-order
  invariance, positive vertical offsets, and anchor-only occupancy.
- The four-way numbering-suffix matrix passed: omitted/default tab, explicit
  tab, `nothing`, and `space`.
- List-counter and plain hanging-indent regressions passed.
- `complex`: 6/6 pages, SSIM 0.9374006584.
- `gdocs-anchors-toc`: 10/10 pages, SSIM 0.6898937923.
- `gdocs-tables-longdoc`: 44/44 pages, SSIM 0.8124519676.

## Verification not completed after the final source edits

Execution stopped before these gates finished:

- full `cargo test --locked`;
- strict all-target/all-feature Clippy;
- final `cargo fmt --check` and `git diff --check`;
- final wasm32 build health;
- final `office-core/wasm/build.sh` after the latest source changes;
- final Node package suite;
- final aggregate 18-case DOCX fidelity run;
- final independent review of the complete uncommitted patch.

The checked-in wasm files were rebuilt earlier in the remediation, then Rust
source changed again. Treat `office-core/wasm/pkg/` as stale until `build.sh`
runs successfully and its fingerprint is rechecked.

Before the last source changes, the Node suite had two real failures: a
deterministic complex-output hash drift and a comparison-report
`tabs-leaders` classification drift. Do not update expected values until the
rebuilt package is inspected and the behavior is accepted.

## Git state

- No remediation commit exists after `69525d0`.
- No files are staged.
- No branch was pushed by this work.
- No release or tag was created.
