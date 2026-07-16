# Fix specifications — code review 1

Execution order: **§1 (B1) and B2 first** (they unblock everyone's ability to verify anything), then §2 (S1) → §3 (S2) → §4 (S3), then S4 verification, then rebuild `pkg/` again and re-run the full matrix. Rules unchanged: no threshold/golden/manifest-hash relaxation; every layout-constant change gets a `LAYOUT_CALIBRATION.md` entry; no global offsets.

## §1 — B1: the harness resolves fonts through the REAL on-demand pipeline (owner direction, 2026-07-16)

**Owner ruling: `--font-dir` must not be part of fidelity testing. The harness must exercise document processing the way production does — discover fonts, fetch them, inject them, convert.** The §F5 decision is resolved as: *harness-as-host live resolution with a committed lock file*.

1. **Automatic per-case resolution inside `run`/`probe`** (new dev-only `fidelity/src/resolver.rs` — network code lives in the fidelity crate, never in `office-core`; the core boundary is untouched). For each case, before converting, the harness performs exactly the production host sequence:
   `office_core::font_requests(source)` → normalize + alias (same mapping the JS provider uses: `Calibri→carlito`, `Cambria→caladea`, `Arial→arimo`, `Times New Roman→tinos`, `Courier New→cousine`, `Ubuntu→ubuntu`, …) → **content-hash cache** (`office-core/fidelity/fonts-cache/`, gitignored) → on miss, **live fetch** from the pinned jsDelivr Fontsource paths (`https://cdn.jsdelivr.net/fontsource/fonts/{id}@{pin}/{subset}-{weight}-{style}.ttf`) → validate → register via `ConvertOptions.font_bytes` → convert. Families the native `bundled-fonts` set already satisfies exactly are not fetched (they resolve before the provider step, same as an already-registered face in production).
2. **`fidelity/fonts.lock` (committed):** maps `fontsource-id@pin/file → sha256` for every artifact the corpus needs (`ubuntu@v21`, `ubuntu-mono@v19` faces at minimum). Fetched bytes MUST match the lock or the run fails hard (supply-chain guard + determinism: the corpus scores are a function of locked bytes, honoring both "real fetch" and reproducibility). A `fidelity -- lock-fonts` maintenance command performs first-fetch/refresh and rewrites the lock deliberately; `run` never mutates it.
3. **Offline semantics:** warm cache ⇒ fully offline runs (steady state — identical to production's persistent cache). Cold cache + no network ⇒ instructive bail naming the fonts and cases (`case gdocs-anchors-toc requires Ubuntu; network unavailable and fonts-cache is cold`) — never a silent SSIM red, never a substituted-font score.
4. **Flag demotion:** `--font-dir` remains solely as a diagnostic override for probe experiments; documentation, CI, and `release.sh` use the automatic path with **no manual pre-step**. CI caches `fonts-cache/` across runs; a cold CI run performs one hash-verified fetch (owner-accepted network dependency, already the 6B posture).
5. **Manifest schema addition (additive):** `required_fonts = ["Ubuntu", "Ubuntu Mono"]` on `gdocs-anchors-toc` — the pre-convert check asserts these resolve to exact family matches after resolution, failing with the instructive message otherwise (ties into §4's per-case font assertions, which then verify the *output* embedded the same families).
6. **Production-parity note:** this makes the native harness a faithful host-sequence mirror (requests → resolve → register → convert). The wasm package lane (S4) separately covers the JS provider implementation itself with the deterministic mock; an optional env-gated integration test may point the package at the same locked URLs. A future `--fonts=fetched` lane that disables `bundled-fonts` entirely and fetches *every* family (full production-fontless emulation) is noted as a follow-up, not required for sign-off.
7. Tests: lock-mismatch hard failure; cold-offline bail message; warm-cache offline green; `gdocs-anchors-toc` resolves Ubuntu with **no** `font_fallback` warning; alias-table parity test between the resolver map and the JS provider map (parse the `.cjs` table or share a generated JSON — implementer's choice, but drift must be caught by a test).

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
