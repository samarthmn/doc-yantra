import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { auditBinaryProvenance, auditPublicSource, auditSecretText, exportPublicSource, validateProvenanceData } from "../scripts/export-public-source.mjs";

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

test("all public packages declare their applicable licenses and carry the commercial licensing notice", () => {
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

test("public source exporter rejects common secret content and unsafe destinations", () => {
  const scratch = join(root, "tmp", `public-export-${process.pid}`);
  const source = join(scratch, "source");
  mkdirSync(join(source, "scripts"), { recursive: true });
  writeFileSync(join(source, "LICENSE"), readFileSync(join(root, "LICENSE")));
  writeFileSync(join(source, "THIRD_PARTY_NOTICES.md"), "notices\n");
  writeFileSync(join(source, "FIXTURE_PROVENANCE.json"), readFileSync(join(root, "FIXTURE_PROVENANCE.json")));
  writeFileSync(join(source, "FIXTURE_PROVENANCE.md"), "mock-files fidelity-corpus real-world-test-files\n");
  writeFileSync(join(source, "scripts", "ok.mjs"), "export {}\n");
  try {
    for (const secret of [
      "-----BEGIN PRIVATE KEY-----",
      "npm_config_//registry.npmjs.org/:_authToken=secret",
      "AWS_SECRET_ACCESS_KEY=secret",
      "STRIPE_SECRET_KEY=sk_live_secret",
      "password=secret",
      "https://user:secret@example.test/path",
    ]) {
      writeFileSync(join(source, "scripts", "ok.mjs"), `${secret}\n`);
      assert.throws(() => auditPublicSource(source), /secret|credential|private|token|password|AWS|Stripe/i, secret);
    }
    rmSync(join(source, "scripts", "ok.mjs"));
    symlinkSync("../LICENSE", join(source, "scripts", "link.mjs"));
    assert.throws(() => auditPublicSource(source), /symlink/i);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});

test("secret audit allows explicit placeholders but provenance schema fails closed", () => {
  for (const placeholder of ["YOUR_TOKEN", "REPLACE_ME", "<token>", "example"]) {
    assert.doesNotThrow(() => auditSecretText("fixture.txt", `API_TOKEN=${placeholder}`));
  }
  assert.throws(() => auditSecretText("fixture.txt", "API_TOKEN=real-secret-value"), /secret|credential/i);
  assert.throws(
    () => auditSecretText("fixture.txt", "NPM_TOKEN=${{ secrets.NPM_TOKEN }}\nNPM_TOKEN=real-secret-value"),
    /secret|credential/i,
    "a placeholder must not hide a later credential",
  );
  for (const token of [
    `ghp_${"A".repeat(36)}`,
    `github_pat_${"A".repeat(82)}`,
    `npm_${"A".repeat(36)}`,
    `AIza${"A".repeat(35)}`,
  ]) {
    assert.throws(() => auditSecretText("fixture.txt", token), /secret|credential/i, token.slice(0, 12));
  }
  const provenance = JSON.parse(readFileSync(join(root, "FIXTURE_PROVENANCE.json"), "utf8"));
  assert.doesNotThrow(() => validateProvenanceData(provenance));
  assert.throws(() => validateProvenanceData({ ...provenance, retainedFixtures: [{ path: "x", status: "approved" }] }), /provenance|incomplete|hash/i);
});

test("unapproved public fixture binaries require hash-verified provenance", () => {
  const scratch = join(root, "tmp", `binary-provenance-${process.pid}`);
  const relative = "public-tests/example.pdf";
  const bytes = Buffer.from("fixture bytes");
  mkdirSync(join(scratch, "public-tests"), { recursive: true });
  writeFileSync(join(scratch, relative), bytes);
  try {
    assert.throws(
      () => auditBinaryProvenance(scratch, [relative], { retainedFixtures: [] }),
      /provenance/i,
    );
    assert.throws(
      () => auditBinaryProvenance(scratch, [relative], {
        retainedFixtures: [{ path: relative, sha256: "0".repeat(64) }],
      }),
      /hash/i,
    );
    assert.doesNotThrow(() => auditBinaryProvenance(scratch, [relative], {
      retainedFixtures: [{
        path: relative,
        sha256: createHash("sha256").update(bytes).digest("hex"),
      }],
    }));
  } finally {
    rmSync(scratch, { recursive: true, force: true });
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

test("release workflows gate the exact publish commit and exercise the clean public export", () => {
  const readiness = readFileSync(join(root, ".github/workflows/publish-wasm.yml"), "utf8");
  const publisher = readFileSync(join(root, ".github/workflows/publish-npm.yml"), "utf8");
  assert.doesNotMatch(readiness, /^\s+paths:\s*$/mu, "all release-input changes must trigger readiness");
  assert.match(readiness, /tests\/all\.test\.mjs/u);
  assert.match(readiness, /scripts\/clean-export-smoke\.sh/u);
  const auditJob = readiness.slice(
    readiness.indexOf("audit-and-core-tests:"),
    readiness.indexOf("package-build:"),
  );
  assert.match(auditJob, /cargo install --locked --features cli cargo-about@0\.9\.2/u);
  assert.match(auditJob, /scripts\/generate-rust-license-notices\.sh --check/u);
  assert.match(publisher, /push:\s*\n\s+tags:\s*\n\s+- ["']0\.0\.\*["']/u);
  assert.match(publisher, /registry-url:\s*https:\/\/npm\.pkg\.github\.com/u);
  assert.match(publisher, /github\.actor\s*==\s*github\.repository_owner/u);
  assert.match(publisher, /Verify locked notices and committed artifacts/u);
  assert.doesNotMatch(publisher, /\.\/(?:pdf|office)-core\/wasm\/build\.sh/u);
  assert.match(publisher, /git diff --exit-code/u);
  assert.match(publisher, /scripts\/clean-export-smoke\.sh/u);
  assert.match(publisher, /scripts\/verify-font-cdn\.mjs/u);
  assert.match(publisher, /packages:\s*write/u);
  assert.match(publisher, /NODE_AUTH_TOKEN:\s*\$\{\{ secrets\.GITHUB_TOKEN \}\}/u);
  assert.doesNotMatch(
    publisher,
    /registry-url:\s*https:\/\/registry\.npmjs\.org|npm publish[^\n]*registry\.npmjs\.org|npm-production|NPM_TOKEN/u,
  );
});

test("exporter never recursively deletes an existing destination", () => {
  const destination = join(root, "tmp", `existing-export-${process.pid}`);
  mkdirSync(destination, { recursive: true });
  writeFileSync(join(destination, "sentinel.txt"), "preserve me\n");
  try {
    assert.throws(() => exportPublicSource(root, destination), /already exists|delete/i);
    assert.equal(readFileSync(join(destination, "sentinel.txt"), "utf8"), "preserve me\n");
  } finally {
    rmSync(destination, { recursive: true, force: true });
  }
});

test("exporter rejects a symlink destination before writing through it", () => {
  const destination = join(root, "tmp", `symlink-export-${process.pid}`);
  symlinkSync(root, destination);
  try {
    assert.throws(() => exportPublicSource(root, destination), /symlink/i);
  } finally {
    rmSync(destination, { force: true });
  }
});

test("exporter rejects a symlinked destination ancestor", () => {
  const scratch = join(root, "tmp", `symlink-parent-${process.pid}`);
  const outside = join(scratch, "outside");
  const linkedParent = join(scratch, "linked-parent");
  const destination = join(linkedParent, "nested", "candidate");
  mkdirSync(outside, { recursive: true });
  symlinkSync(outside, linkedParent);
  try {
    assert.throws(() => exportPublicSource(root, destination), /symlink/i);
    assert.equal(existsSync(join(outside, "README.md")), false, "export must not traverse the linked parent");
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});

test("public source audit rejects an unexpected nested reader-facing file", () => {
  const unexpected = join(root, "pdf-core", "internal-notes.md");
  writeFileSync(unexpected, "internal note\n");
  try {
    assert.throws(() => auditPublicSource(root), /unexpected public path|manifest/i);
  } finally {
    rmSync(unexpected, { force: true });
  }
});

test("public source audit rejects a missing exact path manifest", () => {
  const manifest = join(root, "scripts/public-source-manifest.json");
  const original = readFileSync(manifest);
  try {
    rmSync(manifest);
    assert.throws(() => auditPublicSource(root), /path manifest|manifest/i);
  } finally {
    writeFileSync(manifest, original);
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
