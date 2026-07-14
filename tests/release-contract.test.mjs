import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function cargoPackageVersion(relativePath) {
  const source = readFileSync(join(root, relativePath), "utf8");
  const packageAt = source.search(/^\[package\]\s*$/m);
  assert.notEqual(packageAt, -1, `${relativePath}: missing [package] section`);
  const afterHeader = source.slice(packageAt).replace(/^\[package\]\s*$/m, "");
  const nextSection = afterHeader.search(/^\[/m);
  const packageSection = nextSection === -1 ? afterHeader : afterHeader.slice(0, nextSection);
  const version = packageSection?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  assert.ok(version, `${relativePath}: missing [package] version`);
  return version;
}

function packageVersion(relativePath) {
  return JSON.parse(readFileSync(join(root, relativePath), "utf8")).version;
}

test("all release surfaces carry one immutable version", () => {
  const versions = new Map([
    ["pdf-core", cargoPackageVersion("pdf-core/Cargo.toml")],
    ["pdf-core-wasm", cargoPackageVersion("pdf-core/wasm/Cargo.toml")],
    ["office-core", cargoPackageVersion("office-core/Cargo.toml")],
    ["office-core-wasm", cargoPackageVersion("office-core/wasm/Cargo.toml")],
    ["PDF npm package", packageVersion("pdf-core/wasm/pkg/package.json")],
    ["Office npm package", packageVersion("office-core/wasm/pkg/package.json")],
    ["PDF wasm engine", require(join(root, "pdf-core/wasm/pkg/pdf_core_wasm.js")).version()],
    [
      "Office wasm engine",
      require(join(root, "office-core/wasm/pkg/office_core_wasm.js")).version(),
    ],
  ]);
  const expected = versions.values().next().value;
  assert.match(
    expected,
    /^0\.0\.\d+$/,
    `release version must stay on the 0.0.x line: ${expected}`,
  );
  for (const [surface, version] of versions) {
    assert.equal(version, expected, `${surface} has release version ${version}, expected ${expected}`);
  }
});

test("office source fingerprint covers build-time font embedding", () => {
  const fingerprintSource = readFileSync(join(root, "scripts/source-fingerprint.mjs"), "utf8");
  const officeInputs = fingerprintSource.match(/office:\s*\[([\s\S]*?)\n\s*\],/u)?.[1];
  assert.ok(officeInputs, "scripts/source-fingerprint.mjs must declare office inputs");
  assert.match(
    officeInputs,
    /"office-core\/build\.rs"/u,
    "changing office-core/build.rs must invalidate the Office package fingerprint",
  );
});
