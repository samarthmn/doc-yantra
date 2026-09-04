#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  mkdirSync,
  readdirSync,
  realpathSync,
  readFileSync,
  rmSync,
  statSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { verifyFingerprint } from "./source-fingerprint.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const CORE_PACKED_LIMIT = 6 * 1024 * 1024;
const CORE_UNPACKED_LIMIT = 15 * 1024 * 1024;
const FONT_PACKED_LIMIT = 145 * 1024 * 1024;
const FONT_EXTENSION = /\.(?:otc|otf|ttc|ttf|woff2?)$/iu;
const FORBIDDEN_CORE_PATH = /(^|\/)(?:corpora|corpus|fidelity-corpus|fixtures|fonts|mock-files|src|target|tests?)(?:\/|$)/iu;
const PUBLIC_REGISTRY = "https://registry.npmjs.org";
const ROOT_LICENSE = readFileSync(join(ROOT, "LICENSE"));
const ROOT_COMMERCIAL_NOTICE = readFileSync(join(ROOT, "COMMERCIAL-LICENSING.md"));
const ADOBE_NOTICE = readFileSync(join(ROOT, "pdf-core/assets/cmaps/LICENSE-adobe-cmap-resources.md"));

const PACKAGES = Object.freeze([
  {
    name: "@samarthmn/doc-yantra",
    path: "pdf-core/wasm/pkg",
    engine: true,
    rustNotice: "pdf-core/wasm/THIRD_PARTY_RUST_LICENSES.txt",
  },
  {
    name: "@samarthmn/doc-yantra-office",
    path: "office-core/wasm/pkg",
    engine: true,
    rustNotice: "office-core/wasm/THIRD_PARTY_RUST_LICENSES.txt",
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

export function tarballSri(archivePath) {
  return `sha512-${createHash("sha512").update(readFileSync(archivePath)).digest("base64")}`;
}

export function assertDeterministicPack(first, second, packagePath) {
  invariant(
    first.filename === second.filename,
    `${packagePath}: repeat pack is not deterministic (filenames differ)`,
  );
  invariant(
    first.integrity === second.integrity,
    `${packagePath}: repeat pack is not deterministic (SHA-512 SRI differs)`,
  );
  invariant(
    readFileSync(first.archivePath).equals(readFileSync(second.archivePath)),
    `${packagePath}: repeat pack is not deterministic (tarball bytes differ)`,
  );
  return first;
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function npmInvocation() {
  const npmPath = process.env.npm_execpath;
  if (npmPath) return [process.execPath, npmPath];
  try {
    const candidate = realpathSync(execFileSync("which", ["npm"], { encoding: "utf8" }).trim());
    if (candidate.endsWith(".js") || readFileSync(candidate, "utf8").startsWith("#!")) return [process.execPath, candidate];
  } catch {
    // Fall back to the platform command when npm is not a Node CLI shim.
  }
  return ["npm"];
}

function packedArchive(packagePath, packedDir) {
  const expected = readJson(join(ROOT, packagePath, "package.json"));
  for (const filename of readdirSync(packedDir).filter((name) => name.endsWith(".tgz"))) {
    const archivePath = join(packedDir, filename);
    try {
      const packedManifest = JSON.parse(execFileSync("tar", ["-xOf", archivePath, "package/package.json"], { encoding: "utf8" }));
      if (packedManifest.name !== expected.name || packedManifest.version !== expected.version) continue;
      const paths = execFileSync("tar", ["-tzf", archivePath], { encoding: "utf8" })
        .trim().split("\n").filter((path) => path.startsWith("package/") && path !== "package/")
        .map((path) => ({ path: path.slice("package/".length) }));
      const unpackedSize = execFileSync("tar", ["-xOzf", archivePath], { maxBuffer: 300 * 1024 * 1024 }).byteLength;
      return {
        ...packedManifest,
        size: statSync(archivePath).size,
        unpackedSize,
        files: paths,
        archivePath,
        integrity: tarballSri(archivePath),
      };
    } catch {
      // A malformed/unrelated archive cannot satisfy this package's audit.
    }
  }
  throw new Error(`${packagePath}: audited tarball is missing from ${packedDir}`);
}

function packDryRun(packagePath, cache, packedDir) {
  if (packedDir) return packedArchive(packagePath, packedDir);
  const packageRoot = join(ROOT, packagePath);
  mkdirSync(cache, { recursive: true });
  const [npm, ...npmPrefix] = npmInvocation();
  const output = execFileSync(
    npm,
    [...npmPrefix, "pack", "--json", "--ignore-scripts", "--cache", cache, "--pack-destination", cache],
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
  manifest.archivePath = join(cache, manifest.filename);
  const byteIntegrity = tarballSri(manifest.archivePath);
  invariant(
    manifest.integrity === byteIntegrity,
    `${packagePath}: npm pack integrity does not match the tarball bytes`,
  );
  manifest.integrity = byteIntegrity;
  return manifest;
}

function deterministicPack(packagePath, scratch) {
  const name = packagePath.replaceAll("/", "-");
  const first = packDryRun(packagePath, join(scratch, `first-${name}`));
  const second = packDryRun(packagePath, join(scratch, `second-${name}`));
  return assertDeterministicPack(first, second, packagePath);
}

function packedBytes(pack, relativePath) {
  return execFileSync("tar", ["-xOf", pack.archivePath, `package/${relativePath}`]);
}

function packageVersion(packagePath) {
  return readJson(join(ROOT, packagePath, "package.json")).version;
}

function auditRegistryUrls(text) {
  const candidates = text.match(/\b[a-z][a-z0-9+.-]*:\/\/[^\s]+/giu) ?? [];
  for (const candidate of candidates) {
    let parsed;
    try {
      parsed = new URL(candidate);
    } catch {
      invariant(false, `adapter lockfile contains an invalid registry URL: ${candidate}`);
    }
    invariant(
      parsed.protocol === "https:" && parsed.hostname === "registry.npmjs.org" && !parsed.username && !parsed.password && !parsed.port,
      `adapter lockfile contains a non-public registry URL: ${candidate}`,
    );
  }
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
}

export function auditAdapterLockfile(
  version,
  lockPath = join(ROOT, "packages/dockosha-provider/pnpm-lock.yaml"),
  expectedIntegrities,
) {
  const text = readFileSync(lockPath, "utf8");
  invariant(/^lockfileVersion:\s*['"]?9\.0['"]?\s*$/mu.test(text), "adapter lockfile must use pnpm lockfile version 9.0");
  invariant(!/npm\.pkg\.github\.com|(?:^|[\s"'])(?:file|link|workspace):|(?:\.\.|\.\/)(?:pdf-core|office-core|doc-yantra)/iu.test(text), "adapter lockfile contains a private, local, workspace, or sibling dependency source");
  auditRegistryUrls(text);
  const required = ["@samarthmn/doc-yantra", "@samarthmn/doc-yantra-office"];
  const importerStart = text.indexOf("  .:\n", text.indexOf("importers:"));
  const packagesStart = text.indexOf("\npackages:", importerStart);
  invariant(importerStart >= 0 && packagesStart > importerStart, "adapter lockfile importer is missing");
  const importer = text.slice(importerStart, packagesStart);
  const lines = importer.split("\n");
  const integrities = {};
  for (const name of required) {
    const index = lines.findIndex((line) => line.trim() === `'${name}':`);
    invariant(index >= 0, `adapter lockfile importer is missing ${name}`);
    const block = lines.slice(index + 1, index + 4).join("\n");
    invariant(new RegExp(`^\\s+specifier: ${version}$`, "mu").test(block), `adapter lockfile ${name} specifier must be ${version}`);
    invariant(new RegExp(`^\\s+version: ${version}$`, "mu").test(block), `adapter lockfile ${name} resolution must be ${version}`);
    const packageKey = `  '${name}@${version}':`;
    invariant(text.includes(packageKey), `adapter lockfile package entry is missing ${name}@${version}`);
    const bareName = name.slice(name.lastIndexOf("/") + 1);
    const tarball = `https://registry.npmjs.org/${name}/-/${bareName}-${version}.tgz`;
    const packageEntry = text.match(new RegExp(
      `^\\s*'${escapeRegExp(name)}@${escapeRegExp(version)}':\\s*\\n\\s*resolution:\\s*\\{([^}\\n]*)\\}`,
      "mu",
    ));
    invariant(
      packageEntry,
      `adapter lockfile ${name} package resolution is missing`,
    );
    const resolution = packageEntry[1];
    invariant(
      resolution.includes(`tarball: ${tarball}`),
      `adapter lockfile ${name} must resolve from the public npm registry with SHA-512 integrity`,
    );
    const integrity = resolution.match(/(?:^|,)\s*integrity:\s*(sha512-[A-Za-z0-9+/]{86}==)(?:,|$)/u)?.[1];
    invariant(integrity, `adapter lockfile ${name} must contain a valid SHA-512 integrity`);
    if (expectedIntegrities?.[name] !== undefined) {
      invariant(
        integrity === expectedIntegrities[name],
        `adapter lockfile ${name} integrity does not match audited tarball`,
      );
    }
    integrities[name] = integrity;
  }
  return Object.freeze(integrities);
}

function auditCorePackage(config, pack, version) {
  invariant(pack.name === config.name, `${config.path}: expected ${config.name}, found ${pack.name}`);
  invariant(pack.version === version, `${config.name}: version ${pack.version} does not match ${version}`);
  const manifest = readJson(join(ROOT, config.path, "package.json"));
  invariant(manifest.publishConfig?.registry === PUBLIC_REGISTRY, `${config.name}: must publish to the public npm registry`);
  invariant(manifest.license === "AGPL-3.0-only", `${config.name}: license metadata must be AGPL-3.0-only`);
  const packedPaths = new Set(pack.files.map(({ path }) => path));
  invariant(packedPaths.has("LICENSE"), `${config.name}: LICENSE is missing from tarball`);
  invariant(packedPaths.has("COMMERCIAL-LICENSING.md"), `${config.name}: COMMERCIAL-LICENSING.md is missing from tarball`);
  invariant(packedPaths.has("THIRD_PARTY_NOTICES.md"), `${config.name}: THIRD_PARTY_NOTICES.md is missing from tarball`);
  if (config.name === "@samarthmn/dockosha-provider-docyantra") {
    invariant(packedPaths.has("build-fingerprint.json"), `${config.name}: dist fingerprint is missing from tarball`);
  }
  invariant(readFileSync(join(ROOT, config.path, "LICENSE")).equals(ROOT_LICENSE), `${config.name}: LICENSE is not byte-identical to the approved root license`);
  invariant(readFileSync(join(ROOT, config.path, "COMMERCIAL-LICENSING.md")).equals(ROOT_COMMERCIAL_NOTICE), `${config.name}: commercial licensing notice is not byte-identical to the root notice`);
  invariant(packedBytes(pack, "LICENSE").equals(ROOT_LICENSE), `${config.name}: packed LICENSE bytes differ from the approved root license`);
  invariant(packedBytes(pack, "COMMERCIAL-LICENSING.md").equals(ROOT_COMMERCIAL_NOTICE), `${config.name}: packed commercial licensing notice bytes differ from the root notice`);
  invariant(packedBytes(pack, "THIRD_PARTY_NOTICES.md").equals(readFileSync(join(ROOT, config.path, "THIRD_PARTY_NOTICES.md"))), `${config.name}: packed THIRD_PARTY_NOTICES.md bytes differ from the checked-in notice`);
  if (config.name === "@samarthmn/doc-yantra") {
    invariant([...packedPaths].some((path) => /adobe-cmap/i.test(path)), `${config.name}: Adobe CMap notice is missing from tarball`);
    invariant(readFileSync(join(ROOT, config.path, "NOTICE-ADOBE-CMAP.md")).equals(ADOBE_NOTICE), `${config.name}: Adobe CMap notice bytes are not approved`);
    invariant(packedBytes(pack, "NOTICE-ADOBE-CMAP.md").equals(ADOBE_NOTICE), `${config.name}: packed Adobe CMap notice bytes are not approved`);
  }
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
    invariant(packedPaths.has("THIRD_PARTY_RUST_LICENSES.txt"), `${config.name}: Rust dependency license inventory is missing from tarball`);
    const approvedRustNotice = readFileSync(join(ROOT, config.rustNotice));
    invariant(
      packedBytes(pack, "THIRD_PARTY_RUST_LICENSES.txt").equals(approvedRustNotice),
      `${config.name}: packed Rust dependency license inventory differs from the locked generated notice`,
    );
  } else {
    invariant(wasmFiles.length === 0, `${config.name}: adapter must not contain a WASM binary`);
  }
  return Object.freeze({
    name: config.name,
    integrity: pack.integrity,
    packedBytes: pack.size,
    unpackedBytes: pack.unpackedSize,
    fileCount: pack.files.length,
    wasmFiles,
  });
}

function auditFontPackage(pack, version) {
  invariant(pack.name === FONT_PACKAGE.name, `font package name is ${pack.name}`);
  invariant(pack.version === version, `font package version ${pack.version} does not match ${version}`);
  const fontPackage = readJson(join(ROOT, FONT_PACKAGE.path, "package.json"));
  invariant(fontPackage.publishConfig?.registry === PUBLIC_REGISTRY, `${FONT_PACKAGE.name}: must publish to the public npm registry`);
  invariant(fontPackage.license === "AGPL-3.0-only AND OFL-1.1", `${FONT_PACKAGE.name}: license metadata must disclose AGPL-3.0-only and OFL-1.1`);
  invariant(pack.size <= FONT_PACKED_LIMIT, `offline font pack is ${pack.size} bytes; limit is ${FONT_PACKED_LIMIT}`);
  const packageRoot = join(ROOT, FONT_PACKAGE.path);
  const manifest = readJson(join(packageRoot, "fonts", "manifest.json"));
  invariant(Array.isArray(manifest.fonts) && manifest.fonts.length === 17, "font manifest must contain 17 faces");
  const packedPaths = new Set(pack.files.map(({ path }) => path));
  invariant(packedPaths.has("LICENSE"), `${FONT_PACKAGE.name}: LICENSE is missing from tarball`);
  invariant(packedPaths.has("COMMERCIAL-LICENSING.md"), `${FONT_PACKAGE.name}: COMMERCIAL-LICENSING.md is missing from tarball`);
  invariant(packedPaths.has("THIRD_PARTY_NOTICES.md"), `${FONT_PACKAGE.name}: THIRD_PARTY_NOTICES.md is missing from tarball`);
  invariant(readFileSync(join(ROOT, FONT_PACKAGE.path, "LICENSE")).equals(ROOT_LICENSE), `${FONT_PACKAGE.name}: LICENSE is not byte-identical to the approved root license`);
  invariant(readFileSync(join(ROOT, FONT_PACKAGE.path, "COMMERCIAL-LICENSING.md")).equals(ROOT_COMMERCIAL_NOTICE), `${FONT_PACKAGE.name}: commercial licensing notice is not byte-identical to the root notice`);
  invariant(packedBytes(pack, "LICENSE").equals(ROOT_LICENSE), `${FONT_PACKAGE.name}: packed LICENSE bytes differ from the approved root license`);
  invariant(packedBytes(pack, "COMMERCIAL-LICENSING.md").equals(ROOT_COMMERCIAL_NOTICE), `${FONT_PACKAGE.name}: packed commercial licensing notice bytes differ from the root notice`);
  invariant(packedBytes(pack, "THIRD_PARTY_NOTICES.md").equals(readFileSync(join(packageRoot, "THIRD_PARTY_NOTICES.md"))), `${FONT_PACKAGE.name}: packed THIRD_PARTY_NOTICES.md bytes differ from the checked-in notice`);
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
    integrity: pack.integrity,
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
    ["Office package", readJson(join(ROOT, "office-core/wasm/pkg/package.json"))],
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

export function auditPackages({ packedDir } = {}) {
  if (packedDir && !statSync(packedDir).isDirectory()) throw new Error(`packed tarball directory is not a directory: ${packedDir}`);
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
    auditAdapterLockfile(version);
    for (const [target, stamp] of [
      ["pdf", "pdf-core/wasm/pkg/build-fingerprint.json"],
      ["office", "office-core/wasm/pkg/build-fingerprint.json"],
      ["adapter", "packages/dockosha-provider/build-fingerprint.json"],
    ]) {
      verifyFingerprint(target, join(ROOT, stamp));
    }

    const packed = (packagePath) => packedDir
      ? packDryRun(packagePath, scratch, packedDir)
      : deterministicPack(packagePath, scratch);
    const corePackages = PACKAGES.map((config) =>
      auditCorePackage(config, packed(config.path), version),
    );
    const packedBytes = corePackages.reduce((total, item) => total + item.packedBytes, 0);
    const unpackedBytes = corePackages.reduce((total, item) => total + item.unpackedBytes, 0);
    invariant(packedBytes <= CORE_PACKED_LIMIT, `combined core is ${packedBytes} packed bytes; limit is ${CORE_PACKED_LIMIT}`);
    invariant(
      unpackedBytes <= CORE_UNPACKED_LIMIT,
      `combined core is ${unpackedBytes} unpacked bytes; limit is ${CORE_UNPACKED_LIMIT}`,
    );
    auditAdapterLockfile(
      version,
      undefined,
      Object.fromEntries(corePackages.map(({ name, integrity }) => [name, integrity])),
    );
    const fonts = auditFontPackage(packed(FONT_PACKAGE.path), version);
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
  const packedIndex = process.argv.indexOf("--packed-dir");
  const packedDir = packedIndex === -1 ? undefined : resolve(process.argv[packedIndex + 1] ?? "");
  const result = auditPackages({ packedDir });
  if (process.argv.includes("--json")) {
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } else {
    process.stdout.write(
      `package audit passed: core ${result.core.packedBytes} packed / ${result.core.unpackedBytes} unpacked bytes; fonts ${result.fonts.packedBytes} packed bytes\n`,
    );
  }
}
