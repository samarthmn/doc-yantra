# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this repo is

`doc-yantra` is DocKosha's proprietary **PDF engine** (`pdf-core/`, Rust) plus its
distributable **npm package** `@samarthmn/doc-yantra` (`pdf-core/wasm/`, the Zone-1
engine compiled to WebAssembly). Consumers install the package and run PDF ops
in-process — no Rust toolchain, no network hop.

**Not in this repo:** the DocYantra HTTP service (NestJS + Gotenberg/LibreOffice/
Chromium/pdfium) still lives in the DocKosha app repo at `doc-kosha/doc-yantra/`.
It keeps serving the heavy features (arbitrary HTML→PDF, redaction) and is the
fallback for everything the wasm package covers. When you see references to the
"service" or an HTTP fallback, that code is elsewhere.

## Zone 1 vs Zone 2 — the core architectural split

Every module belongs to one zone, and the zone dictates where it can run:

- **Zone 1 — pure Rust** (lopdf + pdf-writer): `assembly`, `watermark`, `meta`,
  `generate` (csv/md/text/image→PDF). Lock-free, parallel-safe, and the **only**
  zone that compiles to wasm. This is what ships in the npm package. (Office →
  PDF is NOT in this engine — it is served by the DocYantra LibreOffice HTTP
  service.)
- **Zone 2 — Rust + pdfium** (bundled BSD dylib): `raster`, `extract`, `redact`,
  `forms`. **Native-only** — needs the pdfium dynamic library, cannot run in
  wasm. Gated behind the `pdfium` cargo feature (`#[cfg(feature = "pdfium")]` in
  `src/lib.rs`). In production these are served by the HTTP service, not the package.

All engine functions take and return `&[u8]`/`Vec<u8>` PDF bytes, keeping the crate
IO-agnostic and embeddable.

## The npm package has two entry points (one crate, two builds)

`pdf-core/wasm/build.sh` produces `pkg/` from Zone 1 only. The wasm crate
(`pdf-core/wasm/src/lib.rs`) is a thin wasm-bindgen wrapper whose watermark JSON
payload is a **faithful port of `parseWatermark`** from the service's
`pdf.service.ts` — so callers switch between the HTTP service and this package
without reshaping data.

| Import | Build | Contents |
| --- | --- | --- |
| `@samarthmn/doc-yantra` | Node CJS (`--target nodejs`) | watermark/merge/csv/md/text/validate (~1.7 MB) |
| `@samarthmn/doc-yantra/web` | Browser ESM (`--target web`) | Zone 1 for in-browser previews |

## office-core — the Office → PDF engine (separate crate)

`office-core/` is a second standalone crate: pure-Rust DOCX/PPTX/XLSX → PDF, with
its own **fail-closed envelope** (`convert()` returns `Unsupported{code}` off-
envelope) and **DocProfile** router contract (`profile()` predicts the verdict
without layout). It reuses none of pdf-core's code and does not modify it (the
fidelity harness consumes pdf-core read-only for rasterization). It compiles to
wasm (`--no-default-features --features wasm,bundled-fonts`) and ships as
`@samarthmn/doc-yantra-office`. See `office-core/README.md`.

**Versioning now spans four Cargo.tomls** kept in lockstep by
`pdf-core/scripts/release.sh`: `pdf-core/Cargo.toml`, `pdf-core/wasm/Cargo.toml`,
`office-core/Cargo.toml`, `office-core/wasm/Cargo.toml`. One `0.0.*` tag publishes
both npm packages via `publish-wasm.yml`.

## Build / test / run commands

Rust engine (`cd pdf-core`):
```bash
./scripts/fetch-pdfium.sh          # ONE-TIME: fetches pdfium dylib for Zone 2 tests
cargo test                         # full suite (Zone 1 + Zone 2)
cargo test --test watermark_test   # a single test file
cargo test page_count              # a single test by name substring
cargo build --release              # CLI binary at target/release/pdfcore
```
pdfium resolution order: `PDFIUM_DYNAMIC_LIB_PATH` → crate-local `pdfium/lib/` → system.

npm package suite (from repo root — validates the **built** `pkg/`, no install):
```bash
node --test tests/                 # requires pkg/ built first (see below)
```

Rebuild the wasm package after any Rust change (`cd pdf-core/wasm`):
```bash
./build.sh                         # rebuilds all three targets into pkg/
```

## Releasing (0.0.x line)

Releases stay on `0.0.x` — **only the patch increments**. Use the script; it
derives the next `x` from the highest `0.0.*` git tag, writes both
Cargo.tomls, rebuilds `pkg/`, commits, tags, and pushes (the `publish-wasm.yml`
Action then publishes to GitHub Packages on the tag):
```bash
pdf-core/scripts/release.sh          # auto: next patch after the highest tag
pdf-core/scripts/release.sh 7        # explicit: release 0.0.7
DRY_RUN=1 pdf-core/scripts/release.sh  # preview, change nothing
```
It refuses to reuse or go below an existing tag (published versions are immutable).
Run it from a clean-enough tree — it does `git add pdf-core` and commits the version
bump plus the regenerated `pkg/` together.

## Rules that are easy to get wrong

- **`pdf-core/wasm/pkg/` is committed and must stay in sync.** Vercel/CI have no
  Rust toolchain, so the built package is checked in. After changing Rust code,
  run `./build.sh` and commit the regenerated `pkg/`. The Node test suite loads
  `pkg/` by path, so stale `pkg/` = stale tests.
- **Version lives in TWO Cargo.tomls** — `pdf-core/Cargo.toml` and
  `pdf-core/wasm/Cargo.toml` — plus both `Cargo.lock`s and the built
  `pkg/package.json`; keep all of them on the same `0.0.x`. The package's
  `version()` returns the wasm crate's. `pdf-core/scripts/release.sh` bumps them
  for you (via the Cargo.tomls + `build.sh`). Published versions are **immutable**.
- **Releasing** is tag-driven and stays on the `0.0.x` line — just run
  `pdf-core/scripts/release.sh` (see the Releasing section above). It picks the
  next patch, rebuilds/commits `pkg/`, tags `0.0.x`, and pushes; the
  `publish-wasm.yml` Action publishes to GitHub Packages on `0.0.*` tags.
- **GitHub Packages requires the npm scope to equal the repo owner.** If the owner
  isn't `samarthmn`, change `OWNER`/`SCOPE`/`PKG_NAME` in `build.sh`, the `scope:`
  in `publish-wasm.yml`, and the consumer's `.npmrc`. See `CONSUMING.md`.

## Behavioral contracts to preserve when editing

- **pdfium is not thread-safe.** All Zone 2 work must go through
  `crate::pdfium::lock()` (a process-wide mutex); Zone 2 ops serialize per process.
  Zone 1 stays lock-free and parallel.
- **Redaction fails closed.** `redact()` returns `Unsupported` rather than ever
  emitting output where secret text could survive. It removes text objects
  (unextractable, not just covered), recurses into Form XObjects, deletes
  intersecting annotations, and **re-verifies the saved output**. Object-level
  granularity is intentional (more aggressive than character-level = safe direction).
  Don't loosen these guarantees.
- **wasm panic/trap contract.** At the wasm boundary, `Err → JsError` is
  recoverable (bad input, oversized rejection); a Rust `panic!`/OOM is an
  **uncatchable trap** that poisons the instance. Zone 1 exports reject oversized
  inputs (~100 MiB/doc, ~200 MiB/merge) as a normal `JsError` before touching
  linear memory. See `CONSUMING.md` § "Panic / trap contract".
- **Fonts are standard-14 only** (WinAnsi/CP1252). Non-Latin-1 text becomes `?`;
  consumers route such text to the HTTP service. Full Unicode needs embedded fonts (deferred).

## Cargo features

`default = ["pdfium", "cli", "parallel"]`. `pdfium` gates Zone 2; `cli` gates the
`pdfcore` binary; `parallel` enables lopdf's rayon (native only — thread pools
panic on wasm); `wasm` routes getrandom to the JS crypto backend. **wasm builds
use `--no-default-features --features wasm`**, which drops Zone 2 and rayon.

## Docs worth reading

`README.md` (repo overview + release flow), `pdf-core/README.md` (engine scope,
speed table, design notes & known limitations), `CONSUMING.md` (install/auth flow
for consumers + full panic/trap contract), `tests/README.md` (both test layers).
Tests run against the shared `mock-files/` corpus (real docx/pptx/xlsx/csv/md/pdf).
Requires Rust ≥ 1.89 and Node ≥ 18 (stable test runner).
