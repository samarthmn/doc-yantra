# Agent tasks 6 — on-demand exact-font acquisition (`gdocs-anchors-toc` + fontless package)

> **STATUS: PLANNING PASS ONLY. No implementation has started; every task below awaits owner approval.**
> This file **replaces** `agent-tasks-6-metric-overlay.md` (deleted in the same commit). The metric-overlay design was never implemented in Rust — superseding it removes no code. The following overlay concepts are formally dropped: compiled-in Ubuntu advance tables, `OverlayEntry`, Ubuntu-metrics-over-Carlito outlines, overlay-specific `/W` arrays, overlay kerning suppression, and TTF vendoring as the fidelity contingency.

## Step 0 — commit the stopped remediation first (precondition, unchanged)

The 21-file uncommitted worktree described in `implementation-handoff-2026-07-15/01-current-status.md` must be reviewed, verified (fmt/clippy/`cargo test --locked`/wasm build/Node suite — investigating the two known drifts rather than masking them), and **committed** before phase-6 or phase-7 work begins.

---

## A. Architectural decision summary

The published `@samarthmn/doc-yantra-office` package becomes **fontless**: zero font binaries in the wasm, the JS glue, or the npm tarball — including the currently bundled Carlito/Caladea/Liberation trio (see §B, biggest blast-radius item). When a DOCX references a non-embedded font, the **host** acquires the exact face at runtime through a configurable, batched, asynchronous **font resolver**; the Rust core stays 100% network-free and only *discovers* requests, *validates/registers* supplied bytes, and *converts*. Layout starts only after resolution completes — never a fallback layout followed by font swap (no reflow, no double work). Fonts resolve through a strict priority order (embedded → registered → caches → provider → policy-gated fallback → structured failure), with explicit exactness categories, licensing enforcement from OS/2 `fsType`, content-hash caching outside the wasm, and identical resolution reporting from `profile` and `convert`. For `gdocs-anchors-toc`, a hash-pinned catalog supplies Ubuntu 0.83 / Ubuntu Mono 0.80 — the exact versions the ground truth embeds — restoring true letterforms *and* metrics, targeting min-page SSIM ≥ 0.90.

## B. Core objective and the fontless invariant

The production package must contain none of: `.ttf`/`.otf`/`.ttc`/`.otc`/`.woff`/`.woff2` files, base64 font blobs, generated arrays holding font programs, compiled-in metric tables, family-specific outline carriers, or hidden fallback assets.

**Consequence discovered by repo inspection (owner must ratify explicitly):** the wasm build currently ships the 20 OFL faces via `--features wasm,bundled-fonts` (`build.rs` → `BUNDLED_FONTS` → `FontDb::with_bundled`). Under the invariant, the production wasm build **drops `bundled-fonts`**; Carlito/Caladea/Liberation move to the provider catalog (they are freely distributable — the broker may serve them; hosts may pre-register them). The `fonts/` directory **stays in the repo** for native dev/tests: the native default feature set keeps `bundled-fonts` so `cargo test` and the fidelity harness remain hermetic. The gate (§L) inspects the *published artifacts*, not the source tree. The package-level Node suite migrates to a local mock provider (§K). Compatible-mode substitution targets (`Calibri→Carlito`, …) become catalog entries rather than compiled-in certainties.

## C. Pipeline (normative)

```
DOCX preflight (core, sync)
  → discover unresolved font faces across every story (§E2)
  → deduplicate to canonical face requests (family aliases + weight + italic + stretch + slots)
host orchestration (async)
  → runtime memory cache → persistent cache → provider (batched; in-flight deduped)
  → download exact bytes → verify hash
core registration (sync)
  → validate structure/identity/version/coverage/permissions → register
core conversion (sync)
  → resolve runs against registered faces → shape → lay out → subset → emit
```

Line breaking and pagination MUST NOT begin until resolution has completed for the chosen mode. Embedded fonts never trigger a request. A single document may mix embedded, fetched, cached, and (compatible mode) fallback faces.

## D. Affected files and symbols (from actual inspection @ `8b742cc`)

| File | Symbols touched | Change |
|---|---|---|
| `office-core/src/fonts/db.rs` | `FontDb::{with_bundled, with_system, register, register_embedded}`, `Face`, `MAX_FONT_FACES`, `SYSTEM_FONT_DIRS` | validated registration returning `FontIdentity`; content-hash per face; **system-dir scan becomes opt-in** (§J: no local-font access without host authorization — today `with_system` auto-scans `/usr/share/fonts` etc.) |
| `office-core/src/fonts/matching.rs` | `resolve`, `Resolved`, `substitute`, `fallback_class`, `coverage_fallbacks` | priority order §H; `ResolutionQuality` categories; `substitute`/`fallback_class` gated behind compatible mode; no implicit bundled assumption |
| `office-core/src/fonts/bundled.rs`, `office-core/build.rs` | `bundled_fonts()`, `BUNDLED_FONTS` | feature stays for native dev/tests; **removed from the wasm build line** in `office-core/wasm/build.sh` |
| `office-core/src/lib.rs` | `register_font`, `RUNTIME_FONTS`, `ConvertOptions`, `convert`, `profile` | `font_requests()` preflight API; `register_font_validated(bytes, expectation) -> FontIdentity`; `ConvertOptions { resolution_mode, include_system_fonts: bool /* default false */ , … }` |
| `office-core/src/profile.rs` | `Collector`, `resolve_fonts`, `DocProfile`, `Stats` | emit structured `FontRequest`s (all stories, effective cascade, eastAsia/cs slots); `DocProfile.fonts: Vec<FontResolutionReport>`; parity with convert |
| `office-core/src/oxml/` (new `font_table.rs`) | — | parse `fontTable.xml` metadata (PANOSE, family class, pitch, charset, usb/csb signatures) into request metadata; today the file is only read for ODTTF embeds (`assemble.rs::register_embedded_fonts`, unchanged, stays priority 1) |
| `office-core/src/error.rs` | `UnsupportedCode` | add `FontUnresolved` (add-never-rename policy); strict-mode failures carry the full unresolved-face list in detail |
| `office-core/src/emit/pdf.rs`, `fonts/subset.rs`, `text/shape.rs`, `fonts/metrics.rs` | — | **no mechanical change**: a registered exact face flows through shaping → metrics → subsetting → `/W` → ToUnicode automatically; add the §K regression tests (incl. the existing no-`TJ` invariant) |
| `office-core/wasm/src/lib.rs` | exports | add `office_font_requests(bytes, ext) -> JSON`, `office_register_font(bytes, expectation_json) -> JSON` (validated; existing `register_font` kept as deprecated alias); keep `office_profile`/`office_to_pdf` sync; error envelope (`{dy, code, message, detail}`) gains font event payloads |
| `office-core/wasm/build.sh` + generated JS glue | — | drop `bundled-fonts`; add the async orchestrator + provider/cache adapters to the package JS (hand-written `js/` shipped alongside the wasm-bindgen output) |
| `wasm-worker/worker-client.cjs`, `worker-entry.cjs` | boot font registration (`extraFontDirs`) | becomes the Node host adapter: provider config, FS persistent cache, in-flight dedup, cancellation |
| `tests/office.test.mjs`, `office-core/tests/*` | — | §K matrix; mock provider/local server |
| `office-core/README.md`, `CONSUMING.md` | fonts/envelope sections | modes, provider contract, migration note |

Out of scope, unchanged: `pdf-core/**` (its npm package ships its own `fonts/` — the §L gate applies to the **office** package only), phase-7 files, all fidelity thresholds/goldens/manifests.

## E. Rust core work items

**E1 — Preflight API.** `pub fn font_requests(bytes: &[u8], format: OfficeFormat, opts: &ConvertOptions) -> Result<Vec<FontRequest>>` — runs the existing profile walk (no layout), returning deduplicated canonical requests. `FontRequest { family, alt_families: Vec<String> /* fontTable altName, full-name aliases */, weight: u16, italic: bool, stretch: Option<String>, slots: Vec<Slot /* ascii|hAnsi|eastAsia|cs */>, panose: Option<[u8;10]>, family_class: Option<String>, pitch: Option<String>, unicode_ranges: Option<[u32;4]>, scripts: Vec<ScriptTag>, vf_coords: Option<Vec<(Tag, f32)>> }`. **Never any document text.**

**E2 — Story coverage.** Requests must come from: body, headers, footers, footnotes, endnotes, numbering labels (level `rPr` + the shared `numbering_label_family` cascade), tables (all nesting), text boxes and drawing text, hyperlink runs, field result content, styles/docDefaults/theme cascade (same `StyleResolver` used by conversion — discovery and conversion may never disagree), and the eastAsia/cs slots when the run's script classification selects them. Comments do not paint in print view and are excluded by design (record in the code comment). Dedupe at face level; one request per unique face per document.

**E3 — Modes.** `ResolutionMode::{Strict, Compatible}` on `ConvertOptions` (offline is a host property: it simply configures no provider — the core never knows about networks). Strict: only Embedded/Registered exact faces; anything else → `Err(Unsupported(FontUnresolved))` listing every unresolved face. Compatible: exact first; then the configured fallback path (today's `substitute`/`fallback_class`) with mandatory reporting; never labeled exact. Timeouts/failures in the host must surface as *resolution results*, never as a silent mode change.

**E4 — Validated registration.** `register_font_validated(bytes, Option<FontExpectation>) -> Result<FontIdentity>` where `FontExpectation { family, weight, italic, stretch, version: Option<String>, sha256: Option<[u8;32]>, face_index: Option<u32> }` and `FontIdentity { families, full_name, postscript_name, subfamily, weight, italic, stretch, version, face_count, face_index, sha256, fs_type: EmbeddingPermission, coverage_ok: bool }`. Checks (all local, `ttf_parser`-based): SFNT/OTF structure parses; size ≤ configurable cap (default 10 MiB/face-file); collection face count and requested index valid; name-table identity **matches the expectation** (reject "the URL said Ubuntu but the name table says something else"); version string recorded; SHA-256 equals the catalog's when provided; required script/char coverage per the request; variable axes present when coords requested; OS/2 `fsType` decoded. No WOFF/WOFF2 decoding in the core — the provider contract delivers SFNT bytes (a broker may transcode outside the package).

**E5 — Embedding-permission policy (`fsType`).** Installable/editable/preview-and-print → subset locally and embed (current pipeline). No-subsetting bit set → embed the full font only if policy + size limits permit, else structured policy result. Restricted-license bit → do not embed, do not launder through another format; strict fails, compatible may substitute (reported). Unknown/contradictory → structured policy result, never silent.

**E6 — Exactness categories** (stable, serialized snake_case): `embedded_exact`, `hash_pinned_exact`, `catalog_version_assumed`, `family_style_compatible`, `fallback`, `unresolved`. A family+style match without a known expected version/hash must never be labeled exact — the DOCX does not record font versions; `catalog_version_assumed` is the honest ceiling there.

## F. Host layers (JS package + worker)

**F1 — Provider contract** (deliverable 5; TypeScript-facing):

```ts
interface FontProvider {
  resolve(requests: CanonicalFontRequest[], signal: AbortSignal): Promise<FontResult[]>;
}
type FontResult =
  | { status: "found"; request: CanonicalFontRequest; artifact: FontArtifact }
  | { status: "not_found"; request: CanonicalFontRequest }
  | { status: "denied"; request: CanonicalFontRequest; reason: string }      // license/policy
  | { status: "temporary_failure"; request: CanonicalFontRequest; reason: string };
interface FontArtifact {
  family: string; fullName: string; postscriptName: string;
  weight: number; italic: boolean; stretch?: string;
  version: string; faceIndex: number; format: "ttf"|"otf"|"ttc"|"otc";
  sha256: string; source: string;                    // catalog identifier
  license: { id: string; embeddable: boolean; subsettable: boolean };
  bytes?: Uint8Array; url?: string;                  // signed URL from the broker's manifest
}
```

Batched (one call per document), no document text ever, **no URL derived from document content** — the provider selects from an allowlisted catalog keyed by normalized request. The four statuses are distinct; the orchestrator must not collapse them into "missing". Adapters this abstraction must permit (not built now): user-uploaded fonts, enterprise font servers, Local Font Access (browser, user-authorized), OS font discovery (host-authorized), app-owned assets, private licensed repositories.

**F2 — Reference HTTP broker adapter** (optional, configurable — endpoint, auth headers, timeout, max bytes, redirect limit, parallelism): (1) POST one batched resolution request of canonical metadata; (2) receive a versioned manifest of artifacts (hashes + signed URLs or inline bytes); (3) download unique artifacts; (4) verify SHA-256; (5) hand bytes to registration. HTTPS-only in production; content-type checked; no Google/Microsoft endpoints hardcoded; no undocumented cloud-font services.

**F3 — Cache design** (deliverable 7). Two levels, both outside the wasm: in-memory `Map<sha256, bytes>` per process/session, plus optional persistent cache (browser: IndexedDB; Node worker: a host-configured directory) storing **content-addressed** font bytes and a separate request-index `normalized(family,weight,italic,stretch) + versionPolicy + providerId → sha256`. Behaviors (all tested, §K): cold cache = exactly one provider call per unique face; warm = zero network; concurrent conversions share one in-flight promise per face; offline+warm succeeds; offline+cold yields deterministic `unresolved`; catalog hash change invalidates the request-index entry (content store untouched); corrupt cached bytes (hash mismatch on read) are evicted and re-resolved; negative results cached briefly (configurable TTL) against request storms. Nothing font-shaped is ever emitted into the wasm module or generated JS.

**F4 — Public API and migration** (deliverable: §15). Today's package API is synchronous CJS (`office_profile`, `office_to_pdf`, `register_font`) — documented as stable in CONSUMING.md. Smallest safe migration, no hidden async:
- Sync exports remain and keep working whenever all needed fonts are embedded or already registered (strict/offline usage).
- New sync exports: `office_font_requests`, `office_register_font` (validated).
- New **async convenience in the JS glue**: `convertOnDemand(bytes, ext, { provider, mode, memoryCache, persistentCache, signal, limits }) → Promise<Uint8Array>` orchestrating preflight → caches → provider → register → convert. The advanced two-stage flow (preflight; then register+convert) is these same exports used directly.
- `wasm-worker/worker-client.cjs` (DocKosha's actual entry point — already async) gains the provider/cache configuration; `extraFontDirs` boot registration remains as an app-owned-assets adapter.
- Native Rust and the CLI stay deterministic and network-free; the CLI gains `--font-dir`/`--system-fonts` flags only (explicit host authorization).
- Consumer impact is additive; DocKosha's `officeCoreWasm.ts` adapter change is recorded as a doc-kosha follow-up (outside this repo, next pin bump).

## G. Runtime registration contract (deliverable 6)

Host calls `office_register_font(bytes, expectationJson)`; the core validates per E4 and returns the `FontIdentity` JSON (or the structured error envelope). Registered faces enter the same process-global registry `register_font` uses today and match through the **exact-family path** (`pick_in_family`) — fetched fonts are ordinary first-class faces, not a parallel mechanism. Determinism contract restated: output is a pure function of *(document bytes, registered font set, options)*; the resolution report records the font-set hashes so any output is reproducible.

## H. Resolution order (normative)

1. Font embedded in the DOCX (ODTTF path — unchanged, never triggers any request)
2. Exact runtime-registered face
3. Exact face in host memory cache → registered on demand
4. Exact face in host persistent cache → registered on demand
5. Exact face from the configured provider
6. Explicit compatible fallback (compatible mode only; reported)
7. Structured `FontUnresolved` failure (strict) / reported fallback absence (compatible)

## I. Error and telemetry model (deliverable 8)

Per requested face, both `DocProfile.fonts[]` and the conversion report carry: requested family+style, effective OOXML slot(s), selected identity (family/PS name/version/hash), resolution source (`embedded|registered|memory_cache|persistent_cache|provider|fallback|none`), cache status, fetch duration (host-filled), exactness category (E6), embedding-permission result, coverage result, failure reason, fallback identity. Event taxonomy (host telemetry): `font_embedded`, `font_runtime_registered`, `font_memory_cache_hit`, `font_persistent_cache_hit`, `font_fetched`, `font_fetch_failed`, `font_not_found`, `font_identity_mismatch`, `font_hash_mismatch`, `font_license_restricted`, `font_version_assumed`, `font_fallback_used`. A successful exact fetch is telemetry, not a warning; fallback and unresolved are visible warnings/errors. Profile and convert must report identically given the same registered-font state (parity test, §K).

## J. Security requirements (mapped to layers)

Host/broker: HTTPS-only in production; configurable timeout, max font size, redirect limit (≤3), bounded parallelism, CORS documented for browser use, content-type checks, SHA-256 verification, cancellation via `AbortSignal` when conversion aborts, negative-cache backoff against repeated failures, no SSRF-prone server-side resolver (allowlisted catalog only), never any document text in requests. Core: hardened parsing (`ttf_parser` is already panic-averse; fuzz the registration path in tests), face-count/size limits (`MAX_FONT_FACES` today), **no automatic local-font access**: `FontDb::with_system`'s OS-directory scan becomes opt-in (`include_system_fonts`/`--system-fonts`), defaulting off everywhere. No arbitrary URL derived from document content anywhere.

## K. Test matrix (deliverable 9)

*Core (Rust):* request extraction per story (body/header/footer/footnote/endnote/numbering/table/textbox/hyperlink/field-result); style+theme cascade equivalence with conversion; alias normalization; weight/italic/stretch distinction; face-level dedupe; validated registration happy path; identity mismatch rejection; version + SHA mismatch; corrupt/truncated/oversized/unsupported-format rejection; TTC face-index handling; `fsType` matrix (all four E5 branches); coverage failure; VF coords (accept static instance / reject missing axes); strict-mode structured failure lists all faces; compatible-mode fallback reporting; profile/convert parity; exact fetched font drives shaping + metrics + `/W` + ToUnicode + selection geometry; no-`TJ` invariant preserved; no fallback after exact registration.
*Host (Node, mock provider / local server — CI never touches a public CDN):* cold-cache single request per face; warm-cache zero network; in-flight dedupe under concurrency; offline+warm success; offline+cold deterministic failure; timeout → `temporary_failure` (mode unchanged); `not_found` vs `denied` distinct handling; catalog-hash rotation invalidates; corrupt persistent entry evicted; abort cancels fetches.
*Fixtures:* test-only font assets (incl. Ubuntu, UFL-permitting) live under test fixtures **only if** excluded from every production artifact — the §L gate is the enforcement; otherwise the mock provider serves them from an untracked local cache (H6).

## L. Package-size and fontless-artifact gate (deliverable 10)

New release-blocking check (extend `tests/release-contract.test.mjs` or a sibling) that inspects the **packed artifact** (`npm pack` output of `office-core/wasm/pkg`, not the source tree): fail on any `.ttf/.otf/.ttc/.otc/.woff/.woff2` entry; scan every packed file (including the `.wasm` binaries and JS) for font magic bytes (`00 01 00 00`, `OTTO`, `ttcf`, `wOFF`, `wOF2`) both raw and after decoding base64-looking spans; fail on generated arrays representing font programs and on any resurrected metric-database symbols. Record wasm sizes before/after phase 6 in the PR description — resolution code may grow the package slightly; font payload may not. (Scope: the **office** package; `@samarthmn/doc-yantra` (pdf) ships its own fonts by design and is out of scope.)

## M. Fidelity gates (§18)

- `gdocs-anchors-toc`: provider resolves **hash-pinned Ubuntu 0.83 (R/B/I/BI) + Ubuntu Mono 0.80** (styles per the document's actual demand from preflight); the emitted PDF embeds subsets derived from those bytes; page count stays 10; **min page SSIM ≥ 0.90**; report per-page scores against the handoff baseline (0.897/0.811/0.690/0.919/0.922/0.907/0.791/0.989/0.850/0.910); verify line breaks and anchor positions, not just SSIM.
- Full corpus afterward with **zero** changes to ground truths, thresholds, goldens, manifests, or rasterization settings; no regression in passing cases. Native harness/tests keep the `bundled-fonts` feature (dev builds), so existing cases remain hermetic; the package-level suite uses the mock provider to supply the OFL trio + Ubuntu.

## N. Human task H6 (revised — replaces metrics extraction; mirrored in `human-tasks.md`)

Provision legal, versioned **broker catalog entries** for Ubuntu 0.83 Regular/Bold/Italic/BoldItalic and the Ubuntu Mono 0.80 styles the fixture demands. Per artifact record: source, license (UFL-1.0), internal family + PostScript names, version string, SHA-256, face index, `fsType` embedding permissions, CORS behavior of the serving endpoint, broker catalog id. **No font binaries committed to the repository or the npm package**; dev/CI retrieve through the configured broker or a local untracked cache.

## O. Scope boundaries, risks, unresolved assumptions (deliverable 12)

**Must not:** reconstruct outlines from PANOSE; call metric similarity "exact"; bundle fallback families into wasm; scrape undocumented cloud-font services; depend on Microsoft/Google infrastructure; send document text; build a font marketplace; touch table geometry or phase 7's methodology; introduce global calibration offsets. When the provider cannot legally supply a font, strict mode fails honestly.

**Risks / open items for owner sign-off:**
1. **OFL-trio removal blast radius** (§B) — package tests, DocKosha's worker config, and compatible-mode behavior all shift from "always available" to "provider/catalog-supplied". Ratify explicitly.
2. **CI font provisioning** — H6 forbids committing binaries; CI for the anchors-toc gate and the package suite therefore needs a seeded local cache or reachable broker. Options (pick one at implementation): CI-provisioned artifact store, or UFL test fixtures under `tests/` excluded from artifacts (the gate proves exclusion) — the latter contradicts H6's letter and needs an owner exception if chosen.
3. **Determinism restated** — output now depends on the registered font set; reproducibility requires recording font hashes (§G). The Node deterministic-output test keys on (input, font set).
4. **First-conversion latency** — cold-cache fetch adds provider RTT + download to the first conversion per face; budget and surface in telemetry (`fetch duration`), warm path unchanged.
5. **DocKosha adapter change** is out of this repo (recorded for the pin-bump follow-up).
6. **Version ambiguity is inherent** — DOCX never records the author's font version; `catalog_version_assumed` is the honest label (§E6). The catalog pins 0.83/0.80 for this fixture because the ground truth embeds them.

## P. References updated after the rename (deliverable 13)

- `agent-tasks-7-table-geometry.md` — step-0 pointer now names this file (scope/methodology untouched).
- `README.md` — phase table row 6.
- `human-tasks.md` — H6 replaced (metrics extraction → broker provisioning).
- `implementation-handoff-2026-07-15/README.md` — continuation pointer.
- `implementation-handoff-2026-07-15/02-blockers-and-owner-actions.md` — Resolutions §1 amended (overlay superseded by on-demand acquisition; same-day owner decision).
- The old `agent-tasks-6-metric-overlay.md` is deleted from the branch.

## Local verification (when implementation is approved and lands)

```bash
cd office-core && cargo test && cargo clippy --all-targets -- -D warnings
cd wasm && ./build.sh && cd ../..                      # fontless production build
node --test tests/                                     # incl. mock-provider matrix + fontless gate
cd office-core && cargo run -p fidelity -- run --case gdocs-anchors-toc   # ≥ 0.90
cargo run -p fidelity -- run --format docx             # full corpus, zero regressions
```
