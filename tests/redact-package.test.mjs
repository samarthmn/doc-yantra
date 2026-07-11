import { describe, test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { readFileSync } from "node:fs";
import zlib from "node:zlib";

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const main = require(join(ROOT, "pdf-core", "wasm", "pkg"));

function isPdf(bytes) {
  return Buffer.from(bytes.subarray(0, 5)).toString("latin1") === "%PDF-";
}

function isUnsupported(error) {
  return error instanceof Error && error.message.startsWith("unsupported:");
}

function drawnText(bytes) {
  const pdf = Buffer.from(bytes);
  const streamMarker = Buffer.from("stream");
  const endMarker = Buffer.from("endstream");
  let cursor = 0;
  let text = "";

  for (;;) {
    const streamAt = pdf.indexOf(streamMarker, cursor);
    if (streamAt === -1) break;
    let dataAt = streamAt + streamMarker.length;
    if (pdf[dataAt] === 0x0d) dataAt++;
    if (pdf[dataAt] === 0x0a) dataAt++;
    const endAt = pdf.indexOf(endMarker, dataAt);
    if (endAt === -1) break;
    const data = pdf.subarray(dataAt, endAt);
    try {
      text += zlib.inflateSync(data).toString("latin1");
    } catch {
      text += data.toString("latin1");
    }
    cursor = endAt + endMarker.length;
  }
  return text;
}

function buildPdf(objects) {
  let body = "%PDF-1.7\n";
  const offsets = [0];
  for (let index = 0; index < objects.length; index++) {
    offsets.push(Buffer.byteLength(body, "latin1"));
    body += `${index + 1} 0 obj\n${objects[index]}\nendobj\n`;
  }
  const xrefAt = Buffer.byteLength(body, "latin1");
  body += `xref\n0 ${objects.length + 1}\n`;
  body += "0000000000 65535 f \n";
  for (const offset of offsets.slice(1)) {
    body += `${String(offset).padStart(10, "0")} 00000 n \n`;
  }
  body += `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\n`;
  body += `startxref\n${xrefAt}\n%%EOF\n`;
  return new Uint8Array(Buffer.from(body, "latin1"));
}

function type3Pdf() {
  const content = "BT /F3 24 Tf 72 720 Td (aaa) Tj ET";
  const charProc = "750 0 0 0 750 750 d1 0 0 750 750 re f";
  return buildPdf([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F3 5 0 R >> >> >>",
    `<< /Length ${Buffer.byteLength(content)} >>\nstream\n${content}\nendstream`,
    "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 750 750] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /square 6 0 R >> /Encoding << /Type /Encoding /Differences [97 /square] >> /FirstChar 97 /LastChar 97 /Widths [750] >>",
    `<< /Length ${Buffer.byteLength(charProc)} >>\nstream\n${charProc}\nendstream`,
  ]);
}

function partialRedactionPdf(secret, survivor) {
  const content = [
    `BT /F1 24 Tf 72 720 Td (${secret}) Tj ET`,
    `BT /F1 24 Tf 72 100 Td (${survivor}) Tj ET`,
  ].join("\n");
  return buildPdf([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    `<< /Length ${Buffer.byteLength(content)} >>\nstream\n${content}\nendstream`,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
  ]);
}

const fullPage = JSON.stringify([
  { pageIndex: 0, left: 0, top: 0, width: 100, height: 100 },
]);

describe("redact_pdf", () => {
  test("removes target text and returns a valid, sane-sized PDF", () => {
    const secret = "WASM-REDACT-SECRET-91f";
    const input = main.text_to_pdf(secret);
    assert.ok(drawnText(input).includes(secret), "fixture contains drawn secret");

    const output = main.redact_pdf(input, fullPage);
    const info = JSON.parse(main.validate_pdf(output));

    assert.ok(isPdf(output), "has %PDF magic");
    assert.equal(info.pageCount, main.page_count(input));
    assert.ok(output.byteLength > 100, "output is not truncated");
    assert.ok(output.byteLength < input.byteLength * 10 + 50_000, "output size is bounded");
    assert.ok(!drawnText(output).includes(secret), "drawn secret was removed");
  });

  test("removes a targeted top-band object and preserves outside text", () => {
    const secret = "WASM-TOP-SECRET-42";
    const survivor = "WASM-BOTTOM-SURVIVOR-73";
    const input = partialRedactionPdf(secret, survivor);
    const topBand = JSON.stringify([
      { pageIndex: 0, left: 0, top: 0, width: 60, height: 20 },
    ]);
    const before = drawnText(input);
    assert.ok(before.includes(secret), "fixture contains targeted text");
    assert.ok(before.includes(survivor), "fixture contains outside text");

    const output = main.redact_pdf(input, topBand);
    const after = drawnText(output);

    assert.ok(isPdf(output), "has %PDF magic");
    assert.equal(JSON.parse(main.validate_pdf(output)).pageCount, 1);
    assert.ok(!after.includes(secret), "targeted text was removed");
    assert.ok(after.includes(survivor), "outside text was preserved");
  });

  test("rejects malformed areas JSON", () => {
    const input = main.text_to_pdf("bad JSON fixture");
    assert.throws(() => main.redact_pdf(input, "{not-json"));
  });

  test("returns unsupported prefix for a Type3 font", () => {
    assert.throws(() => main.redact_pdf(type3Pdf(), fullPage), isUnsupported);
  });

  test("rejects an oversized PDF before reading or copying its bytes", () => {
    const oversized = {
      byteLength: 100 * 1024 * 1024 + 1,
      get length() {
        throw new Error("wasm input copy was attempted");
      },
    };

    assert.throws(
      () => main.redact_pdf(oversized, fullPage),
      /PDF is 104857601 bytes, over the 100 MiB in-process limit/,
    );
  });

  test("guards both generated targets before copying PDF bytes into wasm", () => {
    for (const relative of ["pdf_core_wasm.js", "web/pdf_core_wasm.js"]) {
      const source = readFileSync(join(ROOT, "pdf-core", "wasm", "pkg", relative), "utf8");
      const functionAt = source.indexOf("function redact_pdf(pdf, areas_json)");
      const guardAt = source.indexOf("guardRedactPdfInput(pdf, areas_json);", functionAt);
      const copyAt = source.indexOf("passArray8ToWasm0(pdf", functionAt);

      assert.notEqual(functionAt, -1, `${relative}: redact_pdf missing`);
      assert.ok(guardAt > functionAt, `${relative}: pre-copy guard missing`);
      assert.ok(copyAt > guardAt, `${relative}: PDF copied before size guard`);
    }
  });
});
