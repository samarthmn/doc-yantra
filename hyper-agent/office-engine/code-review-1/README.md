# Code review — round 1 (office-core engine)

**Scope:** branch `hyper-agent/office-engine` at `6275728` (= tag `0.0.6`, published to GitHub Packages), reviewed against the plan in `hyper-agent/office-engine/` (agent tasks 1–7).
**Review depth:** contracts, container security, profile/router contract, wasm boundary, packaging/CI, and the fidelity-harness history were read line-by-line (`lib.rs`, `error/profile/opc`, `wasm/src/lib.rs`, `Cargo.toml`, workflows, `LAYOUT_CALIBRATION.md`, `office-core/README.md`). Layout/emit/fonts/text/pptx/xlsx internals were reviewed via their contracts, unit/corpus test evidence, and commit history — not line-by-line; per-area deep-dives can follow on request.
**Verdict:** ⚠️ **Changes requested** — the engine is genuinely Tier A, the contracts are faithfully implemented, and the fidelity loop demonstrably works (the `w:kern` calibration took the docx corpus green). One finding is a contract-violating blocker, but it is **PPTX-scoped**: nothing here blocks DOCX shadow/rollout in DocKosha once that repo's own blockers are fixed.

## Findings index

| # | Severity | Area | Issue |
|---|---|---|---|
| Y-B1 | **Blocker (pptx)** | `src/profile.rs` + `src/pptx/` | Preset/custom geometry not implemented (bounding-box fills) **and not profile-gated** → shape-bearing decks convert silently degraded |
| Y-S1 | Should-fix | `LAYOUT_CALIBRATION.md` | Calibration log not updated for the `w:kern` fix — the log's entire purpose |
| Y-S2 | Should-fix | `src/profile.rs`, `src/lib.rs`, `emit/` | Mismatched-format content profiles `supported:true` and can emit a 0-page (invalid) PDF |
| Y-S3 | Should-fix | `src/profile.rs` | Headers/footers profiled for the final section only; mid-section headers escape the envelope; hdr/ftr parse errors silently swallowed |
| Y-S4 | Should-fix | repo hygiene | Engine released from an unmerged branch; `main` has no engine — merge after this round closes |
| Y-N1 | Nit | `src/profile.rs` | `shape_count` counts groups as shapes (stats inflation) |
| Y-N2 | Nit | `src/profile.rs` | `fonts_used` collects only ascii/hAnsi families (eastAsia/cs ignored; CJK gate mitigates) |
| Y-N3 | Nit | `.github/workflows/publish-wasm.yml` | Workflow still named "Publish pdf-core-wasm"; tests waived on tag-push by design — rename + keep the release.sh-only discipline |

---

## Y-B1 (Blocker, pptx routing) — ungated bounding-box geometry violates the fail-closed contract

`office-core/README.md` (Deferred): *"v1 renders shape fills against their bounding box"* — the plan's T5.3 guide-formula evaluator was deferred. Deferring the evaluator is a legitimate scope call. **Not gating it is not.** `profile_pptx()` walks shapes and flags charts/SmartArt/OLE/crops/scripts — but never inspects `prstGeom`/`custGeom`. Consequence: a deck of arrows, stars, chevrons, callouts profiles `supported: true`, and `convert()` draws every one of them as a filled rectangle. That is precisely the "silently-degraded page" contract #1 forbids, and DocKosha's router trusts `supported` blindly (by design — this contract is what makes engine-first routing safe).

There is also a dead no-op in the walker — `if matches!(props.fill, Some(Fill::LinearGradient { .. })) && props.custom_geom { /* comment only */ }` — which suggests a gating intent that never landed.

**Fix (either, before any pptx traffic):**
1. **Gate it (recommended now):** in `profile_pptx`'s walker, flag any shape whose geometry is not plain `rect` — `prstGeom prst="rect"` passes; every other preset → `add_reason_deduped(UnknownElement, format!("preset:{name}"))` (or a dedicated `ShapeGeometry` code appended to the enum — additive is allowed); `custGeom` → the same. Delete the dead `if`. Add profile tests: a `star5` deck and a `custGeom` deck are unsupported; a rect-only deck passes. `render_pptx` keeps its bounding-box path only for `rect`.
2. **Or land T5.3** (vendored `presetShapeDefinitions.xml` + formula evaluator) and validate via the pptx fidelity corpus (`shapes-preset-matrix` case) before routing.

**Cross-repo action (recorded in doc-kosha's review too):** until this lands + pptx corpus passes H4 gates, DocKosha must not route pptx — enforced today only by luck (its own env-parsing blocker keeps everything off). Belt-and-suspenders option there: temporarily restrict `OFFICE_ENGINE_EXTENSIONS` to `docx`/`xlsx`/`xlsm`.

## Y-S1 — the calibration log rotted on day one

`LAYOUT_CALIBRATION.md` still reads *"No adjustments yet — awaiting Word ground truths"*, yet commit `91ac3f3` performed a textbook calibration: kerning gated on `w:kern` (Word kerns only when asked; unconditional kerning narrowed lines → break drift → long-doc SSIM 0.64), taking the four dense-text cases to 0.955/0.953/0.993/0.971. That is exactly the institutional memory the log exists to keep. **Fix:** add the entry (axis: line-break drift; change: `RunProps.kern_min_pt` gating, docx=w:kern, pptx=always, xlsx=never; driving cases + before/after scores), and treat "calibration commit ⇒ log entry" as a review-enforced invariant from now on.

## Y-S2 — cross-format content: `supported:true`, then a 0-page PDF

The test `all_formats_profile_without_panicking` enshrines that a **docx** profiled as `Pptx`/`Xlsx` yields an empty *supported* verdict (0 slides/sheets). `convert()` on such input would lay out zero pages and emit a 0-page PDF — invalid per spec, treated as corrupt by viewers. DocKosha's router happens to catch this via its `page_count > 0` validation, but the CLI and any other consumer get a broken file with `Ok(..)`.
**Fix:** (a) `PptxDoc::open`/`XlsxDoc::open` verify the main part's content type (`presentationml`/`spreadsheetml`) and return `Invalid("main part is not a presentation/workbook")` otherwise — `Package::content_type` already exists for this; (b) `emit::pdf::emit` returns `Internal("document produced no pages")` for an empty page list (defense in depth); (c) update the test to assert the *new* correct behavior.

## Y-S3 — envelope leak: only the final section's headers/footers are profiled

`header_footer_parts()` reads refs from `document.final_sect` only. A multi-section document (which the docx envelope supports — "sections" are in) can carry different headers per section; content in a mid-section header (a chart, complex script, missing font) is never profiled, yet layout will process that header. Whether layout then fails closed or silently degrades depends on its internal guards — either way the profile↔convert agreement contract is at risk for exactly the class of document the contract-lock test can't cover unless a fixture exists.
**Fix:** union header/footer refs across every `sectPr` (each paragraph `sect_break` + `final_sect`); add a two-section fixture with an unsupported element in the *first* section's header and assert the profile catches it. Also: the `if let Ok(blocks) = wml::parse_hdr_ftr(text)` swallows header parse failures silently — propagate as `Invalid` (or an `unknown_element` reason) instead of skipping.

## Y-S4 — release hygiene: `0.0.6` is published from an unmerged branch

`main` still ends at the plan commit; the entire engine + corpus + workflow changes live only on `hyper-agent/office-engine` (the tag is reachable, so the published package is safe). Two risks: `main`'s docs/README don't mention office-core (confusing for any consumer reading the repo), and a future force-push/cleanup of the branch orphans the tag's context. **Fix:** merge the branch into `main` once Y-B1/Y-S2/Y-S3 land (the branch already contains the plan folder, so the merge is clean history, no conflicts expected).

## Nits

- **Y-N1:** in `profile_pptx::walk`, `*shape_count += 1` fires for `Group` elements *and* their children — stats.shapes inflates on grouped decks. Count leaf shapes only.
- **Y-N2:** `Collector::run_font` and the pptx font walk collect ascii/hAnsi/typeface only; `eastAsia`/`cs` families never reach `fonts_used`/`fonts_missing`. The CJK-coverage gate catches the fatal case; the stats/warnings just under-report. Note or fix.
- **Y-N3:** `publish-wasm.yml` is still named "Publish pdf-core-wasm" though it now publishes both packages; rename to "Publish wasm packages". The "tests waived on tag, release.sh is the gate" note is an accepted trade-off — keep cutting releases exclusively via `release.sh`.

---

## What's correct (verified, no action)

- **Tier A is real:** `office-core/Cargo.toml` dependencies are exactly the permitted leaf set (zip/roxmltree/ttf-parser/subsetter/unicode-*/pdf-writer/image/miniz_oxide/serde/thiserror/sha2 + optional clap). No Typst, no pdfium, no engines, nothing copyleft. `#![deny(missing_docs)]` on the crate.
- **Contracts implemented as planned:** `convert()` runs `profile()` first and fails closed for all three formats (and `7be7e64` locks profile↔convert agreement over the real mock corpus for pptx/xlsx — guarding exactly the false-positive direction that matters); `DocProfile` serializes the planned camelCase shape with golden tests; the wasm boundary ships `version`/`office_profile` (never throws for valid input, `from_error` mapping)/`office_to_pdf` (`unsupported:<code>: ` prefix via the serde slug — matches DocKosha's parser)/`register_font` (validates before storing), with the 50 MiB pre-copy guard.
- **Container security:** `opc.rs` has all planned zip-bomb guards (10K entries / 512 MiB part / 1 GiB total, plus a belt-and-suspenders post-decompression cap), CFB magic detection with the `EncryptedPackage`/`EncryptionInfo` UTF-16LE heuristic separating `encrypted` from `legacy_format`, normalized part names, `..`-safe target resolution — all unit-tested.
- **Determinism:** `/ID` = SHA-256(input ‖ version)[..16]; documented emission-order object numbering; fixed-date fallback.
- **The harness is not theater:** corpus committed with Word ground truths (H3 done), determinism hardening (pinned A4 + docDefaults + explicit Normal — `4396064`/`ffd6b88`), a page-size-mismatch diagnostic instead of degenerate 0.0 scores, a redesigned report, an invalid-fixture fix caught *because Word auto-repaired it* (`29e2686`), and a real calibration (kerning) that took docx fully green. This loop working end-to-end is the single most important thing the plan asked for.
- **Packaging/CI:** `office-core-ci.yml` present; publish workflow extended with an idempotent office publish (skip-if-exists guard) after pdf-core's, independent failure domains; fidelity harness is a non-default workspace member so CI needs no pdfium; wasm crate excluded from the workspace and built by `build.sh` per pdf-core convention; `0.0.6` published and consumed by DocKosha (pins landed).
- **pdf-core untouched** apart from the release version bump, per the plan's hard rule.

## Re-verification checklist (same round, after fixes)

- [ ] Y-B1: geometry gate in `profile_pptx` (+ tests: star5/custGeom unsupported, rect-only passes) — or T5.3 evaluator landed + pptx corpus green
- [ ] Y-S2: content-type check in Pptx/Xlsx open + 0-page emit guard + test updated
- [ ] Y-S3: all-section header/footer profiling + swallowed-parse-error fix + two-section fixture
- [ ] Y-S1: calibration log entry for `w:kern`
- [ ] Y-S4: branch merged to `main` (after the above)
- [ ] `cargo test` (office-core) green; `cargo run -p fidelity -- run --format docx` still fully green; wasm check green
- [ ] Release `0.0.7` via `release.sh` when Y-B1 lands (DocKosha pin bump follows)
