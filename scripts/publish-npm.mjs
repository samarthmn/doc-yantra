#!/usr/bin/env node

import { existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const REGISTRY = "https://registry.npmjs.org";
const PACKAGE_ORDER = [
  ["packages/doc-yantra-fonts", "@samarthmn/doc-yantra-fonts"],
  ["pdf-core/wasm/pkg", "@samarthmn/doc-yantra"],
  ["office-core/wasm/pkg", "@samarthmn/doc-yantra-office"],
  ["packages/dockosha-provider", "@samarthmn/dockosha-provider-docyantra"],
];

function versionAtLeast(actual, minimum) {
  const parse = (value) => String(value).trim().replace(/^v/u, "").split(".").map(Number);
  const [actualMajor = 0, actualMinor = 0, actualPatch = 0] = parse(actual);
  const [minimumMajor, minimumMinor, minimumPatch] = parse(minimum);
  return actualMajor > minimumMajor ||
    (actualMajor === minimumMajor && (actualMinor > minimumMinor ||
      (actualMinor === minimumMinor && actualPatch >= minimumPatch)));
}

export function assertRuntimeFloors({ nodeVersion = process.versions.node, npmVersion }) {
  if (!versionAtLeast(nodeVersion, "22.14.0")) {
    throw new Error(`Node.js >= 22.14.0 is required; found ${nodeVersion}`);
  }
  if (!npmVersion || !versionAtLeast(npmVersion, "11.5.1")) {
    throw new Error(`npm >= 11.5.1 is required; found ${npmVersion ?? "unknown"}`);
  }
}

function defaultRun(command, args, options = {}) {
  const result = spawnSync(command, args, { ...options, encoding: "utf8" });
  if (result.error) throw result.error;
  return { status: result.status ?? 1, stdout: result.stdout ?? "", stderr: result.stderr ?? "" };
}

function defaultAudit({ root, packDir, run }) {
  const result = invoke(run, process.execPath, ["scripts/package-audit.mjs", "--packed-dir", packDir], { cwd: root });
  if (result.status !== 0) throw new Error(`exact-tarball package audit failed: ${result.stderr || result.stdout}`);
}

function invoke(run, command, args, options) {
  const result = run(command, args, options) ?? {};
  return {
    status: result.status ?? 0,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
  };
}

function parseJson(stdout, action) {
  try {
    return JSON.parse(stdout);
  } catch (error) {
    throw new Error(`${action} returned invalid JSON: ${error.message}`);
  }
}

function packageSpecs(root) {
  return PACKAGE_ORDER.map(([relative, name]) => {
    const directory = join(root, relative);
    const manifest = JSON.parse(readFileSync(join(directory, "package.json"), "utf8"));
    if (manifest.name !== name || !manifest.version) throw new Error(`invalid package manifest: ${relative}`);
    return { directory, relative, name, version: manifest.version };
  });
}

function missingVersion(result) {
  return result.status !== 0 && /\b(?:E404|404)\b/u.test(`${result.stdout}\n${result.stderr}`);
}

function readIntegrity(run, spec) {
  const result = invoke(run, "npm", ["view", `${spec.name}@${spec.version}`, "dist.integrity", "--json", "--registry", REGISTRY]);
  if (result.status === 0) {
    const value = parseJson(result.stdout, `npm view ${spec.name}@${spec.version}`);
    if (typeof value !== "string" || !value) throw new Error(`registry returned no dist.integrity for ${spec.name}@${spec.version}`);
    return value;
  }
  if (missingVersion(result)) return undefined;
  throw new Error(`registry lookup failed for ${spec.name}@${spec.version}: ${result.stderr || result.stdout}`);
}

function pack(run, spec, packDir) {
  const result = invoke(run, "npm", ["pack", "--json", "--ignore-scripts", "--pack-destination", packDir], { cwd: spec.directory });
  if (result.status !== 0) throw new Error(`npm pack failed for ${spec.name}: ${result.stderr || result.stdout}`);
  const packed = parseJson(result.stdout, `npm pack ${spec.name}`);
  if (!Array.isArray(packed) || packed.length !== 1 || !packed[0]?.filename || !packed[0]?.integrity) {
    throw new Error(`npm pack did not return one integrity-addressed tarball for ${spec.name}`);
  }
  const tarball = join(packDir, packed[0].filename);
  const integrity = `sha512-${createHash("sha512").update(readFileSync(tarball)).digest("base64")}`;
  if (packed[0].integrity !== integrity) {
    throw new Error(`pack integrity mismatch for ${spec.name}: npm=${packed[0].integrity}, bytes=${integrity}`);
  }
  return { ...spec, filename: packed[0].filename, integrity, tarball };
}

function assertPackDestination(root, packDir) {
  const tmp = join(root, "tmp");
  if (!packDir.startsWith(`${tmp}/`)) throw new Error("publisher pack directory must remain under repository tmp/");
}

export async function publishPackages({ root = ROOT, dryRun = false, run = defaultRun, audit = defaultAudit } = {}) {
  const npmVersionResult = invoke(run, "npm", ["--version"]);
  if (npmVersionResult.status !== 0) throw new Error(`could not determine npm version: ${npmVersionResult.stderr || npmVersionResult.stdout}`);
  assertRuntimeFloors({ npmVersion: npmVersionResult.stdout.trim() });

  const packDir = join(root, "tmp", `publish-npm-${process.pid}`);
  assertPackDestination(root, packDir);
  if (existsSync(packDir)) throw new Error(`refusing to reuse publisher temporary directory: ${packDir}`);
  mkdirSync(packDir, { recursive: true });
  try {
    const tarballs = packageSpecs(root).map((spec) => pack(run, spec, packDir));
    audit({ root, packDir, tarballs, run });
    // Validate every immutable registry slot before publishing the first
    // package, so a later conflict cannot leave a partial lockstep release.
    const registryState = tarballs.map((tarball) => ({
      tarball,
      existing: readIntegrity(run, tarball),
    }));
    for (const { tarball, existing } of registryState) {
      if (existing !== undefined && existing !== tarball.integrity) {
        throw new Error(`immutable integrity mismatch for ${tarball.name}@${tarball.version}: registry=${existing}, local=${tarball.integrity}`);
      }
    }
    const published = [];
    const skipped = [];
    const wouldPublish = [];
    for (const { tarball, existing } of registryState) {
      if (existing !== undefined) {
        skipped.push(tarball);
        continue;
      }
      if (dryRun) {
        wouldPublish.push(tarball);
        continue;
      }
      const result = invoke(run, "npm", ["publish", tarball.tarball, "--provenance", "--access", "public", "--registry", REGISTRY]);
      if (result.status !== 0) {
        if (!/\b(?:E409|409)\b/u.test(`${result.stdout}\n${result.stderr}`)) {
          throw new Error(`npm publish failed for ${tarball.name}@${tarball.version}: ${result.stderr || result.stdout}`);
        }
        const raced = readIntegrity(run, tarball);
        if (raced !== tarball.integrity) {
          throw new Error(`publish race integrity mismatch for ${tarball.name}@${tarball.version}`);
        }
        skipped.push(tarball);
        continue;
      }
      const verified = readIntegrity(run, tarball);
      if (verified !== tarball.integrity) {
        throw new Error(`post-publication integrity verification failed for ${tarball.name}@${tarball.version}`);
      }
      published.push(tarball);
    }
    return Object.freeze({ published, skipped, wouldPublish });
  } finally {
    rmSync(packDir, { recursive: true, force: true });
  }
}

function cli(argv) {
  if (argv.length > 1 || (argv.length === 1 && argv[0] !== "--dry-run")) {
    throw new Error("usage: node scripts/publish-npm.mjs [--dry-run]");
  }
  return { dryRun: argv.includes("--dry-run") };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  try {
    const result = await publishPackages(cli(process.argv.slice(2)));
    for (const tarball of result.skipped) process.stdout.write(`already verified: ${tarball.name}@${tarball.version}\n`);
    for (const tarball of result.wouldPublish) process.stdout.write(`would publish: ${tarball.name}@${tarball.version}\n`);
    for (const tarball of result.published) process.stdout.write(`published and verified: ${tarball.name}@${tarball.version}\n`);
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  }
}
