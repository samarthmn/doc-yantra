# Agent tasks 7 — WASM npm package, release integration, docs

Prereq: task 3 (first release can be docx-only; pptx/xlsx flow in via later releases with zero packaging changes). Mirror `pdf-core/wasm/` conventions exactly — read `pdf-core/wasm/build.sh`, `pdf-core/wasm/Cargo.toml`, and `pdf-core/wasm/src/lib.rs` first and copy their idioms (size guards, JsError mapping, panic hook).

## T7.1 wasm crate — `office-core/wasm/`

- `office-core/wasm/Cargo.toml`: package `office-core-wasm`, same version line (0.0.x), `crate-type = ["cdylib"]`, deps: `office-core = { path = "..", default-features = false, features = ["wasm", "bundled-fonts"] }`, `wasm-bindgen`, `js-sys`, `console_error_panic_hook`, `getrandom` with `wasm_js` (match whatever pdf-core/wasm does for getrandom).
- `office-core/wasm/src/lib.rs` exports:

```rust
#[wasm_bindgen]
pub fn version() -> String;                       // env!("CARGO_PKG_VERSION")

/// DocProfile JSON (plan README contract). NEVER throws for well-formed calls:
/// container-level failures serialize via DocProfile::from_error().
/// Oversized input (> 50 MiB) returns the InputTooLarge profile BEFORE copying.
#[wasm_bindgen]
pub fn office_profile(bytes: &[u8], extension: &str) -> Result<String, JsError>;

/// PDF bytes. Throws JsError with message "unsupported:<code>: <detail>" for
/// envelope failures (the "unsupported:" prefix is parsed by DocKosha — stable API),
/// "invalid: …" / "internal: …" otherwise. Same 50 MiB pre-copy size guard.
#[wasm_bindgen]
pub fn office_to_pdf(bytes: &[u8], extension: &str) -> Result<Vec<u8>, JsError>;

/// Adds a font to the process-global FontDb (browser builds ship the same
/// bundled fonts; this is for brand fonts). Errors on unparseable font bytes.
#[wasm_bindgen]
pub fn register_font(bytes: &[u8]) -> Result<(), JsError>;
```
Trap contract: identical to pdf-core (document in the package README: Err→JsError recoverable; panic/OOM → trap → discard instance). The engine's global FontDb lives in a `OnceLock<Mutex<FontDb>>` initialized with bundled fonts on first use — wasm is single-threaded, the Mutex is uncontended.

- `office-core/wasm/build.sh`: copy `pdf-core/wasm/build.sh` and adjust: crate dir, `PKG_NAME="doc-yantra-office"`, `SCOPE="samarthmn"`, same two targets (`--target nodejs` main + `--target web` under `web/`), same package.json post-processing (name `@samarthmn/doc-yantra-office`, `main`, `exports` incl. `./web`, `files`, `publishConfig.registry` = npm.pkg.github.com). **Committed `pkg/`** — same rule as pdf-core (no Rust toolchain on consumers/CI).
- Size budget: assert in build.sh that the nodejs `.wasm` is < 25 MB (engine + 20 bundled fonts; fonts dominate — if over budget, subset the bundled fonts to WinAnsi+Latin-Extended coverage at vendor time and note it in the README). Print the size in the build output.

## T7.2 Node test suite — repo root `tests/`

Follow the existing pattern (`node --test tests/`, loads built `pkg/` by path — see `tests/README.md`):
- `tests/office.test.js`: `version()` matches Cargo version; `office_profile` on `mock-files/*.docx` returns `supported:true` JSON; on a `.pptx` **before task 5 lands** returns `format_not_implemented` (update this assertion when 5/6 merge); `office_to_pdf` returns bytes starting `%PDF-`, deterministic across two calls (byte-equal); unsupported fixture throws message starting `unsupported:`; oversized buffer (51 MiB of zeros) → InputTooLarge profile without throwing; `register_font` with garbage throws, with a real TTF succeeds.

## T7.3 Publish workflow + release script

- `.github/workflows/publish-wasm.yml`: extend the existing job (do NOT create a second workflow) to also `npm publish` from `office-core/wasm/pkg` on the same `0.0.*` tags, same GITHUB_TOKEN/`packages: write`. Keep pdf-core's publish exactly as-is; office publish step runs after it; a failure in the office step must not un-publish pdf-core (steps are independent; document that a partial publish is fixed by re-running the job — npm refuses double-publish of the same version, so guard with `npm view @samarthmn/doc-yantra-office@$VERSION` skip-if-exists).
- `pdf-core/scripts/release.sh`: extend to (a) bump `office-core/Cargo.toml` + `office-core/wasm/Cargo.toml` to the same 0.0.x it computes today, (b) run `office-core/wasm/build.sh`, (c) `git add office-core` alongside `git add pdf-core`. Keep the script's existing guarantees (refuses tag reuse, DRY_RUN). All four Cargo.tomls + both pkg/package.json stay on one version line.

## T7.4 Docs

- Repo `README.md`: add an `office-core/` row to the components table + a "Office → PDF" quick-start (CLI + npm snippets, envelope summary, link to `hyper-agent/office-engine/README.md` for the contract).
- `CLAUDE.md`: add office-core section — the Zone-style split now reads: pdf-core Zone 1/2 (unchanged) + office-core (pure Rust, wasm-able, fail-closed envelope, DocProfile contract); the release/versioning gotchas now cover four Cargo.tomls; wasm builds use `--no-default-features --features wasm,bundled-fonts`.
- `CONSUMING.md`: add `@samarthmn/doc-yantra-office` install/usage section (same registry/token flow; document the `unsupported:` error prefix and the 50 MiB guard; note DocKosha's adapter is `src/server/officeCoreWasm.ts`).
- `office-core/README.md`: scope, envelope tables (copy from task files 3/5/6), speed table (fill with measured numbers from criterion benches — add `benches/convert.rs` mirroring pdf-core's bench setup), design notes, LAYOUT_CALIBRATION.md pointer.

## T7.5 Deliberately deferred (record, don't build)

Add a "Deferred" section to `office-core/README.md`: N-API native addon (napi-rs v3 — wasm CJS suffices for DocKosha on Vercel today; native addon becomes worthwhile for a self-hosted service or >50 MiB docs), PDF/A-2b output, complex-script shaping via harfrust, charts (own renderer), in-browser font-fetch strategy for slim web builds. Each with one line of rationale so future planning starts from context, not archaeology.

## Local verification

1. `cd office-core/wasm && ./build.sh` — builds both targets; size line printed and under budget; `pkg/` diff committed.
2. `node --test tests/` — all green against the freshly built pkg.
3. `DRY_RUN=1 pdf-core/scripts/release.sh` — shows both crates bumping in lockstep, both pkg rebuilds staged.
4. Tag-push a `0.0.x` (real release, human H5) → workflow publishes BOTH packages; `npm view @samarthmn/doc-yantra-office versions --registry=https://npm.pkg.github.com` lists it.
