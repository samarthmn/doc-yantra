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

// Everything that can change the bytes of the built package, and nothing that
// cannot (tests, target/, pkg/ itself). Over-inclusion is the safe direction:
// a false "stale" costs one rebuild; a false "fresh" ships stale artifacts.
const INPUT_ROOTS = {
  pdf: [
    "pdf-core/src",
    "pdf-core/Cargo.toml",
    "pdf-core/wasm/src",
    "pdf-core/wasm/Cargo.toml",
    "pdf-core/wasm/Cargo.lock",
    "pdf-core/wasm/build.sh",
    "pdf-core/wasm/worker.js",
    "pdf-core/wasm/worker-ops.cjs",
    "pdf-core/wasm/postprocess-glue.mjs",
    "pdf-core/wasm/fonts",
    "office-core/fonts/liberation",
    "wasm-worker",
    "text-core/src",
    "text-core/Cargo.toml",
  ],
  office: [
    "office-core/src",
    "office-core/Cargo.toml",
    "office-core/build.rs",
    "office-core/fonts",
    "office-core/wasm/src",
    "office-core/wasm/Cargo.toml",
    "office-core/wasm/Cargo.lock",
    "office-core/wasm/build.sh",
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
};

const EXCLUDED_NAMES = new Set([".DS_Store", "target", "pkg", "pkg-web", "node_modules"]);

function collectFiles(absPath, relPath, out) {
  const stat = statSync(absPath);
  if (stat.isFile()) {
    out.push(relPath);
    return;
  }
  if (!stat.isDirectory()) return;
  for (const entry of readdirSync(absPath).sort()) {
    if (EXCLUDED_NAMES.has(entry)) continue;
    collectFiles(path.join(absPath, entry), `${relPath}/${entry}`, out);
  }
}

function sourceHash(target) {
  const roots = INPUT_ROOTS[target];
  if (!roots) throw new Error(`unknown target "${target}" (expected pdf|office)`);
  const files = [];
  for (const root of roots) {
    const abs = path.join(repoRoot, root);
    if (!existsSync(abs)) throw new Error(`missing fingerprint input: ${root}`);
    collectFiles(abs, root, files);
  }
  files.sort();
  const hash = createHash("sha256");
  for (const file of files) {
    hash.update(file);
    hash.update("\0");
    hash.update(readFileSync(path.join(repoRoot, file)));
    hash.update("\0");
  }
  return { hash: hash.digest("hex"), fileCount: files.length };
}

const [target, flag, outFile] = process.argv.slice(2);
const { hash, fileCount } = sourceHash(target);
if (flag === "--verify" && outFile) {
  const stamp = JSON.parse(readFileSync(outFile, "utf8"));
  if (
    stamp.target !== target ||
    stamp.sourceHash !== hash ||
    stamp.fileCount !== fileCount
  ) {
    console.error(
      `error: source fingerprint mismatch for ${target}: ` +
        `stamp=${stamp.sourceHash ?? "missing"}/${stamp.fileCount ?? "missing"}, ` +
        `source=${hash}/${fileCount}`,
    );
    process.exit(1);
  }
  console.log(`verified fingerprint ${target}: ${hash} (${fileCount} files)`);
} else if (flag === "--out" && outFile) {
  const stamp = {
    target,
    sourceHash: hash,
    fileCount,
    builtAt: new Date().toISOString(),
  };
  writeFileSync(outFile, JSON.stringify(stamp, null, 2) + "\n");
  console.log(`fingerprint ${target}: ${hash} (${fileCount} files) -> ${outFile}`);
} else if (!flag) {
  process.stdout.write(hash + "\n");
} else {
  console.error(
    "usage: source-fingerprint.mjs <pdf|office> [--out FILE|--verify FILE]",
  );
  process.exit(2);
}
