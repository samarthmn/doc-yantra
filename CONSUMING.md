# Consuming `@samarthmn/doc-yantra`

The WebAssembly build of the pdf-core engine is published as a **private npm
package on GitHub Packages** and consumed by the DocKosha Next.js app (and any
other Node/Vercel service). It runs merge, watermark, and csv/markdown/text →
PDF **in-process** — no network hop, no Rust toolchain on the consumer side.

> **Scope must match the GitHub owner.** GitHub Packages requires the npm scope
> (`@samarthmn`) to equal the GitHub user/org that owns this repo. If your owner
> is **not** `samarthmn`, change `SCOPE`/`OWNER` in
> [`pdf-core/wasm/build.sh`](pdf-core/wasm/build.sh), the `scope:` in
> [`.github/workflows/publish-wasm.yml`](.github/workflows/publish-wasm.yml),
> and the `@samarthmn:registry` line in the consumer’s `.npmrc`, then rebuild.

---

## 1. Publish a version (maintainers)

The built package lives at `pdf-core/wasm/pkg` and **is committed** so consumers
and CI never need Rust. Releases stay on the **0.0.x** line (only the patch
increments). One script does the whole cut — it picks the next `x` from the
`0.0.*` tags, bumps both Cargo.tomls in sync (`version()` returns the wasm
crate's), rebuilds `pkg/`, commits, tags `0.0.x`, and pushes (requires Rust
+ wasm-pack locally):

```bash
pdf-core/scripts/release.sh            # auto: next patch, commit, tag, push
pdf-core/scripts/release.sh 7          # explicit: release 0.0.7
DRY_RUN=1 pdf-core/scripts/release.sh  # preview only
```

Published versions are immutable — the script refuses to reuse or go below an
existing tag; always bump.

The [`publish-wasm`](.github/workflows/publish-wasm.yml) GitHub Action publishes
using the repo’s built-in `GITHUB_TOKEN` (needs `packages: write`, already set).
You can also publish manually from `pdf-core/wasm/pkg` with `npm publish` after
authenticating to GitHub Packages.

## 2. Create an install token

Consumers authenticate to GitHub Packages with a token that has **`read:packages`**:

- **Fine-grained PAT** (recommended): _Account → Developer settings → Personal
  access tokens → Fine-grained_ → grant **Packages: read-only** for this repo.
- Or a classic PAT with the `read:packages` scope.

## 3. Install in an app (e.g. DocKosha)

The **registry** goes in the app's committed `.npmrc`; the **credential** must
NOT — pnpm refuses to expand env vars in a committed project `.npmrc` (a leak
guard) and the install would 401.

**`.npmrc`** at the app root (registry only, safe to commit):

```ini
@samarthmn:registry=https://npm.pkg.github.com
```

**`package.json`**:

```jsonc
"dependencies": {
  // Releases are on the 0.0.x line — pin the exact patch (a caret on 0.0.x
  // resolves only that exact version) and bump it when a new patch ships.
  "@samarthmn/doc-yantra": "0.0.2"
}
```

**Local install** — put the token in your **user-level** `~/.npmrc` (which pnpm
trusts), then install:

```bash
# writes //npm.pkg.github.com/:_authToken=<token> into ~/.npmrc
pnpm config set "//npm.pkg.github.com/:_authToken" "$GITHUB_PACKAGES_TOKEN"
pnpm install
```

(Or add `//npm.pkg.github.com/:_authToken=${GITHUB_PACKAGES_TOKEN}` to
`~/.npmrc` by hand — pnpm expands env vars in the user-level file.)

**On Vercel:** add `GITHUB_PACKAGES_TOKEN` as a project environment variable
(all environments), then set the project's **Install Command** so the token
lands in a trusted `~/.npmrc` before install:

```bash
npm config set "//npm.pkg.github.com/:_authToken" "$GITHUB_PACKAGES_TOKEN" && pnpm install
```

Keep the package in `serverExternalPackages` (Next.js) so the `.wasm` binary is
traced into the function bundle instead of being bundled by the compiler.

## 4. Use it

`wasm-pack --target nodejs` emits CommonJS; import it in server code only.

The package has three entry points:

| Import | Runtime | Contents |
| --- | --- | --- |
| `@samarthmn/doc-yantra` | Node (CJS) | watermark, merge, csv/md/text → PDF, validate (1.7MB) |
| `@samarthmn/doc-yantra/office` | Node (CJS) | docx/pptx/xlsx → PDF (pure-Rust office2pdf; 44MB — lazy-load it, never on hot paths) |
| `@samarthmn/doc-yantra/web` | Browser (ESM) | Zone 1 for client-side previews; call `await mod.default(wasmUrl)` first and serve `web/pdf_core_wasm_bg.wasm` yourself |

```ts
import * as pdf from "@samarthmn/doc-yantra";

// merge + watermark (same JSON payload DocYantra's /pdf/merge accepts)
const out = pdf.merge_pdfs(
  [new Uint8Array(pdfA), new Uint8Array(pdfB)],
  JSON.stringify({ pattern: "diagonal_grid", lines: ["CONFIDENTIAL"] }),
  undefined,
);

pdf.csv_to_pdf(new Uint8Array(csvBytes)); // → Uint8Array (landscape table)
pdf.markdown_to_pdf(markdownString);
pdf.page_count(new Uint8Array(pdfBytes)); // → number
pdf.validate_pdf(new Uint8Array(pdfBytes)); // → JSON string

// Office conversion (heavy module — import lazily, e.g. behind a flag)
const office = require("@samarthmn/doc-yantra/office");
const { pdf: bytes, warnings } = office.office_to_pdf(new Uint8Array(docx), "docx");
```

In DocKosha this is wrapped by `src/server/pdfCoreWasm.ts`, which never throws
and falls back to the DocYantra HTTP service on any failure. Text outside the
WinAnsi/CP1252 repertoire — in csv/md conversion or watermark lines — is routed
to DocYantra (the engine has standard-14 fonts only), so the local path is
always a pure speed win.

---

## Alternative: install straight from git (no registry)

If you’d rather skip GitHub Packages, install the committed `pkg/` directly.
This needs no scope-matching, but Vercel still needs git auth for the private
repo (set `GITHUB_TOKEN` and rewrite the URL in the install step):

```jsonc
"@samarthmn/doc-yantra": "github:samarthmn/doc-yantra#0.0.2&path:/pdf-core/wasm/pkg"
```

`#path:` support varies by pnpm version — verify with yours, or use a dedicated
root-level package repo if the subdirectory form gives trouble. The GitHub
Packages route above is the supported, recommended path.

## Local development

DocKosha ships against the **published registry package** (`package.json` pins
the exact `0.0.x`), not a sibling checkout. While iterating on the engine,
temporarily link the local build, then restore the registry pin before committing:

```bash
# from the DocKosha app
pnpm add link:../doc-yantra/pdf-core/wasm/pkg   # sibling checkout
# rebuild after Rust changes:
( cd ../doc-yantra/pdf-core/wasm && ./build.sh )
# …then restore "@samarthmn/doc-yantra": "0.0.2" and `pnpm install` before commit,
# so CI/Vercel resolve the published package (a link: path breaks them).
```

## Panic / trap contract

The wasm boundary distinguishes two failure modes, and consumers must treat
them differently:

- **`Err` → thrown `JsError`** (recoverable): invalid input, unsupported
  format, or an over-size rejection (see below). Catch it and fall back; the
  instance stays healthy and can be reused.
- **Panic / OOM → trap** (fatal to the instance): a Rust `panic!` or an
  out-of-memory abort in wasm is **not** a catchable `JsError` —
  `console_error_panic_hook` only *logs* it. A trap can leave the module's
  linear memory in an undefined state, so **the instance must be discarded and
  recreated**; do not keep calling into a trapped instance.

The Zone-1 input path has no known reachable `panic!`, but very large inputs can
still OOM-abort. To keep that recoverable, the exports **reject oversized inputs
as a normal `JsError`** before touching linear memory (≈100 MiB per document,
≈200 MiB per merge set). Consumers should apply their own, lower limits and
route large jobs to the HTTP service (DocKosha does this in
`src/server/pdfCoreWasm.ts`), and — because merge/watermark/csv/md run on one
shared instance — treat a trapped instance as recreate-and-fall-back.
