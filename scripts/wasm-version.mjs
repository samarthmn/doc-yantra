#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join, resolve } from "node:path";

const require = createRequire(import.meta.url);

function cargoPackageVersion(manifestPath) {
  const source = readFileSync(manifestPath, "utf8");
  const packageAt = source.search(/^\[package\]\s*$/mu);
  if (packageAt === -1) throw new Error(`${manifestPath}: missing [package] section`);
  const afterHeader = source.slice(packageAt).replace(/^\[package\]\s*$/mu, "");
  const nextSection = afterHeader.search(/^\[/mu);
  const packageSection = nextSection === -1 ? afterHeader : afterHeader.slice(0, nextSection);
  const version = packageSection.match(/^version\s*=\s*"([^"]+)"/mu)?.[1];
  if (!version) throw new Error(`${manifestPath}: missing [package] version`);
  return version;
}

function builtVersions(packageRoot) {
  const packageVersion = JSON.parse(
    readFileSync(join(packageRoot, "package.json"), "utf8"),
  ).version;
  const entry = require.resolve(packageRoot);
  delete require.cache[entry];
  const engine = require(entry);
  if (typeof engine.version !== "function") {
    throw new Error(`${packageRoot}: package does not export version()`);
  }
  return { packageVersion, engineVersion: engine.version() };
}

function versionMismatch(expected, built) {
  if (built.packageVersion !== expected) {
    return `npm package version ${built.packageVersion}, expected ${expected}`;
  }
  if (built.engineVersion !== expected) {
    return `embedded engine version ${built.engineVersion}, expected ${expected}`;
  }
  return null;
}

function usage() {
  console.error("usage: wasm-version.mjs <needs-clean|verify> <Cargo.toml> <pkg-dir>");
  process.exit(2);
}

const [command, manifestArg, packageArg] = process.argv.slice(2);
if (!command || !manifestArg || !packageArg) usage();

const manifestPath = resolve(manifestArg);
const packageRoot = resolve(packageArg);
const expected = cargoPackageVersion(manifestPath);

if (command === "needs-clean") {
  try {
    const mismatch = versionMismatch(expected, builtVersions(packageRoot));
    if (mismatch) {
      console.log(`invalidating stale wasm artifacts: ${mismatch}`);
      process.exit(0);
    }
    process.exit(1);
  } catch (error) {
    console.log(`invalidating unreadable wasm artifacts: ${error.message}`);
    process.exit(0);
  }
}

if (command === "verify") {
  try {
    const mismatch = versionMismatch(expected, builtVersions(packageRoot));
    if (mismatch) throw new Error(mismatch);
    console.log(`verified embedded wasm version ${expected}`);
  } catch (error) {
    console.error(`error: ${error.message}`);
    process.exit(1);
  }
} else {
  usage();
}
