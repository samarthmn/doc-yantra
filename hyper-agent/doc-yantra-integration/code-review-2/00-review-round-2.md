# Engine Review Round 2 — Fix Verification

**Reviewed:** `main` @ `faee2eb` (3 commits since round 1: `cb611f4` "wasm: v0.0.3", `a69e52d` docs/CLAUDE.md, `faee2eb` tag-scheme change). Consumer-side round 2: `hyper-agent/doc-yantra-integration/code-review-2/` on doc-kosha `native-pdf`.

**Owner decision applied:** CI will not be implemented for now — Y-2 is **waived**; see R2-Y1 for the cheap in-script substitute.

## Verdict

Every code/docs finding from round 1 is resolved, and the 0.0.3 release actually shipped: the tag exists (`wasm-v0.0.3`, published under the old trigger during the scheme switch) and — the strong proof — doc-kosha's lockfile now records `@samarthmn/doc-yantra@0.0.3` resolved from `npm.pkg.github.com` with a sha512 integrity hash. The new `release.sh` makes the whole version story mechanical and immutable. Remaining: one should-fix (tests inside `release.sh`, since CI is waived) and one docs nit.

## Round-1 finding resolution

| ID | Round-1 severity | Status | Evidence |
| --- | --- | --- | --- |
| Y-1 `^0.0.1` can never resolve 0.0.2 | Blocker | ✅ **Resolved** | Coherent policy adopted and documented: releases stay on the 0.0.x line, versions are immutable, consumers **pin the exact patch** ("a caret on 0.0.x resolves only that exact version" — CONSUMING.md §3). `release.sh` enforces monotonic bumps and refuses tag reuse. doc-kosha pins 0.0.3 and resolves it (lockfile integrity + tarball verified). Style nit on the consumer's caret → doc-kosha R2-3. |
| Y-2 no CI test gate | Should-fix (high) | 🟡 **Waived by owner** | Publish workflow still builds + publishes without tests. Substitute → **R2-Y1**. |
| Y-3 core/wasm version desync | Should-fix | ✅ **Resolved** | Both Cargo.tomls at `0.0.3`; `release.sh` writes the version into **both** (`set_version` on each) and the docs state `version()` returns the wasm crate's. |
| Y-4 MSRV inconsistent/unenforced | Should-fix | ✅ **Resolved** | `rust-version = "1.89"` in both `pdf-core/Cargo.toml` and `pdf-core/wasm/Cargo.toml` — cargo now enforces it; docs aligned. |
| Y-5 panic traps undocumented | Should-fix | ✅ **Resolved** | CONSUMING.md gained a "Panic / trap contract" section: `Err`→`JsError` (recoverable, instance reusable) vs panic/OOM→trap (fatal, **discard and recreate**), with the size-reject rationale. The consumer-side enforcement gap (evicting a trapped instance) is tracked as doc-kosha **R2-1**. |
| Y-6 no input-size guard | Should-fix | ✅ **Resolved** | `guard_input_size` in `wasm/src/lib.rs`: 100 MiB/document, 200 MiB/merge set, checked via `Uint8Array.length` **before** copying into linear memory, thrown as a normal `JsError` with a "use the HTTP service" message. Applied to every export (merge, watermark, csv, md, text, page_count, validate, office). Backstop sits above DocKosha's own 25/100 MB caps — correct layering. |
| Y-7 `office_to_pdf` typed `any` | Nit | ✅ **Resolved** | `#[wasm_bindgen(typescript_custom_section)]` `OfficeResult { pdf: Uint8Array; warnings: string[] }` + `typescript_type` extern + `unchecked_into` — the office `.d.ts` now exports the typed shape. |
| Y-8 docs drift + module-format lean | Nit | ✅ **Resolved** | `pkg/web/package.json` = `{"type":"module"}` committed (build.sh updated to emit it); CONSUMING.md/README rewritten for the registry + exact-pin reality; the stale `link:` claim about DocKosha is gone ("DocKosha ships against the published registry package"). Residual example-lag → R2-Y2. |

## New findings (round 2)

### R2-Y1 · [SHOULD-FIX] `release.sh` should run the tests it is now the only gate for

With CI waived, `release.sh` is the sole thing standing between a regression and a published, **immutable** version. It currently bumps → rebuilds → commits → tags → pushes without running a single test. Insert between the rebuild and the commit:

```bash
# ─── 2b. Gate: this script is the only test gate (no CI by decision) ────────
echo "  running cargo tests ..."
cargo test --quiet
cargo test --quiet --no-default-features --features wasm
echo "  running package tests against the rebuilt pkg ..."
( cd "$REPO_ROOT" && node --test tests/ )
```

(`cargo test` needs pdfium fetched once — `./scripts/fetch-pdfium.sh`; fail with a clear message if `pdfium/` is absent rather than mid-suite.) This keeps the no-CI decision intact while making an untested publish impossible by the supported path. Note `workflow_dispatch` and a manual `npm publish` still bypass it — acceptable residual risk, worth a comment in the workflow.

### R2-Y2 · [NIT] Doc examples pin a version that will always lag

CONSUMING.md §3 shows `"@samarthmn/doc-yantra": "0.0.2"` and the git-install alternative pins `#0.0.2`; current is 0.0.3 and next week it won't be. Phrase examples as `"0.0.x — the latest tag"` (or add a one-liner: `git tag --list '0.0.*' | sort -V | tail -1`) so the doc never rots by one patch. Same for the two `0.0.2` mentions in the local-development section.

### Release-flow consistency check (info, verified)

- `release.sh` scans **both** tag forms (`0.0.*` and legacy `wasm-v0.0.*`) for the highest patch — so the next auto release is 0.0.4, not a duplicate.
- New tags are bare `0.0.x`; `publish-wasm.yml` (at current main) triggers on `0.0.*`. GitHub uses the workflow file at the tagged commit, and every future tag will contain `faee2eb`'s trigger — consistent end-to-end.
- `wasm-v0.0.3` published under the old trigger during the switch; the registry artifact is live (consumer lockfile proves resolution + integrity). No action needed.
- Dry-run mode, immutability guards, and both-toml bumping are all present and correct in the script.

## What's solid (round 2)

- The size guards check `Uint8Array.length` on the JS side **before** `to_vec()` copies — the rejection genuinely happens outside linear memory, which is the whole point.
- The trap contract is written for the consumer that actually exists (names `src/server/pdfCoreWasm.ts` and its lower limits) rather than as abstract advice.
- `release.sh` is careful shell: `set -euo pipefail`, tag-collision and monotonicity checks, awk-based single-line version rewrite, DRY_RUN mode, and it pushes main + tag atomically in one command.
- The web ESM marker closes the last resolver edge case from round 1 without touching the exports map.
