# Fix specifications — code review 1

Execution order: **§1 (B1) and B2 first** (they unblock everyone's ability to verify anything), then §2 (S1) → §3 (S2) → §4 (S3), then S4 verification, then rebuild `pkg/` again and re-run the full matrix. Rules unchanged: no threshold/golden/manifest-hash relaxation; every layout-constant change gets a `LAYOUT_CALIBRATION.md` entry; no global offsets.

## §1 — B1: committed font provisioning for the harness (`fetch-fonts` + declared fonts)

1. **New subcommand** `cargo run -p fidelity -- fetch-fonts`: downloads the two pinned artifacts' faces (`ubuntu@v21` R/B/I/BI, `ubuntu-mono@v19` styles required by preflight) from the pinned jsDelivr paths (`https://cdn.jsdelivr.net/fontsource/fonts/ubuntu@<pin>/latin-<w>-<style>.ttf`, per 6B §A), **verifies SHA-256 against constants recorded in the source**, and writes them to `office-core/fidelity/fonts-cache/` (gitignored). Record the eight SHAs at implementation time (first fetch prints them; pin thereafter). Refuses to overwrite mismatching bytes.
2. **Manifest schema addition (additive):** optional `required_fonts = ["Ubuntu", "Ubuntu Mono"]` on `gdocs-anchors-toc`'s manifest (hash fields untouched — manifests declare *requirements*, this is not a threshold change).
3. **Harness behavior:** `run`/`probe` implicitly append `fonts-cache/` to `font_dirs` when it exists. Before converting a case whose `required_fonts` cannot be resolved to an exact family match (registration-level check via the FontDb after option assembly), **bail with an instructive error**: `case gdocs-anchors-toc requires fonts [Ubuntu]; run 'cargo run -p fidelity -- fetch-fonts'` — never a silent SSIM red.
4. **Release wiring:** `release.sh`'s corpus step runs `fetch-fonts` first (network at release time is owner-accepted for these pinned, hash-verified bytes). CI likewise. Document in `human-tasks.md` H6 (the §F5 decision is hereby resolved as *seeded cache*).
5. Tests: sha-mismatch refusal; missing-cache bail message; cache-present run resolves Ubuntu as exact (no `font_fallback` warning — ties into §4).

## §2 — S1: implement Word AutoFit for auto-layout tables (`layout/table.rs`)

**Gate (decides which tables refit — keeps every calibrated case untouched):** run AutoFit iff
`!props.layout_fixed` **and** `props.width ∈ {None, Auto, Nil}` **and** no cell in the table carries an explicit `tcW` of type `dxa`/`pct`.
Under this gate: `complex` (auto `tblW` **but** per-cell `tcW dxa`) → excluded; all three gdocs fixtures (`tblLayout=fixed`) → excluded; the synthetic `table-fixed`/`table-merges`/`table-nested` builders (no `tblW`, no `tcW`, no `tblLayout`) → **included**, matching what Word actually did to their ground truths.

**Algorithm (CSS2 auto-table-layout shape, which Word's AutoFit follows for this subset):**
1. Per cell: `min_content` = widest unbreakable unit (shape each word/atom at the cell's effective run props; reuse the paragraph shaper — no new text machinery), `max_content` = widest unwrapped line; add left+right cell margins and the cell's share of vertical border widths. Spanning cells distribute any excess of their min/max over the spanned columns' current sums proportionally (same distribution rule as the existing `apply_aggregate_width`).
2. Per column: `col_min = max(cell mins)`, `col_max = max(cell maxes)`.
3. Table width `W = min(Σ col_max, avail)`; if `Σ col_min > avail`, `W = Σ col_min` (may overflow the content box exactly as Word does).
4. Column widths: if `W == Σ col_max` → use `col_max`. Else distribute: start at `col_min`, give the remaining `W − Σ col_min` to columns proportionally to `(col_max − col_min)`.
5. Nested tables: recurse (inner AutoFit produces the inner min/max that feed the outer cell's measurements — measure once, reuse).
6. Empty/degenerate content: floor columns at 1pt as today.
- `resolve_columns` keeps its current path for everything outside the gate; delete the misleading "refit is a … refinement" header comment and document the gate.

**Verification:** unit tests pinning absolute widths for a 3×3 "RnCn" fixture (expect ≈ text advance + 2×5.4pt margins per column at 11pt Carlito — assert within 0.5pt); `probe --case table-fixed --page 1` before/after (expect column-edge deltas collapse from ~hundreds of pt to <1pt); re-run the three cases + full corpus; `LAYOUT_CALIBRATION.md` entry with the measured numbers. Expect the three synthetic cases' SSIM to *rise*; if any calibrated case moves >0.002 SSIM, stop and investigate the gate.

## §3 — S2: ink-weighted SSIM (`fidelity/src/ssim.rs`)

1. Per 8×8 window compute `ink = max(inkA, inkB)` where `ink(img) = fraction of the window's pixels with luma < 250` (any non-background ink).
2. Page score: `Σ(w_i · s_i) / Σ(w_i)` with `w_i = max(ink_i, ε)`, `ε = 0.01` (keeps a stabilizing floor and defines blank-vs-blank pages as 1.0 when all windows are blank; a window with ink in exactly one image gets full weight *and* a low `s_i` — that is the point).
3. Report **both** metrics during transition: `ssim` (new, gating) and `ssim_unweighted` (old, informational) in `report.json` + HTML columns; heatmap threshold unchanged.
4. **Rollout discipline:** thresholds stay frozen. Re-run the full corpus; expect sparse table cases to *drop* (possibly below threshold until §2 lands — sequencing §2 before §3 keeps the tree green; if landed together, one commit is fine). Any previously-green case that fails under weighting with *visually correct* output is an H3 judgment call for the owner — document per case; do not silently adjust.
5. Tests: blank-page pair → 1.0; small-black-rect-on-white vs blank → near 0 (was ≈0.95 under mean — pin the old value in a comparison test to document the fix); existing tests updated for the new field names.

## §4 — S3: per-case font-family assertions

1. **Extraction helper** (fidelity crate, native): parse each PDF's font dictionaries and return the set of embedded family names — `/BaseFont` with subset prefixes (`ABCDEF+`) stripped, normalized (strip style suffixes after `-`/`,`; PostScript family aliasing via the name table when the descriptor lacks a clean name). `pdf_core`/`lopdf` object access suffices; no rasterization.
2. **Manifest schema (additive):** `expected_fonts = [...]` — the families *our* PDF must embed (post-substitution), e.g. `gdocs-anchors-toc`: `["Ubuntu", "Ubuntu Mono", "Carlito"]`; `gdocs-tables-longdoc`: `["Carlito"]`; `complex`: `["Carlito"]`; synthetic cases: `["Carlito"]` (their builders pin Calibri). Optional `truth_fonts` for GT sanity (skip if noisy).
3. **Gate:** after convert, assert `embedded(ours) ⊆ expected_fonts` and every `expected_fonts` entry that the document's preflight demanded is present. On failure the case FAILS with `note = "font mismatch: expected [Ubuntu…], embedded [LiberationSans]"` — the owner's longdoc/anchors symptoms become one-line diagnoses. Add a `fonts` column to the HTML report.
4. Tests: doctored expectation mismatch fails with the message; all manifests gain `expected_fonts`; full corpus green afterward.

## S4 — verify/complete the 6A package-suite deliverables

Check `tests/office.test.mjs` + `tests/worker-office.test.mjs` for: deterministic **mock provider** usage (no live CDN in CI), the deterministic-output hash re-keyed to (input, registered font set), and a **packed-tarball fontless gate** (`npm pack` the built `pkg/`, fail on font file extensions, sfnt magics `00 01 00 00`/`OTTO`/`ttcf`/`wOFF`/`wOF2` raw and base64-decoded, and on any resurrected font arrays). Implement whichever are missing; they are 6A acceptance items, not extras.

## Sequencing note for the implementing agent

B2 (pkg rebuild) is cheap but must be **re-done last** after §2/§3/§4 land — rebuild once at the end, commit `pkg/` + fingerprint, then run the fresh-checkout verification block from the review README verbatim.
