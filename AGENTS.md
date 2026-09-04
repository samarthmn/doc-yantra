# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## What this repo is

`doc-yantra` is DocKosha's AGPL-3.0-only **PDF engine** (`pdf-core/`, Rust) plus its
distributable **npm package** `@samarthmn/doc-yantra` (`pdf-core/wasm/`, the Zone-1
engine compiled to WebAssembly). Consumers install the package and run PDF ops
in-process — no Rust toolchain, no network hop.

The wasm packages are the supported conversion path; operations the engines
cannot prove fail closed with an `unsupported:*` error.

## Zone 1 vs Zone 2 — the core architectural split

Every module belongs to one zone, and the zone dictates where it can run:

- **Zone 1 — pure Rust** (lopdf + pdf-writer): `assembly`, `watermark`, `meta`,
  `generate` (csv/md/text/image→PDF), `redact_pure`. Lock-free, parallel-safe,
  and the only zone that compiles to wasm. This is what ships in the npm package.
- **Zone 2 — Rust + pdfium** (bundled BSD dylib): `raster`, `extract`, legacy
  `redact`, `forms`. **Native-only** — needs the pdfium dynamic library, cannot run in
  wasm. Gated behind the `pdfium` cargo feature (`#[cfg(feature = "pdfium")]` in
  `src/lib.rs`). Legacy redaction remains for native cross-validation; the
  package ships `redact_pure`.

All engine functions take and return `&[u8]`/`Vec<u8>` PDF bytes, keeping the crate
IO-agnostic and embeddable.

## The npm package has two entry points (one crate, two builds)

`pdf-core/wasm/build.sh` produces `pkg/` from Zone 1 only. The wasm crate
(`pdf-core/wasm/src/lib.rs`) is a thin wasm-bindgen wrapper whose watermark JSON
payload is a **faithful port of `parseWatermark`** from the retired HTTP
service — DocKosha kept the same payload shape across the migration, so the
package accepts exactly what the service did.

| Import | Build | Contents |
| --- | --- | --- |
| `@samarthmn/doc-yantra` | Node CJS (`--target nodejs`) | watermark/merge/csv/md/text/redact/validate (~2.8 MB) |
| `@samarthmn/doc-yantra/web` | Browser ESM (`--target web`) | Zone 1 for in-browser previews |

## office-core — the Office → PDF engine (separate crate)

`office-core/` is a second standalone crate: pure-Rust DOCX/PPTX/XLSX → PDF, with
its own **fail-closed envelope** (`convert()` returns `Unsupported{code}` off-
envelope) and **DocProfile** router contract (`profile()` predicts the verdict
without layout). It reuses none of pdf-core's code and does not modify it. It compiles to
wasm (`--no-default-features --features wasm`) and ships as the fontless
`@samarthmn/doc-yantra-office`. See `office-core/README.md`.

The Office package never embeds a font payload. Hosts use
`office_font_requests` → provider/cache → `office_register_font` before profile
and conversion; native default builds alone retain `bundled-fonts` for local
tests.

**Versioning spans four Cargo.tomls plus the adapter manifest/lockfile**, kept
in lockstep by `scripts/release.sh`: `pdf-core/Cargo.toml`,
`pdf-core/wasm/Cargo.toml`, `office-core/Cargo.toml`,
`office-core/wasm/Cargo.toml`, `packages/dockosha-provider/package.json` /
`pnpm-lock.yaml`, and `packages/doc-yantra-fonts/package.json`. The optional
font package owns the single canonical distribution-font copy. The owner-only
publish workflow validates and ships all four packages together.

## Build / test / run commands

Rust engine (`cd pdf-core`):
```bash
PDFIUM_SHA256_LINUX_X64=<reviewed-sha256> ./scripts/fetch-pdfium.sh
cargo test                         # full suite (Zone 1 + Zone 2)
cargo test --test watermark_test   # a single test file
cargo test page_count              # a single test by name substring
cargo build --release              # CLI binary at target/release/pdfcore
```
pdfium resolution order: `PDFIUM_DYNAMIC_LIB_PATH` → crate-local `pdfium/lib/` → system.

npm package suite (from repo root — validates the **built** `pkg/`; packages
load by path, no install; the PDF.js paint regression alone needs a one-time
`npm install` inside `tests/` and skips loudly otherwise):
```bash
node --test tests/                 # requires pkg/ built first (see below)
```

Rebuild the wasm package after any Rust change (`cd pdf-core/wasm`):
```bash
./build.sh                         # rebuilds Node + browser targets into pkg/
```

Rebuild the Office wasm package after any `office-core` or Office host-glue
change (`cd office-core/wasm && ./build.sh`). Its packed-artifact test rejects
font files and embedded SFNT programs.

## Releasing (0.0.x line)

Releases stay on `0.0.x` — **only the patch increments**. Use the script as a
side-effect-free preparation gate; it validates lockstep manifests, package
audits, and the public source candidate without changing repository or registry
state:
```bash
scripts/release.sh          # validate the 0.0.25 candidate
scripts/release.sh --publish # push its tag and start GitHub Packages release
```
The owner-only tag workflow performs shipping after the same gate; published
versions are immutable.

## Rules that are easy to get wrong

- **`pdf-core/wasm/pkg/` is committed and must stay in sync.** Vercel/CI have no
  Rust toolchain, so the built package is checked in. After changing Rust code,
  run `./build.sh` and keep the regenerated `pkg/` in the reviewed change. The Node test suite loads
  `pkg/` by path, so stale `pkg/` = stale tests.
- **Version lives in TWO Cargo.toml files** — `pdf-core/Cargo.toml` and
  `pdf-core/wasm/Cargo.toml` — plus both `Cargo.lock`s and the built
  `pkg/package.json`; keep all of them on the same `0.0.x`. The package's
  `version()` returns the wasm crate's. Published versions are **immutable**.
- **The DocKosha adapter joins that same release.** Its committed `dist/` is
  checked by the adapter source-to-dist fingerprint and package audit.
- **Distribution fonts have one owner.** `packages/doc-yantra-fonts/` contains
  the canonical hashes, binaries, and licences. Core packages contain only the
  small manifest/provider metadata and must never grow a `fonts/` directory.
- **Releasing** stays on the `0.0.x` line. `scripts/release.sh` is a
  side-effect-free preparation gate; `scripts/release.sh --publish` pushes the
  reviewed version tag that starts the GitHub Packages workflow.
- **Package identity is fixed.** Keep the four approved names on GitHub
  Packages; see `CONSUMING.md` for the consumer setup.

## Behavioral contracts to preserve when editing

- **pdfium is not thread-safe.** All Zone 2 work must go through
  `crate::pdfium::lock()` (a process-wide mutex); Zone 2 ops serialize per process.
  Zone 1 stays lock-free and parallel.
- **Redaction fails closed.** `redact_pure()` and legacy `redact()` return
  `Unsupported` rather than ever
  emitting output where secret text could survive. It removes text objects
  (unextractable, not just covered), remove Form XObject content, delete
  intersecting annotations, and **re-verifies the saved output**. Object-level
  granularity is intentional (more aggressive than character-level = safe direction).
  Don't loosen these guarantees.
- **wasm panic/trap contract.** At the wasm boundary, `Err → JsError` is
  recoverable (bad input, oversized rejection); a Rust `panic!`/OOM is an
  **uncatchable trap** that poisons the instance. Zone 1 exports reject oversized
  inputs (~100 MiB/doc, ~200 MiB/merge) as a normal `JsError` before touching
  linear memory. See `CONSUMING.md` § "Panic / trap contract".
- **Fonts: standard-14 (WinAnsi/CP1252) plus runtime-registered Unicode faces.**
  Non-WinAnsi text renders through `register_font()`-supplied faces as embedded
  CID subsets; complex-script/RTL text and uncovered glyphs fail closed with
  `unsupported:*` (no HTTP fallback exists).

## Cargo features

`default = ["pdfium", "cli", "parallel"]`. `pdfium` gates Zone 2; `cli` gates the
`pdfcore` binary; `parallel` enables lopdf's rayon (native only — thread pools
panic on wasm); `wasm` routes getrandom to the JS crypto backend. **wasm builds
use `--no-default-features --features wasm`**, which drops Zone 2 and rayon.

## Docs worth reading

`README.md` (repo overview + release flow), `pdf-core/README.md` (engine scope,
speed table, design notes & known limitations), and `CONSUMING.md` (install flow
for consumers + full panic/trap contract). The public candidate exposes only
self-contained checks under `public-tests/`; fixture-dependent regression and
owner-golden checks remain private. Requires Rust ≥ 1.89 and Node ≥ 18.
