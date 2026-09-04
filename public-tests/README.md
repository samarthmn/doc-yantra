# Public self-contained checks

The public source candidate ships these smoke checks plus two public audit
checks in `tests/`. Together they use only committed generated packages and
synthetic bytes; they do not require private fixtures or owner-supplied Word
goldens.

```bash
node --test public-tests/*.test.mjs tests/font-cdn-tools.test.mjs tests/license-metadata.test.mjs
cargo test --manifest-path pdf-core/Cargo.toml --lib --no-default-features --features wasm
cargo metadata --manifest-path office-core/Cargo.toml --locked --no-deps --format-version 1
cargo check --manifest-path office-core/Cargo.toml --locked --lib --no-default-features --features wasm
```

The private regression and owner-golden suites remain in the working checkout
and keep their original thresholds. They are intentionally omitted from this
candidate until each input has an approved provenance record.
