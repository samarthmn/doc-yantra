# Consuming DocYantra

DocYantra publishes four packages to GitHub Packages. The first-party
source and wrapper code is licensed under AGPL-3.0-or-later in [`LICENSE`](LICENSE).
Enterprise licensing is available only through a separate signed agreement;
see [`COMMERCIAL-LICENSING.md`](COMMERCIAL-LICENSING.md).

For deployment, offline fonts, source builds, and operational verification, see
[Running DocYantra on your own infrastructure](SELF-HOSTING.md). To run the full
application, follow [DocKosha self-hosting](https://github.com/samarthmn/doc-kosha/blob/HEAD/docs/self-hosting.md).

## Install

Pin all required packages to the same release candidate (currently `0.0.28`):

```json
{
  "dependencies": {
    "@samarthmn/doc-yantra": "0.0.28",
    "@samarthmn/doc-yantra-office": "0.0.28",
    "@samarthmn/dockosha-provider-docyantra": "0.0.28"
  }
}
```

Configure the `@samarthmn` scope in the consuming repository and authenticate
with a GitHub token that can read packages:

```ini
@samarthmn:registry=https://npm.pkg.github.com
```

For pnpm 11 consumers, also declare the scoped registry in
`pnpm-workspace.yaml` (merge this into an existing file):

```yaml
registries:
  default: https://registry.npmjs.org/
  "@samarthmn": https://npm.pkg.github.com/
```

Keep authentication in user-level npm configuration or the CI environment,
never in either committed configuration file. GitHub Actions consumers need
`packages: read`, registry authentication, and repository read access to all
four packages. DocKosha explicitly installs the matching font package even
though the engine packages declare it as an optional peer.

When migrating an older lockfile, replace stale DocYantra entries containing
npmjs tarball URLs with registry-resolved entries; do not hand-edit their
integrity values or disable supply-chain checks. Validate the result with a
fresh `pnpm install --frozen-lockfile`.

Fonts are optional and are never installed by default. For an offline
deployment, add the separate font pack at the same version:

```json
"@samarthmn/doc-yantra-fonts": "0.0.28"
```

The default font-free path keeps font-independent operations local. Hosts that
need Unicode faces may install the optional pack or provide a validated runtime
font provider.

## Local development

Build and pack the four candidates locally, then install the resulting sibling
tarballs in a disposable consumer project. Do not add sibling, `file:`, or
`link:` dependencies to a committed application manifest. Package audits and
smoke checks should pass before a candidate is shared.

## API overview

`@samarthmn/doc-yantra` provides PDF merge, split, watermark, metadata,
CSV/Markdown/text/image generation, validation, page counting, and fail-closed
redaction. Its root entry point is Node CJS; `@samarthmn/doc-yantra/web` is the
browser ESM entry point.

`@samarthmn/doc-yantra-office` provides the Office profile and conversion
exports for DOCX/PPTX/XLSX, plus host font registration and worker APIs. It is
fontless by default and fails closed outside its supported envelope.

`@samarthmn/dockosha-provider-docyantra` adapts both engines to DocKosha's
provider contract. DocKosha declares DocYantra as a mandatory direct `0.0.28`
dependency; there is no provider-selection environment variable or fallback.

```js
const pdf = require("@samarthmn/doc-yantra");
const output = pdf.text_to_pdf("hello from DocYantra");
```

## Safety and limits

Malformed, unsupported, encrypted, or oversized inputs fail closed with a
recoverable `unsupported:*` error. A WebAssembly panic/trap is not recoverable;
validate input sizes before passing untrusted bytes to the synchronous exports.
Worker APIs isolate the engine and expose structured failures.

## Maintainer release preparation

Versions remain lockstep on the `0.0.x` line and published versions are
immutable. The default release preparation is side-effect-free. Once the exact
candidate commit is reviewed, `--publish` pushes its version tag; the tag starts
the GitHub Packages and GitHub release workflow.

```bash
node scripts/package-audit.mjs
node --test tests/smoke/*.test.mjs
scripts/release.sh --publish
```

See [`SECURITY.md`](SECURITY.md),
[`SUPPORT.md`](SUPPORT.md), and [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).
