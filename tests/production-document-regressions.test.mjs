import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, join } from "node:path";
import { storedZip, buildPdf } from "./helpers/synthetic-documents.mjs";
import { loadPdfjsStack } from "./helpers/pdfjs-paint.mjs";
const require = createRequire(import.meta.url);
const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const stack = loadPdfjsStack();
if (!stack && process.env.REQUIRE_PDFJS_PAINT === "1")
  throw new Error(
    "Packaged document regressions require the locked PDF.js test dependencies",
  );
const PDF = join(ROOT, "pdf-core/wasm/pkg");
const OFFICE = join(ROOT, "office-core/wasm/pkg");
const options = JSON.stringify({ mode: "compatible" });
const font = readFileSync(
  join(ROOT, "packages/doc-yantra-fonts/fonts/LiberationSans-Regular.ttf"),
);

function noteWorkbook({
  visible = true,
  print = true,
  control = false,
  canonical = true,
  anchor = "1, 0, 0, 10, 4, 0, 4, 10",
} = {}) {
  const S = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
  const R =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
  const entries = [
    [
      "[Content_Types].xml",
      `<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>`,
    ],
    [
      "_rels/.rels",
      `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="root" Type="${R}/officeDocument" Target="xl/workbook.xml"/></Relationships>`,
    ],
    [
      "xl/workbook.xml",
      `<workbook xmlns="${S}" xmlns:r="${R}"><sheets><sheet name="Notes" sheetId="1" r:id="sheet"/></sheets></workbook>`,
    ],
    [
      "xl/_rels/workbook.xml.rels",
      `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheet" Type="${R}/worksheet" Target="worksheets/sheet1.xml"/></Relationships>`,
    ],
    [
      "xl/worksheets/sheet1.xml",
      `<worksheet xmlns="${S}" xmlns:r="${R}"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Keep cell</t></is></c></row></sheetData><pageSetup cellComments="${print ? "asDisplayed" : "none"}"/><legacyDrawing r:id="vml"/></worksheet>`,
    ],
    [
      "xl/worksheets/_rels/sheet1.xml.rels",
      `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="vml" Type="${R}/vmlDrawing" Target="../drawings/note.vml"/><Relationship Id="comments" Type="${R}/comments" Target="../comments.xml"/></Relationships>`,
    ],
    [
      "xl/comments.xml",
      `<comments xmlns="${S}"><commentList><comment ref="A1"><text><t>Résumé note</t></text></comment></commentList></comments>`,
    ],
    [
      "xl/drawings/note.vml",
      `<xml xmlns:v="urn:schemas-microsoft-com:vml" xmlns:x="urn:schemas-microsoft-com:office:excel" xmlns:o="urn:schemas-microsoft-com:office:office">${canonical ? '<v:shapetype id="_x0000_t202" coordsize="21600,21600" o:spt="202" path="m,l,21600r21600,l21600,xe"/>' : ""}<v:shape type="#_x0000_t202" style="position:absolute;margin-left:70pt;margin-top:10pt;width:180pt;height:60pt;visibility:${visible ? "visible" : "hidden"}" fillcolor="#ffffe1"><v:textbox/><x:ClientData ObjectType="${control ? "Button" : "Note"}">${visible ? "<x:Visible/>" : ""}<x:Row>0</x:Row><x:Column>0</x:Column><x:Anchor>${anchor}</x:Anchor></x:ClientData></v:shape></xml>`,
    ],
  ];
  return new Uint8Array(storedZip(entries));
}

// Read the committed TrueType's actual BMP cmap; fixture IDs cannot silently
// drift if the font package is updated.
function glyphId(bytes, code) {
  let cmap;
  for (let i = 0; i < bytes.readUInt16BE(4); i++) {
    const at = 12 + i * 16;
    if (bytes.toString("ascii", at, at + 4) === "cmap")
      cmap = bytes.readUInt32BE(at + 8);
  }
  assert.notEqual(cmap, undefined);
  for (let i = 0; i < bytes.readUInt16BE(cmap + 2); i++) {
    const at = cmap + 4 + i * 8;
    const sub = cmap + bytes.readUInt32BE(at + 4);
    if (bytes.readUInt16BE(sub) !== 4) continue;
    const count = bytes.readUInt16BE(sub + 6) / 2;
    const ends = sub + 14,
      starts = ends + 2 * count + 2,
      deltas = starts + 2 * count,
      offsets = deltas + 2 * count;
    for (let seg = 0; seg < count; seg++)
      if (
        code >= bytes.readUInt16BE(starts + 2 * seg) &&
        code <= bytes.readUInt16BE(ends + 2 * seg)
      ) {
        const delta = bytes.readInt16BE(deltas + 2 * seg),
          range = bytes.readUInt16BE(offsets + 2 * seg);
        const gid = range
          ? bytes.readUInt16BE(
              offsets +
                2 * seg +
                range +
                2 * (code - bytes.readUInt16BE(starts + 2 * seg)),
            )
          : code;
        return (gid + delta) & 65535;
      }
  }
  throw new Error("Fixture font does not map character");
}
function cidPdf(tx = 0, ty = 0) {
  const i = glyphId(font, 105).toString(16).padStart(4, "0"),
    m = glyphId(font, 109).toString(16).padStart(4, "0");
  const content = `q 1 0 0 1 ${tx} ${ty} cm BT /F1 20 Tf 1 0 0 1 50 100 Tm <${i}> Tj 1 0 0 1 60 100 Tm <${m}> Tj ET Q`;
  const cmap = `/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def /CMapName /NoteFixture def /CMapType 2 def 1 begincodespacerange <0000> <FFFF> endcodespacerange 2 beginbfchar <${i}> <0069> <${m}> <006d> endbfchar endcmap CMapName currentdict /CMap defineresource pop end end`;
  return buildPdf([
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    `<< /Length ${content.length} >>\nstream\n${content}\nendstream`,
    "<< /Type /Font /Subtype /Type0 /BaseFont /LiberationSans /Encoding /Identity-H /DescendantFonts [6 0 R] /ToUnicode 9 0 R >>",
    "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /LiberationSans /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 7 0 R /CIDToGIDMap /Identity /DW 1000 >>",
    "<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 /FontBBox [0 -300 1200 1000] /ItalicAngle 0 /Ascent 1000 /Descent -300 /CapHeight 1000 /StemV 80 /FontFile2 8 0 R >>",
    `<< /Length ${font.length} /Length1 ${font.length} >>\nstream\n${font.toString("latin1")}\nendstream`,
    `<< /Length ${cmap.length} >>\nstream\n${cmap}\nendstream`,
  ]);
}
async function inspect(bytes) {
  const doc = await stack.pdfjsLib.getDocument({
    data: new Uint8Array(bytes),
    useSystemFonts: false,
    disableFontFace: true,
  }).promise;
  try {
    const page = await doc.getPage(1);
    return {
      text: (await page.getTextContent()).items.filter(
        (i) => typeof i.str === "string",
      ),
      ops: await page.getOperatorList(),
    };
  } finally {
    await doc.destroy();
  }
}
async function engines(kind) {
  if (kind === "node") return { pdf: require(PDF), office: require(OFFICE) };
  const pdf = await import(pathToFileURL(join(PDF, "web/pdf_core_wasm.js")));
  const office = await import(
    pathToFileURL(join(OFFICE, "web/office_core_wasm.js"))
  );
  await pdf.default({
    module_or_path: readFileSync(join(PDF, "pdf_core_wasm_bg.wasm")),
  });
  await office.default({
    module_or_path: readFileSync(join(OFFICE, "office_core_wasm_bg.wasm")),
  });
  return { pdf, office };
}
for (const kind of ["node", "web"]) {
  test(
    `packaged ${kind}: legacy Note text, print visibility and authored geometry`,
    { skip: !stack },
    async () => {
      const { office } = await engines(kind);
      office.office_register_font(font);
      office.office_register_font(
        readFileSync(
          join(ROOT, "office-core/fonts/carlito/Carlito-Regular.ttf"),
        ),
      );
      for (const settings of [{}, { visible: false }, { print: false }]) {
        const input = noteWorkbook(settings);
        const profile = JSON.parse(
          office.office_profile_with_options(input, "xlsx", options),
        );
        assert.equal(profile.supported, true, JSON.stringify(profile.reasons));
        const output = office.office_to_pdf_with_options(
          input,
          "xlsx",
          options,
        );
        const { text, ops } = await inspect(output);
        const words = text.map((t) => t.str).join("");
        assert.ok(words.includes("Keep cell"), words);
        const printed = settings.visible !== false && settings.print !== false;
        assert.equal(words.includes("Résumé note"), printed, words);
        if (printed)
          assert.ok(
            ops.fnArray.some(
              (op, index) =>
                op === stack.pdfjsLib.OPS.constructPath &&
                Array.from(ops.argsArray[index][0]).includes(
                  stack.pdfjsLib.OPS.rectangle,
                ) &&
                Array.from(ops.argsArray[index][1]).some(
                  (n, i, values) => n === 180 && values[i + 1] === 60,
                ),
            ),
            "authored180x60 Note rectangle absent",
          );
      }
      for (const settings of [
        { control: true },
        { canonical: false },
        { anchor: "1, 0, 0, 10, 1, 0, 4, 10" },
      ]) {
        const input = noteWorkbook(settings);
        assert.equal(
          JSON.parse(office.office_profile_with_options(input, "xlsx", options))
            .supported,
          false,
        );
        assert.throws(() =>
          office.office_to_pdf_with_options(input, "xlsx", options),
        );
      }
    },
  );
  test(
    `packaged ${kind}: embedded CID glyph redaction preserves transformed neighbor`,
    { skip: !stack },
    async () => {
      const { pdf } = await engines(kind);
      for (const [tx, ty] of [
        [0, 0],
        [20, 10],
      ]) {
        const input = cidPdf(tx, ty);
        const before = await inspect(input);
        assert.ok(
          before.text
            .map((t) => t.str)
            .join("")
            .includes("i"),
        );
        assert.ok(
          before.text
            .map((t) => t.str)
            .join("")
            .includes("m"),
        );
        const report = pdf.redact_pdf_with_report(
          input,
          JSON.stringify([
            {
              pageIndex: 0,
              left: (61 + tx) / 2,
              top: (86 - ty) / 2,
              width: 2,
              height: 8,
            },
          ]),
        );
        const after = await inspect(report.pdf);
        const text = after.text.map((t) => t.str).join("");
        assert.ok(text.includes("i"), `neighbor lost: ${text}`);
        assert.ok(!text.includes("m"), `selected glyph survived: ${text}`);
        const retained = after.text.find((t) => t.str.includes("i"));
        assert.equal(retained.transform[4], 50 + tx);
        assert.equal(retained.transform[5], 100 + ty);
        assert.equal(report.pageCount, 1);
        assert.ok(
          report.warnings.every(
            (w) => w.code === "over_redaction" && w.kind === "text",
          ),
        );
      }
    },
  );
}
