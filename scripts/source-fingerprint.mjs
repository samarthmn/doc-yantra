#!/usr/bin/env node
// Deterministic fingerprint of the source inputs that produce each wasm package.
// build.sh stamps the result into pkg/build-fingerprint.json; consumers compare
// that stamp against a fresh run of this script to detect a stale pkg/ build
// (doc-kosha tests/unit/package-freshness.test.ts).
//
// Usage:
//   node scripts/source-fingerprint.mjs <pdf|office>              print the hash
//   node scripts/source-fingerprint.mjs <pdf|office> --out FILE   write the JSON stamp
//   node scripts/source-fingerprint.mjs <pdf|office> --verify FILE
//                                                            verify a saved stamp
import { createHash } from "node:crypto";
import { readFileSync, readdirSync, statSync, writeFileSync, existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const ARTIFACT_ROOTS = {
  pdf: "pdf-core/wasm/pkg",
  office: "office-core/wasm/pkg",
  adapter: "packages/dockosha-provider/dist",
};

// Everything that can change the bytes of the built package, and nothing that
// cannot (tests, target/, pkg/ itself). Over-inclusion is the safe direction:
// a false "stale" costs one rebuild; a false "fresh" ships stale artifacts.
const INPUT_ROOTS = {
  pdf: [
    "rust-toolchain.toml",
    "LICENSE",
    "COMMERCIAL-LICENSING.md",
    "THIRD_PARTY_NOTICES.md",
    "legal/rust",
    "scripts/generate-rust-license-notices.sh",
    "pdf-core/assets/cmaps/LICENSE-adobe-cmap-resources.md",
    "pdf-core/README.md",
    "pdf-core/src",
    "pdf-core/Cargo.toml",
    "pdf-core/wasm/src",
    "pdf-core/wasm/Cargo.toml",
    "pdf-core/wasm/Cargo.lock",
    "pdf-core/wasm/build.sh",
    "pdf-core/wasm/THIRD_PARTY_RUST_LICENSES.txt",
    "pdf-core/wasm/worker.js",
    "pdf-core/wasm/worker-ops.cjs",
    "pdf-core/wasm/font-provider.cjs",
    "pdf-core/wasm/font-provider.d.ts",
    "pdf-core/wasm/postprocess-glue.mjs",
    "packages/doc-yantra-fonts/fonts/manifest.json",
    "wasm-worker",
    "text-core/src",
    "text-core/Cargo.toml",
  ],
  office: [
    "rust-toolchain.toml",
    "LICENSE",
    "COMMERCIAL-LICENSING.md",
    "THIRD_PARTY_NOTICES.md",
    "legal/rust",
    "scripts/generate-rust-license-notices.sh",
    "office-core/LICENSE",
    "office-core/README.md",
    "office-core/src",
    "office-core/Cargo.toml",
    "office-core/build.rs",
    "office-core/fonts",
    "office-core/wasm/src",
    "office-core/wasm/Cargo.toml",
    "office-core/wasm/Cargo.lock",
    "office-core/wasm/build.sh",
    "office-core/wasm/THIRD_PARTY_RUST_LICENSES.txt",
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
    "office-core/wasm/worker.js",
    "office-core/wasm/worker.d.ts",
    "office-core/wasm/worker-ops.cjs",
    "office-core/wasm/postprocess-glue.mjs",
    "wasm-worker",
    "text-core/src",
    "text-core/Cargo.toml",
  ],
  adapter: [
    "LICENSE",
    "COMMERCIAL-LICENSING.md",
    "THIRD_PARTY_NOTICES.md",
    "packages/dockosha-provider/LICENSE",
    "packages/dockosha-provider/COMMERCIAL-LICENSING.md",
    "packages/dockosha-provider/README.md",
    "packages/dockosha-provider/THIRD_PARTY_NOTICES.md",
    "packages/dockosha-provider/src",
    "packages/dockosha-provider/package.json",
    "packages/dockosha-provider/pnpm-lock.yaml",
    "packages/dockosha-provider/tsconfig.json",
  ],
};

const EXCLUDED_NAMES = new Set([".DS_Store", "target", "pkg", "pkg-web", "node_modules"]);
const SOURCE_ONLY_FILES = new Set(["office-core/src/testkit.rs"]);

function collectFiles(absPath, relPath, out, excluded = new Set()) {
  const stat = statSync(absPath);
  if (stat.isFile()) {
    if (!excluded.has(relPath)) out.push(relPath);
    return;
  }
  if (!stat.isDirectory()) return;
  for (const entry of readdirSync(absPath).sort()) {
    if (EXCLUDED_NAMES.has(entry)) continue;
    collectFiles(path.join(absPath, entry), `${relPath}/${entry}`, out, excluded);
  }
}

export function sourceHash(target, baseRoot = repoRoot) {
  const roots = INPUT_ROOTS[target];
  if (!roots) throw new Error(`unknown target "${target}" (expected pdf|office|adapter)`);
  const files = [];
  for (const root of roots) {
    const abs = path.join(baseRoot, root);
    if (!existsSync(abs)) throw new Error(`missing fingerprint input: ${root}`);
    collectFiles(abs, root, files, SOURCE_ONLY_FILES);
  }
  files.sort();
  const hash = createHash("sha256");
  for (const file of files) {
    hash.update(file);
    hash.update("\0");
    hash.update(readFileSync(path.join(baseRoot, file)));
    hash.update("\0");
  }
  return { hash: hash.digest("hex"), fileCount: files.length };
}

export function artifactFingerprint(target, baseRoot = repoRoot) {
  const root = ARTIFACT_ROOTS[target];
  if (!root) throw new Error(`unknown artifact target "${target}"`);
  const files = [];
  const excluded = target === "adapter" ? new Set() : new Set([`${root}/build-fingerprint.json`]);
  collectFiles(path.join(baseRoot, root), root, files, excluded);
  files.sort();
  const artifacts = files.map((file) => {
    const bytes = readFileSync(path.join(baseRoot, file));
    return {
      path: file.slice(root.length + 1),
      sha256: createHash("sha256").update(bytes).digest("hex"),
      byteLength: bytes.byteLength,
    };
  });
  const hash = createHash("sha256");
  for (const file of files) {
    hash.update(file);
    hash.update("\0");
    hash.update(readFileSync(path.join(baseRoot, file)));
    hash.update("\0");
  }
  return { hash: hash.digest("hex"), fileCount: files.length, artifacts };
}

export function verifyFingerprint(target, stampPath, baseRoot = repoRoot) {
  const stamp = JSON.parse(readFileSync(stampPath, "utf8"));
  const source = sourceHash(target, baseRoot);
  const artifact = artifactFingerprint(target, baseRoot);
  if (
    stamp.target !== target || stamp.sourceHash !== source.hash || stamp.fileCount !== source.fileCount ||
    stamp.artifactHash !== artifact.hash || stamp.artifactFileCount !== artifact.fileCount ||
    JSON.stringify(stamp.artifacts) !== JSON.stringify(artifact.artifacts)
  ) {
    throw new Error(`fingerprint mismatch for ${target}: source or generated artifact bytes differ`);
  }
  return { source, artifact };
}

export function writeFingerprint(target, stampPath, baseRoot = repoRoot) {
  const source = sourceHash(target, baseRoot);
  const artifact = artifactFingerprint(target, baseRoot);
  const stamp = {
    target,
    sourceHash: source.hash,
    fileCount: source.fileCount,
    artifactHash: artifact.hash,
    artifactFileCount: artifact.fileCount,
    artifacts: artifact.artifacts,
  };
  writeFileSync(stampPath, JSON.stringify(stamp, null, 2) + "\n");
  return stamp;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  try {
    const [target, flag, outFile] = process.argv.slice(2);
    if (flag === "--verify" && outFile) {
      const result = verifyFingerprint(target, path.resolve(outFile));
      console.log(`verified fingerprint ${target}: ${result.source.hash} (${result.source.fileCount} files; ${result.artifact.fileCount} artifacts)`);
    } else if (flag === "--out" && outFile) {
      const stamp = writeFingerprint(target, path.resolve(outFile));
      console.log(`fingerprint ${target}: ${stamp.sourceHash} (${stamp.fileCount} files; ${stamp.artifactFileCount} artifacts) -> ${outFile}`);
    } else if (!flag) {
      process.stdout.write(sourceHash(target).hash + "\n");
    } else {
      throw new Error("usage: source-fingerprint.mjs <pdf|office|adapter> [--out FILE|--verify FILE]");
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}
