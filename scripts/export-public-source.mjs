#!/usr/bin/env node

import { cpSync, existsSync, lstatSync, mkdirSync, readdirSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { verifyFingerprint } from "./source-fingerprint.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const EXCLUDED = new Set([
  ".git", ".claude", ".superpowers", "hyper-agent", "target", "tmp", ".DS_Store",
  "node_modules", "mock-files", "fidelity-corpus", "real-world-test-files", "cache", "caches",
  ".cache", "tests", "test", "fidelity", "CLAUDE.md",
]);
const EXCLUDED_PATHS = new Set([
  ".github/workflows/fidelity.yml",
  "docs/superpowers",
  "office-core/fidelity/fonts-cache",
  "office-core/LAYOUT_CALIBRATION.md",
  "scripts/llm-fidelity.mjs",
]);
const TOP_LEVEL = new Set([
  ".github", ".gitignore", "AGENTS.md", "COMMERCIAL-LICENSING.md", "CONSUMING.md", "CONTRIBUTING.md",
  "FIXTURE_PROVENANCE.md", "FIXTURE_PROVENANCE.json", "LICENSE", "README.md", "SECURITY.md", "SUPPORT.md",
  "THIRD_PARTY_NOTICES.md", "TRADEMARKS.md", "legal", "rust-toolchain.toml", "office-core", "packages", "pdf-core",
  "public-tests", "scripts", "tests", "text-core", "wasm-worker",
]);
const SECRET_NAME = /(^|\/)(?:\.env(?:\..*)?|.*(?:secret|credential|token|password|private[-_]?key).*|id_rsa(?:\.pub)?|npmrc)$/iu;
const PLACEHOLDER = /^(?:$|example|example\.com|placeholder|changeme|change[-_ ]?me|redacted|dummy|test|testing|replace[-_ ]?me|your[_ -]?[a-z0-9_-]+|<[^>]+>|\$[A-Z_][A-Z0-9_]*|\$\{\{[^}]+\}\}|\$\{[^}]+\})$/iu;
const SECRET_PATTERNS = [
  /-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----/gu,
  /(?:^|[\s"'])(?:npm_config_)?(?:_authToken|authToken)\s*[:=]\s*["']?([^\s"']+)/giu,
  /(?:^|[\s"'])\b(?:GITHUB_TOKEN|GH_TOKEN|NPM_TOKEN|AWS_SECRET_ACCESS_KEY|AWS_SESSION_TOKEN|STRIPE_SECRET_KEY|api[_-]?(?:key|token)|access[_-]?token|auth[_-]?token|secret[_-]?key|password|passwd|credential)\b\s*[:=]\s*["']?(\$\{\{[^}]+\}\}|[^\s"']+)/giu,
  /\bAKIA[0-9A-Z]{16}\b/gu,
  /\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{12,}\b/gu,
  /\b(?:ghp_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{50,}|npm_[A-Za-z0-9]{36,}|AIza[A-Za-z0-9_-]{35})\b/gu,
  /https?:\/\/[^\s/@:]+:([^\s/@]+)@/giu,
  /(?:npm\.pkg\.github\.com|registry\.npmjs\.org\/.+_authToken\s*=\s*[^\s]+)/giu,
];
const BINARY_ALLOW = /\.(?:wasm|pdf|png|jpe?g|gif|webp|docx|pptx|xlsx|xlsm|xls|ttf|otf|ttc|woff2?)$/iu;
const TEXT_EXT = /\.(?:c|cc|cpp|css|cmake|csv|d\.ts|d\.tsx|gn|h|hbs|hh|hpp|inc|inl|html?|js|json|mjs|cjs|map|md|py|rs|sh|toml|ts|tsx|yaml|yml|lock|txt|xml)$/iu;
const TEXT_FILE = /(^|\/)(?:LICENSE|NOTICE[^/]*|VERSION|\.gitignore|pdf-core\/assets\/cmaps\/[^/]+)$/iu;
const MANIFEST_PATH = "scripts/public-source-manifest.json";
const APPROVED_DISTRIBUTION_BINARY_PREFIXES = [
  "office-core/fonts/",
  "packages/doc-yantra-fonts/fonts/",
  "pdf-core/wasm/pkg/",
  "office-core/wasm/pkg/",
];
const BUILDER_PATH = /(?:\/home\/[^/]+\/|\/Users\/[^/]+\/|[A-Za-z]:\\Users\\|\/__w\/[^/]+\/)/u;

function isPdfiumNotice(path) {
  return path === "pdf-core/pdfium/LICENSE" || path.startsWith("pdf-core/pdfium/licenses/");
}

function fail(message) {
  throw new Error(`public source audit failed: ${message}`);
}

function isExcluded(path) {
  // The CDN-tool contract is deliberately public: consumers must be able to
  // reproduce the checked-in font assets without the private test corpus.
  if (path === "tests" || path === "tests/font-cdn-tools.test.mjs" || path === "tests/license-metadata.test.mjs") return false;
  if (path === "pdf-core/pdfium" || path === "pdf-core/pdfium/licenses") return false;
  if (path.startsWith("pdf-core/pdfium/")) return !isPdfiumNotice(path);
  return EXCLUDED_PATHS.has(path) || path.split("/").some((part) => EXCLUDED.has(part));
}

function inspectTree(source, current = "", files = []) {
  const absolute = join(source, current);
  const stat = lstatSync(absolute);
  if (stat.isSymbolicLink()) fail(`symlink is not permitted: ${current || "."}`);
  if (stat.isFile()) {
    if (current && !isExcluded(current)) files.push(current);
    return files;
  }
  if (!stat.isDirectory()) fail(`unsupported filesystem entry: ${current}`);
  for (const entry of readdirSync(absolute).sort()) {
    const next = current ? `${current}/${entry}` : entry;
    const nextStat = lstatSync(join(source, next));
    if (nextStat.isSymbolicLink()) fail(`symlink is not permitted: ${next}`);
    if (isExcluded(next)) continue;
    inspectTree(source, next, files);
  }
  return files;
}

function auditFiles(source, files) {
  for (const path of files) {
    const top = path.split("/")[0];
    if (SECRET_NAME.test(path)) fail(`secret-like path: ${path}`);
    if (!TOP_LEVEL.has(top)) fail(`unexpected top-level path: ${path}`);
    const absolute = join(source, path);
    const bytes = readFileSync(absolute);
    const isText = TEXT_EXT.test(path) || TEXT_FILE.test(path) || isPdfiumNotice(path);
    if (!isText && !BINARY_ALLOW.test(path)) fail(`unapproved binary or unknown content: ${path}`);
    if (isText) {
      const text = bytes.toString("utf8");
      if (text.includes("\u0000")) fail(`binary content has text extension: ${path}`);
      auditSecretText(path, text);
    } else if (path.endsWith(".wasm")) {
      const text = bytes.toString("latin1");
      auditSecretText(path, text);
      if (BUILDER_PATH.test(text)) fail(`absolute builder path in generated WASM: ${path}`);
    }
  }
}

function auditPathManifest(source, files) {
  const manifestPath = join(source, MANIFEST_PATH);
  if (!existsSync(manifestPath)) fail(`required public path manifest is missing: ${MANIFEST_PATH}`);
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  if (manifest.schemaVersion !== 1 || !Array.isArray(manifest.files)) fail("public path manifest schema is invalid");
  const expected = [...new Set(manifest.files)].sort();
  const actual = [...files].sort();
  if (JSON.stringify(expected) !== JSON.stringify(actual)) {
    const missing = expected.filter((file) => !actual.includes(file));
    const unexpected = actual.filter((file) => !expected.includes(file));
    fail(`public path manifest mismatch; missing=${missing.join(",") || "none"}; unexpected=${unexpected.join(",") || "none"}`);
  }
}

function assertSafeDestination(destination, sourceRoot) {
  const safeTempRoot = resolve(ROOT, "tmp");
  const insideSource = destination === sourceRoot || destination.startsWith(`${sourceRoot}${sep}`);
  if (insideSource && !destination.startsWith(`${safeTempRoot}${sep}`)) {
    fail("output must be outside the source tree unless it is a fresh child of source-root tmp/");
  }
  // Every existing ancestor is checked with lstat. This prevents a seemingly
  // safe nested path from traversing a symlink to an unrelated directory.
  let current = destination;
  while (true) {
    try {
      if (lstatSync(current).isSymbolicLink()) fail(`output path may not traverse a symlink: ${current}`);
    } catch (error) {
      if (error instanceof Error && error.message.startsWith("public source audit failed:")) throw error;
      if (error?.code !== "ENOENT") throw error;
    }
    const parent = dirname(current);
    if (parent === current) break;
    current = parent;
  }
}

export function auditSecretText(path, text) {
  for (const pattern of SECRET_PATTERNS) {
    pattern.lastIndex = 0;
    for (const match of text.matchAll(pattern)) {
      const value = match[1] ?? match[0];
      if (!PLACEHOLDER.test(value.replace(/^['"]|['"]$/gu, ""))) fail(`secret or credential material: ${path}`);
    }
  }
}

export function validateProvenanceData(provenance) {
  if (provenance.schemaVersion !== 1 || !Array.isArray(provenance.retainedFixtures) || !Array.isArray(provenance.omittedRoots)) fail("fixture provenance JSON schema is invalid");
  for (const item of provenance.retainedFixtures) {
    if (!item.path || !item.sha256 || !item.origin || !item.rights || !item.reviewer || item.status !== "approved") fail(`fixture provenance entry is incomplete: ${item.path ?? "unknown"}`);
    if (!/^[a-f0-9]{64}$/u.test(item.sha256)) fail(`fixture provenance hash is invalid: ${item.path}`);
  }
  for (const root of ["mock-files", "fidelity-corpus", "real-world-test-files"]) {
    if (!provenance.omittedRoots.some((entry) => entry.path === root && entry.status === "omitted")) fail(`fixture provenance does not omit ${root}`);
  }
  if (!provenance.ownerGoldenIntake?.schema || !Array.isArray(provenance.ownerGoldenIntake?.steps) || provenance.ownerGoldenIntake.steps.length < 3) fail("owner-golden intake schema is incomplete");
}

export function auditBinaryProvenance(source, files, provenance) {
  const retained = new Map((provenance.retainedFixtures ?? []).map((entry) => [entry.path, entry]));
  const exported = new Set(files);
  for (const path of files) {
    if (!BINARY_ALLOW.test(path) || APPROVED_DISTRIBUTION_BINARY_PREFIXES.some((prefix) => path.startsWith(prefix))) continue;
    const entry = retained.get(path);
    if (!entry) fail(`binary fixture has no provenance entry: ${path}`);
    const actual = createHash("sha256").update(readFileSync(join(source, path))).digest("hex");
    if (actual !== entry.sha256) fail(`binary fixture provenance hash mismatch: ${path}`);
  }
  for (const path of retained.keys()) {
    if (!exported.has(path)) fail(`provenance entry is not present in the public export: ${path}`);
  }
}

function auditNotices(source) {
  for (const path of ["LICENSE", "COMMERCIAL-LICENSING.md", "THIRD_PARTY_NOTICES.md", "FIXTURE_PROVENANCE.md", "FIXTURE_PROVENANCE.json"]) {
    if (!existsSync(join(source, path))) fail(`required notice is missing: ${path}`);
  }
  const license = readFileSync(join(source, "LICENSE"), "utf8");
  for (const marker of [
    "GNU AFFERO GENERAL PUBLIC LICENSE",
    "Version 3, 19 November 2007",
    "Copyright (C) 2007 Free Software Foundation, Inc.",
    "<https://www.gnu.org/licenses/>",
  ]) {
    if (!license.includes(marker)) fail("LICENSE is missing an unmodified GNU AGPLv3 marker");
  }
  if (!readFileSync(join(source, "COMMERCIAL-LICENSING.md"), "utf8").trim()) fail("COMMERCIAL-LICENSING.md is empty");
  const provenance = JSON.parse(readFileSync(join(source, "FIXTURE_PROVENANCE.json"), "utf8"));
  validateProvenanceData(provenance);
  return provenance;
}

function auditFingerprints(source) {
  for (const [target, stamp] of [
    ["pdf", "pdf-core/wasm/pkg/build-fingerprint.json"],
    ["office", "office-core/wasm/pkg/build-fingerprint.json"],
    ["adapter", "packages/dockosha-provider/build-fingerprint.json"],
  ]) {
    const stampPath = join(source, stamp);
    if (!existsSync(stampPath)) fail(`generated artifact fingerprint is missing: ${stamp}`);
    try { verifyFingerprint(target, stampPath, source); } catch (error) { fail(`generated artifact/source fingerprint drift: ${stamp}: ${error.message}`); }
  }
}

export function auditPublicSource(source = ROOT) {
  const files = inspectTree(source);
  auditFiles(source, files);
  auditPathManifest(source, files);
  const provenance = auditNotices(source);
  auditBinaryProvenance(source, files, provenance);
  auditFingerprints(source);
  return Object.freeze({ fileCount: files.length, excluded: [...EXCLUDED].sort() });
}

export function publicSourceFiles(source = ROOT) {
  return Object.freeze([...inspectTree(source)].sort());
}

export function exportPublicSource(source = ROOT, outDir) {
  if (!outDir) throw new Error("public source export requires --out-dir");
  const result = auditPublicSource(source);
  const destination = resolve(outDir);
  const sourceRoot = resolve(source);
  assertSafeDestination(destination, sourceRoot);
  try {
    const destinationStat = lstatSync(destination);
    if (destinationStat.isSymbolicLink()) fail(`output destination may not be a symlink: ${destination}`);
    fail(`output destination already exists; refusing to delete it: ${destination}`);
  } catch (error) {
    if (error instanceof Error && error.message.startsWith("public source audit failed:")) throw error;
    if (error?.code !== "ENOENT") throw error;
  }
  mkdirSync(dirname(destination), { recursive: true });
  mkdirSync(destination, { recursive: true });
  for (const path of inspectTree(source)) {
    mkdirSync(dirname(join(destination, path)), { recursive: true });
    cpSync(join(source, path), join(destination, path));
  }
  const copied = auditPublicSource(destination);
  return Object.freeze({ ...copied, destination });
}

function args(argv) {
  const sourceIndex = argv.indexOf("--source");
  const outIndex = argv.includes("--out-dir") ? argv.indexOf("--out-dir") : argv.indexOf("--out");
  return {
    source: sourceIndex === -1 ? ROOT : resolve(argv[sourceIndex + 1]),
    outDir: outIndex === -1 ? undefined : resolve(argv[outIndex + 1]),
    checkOnly: argv.includes("--check-only"),
  };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  try {
    const options = args(process.argv.slice(2));
    const result = options.checkOnly ? auditPublicSource(options.source) : exportPublicSource(options.source, options.outDir);
    process.stdout.write(`public source audit passed: ${result.fileCount} files${result.destination ? ` -> ${result.destination}` : ""}\n`);
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  }
}
