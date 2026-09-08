import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const node = process.execPath;
const licenseHash = createHash("sha256").update(readFileSync(join(root, "LICENSE"))).digest("hex");

function runFingerprint(...args) {
  return spawnSync(node, [join(root, "scripts", "source-fingerprint.mjs"), ...args], {
    cwd: root,
    encoding: "utf8",
  });
}

function runScript(script, ...args) {
  return spawnSync(node, [join(root, "scripts", script), ...args], {
    cwd: root,
    encoding: "utf8",
  });
}

test("all four package LICENSE files are byte-identical to the approved root license", () => {
  for (const relative of [
    "pdf-core/wasm/pkg/LICENSE",
    "office-core/wasm/pkg/LICENSE",
    "packages/doc-yantra-fonts/LICENSE",
    "packages/dockosha-provider/LICENSE",
  ]) {
    const bytes = readFileSync(join(root, relative));
    assert.equal(createHash("sha256").update(bytes).digest("hex"), licenseHash, relative);
    assert.deepEqual(bytes, readFileSync(join(root, "LICENSE")), relative);
  }
});

test("all packages declare their applicable licenses and carry the commercial licensing notice", () => {
  for (const relative of [
    "pdf-core/wasm/pkg",
    "office-core/wasm/pkg",
    "packages/doc-yantra-fonts",
    "packages/dockosha-provider",
  ]) {
    const manifest = JSON.parse(readFileSync(join(root, relative, "package.json"), "utf8"));
    const expectedLicense = relative === "packages/doc-yantra-fonts"
      ? "AGPL-3.0-only AND OFL-1.1"
      : "AGPL-3.0-only";
    assert.equal(manifest.license, expectedLicense, `${relative}: package metadata`);
    assert.deepEqual(
      readFileSync(join(root, relative, "COMMERCIAL-LICENSING.md")),
      readFileSync(join(root, "COMMERCIAL-LICENSING.md")),
      `${relative}: commercial notice bytes`,
    );
  }
});

test("generated artifact fingerprints reject modified WASM and metadata bytes", () => {
  for (const [target, artifact] of [
    ["pdf", "pdf-core/wasm/pkg/package.json"],
    ["office", "office-core/wasm/pkg/office_core_wasm_bg.wasm"],
  ]) {
    const path = join(root, artifact);
    const original = readFileSync(path);
    try {
      writeFileSync(path, Buffer.concat([original, Buffer.from([0])]))
      const rejected = runFingerprint(target, "--verify", `${artifact.split("/").slice(0, -1).join("/")}/build-fingerprint.json`);
      assert.notEqual(rejected.status, 0, `${target} mutation must be rejected`);
      assert.match(rejected.stderr, /fingerprint mismatch|generated artifact/i);
    } finally {
      writeFileSync(path, original);
    }
    const accepted = runFingerprint(target, "--verify", `${artifact.split("/").slice(0, -1).join("/")}/build-fingerprint.json`);
    assert.equal(accepted.status, 0, accepted.stderr);
  }
});

test("generated fingerprints include copied legal and adapter README sources", () => {
  for (const [target, relative, stamp] of [
    ["pdf", "THIRD_PARTY_NOTICES.md", "pdf-core/wasm/pkg/build-fingerprint.json"],
    ["office", "office-core/README.md", "office-core/wasm/pkg/build-fingerprint.json"],
    ["adapter", "packages/dockosha-provider/README.md", "packages/dockosha-provider/build-fingerprint.json"],
  ]) {
    const path = join(root, relative);
    const original = readFileSync(path);
    try {
      writeFileSync(path, Buffer.concat([original, Buffer.from("source mutation\n")]));
      const rejected = runFingerprint(target, "--verify", stamp);
      assert.notEqual(rejected.status, 0, `${target} legal/readme source mutation must be rejected`);
    } finally {
      writeFileSync(path, original);
    }
    const accepted = runFingerprint(target, "--verify", stamp);
    assert.equal(accepted.status, 0, accepted.stderr);
  }
});

test("generated WASM packages contain no absolute builder paths", () => {
  const forbidden = /(?:\/home\/[^/]+\/|\/Users\/[^/]+\/|[A-Za-z]:\\Users\\|\/__w\/[^/]+\/)/u;
  for (const relative of [
    "pdf-core/wasm/pkg/pdf_core_wasm_bg.wasm",
    "office-core/wasm/pkg/office_core_wasm_bg.wasm",
  ]) {
    assert.doesNotMatch(readFileSync(join(root, relative)).toString("latin1"), forbidden, relative);
  }
  for (const relative of ["pdf-core/wasm/build.sh", "office-core/wasm/build.sh"]) {
    assert.match(readFileSync(join(root, relative), "utf8"), /--remap-path-prefix/u, relative);
  }
});

test("Rust dependency notices are generated from locked WASM graphs and packaged", () => {
  const generator = readFileSync(join(root, "scripts/generate-rust-license-notices.sh"), "utf8");
  assert.match(generator, /--locked/u);
  assert.match(generator, /cargo-about 0\.9\.2/u);
  for (const [target, notice, build] of [
    ["pdf", "pdf-core/wasm/THIRD_PARTY_RUST_LICENSES.txt", "pdf-core/wasm/build.sh"],
    ["office", "office-core/wasm/THIRD_PARTY_RUST_LICENSES.txt", "office-core/wasm/build.sh"],
  ]) {
    const text = readFileSync(join(root, notice), "utf8");
    assert.ok(text.length > 100_000, `${target}: license inventory is unexpectedly small`);
    assert.match(text, /MIT License/u);
    assert.match(text, /Apache License/u);
    assert.match(readFileSync(join(root, build), "utf8"), /THIRD_PARTY_RUST_LICENSES\.txt/u);
  }
});

test("package audit rejects changed third-party notice bytes", () => {
  const notice = join(root, "pdf-core/wasm/pkg/THIRD_PARTY_NOTICES.md");
  const original = readFileSync(notice);
  try {
    writeFileSync(notice, Buffer.concat([original, Buffer.from("tampered\n")]));
    const result = runScript("package-audit.mjs");
    assert.notEqual(result.status, 0, "tampered package notice must fail package audit");
  } finally {
    writeFileSync(notice, original);
  }
});

test("package audit rejects a public-registry source for a private engine", () => {
  const lockfile = join(root, "packages/dockosha-provider/pnpm-lock.yaml");
  const stamp = join(root, "packages/dockosha-provider/build-fingerprint.json");
  const original = readFileSync(lockfile, "utf8");
  const originalStamp = readFileSync(stamp);
  try {
    writeFileSync(lockfile, original.replace(
      "resolution: {integrity:",
      "resolution: {tarball: https://registry.npmjs.org/private-engine.tgz, integrity:",
    ));
    const restamped = runFingerprint("adapter", "--out", stamp);
    assert.equal(restamped.status, 0, restamped.stderr);
    const result = runScript("package-audit.mjs");
    assert.notEqual(result.status, 0, "public-registry engine source must fail package audit");
  } finally {
    writeFileSync(lockfile, original);
    writeFileSync(stamp, originalStamp);
  }
});

test("package audit rejects registry URL spoof hosts", () => {
  const lockfile = join(root, "packages/dockosha-provider/pnpm-lock.yaml");
  const stamp = join(root, "packages/dockosha-provider/build-fingerprint.json");
  const originalLock = readFileSync(lockfile, "utf8");
  const originalStamp = readFileSync(stamp);
  for (const spoof of [
    "https://registry.npmjs.org.evil.invalid/packages.tgz",
    "https://registry.npmjs.org@evil.invalid/packages.tgz",
  ]) {
    try {
      writeFileSync(lockfile, `${originalLock}\n# untrusted mirror: ${spoof}\n`);
      const restamped = runFingerprint("adapter", "--out", stamp);
      assert.equal(restamped.status, 0, restamped.stderr);
      const result = runScript("package-audit.mjs");
      assert.notEqual(result.status, 0, `spoofed registry host must fail: ${spoof}`);
    } finally {
      writeFileSync(lockfile, originalLock);
      writeFileSync(stamp, originalStamp);
    }
  }
});

test("release verification leaves working tree and index untouched", () => {
  const before = spawnSync("git", ["status", "--short"], { cwd: root, encoding: "utf8" }).stdout;
  const result = spawnSync(join(root, "scripts", "release.sh"), [], { cwd: root, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  const after = spawnSync("git", ["status", "--short"], { cwd: root, encoding: "utf8" }).stdout;
  assert.equal(after, before);
});
