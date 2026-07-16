# Findings — code review 1

References are to the branch head at review time (`8ba9042`).

## B1 — Blocker: fidelity green depends on an uncommitted local font directory

**Where:** `office-core/fidelity/src/main.rs` — `Run`/`Probe` accept repeatable `--font-dir` flags feeding `ConvertOptions.font_dirs`; nothing else supplies fonts beyond the native `bundled-fonts` set (Carlito/Caladea/Liberation only — `office-core/Cargo.toml`, `default = ["cli", "bundled-fonts"]`).

**What happens:** `gdocs-anchors-toc` requires Ubuntu 0.83 / Ubuntu Mono 0.80. The calibration log's passing run (0.9071 min) was necessarily invoked with a `--font-dir` pointing at a directory that exists only on the implementing machine. On any fresh checkout — including the owner's — the documented command `cargo run -p fidelity -- run --format docx` resolves Ubuntu through `fallback_class` → Liberation Sans → different advances → different wrap points → **the owner's exact symptoms**: "test failing" and "text not the same as the ground truth" (same characters, different line breaks — which at raster level reads as different text). The release gate (`release.sh` runs the corpus) is therefore red by construction, and the 6B §F5 CI-provisioning decision, left open in the plan, has silently become a defect.

**Consequence for review policy:** a green that cannot be reproduced from the repository is not a green. Fix per `02-fix-specs.md` §1 (seeded, hash-verified font cache + manifest-declared font requirements + a clear failure message). No threshold, golden, or manifest content changes are acceptable as a workaround.

## B2 — Blocker: committed wasm `pkg/` is stale and internally inconsistent

**Evidence from commit contents:** `pkg/office_core_wasm_bg.wasm` (+ `pkg/web/...`) and `pkg/build-fingerprint.json` were last rebuilt in `0480b1b`. Both later commits changed engine source (`729f9a5`: +6,555 lines incl. `lib.rs`, `profile.rs`, fonts modules; `59fc743`: table.rs/flow.rs/matching.rs) **without touching `pkg/`**. Worse, `59fc743` updated `office-core/wasm/index.cjs` (+20) — package JS now targets an API surface the committed wasm binary does not export. Any consumer installing the package gets pre-6A layout behavior with post-6B JS: at best stale output (plausibly the owner's `gdocs-tables-longdoc` "wrong font" observation, if converted through the package with no provider configured), at worst runtime errors.

**Fix:** run `wasm/build.sh`, commit the regenerated `pkg/` (repo rule: "pkg/ is committed and must stay in sync"), and confirm doc-kosha's `package-freshness` fingerprint matches. This must re-happen after S1/S2 land.

## S1 — Should-fix: autofit (auto table layout) is unimplemented; synthetic table cases render grid-width tables

**Where:** `office-core/src/layout/table.rs::resolve_columns` — header comment still reads "v1 uses Word's stored `tblGrid` widths… the min/max autofit refit is a fidelity-harness refinement." For `!layout_fixed` tables the code trusts `tblGrid` and only scales toward `avail.min(total)`.

**Owner's screenshot, explained:** the synthetic corpus builders author `gridCol` values (~2600 twips ≈ 130pt per column) that Word ignores when it lays the fixture out — with `tblLayout` absent, `tblW` absent, and no `tcW`, Word runs **AutoFit** and shrinks columns to content (~1.7" total for the 3×3 case). We render ~5.4". Affects `table-fixed`, `table-merges`, `table-nested` exactly as reported. Note the interaction with real-world files: Word- and Google-authored documents write *resolved* grids (and the gdocs fixtures use `tblLayout=fixed`; `complex` has per-cell `tcW`), so the current grid-trust path is correct for them — the fix must be gated to not disturb calibrated cases (spec §2).

## S2 — Should-fix: SSIM scoring cannot see sparse-page geometry errors

**Where:** `office-core/fidelity/src/ssim.rs::score_pages` — page score is the **mean over all 8×8 windows**. A blank-vs-blank window scores exactly 1.0 (equal means, zero variance). On a Letter page at scale 2.0 (≈1224×1584 px ≈ 30k windows), the 3-row table occupies ≈5% of windows; even if every table window scored 0.0 the page mean stays ≈0.95 — above the 0.93–0.95 synthetic-case thresholds. This is precisely "the fidelity test does not test or check this." Fix: ink-weighted window aggregation (spec §3), which also sharpens dense-page scores' meaning.

## S3 — Should-fix: no font-identity verification per case

**Where:** `score_case` (`fidelity/src/main.rs`) compares rasters only. A provisioning failure (B1) or provider misconfiguration surfaces as an inscrutable SSIM drop rather than "expected Ubuntu, rendered Liberation Sans". The owner explicitly requests family-level verification per test. Fix: extract embedded font families from **both** PDFs per case and assert against manifest-declared expectations through the documented substitution map (spec §4). This also gives `gdocs-tables-longdoc` a direct guard (expected: Carlito, never Noto/Liberation).

## S4 — Should-fix: 6A package-suite deliverables not visible in the reviewed commits

Neither `729f9a5` nor `59fc743` touches `tests/office.test.mjs` (only `tests/worker-office.test.mjs` got a 2-line change in `0480b1b`). The 6A spec requires (a) migrating package tests to the deterministic mock provider, (b) re-keying the deterministic-output hash to (input, font set), and (c) the **packed-tarball fontless gate** (extension + sfnt-magic + base64 scan over `npm pack` output — `build.sh`'s 8 MB budget is necessary but not sufficient). Verify these exist from earlier work or implement them; the review could not locate them.

## Nits

- **N1:** `fidelity/src/main.rs::run` accepts `_update_report` and ignores it — remove the flag or implement it.
- **N2:** `probe_page_count` renders up to 10,000 pages to count them; `pdf_core` exposes a page count in one call — dev-only cost, but needless.
- **N3:** `ssim.rs::heatmap` takes unused `_w/_h` parameters.

## Explicitly verified as sound (no action)

- Fragment-edge reservations and the `tblInd` outer-edge/centerline split are evidence-backed (probe numbers in `LAYOUT_CALIBRATION.md`) with the `complex` control at +0.02pt.
- `validate_provenance` Creator-fallback + bare-online-Word matcher branch carries a regression test against the real `gdocs-anchors-toc` ground truth.
- Native `bundled-fonts` retained for hermetic dev/harness (plan-conformant); wasm build takes fonts only from the host.
- The roundtrip and selectability tests (ToUnicode / subset mapping) remain in place.
