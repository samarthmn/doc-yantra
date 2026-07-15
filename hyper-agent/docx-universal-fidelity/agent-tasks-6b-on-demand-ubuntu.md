# Agent tasks 6B — on-demand exact Ubuntu acquisition (`gdocs-anchors-toc` gate)

> **STATUS: planning complete (owner-approved restructure, 2026-07-15). Implementation not started.** Depends on **6A complete** (`agent-tasks-6a-fontless-package.md` — fontless package, provider abstraction, validated registration, mock-provider test suite all green with zero fidelity movement). 6B exercises that architecture against a real catalog and owns the fidelity gate. Together with 6A it replaces the deleted `agent-tasks-6-on-demand-fonts.md`.

## ⚠️ Release coupling (restated from 6A)

6A and 6B ship in **one** `0.0.x` release; nothing is tagged between them. The release additionally waits on the DocKosha worker follow-ups (§E) so a fontless package never reaches production without host font supply.

## §A Reference HTTP broker adapter (host layer, optional, configurable)

Configuration: endpoint, auth headers, timeout, max artifact bytes, redirect limit (≤3), parallelism. Protocol: (1) POST one batched resolution request of canonical face metadata (never text); (2) receive a versioned manifest — artifact metadata (§6A C1 `FontArtifact`) with SHA-256 and signed URLs or inline bytes; (3) download unique artifacts; (4) verify SHA-256; (5) hand bytes to `office_register_font` with the full `FontExpectation`. `found` / `not_found` / `denied` / `temporary_failure` handled distinctly — never collapsed into "missing". HTTPS-only in production; CORS documented for browser consumers; no Google/Microsoft endpoints, no undocumented cloud-font services, document text never transmitted.

## §B Catalog entries — hash-pinned Ubuntu (with human task H6)

- **Ubuntu 0.83** Regular / Bold / Italic / Bold Italic and the **Ubuntu Mono 0.80** styles the fixture's preflight actually demands (`office_font_requests` on `gdocs-anchors-toc/source.docx` decides the exact set — verify against the ground truth's embedded subsets).
- These legacy versions match the ground-truth PDF embeds; the catalog must pin them by SHA-256 and **must not** drift to the redesigned current Ubuntu release (different outlines *and* metrics).
- Per-artifact catalog record (H6 provisions it): source, license (UFL-1.0), internal family + PostScript names, version string, SHA-256, face index, `fsType` embedding permissions, CORS behavior, catalog identifier.
- Expected resolution category for all faces: `hash_pinned_exact`. A family/style match without the pinned hash may only be labeled `catalog_version_assumed` (§6A A6).
- **No font binaries are committed to the repository or any package.** Dev/CI obtain them via the configured broker or a seeded local cache — see §F.

## §C End-to-end validation on the real fixture

1. `office_font_requests(1.docx)` emits the deduplicated Ubuntu/Ubuntu Mono face set from every story (body, tables, TOC field results, footnotes) — assert the set matches the ground truth's embedded families.
2. Broker/mock resolves; `office_register_font` validates: name-table identity = Ubuntu (reject imposters), version = 0.83/0.80, SHA matches, `fsType` permits subsetting (UFL does), coverage passes for the document's scripts.
3. Convert: registered exact faces flow through shaping → line metrics → layout → subsetting → `/W` → ToUnicode → selection geometry (no code path changes — assert via the §6A parity/no-`TJ`/selection tests on this fixture).
4. Telemetry: every face reports source=`provider` (or cache on re-run), category=`hash_pinned_exact`; **no `font_fallback_used`**, no substitution warnings for Ubuntu.
5. Negative controls: catalog entry with wrong hash → `font_hash_mismatch`, conversion fails strict; provider `denied` → structured policy result; offline+cold → deterministic `unresolved`.

## §D Fidelity gate

- `cargo run -p fidelity -- run --case gdocs-anchors-toc`: page count 10; **min page SSIM ≥ 0.90**; report per-page scores against the handoff baseline (0.8973 / 0.8114 / 0.6899 / 0.9193 / 0.9222 / 0.9066 / 0.7909 / 0.9889 / 0.8496 / 0.9102); verify line breaks and anchor positions in addition to SSIM (the emitted PDF must embed subsets derived from the fetched Ubuntu bytes — check via the PDF font dictionaries).
- Full corpus afterward: **zero** changes to ground truths, thresholds, goldens, manifests, or rasterization settings; no regression in any passing case (`complex` observed floor 0.9374, `gdocs-columns` 0.9280; `gdocs-tables-longdoc` remains phase 7's gate).
- The harness runs natively (bundled dev fonts for the trio) + Ubuntu from the §F-decided CI source, registered through `ConvertOptions.font_bytes`/`font_dirs` — never committed.

## §E DocKosha follow-ups (cross-repo, recorded for the pin bump — no doc-kosha changes in this repo)

1. Worker boot-registers the OFL trio + CJK assets from the pdf package's `fonts/` directory (§6A C2).
2. Worker configures the broker/provider for tail families (Ubuntu-class) with the persistent cache directory.
3. Router adopts the canonical sequence: requests → resolve/register → profile → convert (§6A B).
4. Pin bump to the release containing 6A+6B.

## §F Open decision (explicitly unresolved — owner picks during implementation)

**CI/dev font provisioning for Ubuntu**, two admissible resolutions (6A's mock and gate accept either without rework):
- **(i) Seeded external cache:** CI provisions the pinned artifacts into the host persistent cache from an owner-controlled store; repo stays binary-free (H6's letter).
- **(ii) Test-only UFL fixtures** under `tests/` excluded from all published artifacts (the §6A D3 gate is the enforcement); simplest hermetic CI, but requires an owner exception to H6's "no binaries in the repository".

Record the choice in this file when made.

## §G Risks

1. Cold-conversion latency for tail families = provider RTT + download on first use (warm path zero-network); surface `fetch duration` in telemetry and budget in DocKosha.
2. Version ambiguity is inherent to DOCX (§6A A6) — the 0.83/0.80 pins are justified by the ground truth's embeds, not by anything in the DOCX.
3. Browser consumers need broker CORS + IndexedDB cache before the web build benefits; Node/worker path is the primary target.
4. If min-page SSIM lands just short despite exact fonts, the residue is geometry, not typography — hand evidence to phase 7's probe; **no threshold/goldens changes, no global offsets.**

## Human task H6 (unchanged in substance — provisions §B; see `human-tasks.md`)

## Local verification

```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
node --test tests/                                             # incl. broker adapter (mock/local server)
cargo run -p fidelity -- run --case gdocs-anchors-toc          # ≥ 0.90, pages 10/10
cargo run -p fidelity -- run --format docx                     # full corpus, zero regressions
DRY_RUN=1 pdf-core/scripts/release.sh                          # combined 6A+6B release preview only
```
