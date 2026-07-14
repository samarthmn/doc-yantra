# Agent tasks 5 — corpus cases, gates, and documentation (freeze the win)

Depends on: agent-tasks-1..4 landed; human-tasks **H1** (fixture binaries committed) and **H2** (provenance strings) done. All work in `office-core/` + `fidelity-corpus/` + `tests/`. References `main` @ `4bb9efc`.

## T5.1 — Three frozen corpus cases

H1 placed the binaries; author the manifests (`fidelity-corpus/docx/<case>/manifest.toml`), mirroring `fidelity-corpus/docx/complex/manifest.toml` exactly. These are committed-only cases like `complex/` — they must NOT be added to `office-core/fidelity/src/corpus.rs::cases()` (that list is the *synthetic* corpus; `seed` iterates only it and therefore never touches these directories — verify once and note it in the manifest `notes`).

```toml
# fidelity-corpus/docx/gdocs-anchors-toc/manifest.toml
title = "Google Docs export: TOC field, internal links, page-anchored images, floating table, footnotes"
tier = "core"
ssim_threshold = 0.90
max_page_count_delta = 0
word_version = "<H2 string, e.g. Microsoft Word (online export service)>"
ground_truth_exported_at = "2026-07-14"   # confirm against PDF CreationDate (H2)
input_sha256 = "1e68879b7bd7b161ed320fa5f203cfe3709cb300a4c4476941bc2ea60b47ca91"
golden_sha256 = "8b42f556073a3a2e251664abc6a65ac247975124072d658a2920406bee2533c1"
notes = "Public sample document (Sample-Files.com); Word-exported ground truth. Committed-only case (not in corpus.rs cases()); freezes the docx-universal-fidelity envelope expansion. Expected 10 pages."
```

Same shape for `gdocs-tables-longdoc` (input `5ea1a2c4…`, golden `0edeb20b…`, notes: "44 pages; page-count parity is the primary gate; tables + list indents") and `gdocs-columns` (input `4fcfc62f…`, golden `5f731bda…`, notes: "3 pages; continuous 2-column bands with balancing; footer inherited across sections").

Threshold policy: start every case at the harness minimum `ssim_threshold = 0.90`, `tier = "core"` (they gate releases — that is the point). After H3 review, ratchet thresholds up to observed-minus-margin (like `complex/` at 0.90 with observed 0.9276) in a follow-up commit; never down without an H3 note.

## T5.2 — Provenance matcher for online-service exports (skip if H2 chose re-export)

`office-core/fidelity/src/main.rs::word_producer_matches` accepts only LTSC / "for Microsoft 365" / 4-digit-year declared strings. The attached ground truths carry bare `Creator: "Microsoft Word"` (Word's online export service stamps no Producer/version). Extend, keeping strictness:

```rust
// After the ltsc / 365 branches, before the 4-digit-year branch:
// Word's online export service stamps the bare family string with no
// version. Accept it only when BOTH sides are exactly the bare family —
// a bare declaration must never match an LTSC/365/year-stamped PDF.
if declared == "microsoftwordonlineexportservice" || declared == "microsoftword" {
    return actual == "microsoftword";
}
```

Tests in the existing `provenance_helpers_check_word_variant_and_embedded_date` style: bare↔bare passes; bare declared vs `"Microsoft® Word LTSC"` actual fails; `"Microsoft Word 2021"` declared vs bare actual fails. Also confirm `validate_provenance`'s Producer-or-Creator fallback picks up Creator when Producer is absent (it does — `metadata.producer.or(metadata.creator)` — pin with a test PDF byte-string if cheap).

## T5.3 — Package-level regression (`tests/office.test.mjs`)

The Node suite validates the **built** `office-core/wasm/pkg`. Add a `gdocs fixtures` describe-block:

1. Load the three sources from `fidelity-corpus/docx/*/source.docx` (path-relative like existing fixture loads from `mock-files/`).
2. For each: `office_profile(bytes, "docx")` → `supported === true`, `reasons.length === 0` (allow only documented warnings); `office_to_pdf(bytes, "docx")` → bytes start `%PDF-`.
3. Page-count parity via the sibling pdf package (already loaded by the parity tests): `pdf.page_count(out)` → `10`, `44`, `3`.
4. Runtime note: 2.docx is 44 pages and 4.docx already runs in this suite; if wall-time matters, keep `gdocs-tables-longdoc` behind the same long-running convention other slow tests use (follow `tests/README.md`).

## T5.4 — Profile/convert parity sweep (the contract, mechanized)

New Rust integration test `office-core/tests/envelope_parity_test.rs`:

```rust
// For every DOCX in fidelity-corpus/docx/*/source.docx and mock-files/**/*.docx:
// profile() and convert() must agree — supported ⇔ Ok, and on failure the
// first reason code must equal the convert error code.
```

Iterate both trees (skip non-docx), run `profile` + `convert`, assert agreement. This turns the "profile predicts the verdict" contract from prose into a gate covering every present and future fixture. (It also retro-covers T2.7/T3.4's preflight widenings.)

## T5.5 — Documentation truth-up

1. `office-core/README.md`:
   - **Formats table** DOCX row: add footnotes/endnotes (sequential numbering), paragraph-mark rPr, cross-paragraph field results, tab leaders, internal link destinations, page-anchored drawings (margin/page-relative V, vertical align, column-relative H), multiple anchors per paragraph (≤1 paragraph-relative), floating-table side wrap, continuous multi-column section bands with balancing, decimal measurement tolerance.
   - **Envelope (summary)** anchor paragraph: rewrite to the new bounded subset (current text says margin-relative-H + paragraph-relative-offset-V only — now stale); enumerate what still fails closed (`wrapThrough`, `simplePos`, inside/outside alignment, line/character origins, anchors in multi-column sections, overlapping same-side bands, no-window layouts, non-convergent page floats).
   - New warning codes: `bullet_substituted`, `superscript_synthesized`, `floating_table_block_fallback`.
2. `office-core/LAYOUT_CALIBRATION.md`: entries for any constant introduced/tuned (band window floor 36 pt, column-balance fudge, leader metrics) — with per-page SSIM evidence from the H3 run, per that file's invariant.
3. `CONSUMING.md`: no API change (confirm: `office_profile`/`office_to_pdf`/`register_font` signatures untouched; new behavior is additive). One line in the office-package section noting the envelope expansion and new warning codes, so DocKosha's fallback-reason dashboards aren't surprised.
4. Do NOT edit `hyper-agent/office-engine/**` (frozen history of the original build plan).

## T5.6 — Definition of done (whole plan)

| Check | Gate |
|---|---|
| `officecore convert` succeeds on all four reference files | exit 0; no `unsupported:*` |
| Page counts | 10 / 44 / 3 / 6 (`max_page_count_delta = 0`) |
| Fidelity | `cargo run -p fidelity -- run --format docx` green: 100% core (now 17+ core cases incl. the three new), ≥90% extended; `complex/` floor 0.927610 not regressed |
| Parity | `envelope_parity_test` green over corpus + mock-files |
| Package | `wasm/build.sh` rebuilt `pkg/` committed; `node --test tests/` green incl. T5.3 |
| Docs | README envelope current; calibration log updated; PR description carries T1.0 baseline vs final reasons diff |
| Release | via H4: `pdf-core/scripts/release.sh` (its fidelity gate now includes the three cases) |

## Local verification

```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
cargo run -p fidelity -- run --format docx
cargo run -p fidelity -- missing-ground-truth        # must print "all corpus cases have ground truths"
cd wasm && ./build.sh && cd ../.. && node --test tests/
DRY_RUN=1 pdf-core/scripts/release.sh                # preview the release gate end-to-end
```
