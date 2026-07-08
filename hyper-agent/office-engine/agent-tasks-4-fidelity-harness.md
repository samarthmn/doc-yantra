# Agent tasks 4 — fidelity harness (corpus, SSIM scoring, reports)

Prereq: task 3 (a converter to score). The harness is the plan's centerpiece: it turns "high fidelity" from an adjective into a number, gates what the router may claim, and converts every real-world failure into a permanent regression test.

## T4.1 Harness crate — `office-core/fidelity/`

New **dev-only** crate (native-only; never published, never wasm):

```toml
[package]
name = "fidelity"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
office-core = { path = ".." }
pdf-core = { path = "../../pdf-core", default-features = false, features = ["pdfium"] }  # rasterization ONLY
image = { version = "0.25", default-features = false, features = ["png"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
clap = { version = "4", features = ["derive"] }
anyhow = "1"
```
pdfium is a **test instrument** here (rasterizing both PDFs to compare) — it is not part of the engine; the Tier A "no external engines" rule is about the conversion path. Requires `pdf-core/scripts/fetch-pdfium.sh` (documented in human-tasks H1). Use `pdf_core::raster::render_page(pdf, i, &RenderOptions { scale: 2.0, ..Default::default() })` → 144-DPI PNGs for both our output and the ground truth.

## T4.2 Corpus layout — repo root `fidelity-corpus/`

```
fidelity-corpus/
  docx/<case-name>/
    source.docx          # authored fixture (synthetic ONLY — never customer content)
    ground-truth.pdf     # Word export (human task H3)
    manifest.toml
  pptx/<case>/…  xlsx/<case>/…
```

`manifest.toml`:
```toml
title = "tables: gridSpan + vMerge + per-cell borders"
tier = "core"          # core | extended (H4 gates: core must be 100%)
ssim_threshold = 0.97   # per-case override allowed
max_page_count_delta = 0
word_version = "16.106.1"
notes = "…"
```

Seed the corpus (agent-authored `source.docx` files, built with the testkit writer or hand-assembled XML zipped by a script — keep each case MINIMAL, one feature axis per case):
- `docx`: plain-paragraphs, run-formatting-matrix, justify-long-text, fonts-substitution (Calibri/Cambria/Arial/Times), embedded-font, lists-multilevel, table-fixed, table-autofit, table-merges, table-nested, images-inline, headers-footers-firstpage, sections-orientation-change, page-fields, hyperlinks, tabs-leaders, spacing-exact-atleast, long-doc-50-pages (pagination stability). 18 cases minimum.
- `pptx` (task 5 fills): title-body-placeholders, shapes-preset-matrix, gradient-fills, table, images, two-masters. 
- `xlsx` (task 6 fills): number-formats-matrix, column-widths, merged-cells, print-scaling-fittopage, repeat-title-rows, borders-fills.

## T4.3 Scoring — `fidelity/src/ssim.rs`

Implement standard grayscale SSIM (no external crate):
1. Both pages rendered at scale 2.0; if dimensions differ by ≤ 2px, center-pad the smaller with white to match; larger difference → page score 0 (geometry mismatch is a failure, not a resize).
2. Luma: `0.299R + 0.587G + 0.114B` per pixel, f64.
3. 8×8 windows, stride 8: per window compute means, variances, covariance; `SSIM = ((2·μxμy + C1)(2·σxy + C2)) / ((μx²+μy²+C1)(σx²+σy²+C2))`, `C1=(0.01·255)²`, `C2=(0.03·255)²`. Page score = mean over windows.
4. Also compute `diff_pixel_pct` (pixels whose luma differs > 12/255) and produce a heatmap PNG (red overlay where windows score < 0.9) for the report.
5. Case score = min over pages; page-count mismatch beyond `max_page_count_delta` → fail regardless.

## T4.4 Runner & report — `fidelity/src/main.rs`

```
cargo run -p fidelity -- run [--format docx|pptx|xlsx] [--case NAME] [--update-report]
cargo run -p fidelity -- missing-ground-truth        # lists cases lacking ground-truth.pdf (for human H3)
```
`run`: for each case with ground truth: convert with office-core (Unsupported → case FAILS with the code — the corpus must stay inside the envelope; an intentionally-unsupported case belongs in unit tests, not the corpus), rasterize both, score. Outputs:
- `fidelity-report/report.json` — machine-readable: per case {ssim per page, diff_pct, pass, durations}. 
- `fidelity-report/index.html` — self-contained static HTML: summary table (case, tier, SSIM, pass/fail badge), and per case side-by-side `<img>` triplets (ours | ground truth | heatmap). No JS frameworks; inline CSS; images as relative files under `fidelity-report/pages/`.
- Exit code 1 if any `tier="core"` case fails (CI-friendly).
`fidelity-report/` goes into the repo-root `.gitignore`.

## T4.5 Calibration loop (method, executed via corpus iterations)

The formulas in task 3 (line height, spacing collapse, autofit distribution, super/sub factors) contain constants that must be tuned to Word's behavior. Procedure per failing case: read the heatmap → identify the axis (line height drift compounds down the page; table width mismatch shifts columns; spacing errors show at paragraph boundaries) → adjust the ONE relevant constant/formula in `src/layout/` → rerun `--case` → commit when the case passes without regressing others (`run --format docx` full pass). Keep a `LAYOUT_CALIBRATION.md` in `office-core/` logging each adjustment (constant, before/after, driving case) — this file is the institutional memory that makes agent-driven calibration convergent instead of thrashing.

## T4.6 CI hook

Extend `.github/workflows/office-core-ci.yml`: a `fidelity-smoke` job that runs `fetch-pdfium.sh` and `cargo run -p fidelity -- run --format docx` **only when ground-truth PDFs exist in the tree** (guard with a shell `find` check), non-blocking (`continue-on-error: true`) until H4 declares the docx tier shippable, then flip to blocking by removing that line (leave a TODO comment referencing this plan).

## Local verification

1. `pdf-core/scripts/fetch-pdfium.sh && cargo test -p fidelity` (unit tests for SSIM: identical images → 1.0; inverted → ~0; shifted-by-1px text → between, asserted range).
2. `cargo run -p fidelity -- missing-ground-truth` lists every seeded case (before H3) and empties after H3.
3. After H3: `cargo run -p fidelity -- run --format docx` produces `fidelity-report/index.html`; open it and confirm images render.
