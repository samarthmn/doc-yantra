# DocKosha DocYantra provider

`@samarthmn/dockosha-provider-docyantra` is the private adapter between
DocKosha's public `DocumentProcessingProvider` contract and the two proprietary
DocYantra WebAssembly engine packages. It owns the worker clients, typed failure
normalization, Office font provisioning, and the browser wasm-asset sync hook.

## Build and test

The engine packages are pinned to the private `0.0.17` releases that produced
DocKosha's frozen byte goldens. No adapter publication is needed for local
verification:

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

## Publishing is a maintainer gate

This package is deliberately marked `"private": true`; no automated workflow
publishes it. A maintainer who decides to release it must:

1. Replace `src/provider-interface.ts` with a dependency on the published
   `@dockosha/provider-interface` package and verify the contracts match.
2. Choose and commit an immutable version, and review the exact private engine
   dependency pins against DocKosha's byte-parity fixtures.
3. Run `pnpm test` and `npm pack --dry-run`, then review the tarball contents.
4. Remove the `"private": true` guard only in that reviewed release change,
   authenticate to the private GitHub Packages registry, and manually run the
   publish command.

Publishing, tagging, pushing, and registry authentication are intentionally
outside this extraction task.

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
