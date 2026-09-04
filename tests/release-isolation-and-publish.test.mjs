import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import { exportPublicSource, publicSourceFiles } from "../scripts/export-public-source.mjs";
import { assertDeterministicPack, auditAdapterLockfile, tarballSri } from "../scripts/package-audit.mjs";
import { publishPackages } from "../scripts/publish-npm.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function packedResult(args, filename = "package.tgz", contents = "packed tarball") {
  const destination = args[args.indexOf("--pack-destination") + 1];
  const bytes = Buffer.from(contents);
  writeFileSync(join(destination, filename), bytes);
  return {
    stdout: JSON.stringify([{
      filename,
      integrity: `sha512-${createHash("sha512").update(bytes).digest("base64")}`,
    }]),
  };
}

test("office-core is a one-package locked workspace while fidelity owns a separate lockfile", () => {
  const office = readFileSync(join(root, "office-core", "Cargo.toml"), "utf8");
  const workspace = office.slice(office.indexOf("[workspace]"));
  assert.doesNotMatch(workspace, /^members\s*=/mu, "the public office workspace must not resolve fidelity");
  assert.equal(existsSync(join(root, "office-core", "fidelity", "Cargo.lock")), true);
  assert.match(readFileSync(join(root, "office-core", "fidelity", "Cargo.toml"), "utf8"), /^\[workspace\]$/mu);
});

test("private fidelity fetches one fixed PDFium build with a platform checksum gate", () => {
  const fetch = readFileSync(join(root, "pdf-core", "scripts", "fetch-pdfium.sh"), "utf8");
  assert.match(fetch, /^PDFIUM_BUILD="7763"$/mu);
  assert.doesNotMatch(fetch, /PDFIUM_BUILD="\$\{/u);
  assert.match(fetch, /owner-required checksum\/provenance gate/u);
  assert.doesNotMatch(fetch, /\$\{TARGET\^\^\}/u, "macOS Bash 3.2 has no uppercase expansion");
  assert.match(fetch, /command -v shasum/u, "macOS must have a portable checksum fallback");
  assert.match(fetch, /tr '\[:upper:\]' '\[:lower:\]'/u, "uppercase checksum input must work with macOS shasum output");
  const workflow = readFileSync(join(root, ".github", "workflows", "fidelity.yml"), "utf8");
  assert.doesNotMatch(workflow, /^\s*PDFIUM_BUILD:/mu);
});

test("public export retains PDFium license notices but excludes downloaded PDFium artifacts", () => {
  const expectedNotices = [
    "pdf-core/pdfium/LICENSE",
    ...readdirSync(join(root, "pdf-core", "pdfium", "licenses"))
      .sort()
      .map((name) => `pdf-core/pdfium/licenses/${name}`),
  ];
  const publicFiles = new Set(publicSourceFiles(root));
  const trackedFiles = new Set(
    execFileSync("git", ["ls-files", "--", "pdf-core/pdfium"], { cwd: root, encoding: "utf8" })
      .trim()
      .split("\n")
      .filter(Boolean),
  );
  for (const notice of expectedNotices) {
    assert.equal(publicFiles.has(notice), true, `missing public PDFium notice: ${notice}`);
    assert.equal(trackedFiles.has(notice), true, `PDFium notice is not committed: ${notice}`);
  }
  assert.equal(publicFiles.has("pdf-core/pdfium/lib/libpdfium.so"), false);
  assert.equal(publicFiles.has("pdf-core/pdfium/include/fpdfview.h"), false);
  assert.match(
    readFileSync(join(root, "THIRD_PARTY_NOTICES.md"), "utf8"),
    /\[`pdf-core\/pdfium\/LICENSE`\]\(pdf-core\/pdfium\/LICENSE\)/u,
  );
});

test("public export omits the private fidelity harness and its workflow", () => {
  const scratch = join(root, "tmp", `docyantra-public-export-${process.pid}`);
  const destination = join(scratch, "candidate");
  mkdirSync(scratch, { recursive: true });
  try {
    exportPublicSource(root, destination);
    assert.equal(existsSync(join(destination, "office-core", "fidelity")), false);
    assert.equal(existsSync(join(destination, ".github", "workflows", "fidelity.yml")), false);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});

test("publisher only releases tarballs absent from the registry and verifies their integrity", async () => {
  const commands = [];
  const views = new Map();
  const localIntegrity = `sha512-${createHash("sha512").update("fonts tarball").digest("base64")}`;
  const result = await publishPackages({
    root,
    dryRun: false,
    run(command, args) {
      commands.push([command, args]);
      if (command === process.execPath) return { stdout: "package audit passed" };
      if (args[0] === "--version") return { stdout: "11.5.1" };
      if (args[0] === "pack") return packedResult(args, "fonts.tgz", "fonts tarball");
      if (args[0] === "view") {
        const key = args[1];
        const count = (views.get(key) ?? 0) + 1;
        views.set(key, count);
        return count === 1
          ? { stdout: "not found", stderr: "npm error code E404", status: 1 }
          : { stdout: JSON.stringify(localIntegrity) };
      }
      if (args[0] === "publish") return { stdout: "published" };
      throw new Error(`unexpected command: ${command} ${args.join(" ")}`);
    },
  });
  assert.equal(result.published.length, 4);
  assert.equal(result.skipped.length, 0);
  assert.equal(commands.filter(([, args]) => args[0] === "publish").length, 4);
  const auditAt = commands.findIndex(([command]) => command === process.execPath);
  const firstRegistryLookup = commands.findIndex(([, args]) => args[0] === "view");
  assert.ok(auditAt >= 0 && auditAt < firstRegistryLookup, "the packed tarballs must be audited before registry decisions");
});

test("publisher skips an exact existing integrity but refuses a mismatched immutable version", async () => {
  let view = 0;
  const localIntegrity = `sha512-${createHash("sha512").update("core tarball").digest("base64")}`;
  await assert.rejects(
    publishPackages({
      root,
      audit() {},
      run(_command, args) {
        if (args[0] === "--version") return { stdout: "11.5.1" };
        if (args[0] === "pack") return packedResult(args, "core.tgz", "core tarball");
        if (args[0] === "view") {
          view += 1;
          return { stdout: JSON.stringify(view === 1 ? localIntegrity : "sha512-different") };
        }
        throw new Error("publishing must not start after an integrity mismatch");
      },
    }),
    /integrity mismatch/u,
  );
});

test("publisher validates every immutable registry slot before publishing any package", async () => {
  let view = 0;
  let publishes = 0;
  await assert.rejects(
    publishPackages({
      root,
      audit() {},
      run(_command, args) {
        if (args[0] === "--version") return { stdout: "11.5.1" };
        if (args[0] === "pack") return packedResult(args, "candidate.tgz", "candidate bytes");
        if (args[0] === "view") {
          view += 1;
          if (view === 1) return { stderr: "npm error code E404", status: 1 };
          return { stdout: JSON.stringify("sha512-different") };
        }
        if (args[0] === "publish") {
          publishes += 1;
          return { stdout: "published" };
        }
        throw new Error(`unexpected command: ${args[0]}`);
      },
    }),
    /integrity mismatch/u,
  );
  assert.equal(publishes, 0, "an immutable mismatch must abort before the first publish");
});

test("publisher rejects an npm-reported integrity that does not match its packed bytes", async () => {
  let auditCalled = false;
  await assert.rejects(
    publishPackages({
      root,
      audit() { auditCalled = true; },
      run(_command, args) {
        if (args[0] === "--version") return { stdout: "11.5.1" };
        if (args[0] === "pack") {
          const destination = args[args.indexOf("--pack-destination") + 1];
          const filename = "forged-integrity.tgz";
          writeFileSync(join(destination, filename), "real tarball bytes");
          return { stdout: JSON.stringify([{ filename, integrity: "sha512-forged" }]) };
        }
        throw new Error(`npm-reported integrity must fail before ${args[0]}`);
      },
    }),
    /pack integrity mismatch/u,
  );
  assert.equal(auditCalled, false, "forged bytes must be rejected before the tarball audit");
});

test("package audit rejects repeat packs whose tarball bytes differ", () => {
  const scratch = join(root, "tmp", `nondeterministic-pack-${process.pid}`);
  const first = join(scratch, "first.tgz");
  const second = join(scratch, "second.tgz");
  mkdirSync(scratch, { recursive: true });
  try {
    writeFileSync(first, "first bytes");
    writeFileSync(second, "second bytes");
    assert.throws(
      () => assertDeterministicPack(
        { filename: "package.tgz", integrity: tarballSri(first), archivePath: first },
        { filename: "package.tgz", integrity: tarballSri(second), archivePath: second },
        "fixture package",
      ),
      /repeat pack is not deterministic/u,
    );
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});

test("adapter lock contract rejects public engine tarballs without SHA-512 SRI", () => {
  const scratch = join(root, "tmp", `adapter-lock-integrity-${process.pid}`);
  const lockPath = join(scratch, "pnpm-lock.yaml");
  const version = JSON.parse(
    readFileSync(join(root, "packages", "dockosha-provider", "package.json"), "utf8"),
  ).version;
  const enginePackages = [
    "@samarthmn/doc-yantra",
    "@samarthmn/doc-yantra-office",
  ];
  const actualIntegrity = `sha512-${"A".repeat(86)}==`;
  const wrongIntegrity = `sha512-${"B".repeat(86)}==`;
  const expected = {
    "@samarthmn/doc-yantra": actualIntegrity,
    "@samarthmn/doc-yantra-office": actualIntegrity,
  };
  mkdirSync(scratch, { recursive: true });
  try {
    const lock = readFileSync(join(root, "packages", "dockosha-provider", "pnpm-lock.yaml"), "utf8");
    const replaceEngineResolutions = (source, transform) =>
      enginePackages.reduce((fixture, packageName) => {
        const packageKey = `${packageName}@${version}`;
        const resolutionPattern = new RegExp(
          `(^\\s*'${packageKey}':\\s*\\n\\s*resolution:\\s*\\{)([^}\\n]*)(\\})`,
          "mu",
        );
        assert.match(
          fixture,
          resolutionPattern,
          `current lockfile is missing the ${packageKey} resolution`,
        );
        return fixture.replace(
          resolutionPattern,
          (_, prefix, resolution, suffix) =>
            `${prefix}${transform(resolution)}${suffix}`,
        );
      }, source);

    // Derive the missing-SRI fixture from the current lockfile so this test
    // remains valid when an audited tarball integrity changes.
    const missingIntegrity = replaceEngineResolutions(lock, (resolution) =>
      resolution
        .split(/,\s*/u)
        .filter(
          (field) =>
            !/^integrity:\s*sha512-[A-Za-z0-9+/]{86}==$/u.test(field),
        )
        .join(", "),
    );
    writeFileSync(lockPath, missingIntegrity);
    assert.throws(
      () => auditAdapterLockfile(version, lockPath),
      /integrity/u,
    );

    // The expected-integrity path uses an intentionally synthetic canonical
    // SRI, rather than coupling this unit test to the live package tarballs.
    writeFileSync(
      lockPath,
      replaceEngineResolutions(
        missingIntegrity,
        (resolution) => `integrity: ${actualIntegrity}, ${resolution}`,
      ),
    );
    assert.doesNotThrow(() => auditAdapterLockfile(version, lockPath, expected));
    assert.throws(
      () => auditAdapterLockfile(version, lockPath, {
        ...expected,
        "@samarthmn/doc-yantra": wrongIntegrity,
      }),
      /does not match audited tarball/u,
    );
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});

test("publisher dry run packs in dependency order and never invokes npm publish", async () => {
  const commands = [];
  const result = await publishPackages({
    root,
    dryRun: true,
    audit() {},
    run(command, args) {
      commands.push([command, args]);
      if (args[0] === "--version") return { stdout: "11.5.1" };
      if (args[0] === "pack") return packedResult(args);
      if (args[0] === "view") return { stdout: "not found", stderr: "npm error code E404", status: 1 };
      throw new Error(`dry-run must not execute ${args[0]}`);
    },
  });
  assert.deepEqual(result.wouldPublish.map(({ name }) => name), [
    "@samarthmn/doc-yantra-fonts",
    "@samarthmn/doc-yantra",
    "@samarthmn/doc-yantra-office",
    "@samarthmn/dockosha-provider-docyantra",
  ]);
  assert.equal(commands.some(([, args]) => args[0] === "publish"), false);
});
