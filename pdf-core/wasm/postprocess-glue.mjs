#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const MARKER = "DOC_YANTRA_FONT_PRECOPY_GUARD_V1";
const GUARD_CALL = "guardRegisteredFontInput(bytes);";
const HELPERS = `
const ${MARKER} = 32 * 1024 * 1024;

function guardRegisteredFontInput(bytes) {
    if (bytes.byteLength <= ${MARKER}) return;
    throw new Error(\`font is \${bytes.byteLength} bytes, over the 32 MiB in-process limit\`);
}
`;

function countOccurrences(source, needle) {
  return source.split(needle).length - 1;
}

function assertPatched(source, label) {
  if (
    countOccurrences(source, `const ${MARKER} =`) !== 1 ||
    countOccurrences(source, GUARD_CALL) !== 1
  ) {
    throw new Error(`${label}: expected exactly one font pre-copy guard`);
  }

  const signature = /^(?:export )?function register_font\(bytes\) \{$/m;
  const match = source.match(signature);
  if (match === null) {
    throw new Error(`${label}: missing generated register_font export`);
  }
  const functionStart = match.index;
  const guardAt = source.indexOf(GUARD_CALL, functionStart);
  const copyAt = source.indexOf("passArray8ToWasm0(bytes", functionStart);
  if (guardAt < functionStart || copyAt < guardAt) {
    throw new Error(`${label}: register_font guard must run before the wasm input copy`);
  }
}

export function transformPdfGlue(source, label = "pdf wasm glue") {
  if (source.includes(`const ${MARKER} =`)) {
    assertPatched(source, label);
    return source;
  }

  const firstLineEnd = source.indexOf("\n");
  if (firstLineEnd === -1) {
    throw new Error(`${label}: missing generated header`);
  }
  let output = `${source.slice(0, firstLineEnd + 1)}${HELPERS}${source.slice(firstLineEnd + 1)}`;

  const signature = /^(?:export )?function register_font\(bytes\) \{$/gm;
  const matches = [...output.matchAll(signature)];
  if (matches.length !== 1) {
    throw new Error(`${label}: expected exactly one generated register_font export`);
  }
  const declaration = matches[0][0];
  output = output.replace(declaration, `${declaration}\n    ${GUARD_CALL}`);

  assertPatched(output, label);
  return output;
}

function postprocess(path) {
  const source = readFileSync(path, "utf8");
  const output = transformPdfGlue(source, path);
  writeFileSync(path, output);
  console.log(`guarded ${path}`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const paths = process.argv.slice(2);
  if (paths.length === 0) {
    throw new Error("usage: postprocess-glue.mjs <generated-js> [...]");
  }
  for (const path of paths) postprocess(path);
}
