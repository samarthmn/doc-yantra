import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { mkdirSync, readFileSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const FONT_EXTENSION = /\.(?:otc|otf|ttc|ttf|woff2?)$/iu;
const FONT_MAGICS = [
  Buffer.from([0x00, 0x01, 0x00, 0x00]),
  Buffer.from("OTTO"),
  Buffer.from("ttcf"),
  Buffer.from("wOFF"),
  Buffer.from("wOF2"),
];

function hasPlausibleFontAt(bytes, offset) {
  const magic = bytes.subarray(offset, offset + 4).toString("latin1");
  if (magic === "wOFF" || magic === "wOF2") {
    if (offset + 48 > bytes.length) return false;
    const length = bytes.readUInt32BE(offset + 8);
    const tables = bytes.readUInt16BE(offset + 12);
    return tables > 0 && tables <= 512 && length >= 48 && offset + length <= bytes.length;
  }
  if (magic === "ttcf") {
    if (offset + 16 > bytes.length) return false;
    const faces = bytes.readUInt32BE(offset + 8);
    if (faces === 0 || faces > 256 || offset + 12 + faces * 4 > bytes.length) return false;
    for (let index = 0; index < faces; index += 1) {
      const faceOffset = offset + bytes.readUInt32BE(offset + 12 + index * 4);
      if (!hasPlausibleFontAt(bytes, faceOffset)) return false;
    }
    return true;
  }
  if (offset + 12 > bytes.length) return false;
  const tables = bytes.readUInt16BE(offset + 4);
  if (tables === 0 || tables > 512 || offset + 12 + tables * 16 > bytes.length) return false;
  for (let index = 0; index < tables; index += 1) {
    const record = offset + 12 + index * 16;
    const tag = bytes.subarray(record, record + 4);
    if (!tag.every((byte) => byte >= 0x20 && byte <= 0x7e)) return false;
    const tableOffset = bytes.readUInt32BE(record + 8);
    const tableLength = bytes.readUInt32BE(record + 12);
    if (tableOffset < 12 + tables * 16 || offset + tableOffset + tableLength > bytes.length) {
      return false;
    }
  }
  return true;
}

function findFontProgram(bytes) {
  for (const magic of FONT_MAGICS) {
    let offset = bytes.indexOf(magic);
    while (offset !== -1) {
      if (hasPlausibleFontAt(bytes, offset)) return offset;
      offset = bytes.indexOf(magic, offset + 1);
    }
  }
  return -1;
}

function cargoPackageVersion(relativePath) {
  const source = readFileSync(join(root, relativePath), "utf8");
  const packageAt = source.search(/^\[package\]\s*$/m);
  assert.notEqual(packageAt, -1, `${relativePath}: missing [package] section`);
  const afterHeader = source.slice(packageAt).replace(/^\[package\]\s*$/m, "");
  const nextSection = afterHeader.search(/^\[/m);
  const packageSection = nextSection === -1 ? afterHeader : afterHeader.slice(0, nextSection);
  const version = packageSection?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  assert.ok(version, `${relativePath}: missing [package] version`);
  return version;
}

function packageVersion(relativePath) {
  return JSON.parse(readFileSync(join(root, relativePath), "utf8")).version;
}

test("all release surfaces carry one immutable version", () => {
  const versions = new Map([
    ["pdf-core", cargoPackageVersion("pdf-core/Cargo.toml")],
    ["pdf-core-wasm", cargoPackageVersion("pdf-core/wasm/Cargo.toml")],
    ["office-core", cargoPackageVersion("office-core/Cargo.toml")],
    ["office-core-wasm", cargoPackageVersion("office-core/wasm/Cargo.toml")],
    ["PDF npm package", packageVersion("pdf-core/wasm/pkg/package.json")],
    ["Office npm package", packageVersion("office-core/wasm/pkg/package.json")],
    ["PDF wasm engine", require(join(root, "pdf-core/wasm/pkg/pdf_core_wasm.js")).version()],
    [
      "Office wasm engine",
      require(join(root, "office-core/wasm/pkg/office_core_wasm.js")).version(),
    ],
  ]);
  const expected = versions.values().next().value;
  assert.match(
    expected,
    /^0\.0\.\d+$/,
    `release version must stay on the 0.0.x line: ${expected}`,
  );
  for (const [surface, version] of versions) {
    assert.equal(version, expected, `${surface} has release version ${version}, expected ${expected}`);
  }
});

test("office source fingerprint covers the fontless host package surface", () => {
  const fingerprintSource = readFileSync(join(root, "scripts/source-fingerprint.mjs"), "utf8");
  const officeInputs = fingerprintSource.match(/office:\s*\[([\s\S]*?)\n\s*\],/u)?.[1];
  assert.ok(officeInputs, "scripts/source-fingerprint.mjs must declare office inputs");
  assert.match(
    officeInputs,
    /"office-core\/build\.rs"/u,
    "changing office-core/build.rs must invalidate the Office package fingerprint",
  );
  for (const path of [
    "office-core/wasm/index.cjs",
    "office-core/wasm/index.d.ts",
    "office-core/wasm/font-provider.cjs",
    "office-core/wasm/font-provider.d.ts",
    "office-core/wasm/font-provider-web.js",
    "office-core/wasm/font-provider-web.d.ts",
    "office-core/wasm/web-index.js",
    "office-core/wasm/web-index.d.ts",
  ]) {
    assert.match(officeInputs, new RegExp(JSON.stringify(path).replaceAll("/", "\\/")));
  }
});

test("packed Office artifact contains no font file or embedded font program", () => {
  const packageRoot = join(root, "office-core", "wasm", "pkg");
  const cache = join(root, "tmp", `npm-pack-cache-${process.pid}`);
  mkdirSync(cache, { recursive: true });
  let manifest;
  try {
    manifest = JSON.parse(execFileSync(
      "npm",
      ["pack", "--dry-run", "--json", "--cache", cache],
      { cwd: packageRoot, encoding: "utf8" },
    ));
  } finally {
    rmSync(cache, { recursive: true, force: true });
  }
  const files = manifest[0]?.files?.map(({ path }) => path);
  assert.ok(Array.isArray(files) && files.length > 0, "npm pack returned no Office files");
  for (const path of files) {
    assert.doesNotMatch(path, FONT_EXTENSION, `packed font binary: ${path}`);
    const bytes = readFileSync(join(packageRoot, path));
    assert.equal(findFontProgram(bytes), -1, `embedded font program in packed file: ${path}`);
    if (!/\.(?:c?js|mjs|json|ts)$/iu.test(path)) continue;
    const source = bytes.toString("utf8");
    for (const candidate of source.match(/[A-Za-z0-9+/]{48,}={0,2}/gu) ?? []) {
      const decoded = Buffer.from(candidate, "base64");
      assert.equal(
        findFontProgram(decoded),
        -1,
        `base64-encoded font program in packed file: ${path}`,
      );
    }
    assert.doesNotMatch(
      source,
      /\[(?:\s*(?:0x00|0)\s*,\s*(?:0x01|1)\s*,\s*(?:0x00|0)\s*,\s*(?:0x00|0)\s*,)(?:\s*(?:0x[\da-f]+|\d+)\s*,){8,}/iu,
      `generated font byte array in packed file: ${path}`,
    );
  }
});
