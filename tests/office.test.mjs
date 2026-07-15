// office-core wasm package tests. Loads the BUILT package by path (mirroring a
// Node/Vercel consumer). A missing or unloadable package is a release failure.

import { test } from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";
import { transformOfficeGlue } from "../office-core/wasm/postprocess-glue.mjs";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const PKG = join(here, "../office-core/wasm/pkg");
const MOCK = join(here, "../mock-files");
const CORPUS = join(here, "../fidelity-corpus/docx");
const CARLITO = join(here, "../office-core/fonts/carlito/Carlito-Regular.ttf");

const engine = require(PKG);
const pdf = require(join(here, "../pdf-core/wasm/pkg"));
const { createMockOfficeFontProvider } = require("./fixtures/mock-office-font-provider.cjs");
const provider = createMockOfficeFontProvider();
const fontCache = new engine.MemoryFontCache();

function convertOnDemand(bytes, extension) {
  return engine.convertOnDemand(bytes, extension, {
    provider,
    memoryCache: fontCache,
    mode: "compatible",
  });
}

const DOCUMENTED_WARNING_CODES = new Set([
  "bullet_substituted",
  "floating_table_block_fallback",
  "font_fallback",
  "font_substituted",
]);

test("office-core wasm package", async (t) => {
  await t.test("version() matches the crate", () => {
    assert.match(engine.version(), /^\d+\.\d+\.\d+$/);
  });

  await t.test("provider-backed profile on docx returns supported JSON", async () => {
    const bytes = readFileSync(join(MOCK, "docx/file1.docx"));
    const { profile: p } = await convertOnDemand(bytes, "docx");
    assert.equal(p.engine, "office-core");
    assert.equal(p.format, "docx");
    assert.equal(p.supported, true);
  });

  await t.test("provider-backed conversion returns %PDF- and is deterministic", async () => {
    const bytes = readFileSync(join(MOCK, "docx/file1.docx"));
    const a = Buffer.from((await convertOnDemand(bytes, "docx")).pdf);
    const b = Buffer.from((await convertOnDemand(bytes, "docx")).pdf);
    assert.equal(a.subarray(0, 5).toString(), "%PDF-");
    assert.deepEqual(a, b);
  });

  await t.test("pptx, xlsx, and xlsm also convert through the provider", async () => {
    for (const [file, ext] of [
      ["pptx/file1.pptx", "pptx"],
      ["xlsx/file1.xlsx", "xlsx"],
      ["xlsm/file1.xlsm", "xlsm"],
    ]) {
      const out = Buffer.from((await convertOnDemand(readFileSync(join(MOCK, file)), ext)).pdf);
      assert.equal(out.subarray(0, 5).toString(), "%PDF-");
    }
  });

  await t.test("browser ESM package initializes and converts XLSM", async () => {
    const webRoot = join(here, "../office-core/wasm/pkg/web");
    const web = await import(pathToFileURL(join(webRoot, "web-index.js")));
    await web.default({
      module_or_path: readFileSync(join(webRoot, "office_core_wasm_bg.wasm")),
    });
    const input = readFileSync(join(MOCK, "xlsm/file1.xlsm"));
    assert.equal(typeof web.IndexedDbFontCache, "function");
    const out = Buffer.from((await web.convertOnDemand(input, "xlsm", {
      provider: createMockOfficeFontProvider(),
      memoryCache: new web.MemoryFontCache(),
    })).pdf);
    assert.equal(out.subarray(0, 5).toString(), "%PDF-");
  });

  await t.test("oversized input → input_too_large profile without throwing", () => {
    const big = new Uint8Array(51 * 1024 * 1024);
    const p = JSON.parse(engine.office_profile(big, "docx"));
    assert.equal(p.supported, false);
    assert.ok(p.reasons.some((r) => r.code === "input_too_large"));
    assert.throws(
      () => engine.office_to_pdf(big, "docx"),
      /invalid: input \(53477376 bytes\) exceeds the in-process limit/,
    );
  });

  await t.test("Office and font guards run before wasm input allocation in both targets", () => {
    for (const relative of [
      "../office-core/wasm/pkg/office_core_wasm.js",
      "../office-core/wasm/pkg/web/office_core_wasm.js",
    ]) {
      const source = readFileSync(join(here, relative), "utf8");
      for (const [name, args, guard] of [
        ["office_profile", "bytes, extension", "guardOfficeProfileInput(bytes, extension)"],
        ["office_profile_with_options", "bytes, extension, options_json", "guardOfficeProfileInput(bytes, extension)"],
        ["office_font_requests", "bytes, extension", "guardOfficeConversionInput(bytes, extension)"],
        ["office_to_pdf", "bytes, extension", "guardOfficeConversionInput(bytes, extension)"],
        ["office_to_pdf_with_options", "bytes, extension, options_json", "guardOfficeConversionInput(bytes, extension)"],
        ["office_register_font", "bytes, expectation_json", "guardOfficeFontInput(bytes)"],
      ]) {
        const functionStart = source.indexOf(`function ${name}(${args})`);
        const guardAt = source.indexOf(guard, functionStart);
        const copyAt = source.indexOf("passArray8ToWasm0(bytes", functionStart);
        assert.notEqual(functionStart, -1, `${relative}: ${name} missing`);
        assert.ok(guardAt > functionStart, `${relative}: ${name} guard missing`);
        assert.ok(copyAt > guardAt, `${relative}: ${name} copies before its guard`);
      }
    }
  });

  await t.test("glue postprocessor is deterministic and idempotent", () => {
    const generated = [
      "/* generated */",
      "export function office_profile(bytes, extension) {",
      "    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);",
      "}",
      "export function office_to_pdf(bytes, extension) {",
      "    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);",
      "}",
      "export function office_font_requests(bytes, extension) {",
      "    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);",
      "}",
      "export function office_profile_with_options(bytes, extension, options_json) {",
      "    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);",
      "}",
      "export function office_to_pdf_with_options(bytes, extension, options_json) {",
      "    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);",
      "}",
      "export function office_register_font(bytes, expectation_json) {",
      "    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);",
      "}",
      "function version() { return '0.0.0'; }",
      "",
    ].join("\n");
    const once = transformOfficeGlue(generated, "fixture.js");
    assert.equal(transformOfficeGlue(generated, "fixture.js"), once);
    assert.equal(transformOfficeGlue(once, "fixture.js"), once);
  });

  await t.test("register_font: garbage throws, a real TTF succeeds", () => {
    assert.throws(() => engine.register_font(new Uint8Array([1, 2, 3, 4])));
    assert.doesNotThrow(() => engine.register_font(readFileSync(CARLITO)));
  });

  await t.test("gdocs fixtures preserve the Word page counts", async () => {
    for (const [name, expectedPages] of [
      ["gdocs-anchors-toc", 10],
      ["gdocs-tables-longdoc", 44],
      ["gdocs-columns", 3],
    ]) {
      const bytes = readFileSync(join(CORPUS, name, "source.docx"));
      const converted = await convertOnDemand(bytes, "docx");
      const profile = converted.profile;
      assert.equal(profile.supported, true, `${name}: ${JSON.stringify(profile.reasons)}`);
      assert.deepEqual(profile.reasons, [], `${name}: profile reasons`);
      assert.ok(
        profile.warnings.every((warning) => DOCUMENTED_WARNING_CODES.has(warning.code)),
        `${name}: undocumented warnings ${JSON.stringify(profile.warnings)}`,
      );

      const output = converted.pdf;
      assert.equal(Buffer.from(output.subarray(0, 5)).toString(), "%PDF-", name);
      assert.equal(pdf.page_count(output), expectedPages, `${name}: Word page parity`);
    }
  });
});
