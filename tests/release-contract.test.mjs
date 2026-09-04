import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
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

function npmPackManifest(packageRoot, label) {
  const cache = join(root, "tmp", `npm-pack-cache-${label}-${process.pid}`);
  mkdirSync(cache, { recursive: true });
  try {
    const result = JSON.parse(
      execFileSync("npm", ["pack", "--dry-run", "--json", "--cache", cache], {
        cwd: packageRoot,
        encoding: "utf8",
      }),
    );
    assert.equal(result.length, 1, `${label}: npm pack returned ${result.length} manifests`);
    return result[0];
  } finally {
    rmSync(cache, { recursive: true, force: true });
  }
}

function runWasmVersionGuard(...args) {
  return spawnSync(process.execPath, [join(root, "scripts", "wasm-version.mjs"), ...args], {
    cwd: root,
    encoding: "utf8",
  });
}

function runSourceFingerprintGuard(...args) {
  return spawnSync(
    process.execPath,
    [join(root, "scripts", "source-fingerprint.mjs"), ...args],
    {
      cwd: root,
      encoding: "utf8",
    },
  );
}

test("wasm version guard detects and rejects a stale embedded engine version", () => {
  const scratch = join(root, "tmp", `wasm-version-guard-${process.pid}`);
  const packageRoot = join(scratch, "pkg");
  mkdirSync(packageRoot, { recursive: true });
  try {
    const manifest = join(scratch, "Cargo.toml");
    writeFileSync(manifest, '[package]\nname = "fixture-wasm"\nversion = "0.0.11"\n');
    writeFileSync(
      join(packageRoot, "package.json"),
      `${JSON.stringify({ name: "fixture-wasm", version: "0.0.11", main: "index.cjs" })}\n`,
    );
    writeFileSync(join(packageRoot, "index.cjs"), 'exports.version = () => "0.0.10";\n');

    const stale = runWasmVersionGuard("needs-clean", manifest, packageRoot);
    assert.equal(stale.status, 0, stale.stderr);
    const rejected = runWasmVersionGuard("verify", manifest, packageRoot);
    assert.notEqual(rejected.status, 0);
    assert.match(rejected.stderr, /embedded engine version 0\.0\.10, expected 0\.0\.11/u);

    writeFileSync(join(packageRoot, "index.cjs"), 'exports.version = () => "0.0.11";\n');
    const current = runWasmVersionGuard("needs-clean", manifest, packageRoot);
    assert.equal(current.status, 1, current.stderr);
    const accepted = runWasmVersionGuard("verify", manifest, packageRoot);
    assert.equal(accepted.status, 0, accepted.stderr);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
});

test("both wasm build scripts invalidate stale versioned artifacts before building", () => {
  for (const [relativePath, packages] of [
    ["pdf-core/wasm/build.sh", ["pdf-core", "pdf-core-wasm"]],
    ["office-core/wasm/build.sh", ["office-core", "office-core-wasm"]],
  ]) {
    const source = readFileSync(join(root, relativePath), "utf8");
    assert.match(source, /wasm-version\.mjs needs-clean Cargo\.toml pkg/u, relativePath);
    assert.match(source, /cargo clean/u, relativePath);
    for (const packageName of packages) {
      assert.match(source, new RegExp(`-p ${packageName}(?:\\s|$)`, "u"), relativePath);
    }
    assert.match(source, /wasm-version\.mjs verify Cargo\.toml pkg/u, relativePath);
  }
});

test("source fingerprint guard verifies committed packages and rejects a forged stamp", () => {
  for (const [target, stamp] of [
    ["pdf", "pdf-core/wasm/pkg/build-fingerprint.json"],
    ["office", "office-core/wasm/pkg/build-fingerprint.json"],
  ]) {
    const accepted = runSourceFingerprintGuard(target, "--verify", stamp);
    assert.equal(accepted.status, 0, accepted.stderr);
  }

  const scratch = join(root, "tmp", `source-fingerprint-guard-${process.pid}.json`);
  mkdirSync(dirname(scratch), { recursive: true });
  try {
    writeFileSync(
      scratch,
      `${JSON.stringify({
        target: "pdf",
        sourceHash: "0".repeat(64),
        fileCount: 0,
        builtAt: "1970-01-01T00:00:00.000Z",
      })}\n`,
    );
    const rejected = runSourceFingerprintGuard("pdf", "--verify", scratch);
    assert.notEqual(rejected.status, 0);
    assert.match(rejected.stderr, /fingerprint mismatch/u);
  } finally {
    rmSync(scratch, { force: true });
  }
});

test("public readiness workflow pins its execution surface and runs audits", () => {
  const workflow = readFileSync(join(root, ".github/workflows/publish-wasm.yml"), "utf8");
  assert.match(workflow, /runs-on:\s*ubuntu-24\.04/u);

  const actionUses = [...workflow.matchAll(/^\s*-?\s*uses:\s*([^\s#]+).*$/gmu)].map(
    (match) => match[1],
  );
  assert.ok(actionUses.length > 0, "publish workflow must use at least one action");
  for (const action of actionUses) {
    assert.match(action, /@[\da-f]{40}$/u, `unpinned action: ${action}`);
  }

  for (const command of [
    "node scripts/export-public-source.mjs --check-only",
    "node scripts/package-audit.mjs",
    "scripts/generate-rust-license-notices.sh --check",
    "node --test tests/all.test.mjs",
    "cargo test --manifest-path pdf-core/Cargo.toml --no-default-features --features wasm",
    "cargo test --manifest-path office-core/Cargo.toml",
    "cargo metadata --manifest-path office-core/Cargo.toml --locked --no-deps --format-version 1",
    "cargo check --manifest-path office-core/Cargo.toml --locked --lib --no-default-features --features wasm",
    "node scripts/source-fingerprint.mjs pdf --verify pdf-core/wasm/pkg/build-fingerprint.json",
    "node scripts/source-fingerprint.mjs office --verify office-core/wasm/pkg/build-fingerprint.json",
  ]) {
    assert.ok(workflow.includes(command), `publish workflow is missing: ${command}`);
  }
  assert.doesNotMatch(workflow, /npm\.pkg\.github\.com|\bnpm publish\b|GITHUB_TOKEN/u);
  assert.match(workflow, /pull_request/u);
  assert.match(workflow, /workflow_dispatch/u);
});

test("public self-contained check guide matches the exported test surface", () => {
  const guide = readFileSync(join(root, "public-tests", "README.md"), "utf8");
  for (const command of [
    "node --test public-tests/*.test.mjs tests/font-cdn-tools.test.mjs tests/license-metadata.test.mjs",
    "cargo test --manifest-path pdf-core/Cargo.toml --lib --no-default-features --features wasm",
    "cargo metadata --manifest-path office-core/Cargo.toml --locked --no-deps --format-version 1",
    "cargo check --manifest-path office-core/Cargo.toml --locked --lib --no-default-features --features wasm",
  ]) {
    assert.ok(guide.includes(command), `public guide is missing: ${command}`);
  }
  assert.doesNotMatch(guide, /office-core\/public-check/u);
});

test("release gate is a side-effect-free public preparation check", () => {
  const beforeStatus = execFileSync("git", ["status", "--porcelain=v1"], { cwd: root, encoding: "utf8" });
  const beforeIndex = execFileSync("git", ["diff", "--cached", "--binary"], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
  });
  const result = spawnSync("bash", [join(root, "scripts/release.sh")], { cwd: root, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /release preparation passed for 0\.0\.22/u);
  assert.equal(execFileSync("git", ["status", "--porcelain=v1"], { cwd: root, encoding: "utf8" }), beforeStatus);
  assert.equal(execFileSync("git", ["diff", "--cached", "--binary"], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
  }), beforeIndex);

  const toolchainPath = join(root, "rust-toolchain.toml");
  assert.ok(existsSync(toolchainPath), "release builds must pin the Rust toolchain");
  const toolchain = readFileSync(toolchainPath, "utf8");
  assert.match(toolchain, /channel\s*=\s*"1\.89\.0"/u);
  assert.match(toolchain, /"wasm32-unknown-unknown"/u);
});

test("all release surfaces carry one immutable version", () => {
  const versions = new Map([
    ["pdf-core", cargoPackageVersion("pdf-core/Cargo.toml")],
    ["pdf-core-wasm", cargoPackageVersion("pdf-core/wasm/Cargo.toml")],
    ["office-core", cargoPackageVersion("office-core/Cargo.toml")],
    ["office-core-wasm", cargoPackageVersion("office-core/wasm/Cargo.toml")],
    ["PDF npm package", packageVersion("pdf-core/wasm/pkg/package.json")],
    ["Office npm package", packageVersion("office-core/wasm/pkg/package.json")],
    ["offline font npm package", packageVersion("packages/doc-yantra-fonts/package.json")],
    ["DocKosha adapter", packageVersion("packages/dockosha-provider/package.json")],
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
    "office-core/wasm/fontsource-provider.cjs",
    "office-core/wasm/fontsource-provider.d.ts",
    "office-core/wasm/fontsource-provider-web.js",
    "office-core/wasm/noto-fallback.cjs",
    "office-core/wasm/noto-fallback.d.ts",
    "office-core/wasm/font-provider-web.js",
    "office-core/wasm/font-provider-web.d.ts",
    "office-core/wasm/web-index.js",
    "office-core/wasm/web-index.d.ts",
  ]) {
    assert.match(officeInputs, new RegExp(JSON.stringify(path).replaceAll("/", "\\/")));
  }
});

test("Office CJS provider subpaths are also importable from ESM", () => {
  const packageRoot = join(root, "office-core", "wasm", "pkg");
  const script = `
    const fontsource = await import('@samarthmn/doc-yantra-office/fontsource-provider');
    const noto = await import('@samarthmn/doc-yantra-office/noto-fallback');
    if (typeof fontsource.default?.FontsourceProvider !== 'function') throw new Error('FontsourceProvider missing');
    if (typeof noto.default?.NotoFallbackProvider !== 'function') throw new Error('NotoFallbackProvider missing');
  `;
  execFileSync(process.execPath, ["--input-type=module", "--eval", script], {
    cwd: packageRoot,
    stdio: "pipe",
  });
});

test("packed Office artifact contains no font file or embedded font program", () => {
  const packageRoot = join(root, "office-core", "wasm", "pkg");
  const manifest = npmPackManifest(packageRoot, "office");
  const files = manifest.files?.map(({ path }) => path);
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

test("packed PDF core contains no font binary or embedded font program", () => {
  const packageRoot = join(root, "pdf-core", "wasm", "pkg");
  const manifest = npmPackManifest(packageRoot, "pdf");
  const files = manifest.files?.map(({ path }) => path);
  assert.ok(Array.isArray(files) && files.length > 0, "npm pack returned no PDF files");
  for (const path of files) {
    assert.doesNotMatch(path, FONT_EXTENSION, `packed font binary: ${path}`);
    const bytes = readFileSync(join(packageRoot, path));
    assert.equal(findFontProgram(bytes), -1, `embedded font program in packed file: ${path}`);
  }
});

test("offline font package owns every hash-pinned font and licence", () => {
  const packageRoot = join(root, "packages", "doc-yantra-fonts");
  const metadata = JSON.parse(readFileSync(join(packageRoot, "package.json"), "utf8"));
  assert.equal(metadata.name, "@samarthmn/doc-yantra-fonts");

  const manifest = JSON.parse(readFileSync(join(packageRoot, "fonts", "manifest.json"), "utf8"));
  assert.equal(manifest.fonts.length, 17);
  for (const font of manifest.fonts) {
    const fontPath = join(packageRoot, "fonts", font.file);
    const actualHash = createHash("sha256").update(readFileSync(fontPath)).digest("hex");
    assert.equal(actualHash, font.sha256, `${font.file}: canonical hash mismatch`);
    assert.ok(existsSync(join(packageRoot, "fonts", font.license)), `${font.file}: missing licence`);
  }
});

test("canonical distribution fonts are not duplicated in native fixture directories", () => {
  const packageRoot = join(root, "packages", "doc-yantra-fonts");
  const manifest = JSON.parse(readFileSync(join(packageRoot, "fonts", "manifest.json"), "utf8"));
  const canonicalHashes = new Map(manifest.fonts.map((font) => [font.sha256, font.file]));
  const nativeFontRoot = join(root, "office-core", "fonts");

  const pending = [nativeFontRoot];
  while (pending.length > 0) {
    const directory = pending.pop();
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) {
        pending.push(path);
        continue;
      }
      if (!FONT_EXTENSION.test(entry.name)) continue;
      const hash = createHash("sha256").update(readFileSync(path)).digest("hex");
      assert.equal(
        canonicalHashes.has(hash),
        false,
        `${path} duplicates canonical font ${canonicalHashes.get(hash)}`,
      );
    }
  }
});

test("each packed engine contains exactly one WebAssembly binary", () => {
  for (const [label, packageRoot] of [
    ["PDF", join(root, "pdf-core", "wasm", "pkg")],
    ["Office", join(root, "office-core", "wasm", "pkg")],
  ]) {
    const manifest = npmPackManifest(packageRoot, `${label.toLowerCase()}-wasm-count`);
    const wasmFiles = manifest.files.map(({ path }) => path).filter((path) => path.endsWith(".wasm"));
    assert.deepEqual(wasmFiles, [wasmFiles[0]], `${label} package contains duplicate WASM binaries`);
    assert.equal(wasmFiles.length, 1, `${label} package must contain exactly one WASM binary`);
  }
});

test("package audit enforces the release contents and byte budgets", () => {
  const audit = JSON.parse(
    execFileSync(process.execPath, [join(root, "scripts", "package-audit.mjs"), "--json"], {
      cwd: root,
      encoding: "utf8",
    }),
  );
  assert.equal(audit.version, packageVersion("pdf-core/wasm/pkg/package.json"));
  assert.deepEqual(
    audit.core.packages.map(({ name }) => name),
    [
      "@samarthmn/doc-yantra",
      "@samarthmn/doc-yantra-office",
      "@samarthmn/dockosha-provider-docyantra",
    ],
  );
  assert.ok(audit.core.packedBytes <= 6 * 1024 * 1024, audit.core);
  assert.ok(audit.core.unpackedBytes <= 15 * 1024 * 1024, audit.core);
  assert.equal(audit.fonts.name, "@samarthmn/doc-yantra-fonts");
  assert.equal(audit.fonts.fontCount, 17);
  assert.ok(audit.fonts.packedBytes <= 145 * 1024 * 1024, audit.fonts);
});
