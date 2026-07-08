// office-core wasm package tests. Loads the BUILT package by path (mirroring a
// Node/Vercel consumer). Skips the whole suite until office-core/wasm/pkg is
// built (see office-core/wasm/build.sh) — so `node --test tests/` stays green
// before the first wasm-pack build.

import { test } from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const PKG = join(here, "../office-core/wasm/pkg/office_core_wasm.js");
const MOCK = join(here, "../mock-files");
const CARLITO = join(here, "../office-core/fonts/carlito/Carlito-Regular.ttf");

let engine = null;
try {
  engine = require(PKG);
} catch {
  /* package not built yet */
}

const skip = engine ? false : "office-core/wasm/pkg not built (run office-core/wasm/build.sh)";

test("office-core wasm package", { skip }, async (t) => {
  await t.test("version() matches the crate", () => {
    assert.match(engine.version(), /^\d+\.\d+\.\d+$/);
  });

  await t.test("office_profile on docx returns supported JSON", () => {
    const bytes = readFileSync(join(MOCK, "docx/file1.docx"));
    const p = JSON.parse(engine.office_profile(bytes, "docx"));
    assert.equal(p.engine, "office-core");
    assert.equal(p.format, "docx");
    assert.equal(p.supported, true);
  });

  await t.test("office_to_pdf returns %PDF- and is deterministic", () => {
    const bytes = readFileSync(join(MOCK, "docx/file1.docx"));
    const a = Buffer.from(engine.office_to_pdf(bytes, "docx"));
    const b = Buffer.from(engine.office_to_pdf(bytes, "docx"));
    assert.equal(a.subarray(0, 5).toString(), "%PDF-");
    assert.deepEqual(a, b);
  });

  await t.test("pptx and xlsx also convert", () => {
    for (const [file, ext] of [["pptx/file1.pptx", "pptx"], ["xlsx/file1.xlsx", "xlsx"]]) {
      const out = Buffer.from(engine.office_to_pdf(readFileSync(join(MOCK, file)), ext));
      assert.equal(out.subarray(0, 5).toString(), "%PDF-");
    }
  });

  await t.test("oversized input → input_too_large profile without throwing", () => {
    const big = new Uint8Array(51 * 1024 * 1024);
    const p = JSON.parse(engine.office_profile(big, "docx"));
    assert.equal(p.supported, false);
    assert.ok(p.reasons.some((r) => r.code === "input_too_large"));
  });

  await t.test("register_font: garbage throws, a real TTF succeeds", () => {
    assert.throws(() => engine.register_font(new Uint8Array([1, 2, 3, 4])));
    assert.doesNotThrow(() => engine.register_font(readFileSync(CARLITO)));
  });
});
