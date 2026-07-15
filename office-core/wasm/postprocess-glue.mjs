#!/usr/bin/env node

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const MARKER = "DOC_YANTRA_OFFICE_PRECOPY_GUARD_V1";
const HELPERS = `
const ${MARKER} = 50 * 1024 * 1024;
const DOC_YANTRA_OFFICE_FONT_PRECOPY_LIMIT = 10 * 1024 * 1024;

function dyEnvelopeError(code, message) {
    return new Error(JSON.stringify({ dy: 1, code, message }));
}

function officeFormatForInputGuard(extension) {
    switch (extension.toLowerCase()) {
        case "docx": return "docx";
        case "pptx": return "pptx";
        case "xlsx":
        case "xlsm": return "xlsx";
        default: throw dyEnvelopeError("invalid_input", \`invalid: unsupported extension \${extension}\`);
    }
}

function guardOfficeProfileInput(bytes, extension) {
    if (bytes.byteLength <= ${MARKER}) return undefined;
    const format = officeFormatForInputGuard(extension);
    return JSON.stringify({
        engine: "office-core",
        engineVersion: version(),
        format,
        supported: false,
        reasons: [{
            code: "input_too_large",
            detail: \`\${bytes.byteLength} bytes exceeds the in-process limit\`,
        }],
        warnings: [],
        stats: {
            paragraphs: 0,
            tables: 0,
            images: 0,
            fontsUsed: [],
            fontsMissing: [],
        },
        fonts: [],
    });
}

function guardOfficeConversionInput(bytes, extension) {
    if (bytes.byteLength <= ${MARKER}) return;
    officeFormatForInputGuard(extension);
    throw dyEnvelopeError("resource_limit", \`invalid: input (\${bytes.byteLength} bytes) exceeds the in-process limit\`);
}

function guardOfficeFontInput(bytes) {
    if (bytes.byteLength <= DOC_YANTRA_OFFICE_FONT_PRECOPY_LIMIT) return;
    throw dyEnvelopeError("resource_limit", \`invalid: font input (\${bytes.byteLength} bytes) exceeds the registration limit\`);
}
`;

function replaceOnce(source, needle, replacement, label) {
  const first = source.indexOf(needle);
  if (first === -1 || source.indexOf(needle, first + needle.length) !== -1) {
    throw new Error(`${label}: expected exactly one ${JSON.stringify(needle)}`);
  }
  return source.replace(needle, replacement);
}

function countOccurrences(source, needle) {
  return source.split(needle).length - 1;
}

function assertPatched(source, label) {
  const profileCall = "guardOfficeProfileInput(bytes, extension);";
  const conversionCall = "guardOfficeConversionInput(bytes, extension);";
  const fontCall = "guardOfficeFontInput(bytes);";
  if (
    countOccurrences(source, `const ${MARKER} =`) !== 1 ||
    countOccurrences(source, profileCall) !== 2 ||
    countOccurrences(source, conversionCall) !== 3 ||
    countOccurrences(source, fontCall) !== 1
  ) {
    throw new Error(`${label}: expected exactly one pre-copy guard per Office export`);
  }

  for (const [name, signatureArgs, guard] of [
    ["office_profile", "bytes, extension", profileCall],
    ["office_profile_with_options", "bytes, extension, options_json", profileCall],
    ["office_font_requests", "bytes, extension", conversionCall],
    ["office_to_pdf", "bytes, extension", conversionCall],
    ["office_to_pdf_with_options", "bytes, extension, options_json", conversionCall],
    ["office_register_font", "bytes, expectation_json", fontCall],
  ]) {
    const signature = new RegExp(`^(?:export )?function ${name}\\(${signatureArgs}\\) \\{$`, "m");
    const match = source.match(signature);
    if (match === null) throw new Error(`${label}: missing generated ${name} export`);
    const functionStart = match.index;
    const guardAt = source.indexOf(guard, functionStart);
    const copyAt = source.indexOf("passArray8ToWasm0(bytes", functionStart);
    if (guardAt < functionStart || copyAt < guardAt) {
      throw new Error(`${label}: ${name} guard must run before the wasm input copy`);
    }
  }
}

export function transformOfficeGlue(source, label = "office wasm glue") {
  if (source.includes(`const ${MARKER} =`)) {
    assertPatched(source, label);
    return source;
  }

  const firstLineEnd = source.indexOf("\n");
  if (firstLineEnd === -1) throw new Error(`${label}: missing generated header`);
  let output = `${source.slice(0, firstLineEnd + 1)}${HELPERS}${source.slice(firstLineEnd + 1)}`;

  for (const [name, inserted] of [
    [
      "office_profile",
      "    const oversizedProfile = guardOfficeProfileInput(bytes, extension);\n    if (oversizedProfile !== undefined) return oversizedProfile;",
    ],
    ["office_to_pdf", "    guardOfficeConversionInput(bytes, extension);"],
    ["office_font_requests", "    guardOfficeConversionInput(bytes, extension);"],
    [
      "office_profile_with_options",
      "    const oversizedProfile = guardOfficeProfileInput(bytes, extension);\n    if (oversizedProfile !== undefined) return oversizedProfile;",
    ],
    ["office_to_pdf_with_options", "    guardOfficeConversionInput(bytes, extension);"],
    ["office_register_font", "    guardOfficeFontInput(bytes);"],
  ]) {
    const argumentsByName = {
      office_profile: "bytes, extension",
      office_to_pdf: "bytes, extension",
      office_font_requests: "bytes, extension",
      office_profile_with_options: "bytes, extension, options_json",
      office_to_pdf_with_options: "bytes, extension, options_json",
      office_register_font: "bytes, expectation_json",
    };
    const signature = new RegExp(
      `^(?:export )?function ${name}\\(${argumentsByName[name]}\\) \\{$`,
      "gm",
    );
    const matches = [...output.matchAll(signature)];
    if (matches.length !== 1) {
      throw new Error(`${label}: expected exactly one generated ${name} export`);
    }
    const declaration = matches[0][0];
    output = replaceOnce(output, declaration, `${declaration}\n${inserted}`, label);
  }

  assertPatched(output, label);
  return output;
}

function postprocess(path) {
  const source = readFileSync(path, "utf8");
  const output = transformOfficeGlue(source, path);
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
