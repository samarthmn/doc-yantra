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
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import zlib from "node:zlib";

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const PKG = join(ROOT, "pdf-core", "wasm", "pkg");

const main = require(join(PKG, "pdf_core_wasm.js"));

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
