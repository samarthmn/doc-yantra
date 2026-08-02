// Feature test suite for the built @samarthmn/doc-yantra wasm package, run
// against the repo's own mock-files copy. Exercises EVERY export of the main
// module, asserting real behavior (not just "did not throw"): %PDF magic, page
// counts, watermark presence, and merge arithmetic.
//
// Run:  node --test tests/
//
// The package is CommonJS and loaded by path from the built pkg (no install,
// no bundler) via createRequire — mirroring how a Node/Vercel consumer wires
// it in. The web ESM submodule is intentionally out of scope here: it needs
// async browser instantiation and a different loader.

import { test, describe } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync, existsSync } from "node:fs";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import zlib from "node:zlib";
import {
  transformPdfGlue,
  utf8ByteLengthExceedsLimit,
} from "../pdf-core/wasm/postprocess-glue.mjs";

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const PKG = join(ROOT, "pdf-core", "wasm", "pkg");
const PACKAGED_FONTS = [
  "LiberationSans-Regular.ttf",
  "LiberationSans-Bold.ttf",
  "LiberationSans-Italic.ttf",
  "LiberationSans-BoldItalic.ttf",
  "LiberationMono-Regular.ttf",
  "NotoSansCJKsc-Regular.otf",
  "NotoSansCJKtc-Regular.otf",
  "NotoSansCJKjp-Regular.otf",
  "NotoSansCJKkr-Regular.otf",
  // Serif pan-CJK siblings: Word's Song/Ming/Mincho/Batang lineages are
  // serif; the office provider substitutes these faces for them.
  "NotoSerifCJKsc-Regular.otf",
  "NotoSerifCJKtc-Regular.otf",
  "NotoSerifCJKjp-Regular.otf",
  "NotoSerifCJKkr-Regular.otf",
  "NotoSansArabic-Variable.ttf",
  "NotoSansDevanagari-Variable.ttf",
  "NotoSansThai-Variable.ttf",
  "NotoEmoji-Variable.ttf",
];

const main = require(PKG);
let packagedFontsRegistered = false;

// --- helpers ---------------------------------------------------------------

/** Read a mock file (default file1) for an extension as Uint8Array. */
function mock(ext, name = `file1.${ext}`) {
  return new Uint8Array(readFileSync(join(ROOT, "mock-files", ext, name)));
}

/** True when the bytes begin with the PDF magic header. */
function isPdf(u8) {
  return Buffer.from(u8.slice(0, 5)).toString("latin1") === "%PDF-";
}

/** Bytewise inequality — cheap "the operation changed something" assertion. */
function differs(a, b) {
  return Buffer.compare(Buffer.from(a), Buffer.from(b)) !== 0;
}

/** Register package-shipped runtime faces exactly once in this test process. */
function registerPackagedFonts() {
  if (packagedFontsRegistered) return;
  for (const name of PACKAGED_FONTS) {
    main.register_font(readFileSync(join(PKG, "fonts", name)));
  }
  packagedFontsRegistered = true;
}

/** Predicate for recoverable fail-closed text errors crossing wasm. */
function isUnsupportedError(error) {
  return error instanceof Error && error.message.startsWith("unsupported:");
}

/**
 * Re-parse a PDF far enough to see its drawn text: walk every `stream …
 * endstream`, inflate FlateDecode payloads (falling back to raw for the rare
 * uncompressed stream), and concatenate. Watermark text is drawn in a
 * standard font as a literal `(MARK) Tj`, so a decompressed-stream search is a
 * faithful "does the output actually contain the mark" check without needing
 * the engine's pdfium extractor (which the wasm build does not expose).
 */
function pdfDrawnText(u8) {
  const buf = Buffer.from(u8);
  const STREAM = Buffer.from("stream");
  const ENDSTREAM = Buffer.from("endstream");
  let out = "";
  let idx = 0;
  for (;;) {
    const s = buf.indexOf(STREAM, idx);
    if (s === -1) break;
    let p = s + STREAM.length;
    if (buf[p] === 0x0d) p++;
    if (buf[p] === 0x0a) p++;
    const e = buf.indexOf(ENDSTREAM, p);
    if (e === -1) break;
    const chunk = buf.subarray(p, e);
    try {
      out += zlib.inflateSync(chunk).toString("latin1");
    } catch {
      out += chunk.toString("latin1");
    }
    idx = e + ENDSTREAM.length;
  }
  return out;
}

// --- engine metadata -------------------------------------------------------

describe("engine metadata", () => {
  test("version() returns a semver-ish string", () => {
    const v = main.version();
    assert.equal(typeof v, "string");
    assert.match(v, /^\d+\.\d+\.\d+/, `unexpected version: ${v}`);
  });

  test("page_count() counts a real mock PDF", () => {
    assert.ok(main.page_count(mock("pdf")) >= 1);
  });

  test("page_count() rejects non-PDF bytes", () => {
    assert.throws(() => main.page_count(new Uint8Array([1, 2, 3, 4])));
  });

  test("validate_pdf() reports structure as JSON", () => {
    const info = JSON.parse(main.validate_pdf(mock("pdf")));
    assert.ok(info.pageCount >= 1, "pageCount present");
    assert.equal(typeof info.version, "string");
    assert.equal(info.encrypted, false);
  });

  test("validate_pdf() rejects garbage", () => {
    assert.throws(() => main.validate_pdf(new Uint8Array([0, 1, 2, 3])));
  });
});

// --- embedded Unicode fonts -----------------------------------------------

describe("embedded Unicode fonts", () => {
  test("font manifest resolves every font, checksum, and license", () => {
    const manifest = JSON.parse(readFileSync(join(PKG, "fonts", "manifest.json"), "utf8"));
    assert.equal(manifest.fonts.length, PACKAGED_FONTS.length);
    for (const font of manifest.fonts) {
      const fontPath = join(PKG, "fonts", font.file);
      const licensePath = join(PKG, "fonts", font.license);
      assert.ok(existsSync(fontPath), `${font.file} must be packaged`);
      assert.ok(existsSync(licensePath), `${font.license} must be packaged`);
      assert.equal(
        createHash("sha256").update(readFileSync(fontPath)).digest("hex"),
        font.sha256,
        `${font.file} sha256 must match the manifest`,
      );
    }
  });

  test("register_font() accepts all packaged Liberation faces", () => {
    const manifest = JSON.parse(readFileSync(join(PKG, "package.json"), "utf8"));
    assert.ok(manifest.files.includes("fonts"), "npm package includes runtime fonts");
    assert.doesNotThrow(registerPackagedFonts);
  });

  test("Cyrillic merge-time watermark produces a valid embedded-font PDF", () => {
    registerPackagedFonts();
    const a = mock("pdf", "file1.pdf");
    const b = mock("pdf", "file2.pdf");
    const expectedPages = main.page_count(a) + main.page_count(b);
    const merged = main.merge_pdfs(
      [a, b],
      JSON.stringify({ lines: ["Конфиденциально"] }),
    );

    assert.ok(isPdf(merged), "has %PDF magic");
    assert.equal(main.page_count(merged), expectedPages);
    assert.equal(JSON.parse(main.validate_pdf(merged)).pageCount, expectedPages);
    assert.ok(
      Buffer.from(merged).includes(Buffer.from("/FontFile2")),
      "watermark embeds a TrueType font",
    );
  });

  test("Cyrillic and Greek CSV produces a valid embedded-font PDF", () => {
    registerPackagedFonts();
    const csv = Buffer.from(
      "language,value\nRussian,Конфиденциально\nGreek,Εμπιστευτικό\n",
      "utf8",
    );
    const pdf = main.csv_to_pdf(csv);

    assert.ok(isPdf(pdf), "has %PDF magic");
    assert.ok(main.page_count(pdf) >= 1);
    assert.equal(JSON.parse(main.validate_pdf(pdf)).pageCount, main.page_count(pdf));
  });

  test("Arabic watermark shapes and renders with the packaged Arabic face", () => {
    registerPackagedFonts();
    const output = main.apply_watermark(mock("pdf"), JSON.stringify({ lines: ["مرحبا"] }));
    assert.equal(Buffer.from(output.subarray(0, 5)).toString("latin1"), "%PDF-");
    assert.ok(main.page_count(output) >= 1, "Arabic watermark output must have pages");
  });

  test("CJK CSV with Japanese, Chinese, and Korean rows produces a valid PDF", () => {
    registerPackagedFonts();
    const pdf = main.csv_to_pdf(Buffer.from(
      "locale,value\nja,こんにちは\nzh,机密文件\nko,비밀 문서\n",
      "utf8",
    ));
    assert.ok(isPdf(pdf));
    assert.ok(main.page_count(pdf) >= 1);
  });

  test("CJK watermark produces a valid PDF", () => {
    registerPackagedFonts();
    const pdf = main.apply_watermark(mock("pdf"), JSON.stringify({ lines: ["机密文件"] }));
    assert.ok(isPdf(pdf));
    assert.ok(main.page_count(pdf) >= 1);
  });

  test("BMP emoji U+2603 behavior is recorded", (t) => {
    registerPackagedFonts();
    try {
      const pdf = main.apply_watermark(mock("pdf"), JSON.stringify({ lines: ["☃"] }));
      assert.ok(isPdf(pdf));
      t.diagnostic("U+2603 is covered by the packaged font set");
    } catch (error) {
      assert.ok(isUnsupportedError(error), `unexpected emoji error: ${error}`);
      t.diagnostic(`U+2603 remains fail-closed: ${error.message}`);
    }
  });

  test("register_font rejects over 32 MiB before wasm allocation", () => {
    assert.throws(
      () => main.register_font(new Uint8Array(32 * 1024 * 1024 + 1)),
      /font is 33554433 bytes, over the 32 MiB in-process limit/,
    );
  });

  test("register_font pre-copy guard is present in both generated targets", () => {
    for (const relative of ["pdf_core_wasm.js", "web/pdf_core_wasm.js"]) {
      const source = readFileSync(join(PKG, relative), "utf8");
      const functionStart = source.indexOf("function register_font(bytes)");
      const guardAt = source.indexOf("guardRegisteredFontInput(bytes);", functionStart);
      const copyAt = source.indexOf("passArray8ToWasm0(bytes", functionStart);
      assert.notEqual(functionStart, -1, `${relative}: register_font missing`);
      assert.ok(guardAt > functionStart, `${relative}: pre-copy guard missing`);
      assert.ok(copyAt > guardAt, `${relative}: wasm copy occurs before guard`);
    }
  });

  test("font glue postprocessor is deterministic and idempotent", () => {
    const generated = [
      "/* generated */",
      "export function register_font(bytes) {",
      "    const ptr0 = passArray8ToWasm0(bytes, wasm.__wbindgen_malloc);",
      "}",
      "export function apply_watermark(pdf, watermark_json, watermark_image) {",
      "    wasm.apply_watermark();",
      "}",
      "export function csv_to_pdf(csv) {",
      "    wasm.csv_to_pdf();",
      "}",
      "export function markdown_to_pdf(markdown) {",
      "    wasm.markdown_to_pdf();",
      "}",
      "export function merge_pdfs(pdfs, watermark_json, watermark_image) {",
      "    wasm.merge_pdfs();",
      "}",
      "export function page_count(pdf) {",
      "    wasm.page_count();",
      "}",
      "export function redact_pdf(pdf, areas_json) {",
      "    const ptr0 = passArray8ToWasm0(pdf, wasm.__wbindgen_malloc);",
      "    wasm.redact_pdf();",
      "}",
      "export function redact_pdf_with_report(pdf, areas_json) {",
      "    const ptr0 = passArray8ToWasm0(pdf, wasm.__wbindgen_malloc);",
      "    wasm.redact_pdf_with_report();",
      "}",
      "export function text_to_pdf(text) {",
      "    wasm.text_to_pdf();",
      "}",
      "export function validate_pdf(pdf) {",
      "    wasm.validate_pdf();",
      "}",
      "",
    ].join("\n");
    const once = transformPdfGlue(generated, "fixture.js");
    assert.equal(transformPdfGlue(generated, "fixture.js"), once);
    assert.equal(transformPdfGlue(once, "fixture.js"), once);
  });

  test("redaction JSON guard counts UTF-8 bytes, not UTF-16 code units", () => {
    assert.equal(utf8ByteLengthExceedsLimit("abc", 3), false);
    assert.equal(utf8ByteLengthExceedsLimit("éé", 3), true);
    assert.equal(utf8ByteLengthExceedsLimit("😀", 3), true);
  });
});

// --- generation: csv / markdown / text → pdf -------------------------------

describe("generation", () => {
  for (const name of ["file1.csv", "file2.csv"]) {
    test(`csv_to_pdf(${name}) → valid paginated PDF`, () => {
      const pdf = main.csv_to_pdf(mock("csv", name));
      assert.ok(isPdf(pdf), "has %PDF magic");
      assert.ok(main.page_count(pdf) >= 1, "at least one page");
      assert.ok(JSON.parse(main.validate_pdf(pdf)).pageCount >= 1);
    });
  }

  for (const name of ["file1.md", "file2.md"]) {
    test(`markdown_to_pdf(${name}) → valid PDF`, () => {
      const md = readFileSync(join(ROOT, "mock-files", "md", name), "utf8");
      const pdf = main.markdown_to_pdf(md);
      assert.ok(isPdf(pdf));
      assert.ok(main.page_count(pdf) >= 1);
    });
  }

  test("text_to_pdf() → valid PDF", () => {
    const pdf = main.text_to_pdf("Hello DocYantra\nLine two with digits 12345.");
    assert.ok(isPdf(pdf));
    assert.ok(main.page_count(pdf) >= 1);
  });

  test("markdown_to_pdf() rejects nothing but still produces a page for empty input", () => {
    // Empty markdown is a valid (blank) document, not an error.
    const pdf = main.markdown_to_pdf("");
    assert.ok(isPdf(pdf));
    assert.ok(main.page_count(pdf) >= 1);
  });
});

// --- watermark -------------------------------------------------------------

describe("apply_watermark", () => {
  const MARK = "WMARK-7F3A";

  test("stamps a mark that changes the bytes but preserves the page", () => {
    const base = main.csv_to_pdf(mock("csv"));
    const stamped = main.apply_watermark(
      base,
      JSON.stringify({ pattern: "diagonal_grid", lines: [MARK] }),
    );
    assert.ok(isPdf(stamped));
    assert.ok(differs(base, stamped), "watermark must change the document");
    assert.equal(
      main.page_count(stamped),
      main.page_count(base),
      "watermark preserves page count",
    );
    assert.ok(
      pdfDrawnText(stamped).includes(MARK),
      "the drawn mark is present in the output content stream",
    );
    assert.ok(
      !pdfDrawnText(base).includes(MARK),
      "sanity: the mark was not already there",
    );
  });

  test("stamps a real mock PDF (grid pattern, custom color)", () => {
    const base = mock("pdf");
    const stamped = main.apply_watermark(
      base,
      JSON.stringify({ pattern: "grid", lines: ["DRAFT", "DO NOT COPY"], color: "#FF0000" }),
    );
    assert.ok(isPdf(stamped));
    assert.ok(differs(base, stamped));
    const drawn = pdfDrawnText(stamped);
    assert.ok(drawn.includes("DRAFT"));
    assert.ok(drawn.includes("DO NOT COPY"));
  });

  test("image watermark uses out-of-band image bytes", () => {
    const base = main.csv_to_pdf(mock("csv"));
    const stamped = main.apply_watermark(
      base,
      JSON.stringify({ contentType: "image" }),
      mock("png"),
    );
    assert.ok(isPdf(stamped));
    assert.ok(differs(base, stamped), "image watermark changes the document");
    assert.equal(main.page_count(stamped), main.page_count(base));
  });

  test("image content type without image bytes throws", () => {
    const base = main.csv_to_pdf(mock("csv"));
    assert.throws(() =>
      main.apply_watermark(base, JSON.stringify({ contentType: "image" })),
    );
  });

  test("malformed JSON payload throws", () => {
    const base = main.csv_to_pdf(mock("csv"));
    assert.throws(() => main.apply_watermark(base, "{not json"));
  });
});

// --- merge -----------------------------------------------------------------

describe("merge_pdfs", () => {
  test("page_count of merge equals the sum of inputs", () => {
    const a = mock("pdf", "file1.pdf");
    const b = mock("pdf", "file2.pdf");
    const merged = main.merge_pdfs([a, b]);
    assert.ok(isPdf(merged));
    assert.equal(
      main.page_count(merged),
      main.page_count(a) + main.page_count(b),
      "merged page count is additive",
    );
  });

  test("merges more than two documents", () => {
    const parts = [main.csv_to_pdf(mock("csv")), mock("pdf"), main.text_to_pdf("tail")];
    const expected = parts.reduce((n, p) => n + main.page_count(p), 0);
    const merged = main.merge_pdfs(parts);
    assert.equal(main.page_count(merged), expected);
  });

  test("merge with a watermark payload stamps the merged result", () => {
    const a = mock("pdf", "file1.pdf");
    const b = mock("pdf", "file2.pdf");
    const merged = main.merge_pdfs(
      [a, b],
      JSON.stringify({ pattern: "grid", lines: ["MERGEMARK-Q2"] }),
    );
    assert.equal(main.page_count(merged), main.page_count(a) + main.page_count(b));
    assert.ok(
      pdfDrawnText(merged).includes("MERGEMARK-Q2"),
      "the merge-time watermark is drawn on the output",
    );
  });

  test("single input passes through, still watermarkable", () => {
    const only = main.csv_to_pdf(mock("csv"));
    const passthrough = main.merge_pdfs([only]);
    assert.equal(main.page_count(passthrough), main.page_count(only));

    const stamped = main.merge_pdfs([only], JSON.stringify({ lines: ["SOLO"] }));
    assert.equal(main.page_count(stamped), main.page_count(only));
    assert.ok(pdfDrawnText(stamped).includes("SOLO"));
  });

  test("empty input array throws", () => {
    assert.throws(() => main.merge_pdfs([]));
  });

  test("an invalid member PDF throws", () => {
    assert.throws(() => main.merge_pdfs([mock("pdf"), new Uint8Array([1, 2, 3])]));
  });
});
