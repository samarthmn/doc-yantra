# DocYantra and DocKosha Release Stabilization Design

## Goal

Make the DocYantra Office engine safe for hostile input, make every supported build and test configuration pass, and prove that DocKosha runs against the exact package bytes intended for publication. Preserve the canonical Word-fidelity results. Update older DocKosha page-count and byte goldens only after semantic and visual review confirms the new output.

## Scope

This work covers four release-blocking domains:

1. Office parser and resource safety in `office-core` and its Wasm wrapper.
2. Rendering correctness, bounded memory use, and bounded CPU work.
3. Reproducible package builds and a verified publishing workflow.
4. DocKosha dependency health, local package integration, and complete application verification.

The work keeps the public fail-closed contract. Supported documents produce the same fidelity-approved result. Inputs outside the documented envelope return a typed recoverable error. They never panic, exhaust memory, overflow the stack, or poison a Wasm worker.

## Architecture

### Shared resource limits

Add a focused `resource_limits` module to `office-core`. It owns checked cardinality helpers, the page budget, aggregate decoded-image bytes, generated-image variant bytes, and runtime-font registry limits. Keep ZIP-specific decompression limits in `opc`; the shared module covers work created after or alongside decompression.

Add a stable `UnsupportedCode::ResourceLimit` variant and map it to the existing Wasm `resource_limit` taxonomy. Keep `DecompressionLimit` for ZIP/XML expansion limits and the existing image-specific codes for image dimension or allocation failures. Syntax errors remain `Invalid`; a valid but excessive structure returns `Unsupported`.

Parser models validate document-controlled values before storing them. This includes section column counts, table spans, cell coordinates, numbering starts/restarts, tab-stop counts, and other values later used in ranges or allocations. All conversions and sums use checked arithmetic. The same parsers feed `profile()` and `convert()`, so both operations report the same stable reason code.

### XML and ZIP admission

Construct each XML DOM with `roxmltree::ParsingOptions.nodes_limit` instead of parsing without a limit and counting afterward. Preserve the existing depth scan and one-million-node public ceiling unless corpus measurement proves a lower ceiling accepts every supported fixture with at least fourfold headroom. Reject a limit hit as `DecompressionLimit` before the complete DOM exists.

Read the ZIP end-of-central-directory and ZIP64 metadata with a small bounded preflight before constructing `zip::ZipArchive`. Reject declared entry counts above 10,000 or inconsistent directory metadata. Continue enforcing the existing per-part, aggregate-byte, relationship-count, and relationship-depth limits during extraction.

### Page and layout work

Introduce a `PageBudget` with a production limit of 2,000 pages and a configurable limit for unit tests. Pass it through DOCX, PPTX, and XLSX renderers. Every operation that creates or inserts a page consumes the budget before allocation. The post-layout count remains as defense in depth.

Validate Word column counts and table spans against explicit engine limits before layout. Reject zero or overflowing XLSX row/column references, including `A0`, during cell-reference parsing. Restrict numbering starts and restarts to a supported finite range, use checked counter arithmetic, and format negative values without recursive negation. `i64::MIN` must return a typed refusal rather than recurse.

Replace repeated whole-collection `retain` scans in effective tab and numbering state with bounded, keyed operations. Tab precedence uses a sort/deduplicate or ordered-map pass. Numbering counters remain grouped by `num_id`, so resetting deeper levels touches only one list. Parse-time limits cap tab stops, numbering instances, and paragraph-driven state independently of the algorithmic improvement.

### Images and fonts

Budget real decoded bytes and conservative peak working-set bytes, not pixels alone. Count source images, retained decoded buffers, and generated variants. Check the budget before decoding or JPEG encoding whenever dimensions make the allocation predictable. Share or move retained image buffers instead of cloning full decoded images.

An image variant key contains only fields that change encoded bytes. Placement, extent, crop, rotation, and wrapping stay in layout state but do not create duplicate JPEG exports when encoding is identical. Collect variant keys in a bounded set and enforce the same limit during profile and conversion.

Cap the runtime font registry by face count and aggregate bytes before copying input. Store immutable font bytes through shared ownership so each profile or conversion does not clone every registered face. Duplicate registered faces remain idempotent. The public Wasm API returns `resource_limit` when a host exceeds the registry envelope.

## Error Flow

All new guards return `OfficeError::Unsupported` with a stable code and specific detail. The Wasm boundary serializes the result through the existing `{code, message, detail}` envelope. DocKosha classifies the machine code; it never parses prose. Tests assert both the Rust variant and Wasm taxonomy.

`profile()` must reject predictable limit violations before conversion. Conversion retains independent guards for dynamic layout and peak-memory limits that profile cannot calculate exactly. Such conversion-time refusals use the same `resource_limit` taxonomy and remain recoverable.

## Test Strategy

Every behavioral change follows a red-green-refactor cycle. Tests exercise parsers and small configurable budget helpers so the failing test cannot allocate hostile amounts of memory or trigger a real stack overflow.

Add focused tests for:

- excessive section columns and table spans;
- invalid and overflowing XLSX coordinates;
- extreme numbering starts, checked counter transitions, and `i64::MIN`;
- XML node admission during DOM construction;
- ZIP central-directory entry preflight;
- page-budget exhaustion in all three renderers;
- bounded, linear tab and numbering state;
- decoded-image and variant working-set limits;
- encoding-equivalent image placements sharing one variant;
- runtime-font count and byte limits;
- matching profile, native conversion, and Wasm error taxonomy.

Existing native, Wasm, package, fidelity, and DocKosha tests remain regression gates. Performance-sensitive tests assert bounded operations or state size rather than elapsed wall-clock time.

## Build and Publish Integrity

Pin Rust 1.96.1 in `rust-toolchain.toml`, pin wasm-pack and wasm-bindgen-compatible tooling, and make both build scripts refuse unpinned fallback execution. Pin privileged GitHub Actions to reviewed full commit SHAs.

The publish workflow performs these steps on the tagged commit:

1. Install the pinned toolchain and Wasm target.
2. Install checksum-verified build tooling.
3. Rebuild both packages with locked dependencies.
4. Run Rust, Wasm, package, and release-contract tests.
5. Compare regenerated `pkg/` trees byte-for-byte with the tagged trees.
6. Publish only the verified artifacts.

Add per-platform SHA-256 checks before extracting Pdfium. Replace mutable Fontsource `latest` catalog entries with exact versions and expected content digests. Generate third-party dependency notices for both npm package allowlists.

The local release gate builds each package twice from clean intermediate directories and requires identical Wasm hashes. Source fingerprints remain useful freshness metadata but cannot substitute for binary comparison.

## DocKosha Integration

Use DocKosha's supported paired `file:` overrides in `pnpm-workspace.yaml`; never use `pnpm link`. Rebuild DocYantra first, force a DocKosha install so hoisted copies refresh, run package-freshness checks, and synchronize browser Wasm before starting Next.js.

Upgrade DocKosha production dependencies in isolated groups: framework, document/PDF, mail, AWS/storage, and telemetry. Run unit, type, lint, and build checks after each group. The final production audit must contain no critical or high actionable findings. Fix lower-severity findings when compatible patches exist; document any remaining unpatched advisory with its runtime reachability and owner.

Regenerate the Office byte golden in a fresh worker. Accept a changed page count or hash only when the canonical fidelity corpus passes, extracted text and links remain correct, page dimensions remain correct, and rendered pages pass visual comparison. The approved fidelity-first output becomes the new DocKosha expectation.

After integration testing, restore registry dependency declarations and reinstall the immutable published package bytes. Confirm that manifests, lockfiles, module provenance, and `node_modules` contain no local `file:` or `link:` reference.

## Acceptance Gates

The work is ready to merge only when all of these conditions hold:

- `cargo fmt --check` passes for every Rust workspace.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` passes.
- Every native, no-default/Wasm, all-feature, wrapper, package, and fidelity test passes.
- The 21-case DOCX fidelity release gate reports no core failure and all extended gates pass.
- Cargo advisory checks contain no unaccepted vulnerability in a shipped path.
- Two clean package rebuilds produce identical bytes.
- Publish CI rebuilds and verifies the tagged package before publishing.
- DocKosha package-freshness, unit, browser, lint, format, typecheck, knip, production-build, and production-audit gates pass.
- Browser retries have an identified external cause or are eliminated; an unexplained retry fails the gate.
- Both repositories contain only intentional reviewed changes, and scratch artifacts are removed.

## Delivery Structure

Keep changes reviewable through separate commits for parser/resource admission, layout/image/font bounds, quality fixes, release integrity, DocKosha dependency upgrades, and fidelity-golden approval. Rebuild committed package artifacts only after source and toolchain changes settle. Run the complete cross-repository verification against those final bytes before merge or publication.
