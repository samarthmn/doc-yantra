#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const MARKER = "DOC_YANTRA_FONT_PRECOPY_GUARD_V1";
const GUARD_CALL = "guardRegisteredFontInput(bytes);";
const PDF_MARKER = "DOC_YANTRA_PDF_PRECOPY_GUARD_V1";
const PDF_GUARD_CALL = "guardRedactPdfInput(pdf, areas_json);";

export function utf8ByteLengthExceedsLimit(value, limit) {
  if (value.length > limit) return true;
  let bytes = 0;
  for (let index = 0; index < value.length; index++) {
    const code = value.charCodeAt(index);
    if (code < 0x80) {
      bytes += 1;
    } else if (code < 0x800) {
      bytes += 2;
    } else if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (next >= 0xdc00 && next <= 0xdfff) {
        bytes += 4;
        index++;
      } else {
        bytes += 3;
      }
    } else {
      bytes += 3;
    }
    if (bytes > limit) return true;
  }
  return false;
}

const HELPERS = `
const ${MARKER} = 32 * 1024 * 1024;
const ${PDF_MARKER} = 100 * 1024 * 1024;

${utf8ByteLengthExceedsLimit.toString()}

function guardRegisteredFontInput(bytes) {
    if (bytes.byteLength <= ${MARKER}) return;
    throw new Error(\`font is \${bytes.byteLength} bytes, over the 32 MiB in-process limit\`);
}

function guardRedactPdfInput(pdf, areas_json) {
    if (pdf.byteLength > ${PDF_MARKER}) {
        throw new Error(\`PDF is \${pdf.byteLength} bytes, over the 100 MiB in-process limit\`);
    }
    if (utf8ByteLengthExceedsLimit(areas_json, ${PDF_MARKER})) {
        throw new Error("redaction JSON is over the 100 MiB in-process limit");
    }
}
`;

function countOccurrences(source, needle) {
  return source.split(needle).length - 1;
}

function assertPatched(source, label) {
  if (
    countOccurrences(source, `const ${MARKER} =`) !== 1 ||
    countOccurrences(source, GUARD_CALL) !== 1 ||
    countOccurrences(source, `const ${PDF_MARKER} =`) !== 1 ||
    countOccurrences(source, PDF_GUARD_CALL) !== 1 ||
    countOccurrences(source, "function utf8ByteLengthExceedsLimit(value, limit)") !== 1
  ) {
    throw new Error(`${label}: expected exactly one font and PDF pre-copy guard`);
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


  const redactSignature = /^(?:export )?function redact_pdf\(pdf, areas_json\) \{$/m;
  const redactMatch = source.match(redactSignature);
  if (redactMatch === null) {
    throw new Error(`${label}: missing generated redact_pdf export`);
  }
  const redactStart = redactMatch.index;
  const redactGuardAt = source.indexOf(PDF_GUARD_CALL, redactStart);
  const redactCopyAt = source.indexOf("passArray8ToWasm0(pdf", redactStart);
  if (redactGuardAt < redactStart || redactCopyAt < redactGuardAt) {
    throw new Error(`${label}: redact_pdf guard must run before the wasm input copy`);
  }
}

export function transformPdfGlue(source, label = "pdf wasm glue") {
  if (source.includes(`const ${MARKER} =`) || source.includes(`const ${PDF_MARKER} =`)) {
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

  const redactSignature = /^(?:export )?function redact_pdf\(pdf, areas_json\) \{$/gm;
  const redactMatches = [...output.matchAll(redactSignature)];
  if (redactMatches.length !== 1) {
    throw new Error(`${label}: expected exactly one generated redact_pdf export`);
  }
  const redactDeclaration = redactMatches[0][0];
  output = output.replace(redactDeclaration, `${redactDeclaration}\n    ${PDF_GUARD_CALL}`);

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
