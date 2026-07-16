# Human tasks — docx-universal-fidelity (doc-yantra repo)

Everything here needs a human (the fixture binaries, a Word installation, judgment calls at review gates, releases). Steps are ordered relative to the agent phases. **If a step here blocks an agent task, both files say so.**

## H1. Commit the three reference fixture pairs (BEFORE agent phase 5; ~10 min)

You hold the six original files (`1.docx` … `3.pdf`) locally — this step is a plain copy. (Backups exist for anyone without them: the planning thread https://hyperagent.com/thread/cmrki1y8p0ixg06adbyatevrz carries the originals, and this branch carries bit-exact base64 copies under `hyper-agent/docx-universal-fidelity/fixtures/*.b64` with a decode script in `fixtures/README.md`.) `4.docx`/`4.pdf` are already committed as `fidelity-corpus/docx/complex/` — do NOT re-add them.

1. Copy your local files into the corpus (from the repo root; exact destination names `source.docx` / `ground-truth.pdf`):
   ```bash
   SRC=/path/to/your/reference-files   # wherever 1.docx … 3.pdf live
   mkdir -p fidelity-corpus/docx/{gdocs-anchors-toc,gdocs-tables-longdoc,gdocs-columns}
   cp "$SRC/1.docx" fidelity-corpus/docx/gdocs-anchors-toc/source.docx
   cp "$SRC/1.pdf"  fidelity-corpus/docx/gdocs-anchors-toc/ground-truth.pdf
   cp "$SRC/2.docx" fidelity-corpus/docx/gdocs-tables-longdoc/source.docx
   cp "$SRC/2.pdf"  fidelity-corpus/docx/gdocs-tables-longdoc/ground-truth.pdf
   cp "$SRC/3.docx" fidelity-corpus/docx/gdocs-columns/source.docx
   cp "$SRC/3.pdf"  fidelity-corpus/docx/gdocs-columns/ground-truth.pdf
   ```
2. Commit the binaries only at these `fidelity-corpus/docx/<case>/` paths — never under `hyper-agent/`. The `fixtures/*.b64` copies stay untouched on the plan branch as provenance.
3. Verify integrity (`shasum -a 256 <file>`), expected SHA-256:
   | File | SHA-256 |
   |---|---|
   | gdocs-anchors-toc/source.docx | `1e68879b7bd7b161ed320fa5f203cfe3709cb300a4c4476941bc2ea60b47ca91` |
   | gdocs-anchors-toc/ground-truth.pdf | `8b42f556073a3a2e251664abc6a65ac247975124072d658a2920406bee2533c1` |
   | gdocs-tables-longdoc/source.docx | `5ea1a2c4e049adf55e2618ac7a73f72791170a81dd1bee2b6a888f736d3287f3` |
   | gdocs-tables-longdoc/ground-truth.pdf | `0edeb20b010ed04474eee8add27b58476cc384b584bf0abfcfbc2fe983323ac7` |
   | gdocs-columns/source.docx | `4fcfc62f878667f0f1cbd5f55204a7cf4e3c88dd31c05354e0bd54651ef5a397` |
   | gdocs-columns/ground-truth.pdf | `5f731bda8c149c00a1683f81f15049babab9d10366fae38d0f19f8164956d6f0` |
4. Content-policy check (done during planning, re-confirm): these are **publicly distributed sample documents** (file 2's footer literally reads "This sample DOCX file is provided by Sample-Files.com"), not customer content — committing them follows the `complex/` precedent.
5. Commit on the implementation branch. Agent task 5.1 authors the three `manifest.toml` files; it needs these binaries in place to validate hashes.
6. Expected outcome: `cargo run -p fidelity -- missing-ground-truth` no longer lists the three new cases.

## H2. Ground-truth provenance — RECORDED 2026-07-14 (nothing left to do)

**Recorded from the plan owner:** all three ground truths were exported with **Microsoft Word for the web** (https://word.cloud.microsoft/ — the online exporter stamps no version) on **2026-07-14, ~18:00 IST**. This matches the planning-time metadata read of the PDFs: Producer absent, `Creator: "Microsoft Word"` (bare family string), embedded `CreationDate: 2026-07-14`.

Consequences (already reflected in the agent tasks — no further human action):
1. Task 5.1's manifests use `word_version = "Microsoft Word (online export service)"` and `ground_truth_exported_at = "2026-07-14"` for all three cases, with the word.cloud.microsoft detail in `notes`.
2. The current `word_producer_matches` accepts only LTSC / "for Microsoft 365" / 4-digit-year variants, so this bare-family metadata cannot pass today — **task 5.2 (matcher extension) is REQUIRED**; the re-export alternative was declined.
3. The implementing agent re-verifies the embedded metadata when authoring the manifests (`exiftool <ground-truth.pdf> | grep -iE 'producer|creator|createdate'` or `pdf_core::meta::get_metadata`); if any PDF disagrees with the above, stop and reconcile with the plan owner.
4. Expected outcome once tasks 5.1 + 5.2 land: `cargo run -p fidelity -- run --format docx` passes provenance validation for all three cases (SSIM gates may stay red until phases 2–4 complete — that is the point of the gate).

## H3. Review gates (AFTER phase 3, and again AFTER phase 5; ~30 min each)

1. `cargo run -p fidelity -- run --format docx` → open `tmp/fidelity-report/index.html`.
2. For each `gdocs-*` case, eyeball ours vs ground truth vs heatmap:
   - `gdocs-columns`: 3 pages; two balanced 2-column bands; footer text on each page.
   - `gdocs-anchors-toc`: 10 pages; two top-corner wrapped images on page 1 area; dotted-leader TOC with page numbers; footnotes 1/2 at page bottoms; floating info-table with side text.
   - `gdocs-tables-longdoc`: 44 pages; page-count parity is the main gate; scan tables and list indents.
3. Per failing case decide: engine bug (hand the case name + heatmap back to the coding agent) vs threshold/ground-truth problem (adjust that case's `manifest.toml`, note why in the commit).
4. Any layout-constant change an agent makes during calibration must have a matching entry in `office-core/LAYOUT_CALIBRATION.md` — reject review otherwise.
5. Sign-off criterion (same as the office-engine plan): 100% of `tier: core` cases pass, ≥ 90% of `extended`; none of the three new cases may regress `complex/` (its per-page SSIM floor today is 0.927610).

## H4. Release + consumer pin bump (AFTER phase 5)

1. From clean `main`: `pdf-core/scripts/release.sh` (bumps all four Cargo manifests, rebuilds both wasm packages, runs the blocking DOCX fidelity corpus — now including the three gdocs cases — commits, tags `0.0.x`, pushes; `publish-wasm.yml` publishes on the tag).
2. In doc-kosha (outside this plan's scope): bump the `@samarthmn/doc-yantra-office` pin to the new `0.0.x` + `pnpm install`, per that repo's standing process. No doc-kosha code changes are needed.
3. Expected outcome: DocKosha converts the three previously-failing documents in production; `unsupported:unknown_element "w:rPr in w:pPr"` disappears from conversion-failure telemetry.

## H5. Corpus flywheel (ONGOING)

When DocKosha telemetry surfaces new real-world failures: reproduce with either a synthetic equivalent (never customer text) or a verified public sample, add a `fidelity-corpus/docx/<case>/`, export ground truth per H2's provenance rules, and hand the failing case to a coding agent. Candidates already known from this analysis: a scientific-notation/superscript-heavy case (locks task 4.5's behavior), and a Google-Docs export carrying `⮚`/`♦` deep-level bullets (locks task 4.4).

## H6. Verify the Fontsource Ubuntu pins + stage the fallback family (WITH agent phase 6B; ~20 min)

Phase 6 is **on-demand fonts via Fontsource** (owner direction, final revision 2026-07-15): the published package ships zero font binaries; the worker keeps one local fallback family; everything else is fetched on demand. See `agent-tasks-6b-on-demand-ubuntu.md`.

1. **Ubuntu lineage check — ✅ DONE 2026-07-15 (agent-run, no human action needed).** Downloaded Fontsource `ubuntu@v21` and `ubuntu-mono@v19` TTFs (bytes served by jsDelivr; metadata by api.fontsource.org) and parsed their `name` tables: **Ubuntu Regular/Bold = version `0.83`**, **Ubuntu Mono = `Version 0.80`** — exact match to the ground-truth embeds. Non-variable weights [300,400,500,700] confirm the classic family; the redesign is the separate id `ubuntu-sans`. **Pins recorded: `ubuntu@v21`, `ubuntu-mono@v19`.** No fallback-source substitution needed.
2. **Fallback family — ✅ DONE 2026-07-15 (owner staged, agent verified).** Six faces + OFL.txt live on first-party storage at `https://public.dockosha.com/fallback-fonts/NotoSans/`: Noto Sans Regular/Bold/Italic/BoldItalic v2.015, plus **Noto Sans Symbols v2.003 and Noto Sans Symbols 2 v2.008** — added after verification showed modern Noto Sans lacks the symbol blocks (● U+25CF etc.). Union coverage across the six faces verified for the fallback probe set (bullets incl. ⮚ U+2B9A, dashes, quotes, superscripts, Cyrillic/Greek). Nothing committed to any repo or package. Worker boot-fetches from this origin and persists in the font cache (see 6B §B).
3. **CI provisioning choice — ✅ DONE 2026-07-15 (agent-run).** Package and worker tests use the deterministic mock provider plus local OFL fixture fonts; CI never calls Fontsource, jsDelivr, or the first-party fallback origin.
4. **Provider gate — ✅ DONE 2026-07-15.** All requested faces resolve to the fetched assets (Ubuntu 0.83 in four styles, Ubuntu Mono 0.80, Cousine, and Noto Symbols), with 10/10 pages. The verified fonts first reached 0.8867034393; the phase-7 line-box probe then identified that Word reserves the hhea line gap above the glyph ascent. After that general geometry fix, the minimum SSIM is **0.9070758765**, above 0.90, without changing a golden or threshold.

## H7. Diagnostic ground truths for the table probe (WITH agent phase 7; ~20 min)

1. The agent (T7.3) authors six single-variable diagnostic DOCX fixtures and hands them to you.
2. Export each to PDF via word.cloud.microsoft — the same flow used for H2.
3. Place them at `office-core/fidelity/diag/<name>/ground-truth.pdf`. These are probe diagnostics only — **not** under `fidelity-corpus/` (everything there gates releases), no manifests, no SSIM thresholds.
4. Expected outcome: `cargo run -p fidelity -- probe --diag <name>` reports paired geometry deltas for each fixture.

## No other human work

No infrastructure, env vars, or third-party dashboards are involved in this repo. Toolchain prerequisites are unchanged from the office-engine plan (Rust ≥ 1.89 with `wasm32-unknown-unknown`, wasm-pack, one-time `pdf-core/scripts/fetch-pdfium.sh` for the harness).
