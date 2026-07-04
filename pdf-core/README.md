# pdf-core — DocKosha's native PDF engine

Proprietary Rust engine for every PDF-native operation DocKosha performs.
Replaces the pdf-lib (Node) + PyMuPDF (Python, AGPL) paths inside DocYantra
with one in-process, memory-safe library — no subprocess spawns, no
cross-language hops, permissively-licensed throughout (lopdf/pdf-writer:
MIT/Apache-2.0; pdfium: BSD — the AGPL PyMuPDF dependency is gone).

## Scope

**Zone 1 — pure Rust (lopdf + pdf-writer), embeddable anywhere (service, N-API, WASM):**

| Module | Operations |
|---|---|
| `assembly` | merge, split, extract/delete/reorder pages, rotate |
| `watermark` | text/image/hybrid watermarks (single + grid, rotation, opacity) — an exact port of DocYantra's pdf-lib algorithm; page-number / Bates stamping |
| `meta` | page count, validate, page sizes, metadata get/set, compress, encrypt (RC4-128) |
| `generate` | CSV → PDF (paginated tables), Markdown → PDF, text → PDF, images → PDF — replaces the Chromium html-to-pdf route for csv/md |

**Zone 2 — Rust + pdfium (bundled BSD dylib):**

| Module | Operations |
|---|---|
| `raster` | page rendering, thumbnails (PNG/JPEG) |
| `extract` | full/per-page text extraction (rotation-safe), search with match rects |
| `redact` | **true redaction** — text objects removed from content (unextractable), black box drawn; DocYantra-compatible percent rects, rotation-aware (`/Rotate` pages measured as displayed); recurses into Form XObjects; deletes intersecting annotations; fails closed (`Unsupported`) rather than ever returning output with secret text surviving |
| `forms` | AcroForm field listing, text-field fill, flatten |

**Out of scope (stays on heavy engines):** Office → PDF (LibreOffice),
arbitrary HTML → PDF (Chromium). Those are served by the DocYantra HTTP service, which lives in the DocKosha app repo (`doc-kosha/doc-yantra/`).

## Speed (measured, release build, Apple Silicon)

| Operation | pdf-core | DocYantra today |
|---|---|---|
| CSV → PDF | ~5 ms | ~1–3 s (Chromium) |
| Markdown → PDF | ~4 ms | ~1–3 s (Chromium) |
| Merge | ~5 ms | ~500 ms–2 s (HTTP + pdf-lib) |
| Watermark (grid, 2 lines) | ~6 ms | ~500 ms–2 s (HTTP + pdf-lib) |
| Redact + verify | ~22 ms | ~300 ms–1 s (spawn python3 + PyMuPDF) |
| Thumbnail 480px | ~20 ms | n/a (client-side only today) |
| Text extraction | ~50 ms | n/a |

## Usage

```rust
use pdf_core::{assembly, watermark::{self, WatermarkSpec, Pattern}};

let merged = assembly::merge(&[&doc_a, &doc_b])?;
let stamped = watermark::apply_watermark(&merged, &WatermarkSpec {
    pattern: Pattern::Grid,
    rotation_deg: -40.0,
    lines: vec!["CONFIDENTIAL".into(), "viewer@example.com".into()],
    ..WatermarkSpec::default()
})?;
```

CLI (`cargo build --release`, binary at `target/release/pdfcore`):

```
pdfcore csv data.csv -o out.pdf
pdfcore watermark in.pdf -o out.pdf -l CONFIDENTIAL -l user@x.com
pdfcore redact in.pdf -o out.pdf --page 0 --left 5 --top 2 --width 60 --height 12
pdfcore thumb in.pdf -o page0.png --width 480
```

## Setup

```
./scripts/fetch-pdfium.sh   # downloads the pdfium dylib (build 7763) for your platform
cargo test                  # 162 tests (includes regressions from an adversarial audit)
```

pdfium resolution order: `PDFIUM_DYNAMIC_LIB_PATH` env var → crate-local
`pdfium/lib/` → system library.

## Design notes & known limitations

- **pdfium is not thread-safe** (pdfium-render's `thread_safe` feature only
  adds Send/Sync markers — it does not lock). All pdfium work goes through
  `crate::pdfium::lock()`, a process-wide mutex whose handle lifetime-bounds
  every document borrowed from it. Zone 1 modules are lock-free and fully
  parallel; Zone 2 operations serialize per process.
- **Redaction granularity is object-level:** a text object partially covered
  by a rect is removed entirely (strictly more aggressive than PyMuPDF's
  character-level — the safe direction for redaction). The full bounds of
  everything removed are covered with black boxes, and
  `RedactOutput::warnings` reports any removal that meaningfully exceeds the
  requested rect so the UI can inform the user. Images/vector art are
  covered, not removed (matches `PDF_REDACT_IMAGE_NONE`).
- **Redaction fails closed:** text inside Form XObjects is removed
  recursively; intersecting annotations are deleted; if pdfium cannot
  guarantee removal (or its content regeneration would drop unrelated text —
  seen with Type3-font PDFs), `redact()` returns `Unsupported` instead of a
  silently-leaky or silently-gutted document. The saved output is re-verified
  before it is returned.
- **Redaction leak workaround:** the bundled pdfium segfaults destroying
  removed text objects, so their tiny handles are deliberately leaked
  (`mem::forget`, ~1.2KB per removed object, process-lifetime, bounded).
- **Encrypted PDFs are refused** by `compress()`/`set_metadata()` (lopdf
  would silently strip or corrupt them); `validate()` detects `/Encrypt`
  including owner-locked (empty user password) documents. Metadata is
  written spec-compliantly (ASCII literal or UTF-16BE with BOM) and read
  with PDFDocEncoding fallback; files with trailing garbage after `%%EOF`
  are tolerated.
- **Fonts:** standard-14 (Helvetica/Courier) with AFM metrics; text encodes
  to WinAnsi (non-Latin-1 chars become `?`). Full Unicode needs embedded-font
  support (deferred).
- **`generate::images_to_pdf` embeds raw RGB** (no Flate) to avoid a deflate
  dependency — large photos produce large PDFs. Switch to FlateDecode when a
  deflate crate is added.
- **Encryption** is RC4-128 (what lopdf 0.42 writes today); AES-256 deferred.
- Merge drops bookmarks/outlines (v1).

## WebAssembly package (Zone 1 in the Next.js app)

`wasm/` wraps Zone 1 for Node via wasm-bindgen. The watermark entry points
accept the **same JSON payload** DocKosha sends to DocYantra's
`/docyantra/pdf/merge`, so the app switches engines without reshaping data.

```
cd wasm && pnpm dlx wasm-pack build --release --target nodejs --scope samarthmn
```

The output (`wasm/pkg`, ~1.7MB `.wasm`) is installed in the app as
`@samarthmn/doc-yantra` (pnpm `link:` dependency) and listed in
`serverExternalPackages` in `next.config.ts` so Vercel file-traces the
binary into each function. **`wasm/pkg` must be committed** — Vercel builds
don't have a Rust toolchain. Rebuild + re-commit after changing Rust code.

Wired in the app (wasm-first, DocYantra HTTP fallback on any failure):
- `src/server/pdfCoreWasm.ts` — adapter (lazy load, never throws)
- `src/server/watermarkService.ts` — merge + watermark (all download routes)
- `src/server/conversionService.ts` — csv/md conversion (no more Chromium)

Feature flags: the crate's `pdfium` feature (default on) gates Zone 2;
wasm builds use `--no-default-features --features wasm` (also disables
lopdf's rayon, which cannot run threads in wasm).

## Deferred (roadmap)

Embedded TTF fonts + full Unicode · AES-256 encryption · linearization ·
attachments/bookmarks editing · annotations API · OCR (Zone 3) · PAdES
signing (Zone 3) · N-API packaging · redaction in-app (needs pdfium-wasm).

## Remaining DocYantra usage after WASM integration

Office→PDF (LibreOffice) · arbitrary HTML→PDF (Chromium) · redaction
(pdfium, Zone 2) · fallback for every wasm-covered operation.
