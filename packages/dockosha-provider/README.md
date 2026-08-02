# DocKosha DocYantra provider

`@samarthmn/dockosha-provider-docyantra` is the private adapter between
DocKosha's public `DocumentProcessingProvider` contract and the two proprietary
DocYantra WebAssembly engine packages. It owns the worker clients, typed failure
normalization, Office font provisioning, and the browser wasm-asset sync hook.

Office conversions use one isolated Office worker per document. Runtime font
registration inside `office-core` is cumulative and bounded; reusing a worker
across unrelated documents can otherwise exhaust that registry after loading
several large regional CJK faces and silently select an earlier coverage
fallback. The verified parent-side font cache remains shared, so subsequent
workers re-register cached bytes without repeating successful font downloads.

## Build and test

The checked-in manifest uses exact private engine pins. The release script
aligns the adapter and both engines on one `0.0.x` version when it cuts a
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

The adapter releases with both engines on the same `0.0.x` version, release
commit, and tag. From a clean `main`, maintainers run the repository release
script; it bumps the adapter version, both engine dependency pins, and the
committed lockfile at release time:

```bash
scripts/release.sh
DRY_RUN=1 scripts/release.sh
```

The local release gate installs the freshly built engine packages by path with
`npm install --no-save`, then rebuilds `dist/` and runs the adapter tests without
requiring the not-yet-published engine version from the registry. The tag
workflow validates all three versions and publishes both engines before this
adapter, using the built-in `GITHUB_TOKEN`; CI publishes the committed `dist/`
without rebuilding it.

## DocKosha configuration

DocKosha reads two engine-related environment variables:

- `DOCKOSHA_ENGINE_PROVIDER_PACKAGE` — runtime module specifier for this
  adapter, normally `@samarthmn/dockosha-provider-docyantra`.
- `DOCKOSHA_ENGINE_TRACE_PACKAGES` — comma-separated packages Next.js must
  externalize and trace. A registry install should list this adapter plus
  `@samarthmn/doc-yantra` and `@samarthmn/doc-yantra-office`.

If the provider variable is unset or its module cannot be resolved, DocKosha
selects the community provider. The adapter also exports
`syncBrowserAssets()`, used by DocKosha's generic predev/prebuild hook to copy
the wasm-pack browser glue and binary into `public/wasm/`.
