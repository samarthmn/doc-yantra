// Glyph-coverage regressions for the on-demand Office worker: Fontsource
// symbol/math supplements, pan-CJK registration within the 24 MiB caps, CFF
// (CIDFontType0) emission, and the mock-files/complex-docx corpus. Entirely
// offline: every provider serves local repository bytes through stub fetches.
import { describe, test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { readFileSync, readdirSync } from "node:fs";

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const PKG = join(ROOT, "office-core", "wasm", "pkg");
const { createOfficeEngineWorker } = require(join(PKG, "worker.js"));
const { FontsourceProvider } = require(join(PKG, "fontsource-provider.cjs"));
const { createMockOfficeFontProvider } = require("./fixtures/mock-office-font-provider.cjs");

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

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
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralBytes.length, 12);
  end.writeUInt32LE(offset, 16);
  return Buffer.concat([...local, centralBytes, end]);
}

const CONTENT_TYPES = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>`;

const ROOT_RELS = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>`;

function docxWithBody(bodyInner) {
  const document = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>${bodyInner}<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr></w:body></w:document>`;
  return new Uint8Array(storedZip([
    ["[Content_Types].xml", CONTENT_TYPES],
    ["_rels/.rels", ROOT_RELS],
    ["word/document.xml", document],
  ]));
}

const CARLITO_REGULAR = readFileSync(join(ROOT, "office-core", "fonts", "carlito", "Carlito-Regular.ttf"));
const CARLITO_BOLD = readFileSync(join(ROOT, "office-core", "fonts", "carlito", "Carlito-Bold.ttf"));
const NOTO_SANS_MATH = readFileSync(join(HERE, "fixtures", "fonts", "NotoSansMath-Regular.ttf"));

/**
 * Offline Fontsource stub: Carlito artifacts serve the full repository TTFs,
 * the Noto Sans Math supplement serves the committed fixture, and Noto Sans
 * Symbols 2 is a deterministic 404 (exercising the supplement-skip path).
 */
function fontsourceStub({ withMath = true } = {}) {
  return new FontsourceProvider({
    fetch: async (url) => {
      const href = String(url);
      const body = href.includes("/carlito@")
        ? (href.includes("-700-") ? CARLITO_BOLD : CARLITO_REGULAR)
        : href.includes("/noto-sans-math@") && withMath
          ? NOTO_SANS_MATH
          : undefined;
      if (!body) return new Response(null, { status: 404 });
      return new Response(new Uint8Array(body), {
        headers: { "content-type": "font/ttf" },
      });
    },
  });
}

const CJK_FONT_DIR = join(ROOT, "packages", "doc-yantra-fonts", "fonts");
const CJK_FILES = {
  sc: ["NotoSansCJKsc-Regular.otf", "Noto Sans CJK SC", "NotoSansCJKsc-Regular"],
  jp: ["NotoSansCJKjp-Regular.otf", "Noto Sans CJK JP", "NotoSansCJKjp-Regular"],
  kr: ["NotoSansCJKkr-Regular.otf", "Noto Sans CJK KR", "NotoSansCJKkr-Regular"],
};

function cjkArtifact(variant) {
  const [file, family, postscriptName] = CJK_FILES[variant];
  const bytes = new Uint8Array(readFileSync(join(CJK_FONT_DIR, file)));
  return {
    family,
    fullName: family,
    postscriptName,
    weight: 400,
    italic: false,
    stretch: 5,
    version: "2.004",
    faceIndex: 0,
    format: "otf",
    sha256: sha256(bytes),
    source: "doc-yantra-offline-fonts",
    coverageScripts: ["cjk"],
    license: { id: "OFL-1.1", embeddable: true, subsettable: true },
    bytes,
  };
}

function cjkVariantFor(request) {
  const families = [request.family, ...(request.altFamilies ?? [])].map((f) => f.toLowerCase());
  const match = (hints) => hints.some((hint) => families.some((f) => f.includes(hint)));
  if (match(["malgun", "batang", "gulim", "dotum", "gungsuh"])) return "kr";
  if (match(["mincho", "gothic", "meiryo"])) return "jp";
  return "sc";
}

/** Mirrors DocKosha's packaged provider: base results plus a CJK supplement. */
function withCjkSupplement(base) {
  return {
    id: `${base.id}+cjk`,
    cacheKey: (request) => `${base.cacheKey(request)}|cjk:${(request.scripts ?? []).includes("cjk")}`,
    async resolve(requests, signal) {
      const results = await base.resolve(requests, signal);
      return results.map((result) => {
        if (!(result.request.scripts ?? []).includes("cjk")) return result;
        const supplement = cjkArtifact(cjkVariantFor(result.request));
        if (result.status !== "found") {
          return { status: "found", request: result.request, artifact: supplement, artifacts: [supplement] };
        }
        const artifacts = [...(result.artifacts ?? [result.artifact])]
          .map((artifact) => ({ ...artifact, coverageScripts: artifact.coverageScripts ?? ["latin"] }));
        artifacts.push(supplement);
        return { ...result, artifact: artifacts[0], artifacts };
      });
    },
  };
}

/** Mirrors DocKosha's symbols supplement using the committed math fixture. */
function withMathSupplement(base) {
  const supplement = () => {
    const bytes = new Uint8Array(NOTO_SANS_MATH);
    return {
      family: "Noto Sans Math",
      fullName: "Noto Sans Math",
      postscriptName: "NotoSansMath-Regular",
      weight: 400,
      italic: false,
      stretch: 5,
      version: "fixture",
      faceIndex: 0,
      format: "ttf",
      sha256: sha256(bytes),
      source: "tests/fixtures/fonts",
      coverageCodepoints: [0x2260],
      license: { id: "OFL-1.1", embeddable: true, subsettable: true },
      bytes,
    };
  };
  return {
    id: `${base.id}+math`,
    cacheKey: (request) => `${base.cacheKey(request)}|math:${(request.scripts ?? []).includes("symbols")}`,
    async resolve(requests, signal) {
      const results = await base.resolve(requests, signal);
      return results.map((result) => {
        if (!(result.request.scripts ?? []).includes("symbols")) return result;
        const extra = supplement();
        if (result.status !== "found") {
          return { status: "found", request: result.request, artifact: extra, artifacts: [extra] };
        }
        const artifacts = [...(result.artifacts ?? [result.artifact])]
          .map((artifact) => ({ ...artifact, coverageScripts: artifact.coverageScripts ?? ["latin"] }));
        artifacts.push(extra);
        return { ...result, artifact: artifacts[0], artifacts };
      });
    },
  };
}

async function convertWith(provider, bytes) {
  const worker = createOfficeEngineWorker({
    fontProvider: provider,
    fontMode: "compatible",
    notoFallback: false,
  });
  try {
    return await worker.convertOnDemand({ bytes, extension: "docx", timeoutMs: 120_000 });
  } finally {
    await worker.close();
  }
}

const MATH_BODY =
  '<w:p><w:r><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri"/></w:rPr>' +
  "<w:t>operators × ÷ ≠ ≤ ≥ ∞ and ∮ ⊗</w:t></w:r></w:p>";

const CJK_BODY =
  '<w:p><w:r><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="SimSun"/></w:rPr><w:t>季度报告: 你好世界。</w:t></w:r></w:p>' +
  '<w:p><w:r><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="MS Mincho"/></w:rPr><w:t>日本語のテストです。</w:t></w:r></w:p>' +
  '<w:p><w:r><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="Malgun Gothic"/></w:rPr><w:t>한국어 변환 테스트.</w:t></w:r></w:p>';

describe("office worker glyph-coverage supplements", () => {
  test("math operators outside Google latin subsets convert through the Noto Sans Math supplement", async () => {
    const result = await convertWith(fontsourceStub(), docxWithBody(MATH_BODY));
    const pdf = Buffer.from(result.pdf);
    assert.equal(pdf.subarray(0, 5).toString(), "%PDF-");
    const mathResolution = result.providerResults.find((entry) =>
      (entry.artifacts ?? []).some((artifact) => artifact.family === "Noto Sans Math"));
    assert.ok(mathResolution, "the Calibri request must carry the math supplement");
  });

  test("the glyph-coverage gate still fails closed when no supplement covers a codepoint", async () => {
    await assert.rejects(
      convertWith(fontsourceStub({ withMath: false }), docxWithBody(MATH_BODY)),
      (error) => /missing_glyph/.test(error.message) && /U\+222E|U\+2297/i.test(error.message),
    );
  });

  test("pan-CJK OTFs register within the 24 MiB caps and emit CIDFontType0 CFF fonts", async () => {
    const provider = withCjkSupplement(createMockOfficeFontProvider());
    const result = await convertWith(provider, docxWithBody(CJK_BODY));
    const pdf = Buffer.from(result.pdf);
    assert.equal(pdf.subarray(0, 5).toString(), "%PDF-");
    assert.ok(pdf.includes("CIDFontType0C"), "CFF programs must embed as CIDFontType0C");
    assert.ok(pdf.includes("FontFile3"), "CFF descriptors must reference FontFile3");
    assert.ok(
      pdf.includes("/DocYantra-CFF-Encoding"),
      "CFF Type0 fonts must map codes through the embedded encoding CMap",
    );
  });

  test("CJK stays fail-closed when no provider supplies a covering face", async () => {
    await assert.rejects(
      convertWith(createMockOfficeFontProvider(), docxWithBody(CJK_BODY)),
      /no font for CJK/,
    );
  });

  test("every mock-files/complex-docx corpus file converts to a PDF", async () => {
    const corpus = join(ROOT, "mock-files", "complex-docx");
    const files = readdirSync(corpus).filter((file) => file.endsWith(".docx")).sort();
    assert.ok(files.length >= 4, `expected the committed corpus, found ${files.join(", ")}`);
    const provider = withMathSupplement(createMockOfficeFontProvider());
    const worker = createOfficeEngineWorker({
      fontProvider: provider,
      fontMode: "compatible",
      notoFallback: false,
    });
    try {
      for (const file of files) {
        const bytes = new Uint8Array(readFileSync(join(corpus, file)));
        const result = await worker.convertOnDemand({ bytes, extension: "docx", timeoutMs: 180_000 });
        const pdf = Buffer.from(result.pdf);
        assert.equal(pdf.subarray(0, 5).toString(), "%PDF-", `${file} must produce a PDF`);
        assert.ok(pdf.length > 1_000, `${file} produced a suspiciously small PDF (${pdf.length} bytes)`);
      }
    } finally {
      await worker.close();
    }
  });
});
