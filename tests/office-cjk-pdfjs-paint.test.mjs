// PDF.js paint regression for the office package's CJK OpenType/CFF embedding.
//
// DocKosha renders office-core PDFs with pdf.js 3.11.174. That viewer resolves
// CIDFontType0C glyphs by INVERTING the embedded /Encoding CMap per glyph
// (CMap.charCodeOf picks the LOWEST charcode mapping to a CID), so a CMap that
// maps more than one code to a painted glyph's CID silently blanks every code
// but the lowest: text still extracts (ToUnicode is keyed by code) and
// advances still apply, but showText skips the glyph — no ink, no warning.
//
// This test proves actual painted ink: it converts the pan-CJK Word fixtures
// through the BUILT office package with the shipped Noto Sans CJK OTFs
// registered (exactly DocKosha's wiring), renders pages through pdf.js
// 3.11.174 onto a canvas, and asserts the extracted CJK text items' regions
// contain non-background pixels. Structural assertions (CIDFontType0C,
// FontFile3, page counts, extraction) live in office-cjk.test.mjs and are NOT
// sufficient: all of them passed while every CJK glyph rendered blank.
//
// Hermetic: pdfjs-dist + @napi-rs/canvas come from tests/node_modules (run
// `npm install` inside tests/ once); fonts are the repo-committed OTFs; no
// network, no system fonts.

import { test } from "node:test";
import assert from "node:assert";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";
import { loadPdfjsStack, paintProbe, median } from "./helpers/pdfjs-paint.mjs";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const PKG = join(here, "../office-core/wasm/pkg");
const MOCK = join(here, "../mock-files/cjk-docx");
const LATIN_FONTS = join(here, "../office-core/fonts");
const CJK_FONTS = join(here, "../pdf-core/wasm/pkg/fonts");

// The DocKosha consumer target. Keep in lockstep with tests/package.json.
const REQUIRED_PDFJS_VERSION = "3.11.174";

// A painted CJK glyph block at scale 2 covers far more of its box than this;
// the broken emitter measures exactly 0.0 for every item.
const MIN_ITEM_INK = 0.02;
const MIN_MEDIAN_INK = 0.04;

const CASES = [
  // [fixture, page, expected substring of extracted text on that page]
  ["japanese.docx", 1, "明細書"],
  ["japanese.docx", 2, "発明の概要"],
  ["chinese.docx", 1, "说明书"],
  ["korean.docx", 1, "명세서"],
];

const stack = loadPdfjsStack();

if (stack === null && process.env.REQUIRE_PDFJS_PAINT === "1") {
  // Release gates must never pass on a skip: scripts/release.sh sets this
  // after installing tests/package-lock.json, so a missing pdf.js stack at
  // this point is an environment failure, not an optional feature.
  throw new Error(
    "REQUIRE_PDFJS_PAINT=1 but pdfjs-dist/@napi-rs/canvas failed to load from tests/node_modules",
  );
}

test("office-core wasm package: pdf.js paints CJK ink", { skip: stack === null }, async (t) => {
  if (stack === null) return; // skip surfaced above; keep node happy
  assert.equal(
    stack.pdfjsLib.version,
    REQUIRED_PDFJS_VERSION,
    "paint regression must run against DocKosha's pdf.js version",
  );

  const engine = require(PKG);
  const OPTIONS = JSON.stringify({ mode: "compatible" });
  for (const directory of ["carlito", "caladea", "liberation"]) {
    for (const file of readdirSync(join(LATIN_FONTS, directory))) {
      if (!file.endsWith(".ttf")) continue;
      engine.office_register_font(readFileSync(join(LATIN_FONTS, directory, file)));
    }
  }
  for (const file of [
    "NotoSansCJKsc-Regular.otf",
    "NotoSansCJKtc-Regular.otf",
    "NotoSansCJKjp-Regular.otf",
    "NotoSansCJKkr-Regular.otf",
  ]) {
    engine.office_register_font(readFileSync(join(CJK_FONTS, file)));
  }

  const pdfs = new Map();
  for (const fixture of new Set(CASES.map(([f]) => f))) {
    const bytes = readFileSync(join(MOCK, fixture));
    pdfs.set(fixture, engine.office_to_pdf_with_options(bytes, "docx", OPTIONS));
  }

  for (const [fixture, page, expectedText] of CASES) {
    await t.test(`${fixture} page ${page} paints its CJK text`, async () => {
      const probe = await paintProbe(stack, pdfs.get(fixture), page);
      const extracted = probe.items.map((item) => item.str).join("");
      assert.ok(
        extracted.includes(expectedText),
        `${fixture} p${page}: expected ${JSON.stringify(expectedText)} in extracted text: ${extracted.slice(0, 200)}`,
      );

      const cjkItems = probe.items.filter((item) => item.cjk);
      assert.ok(
        cjkItems.length > 0,
        `${fixture} p${page}: no CJK text items found`,
      );
      const ratios = cjkItems.map((item) => item.inkRatio);
      const failing = cjkItems.filter((item) => item.inkRatio < MIN_ITEM_INK);
      assert.ok(
        failing.length === 0,
        `${fixture} p${page}: ${failing.length}/${cjkItems.length} CJK items painted no ink ` +
          `(pdf.js extracts the text but compiles empty glyph paths): ` +
          failing
            .slice(0, 5)
            .map((item) => `${JSON.stringify(item.str.slice(0, 12))}=${item.inkRatio.toFixed(4)}`)
            .join(", "),
      );
      assert.ok(
        median(ratios) >= MIN_MEDIAN_INK,
        `${fixture} p${page}: median CJK ink ${median(ratios).toFixed(4)} below ${MIN_MEDIAN_INK}`,
      );
    });
  }

  await t.test("harness control: Latin ink is detected on korean.docx page 1", async () => {
    const probe = await paintProbe(stack, pdfs.get("korean.docx"), 1);
    const latin = probe.items.filter(
      (item) => !item.cjk && /[A-Za-z]/.test(item.str),
    );
    assert.ok(latin.length > 0, "expected Latin control items");
    assert.ok(
      median(latin.map((item) => item.inkRatio)) >= MIN_ITEM_INK,
      "Latin control items must show ink — otherwise the sampler itself is broken",
    );
  });
});

if (stack === null) {
  console.error(
    "office-cjk-pdfjs-paint: SKIPPED — pdfjs-dist/@napi-rs/canvas missing. " +
      "Run `npm install` inside tests/ to enable the PDF.js paint regression.",
  );
}
