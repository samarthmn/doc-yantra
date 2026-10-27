import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const pdf = require("../../pdf-core/wasm/pkg/pdf_core_wasm.js");
const office = require("../../office-core/wasm/pkg/index.cjs");

test("committed engine packages run without external fixtures", () => {
  const expected = require("../../pdf-core/wasm/pkg/package.json").version;
  for (const path of [
    "../../office-core/wasm/pkg/package.json",
    "../../packages/doc-yantra-fonts/package.json",
    "../../packages/dockosha-provider/package.json",
  ]) {
    assert.equal(require(path).version, expected, `${path}: release version mismatch`);
  }
  assert.equal(pdf.version(), expected);
  assert.ok(pdf.page_count(pdf.text_to_pdf("package smoke")) >= 1);
  assert.equal(office.version(), expected);
  assert.equal(typeof office.office_profile, "function");
});
