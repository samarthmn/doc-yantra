# Code review 1 — phases 6A/6B/7 implementation (`docx-universal-fidelity`)

Reviewed commits: `0480b1b` (step-0 remediation baseline), `729f9a5` (6A fontless provider package), `59fc743` (6B on-demand fonts + phase-7 table geometry), against the plans in this folder. Review scope also covers three owner-reported issues (2026-07-16): synthetic table cases render wrong column widths that fidelity does not catch; `gdocs-anchors-toc` fails outside the implementer's environment with divergent text; `gdocs-tables-longdoc` renders the wrong font family in the owner's environment, and per-case font verification is requested.

**Verdict: NOT signed off.** Two blockers (both about reproducibility, not layout quality), plus three should-fix items that adopt the owner's reported issues with full specifications. The layout engineering itself is strong work.

## What is correct (and genuinely impressive)

- **The phase-7 methodology worked exactly as designed.** The vector probe (`fidelity/src/probe.rs`, 1,573 lines) measured the predicted displacements precisely — `+0.55pt x` on all longdoc table pages (hypothesis H2), `+0.5pt y` on continuation pages only (H1), and 112/71 duplicate vertical segments (H4) — and the fixes follow the evidence: `TableLayout.leading_pt/trailing_pt` exposed to flow and applied **per page fragment**; explicit-negative `w:tblInd` anchors the painted outer edge while default-start tables keep the centerline convention (the `complex` page-6 control measured within 0.02pt — the split is evidence-based, not a fudge); vertical seams now use the same wider-then-darker shared-edge resolver with collinear merge. `gdocs-tables-longdoc` 0.8125 → **0.9181**, 44/44 pages.
- **A genuine calibration discovery:** Word reserves the hhea `lineGap` *above* the glyph ascent. Ubuntu's 28/1000 gap (0.308pt @ 11pt) and Liberation's 67/2048 (0.393pt @ 12pt) both matched the probe deltas — line-box baselines now use `ascent + lineGap`. `gdocs-anchors-toc` 0.8867 → **0.9071** in the implementer's environment; `complex` page 6 *improved* to 0.9499.
- **6A landed architecturally clean:** `fonts/{registration,reporting,requests}.rs`, `oxml/font_table.rs`, `UnsupportedCode::FontUnresolved`, resolution-order rework, profile/convert font reporting, a 594-line resolution test; `build.sh` builds fontless with an enforced **8 MB budget** ("font payload is forbidden") and stamps a source fingerprint for doc-kosha's freshness test; Fontsource + Noto-fallback providers ship for node and web.
- **Calibration-log discipline is exemplary** — every change carries measured evidence, and intermediate red states were reported honestly rather than claimed as wins.
- Legacy `tabs-leaders` and `headers-footers` comparisons went green via principled fixes (shaped leader advances with no fixed constant; `w:pgMar/@w:footer` measured to the footer block's *bottom*).

## Findings index

| ID | Severity | Summary |
|---|---|---|
| B1 | **Blocker** | Fidelity green is unreproducible: anchors-toc needs Ubuntu via `--font-dir`, but no committed provisioning exists — fresh checkouts (and the owner) fail; the release gate is broken by construction |
| B2 | **Blocker** | Committed wasm `pkg/` is stale: binaries last rebuilt at `0480b1b`, Rust changed twice since; `index.cjs` references APIs the committed wasm lacks |
| S1 | Should-fix | Auto-layout (autofit) column widths unimplemented — synthetic table cases render grid-width tables vs Word's shrink-to-content (owner's screenshot) |
| S2 | Should-fix | SSIM scoring is blind on sparse pages (blank windows average catastrophic table errors away) — ink-weighted scoring |
| S3 | Should-fix | No per-case font-family verification — provisioning failures surface as mysterious SSIM drops instead of "Ubuntu missing" |
| S4 | Should-fix | Package-suite 6A deliverables unverified: mock-provider migration + packed-tarball fontless gate not visible in the reviewed commits |
| N1–N3 | Nit | minor (see findings) |

Details and exact code references: [`01-findings.md`](./01-findings.md). Full fix specifications (ready for the implementing agent): [`02-fix-specs.md`](./02-fix-specs.md).

The post-implementation per-case weighted/unweighted comparison is recorded in
[`03-ink-weighted-rollout.md`](./03-ink-weighted-rollout.md); thresholds and
goldens remain frozen as required by §3.4.

The remaining metric-policy conflict, supporting evidence, and owner decisions
needed for sign-off are documented in
[`04-ink-weighted-testing-blocker.md`](./04-ink-weighted-testing-blocker.md).

## Re-verification required for sign-off

From a **fresh checkout** with no local font directories and no manual pre-steps (owner direction: fidelity must exercise the real on-demand font pipeline — no `--font-dir`):
```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
cargo run -p fidelity -- run --format docx    # resolves fonts itself (preflight → locked fetch → register);
                                              # green incl. gdocs-anchors-toc, with font assertions (S3)
cargo run -p fidelity -- run --format docx    # second run: warm cache, zero network — must also be green
cd wasm && ./build.sh && cd ../.. && node --test tests/   # fresh pkg committed (B2), fontless tarball gate (S4)
```
