# DocKosha DocYantra provider

`@samarthmn/dockosha-provider-docyantra` is the AGPL-3.0-only adapter between
DocKosha's public `DocumentProcessingProvider` contract and the two
DocYantra WebAssembly engine packages. It owns the worker clients, typed failure
normalization, Office font provisioning, and the browser wasm-asset sync hook.

Office conversions use one isolated Office worker per document. Runtime font
registration inside `office-core` is cumulative and bounded; reusing a worker
across unrelated documents can otherwise exhaust that registry after loading
several large regional CJK faces and silently select an earlier coverage
fallback. The verified parent-side font cache remains shared, so subsequent
workers re-register cached bytes without repeating successful font downloads.

## Build and test

The checked-in manifest uses exact public engine pins and an optional exact
font-pack peer. The release script aligns the adapter, both engines, and font
package on one `0.0.x` version when it cuts a
lockstep release. For ordinary local verification against the currently pinned
engines:

```bash
cd packages/dockosha-provider
pnpm install
pnpm test
```

The TypeScript build emits CommonJS JavaScript and declarations under `dist/`.
From DocKosha, install it transiently without changing tracked manifests:

```bash
npm install --no-save /absolute/path/to/doc-yantra/packages/dockosha-provider
```

## Publishing in lockstep

The adapter releases with both engines and the optional font pack on the same
immutable `0.0.x` version. Maintainers run the repository preparation gate,
then use the owner-only public npm workflow after reviewing the audited
tarballs:

```bash
scripts/release.sh
```

The gate audits the committed `dist/` against its source fingerprint and packs
all four candidates without changing Git or registry state. The owner-only
workflow performs the separate public npm publish with provenance after checking
that the immutable candidate version does not already exist.

## Font delivery

`createProvider()` remains argument-free. For Office requests, it keeps the
existing CJK region and serif/sans rules but resolves only the selected exact
font. Resolution is memory → persistent cache (when configured by the host) →
installed `@samarthmn/doc-yantra-fonts` → immutable first-party HTTPS asset.
Every artifact is checked against the bundled manifest before registration;
failures remain fail-closed. Install the optional font peer at the same exact
version for network-free conversion.

## DocKosha integration

DocKosha declares this adapter and the two DocYantra engine packages as direct
dependencies at the same exact version. There is no provider-selection
environment variable or fallback provider. The adapter also exports
`syncBrowserAssets()`, used by DocKosha's generic predev/prebuild hook to copy
the wasm-pack browser glue and binary into `public/wasm/`.
