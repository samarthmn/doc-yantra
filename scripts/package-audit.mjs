#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  mkdirSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CORE_PACKED_LIMIT = 6 * 1024 * 1024;
const CORE_UNPACKED_LIMIT = 15 * 1024 * 1024;
const FONT_PACKED_LIMIT = 145 * 1024 * 1024;
const FONT_EXTENSION = /\.(?:otc|otf|ttc|ttf|woff2?)$/iu;
const FORBIDDEN_CORE_PATH = /(^|\/)(?:corpora|corpus|fidelity-corpus|fixtures|fonts|mock-files|src|target|tests?)(?:\/|$)/iu;

const PACKAGES = Object.freeze([
  {
    name: "@samarthmn/doc-yantra",
    path: "pdf-core/wasm/pkg",
    engine: true,
  },
  {
    name: "@samarthmn/doc-yantra-office",
    path: "office-core/wasm/pkg",
    engine: true,
  },
  {
    name: "@samarthmn/dockosha-provider-docyantra",
    path: "packages/dockosha-provider",
    engine: false,
  },
]);
const FONT_PACKAGE = Object.freeze({
  name: "@samarthmn/doc-yantra-fonts",
  path: "packages/doc-yantra-fonts",
});

function invariant(condition, message) {
  if (!condition) throw new Error(message);
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function packDryRun(packagePath, cache) {
  const packageRoot = join(ROOT, packagePath);
  const output = execFileSync(
    "npm",
    ["pack", "--dry-run", "--json", "--ignore-scripts", "--cache", cache],
    { cwd: packageRoot, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
  );
  const result = JSON.parse(output);
  invariant(result.length === 1, `${packagePath}: npm pack returned ${result.length} results`);
  const manifest = result[0];
  invariant(Number.isSafeInteger(manifest.size) && manifest.size > 0, `${packagePath}: invalid packed size`);
  invariant(
    Number.isSafeInteger(manifest.unpackedSize) && manifest.unpackedSize > 0,
    `${packagePath}: invalid unpacked size`,
  );
  invariant(Array.isArray(manifest.files) && manifest.files.length > 0, `${packagePath}: package is empty`);
  return manifest;
}

function packageVersion(packagePath) {
  return readJson(join(ROOT, packagePath, "package.json")).version;
}

function auditCorePackage(config, pack, version) {
  invariant(pack.name === config.name, `${config.path}: expected ${config.name}, found ${pack.name}`);
  invariant(pack.version === version, `${config.name}: version ${pack.version} does not match ${version}`);
  for (const { path } of pack.files) {
    invariant(!FONT_EXTENSION.test(path), `${config.name}: core package contains font binary ${path}`);
    invariant(!FORBIDDEN_CORE_PATH.test(path), `${config.name}: forbidden release path ${path}`);
    invariant(!/\.rs$/iu.test(path), `${config.name}: Rust source leaked into package: ${path}`);
    invariant(!/(^|\/)Cargo\.(?:lock|toml)$/u.test(path), `${config.name}: Cargo metadata leaked: ${path}`);
  }
  const wasmFiles = pack.files
    .map(({ path }) => path)
    .filter((path) => path.endsWith(".wasm"));
  if (config.engine) {
    invariant(
      wasmFiles.length === 1,
      `${config.name}: expected exactly one WASM binary, found ${wasmFiles.join(", ") || "none"}`,
    );
  } else {
    invariant(wasmFiles.length === 0, `${config.name}: adapter must not contain a WASM binary`);
  }
  return Object.freeze({
    name: config.name,
    packedBytes: pack.size,
    unpackedBytes: pack.unpackedSize,
    fileCount: pack.files.length,
    wasmFiles,
  });
}

function auditFontPackage(pack, version) {
  invariant(pack.name === FONT_PACKAGE.name, `font package name is ${pack.name}`);
  invariant(pack.version === version, `font package version ${pack.version} does not match ${version}`);
  invariant(pack.size <= FONT_PACKED_LIMIT, `offline font pack is ${pack.size} bytes; limit is ${FONT_PACKED_LIMIT}`);
  const packageRoot = join(ROOT, FONT_PACKAGE.path);
  const manifest = readJson(join(packageRoot, "fonts", "manifest.json"));
  invariant(Array.isArray(manifest.fonts) && manifest.fonts.length === 17, "font manifest must contain 17 faces");
  const packedPaths = new Set(pack.files.map(({ path }) => path));
  const expectedFontPaths = new Set();
  for (const entry of manifest.fonts) {
    const relative = `fonts/${entry.file}`;
    expectedFontPaths.add(relative);
    invariant(packedPaths.has(relative), `${FONT_PACKAGE.name}: missing ${relative}`);
    invariant(packedPaths.has(`fonts/${entry.license}`), `${FONT_PACKAGE.name}: missing ${entry.license}`);
    const bytes = readFileSync(join(packageRoot, relative));
    invariant(bytes.byteLength === entry.byteLength, `${entry.file}: byte length does not match manifest`);
    const hash = createHash("sha256").update(bytes).digest("hex");
    invariant(hash === entry.sha256, `${entry.file}: SHA-256 does not match manifest`);
  }
  const unexpectedFonts = pack.files
    .map(({ path }) => path)
    .filter((path) => FONT_EXTENSION.test(path) && !expectedFontPaths.has(path));
  invariant(unexpectedFonts.length === 0, `font package contains unmanifested fonts: ${unexpectedFonts.join(", ")}`);
  return Object.freeze({
    name: FONT_PACKAGE.name,
    packedBytes: pack.size,
    unpackedBytes: pack.unpackedSize,
    fileCount: pack.files.length,
    fontCount: manifest.fonts.length,
  });
}

function auditVersionPins(version) {
  const pdf = readJson(join(ROOT, "pdf-core/wasm/pkg/package.json"));
  const adapter = readJson(join(ROOT, "packages/dockosha-provider/package.json"));
  for (const [label, manifest] of [
    ["PDF package", pdf],
    ["adapter", adapter],
  ]) {
    invariant(
      manifest.peerDependencies?.[FONT_PACKAGE.name] === version,
      `${label}: optional font peer must be pinned to ${version}`,
    );
    invariant(
      manifest.peerDependenciesMeta?.[FONT_PACKAGE.name]?.optional === true,
      `${label}: ${FONT_PACKAGE.name} must be an optional peer`,
    );
  }
  for (const packageName of ["@samarthmn/doc-yantra", "@samarthmn/doc-yantra-office"]) {
    invariant(
      adapter.dependencies?.[packageName] === version,
      `adapter dependency ${packageName} must be pinned to ${version}`,
    );
  }
}

export function auditPackages() {
  const scratch = join(ROOT, "tmp", `package-audit-${process.pid}`);
  mkdirSync(scratch, { recursive: true });
  try {
    const versions = [
      ...PACKAGES.map(({ path }) => packageVersion(path)),
      packageVersion(FONT_PACKAGE.path),
    ];
    const version = versions[0];
    invariant(/^0\.0\.\d+$/u.test(version), `release version must be 0.0.x, found ${version}`);
    invariant(versions.every((candidate) => candidate === version), `release package versions differ: ${versions.join(", ")}`);
    auditVersionPins(version);

    const corePackages = PACKAGES.map((config) =>
      auditCorePackage(config, packDryRun(config.path, scratch), version),
    );
    const packedBytes = corePackages.reduce((total, item) => total + item.packedBytes, 0);
    const unpackedBytes = corePackages.reduce((total, item) => total + item.unpackedBytes, 0);
    invariant(packedBytes <= CORE_PACKED_LIMIT, `combined core is ${packedBytes} packed bytes; limit is ${CORE_PACKED_LIMIT}`);
    invariant(
      unpackedBytes <= CORE_UNPACKED_LIMIT,
      `combined core is ${unpackedBytes} unpacked bytes; limit is ${CORE_UNPACKED_LIMIT}`,
    );
    const fonts = auditFontPackage(packDryRun(FONT_PACKAGE.path, scratch), version);
    return Object.freeze({
      version,
      core: Object.freeze({
        packedBytes,
        unpackedBytes,
        packages: Object.freeze(corePackages),
      }),
      fonts,
    });
  } finally {
    rmSync(scratch, { force: true, recursive: true });
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const result = auditPackages();
  if (process.argv.includes("--json")) {
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } else {
    process.stdout.write(
      `package audit passed: core ${result.core.packedBytes} packed / ${result.core.unpackedBytes} unpacked bytes; fonts ${result.fonts.packedBytes} packed bytes\n`,
    );
  }
}
