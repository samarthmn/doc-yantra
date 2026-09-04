import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const pdf = require("../pdf-core/wasm/pkg/pdf_core_wasm.js");
const office = require("../office-core/wasm/pkg/index.cjs");

test("committed public engine packages run without external fixtures", () => {
  assert.equal(pdf.version(), "0.0.23");
  assert.ok(pdf.page_count(pdf.text_to_pdf("public candidate smoke")) >= 1);
  assert.equal(office.version(), "0.0.23");
  assert.equal(typeof office.office_profile, "function");
});
