# Contributing to DocYantra

DocYantra welcomes fixes and improvements that preserve its fail-closed
conversion contracts. Contributions are distributed under the AGPL-3.0-only
terms in [`LICENSE`](LICENSE). Enterprise licensing is available only through a
separate signed agreement; see [`COMMERCIAL-LICENSING.md`](COMMERCIAL-LICENSING.md).

Before opening a change:

1. Read `AGENTS.md` and the relevant package README.
2. Keep package versions lockstep and do not add fonts to default dependencies.
3. Add a focused test first for executable behavior, capture a meaningful red
   failure, then implement the smallest green change.
4. Run the practical Rust/Node test suites and package audits that your change
   touches. Do not publish, tag, or push from a contribution.

Do not include secrets, customer documents, proprietary fonts, or unresolved
real-world fixtures. Synthetic and rights-cleared fixtures must be documented
in `FIXTURE_PROVENANCE.md`.

## Licensing and CLA gate

Before an outside contribution can be merged, the project must have an
approved and activated CLA workflow. Until that workflow is active,
contributions are accepted only from the copyright owner or an authorized
representative under a separately documented written arrangement; an issue
comment, pull-request checkbox, or this file is not a signature. See the
[counsel-review CLA drafts](legal/cla/README.md). The drafts are not legal
advice and are not in force. Do not submit a contribution that relies on a
future commercial relicensing right until the workflow is activated.
