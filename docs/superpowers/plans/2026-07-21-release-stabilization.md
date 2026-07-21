# DocYantra and DocKosha Release Stabilization Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make DocYantra safe and release-ready, then prove DocKosha works with the locally built packages and passes every release gate.

**Architecture:** Follow the approved design in `docs/superpowers/specs/2026-07-21-release-stabilization-design.md`. Add focused limits at parser/allocation boundaries, keep failures typed and recoverable, rebuild reproducibly, and validate DocKosha through paired local package overrides.

**Tech Stack:** Rust, Wasm, Node.js, pnpm, Next.js, Supabase, Cargo and Node test runners.

## Constraints

- [ ] Add a small failing regression test before each fix; never use real OOM, stack-overflow, or timing fixtures.
- [ ] Preserve existing fidelity thresholds and update DocKosha goldens only after semantic and visual proof.
- [ ] Never read private `.env*` files; use local Supabase and explicit test/public configuration.
- [ ] Keep scratch files under the active repository's `tmp/` and remove only files created during this work.

## Task 1: Fix Office Safety and Existing Test Failures

- [ ] Add `UnsupportedCode::ResourceLimit`, Wasm taxonomy coverage, and shared checked budget helpers.
- [ ] Bound XML nodes during `roxmltree` parsing and preflight ZIP/ZIP64 entry counts before `ZipArchive` allocation.
- [ ] Validate Word columns/table spans/tabs and XLSX coordinates/ranges before layout or vector allocation.
- [ ] Fix `i64::MIN`, checked numbering counters, per-list resets, and linear tab resolution.
- [ ] Consume a 2,000-page budget before every DOCX/PPTX/XLSX page creation.
- [ ] Budget image working bytes, deduplicate encoding-equivalent variants, and cap/shared-own runtime fonts.
- [ ] Fix the current all-feature compile, formatting, and clippy failures.
- [ ] Run focused tests after each change, then all Office native, all-feature, Wasm, and fidelity suites.

## Task 2: Fix Release and Supply-Chain Integrity

- [ ] Pin Rust 1.96.1, Node 22.23.1, wasm-pack 0.15.0, and GitHub Actions by reviewed full SHA.
- [ ] Remove mutable wasm-pack fallbacks; checksum downloads before use.
- [ ] Verify Pdfium build 7763 archives before extraction and pin built-in Fontsource versions/content hashes.
- [ ] Upgrade `crossbeam-epoch` to 0.9.20; document the scoped `ttf-parser` maintenance exception without hiding active vulnerabilities.
- [ ] Add deterministic source/artifact provenance and third-party notices to both npm packages.
- [ ] Make CI and the local release gate rebuild twice, test, compare committed package bytes, audit, and inspect packed file lists before publishing.

## Task 3: Rebuild and Verify DocYantra

- [ ] Rebuild both committed Wasm packages from the pinned toolchain.
- [ ] Run format, clippy, native, all-feature, no-default/Wasm, wrapper, package, fidelity, audit, reproducibility, and pack-content gates.
- [ ] Confirm both package trees match their final source/provenance and the worktree contains only intentional changes.

## Task 4: Link and Fix DocKosha

- [ ] With write approval, create a `codex/` branch in `/Users/samarthmn/Projects/si-tech/doc-kosha`.
- [ ] Apply both supported `file:` overrides together in `pnpm-workspace.yaml`, force install, verify package freshness, and sync browser Wasm.
- [ ] Re-run the two known failures. Change `complex 2.docx` from 46 to 44 pages only after its source hash and canonical 44-page fidelity case pass.
- [ ] Regenerate the final Office byte golden only in a fresh worker after page count, extracted content/links, dimensions, structural checks, and visual fidelity pass.
- [ ] Upgrade vulnerable production dependencies in small groups: Next.js; AWS/Supabase; Sentry/PostHog; PDF.js/viewer; Nodemailer; remaining compatible advisories.
- [ ] For PDF.js, migrate viewer compatibility, sync the matching `.mjs` worker, and set `isEvalSupported: false` on every document-loading path.
- [ ] For Nodemailer, set `disableFileAccess` and `disableUrlAccess` and verify delivery through tests/Mailpit.
- [ ] After each dependency group run focused unit/type/build tests; finish with lint-fix, format-fix, typecheck, knip, unit, browser/e2e, production build, and production audit.

## Task 5: Restore Registry Mode and Final Gate

- [ ] Remove all local `file:`/`link:` overrides, reinstall immutable registry packages, and confirm manifests, lockfiles and `node_modules` contain no local provenance.
- [ ] Re-run DocKosha package-freshness plus the complete final gate against registry-mode bytes.
- [ ] Require zero actionable critical/high advisories; document only genuinely unpatched, unreachable exceptions.
- [ ] Review both worktrees, clean created scratch artifacts, and report exact tests, residual risks, and changes. Do not merge or publish without a separate explicit instruction.
