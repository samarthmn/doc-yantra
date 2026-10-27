import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const pdf = require(join(root, "pdf-core/wasm/pkg"));
const office = require(join(root, "office-core/wasm/pkg"));
const basePdf = readFileSync(join(root, "tests/fixtures/samples/pdf/file1.pdf"));

function assertLimit(fn) {
  assert.throws(fn, (error) => error instanceof Error && /limit/i.test(error.message));
}

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function storedZip(entries) {
  const local = [];
  const central = [];
  let offset = 0;
  for (const [name, content] of entries) {
    const filename = Buffer.from(name);
    const data = Buffer.from(content);
    const checksum = crc32(data);
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50, 0);
    header.writeUInt16LE(20, 4);
    header.writeUInt32LE(checksum, 14);
    header.writeUInt32LE(data.length, 18);
    header.writeUInt32LE(data.length, 22);
    header.writeUInt16LE(filename.length, 26);
    local.push(header, filename, data);
    const record = Buffer.alloc(46);
    record.writeUInt32LE(0x02014b50, 0);
    record.writeUInt16LE(20, 4);
    record.writeUInt16LE(20, 6);
    record.writeUInt32LE(checksum, 16);
    record.writeUInt32LE(data.length, 20);
    record.writeUInt32LE(data.length, 24);
    record.writeUInt16LE(filename.length, 28);
    record.writeUInt32LE(offset, 42);
    central.push(record, filename);
    offset += header.length + filename.length + data.length;
  }
  const centralBytes = Buffer.concat(central);
  const zip64 = entries.length > 0xffff;
  let zip64Tail = Buffer.alloc(0);
  if (zip64) {
    const record = Buffer.alloc(56);
    record.writeUInt32LE(0x06064b50, 0);
    record.writeBigUInt64LE(44n, 4);
    record.writeUInt16LE(45, 12);
    record.writeUInt16LE(45, 14);
    record.writeBigUInt64LE(BigInt(entries.length), 24);
    record.writeBigUInt64LE(BigInt(entries.length), 32);
    record.writeBigUInt64LE(BigInt(centralBytes.length), 40);
    record.writeBigUInt64LE(BigInt(offset), 48);
    const locator = Buffer.alloc(20);
    locator.writeUInt32LE(0x07064b50, 0);
    locator.writeBigUInt64LE(BigInt(offset + centralBytes.length), 8);
    locator.writeUInt32LE(1, 16);
    zip64Tail = Buffer.concat([record, locator]);
  }
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(zip64 ? 0xffff : entries.length, 8);
  end.writeUInt16LE(zip64 ? 0xffff : entries.length, 10);
  end.writeUInt32LE(centralBytes.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...local, centralBytes, zip64Tail, end]);
}

test("built pdf package rejects hostile structures without trapping", () => {
  assertLimit(() => pdf.csv_to_pdf(Buffer.from(`${"x,".repeat(100_000)}x`)));

  let markdown = "";
  for (let depth = 0; depth < 500; depth++) markdown += `${"  ".repeat(depth)}- item\n`;
  assertLimit(() => pdf.markdown_to_pdf(markdown));

  const watermark = JSON.stringify({ contentType: "text", lines: Array(10_000).fill("x") });
  assertLimit(() => pdf.apply_watermark(basePdf, watermark));

  const png = Buffer.from(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl2nKsAAAAASUVORK5CYII=",
    "base64",
  );
  png.writeUInt32BE(100_000, 16);
  png.writeUInt32BE(100_000, 20);
  png.writeUInt32BE(crc32(png.subarray(12, 29)), 29);
  assertLimit(() => pdf.apply_watermark(basePdf, JSON.stringify({ contentType: "image" }), png));

  assert.equal(pdf.page_count(basePdf), 1, "instance remains usable after typed failures");
});

test("built office package rejects 100 nested DrawingML groups", () => {
  const groups = "<p:grpSp><p:grpSpPr/>".repeat(100);
  const closes = "</p:grpSp>".repeat(100);
  const archive = storedZip([
    ["[Content_Types].xml", `<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/></Types>`],
    ["_rels/.rels", `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>`],
    ["ppt/presentation.xml", `<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId r:id="rId1"/></p:sldIdLst></p:presentation>`],
    ["ppt/_rels/presentation.xml.rels", `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>`],
    ["ppt/slides/slide1.xml", `<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><p:cSld><p:spTree>${groups}${closes}</p:spTree></p:cSld></p:sld>`],
  ]);
  const profile = JSON.parse(office.office_profile(archive, "pptx"));
  assert.ok(
    profile.reasons.some((reason) => reason.code === "decompression_limit"),
    JSON.stringify(profile),
  );
});

test("built office package rejects 100k ZIP entries within five seconds", () => {
  const started = performance.now();
  const entries = Array.from({ length: 100_000 }, (_, index) => [`p${index}`, "x"]);
  const profile = JSON.parse(office.office_profile(storedZip(entries), "docx"));
  assert.ok(profile.reasons.some((reason) => reason.code === "decompression_limit"));
  assert.ok(performance.now() - started < 5_000);
});

test("built office package rejects 5000-deep XML with decompression_limit", () => {
  const document = Buffer.from(
    `<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>${"<w:sdt>".repeat(5_000)}x${"</w:sdt>".repeat(5_000)}</w:body></w:document>`,
  );
  const archive = storedZip([
    ["[Content_Types].xml", `<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>`],
    ["_rels/.rels", `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>`],
    ["word/document.xml", document],
  ]);
  const profile = JSON.parse(office.office_profile(archive, "docx"));
  assert.equal(profile.supported, false);
  assert.ok(profile.reasons.some((reason) => reason.code === "decompression_limit"));
});
