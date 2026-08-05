#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const TMP_ROOT = join(ROOT, "tmp");
const PACKAGE_ROOT = join(ROOT, "packages", "doc-yantra-fonts");

function outputDirectory(version) {
  const at = process.argv.indexOf("--out-dir");
  const output = resolve(ROOT, at === -1 ? `tmp/font-cdn-assets-${version}` : process.argv[at + 1]);
  const relativePath = relative(TMP_ROOT, output);
  if (!relativePath || relativePath === ".." || relativePath.startsWith(`..${sep}`)) {
    throw new Error(`font CDN output must be a child of ${TMP_ROOT}`);
  }
  if (existsSync(output) && readdirSync(output).length > 0) {
    throw new Error(`font CDN output directory is not empty: ${output}`);
  }
  return output;
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

export function prepareFontCdnAssets() {
  const metadata = JSON.parse(readFileSync(join(PACKAGE_ROOT, "package.json"), "utf8"));
  const manifest = JSON.parse(readFileSync(join(PACKAGE_ROOT, "fonts", "manifest.json"), "utf8"));
  const output = outputDirectory(metadata.version);
  const publicRoot = join(output, "doc-yantra-fonts");
  const checksums = [];
  for (const entry of manifest.fonts) {
    const source = join(PACKAGE_ROOT, "fonts", entry.file);
    const bytes = readFileSync(source);
    if (bytes.byteLength !== entry.byteLength || sha256(bytes) !== entry.sha256) {
      throw new Error(`${entry.file}: canonical file differs from its manifest`);
    }
    const relativePath = join("sha256", entry.sha256, entry.file);
    const destination = join(publicRoot, relativePath);
    mkdirSync(dirname(destination), { recursive: true });
    copyFileSync(source, destination);
    checksums.push(`${entry.sha256}  doc-yantra-fonts/${relativePath}`);
  }
  writeFileSync(join(output, "SHA256SUMS"), `${checksums.join("\n")}\n`);
  writeFileSync(join(publicRoot, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  writeFileSync(
    join(output, "_headers"),
    "/doc-yantra-fonts/sha256/*\n  Access-Control-Allow-Origin: *\n  Cache-Control: public, max-age=31536000, immutable\n  X-Content-Type-Options: nosniff\n",
  );
  writeFileSync(
    join(output, "README.md"),
    "Publish the contents of this directory at https://public.dockosha.com/, preserving paths and the _headers policy. Then run scripts/verify-font-cdn.mjs before releasing.\n",
  );
  return Object.freeze({ output, version: metadata.version, fontCount: manifest.fonts.length });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const result = prepareFontCdnAssets();
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
