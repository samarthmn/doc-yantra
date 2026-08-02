# doc-yantra — DocKosha's native PDF engine (Rust + WebAssembly)

This repo is DocKosha's **proprietary PDF engine** and its distributable
**npm package**. It contains:

| Path | What it is |
| --- | --- |
| `pdf-core/` | The Rust engine: merge, split, watermark, metadata, CSV/Markdown/text/image → PDF, and pure-Rust redaction (Zone 1), plus pdfium-backed raster/extract/forms and legacy redaction cross-validation (Zone 2, native-only). See [`pdf-core/README.md`](pdf-core/README.md). |
| `pdf-core/wasm/` | The **`@samarthmn/doc-yantra`** npm package (built output committed at `pdf-core/wasm/pkg`; consumers never need Rust). Two modules: main (Node CJS), `./web` (browser ESM); both expose Zone 1, including fail-closed redaction. Office→PDF is provided by the separate Office package below. |
| `office-core/` | The pure-Rust **Office → PDF** engine: DOCX/PPTX/XLSX layout + PDF emission, fail-closed envelope, DocProfile router contract. No LibreOffice/Chromium/pdfium in the conversion path. See [`office-core/README.md`](office-core/README.md). |
| `office-core/wasm/` | The **`@samarthmn/doc-yantra-office`** npm package (Node CJS + browser ESM). `office_profile()` / `office_to_pdf()` / `register_font()`. DocKosha routes all office conversions here; envelope misses fail closed (no LibreOffice anywhere). |
| `packages/dockosha-provider/` | The **`@samarthmn/dockosha-provider-docyantra`** TypeScript adapter. Its committed `dist/` and exact engine pins release in lockstep with both wasm engines. |
| `.github/workflows/publish-wasm.yml` | Validates a `0.0.*` tag against `main` and all three committed package versions, then publishes both engines followed by the adapter to **GitHub Packages** without rebuilding or retesting them. |

> **History note:** the DocYantra HTTP service (NestJS + Gotenberg) has been
> retired and deleted from the DocKosha app repo. These packages are the only
> conversion path; anything the engines cannot prove fails closed.

## Quick start (consume the package)

Full guide: **[CONSUMING.md](CONSUMING.md)**. The package is published to GitHub
Packages; consume the registry version (this is how DocKosha ships today):

```jsonc
// app package.json — needs @samarthmn:registry in .npmrc + a read:packages token
"@samarthmn/doc-yantra": "0.0.7",
"@samarthmn/doc-yantra-office": "0.0.7"
```

> Releases stay on the **0.0.x** line. On `0.0.x`, a caret pins the exact patch
> (`^0.0.7` resolves only `0.0.7`), so pin the exact version and bump it in the
> consumer each time a new patch is published.

For local engine iteration you can temporarily `link:` a sibling checkout
(`link:../doc-yantra/pdf-core/wasm/pkg`), but switch back to the registry range
before committing — see CONSUMING.md § "Local development".

```ts
import * as pdf from "@samarthmn/doc-yantra"; // server-side only (CJS)

const merged = pdf.merge_pdfs(
  [new Uint8Array(a), new Uint8Array(b)],
  JSON.stringify({ pattern: "diagonal_grid", lines: ["CONFIDENTIAL"] }),
  undefined,
);
```

The watermark payload keeps the exact shape of the retired DocYantra
service's `/docyantra/pdf/merge` endpoint, so consumers migrated without
reshaping data. One deliberate refinement: when a grid payload omits
`xSpacingPt`/`ySpacingPt`, the default spacing now scales with each page's
box relative to US-Letter (Letter output is byte-identical to the historical
fixed 320pt lattice; slide-sized and other non-Letter pages get the same
visual stamp density instead of one sparse row). Explicit spacing values are
honored exactly as before.

## Developing the engine

```bash
cd pdf-core
./scripts/fetch-pdfium.sh   # one-time: pdfium dylib for native Zone 2
cargo test                  # full engine suite (Zone 1 + Zone 2)
cargo build --release       # CLI at target/release/pdfcore

# rebuild the npm package after Rust changes (updates pkg/ — commit it)
cd wasm && ./build.sh

# rebuild the fontless Office package after office-core changes
cd ../../office-core/wasm && ./build.sh
```

Requires Rust ≥ 1.89 (`rust-version` is pinned in both Cargo.tomls). The wasm build compiles Zone 1 only
(`--no-default-features --features wasm`); Zone 2 needs native pdfium and stays
out of the package. Its legacy redactor exists only for native
cross-validation. Zone-1 redaction has no HTTP fallback.

The Office wasm package is also built with `--no-default-features --features wasm`.
It contains no fonts: hosts discover canonical requests with
`office_font_requests()`, supply validated SFNT bytes with
`office_register_font()`, then profile and convert. Native Office builds retain
the default `bundled-fonts` feature for hermetic engine tests; fidelity corpus
runs register the hash-pinned provider fonts described below.

## Running the full fidelity suite

The full fidelity check runs the harness tests, verifies that every corpus case
has a Word-exported ground truth, then scores the complete blocking DOCX corpus.
Run it from the repository root (the first command is needed only once per
machine):

```bash
./pdf-core/scripts/fetch-pdfium.sh  # one-time: install the native pdfium library
cd office-core
cargo test -p fidelity
cargo run -p fidelity -- missing-ground-truth
cargo run -p fidelity -- run --format docx
```

The corpus command requires every core case to pass and at least 90% of the
extended cases to pass. It writes the HTML report to
`tmp/fidelity-report/index.html` at the repository root. Use
`--report-dir ../tmp/<name>` to keep a separate report for a specific run.
Each page is gated by the frozen legacy/unweighted SSIM threshold and the
vector geometry bounds for border segments and text-run origins. Ink-weighted
SSIM and heatmaps remain diagnostic: both SSIM variants and the geometry result
are included in the JSON and HTML reports.

Corpus runs resolve the fonts declared by each DOCX automatically from the
committed `office-core/fidelity/fonts.lock`. On a cold cache they require
network access and populate the ignored, content-addressed
`office-core/fidelity/fonts-cache/`; warm runs verify and reuse those bytes
without downloading them again. Do not pass host font directories to `run`.
`--font-dir` remains available only on the diagnostic `probe` command.

To score one case while developing:

```bash
cd office-core
cargo run -p fidelity -- run \
  --case gdocs-anchors-toc \
  --report-dir ../tmp/fidelity-gdocs-anchors-toc
```

`cargo run -p fidelity -- lock-fonts` is a maintenance command for an
intentional font-version refresh; it is not part of a normal test run. Review
and commit the resulting `fonts.lock` change together with that upgrade.

## Releasing

Releases stay on the **0.0.x** line — only the patch increments. From a clean
`main`, one script picks the next `x`, bumps all four engine/wasm manifests plus
the adapter manifest and engine pins, rebuilds and tests all three packages,
runs the blocking DOCX fidelity corpus, then commits, tags, and atomically
pushes `main` + `0.0.x`. The tag workflow verifies that the tagged commit is on
`main` and all three committed package versions match the tag, then publishes
both engines followed by the adapter without rebuilding or retesting them:

```bash
scripts/release.sh            # auto: next patch, commit, tag, push
scripts/release.sh 7          # explicit: release 0.0.7
DRY_RUN=1 scripts/release.sh  # preview only

# then, in a follow-up, bump DocKosha's package.json pin to the new 0.0.x + pnpm install
```

Published versions are immutable — the script refuses to reuse or go below an
existing tag; always bump.
