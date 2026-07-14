import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const PKG = join(ROOT, "pdf-core", "wasm", "pkg");
const main = require(PKG);
const legacyManifest = JSON.parse(
  readFileSync(join(ROOT, "fidelity-corpus", "legacy-goldens", "manifest.json"), "utf8"),
);

const OUT_OF_RANGE_AREAS = JSON.stringify([
  { pageIndex: 99, left: 10, top: 10, width: 20, height: 20 },
]);
const PARTIAL_TEXT_AREAS = JSON.stringify([
  { pageIndex: 0, left: 12, top: 12, width: 1, height: 1 },
]);

let packagedFontsRegistered = false;

function mockPdf() {
  const relativePath = "mock-files/pdf/file1.pdf";
  const bytes = readFileSync(join(ROOT, relativePath));
  const row = legacyManifest.goldens.find(({ input_path: inputPath }) => inputPath === relativePath);
  assert.ok(row, `${relativePath} must remain in frozen manifest`);
  const actual = createHash("sha256").update(bytes).digest("hex");
  assert.equal(
    actual,
    row.input_sha256,
    `frozen evidence digest mismatch for ${relativePath}: manifest expected ${row.input_sha256}, disk bytes are ${actual}`,
  );
  return new Uint8Array(bytes);
}

function registerPackagedFonts() {
  if (packagedFontsRegistered) return;

  const fontDir = join(PKG, "fonts");
  const fontNames = JSON.parse(readFileSync(join(fontDir, "manifest.json"), "utf8"))
    .fonts
    .map((font) => font.file);
  assert.ok(fontNames.length > 0, "built package must contain runtime fonts");

  for (const name of fontNames) {
    main.register_font(readFileSync(join(fontDir, name)));
  }
  packagedFontsRegistered = true;
}

function assertValidPdf(pdf) {
  assert.equal(
    Buffer.from(pdf.subarray(0, 5)).toString("latin1"),
    "%PDF-",
    "output must have PDF magic",
  );
  assert.ok(main.page_count(pdf) >= 1, "output must have at least one page");
  assert.ok(JSON.parse(main.validate_pdf(pdf)).pageCount >= 1, "output must validate");
}

function applyTextWatermark(pdf, text) {
  try {
    return main.apply_watermark(
      pdf,
      JSON.stringify({ pattern: "grid", lines: [text] }),
    );
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    assert.fail(`watermark ${JSON.stringify(text)} threw: ${message}`);
  }
}

/**
 * Hash only PDF stream bodies. This ignores trailer/xref metadata drift while
 * retaining page content, embedded-font, and ToUnicode differences. The
 * same-input assertion below proves stream determinism before comparison.
 */
function pdfStreamDigest(pdf) {
  const bytes = Buffer.from(pdf);
  const streamMarker = Buffer.from("stream");
  const endMarker = Buffer.from("endstream");
  const hash = createHash("sha256");
  let cursor = 0;
  let streamCount = 0;

  for (;;) {
    const streamAt = bytes.indexOf(streamMarker, cursor);
    if (streamAt === -1) break;

    let dataAt = streamAt + streamMarker.length;
    if (bytes[dataAt] === 0x0d) dataAt++;
    if (bytes[dataAt] === 0x0a) dataAt++;

    const endAt = bytes.indexOf(endMarker, dataAt);
    assert.notEqual(endAt, -1, "PDF stream must have an endstream marker");
    const body = bytes.subarray(dataAt, endAt);
    hash.update(`${body.byteLength}:`);
    hash.update(body);
    streamCount++;
    cursor = endAt + endMarker.length;
  }

  assert.ok(streamCount > 0, "generated PDF must contain at least one stream");
  return hash.digest("hex");
}

test(
  "FX-RED-010 out-of-range pageIndex must throw at the package boundary",
  () => {
    const input = mockPdf();
    assert.equal(main.page_count(input), 1, "fixture must remain one page");
    assert.throws(
      () => main.redact_pdf(input, OUT_OF_RANGE_AREAS),
      "pageIndex 99 must throw for a one-page PDF",
    );
  },
);

test(
  "FX-RED-011 unchanged-bytes-as-success forbidden",
  () => {
    const input = mockPdf();
    let output;

    try {
      output = main.redact_pdf(input, OUT_OF_RANGE_AREAS);
    } catch {
      return;
    }

    assert.notEqual(
      Buffer.compare(Buffer.from(output), Buffer.from(input)),
      0,
      "a non-empty successful redaction must not return unchanged bytes",
    );
  },
);

test("FX-RED-012 warnings must be surfaced", () => {
  assert.equal(
    typeof main.redact_pdf_with_report,
    "function",
    "built package must export redact_pdf_with_report",
  );

  const rawReport = main.redact_pdf_with_report(mockPdf(), PARTIAL_TEXT_AREAS);
  const report = typeof rawReport === "string" ? JSON.parse(rawReport) : rawReport;
  assert.ok(Array.isArray(report?.warnings), "redaction report must expose warnings[]");
  assert.ok(
    report.warnings.length > 0,
    "partial text-object redaction must report whole-object over-removal",
  );
  assert.deepEqual(
    Object.keys(report.warnings[0]).sort(),
    ["code", "kind", "message", "page"],
    "warnings must use the structured contract",
  );
  assert.equal(report.warnings[0].code, "over_redaction");
  assert.equal(report.warnings[0].page, 0);
  assert.equal(report.warnings[0].kind, "text");
  assert.equal(typeof report.warnings[0].message, "string");
});

test("FX-WMK-005 CJK watermark text must succeed", () => {
  registerPackagedFonts();
  const output = applyTextWatermark(mockPdf(), "机密文件");
  assertValidPdf(output);
});

test("FX-WMK-006 GREEN-GUARD Latin-1 watermark", () => {
  registerPackagedFonts();
  const input = mockPdf();
  const output = applyTextWatermark(input, "Café Zürich — René");
  assertValidPdf(output);
  assert.equal(main.page_count(output), main.page_count(input), "watermark preserves page count");
});

test("FX-UNI-001c package-level café csv", () => {
  registerPackagedFonts();
  const unicodeInput = Buffer.from("café,Zürich", "utf8");
  const foldedInput = Buffer.from("caf?,Z?rich", "utf8");
  const unicodeFirst = main.csv_to_pdf(unicodeInput);
  const unicodeSecond = main.csv_to_pdf(unicodeInput);
  const folded = main.csv_to_pdf(foldedInput);

  const unicodeDigest = pdfStreamDigest(unicodeFirst);
  assert.equal(
    unicodeDigest,
    pdfStreamDigest(unicodeSecond),
    "same-input content streams must be deterministic before differential comparison",
  );
  assert.notEqual(
    unicodeDigest,
    pdfStreamDigest(folded),
    "Unicode CSV must not collapse to the same streams as caf?,Z?rich",
  );
});

test("FX-UNI-016-astral astral emoji watermark", () => {
  registerPackagedFonts();
  const output = applyTextWatermark(mockPdf(), "🔒");
  assertValidPdf(output);
});
