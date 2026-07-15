# Plan: docx-universal-fidelity — process any DOCX, at high fidelity

**Repo in scope:** `samarthmn/doc-yantra` only — every change lives in `office-core/` (the DOCX→PDF engine), its fidelity harness, and the corpus. `pdf-core/` is untouched (the harness keeps consuming it read-only for rasterization). **`Sublime-Innovation-Technologies/doc-kosha` was checked and requires no changes**: the fixes are engine-internal; DocKosha picks them up through the normal `0.0.x` release + pin bump (human-tasks H5). No plan folder, branch, or PR is created in doc-kosha.

**Mission (from the 2026-07-14 planning thread):** four reference DOCX files were supplied with Microsoft-Word-exported PDF ground truths (`1.docx/1.pdf` … `4.docx/4.pdf`). Files 1–3 fail `office_to_pdf()` today; file 4 converts (it is the already-committed `fidelity-corpus/docx/complex/` fixture — SHA-256 `8c2a8c47…` / `e808b449…`). This plan makes files 1–3 convert with high fidelity **and** hardens the engine so DOCX from any mainstream producer (Word, Google Docs, LibreOffice exports of the same idioms) converts rather than failing on bookkeeping noise — while preserving the engine's fail-closed philosophy: **we widen the envelope by implementing constructs, never by silently degrading visible content.**

Planning thread: https://hyperagent.com/thread/cmrki1y8p0ixg06adbyatevrz (fixture files attached there).

---

## Why files 1–3 fail today (verified against `main` @ `4bb9efc`)

The three failing files are Google-Docs-produced DOCX (fingerprint: no `docProps/`, `w:tblGridChange` revision blocks, explicit `w:val="0"` toggles everywhere, `Table1…TableN` literal table styles, `Noto Sans Symbols` bullet fonts, decimal twip values). File 4 is Word-produced. Every failure below was traced statically through `profile()` — the single source of the envelope verdict (`office-core/src/lib.rs::convert_docx`).

| # | Blocker | Where | Hits |
|---|---|---|---|
| 1 | **`w:rPr` inside `w:pPr` (paragraph-mark run properties) is treated as visible-unsupported** → `unknown_element: "w:rPr in w:pPr"`. Google Docs writes it on *every* paragraph (files 1/2/3: 335/1924/64 occurrences; files 2–3 as an *empty* `<w:rPr/>`). ECMA-376 §17.3.1.29 defines it as the paragraph mark's run properties — legitimate, near-invisible bookkeeping. | `office-core/src/oxml/wml.rs`, `parse_para_props`, the arm listing `"suppressLineNumbers" | … | "rPr"` | **all three files** |
| 2 | **Anchored-drawing subset too narrow** → `anchored_drawing` ×4: anchors A/B (77×77 pt corner images) use `positionV relativeFrom="margin"` + vertical `<wp:align>top</wp:align>` (both rejected); anchor C uses `positionH relativeFrom="column"` (only `margin` allowed); and anchors B+C share one paragraph → `"multiple anchored drawings in one paragraph"`. The margin-anchored images sit mid-document (blocks 47/49 of 80), so they must resolve against the page they *land* on — page-scoped floats with left+right wrap bands (agent-tasks-2). | `wml.rs::parse_anchor`; `profile.rs::Collector::paragraph`; `layout/paragraph.rs` | file 1 |
| 3 | **Complex fields cannot span paragraphs** → `field_unsupported: "field without cached result: TOC …"`. The TOC field's `begin`+`separate` sit in sdt paragraph 0, `end` in paragraph 17; the per-paragraph field state machine flushes paragraph 0 as a region with empty `cached_runs`. (`w:hyperlink` inside a field result also bypasses `cached_runs` by design of `inline_level`.) | `wml.rs::inline_level` / `run_into_field`; check in `profile.rs::Collector::paragraph` | file 1 |
| 4 | **Suspected: glyph coverage for superscript characters** U+2076 ⁶, U+2079 ⁹, U+207B ⁻ (scientific notation in body text). Bundled faces are Carlito/Caladea/Liberation only; if none maps these, `add_fallback_face_missing_reason` emits `missing_glyph`. Must be confirmed by running `officecore profile 2.docx` locally (task 1.0). | `profile.rs::add_fallback_face_missing_reason` | file 2 |
| 5 | **Continuous multi-column sections render wrong (post-fix)**: `flow.rs::layout_docx` merges a continuous section into the *following* section's batch ("v1 uses the final section's geometry"), flattening file 3's two 2-column continuous sections into 1 column, and mis-reads `w:type` (a sectPr's type describes how *its own* section begins, i.e. the transition *into* it — not the transition after it). File 3 = 5 same-geometry sections: 1-col → 2-col(unequal) → 1-col → 2-col → 1-col; Word yields 3 pages. | `office-core/src/layout/flow.rs::layout_docx` | file 3 |

**Verified false suspects** (already supported; do not touch): footnotes with matching ids (full layout in `flow.rs::place_footnotes`), floating tables (`w:tblpPr` parsed, laid out), multi-column sections per se (`profile.rs` test `multicolumn_section_is_supported`), `<w:rtl w:val="0"/>`, `w:tblGridChange` (skipped inside `tblGrid`), `w:pgNumType`, `pBdr` with `val="nil"`, `smallCaps`/`vertAlign baseline`/`u none`/`jc left`, decimal `w:w` on `tblW`/`tblInd`/cell margins (`TblWidth::parse` is f64), `w:lang` in styles, `tblStylePr` conditional table styles.

**Fidelity bugs found alongside (no error, wrong output):**
- `parse_i64` rejects decimal twips → `w:ind w:firstLine="192.00000000000003"` / `w:hanging="504…"` (file 1 ×14, plus `numbering.xml`) silently dropped → wrong indents. Same latent risk everywhere `parse_i64` reads a *measurement*.
- Footnote markers render the raw `w:id` (file 1 ids are 0,1 → markers "0"/"1"; Word renders 1/2).
- Internal hyperlinks (`w:hyperlink w:anchor=…`, 18 in file 1's TOC) drop their link because bookmarks are ignored at parse and `LinkTarget` has no anchor form.
- File 1's floating table gets no side text-wrap (placed as a full-width block region).
- `assemble.rs::core_dates` reads `"docprops/core.xml"` (lowercase) — verify OPC part-name case handling; likely dead code path for date metadata.

---

## Approach

Five agent phases plus human tasks. Phase 1 alone brings **all three files inside the profile envelope** (subject to the file-2 coverage check); phases 2–3 make files 1 and 3 *lay out correctly*; phase 4 closes the fidelity gaps; phase 5 freezes all three as blocking corpus cases so the envelope never regresses. The engine's two contracts are preserved throughout: **fail-closed** (unknown visible constructs still refuse) and **DocProfile parity** (`profile()` and `convert()` never disagree — every phase carries parity tests, and the layout preflight trigger is widened where layout gains new failure modes).

| # | File | Delivers | Depends on |
|---|---|---|---|
| 1 | `agent-tasks-1-parser-root-causes.md` | paragraph-mark rPr; cross-paragraph fields; decimal measurements; Google-Docs conformance test battery | — |
| 2 | `agent-tasks-2-anchors-and-floats.md` | margin/page/column-relative anchors, vertical alignment, page-scoped wrap bands; floating-table side wrap | 1 |
| 3 | `agent-tasks-3-columns-and-sections.md` | correct section-transition semantics; per-page column blocks with balancing | 1 (parallel with 2) |
| 4 | `agent-tasks-4-fidelity-and-robustness.md` | footnote numbering; internal link destinations; bullet/superscript glyph strategy; small fixes | 1–3 |
| 5 | `agent-tasks-5-corpus-and-gates.md` | three frozen corpus cases + manifests; provenance matcher for online-service exports; package-level tests; docs | 1–4, H1–H2 |
| 6A | `agent-tasks-6a-fontless-package.md` | fontless production package + host font-resolution architecture: `bundled-fonts` out of production builds, provider abstraction + validated registration + strict/compatible modes, OFL trio served via worker boot-registration from pdf-package assets, mock-provider test migration (determinism re-keyed), fontless-artifact gate. **Zero fidelity movement; release-coupled to 6B** | 1–5 executed (see `implementation-handoff-2026-07-15/`) |
| 6B | `agent-tasks-6b-on-demand-ubuntu.md` | reference HTTP broker + hash-pinned catalog (Ubuntu 0.83 / Ubuntu Mono 0.80), end-to-end validated acquisition, telemetry `hash_pinned_exact` — owns the `gdocs-anchors-toc` ≥ 0.90 gate. CI font-provisioning decision explicitly open. Ships in the same release as 6A | 6A, H6 |
| 7 | `agent-tasks-7-table-geometry.md` | vector geometry probe (content-stream diff vs Word GT); fragment-aware outer border edges; vertical-seam shared-edge painter — unblocks `gdocs-tables-longdoc` | 1–5 executed, H7 (parallel with 6) |

**Execution order:** H1 (fixtures) + H2 (provenance) can happen any time before phase 5 → agent phase 1 → agent phases 2 ∥ 3 → H3 review gate (fidelity run on gdocs cases, expect layout-level convergence) → agent phase 4 → agent phase 5 → H3 final review gate → H4 release + DocKosha pin bump. Log every layout-constant adjustment in `office-core/LAYOUT_CALIBRATION.md` per its invariant.

**Note — fidelity runs while the gdocs fixtures are staged without manifests (H1 done early, phases 1–4):** the harness hard-fails a FULL `cargo run -p fidelity -- run --format docx` on any case directory missing its `manifest.toml` (`main.rs::run` bails before writing reports), and — even with manifests — `validate_provenance` rejects the gdocs ground truths' bare online-export metadata until T5.2's matcher extension lands. Therefore, while phases 1–4 are in flight:
- verify existing-case health with per-case runs: `cargo run -p fidelity -- run --case <name>` (filtering happens before manifest reads, so manifest-less gdocs dirs don't affect scoped runs);
- if mid-plan heatmaps of the gdocs cases are wanted (the H3 gate after phases 2 ∥ 3), execute **T5.2 early** — it is independent and test-covered — and stage the three T5.1 manifests; the full run then writes `report.json`/`index.html` before failing the (expectedly red) gates, which is exactly what the review needs;
- do NOT merge the staged fixtures/manifests to `main` ahead of the engine work — `pdf-core/scripts/release.sh`'s blocking corpus gate would fail for unrelated releases.

## Envelope policy (decision, please review)

- Constructs that are **invisible bookkeeping** (paragraph-mark rPr with no visible effect, explicit `val="0"` toggles, `tblGridChange`) are *parsed or ignored*, never `unknown_element`.
- Constructs with a **visible effect** get implemented (this plan) or stay typed-`Unsupported` (unchanged: charts w/o cache, SmartArt w/o fallback, equations outside subset, wrapThrough, complex-script shaping).
- **Documented-lossy** renders (bullet-glyph substitution, synthesized super/subscript digits) emit `Warning`s in the DocProfile so DocKosha telemetry sees them; they never silently drop content.

## Out of scope (explicit)

PPTX/XLSX paths; complex-script shaping (`harfrust`, still deferred); charts/SmartArt/OLE/WordArt/equations beyond current behavior; pdf-core changes of any kind; wasm API surface changes (`office_profile`/`office_to_pdf`/`register_font` signatures stay; new behavior is additive); doc-kosha repo changes.

## Verification that cannot run in the planning sandbox

The planning sandbox has no Rust toolchain, no wasm-pack, no pdfium, and no package registry. Every agent-task file ends with the local verification list; implementers MUST run locally:

```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
cargo test --target wasm32-unknown-unknown --no-run --no-default-features --features wasm,bundled-fonts   # wasm build health
cargo run -p fidelity -- run --format docx        # blocking corpus incl. the 3 new cases (after H1/H2)
cd wasm && ./build.sh && cd ../.. && node --test tests/                    # rebuild committed pkg + package suite
```
