// Pan-CJK conversion through the BUILT office package. The fontless wasm
// engine renders only host-registered faces: the test registers the OFL Latin
// faces vendored in this repo plus the Noto Sans CJK OTFs that
// `@samarthmn/doc-yantra` ships (exactly DocKosha's wiring), then Chinese /
// Japanese / Korean Word documents convert with Word-parity page counts.
// Without the CJK faces the engine must fail closed rather than emit tofu.

import { test } from "node:test";
import assert from "node:assert";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const PKG = join(here, "../office-core/wasm/pkg");
const MOCK = join(here, "../mock-files/cjk-docx");
const LATIN_FONTS = join(here, "../office-core/fonts");
const CJK_FONTS = join(here, "../packages/doc-yantra-fonts/fonts");

const engine = require(PKG);
const pdf = require(join(here, "../pdf-core/wasm/pkg"));

const OPTIONS = JSON.stringify({ mode: "compatible" });

const PAN_CJK_FACES = [
  ["NotoSansCJKsc-Regular.otf", "Noto Sans CJK SC"],
  ["NotoSansCJKtc-Regular.otf", "Noto Sans CJK TC"],
  ["NotoSansCJKjp-Regular.otf", "Noto Sans CJK JP"],
  ["NotoSansCJKkr-Regular.otf", "Noto Sans CJK KR"],
];

const CASES = [
  ["chinese.docx", 8, "Noto Sans CJK SC"],
  ["japanese.docx", 8, "Noto Sans CJK JP"],
  ["korean.docx", 10, "Noto Sans CJK KR"],
];

function registerLatinFaces() {
  for (const directory of ["carlito", "caladea", "liberation"]) {
    for (const file of readdirSync(join(LATIN_FONTS, directory))) {
      if (!file.endsWith(".ttf")) continue;
      engine.office_register_font(readFileSync(join(LATIN_FONTS, directory, file)));
    }
  }
}

test("office-core wasm package: pan-CJK docx", async (t) => {
  registerLatinFaces();

  await t.test("CJK docx fails closed while no CJK face is registered", () => {
    const bytes = readFileSync(join(MOCK, "chinese.docx"));
    assert.throws(
      () => engine.office_to_pdf_with_options(bytes, "docx", OPTIONS),
      /unsupported:/,
      "conversion without CJK coverage must stay out of envelope",
    );
  });

  await t.test("registering the shipped Noto Sans CJK OTFs reports their identity", () => {
    for (const [file, family] of PAN_CJK_FACES) {
      const identity = JSON.parse(
        engine.office_register_font(readFileSync(join(CJK_FONTS, file))),
      );
      assert.equal(identity.families?.[0] ?? identity.family, family, file);
    }
  });

  await t.test("chinese, japanese, and korean docx convert with Word page parity", () => {
    for (const [file, expectedPages, family] of CASES) {
      const bytes = readFileSync(join(MOCK, file));
      const profile = JSON.parse(engine.office_profile_with_options(bytes, "docx", OPTIONS));
      assert.equal(profile.supported, true, `${file}: ${JSON.stringify(profile.reasons)}`);

      const output = engine.office_to_pdf_with_options(bytes, "docx", OPTIONS);
      assert.equal(Buffer.from(output.subarray(0, 5)).toString(), "%PDF-", file);
      assert.equal(pdf.page_count(output), expectedPages, `${file}: Word page parity`);
      assert.ok(
        Buffer.from(output).includes(family),
        `${file}: embedded pan-CJK descriptor must identify ${family}`,
      );
    }
  });

  await t.test("conversion is deterministic with registered CJK faces", () => {
    const bytes = readFileSync(join(MOCK, "korean.docx"));
    const a = Buffer.from(engine.office_to_pdf_with_options(bytes, "docx", OPTIONS));
    const b = Buffer.from(engine.office_to_pdf_with_options(bytes, "docx", OPTIONS));
    assert.deepEqual(a, b);
  });
});
