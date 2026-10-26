// P6 merge/watermark package-parity fixtures. These intentionally load the
// committed Node wasm package, not Cargo output.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import zlib from "node:zlib";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", "..");
const PKG = join(ROOT, "pdf-core", "wasm", "pkg");
const FONT_PKG = join(ROOT, "packages", "doc-yantra-fonts", "fonts");
const main = createRequire(import.meta.url)(PKG);
let fontsRegistered = false;

function bytes(path) { return new Uint8Array(readFileSync(path)); }
function mock(kind, name) { return bytes(join(ROOT, "tests/fixtures/samples", kind, name)); }
function valid(pdf) {
  assert.equal(Buffer.from(pdf.subarray(0, 5)).toString("latin1"), "%PDF-");
  assert.ok(JSON.parse(main.validate_pdf(pdf)).pageCount >= 1);
}
function registerFonts() {
  if (fontsRegistered) return;
  for (const { file } of JSON.parse(readFileSync(join(FONT_PKG, "manifest.json"))).fonts) {
    main.register_font(readFileSync(join(FONT_PKG, file)));
  }
  fontsRegistered = true;
}
function streams(pdf) {
  const data = Buffer.from(pdf); let result = ""; let at = 0;
  for (;;) {
    const start = data.indexOf("stream", at); if (start < 0) return result;
    let body = start + 6; if (data[body] === 13) body++; if (data[body] === 10) body++;
    const end = data.indexOf("endstream", body); if (end < 0) return result;
    const chunk = data.subarray(body, end);
    try { result += zlib.inflateSync(chunk).toString("latin1"); } catch { result += chunk.toString("latin1"); }
    at = end + 9;
  }
}
function mediaBoxes(pdf) {
  return [...Buffer.from(pdf).toString("latin1").matchAll(/\/MediaBox\s*\[\s*([^\]]+)\]/g)]
    .map((m) => m[1].trim().replace(/\s+/g, " "));
}
function textPdf(marker) { return main.text_to_pdf(marker); }
function typedUnsupported(error) { return error instanceof Error && error.message.startsWith("unsupported:"); }

test("P6 merge preserves three-document order, page sum, and MediaBoxes", () => {
  const first = textPdf("PARITY-FIRST");
  const middle = main.csv_to_pdf(Buffer.from("marker,value\nPARITY-MIDDLE,landscape\n"));
  const last = textPdf("PARITY-LAST");
  const inputs = [first, middle, last];
  const merged = main.merge_pdfs(inputs);
  valid(merged);
  assert.equal(main.page_count(merged), inputs.reduce((n, pdf) => n + main.page_count(pdf), 0));
  const rendered = streams(merged);
  assert.ok(rendered.indexOf("PARITY-FIRST") < rendered.indexOf("PARITY-MIDDLE"));
  assert.ok(rendered.indexOf("PARITY-MIDDLE") < rendered.indexOf("PARITY-LAST"));
  const boxes = mediaBoxes(merged);
  assert.ok(boxes.some((box) => box.includes("612 792")), `expected Letter box: ${boxes}`);
  assert.ok(boxes.some((box) => box.includes("792 612")), `expected landscape box: ${boxes}`);
});

test("P6 merge retains markdown link annotations with sane rectangles", () => {
  const linked = main.markdown_to_pdf("[DocKosha link](https://example.com/parity-link)");
  const merged = main.merge_pdfs([textPdf("before link"), linked]);
  const raw = Buffer.from(merged).toString("latin1");
  assert.match(raw, /\/Subtype\s*\/Link/);
  assert.match(raw, /\/URI\s*\(https:\/\/example\.com\/parity-link\)/);
  const rect = raw.match(/\/Rect\s*\[\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)\s*\]/);
  assert.ok(rect, "link annotation has a rectangle");
  assert.ok(Number(rect[3]) > Number(rect[1]) && Number(rect[4]) > Number(rect[2]), `sane link rect: ${rect[0]}`);
});

test("P6 merge-time watermark is valid and drawn on every page", () => {
  // r1-old-runtime-spec.md line 99: retired service applied watermark to every
  // merged page; its payload had no page-selection contract.
  const inputs = [textPdf("merge one"), main.csv_to_pdf(Buffer.from("a,b\n1,2\n")), textPdf("merge three")];
  const mark = "MERGE-WATERMARK-P6";
  const out = main.merge_pdfs(inputs, JSON.stringify({ pattern: "single", lines: [mark] }));
  valid(out);
  assert.equal(main.page_count(out), inputs.reduce((n, pdf) => n + main.page_count(pdf), 0));
  assert.equal((streams(out).match(new RegExp(mark, "g")) ?? []).length, main.page_count(out));
});

test("P6 merge rejects a corrupt member atomically", () => {
  assert.throws(() => main.merge_pdfs([textPdf("good"), new Uint8Array([0, 1, 2, 3]), textPdf("never")]), /PDF 2 is not valid/);
});

test("P6 watermark option matrix preserves structural contracts", () => {
  const base = textPdf("watermark base");
  const png = mock("png", "file1.png");
  const single = main.apply_watermark(base, JSON.stringify({ lines: ["MATRIX"], opacity: 0.23, rotationDeg: 30 }));
  const grid = main.apply_watermark(base, JSON.stringify({ pattern: "grid", lines: ["MATRIX"], opacity: 0.23, rotationDeg: 30 }));
  const image = main.apply_watermark(base, JSON.stringify({ contentType: "image", pattern: "grid" }), png);
  const hybrid = main.apply_watermark(base, JSON.stringify({ contentType: "hybrid", lines: ["HYBRID"], pattern: "grid" }), png);
  for (const output of [single, grid, image, hybrid]) valid(output);
  assert.match(Buffer.from(single).toString("latin1"), /\/ca\s+0\.23/);
  assert.ok(streams(single).includes("0.86603 0.5 -0.5 0.86603"), "rotation matrix is emitted");
  assert.ok((streams(grid).match(/\) Tj/g) ?? []).length > (streams(single).match(/\) Tj/g) ?? []).length, "grid has more anchors than single");
  assert.match(Buffer.from(image).toString("latin1"), /\/Subtype\s*\/Image/);
  assert.ok(streams(hybrid).includes("HYBRID"));
});

for (const [label, probe] of [
  ["Latin-1", "Café Zürich"], ["Greek", "Εμπιστευτικό"], ["Cyrillic", "Конфиденциально"],
  ["CJK SC", "机密文件"], ["CJK TC", "機密文件"], ["CJK JP", "こんにちは"], ["CJK KR", "비밀 문서"], ["astral emoji", "🔒"],
]) test(`P6 Unicode watermark succeeds: ${label}`, () => {
  registerFonts(); const out = main.apply_watermark(textPdf("unicode base"), JSON.stringify({ lines: [probe] })); valid(out);
});

for (const [label, probe] of [["Arabic", "مرحبا"], ["Hebrew", "שלום"], ["Devanagari", "नमस्ते"], ["Thai", "สวัสดี"]]) test(`P6 complex-script watermark behavior: ${label}`, () => {
  registerFonts();
  try { valid(main.apply_watermark(textPdf("complex base"), JSON.stringify({ lines: [probe] }))); }
  catch (error) { assert.ok(typedUnsupported(error), `must fail closed with typed error: ${error}`); }
});
