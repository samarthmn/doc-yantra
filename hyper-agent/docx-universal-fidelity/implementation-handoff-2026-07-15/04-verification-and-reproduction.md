# Verification and reproduction

## Frozen contracts

Do not change these to make the run green:

- `ssim_threshold = 0.90` for the three new core cases;
- exact page-count delta zero;
- committed source DOCX and ground-truth PDF hashes;
- provenance strings and Word-online matcher requirements;
- existing `complex` regression threshold and page count.

Expected pages:

| Case | Pages |
| --- | ---: |
| `gdocs-anchors-toc` | 10 |
| `gdocs-tables-longdoc` | 44 |
| `gdocs-columns` | 3 |
| `complex` | 6 |

At stop time, `git diff -- fidelity-corpus` was empty.

## Resume preflight

From the repository root:

```bash
git status --short
git diff --check
git diff -- fidelity-corpus
git diff -- pdf-core
```

Expected: the stopped 21-file worktree diff is present; `fidelity-corpus` and
`pdf-core` source diffs are empty.

## Native verification

```bash
cd office-core
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
```

The final stopped source has only completed the focused 84-test
`docx_layout_test` run. Record the complete group counts and failures before
editing any expectation.

## Wasm verification

```bash
cd office-core
cargo test --locked --target wasm32-unknown-unknown --no-run \
  --no-default-features --features wasm,bundled-fonts
cd wasm
./build.sh
```

Then verify that `pkg/build-fingerprint.json` matches the live source. The
currently checked-in wasm binaries predate the final numbering/wrap edits.

## Node package verification

From the repository root after rebuilding wasm:

```bash
node --test tests/
```

The previous package run had two failures to investigate rather than mask:

1. deterministic complex output hash drift;
2. comparison-report `tabs-leaders` classification drift.

Inspect the rebuilt output before deciding whether either expected value should
change. The only intentional Node allowlist deletion in the stopped patch is
`superscript_synthesized`, which production never emits.

## Fidelity verification

```bash
cd office-core
cargo run -p fidelity -- missing-ground-truth
cargo run -p fidelity -- run --format docx
```

For focused iteration:

```bash
cargo run -p fidelity -- run --case gdocs-anchors-toc
cargo run -p fidelity -- run --case gdocs-tables-longdoc
cargo run -p fidelity -- run --case gdocs-columns
cargo run -p fidelity -- run --case complex
```

Do not claim completion until the aggregate run passes the unchanged gates and
an independent reviewer confirms no threshold/golden/manifest relaxation.

## Release dry run

The release script intentionally requires clean `main`, so it will refuse the
feature branch. After merge and H3 approval:

```bash
DRY_RUN=1 pdf-core/scripts/release.sh
```

Do not execute a real release from this handoff state.

## Diagnostic artifacts retained

The task was stopped before completion, so the repository `tmp/` diagnostics
were retained for the next engineer. Important paths include:

```text
tmp/phase5-report.md
tmp/phase5-final-alpha-fidelity/
tmp/phase5-after-topbottom-complex/
tmp/phase5-after-geometric-anchors/
tmp/phase5-after-numbering-longdoc/
tmp/phase5-review-final2-anchors/
```

These files support the score and geometry findings in this handoff. Delete
only these task-created artifacts after the remaining gates pass and their
useful evidence has moved into committed documentation.
