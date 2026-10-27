#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function argument(name) {
  const at = process.argv.indexOf(name);
  if (at === -1 || !process.argv[at + 1]) throw new Error(`${name} is required`);
  return resolve(ROOT, process.argv[at + 1]);
}

function optionalArgument(name) {
  const at = process.argv.indexOf(name);
  if (at === -1) return undefined;
  if (!process.argv[at + 1]) throw new Error(`${name} requires a path`);
  return resolve(ROOT, process.argv[at + 1]);
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function verifyChecksums(directory) {
  const lines = readFileSync(join(directory, "SHA256SUMS"), "utf8")
    .trim()
    .split("\n");
  for (const line of lines) {
    const match = line.match(/^([0-9a-f]{64})  (.+)$/u);
    if (!match) throw new Error(`invalid checksum line: ${line}`);
    const path = join(directory, match[2]);
    if (sha256(path) !== match[1]) throw new Error(`checksum mismatch: ${match[2]}`);
  }
}

const VERIFY_SOURCE = String.raw`
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";

const require = createRequire(import.meta.url);
const pdf = require("@samarthmn/doc-yantra");
const office = require("@samarthmn/doc-yantra-office");
assert.match(pdf.version(), /^0\.0\.\d+$/);
assert.equal(office.version(), pdf.version());

const pdfWasm = require.resolve("@samarthmn/doc-yantra/pdf_core_wasm_bg.wasm");
const officeWasm = require.resolve("@samarthmn/doc-yantra-office/office_core_wasm_bg.wasm");
assert.equal(existsSync(pdfWasm), true);
assert.equal(existsSync(officeWasm), true);

const pdfWeb = await import("@samarthmn/doc-yantra/web");
await pdfWeb.default({ module_or_path: readFileSync(pdfWasm) });
assert.match(pdfWeb.version(), /^0\.0\.\d+$/);
const browserPdf = pdfWeb.text_to_pdf("browser asset verification");
assert.equal(Buffer.from(browserPdf.subarray(0, 5)).toString(), "%PDF-");

const officeWeb = await import("@samarthmn/doc-yantra-office/web");
await officeWeb.default({ module_or_path: readFileSync(officeWasm) });
assert.equal(officeWeb.version(), office.version());

const { createPdfEngineWorker } = require("@samarthmn/doc-yantra/worker");
const pdfWorker = createPdfEngineWorker({ fontProvider: false });
try {
  const generated = pdf.text_to_pdf("worker verification");
  assert.equal(await pdfWorker.pageCount({ pdf: generated }), 1);
} finally {
  await pdfWorker.close();
}

const { createOfficeEngineWorker } = require("@samarthmn/doc-yantra-office/worker");
const officeWorker = createOfficeEngineWorker({ notoFallback: false });
try {
  assert.equal(await officeWorker.engineVersion(), office.version());
} finally {
  await officeWorker.close();
}

const adapter = require("@samarthmn/dockosha-provider-docyantra");
const provider = adapter.createProvider();
assert.equal(provider.id, "docyantra");
const assets = join(process.cwd(), "browser-assets");
adapter.syncBrowserAssets({ targetDirectory: assets });
assert.equal(existsSync(join(assets, "pdf_core_wasm.js")), true);
assert.equal(existsSync(join(assets, "pdf_core_wasm_bg.wasm")), true);
`;

const VERIFY_OFFLINE_SOURCE = String.raw`
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
globalThis.fetch = async () => assert.fail("offline verification attempted a network request");

const pdf = require("@samarthmn/doc-yantra");
const fonts = require("@samarthmn/doc-yantra-fonts");
const fontPackage = require("@samarthmn/doc-yantra-fonts/package.json");
assert.equal(fontPackage.version, pdf.version());
assert.equal(fonts.manifest.fonts.length, 17);

const { createFontProvider } = require("@samarthmn/doc-yantra/font-provider");
const exact = await createFontProvider().resolve("LiberationSans-Regular.ttf");
assert.equal(exact.source, "offline");

const telemetry = [];
const { createPdfEngineWorker } = require("@samarthmn/doc-yantra/worker");
const worker = createPdfEngineWorker({
  fontFetch: globalThis.fetch,
  fontTelemetry: (event) => telemetry.push(event),
});
try {
  const output = await worker.textToPdf({
    text: "Καλημέρα العربية हिन्दी ไทย 中文 😀",
  });
  assert.equal(Buffer.from(output.subarray(0, 5)).toString(), "%PDF-");
  assert.equal(await worker.pageCount({ pdf: output }), 1);
  const resolutions = telemetry.filter(({ type }) => type === "font_resolved");
  assert.equal(resolutions.length, 10);
  assert.ok(resolutions.every(({ source }) => source === "offline"));
} finally {
  await worker.close();
}
`;

function installProject(project, tarballs, cache, extras = []) {
  mkdirSync(project, { recursive: true });
  writeFileSync(
    join(project, "package.json"),
    `${JSON.stringify({ private: true, type: "module" }, null, 2)}\n`,
  );
  execFileSync(
    "npm",
    [
      "install",
      "--no-save",
      "--package-lock=false",
      "--ignore-scripts",
      "--no-audit",
      "--no-fund",
      "--registry=https://registry.npmjs.org",
      "--cache",
      cache,
      ...tarballs,
      ...extras,
      "zod@4.4.3",
    ],
    { cwd: project, stdio: "pipe" },
  );
}

export function verifyReleaseAsset(coreAsset, fontAsset) {
  if (!existsSync(coreAsset)) throw new Error(`core asset does not exist: ${coreAsset}`);
  if (fontAsset !== undefined && !existsSync(fontAsset)) {
    throw new Error(`font asset does not exist: ${fontAsset}`);
  }
  const scratch = join(ROOT, "tmp", `release-install-verification-${process.pid}`);
  const extracted = join(scratch, "extracted");
  const project = join(scratch, "project");
  const offlineProject = join(scratch, "offline-project");
  const cache = join(scratch, "npm-cache");
  mkdirSync(extracted, { recursive: true });
  mkdirSync(cache, { recursive: true });
  try {
    execFileSync("unzip", ["-q", coreAsset, "-d", extracted], { stdio: "pipe" });
    verifyChecksums(extracted);
    const tarballs = readdirSync(join(extracted, "packages"))
      .filter((name) => name.endsWith(".tgz"))
      .sort()
      .map((name) => join(extracted, "packages", name));
    if (tarballs.length !== 3) throw new Error(`core asset contains ${tarballs.length} npm tarballs`);
    installProject(project, tarballs, cache);
    writeFileSync(join(project, "verify.mjs"), VERIFY_SOURCE);
    execFileSync(process.execPath, ["verify.mjs"], {
      cwd: project,
      env: { ...process.env, NODE_TEST_CONTEXT: "release-asset" },
      stdio: "pipe",
    });
    if (fontAsset !== undefined) {
      installProject(offlineProject, tarballs, cache, [fontAsset]);
      writeFileSync(join(offlineProject, "verify-offline.mjs"), VERIFY_OFFLINE_SOURCE);
      execFileSync(process.execPath, ["verify-offline.mjs"], {
        cwd: offlineProject,
        env: { ...process.env, NODE_TEST_CONTEXT: "release-asset-offline" },
        stdio: "pipe",
      });
    }
    return Object.freeze({ coreAsset, tarballs: tarballs.length, offlineFonts: fontAsset !== undefined });
  } finally {
    rmSync(scratch, { force: true, recursive: true });
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const result = verifyReleaseAsset(argument("--core"), optionalArgument("--fonts"));
  process.stdout.write(
    `verified clean install from ${result.coreAsset}${result.offlineFonts ? " with offline fonts" : ""}\n`,
  );
}
