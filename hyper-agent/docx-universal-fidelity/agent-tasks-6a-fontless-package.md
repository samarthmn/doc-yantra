# Agent tasks 6A — fontless package + host font-resolution architecture

> **STATUS: planning complete (owner-approved restructure, 2026-07-15). Implementation not started.** This file and `agent-tasks-6b-on-demand-ubuntu.md` together replace `agent-tasks-6-on-demand-fonts.md` (deleted; its design is preserved, re-bucketed, and amended per owner review). 6A is a **pure infrastructure migration with zero fidelity movement**; 6B exercises the architecture for real (Ubuntu) and owns the `gdocs-anchors-toc` gate.

## ⚠️ Release coupling rule (non-negotiable)

**No `0.0.x` release may be tagged between 6A and 6B**, and the release containing them additionally gates on the DocKosha worker being provider/boot-registration configured (cross-repo follow-up, recorded for the pin bump). A published fontless package without host font supply silently breaks every Calibri/Cambria/Arial document that converts today. `release.sh` is not run until 6B's gates pass.

## Step 0 — commit the stopped remediation first (precondition, unchanged)

The 21-file uncommitted worktree in `implementation-handoff-2026-07-15/01-current-status.md` must be reviewed, verified (fmt/clippy/`cargo test --locked`/wasm build/Node suite — investigating the two known drifts, not masking them), and committed before 6A/6B/7 work begins.

## 6A completion invariant (the checkpoint)

1. The packed `@samarthmn/doc-yantra-office` artifacts contain **zero font binaries** (gate, §D3).
2. Every existing test passes: native suites unchanged (native dev/test builds keep `bundled-fonts`), package Node/wasm suites migrated to the deterministic mock provider.
3. The fidelity corpus scores **byte-identically** to pre-6A (native harness untouched) — any fidelity movement later is attributable to 6B alone.
4. No new fonts, no Ubuntu, no fixture-gate changes — those are 6B.

---

## §A Core API work (`office-core`, network-free)

All references from inspection @ `8b742cc`; re-locate by symbol after Step 0.

**A1 — Preflight.** `pub fn font_requests(bytes, format, &ConvertOptions) -> Result<Vec<FontRequest>>`: the profile walk (no layout) emitting deduplicated canonical requests. `FontRequest { family, alt_families, weight, italic, stretch, slots (ascii|hAnsi|eastAsia|cs), panose, family_class, pitch, unicode_ranges, scripts, vf_coords }` — metadata only, **never document text**. New `oxml/font_table.rs` parses `fontTable.xml` metadata (PANOSE/pitch/charset/sig); the ODTTF embed reader in `assemble.rs::register_embedded_fonts` is untouched. Story coverage = body, headers, footers, footnotes, endnotes, numbering labels (incl. `numbering_label_family` cascade), tables at all nesting, text boxes/drawing text, hyperlink runs, field results, styles/docDefaults/theme cascade via the same `StyleResolver` conversion uses, eastAsia/cs slots per script classification. Comments excluded by design (never paint in print view — code comment records this). One request per unique face.

**A2 — Validated registration.** `register_font_validated(bytes, Option<FontExpectation>) -> Result<FontIdentity>`; `FontExpectation { family, weight, italic, stretch, version?, sha256?, face_index? }`; `FontIdentity { families, full_name, postscript_name, subfamily, weight, italic, stretch, version, face_count, face_index, sha256, fs_type: EmbeddingPermission, coverage_ok }`. Local `ttf_parser` checks: SFNT/OTF parses; size cap (default 10 MiB, configurable); TTC count/index valid; **name-table identity matches the expectation** (a face labeled "Ubuntu" whose name table says otherwise is rejected); version recorded; SHA-256 equality when expected; script/char coverage per request; VF axes when coords requested; OS/2 `fsType` decoded. No WOFF/WOFF2 in the core — providers deliver SFNT. Registered faces join the existing registry and match through the ordinary exact-family path (`matching.rs::pick_in_family`) — no parallel font mechanism.

**A3 — Embedding-permission policy (`fsType`).** Installable/editable/preview-print → subset + embed (current pipeline). No-subsetting → embed whole font only within policy/size limits, else structured policy result. Restricted → never embed, never launder through another format; strict fails, compatible substitutes (reported). Unknown/contradictory → structured result, never silent.

**A4 — Modes.** `ResolutionMode::{Strict, Compatible}` on `ConvertOptions` (offline is a host property: configure no provider). Strict: embedded/registered exact only; otherwise `Err(Unsupported(FontUnresolved))` (**new `UnsupportedCode::FontUnresolved`**, add-never-rename) listing every unresolved face. Compatible: exact first, then the existing `substitute()`/`fallback_class()` ladder — which becomes compatible-mode-only — with mandatory reporting; fallback output is never labeled exact. Host timeouts/failures surface as resolution results, never as silent mode changes.

**A5 — Resolution order (normative).** (1) DOCX-embedded → (2) runtime-registered exact → (3) host memory cache → (4) host persistent cache → (5) configured provider → (6) compatible fallback (mode-gated, reported) → (7) structured `FontUnresolved`. Embedded fonts never trigger a request. Mixed sources within one document are normal.

**A6 — Exactness categories** (stable snake_case): `embedded_exact`, `hash_pinned_exact`, `catalog_version_assumed`, `family_style_compatible`, `fallback`, `unresolved`. DOCX never records the author's font version — an unpinned family/style match is at best `catalog_version_assumed`, never "exact".

**A7 — System-font scan becomes opt-in.** `FontDb::with_system` currently auto-scans OS directories (`SYSTEM_FONT_DIRS`, `$HOME` font dirs) on native. Add `ConvertOptions.include_system_fonts: bool` (default **false**) and CLI `--system-fonts`; no local-font access without explicit host authorization.

## §B Router sequence re-spec (profile ↔ registration ordering)

DocKosha calls `office_profile()` **before** any registration today. On a fontless instance that weakens glyph verification and fails CJK closed pre-registration. The canonical sequence becomes:

```
office_font_requests(bytes, ext)        // 1. discover
→ host resolves + office_register_font  // 2. supply (cache/provider/boot assets)
→ office_profile(bytes, ext)            // 3. verdict (now font-aware)
→ office_to_pdf(bytes, ext)             // 4. convert
```

Contract: **profile and convert report identical font resolution given the same registered-font state** (parity test). Document in CONSUMING.md that a profile taken before registration is provisional for font-dependent reasons; DocKosha's adapter change is a cross-repo follow-up (see release rule).

## §C Host layers (JS package + worker)

**C1 — Provider abstraction** (lands in 6A because the test suite needs it):

```ts
interface FontProvider { resolve(requests: CanonicalFontRequest[], signal: AbortSignal): Promise<FontResult[]>; }
type FontResult =
  | { status: "found"; request; artifact: FontArtifact }
  | { status: "not_found"; request }
  | { status: "denied"; request; reason: string }
  | { status: "temporary_failure"; request; reason: string };
interface FontArtifact { family; fullName; postscriptName; weight; italic; stretch?;
  version; faceIndex; format: "ttf"|"otf"|"ttc"|"otc"; sha256; source;
  license: { id; embeddable; subsettable }; bytes?: Uint8Array; url?: string; }
```

Batched (one call per document); no document text; **no URL derived from document content** (allowlisted catalog / trusted callback only); four statuses handled distinctly. The abstraction must admit future adapters: user uploads, enterprise font servers, Local Font Access (user-authorized), OS discovery (host-authorized), app-owned assets, private licensed repositories. **The reference HTTP broker adapter itself is 6B.**

**C2 — Production font-asset policy (owner revision, 2026-07-15: minimal local fonts).** The worker keeps exactly **one local family**: the Noto Sans fallback (four static faces — see 6B §B), boot-registered at start and used only when font loading fails. Everything else — including Carlito/Caladea and the other substitution targets — is **fetched on demand** through the configured provider (6B) and persisted in the content-hash cache, so in steady state fetched families behave as local. Compatible-mode substitution targets must therefore be *fetchable* names (provider alias table, 6B §A1). `wasm-worker/worker-client.cjs`/`worker-entry.cjs` implement fallback boot registration through `office_register_font` and carry provider config, FS persistent cache, in-flight dedup, and `AbortSignal` cancellation. (The earlier "boot-register the OFL trio from pdf-package assets" default is superseded; it remains available to any host as an ordinary app-owned-assets adapter choice.)

**C3 — Caches** (host-side only; nothing font-shaped in wasm or generated JS). Memory `Map<sha256, bytes>` per process/session + optional persistent store (browser IndexedDB; Node directory), content-addressed, with a request-index `normalized(family,weight,italic,stretch)+versionPolicy+providerId → sha256`. Behaviors (tested): cold = one provider call per unique face; warm = zero network; concurrent conversions share one in-flight promise; offline+warm succeeds; offline+cold → deterministic `unresolved`; catalog hash rotation invalidates the index entry; corrupt entry (hash mismatch) evicted and re-resolved; brief negative-result TTL against request storms.

**C4 — Public API & migration.** Sync CJS exports remain (`office_profile`, `office_to_pdf`, `register_font` kept as deprecated alias) and work whenever fonts are embedded/registered. New sync exports: `office_font_requests`, `office_register_font` (validated, returns `FontIdentity` JSON via the existing `{dy, code, message, detail}` envelope). New async JS-glue convenience: `convertOnDemand(bytes, ext, { provider, mode, memoryCache, persistentCache, signal, limits })`. Advanced consumers use the staged sync exports directly. Native/CLI stays deterministic and network-free (`--font-dir`, `--system-fonts` flags only).

## §D Build, packaging, gate

**D1 — Features.** `office-core/wasm/build.sh` drops `bundled-fonts` from the production build line (`--no-default-features --features wasm`); native default features keep `bundled-fonts` so `cargo test` and the fidelity harness stay hermetic. `office-core/fonts/**` remains in the repo (dev/native only).

**D2 — Size.** Record wasm sizes before/after in the PR description; resolution code may grow the package slightly, font payload may not (expected net: ~11 MB → materially smaller).

**D3 — Fontless-artifact gate** (release-blocking; extend `tests/release-contract.test.mjs` or sibling): inspect the **packed artifact** (`npm pack` of `office-core/wasm/pkg`): fail on any `.ttf/.otf/.ttc/.otc/.woff/.woff2` entry; scan every packed file (both `.wasm` binaries and JS) for sfnt magics (`00 01 00 00`, `OTTO`, `ttcf`, `wOFF`, `wOF2`) raw **and** after decoding base64-candidate spans; fail on generated arrays representing font programs. Scope: the office package only (`@samarthmn/doc-yantra` ships its own fonts by design).

## §E Test migration (deterministic mock provider)

- Build a **mock FontProvider** (in-repo test helper + local HTTP variant for adapter tests) serving fixtures deterministically; CI never touches a public CDN.
- Migrate `tests/office.test.mjs` + worker suites to obtain the OFL trio through the deterministic mock provider (local fixture bytes) so every existing package test passes unchanged in behavior.
- **Determinism re-keyed (owner amendment):** output is now a function of *(document bytes, registered font set, options)*. The Node deterministic-hash test pins the mock provider's font set (record font SHA-256s beside the expected output hash); the conversion/telemetry report includes the registered-set hashes so any output is reproducible.
- Core matrix (Rust): request extraction per story; cascade equivalence with conversion; alias normalization; weight/italic/stretch distinction; dedupe; validated-registration happy path; identity/version/SHA mismatch rejections; corrupt/truncated/oversized/unsupported-format rejection; TTC face-index; `fsType` matrix (all §A3 branches); coverage failure; VF coords; strict-mode structured failure lists all faces; compatible-mode reporting; profile/convert parity; registered exact face drives shaping+metrics+`/W`+ToUnicode+selection; **no-`TJ` invariant preserved**; no fallback after exact registration.
- Host matrix (Node, mock): cold single-request per face; warm zero-network; in-flight dedupe; offline+warm success; offline+cold deterministic failure; timeout → `temporary_failure` (mode unchanged); `not_found` vs `denied` distinct; hash-rotation invalidation; corrupt-entry eviction; abort cancels fetches.
- **CI font decision stays explicitly open** (test-only UFL fixtures excluded-from-artifacts vs seeded external cache): 6A's gate and mock are written so either resolution drops in without rework; the decision is owned by 6B/H6.

## §F Security & telemetry

Security: HTTPS-only remote providers in production; configurable timeout, max font size, redirect limit (≤3), bounded parallelism; CORS documented for browser use; content-type checks; SHA-256 verification; hardened parsing (fuzz the registration path); `AbortSignal` cancellation; negative-cache backoff; no SSRF-prone server resolver; no document text to providers; no arbitrary URL from document content; no local-font access without authorization (§A7).

Telemetry (identical from profile and convert): per requested face — requested family+style, effective slot(s), selected identity (family/PS/version/hash), source (`embedded|registered|memory_cache|persistent_cache|provider|fallback|none`), cache status, fetch duration (host-filled), exactness category, embedding-permission result, coverage result, failure reason, fallback identity. Events: `font_embedded`, `font_runtime_registered`, `font_memory_cache_hit`, `font_persistent_cache_hit`, `font_fetched`, `font_fetch_failed`, `font_not_found`, `font_identity_mismatch`, `font_hash_mismatch`, `font_license_restricted`, `font_version_assumed`, `font_fallback_used`. Exact fetches are telemetry; fallback/unresolved are visible warnings/errors.

## §G Doc/build touchpoints (owner amendment — complete list)

`office-core/wasm/build.sh` (feature line) · root `README.md` and `AGENTS.md` (both print the wasm build command and the bundled-fonts contract) · `CONSUMING.md` (new API surface, canonical §B sequence, `extraFontDirs`/boot-registration recommendation, provisional-profile caveat, mode docs) · `office-core/README.md` (fonts section, envelope wording "bundled metric-compatible fonts" → provider-supplied) · `office-core/fonts/README.md` (dev-only status) · `tests/README.md` (mock provider) · `pdf-core/scripts/release.sh` — behavior note only: its Node-suite step must run with the mock-provider config; **no pdf-core source changes**. `publish-wasm.yml` verified unchanged.

## §H Affected files/symbols

Unchanged from the superseded spec's inspection map: `fonts/db.rs` (`with_bundled`/`with_system`/`register`/`register_embedded`, `Face`, `MAX_FONT_FACES`), `fonts/matching.rs` (`resolve`, `Resolved`, `substitute`, `fallback_class`), `fonts/bundled.rs` + `build.rs`, `lib.rs` (`register_font`, `RUNTIME_FONTS`, `ConvertOptions`), `profile.rs` (`Collector`, `resolve_fonts`, `DocProfile.fonts[]`), new `oxml/font_table.rs`, `error.rs` (`FontUnresolved`), `wasm/src/lib.rs` (+ JS glue), `wasm-worker/*.cjs`, `tests/*`. Mechanically untouched: `emit/pdf.rs`, `fonts/subset.rs`, `text/shape.rs`, `fonts/metrics.rs` (registered exact faces flow through automatically), `pdf-core/**`, phase-7 files.

## Local verification (6A checkpoint)

```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
cd wasm && ./build.sh && cd ../..                 # fontless production build
node --test tests/                                # mock-provider suites + fontless gate green
cd office-core && cargo run -p fidelity -- run --format docx   # byte-identical scores vs pre-6A
```
