# Self-contained package checks

These smoke and audit checks use committed generated packages and synthetic
bytes. They do not require document fixtures or owner-supplied Word goldens.

```bash
node --test smoke-tests/*.test.mjs tests/font-cdn-tools.test.mjs tests/license-metadata.test.mjs
cargo test --manifest-path pdf-core/Cargo.toml --lib --no-default-features --features wasm
cargo metadata --manifest-path office-core/Cargo.toml --locked --no-deps --format-version 1
cargo check --manifest-path office-core/Cargo.toml --locked --lib --no-default-features --features wasm
```

Run the full package regression suite with `node --test tests/all.test.mjs`.
Owner-golden fidelity checks run separately with their original thresholds.
