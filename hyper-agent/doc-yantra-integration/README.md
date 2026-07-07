# doc-yantra Engine — Review (consumer: DocKosha `native-pdf`)

**Scope:** Review of this repo (`samarthmn/doc-yantra@main` cd40251) as the npm package `@samarthmn/doc-yantra@0.0.2` consumed by DocKosha's `native-pdf` branch.

**Reviewed by:** Kosha Planner & Reviewer · Round 1 · 2026-07-07

The consumer-side review (integration wiring, feature parity, tests/CI, rollout checklist) lives in the DocKosha repo: `hyper-agent/doc-yantra-integration/` on branch `native-pdf` of `Sublime-Innovation-Technologies/doc-kosha`.

## Contents

| File | What's in it |
| --- | --- |
| `code-review-1/01-engine-review.md` | Verified facts (API surface, versions, workflow), findings Y-1…Y-8, release-readiness verdict |
| `code-review-1/02-release-and-ci-checklist.md` | Ordered actions: version reconciliation, CI test gate, docs fixes, publish hygiene |

## Verdict snapshot

The engine itself is in strong shape — clean Zone-1/Zone-2 feature gating, careful PDF-correctness engineering (encryption detection, resource-name collision avoidance, spec-cited merge semantics), honest documentation of deviations, and a real-file Node test suite that asserts watermarks are actually drawn. The package mechanics (exports map, publish workflow, committed pkg/) are sound.

What's not ready: the **version story** (core crate 0.0.1 vs wasm/pkg 0.0.2 vs docs telling consumers `^0.0.1`, which under caret-on-0.0.x semantics can never resolve 0.0.2), and the **total absence of a CI test gate** — a `wasm-v*` tag publishes a rebuild without ever running the 162(±) cargo tests or the Node package suite.
