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

The engine packages' `wasm/pkg` directories and the DocKosha adapter's `dist/`
are **committed** so consumers and CI never need Rust or TypeScript. Releases
stay on the **0.0.x** line (only the patch increments). From a clean local
`main` branch, one script picks the next `x`, bumps all four Cargo manifests
plus the adapter version and engine pins, rebuilds all three packages, runs the
Rust/package/fidelity/adapter gates, commits, tags `0.0.x`, and pushes (requires
Rust; the build scripts pin wasm-pack 0.15.0 and use pnpm or npx when it is not
installed globally):

```bash
scripts/release.sh            # auto: next patch, commit, tag, push
scripts/release.sh 7          # explicit: release 0.0.7
DRY_RUN=1 scripts/release.sh  # preview only
```

Published versions are immutable — the script refuses to reuse or go below an
existing tag; always bump.

The [`publish-wasm`](.github/workflows/publish-wasm.yml) GitHub Action requires
the tagged commit to be on `main`, requires the tag to match all three committed
package versions, then publishes both engines followed by the adapter using the
repo's built-in `GITHUB_TOKEN` (needs `packages: write`, already set). It does
not rebuild or retest the packages; `release.sh` is the supported local build
and test gate.

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
  // Use the latest published patch — find it with:
  //   git tag --list '0.0.*' | sort -V | tail -1
  "@samarthmn/doc-yantra": "0.0.7",
  "@samarthmn/doc-yantra-office": "0.0.7",
  "@samarthmn/dockosha-provider-docyantra": "0.0.7"
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

The package has two entry points:

| Import | Runtime | Contents |
| --- | --- | --- |
| `@samarthmn/doc-yantra` | Node (CJS) | watermark, merge, csv/md/text → PDF, redaction, validate (~2.8MB) |
| `@samarthmn/doc-yantra/web` | Browser (ESM) | Zone 1 for client-side previews; call `await mod.default(wasmUrl)` first and serve `web/pdf_core_wasm_bg.wasm` yourself |
| `@samarthmn/doc-yantra-office` | Node (CJS) | **Office → PDF**: staged sync exports plus provider-backed `convertOnDemand()` |
| `@samarthmn/doc-yantra-office/web` | Browser (ESM) | same staged/on-demand API plus `IndexedDbFontCache` |

### Office package (`@samarthmn/doc-yantra-office`)

Same registry, token, and `.npmrc` scope flow as `@samarthmn/doc-yantra` (same
GitHub owner). Both packages publish together on each `0.0.*` tag, so pin them to
the same version.

The package is fontless. The canonical order is discover → supply → verdict →
convert; a profile taken before registration is provisional for font-dependent
reasons:

```js
const office = require("@samarthmn/doc-yantra-office");

const requests = JSON.parse(office.office_font_requests(bytes, "docx"));
for (const resolved of await trustedProvider.resolve(requests, signal)) {
  if (resolved.status === "found") {
    office.office_register_font(resolved.artifact.bytes, expectationJson(resolved));
  }
}
const profile = JSON.parse(office.office_profile(bytes, "docx"));
if (profile.supported) office.office_to_pdf(bytes, "docx");

// Convenience facade performs the same sequence, batching the provider call.
const result = await office.convertOnDemand(bytes, "docx", {
  provider: new office.FontsourceProvider(),
  mode: "compatible", // or "strict"
  memoryCache: new office.MemoryFontCache(),
  persistentCache: new office.NodeFontCache(absoluteCacheDirectory),
  signal,
  limits: { maxFontBytes: 24 * 1024 * 1024 },
});
```

Browser consumers initialize the ESM default export first and may use
`IndexedDbFontCache`. Remote providers must use HTTPS, return catalog-allowlisted
bytes (never a document-derived URL), enforce CORS/content type/redirect ≤3,
and pin SHA-256. The package validates identity, coverage, collection index,
variable axes, and OS/2 embedding permission before registration.

`FontsourceProvider` is the packaged allowlisted provider for Node and browser.
It maps Calibri/Cambria/Arial/Times/Courier-compatible families to Fontsource
IDs, fetches one static TTF per required Unicode subset, and registers sibling
subset faces as one request result. Ubuntu (`v21`) and Ubuntu Mono (`v19`) use
the immutable Fontsource `5.2.8` artifact paths and exact internal-version
checks (`0.83` / `Version 0.80`). Other built-in families intentionally follow
the accepted `latest` version policy; resolved hashes and name-table versions
remain visible in font telemetry. A trusted `fallbackSource` callback may point
at a commit-pinned Google Fonts mirror.

**Arial metrics caveat.** Fontsource's Arimo artifacts (including the current
5.2.8 line) carry usWinAscent/Descent 2136/797 instead of Arial's 1854/434,
which inflates every Arial line box ~26% versus Word (measured 2026-08-02).
The DocKosha provider therefore serves Arial-class requests from the packaged
Liberation Sans faces (`@samarthmn/doc-yantra/fonts`, true Arial Windows
metrics, all four styles) instead of fetched Arimo; a literal `Arimo` request
still gets Arimo. Hosts wiring `FontsourceProvider` directly inherit the
inflated Arimo metrics — mirror the replacement if Word-parity line flow
matters.

Because Google-style `latin`/`latin-ext` subset artifacts strip mathematical
operators (U+2200–22FF) and most symbol blocks, any request whose `scripts`
include `symbols` or `math` also fetches pinned supplements — Noto Sans Math
(`5.2.9`, its `latin` artifact is the full font) and Noto Sans Symbols 2's
`symbols` artifact (`5.2.8`) — and returns them as additional artifacts on the
same result. `Noto Sans Symbols`, `Noto Sans Symbols 2`, and `Noto Sans Math`
are also directly resolvable catalog families, so documents authored with those
faces (Google Docs exports routinely reference `Noto Sans Symbols`) resolve
without falling back to a Latin text face.

**CJK (Chinese/Japanese/Korean) in Office documents.** The engine fails closed
(`no font for CJK` / `missing_glyph`) unless a registered face covers the
document's CJK code points; nothing CJK is bundled or fetched by default. Since
0.0.12 the registration caps (engine and host, 24 MiB) admit the pan-CJK
regional OTFs that `@samarthmn/doc-yantra` ships under `fonts/` for pdf-core —
`Noto Sans CJK {SC,TC,JP,KR}` (~16.5 MiB each) and, since 0.0.19, their
`Noto Serif CJK` siblings (~24 MiB each, Serif2.003, hash-pinned). A host
provider supplies them for requests whose `scripts` include `"cjk"`. The
DocKosha provider picks the regional variant from the requested family or the
request's Windows charset (0x81 Hangul → KR, 0x80 Shift-JIS → JP, 0x86 GB →
SC, 0x88 Big5 → TC; default SC), and the style class from the requested
lineage: Song/Ming/Mincho/Batang serif lineages (SimSun, MingLiU, MS Mincho,
Batang, KaiTi, …) get the Serif face — matching how Word renders them — while
Hei/Gothic/Malgun/YaHei sans lineages keep the Sans face; the engine's
compatible-mode substitution ladder prefers the same faces. Every regional OTF
covers the full pan-CJK repertoire, so the variant only selects Han-unified
glyph styling. Emitted PDFs stay small because CFF faces embed as used-glyph
`CIDFontType0` subsets. Bold CJK runs render with synthetic emboldening (the
emitter fills and strokes the Regular outlines at the pdfium/mupdf `size/30`
convention) because the classic East Asian families ship no bold face — the
same compromise GDI makes. Word's automatic East Asian spacing
(`w:autoSpaceDE`/`DN`, default on) is applied: an eighth-em between East Asian
letters and adjacent Latin letters or digits. Known limits: vertical text
(`w:textDirection`) stays outside the envelope, and regional styling for
Han-unified text follows the declared family, not document language tags.

**Stable API surface DocKosha depends on:**

The signatures of `office_profile`, `office_to_pdf`, and the deprecated
`register_font` alias are unchanged. `office_register_font` is the validated
replacement. `strict` mode permits only embedded/registered exact faces;
`compatible` also permits explicitly reported style/substitution/fallback
matches. Provider `not_found`, `denied`, and `temporary_failure` remain distinct.
The DOCX envelope now additionally covers bounded page/paragraph
anchors, floating-table side wrap, sequential notes, internal destinations,
and balanced continuous-column bands; supported profiles may add the warning
codes `bullet_substituted` and `floating_table_block_fallback`. Supported
superscripts use bundled U+207x glyph coverage; uncovered glyphs fail closed as
`missing_glyph` and are not synthesized.

- `office_to_pdf` throws a `JsError` whose message begins with
  `"unsupported:<code>: "` for envelope misses (e.g. `unsupported:footnotes: …`),
  `"invalid: …"` / `"internal: …"` otherwise. Parse the `unsupported:` prefix to
  record the fallback reason.
- The built Node and browser glue reject inputs over **50 MiB** before copying
  into wasm memory; treat that as a terminal input-too-large error.
- Trap contract is identical to `@samarthmn/doc-yantra`: `JsError` is
  recoverable; a panic/OOM traps and the instance must be discarded.

DocKosha's adapter is `src/server/officeCoreWasm.ts` (mirrors `pdfCoreWasm.ts`).

`createOfficeEngineWorker({ fontProvider, fontMemoryCache,
fontPersistentCache, fontMode, notoFallback })` exposes `convertOnDemand()` and
`profileOnDemand()`. `extraFontDirs` remains an explicit, host-authorized boot
registration mechanism; system-font scanning is never implicit.
When `fontProvider` is configured, compatible worker operations automatically
register the six hash/version-pinned first-party Noto fallback faces and persist
them through the same cache. Warm operations perform no fallback network I/O,
and worker replacement re-registers cached bytes. Strict operations return an
empty `fallbackResults` array and never touch the fallback origin. Set
`notoFallback: false` only for a deliberately provisioned offline/test worker,
or inject a provider/options object for controlled deployments.

Office → PDF is provided by the separate
`@samarthmn/doc-yantra-office` package for supported DOCX/PPTX/XLSX/XLSM
documents. Legacy binary formats (.doc/.ppt/.xls) are unsupported, and
documents outside the fail-closed envelope surface `unsupported:<code>`
errors the app records as conversion failures.

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
pdf.redact_pdf(
  new Uint8Array(pdfBytes),
  JSON.stringify([{ pageIndex: 0, left: 10, top: 8, width: 25, height: 6 }]),
); // → Uint8Array; unsupported cases throw with an "unsupported:" prefix
```

In DocKosha this is wrapped by `src/server/pdfCoreWasm.ts`, which keeps wasm
errors recoverable and treats every failure as terminal for that operation —
there is no HTTP fallback for any operation. Text outside the registered font
coverage in csv/md conversion or watermark lines fails closed with an
`unsupported:*` error; register a covering face to widen coverage.

---

## Alternative: install straight from git (no registry)

If you’d rather skip GitHub Packages, install the committed `pkg/` directly.
This needs no scope-matching, but Vercel still needs git auth for the private
repo (set `GITHUB_TOKEN` and rewrite the URL in the install step):

```jsonc
// swap 0.0.x for the latest tag: git tag --list '0.0.*' | sort -V | tail -1
"@samarthmn/doc-yantra": "github:samarthmn/doc-yantra#0.0.x&path:/pdf-core/wasm/pkg"
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
# …then restore the exact registry pin (the latest 0.0.x) and `pnpm install` before commit,
# so CI/Vercel resolve the published package (a link: path breaks them).
```

## Panic / trap contract

The wasm boundary distinguishes two failure modes, and consumers must treat
them differently:

- **`Err` → thrown `JsError`** (recoverable): invalid input, unsupported
  format, or an over-size rejection (see below). Catch it; the instance stays
  healthy and can be reused. Fall back only when that operation has a
  compatible HTTP endpoint. Redaction does not.
- **Panic / OOM → trap** (fatal to the instance): a Rust `panic!` or an
  out-of-memory abort in wasm is **not** a catchable `JsError` —
  `console_error_panic_hook` only *logs* it. A trap can leave the module's
  linear memory in an undefined state, so **the instance must be discarded and
  recreated**; do not keep calling into a trapped instance.

The Zone-1 input path has no known reachable `panic!`, but very large inputs can
still OOM-abort. To keep that recoverable, the exports **reject oversized inputs
as a normal `JsError`** before touching linear memory (≈100 MiB per document,
≈200 MiB per merge set). Consumers should apply their own, lower limits.
Route only operations with compatible endpoints to HTTP; reject oversized
redactions locally. Because merge/watermark/csv/md/redaction run on one shared
instance, discard and recreate the instance after a trap before applying the
operation's fallback policy.
