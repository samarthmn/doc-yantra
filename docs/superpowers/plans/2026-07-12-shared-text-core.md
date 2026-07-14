# Shared text-core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add one HarfRust/bidi shaping engine shared by pdf-core and office-core, preserve fail-closed coverage, and expose office runtime fonts through native, wasm, and worker APIs.

**Architecture:** `text-core` owns font parsing, paragraph itemization, bidi visual runs, cluster-safe face fallback, coverage preflight, and positioned glyph output. The PDF and Office crates retain their layout and embedding machinery, adapting shared glyph IDs, clusters, advances, and offsets into their existing IRs and CID subsets.

**Tech Stack:** Rust 1.89, edition 2024, harfrust, unicode-bidi, unicode-script, ttf-parser; existing pdf-writer/subsetter consumers.

## Global Constraints

- `harfrust` is the only new shaping dependency and is explicitly approved; `unicode-bidi` is reused and `unicode-script` is allowed for script data.
- No ICU4X, Parley, Typst, GPL, LibreOffice, or Chromium code.
- Line breaking remains consumer-owned.
- Non-BMP Rust `char` values must remain intact.
- Missing coverage must be typed and fail closed.
- Do not overwrite in-flight package artifacts or parallel edits.

---

### Task 1: Shared shaping engine

**Files:** Create `text-core/Cargo.toml`, `text-core/src/lib.rs`; modify both consumer Cargo manifests.

**Interfaces:** Produce `Face`, `Direction`, `ItemizedRun`, `ShapedGlyph`, `ShapedRun`, `ShapeError`, `itemize`, `preflight_coverage`, `shape_run`, and `shape_text`.

- [ ] Add unit tests for bidi visual runs, joining/conjunct/Thai clusters when test faces exist, cluster-boundary fallback, exact missing codepoints, and U+1F512.
- [ ] Run `cargo test --manifest-path text-core/Cargo.toml` and confirm the new API tests fail before implementation.
- [ ] Implement plane-safe itemization, coverage, fallback, and HarfRust shaping.
- [ ] Run text-core tests and clippy to green.

### Task 2: pdf-core registered-font shaping

**Files:** Modify `pdf-core/src/embed_font.rs`, `pdf-core/src/generate.rs`, `pdf-core/src/watermark.rs`, and focused tests.

**Interfaces:** Preserve standard-14 WinAnsi output; registered faces expose shared shaping bytes/index and encode shaped GIDs with cluster text into existing CID/ToUnicode data.

- [ ] Write/strengthen native fixtures for Hebrew, mixed RTL/LTR, Arabic/Indic typed coverage, and emoji astral shaping.
- [ ] Run focused tests and observe the old script gate or 1:1 cmap path fail them.
- [ ] Replace registered-font measurement/encoding with shaped runs and remove the script gate.
- [ ] Run focused pdf-core tests to green.

### Task 3: office-core shaping and runtime fonts

**Files:** Modify `office-core/src/text/shape.rs`, script/profile gates, font DB/selection, `office-core/src/lib.rs`, wasm binding, worker sources, declarations/docs, and focused tests.

**Interfaces:** Public `office_core::register_font(&[u8]) -> Result<()>`; shared shaping output adapted to existing Office glyph IR; worker option `extraFontDirs: string[]` sorted and registered at boot.

- [ ] Add failing tests for Hebrew shaping, Arabic typed MissingGlyph, native global runtime registration, and worker directory registration validation/order.
- [ ] Implement shared shaping and remove unconditional ComplexScript/RtlText profile gates.
- [ ] Implement process-global runtime fonts after bundled/document fonts and wire wasm/worker APIs.
- [ ] Run focused office and Node tests to green without rebuilding in-flight packages.

### Task 4: Full verification and report

**Files:** All changed manifests/locks/source/tests/docs; do not regenerate `pkg/` while parallel package edits exist.

- [ ] Run rustfmt and clippy for all three crates.
- [ ] Run text-core, pdf-core, office-core, and fidelity cargo suites.
- [ ] Run applicable Node source tests; record package rebuild as coordinator-owned.
- [ ] Report exact test summaries, fixture flips, script renderability, dependency licenses, deviations, and `git diff --stat`.
