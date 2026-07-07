# 01 — Engine Review (`pdf-core` + `@samarthmn/doc-yantra` wasm package)

## Verified facts

- **Layout:** `pdf-core/` (Rust crate), `pdf-core/wasm/` (bindings + committed `pkg/`), `tests/` (Node suite against built pkg), `mock-files/` (real fixtures), `.github/workflows/publish-wasm.yml` (the ONLY workflow).
- **Features/deps** (`pdf-core/Cargo.toml`): `version 0.0.1`, `edition 2024`, `publish=false`; `default = ["pdfium","cli","parallel","office"]`; gates `pdfium=[dep:pdfium-render]`, `wasm=[lopdf/wasm_js]`, `office=[dep:office2pdf]`. Deps: lopdf 0.42 (no default features), pdf-writer 0.15, pdfium-render 0.9.2 (opt), office2pdf 0.6 (opt), pulldown-cmark 0.13, csv 1.4, image 0.25 (png/jpeg/webp), thiserror 2.
- **Zone gating is correct:** Zone-2 modules (`extract`, `forms`, `raster`, `redact`) behind `#[cfg(feature="pdfium")]`, office behind `#[cfg(feature="office")]`; the wasm crate depends on pdf-core with `default-features=false, features=["wasm"]`, so pdfium/office cannot leak into the main/web wasm builds. The pipeline test is `#![cfg(all(feature="office", feature="pdfium"))]`.
- **Exact wasm surface** (`pkg/pdf_core_wasm.d.ts`):
  ```ts
  apply_watermark(pdf: Uint8Array, watermark_json: string, watermark_image?: Uint8Array | null): Uint8Array
  csv_to_pdf(csv: Uint8Array): Uint8Array
  init(): void
  markdown_to_pdf(markdown: string): Uint8Array
  merge_pdfs(pdfs: Uint8Array[], watermark_json?: string | null, watermark_image?: Uint8Array | null): Uint8Array
  page_count(pdf: Uint8Array): number
  text_to_pdf(text: string): Uint8Array
  validate_pdf(pdf: Uint8Array): string
  version(): string
  ```
  `./office` adds `office_to_pdf(bytes, extension): any`; `./web` is ESM with the same Zone-1 surface plus `initSync`/`__wbg_init`.
- **Error mapping:** Rust `Result::Err` → thrown `JsError` at the boundary; `console_error_panic_hook` installed via `#[wasm_bindgen(start)]`.
- **Watermark payload** (`wasm/src/lib.rs` `WatermarkPayload`, `serde rename_all=camelCase, default`): `pattern, rotationDeg, xSpacingPt, ySpacingPt, offsetXPt, offsetYPt, lines[], color, fontSizePt, opacity, contentType, image{dataUrl,widthPt,heightPt}`; defaults color `#4B5563`, spacing 320, fontSize 14, opacity 0.65, `diagonal_grid` → −40°; core clamps spacing 72–1440, fontSize 8–64, opacity 0.05–1.0, image dims 24–1440 (fallback 180). Matches DocKosha's `buildDocyantraWatermarkPayload` field-for-field.
- **Versions & tags:** `pdf-core/Cargo.toml` = **0.0.1**; `pdf-core/wasm/Cargo.toml` = **0.0.2**; `pkg/package.json` = **0.0.2** (`UNLICENSED`, correct `exports` for `.` / `./office` / `./web` / `./web/*_bg.wasm`, `files` list, `main`/`types`). Tags `wasm-v0.0.1` and `wasm-v0.0.2` exist; release `0.0.2` published 2026-07-04. README + CONSUMING instruct consumers to install **`^0.0.1`**.
- **Publish workflow:** trigger `push: tags: [wasm-v*]` + `workflow_dispatch`; installs Rust + wasm-pack, **rebuilds** via `build.sh`, `npm publish` from `pkg/` with `NODE_AUTH_TOKEN: ${{ secrets.GITHUB_TOKEN }}`, node 22, registry `npm.pkg.github.com`, scope `@samarthmn`, `permissions: {contents: read, packages: write}`. No test step. No other workflow exists.

## Findings

### Y-1 · [BLOCKER] Documented install range `^0.0.1` can never resolve the shipped `0.0.2`

Under npm/pnpm semver, caret on `0.0.x` pins the exact patch: `^0.0.1` matches only `0.0.1`. A consumer following README/CONSUMING today either resolves the older `0.0.1` (whose glue differs from 0.0.2) or fails resolution. DocKosha itself already pins `^0.0.2` — correct — but the docs mislead the next consumer/teammate.
**Fix:** make docs and range agree. Recommended: cut **0.1.0** (caret behaves intuitively on 0.x.y) and document `^0.1.0`; otherwise update every doc to `^0.0.2`. Bump `pdf-core/Cargo.toml`, `wasm/Cargo.toml`, `pkg/package.json`, README, CONSUMING together, re-tag.

### Y-2 · [SHOULD-FIX, high] No CI test gate anywhere; publish ships untested rebuilds

Only workflow is tag-triggered publish, which **rebuilds from source and publishes without running a single test**. The claimed 162 cargo tests (`tests/README.md` says 171 — even the claim disagrees with itself) and the Node package suite run only on developer machines. A regression can reach a `wasm-v*` tag uncaught; a stale committed `pkg/` additionally bites `link:`/git consumers.
**Fix:** add `test.yml` on push/PR: `./scripts/fetch-pdfium.sh && cargo test` (default features), `cargo test --no-default-features --features wasm`, `wasm/build.sh && node --test tests/`, plus `git status --porcelain pkg` to fail if the committed pkg drifted. Make the publish workflow depend on it (or run tests inline before `npm publish`).

### Y-3 · [SHOULD-FIX] Core crate version (0.0.1) out of sync with the package (0.0.2)

`version()` returns the **wasm crate's** version; the release recipe only bumps `wasm/Cargo.toml`, so `pdf-core` rots at 0.0.1. "Which engine version produced this document" should have one answer.
**Fix:** lock the two Cargo versions in the release checklist (or derive one from the other); consider `version()` returning both.

### Y-4 · [SHOULD-FIX] MSRV inconsistent and unenforced

Root README says Rust ≥ 1.88 (twice); `tests/README.md` says ≥ 1.89. Code uses edition 2024 + let-chains (stable 1.88). Nothing enforces it.
**Fix:** pick one (1.89), add `rust-version = "1.89"` to both Cargo.tomls, align docs. Consider pinning the toolchain in the publish workflow for reproducible artifacts (currently floating `stable`).

### Y-5 · [SHOULD-FIX] Panics trap the wasm instance rather than throwing catchable errors

`console_error_panic_hook` only logs; a Rust panic/OOM aborts (unreachable trap) and can poison the instance — it is NOT a `JsError` the consumer's try/catch handles as a normal failure. No obviously reachable panic was found on the Zone-1 input path (the `unreachable!()` in `watermark::free_resource_name` is effectively dead; `.expect` in `generate.rs` is compile-time-constant; font indexing range-guarded; grid anchors bounded via `rem_euclid`) — but large inputs can still OOM-abort.
**Fix:** document the contract explicitly ("a trap is fatal for the instance; recreate it"), and/or wrap entry points so panics become `JsError`; consumers (DocKosha W-6) should treat a trapped instance as recreate-and-fallback.

### Y-6 · [SHOULD-FIX] No input-size guard; wasm32 memory is the failure mode

Inputs are fully copied into linear memory; `merge` holds all inputs + the merged copy simultaneously; `images_to_pdf` embeds raw RGB8 (FlateDecode deferred, per pdf-core README). A big merge set / CSV / image can exhaust wasm32 memory and abort uncatchably.
**Fix:** reject > N MB at the JS boundary with a normal thrown error before entering wasm; document per-op memory expectations; prioritize the deferred image FlateDecode.

### Y-7 · [NIT] `office_to_pdf` returns `any`

The office `.d.ts` types the result as `any`; the real shape is `{ pdf: Uint8Array, warnings: string[] }`.
**Fix:** `#[wasm_bindgen(typescript_custom_section)]` interface (or a hand-written `.d.ts` overlay) so consumers get the shape.

### Y-8 · [NIT] Docs drift + subtle module-format lean

- CONSUMING/README still say "DocKosha currently ships via the sibling `link:` checkout" — stale; DocKosha pins registry `^0.0.2`.
- `pkg/` has no `"type"` field and `web/`/`office/` have no own `package.json` markers; the ESM web build resolves correctly only through the root `exports` conditions. Works for documented usage; a bare Node `require()` of a web path would mis-parse.
- Watermark/text generation silently degrades non-WinAnsi chars to `?` with no warning signal (intentional — DocKosha routes such text away — but a direct consumer gets silent corruption; consider a lossy-encoding warning like office's `warnings[]`).
**Fix:** update docs; add `{"type":"module"}` under `web/` (or `commonjs` under `office/`); optionally surface lossy-encoding warnings.

## What's solid

- Genuine Zone-1/Zone-2 isolation (wasm build cannot pull pdfium/office/rayon).
- PDF-correctness care: `/Encrypt` detection incl. owner-locked/xref-stream trailers, refusal to re-save encrypted docs, spec-compliant metadata encoding, tolerant trailing-bytes parsing.
- Watermark robustness: collision-free resource names on re-watermark, deref/`/Parent` cycle guards, MediaBox-origin-aware anchoring, `rem_euclid`-bounded grid anchors (offset DoS-safe), per-page stream compression; deviations from the TS reference documented with numbers.
- Merge handles inheritable page attributes, AcroForm union, duplicate-object cloning, `/Info` carry-over; outline-drop is deliberate and documented.
- `office_to_pdf` fails safe: zip-signature fast-reject (<2 s asserted in tests), re-validates its own output, scopes to OOXML.
- Node suite tests the **built artifact by path** (mirrors a Vercel consumer) and asserts real behavior — inflates FlateDecode streams to find `(MARK) Tj` rather than "didn't throw".
- Licensing hygiene stated and plausible (lopdf/pdf-writer MIT/Apache-2.0, pdfium BSD via bblanchon binaries fetched per-platform and gitignored, office2pdf pure-Rust/Typst; AGPL PyMuPDF path removed); pkg `UNLICENSED` (private).

## Release-readiness verdict

**Mechanically publishable, documentarily broken.** The workflow, scope, registry, exports map, and committed pkg/ are sound; `wasm-v0.0.2` published 2026-07-04. But a consumer following the repo's own docs (`^0.0.1`) cannot land on it (Y-1), the two crate versions disagree (Y-3), and nothing tests what gets published (Y-2). Clear Y-1/Y-2/Y-3 before the next tag.

## Could not verify (run locally)

1. Committed `pkg/` matches source: `cd pdf-core/wasm && ./build.sh && git status --porcelain pkg` (clean = in sync).
2. Test suites pass + real count: `cd pdf-core && ./scripts/fetch-pdfium.sh && cargo test` and `cargo test --no-default-features --features wasm`; `node --test tests/` from root. Reconcile "162" (README) vs "171" (tests/README).
3. Registry state: `npm view @samarthmn/doc-yantra versions --registry=https://npm.pkg.github.com` from an authed machine.
