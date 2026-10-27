import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const githubRegistry = "https://npm.pkg.github.com";

function readJson(path) {
  return JSON.parse(readFileSync(join(root, path), "utf8"));
}

test("all DocYantra packages publish only to GitHub Packages", () => {
  for (const path of [
    "pdf-core/wasm/pkg/package.json",
    "office-core/wasm/pkg/package.json",
    "packages/doc-yantra-fonts/package.json",
    "packages/dockosha-provider/package.json",
  ]) {
    assert.equal(readJson(path).publishConfig?.registry, githubRegistry, path);
  }
});

test("a 0.0.x tag publishes through GitHub Packages and creates a GitHub release", () => {
  const workflow = readFileSync(join(root, ".github/workflows/publish-npm.yml"), "utf8");
  const timeoutMinutes = Number(workflow.match(/timeout-minutes:\s*(\d+)/u)?.[1]);
  assert.ok(timeoutMinutes >= 60, `release job timeout must cover cold Rust builds, found ${timeoutMinutes} minutes`);
  assert.match(workflow, /push:\s*\n\s+tags:\s*\n\s+- ["']0\.0\.\*["']/u);
  assert.match(workflow, /registry-url:\s*https:\/\/npm\.pkg\.github\.com/u);
  assert.match(workflow, /packages:\s*write/u);
  assert.match(workflow, /NODE_AUTH_TOKEN:\s*\$\{\{ secrets\.GITHUB_TOKEN \}\}/u);
  assert.match(workflow, /node scripts\/publish-npm\.mjs/u);
  assert.doesNotMatch(workflow, /node --test|cargo test/u, "test suites run outside publishing");
  assert.match(workflow, /gh release (?:create|upload)/u);
  assert.match(
    workflow,
    /npm install --ignore-scripts --no-save --package-lock=false --registry=https:\/\/registry\.npmjs\.org/u,
    "adapter build dependencies must not inherit the GitHub Packages publish registry",
  );
  assert.doesNotMatch(
    workflow,
    /\.\/(?:pdf|office)-core\/wasm\/build\.sh/u,
    "the tag job must publish the reviewed artifacts instead of replacing them with runner-specific WASM",
  );
  assert.doesNotMatch(
    workflow,
    /registry-url:\s*https:\/\/registry\.npmjs\.org|npm publish[^\n]*registry\.npmjs\.org|npm-production|NPM_TOKEN/u,
  );
});

test("continuous readiness checks validate committed WASM without cross-runner byte comparisons", () => {
  const workflow = readFileSync(join(root, ".github/workflows/publish-wasm.yml"), "utf8");
  assert.match(workflow, /source-fingerprint\.mjs pdf --verify/u);
  assert.match(workflow, /source-fingerprint\.mjs office --verify/u);
  assert.doesNotMatch(workflow, /\.\/(?:pdf|office)-core\/wasm\/build\.sh/u);
});

test("the release script can safely push the prepared version tag", () => {
  const release = readFileSync(join(root, "scripts/release.sh"), "utf8");
  assert.match(release, /--publish/u);
  assert.match(release, /git tag "\$VERSION"/u);
  assert.match(release, /git push --atomic origin main "\$VERSION"/u);
});
