# Human tasks — office-engine (doc-yantra repo)

Everything here needs a human (local machine, Word license, judgment calls). Steps are ordered relative to the agent tasks.

## H1. Toolchain (BEFORE agent task 1)

1. Rust ≥ 1.89 with `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`.
2. `wasm-pack` installed (same version you already use for pdf-core releases).
3. One-time for the harness (task 4): `pdf-core/scripts/fetch-pdfium.sh` (pdfium dylib for rasterizing PDFs during scoring — dev-only, never shipped).
4. Expected outcome: `cargo --version` ≥ 1.89; `wasm-pack --version` prints.

## H2. Vendor the fallback fonts (BEFORE agent task 2, ~15 min)

Agent task 2 needs these files committed at `office-core/fonts/`. Download and commit (all SIL OFL 1.1 — include each family's LICENSE/OFL.txt next to the files):

1. Liberation family 2.1.x (Sans, Serif, Mono — Regular/Bold/Italic/BoldItalic, 12 TTFs) from the liberation-fonts GitHub releases.
2. Carlito (Regular/Bold/Italic/BoldItalic, 4 TTFs) — Google Fonts or the carlito package.
3. Caladea (4 TTFs) — same sources.
4. Layout: `office-core/fonts/liberation/*.ttf`, `office-core/fonts/carlito/*.ttf`, `office-core/fonts/caladea/*.ttf`, each with its license file.
5. Expected outcome: 20 TTFs + 3 license files committed. (Agents must not fetch binaries from the network; that's why this is a human step.)

## H3. Word ground-truth corpus (BEFORE task 4 scoring; grows continuously)

The harness diffs our output against **Microsoft Word's own PDF export**. Only a human with Word can produce these.

1. For every case under `fidelity-corpus/docx/**` (agent task 4 authors the source `.docx` files; you export them):
   - Open in Word for Mac (note exact version in the case's `manifest.toml`, e.g. `16.106.1`).
   - File → Save As → PDF → **"Best for electronic distribution and accessibility (uses Microsoft online service)"**.
   - Save as `fidelity-corpus/docx/<case>/ground-truth.pdf`.
2. Same for `pptx/**` (PowerPoint export) and `xlsx/**` (Excel: File → Save As → PDF, sheet scope = the case's manifest says which).
3. Commit the PDFs. The harness rasterizes them itself — do not commit PNGs.
4. When agents add new corpus cases later, repeat for just those cases (the harness `--missing-ground-truth` flag lists them).
5. Expected outcome: every corpus case directory contains `ground-truth.pdf`.

## H4. Review gates (AFTER agent tasks 3, 5, 6)

1. Run `cargo run -p fidelity -- run --format docx` (task 4 explains flags) and open `fidelity-report/index.html`.
2. Eyeball every case below threshold; decide per case: engine bug (file an issue / hand back to agent with the case name) vs threshold/ground-truth problem (adjust the case `manifest.toml`).
3. Sign-off criterion to declare a format shippable: **pass rate = 100% of `tier: core` cases, ≥ 90% of `tier: extended` cases** at SSIM ≥ 0.97.
4. Expected outcome: a short note in the PR/commit message of which tiers pass, so DocKosha rollout (that repo's human-tasks) knows what to enable.

## H5. Releases (AFTER agent task 7, then repeatedly)

1. `pdf-core/scripts/release.sh` (extended by task 7 to also build/commit `office-core/wasm/pkg`). It bumps 0.0.x, rebuilds both packages, commits, tags, pushes; the `publish-wasm.yml` workflow publishes **both** npm packages on the `0.0.*` tag.
2. First release after task 3+4 pass H4 (docx-only package is intended); release again after 5 and 6.
3. After each release: bump the pin in doc-kosha (`@samarthmn/doc-yantra-office`) — that repo's human-tasks step H3.
4. Expected outcome: `npm view @samarthmn/doc-yantra-office --registry=https://npm.pkg.github.com` shows the new version.

## H6. Ongoing: corpus flywheel

When DocKosha telemetry (that repo's plan) surfaces real-world fallback documents or shadow mismatches: reproduce with a **synthetic equivalent** (rebuild the structure in Word yourself — NEVER copy customer files or their text into this repo), add it to `fidelity-corpus/`, export ground truth (H3), and hand the failing case to a coding agent.

## No other human work

Everything else in this plan is agent-executable. There are no infrastructure, env-var, or third-party-dashboard steps in this repo (those all live in the doc-kosha plan).
