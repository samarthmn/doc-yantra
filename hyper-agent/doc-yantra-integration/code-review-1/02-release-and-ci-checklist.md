# 02 — Release & CI Checklist (engine repo)

Ordered actions to make this package production-grade for DocKosha and future consumers. Cross-references findings in `01-engine-review.md`.

## 1. Version reconciliation (Y-1, Y-3) — before the next tag

- [ ] Decide the line: **recommended `0.1.0`** (caret ranges behave intuitively on `0.x.y`; `^0.0.x` pins exact patch and has already bitten the docs).
- [ ] Bump **together**: `pdf-core/Cargo.toml`, `pdf-core/wasm/Cargo.toml`, rebuild so `pkg/package.json` matches, commit `pkg/`.
- [ ] Update every doc reference: `README.md` (`^0.0.1` → new range, twice), `CONSUMING.md` (§3 dependency snippet + "Local development" note), and DocKosha's `TESTING.md` (stale `link:` narrative — tracked as W-8 in the consumer review).
- [ ] Tag `wasm-v0.1.0`, confirm the publish workflow succeeds, then bump DocKosha's `package.json` to `^0.1.0` in a follow-up.
- [ ] Add to the release recipe in README: "bump BOTH Cargo.tomls" (today it only mentions `wasm/Cargo.toml`).

## 2. CI test gate (Y-2) — the biggest gap in this repo

Add `.github/workflows/test.yml`:

```yaml
name: test
on:
  push: { branches: [main] }
  pull_request:
jobs:
  rust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable   # or pin 1.89 (Y-4)
      - run: ./pdf-core/scripts/fetch-pdfium.sh
      - run: cargo test --manifest-path pdf-core/Cargo.toml
      - run: cargo test --manifest-path pdf-core/Cargo.toml --no-default-features --features wasm
  package:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install wasm-pack
      - run: cd pdf-core/wasm && ./build.sh
      - run: git status --porcelain pdf-core/wasm/pkg && test -z "$(git status --porcelain pdf-core/wasm/pkg)"   # committed pkg in sync
      - uses: actions/setup-node@v4
        with: { node-version: 22 }
      - run: node --test tests/
```

- [ ] Land it, confirm green, then make `publish-wasm.yml` either `needs:` these jobs (via `workflow_call`) or run the same test steps before `npm publish`.
- [ ] While in there: reconcile the test-count claims (README "162" vs tests/README "171") against actual `cargo test` output.

## 3. Toolchain & MSRV (Y-4)

- [ ] `rust-version = "1.89"` in both Cargo.tomls; align both READMEs to 1.89.
- [ ] Consider pinning the toolchain version in `publish-wasm.yml` for reproducible published artifacts.

## 4. API hardening for consumers (Y-5, Y-6, Y-7, Y-8)

- [ ] Document the panic/trap contract in CONSUMING.md ("a trap poisons the instance; recreate it; it is not a catchable JsError") — DocKosha's wrapper relies on this being understood.
- [ ] Add input-size guidance (and ideally boundary checks) for `merge_pdfs` / `csv_to_pdf` / image embedding; prioritize the deferred FlateDecode for `images_to_pdf`.
- [ ] Type `office_to_pdf` properly (`{ pdf: Uint8Array; warnings: string[] }`).
- [ ] Optional: surface a lossy-encoding warning when watermark/text generation degrades non-WinAnsi chars to `?`, so direct consumers can make the same routing decision DocKosha makes.
- [ ] Add `{"type":"module"}` marker under `pkg/web/` (belt-and-braces for non-conditional resolvers).

## 5. Consumer coordination (cross-repo)

Whenever the engine version bumps:

1. Rebuild + commit `pkg/`, tag `wasm-vX.Y.Z`, CI publishes.
2. Bump DocKosha `package.json` range + `pnpm install` (lockfile update) in a dedicated PR.
3. Remember the **browser copy**: DocKosha's `scripts/sync-wasm-web.mjs` copies `web/pdf_core_wasm_bg.wasm` out of `node_modules` at build time — server and browser engines update together only when the dependency bump lands. Preview-vs-server parity depends on this (consumer review, parity section).
4. Re-run DocKosha's unit suite (imports the real package) + the conversion e2e (engine-fingerprint assertions will catch producer changes).
