#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { auditPackages } from "./package-audit.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const TMP_ROOT = join(ROOT, "tmp");
const CORE_ZIP_LIMIT = 6 * 1024 * 1024;
const FONT_TGZ_LIMIT = 145 * 1024 * 1024;
const CORE_PACKAGES = Object.freeze([
  "pdf-core/wasm/pkg",
  "office-core/wasm/pkg",
  "packages/dockosha-provider",
]);
const FONT_PACKAGE = "packages/doc-yantra-fonts";

function parseOutputDirectory(args, version) {
  const at = args.indexOf("--out-dir");
  const value = at === -1 ? join(TMP_ROOT, `release-assets-${version}`) : args[at + 1];
  if (!value) throw new Error("--out-dir requires a path");
  const output = resolve(ROOT, value);
  const relativePath = relative(TMP_ROOT, output);
  if (relativePath === "" || relativePath.startsWith(`..${sep}`) || relativePath === "..") {
    throw new Error(`release output must be a child of ${TMP_ROOT}`);
  }
  if (existsSync(output) && readdirSync(output).length > 0) {
    throw new Error(`release output directory is not empty: ${output}`);
  }
  return output;
}

function npmPack(packagePath, destination, cache) {
  const result = JSON.parse(
    execFileSync(
      "npm",
      [
        "pack",
        "--json",
        "--ignore-scripts",
        "--pack-destination",
        destination,
        "--cache",
        cache,
      ],
      {
        cwd: join(ROOT, packagePath),
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
      },
    ),
  );
  if (result.length !== 1 || typeof result[0]?.filename !== "string") {
    throw new Error(`${packagePath}: npm pack returned an invalid result`);
  }
  return join(destination, result[0].filename);
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function installationGuide(version, tarballs) {
  return `# DocYantra WASM core ${version}

This consumer archive contains only the three installable core npm tarballs,
their SHA-256 checksums, and this guide. It intentionally excludes Rust source,
tests, fixtures, corpora, and font binaries.

Install all three tarballs from an empty project:

\`\`\`bash
npm install ${tarballs.map((name) => `./packages/${name}`).join(" ")}
\`\`\`

For fully offline Unicode and Office conversion, also download and install
\`doc-yantra-fonts-offline-${version}.tgz\`. Without it, exact hash-pinned
fonts are fetched on demand from the first-party DocKosha asset host.

The GitHub-generated “Source code” ZIP and tarball are development archives;
applications should use this named WASM core asset instead.
`;
}

export function createReleaseAssets(args = process.argv.slice(2)) {
  const audit = auditPackages();
  const output = parseOutputDirectory(args, audit.version);
  const staging = join(output, "core-staging");
  const packages = join(staging, "packages");
  const cache = join(output, "npm-cache");
  mkdirSync(packages, { recursive: true });
  mkdirSync(cache, { recursive: true });
  try {
    const coreTarballs = CORE_PACKAGES.map((packagePath) =>
      npmPack(packagePath, packages, cache),
    );
    const tarballNames = coreTarballs.map((path) => path.slice(packages.length + 1));
    const checksumLines = coreTarballs
      .map((path) => `${sha256(path)}  packages/${path.slice(packages.length + 1)}`)
      .join("\n");
    writeFileSync(join(staging, "SHA256SUMS"), `${checksumLines}\n`);
    writeFileSync(join(staging, "INSTALL.md"), installationGuide(audit.version, tarballNames));

    const coreAsset = join(output, `doc-yantra-wasm-core-${audit.version}.zip`);
    execFileSync("zip", ["-q", "-X", "-r", coreAsset, "."], {
      cwd: staging,
      stdio: "pipe",
    });
    const coreBytes = statSync(coreAsset).size;
    if (coreBytes > CORE_ZIP_LIMIT) {
      throw new Error(`core release asset is ${coreBytes} bytes; limit is ${CORE_ZIP_LIMIT}`);
    }

    const packedFont = npmPack(FONT_PACKAGE, output, cache);
    const fontAsset = join(output, `doc-yantra-fonts-offline-${audit.version}.tgz`);
    renameSync(packedFont, fontAsset);
    const fontBytes = statSync(fontAsset).size;
    if (fontBytes > FONT_TGZ_LIMIT) {
      throw new Error(`offline font asset is ${fontBytes} bytes; limit is ${FONT_TGZ_LIMIT}`);
    }
    return Object.freeze({
      version: audit.version,
      coreAsset,
      coreBytes,
      fontAsset,
      fontBytes,
    });
  } finally {
    rmSync(staging, { force: true, recursive: true });
    rmSync(cache, { force: true, recursive: true });
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const result = createReleaseAssets();
  process.stdout.write(
    process.argv.includes("--json")
      ? `${JSON.stringify(result)}\n`
      : `created ${result.coreAsset} (${result.coreBytes} bytes) and ${result.fontAsset} (${result.fontBytes} bytes)\n`,
  );
}
