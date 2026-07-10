# Publish-Only GitHub Actions Design

**Date:** 2026-07-10
**Status:** Approved

## Goal

Move all build, lint, test, and fidelity work to the local release path. GitHub
Actions will only validate release identity and publish the committed npm
packages. This removes redundant Rust compilation and pdfium setup from hosted
runners.

## Workflow changes

Delete `.github/workflows/office-core-ci.yml`. The repository will no longer run
automatic checks for office-core pushes or pull requests.

Keep `.github/workflows/publish-wasm.yml` on the existing `0.0.*` tag trigger,
but reduce it to these steps:

1. Check out the tagged commit and full Git history with `actions/checkout@v7`.
2. Configure the GitHub Packages registry with `actions/setup-node@v6` and
   disable package-manager caching.
3. Reject an invalid tag, a commit outside `origin/main`, or a tag that differs
   from either committed npm package version.
4. Publish both committed `pkg/` directories with the existing immutable-version
   guards.

The current `node-version: 22` remains. It controls repository commands such as
`node -p` and `npm publish`; it is separate from each action's internal runtime.
The new action majors use Node.js 24 and remove the Node.js 20 deprecation
warning.

The publish workflow will not install Rust, rebuild wasm, fetch pdfium, or run
Cargo, fidelity, or Node test suites.

## Release flow

`pdf-core/scripts/release.sh` remains the supported release entry point. It
bumps all four Cargo manifests, rebuilds both committed npm packages, runs the
local engine/package/fidelity gates, commits the artifacts, creates the tag,
and atomically pushes `main` and the tag.

The tag workflow then publishes those exact committed artifacts. It does not
replace them with output from a second build.

## Failure handling

All tag, ancestry, and version checks run before either publish command. A
failed check publishes nothing. Each package keeps its `npm view` guard, so a
retry skips an immutable package version that already exists. This also allows
a rerun to finish if the first package published before the second package
failed.

The workflow retains `contents: read`, `packages: write`, the scoped GitHub
Packages registry, and `NODE_AUTH_TOKEN` from `GITHUB_TOKEN`.

## Documentation and verification

Update `README.md`, `CONSUMING.md`, workflow comments, and the release-script
test-gate comment to state that testing is local and GitHub only validates and
publishes.

Verify the change locally by parsing the workflow YAML, searching for removed
Node.js 20 action majors and hosted test/build commands, and running the
repository's release-contract test against the committed packages.
