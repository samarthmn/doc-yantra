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
and CI never need Rust. To cut a release:

```bash
# 1. rebuild the package (requires Rust + wasm-pack locally)
cd pdf-core/wasm && ./build.sh && cd -

# 2. bump the version in pdf-core/wasm/Cargo.toml, then rebuild so pkg/ matches
# 3. commit pkg/ and push
git add pdf-core/wasm && git commit -m "wasm: v0.1.1"

# 4. tag it — the publish-wasm workflow builds and publishes to GitHub Packages
git tag wasm-v0.1.1 && git push origin main wasm-v0.1.1
```

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

**`.npmrc`** at the app root:

```ini
@samarthmn:registry=https://npm.pkg.github.com
//npm.pkg.github.com/:_authToken=${GITHUB_PACKAGES_TOKEN}
```

**`package.json`**:

```jsonc
"dependencies": {
  "@samarthmn/doc-yantra": "^0.0.1"
}
```

**Install:**

```bash
export GITHUB_PACKAGES_TOKEN=ghp_xxx   # the read:packages token
pnpm install
```

**On Vercel:** add `GITHUB_PACKAGES_TOKEN` as a project environment variable
(all environments). Vercel expands it in `.npmrc` at install time. Keep the
package in `serverExternalPackages` (Next.js) so the `.wasm` binary is traced
into the function bundle instead of being bundled by the compiler.

## 4. Use it

`wasm-pack --target nodejs` emits CommonJS; import it in server code only.

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
```

In DocKosha this is wrapped by `src/server/pdfCoreWasm.ts`, which never throws
and falls back to the DocYantra HTTP service on any failure. Non-ASCII csv/md
and non-WinAnsi watermark text are routed to DocYantra (the engine has
standard-14 fonts only), so the local path is always a pure speed win.

---

## Alternative: install straight from git (no registry)

If you’d rather skip GitHub Packages, install the committed `pkg/` directly.
This needs no scope-matching, but Vercel still needs git auth for the private
repo (set `GITHUB_TOKEN` and rewrite the URL in the install step):

```jsonc
"@samarthmn/doc-yantra": "github:samarthmn/doc-yantra#wasm-v0.0.1&path:/pdf-core/wasm/pkg"
```

`#path:` support varies by pnpm version — verify with yours, or use a dedicated
root-level package repo if the subdirectory form gives trouble. The GitHub
Packages route above is the supported, recommended path.

## Local development

While iterating on the engine, link the local build instead of publishing:

```bash
# from the DocKosha app
pnpm add link:../doc-yantra/pdf-core/wasm/pkg   # sibling checkout
# rebuild after Rust changes:
( cd ../doc-yantra/pdf-core/wasm && ./build.sh )
```

DocKosha currently ships this way (the app’s dependency points at the sibling
checkout). Switch it to `"^0.0.1"` once the package is published for
teammates/Vercel.
