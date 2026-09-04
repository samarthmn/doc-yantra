import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const AGPL_IDENTIFIER = "AGPL-3.0-only";
const FONT_PACKAGE_IDENTIFIER = "AGPL-3.0-only AND OFL-1.1";
const cargoManifests = [
  "pdf-core/Cargo.toml",
  "pdf-core/wasm/Cargo.toml",
  "office-core/Cargo.toml",
  "office-core/wasm/Cargo.toml",
  "text-core/Cargo.toml",
];
const npmPackages = [
  "pdf-core/wasm/pkg",
  "office-core/wasm/pkg",
  "packages/dockosha-provider",
  "packages/doc-yantra-fonts",
];

function read(relativePath) {
  return readFileSync(join(root, relativePath), "utf8");
}

test("the root license is the unmodified GNU AGPL version 3 text", () => {
  const license = read("LICENSE");
  assert.match(license, /GNU AFFERO GENERAL PUBLIC LICENSE\s+Version 3, 19 November 2007/u);
  assert.match(license, /Everyone is permitted to copy and distribute verbatim copies/u);
  assert.match(license, /<https:\/\/www\.gnu\.org\/licenses\/>/u);
  assert.doesNotMatch(license, /PolyForm|Enterprise|commercial licen[cs]/iu);
});

test("all first-party Cargo packages declare AGPL-3.0-only metadata", () => {
  for (const relativePath of cargoManifests) {
    const manifest = read(relativePath);
    assert.match(manifest, /^license\s*=\s*"AGPL-3\.0-only"\s*$/mu, relativePath);
    assert.doesNotMatch(manifest, /^license-file\s*=/mu, relativePath);
    assert.doesNotMatch(manifest, /source-available/iu, relativePath);
  }
});

test("all public npm packages carry AGPL metadata and the commercial notice", () => {
  const approvedLicense = readFileSync(join(root, "LICENSE"));
  const approvedCommercialNotice = readFileSync(join(root, "COMMERCIAL-LICENSING.md"));

  for (const packageRoot of npmPackages) {
    const manifest = JSON.parse(read(`${packageRoot}/package.json`));
    const expectedLicense = packageRoot === "packages/doc-yantra-fonts"
      ? FONT_PACKAGE_IDENTIFIER
      : AGPL_IDENTIFIER;
    assert.equal(manifest.license, expectedLicense, `${packageRoot}: license`);
    assert.ok(manifest.files.includes("LICENSE"), `${packageRoot}: LICENSE files entry`);
    assert.ok(
      manifest.files.includes("COMMERCIAL-LICENSING.md"),
      `${packageRoot}: commercial notice files entry`,
    );
    assert.deepEqual(
      readFileSync(join(root, packageRoot, "LICENSE")),
      approvedLicense,
      `${packageRoot}: approved LICENSE bytes`,
    );
    assert.deepEqual(
      readFileSync(join(root, packageRoot, "COMMERCIAL-LICENSING.md")),
      approvedCommercialNotice,
      `${packageRoot}: approved commercial notice bytes`,
    );
  }
});

test("generated-package inputs include the approved commercial notice", () => {
  const fingerprint = read("scripts/source-fingerprint.mjs");
  assert.match(fingerprint, /"COMMERCIAL-LICENSING\.md"/u);
  assert.match(
    fingerprint,
    /"packages\/dockosha-provider\/COMMERCIAL-LICENSING\.md"/u,
  );

  for (const relativePath of ["pdf-core/wasm/build.sh", "office-core/wasm/build.sh"]) {
    const script = read(relativePath);
    assert.match(script, /COMMERCIAL-LICENSING\.md/u, relativePath);
    assert.match(script, /j\.license\s*=\s*"AGPL-3\.0-only"/u, relativePath);
  }
});

test("engine packages ship their locked transitive Rust license inventories", () => {
  for (const [packageRoot, sourceNotice] of [
    ["pdf-core/wasm/pkg", "pdf-core/wasm/THIRD_PARTY_RUST_LICENSES.txt"],
    ["office-core/wasm/pkg", "office-core/wasm/THIRD_PARTY_RUST_LICENSES.txt"],
  ]) {
    const manifest = JSON.parse(read(`${packageRoot}/package.json`));
    assert.ok(manifest.files.includes("THIRD_PARTY_RUST_LICENSES.txt"), `${packageRoot}: files entry`);
    assert.deepEqual(
      readFileSync(join(root, packageRoot, "THIRD_PARTY_RUST_LICENSES.txt")),
      readFileSync(join(root, sourceNotice)),
      `${packageRoot}: generated Rust notice bytes`,
    );
  }
});
