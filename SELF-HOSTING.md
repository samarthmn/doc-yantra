# Running DocYantra on your own infrastructure

DocYantra is the open-source Rust and WebAssembly document-processing engine
used by [DocKosha](https://github.com/samarthmn/doc-kosha). First-party engine
and wrapper code is licensed under [AGPL-3.0-or-later](LICENSE); see
[third-party notices](THIRD_PARTY_NOTICES.md) for dependency and font terms.
Enterprise licensing requires a separate signed agreement.

DocYantra is embedded in a host application. It is not an HTTP conversion
service and does not need a separately deployed server. To self-host the full
sharing application, use [DocKosha's setup walkthrough](https://github.com/samarthmn/doc-kosha/blob/HEAD/docs/self-hosting.md)
and [public self-hosting overview](https://www.dockosha.com/self-hosting).
[DocKosha Cloud](https://www.dockosha.com) is the managed alternative.

## Install the versioned packages

Consumers do not need Rust or native pdfium. Use the runtime supported by your
host application; DocKosha pins Node.js 24.x and pnpm 11.25.0. DocYantra's own
package test baseline requires Node.js 18 or newer.

All four package versions must match; the current release is `0.0.28`:

| Package                                  | Purpose                                              |
| ---------------------------------------- | ---------------------------------------------------- |
| `@samarthmn/doc-yantra`                  | PDF operations; Node CJS and browser ESM             |
| `@samarthmn/doc-yantra-office`           | Fontless Office profile and PDF conversion           |
| `@samarthmn/dockosha-provider-docyantra` | Adapter used by DocKosha                             |
| `@samarthmn/doc-yantra-fonts`            | Optional offline font catalog; installed by DocKosha |

Configure the `@samarthmn` registry as `https://npm.pkg.github.com`. For pnpm
11, configure `registries` in `pnpm-workspace.yaml` as shown in
[CONSUMING.md](CONSUMING.md#install). GitHub Packages requires authentication
for public npm packages: use an expiring personal access token (classic) with
`read:packages` and access to the packages. Fine-grained tokens are not supported
for npm package authentication.

Run `npm config edit --location=user` and store
`//npm.pkg.github.com/:_authToken=<your-token>` in the user configuration, or use
protected CI secrets. Never put credentials in source, logs, or an example
command that records a real token in shell history. Authentication is a package
download requirement, not a license fee or a processing-network requirement.

Add only the packages your host needs, pinned to `0.0.28`, then install them
with your package manager. DocKosha users should keep its existing manifests
and run `pnpm install --frozen-lockfile` instead of adding dependencies manually.

## Embed the engine

The PDF package's root export is Node CJS. A minimal font-independent check is:

```js
const pdf = require("@samarthmn/doc-yantra");
const bytes = pdf.text_to_pdf("Hello from DocYantra");
if (!(bytes instanceof Uint8Array) || bytes.length === 0) {
  throw new Error("Expected PDF bytes");
}
```

The browser entry is `@samarthmn/doc-yantra/web`; initialize its WebAssembly
module as documented in [CONSUMING.md](CONSUMING.md). Bundle or serve its wasm
assets with your application rather than treating the Node entry as browser
code. Office conversion uses the separate Office package. Discover requested
fonts, register validated font bytes, then profile and convert the document.

Use the packaged worker APIs for untrusted or expensive work and impose finite
deadlines, input limits, and bounded concurrency. DocKosha's adapter already
owns that integration. No native pdfium, LibreOffice, Chromium, or external
conversion endpoint is part of the wasm processing path.

## Offline fonts and network access

Font-independent operations such as merge, validation, page counting, and
redaction require no font I/O. Unicode text and supported Office conversion
may need exact font faces. The font resolver can use memory/persistent caches,
the matching offline pack, or immutable first-party HTTPS font assets.

For an offline deployment, install `@samarthmn/doc-yantra-fonts` at `0.0.28`
and verify your bundler ships the pack alongside the engine and worker assets.
A custom host can instead supply a validated font provider. Fontless packages
alone do not imply all Unicode conversion works without network access.
See [CONSUMING.md](CONSUMING.md) for the host API and
[Office engine documentation](office-core/README.md) for the supported envelope.

## Build from source

Clone [the repository](https://github.com/samarthmn/doc-yantra) and use Rust
1.89 or newer. The build scripts require `wasm-pack`; follow the engine README
for toolchain setup. Run these from the repository root:

```bash
cd pdf-core/wasm
./build.sh
cd ../../office-core/wasm
./build.sh
cd ../..
node scripts/package-audit.mjs
node --test tests/smoke/*.test.mjs
node --test tests/node/all.test.mjs
```

The wasm builds use Zone 1 pure Rust and exclude pdfium and native parallel
threads. They regenerate committed `pkg/` output; review that output with the
source change. Adapter changes also require rebuilding its committed `dist/`
using the package's build command. All four packages release in lockstep.

For isolated consumer experiments, build and pack the packages into your own
repository-local `tmp/` project. Do not change DocKosha's committed dependency
pins to sibling, `file:`, or `link:` dependencies. A full native PDF-engine
build uses a different path: Zone 2 requires a platform-specific pdfium library
and serialized access. See [pdf-core/README.md](pdf-core/README.md); pdfium is
not required for the wasm deployment described here.

## Verify, upgrade, and handle failures

Test the exact package set that will run in production. Verify a small PDF
operation, supported Office conversion with fonts, watermarking, and redaction
with synthetic fixtures. Verify unsupported and oversized inputs fail closed.
Test in an environment with outbound access disabled before calling a deployment
offline. For the complete app, also run DocKosha's engine bundle verification
and its shared-link smoke checks.

Unsupported document features return typed failures; do not silently substitute
a different converter or omit visible content. Redaction removes supported
content through the engine, not a visual overlay. A WebAssembly panic/trap can
poison the instance; use worker isolation and the recovery rules in
[CONSUMING.md](CONSUMING.md#safety-and-limits). Do not reuse a poisoned instance.

On upgrades, change all required package versions together, retain your last
verified build, rerun package/host checks, and review the documented envelope.
When using DocKosha, retain its original-file behavior for failed previews and
its blocked-delivery behavior for required watermark failures.

For reproducible issues, use the repository with sanitized diagnostics and
synthetic examples. Community support has no SLA or deployment commitment;
see [SUPPORT.md](SUPPORT.md). Never attach customer documents or credentials.
