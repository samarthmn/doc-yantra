import { after, describe, test } from "node:test";
import { deflateSync } from "node:zlib";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { cpSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { performance } from "node:perf_hooks";

process.env.DOC_YANTRA_WORKER_TEST_HOOKS = "1";

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", "..");
const PKG = join(ROOT, "pdf-core", "wasm", "pkg");
const FONT_PKG = join(ROOT, "packages", "doc-yantra-fonts");
const SOURCE_STAGE = join(ROOT, "tmp", `worker-pdf-source-${process.pid}`);
const direct = require(PKG);
const offlineFonts = require(FONT_PKG);
const { createPdfEngineWorker, EngineWorkerError } = require(join(PKG, "worker.js"));
const { ENGINE_ERROR_CODES } = require(join(PKG, "worker-client.cjs"));

const packagedFontOrder = JSON.parse(readFileSync(join(FONT_PKG, "fonts", "manifest.json"), "utf8"))
  .fonts
  .map((font) => font.file);
for (const name of packagedFontOrder) {
  direct.register_font(readFileSync(join(FONT_PKG, "fonts", name)));
}

function mock(kind, name = "file1." + kind) {
  return readFileSync(join(ROOT, "tests/fixtures/samples", kind, name));
}

function assertWorkerError(error, code) {
  assert.ok(error instanceof EngineWorkerError, `expected EngineWorkerError, got ${error}`);
  assert.equal(error.code, code);
  return true;
}

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (0xedb88320 & -(crc & 1));
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const name = Buffer.from(type);
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const checksum = Buffer.alloc(4);
  checksum.writeUInt32BE(crc32(Buffer.concat([name, data])));
  return Buffer.concat([length, name, data, checksum]);
}

/// A structurally valid PNG that declares 60M pixels — well past the watermark budget.
///
/// The IDAT chunk matters: a PNG without one is *malformed*, and the decoder rejects it
/// before any size is read, which would prove `invalid_input` rather than the
/// over-budget classification this test exists to pin.
function oversizedPixelPng() {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(10_000, 0);
  header.writeUInt32BE(6_000, 4);
  header[8] = 8;
  header[9] = 0;
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    pngChunk("IHDR", header),
    pngChunk("IDAT", deflateSync(Buffer.alloc(64))),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
}

async function withWorker(run, options) {
  const worker = createPdfEngineWorker(options);
  try {
    return await run(worker);
  } finally {
    await worker.close();
  }
}

function sourceWorkerFactory() {
  if (!sourceWorkerFactory.value) {
    mkdirSync(join(ROOT, "tmp"), { recursive: true });
    cpSync(PKG, SOURCE_STAGE, { recursive: true });
    for (const [source, destination] of [
      [join(ROOT, "wasm-worker", "worker-client.cjs"), "worker-client.cjs"],
      [join(ROOT, "wasm-worker", "worker-entry.cjs"), "worker-entry.cjs"],
      [join(ROOT, "pdf-core", "wasm", "worker.js"), "worker.js"],
      [join(ROOT, "pdf-core", "wasm", "worker-ops.cjs"), "worker-ops.cjs"],
      [join(ROOT, "pdf-core", "wasm", "font-provider.cjs"), "font-provider.cjs"],
      [join(FONT_PKG, "fonts", "manifest.json"), "font-manifest.json"],
    ]) cpSync(source, join(SOURCE_STAGE, destination));
    const staged = require(join(SOURCE_STAGE, "worker.js"));
    Object.setPrototypeOf(staged.EngineWorkerError.prototype, EngineWorkerError.prototype);
    sourceWorkerFactory.value = staged.createPdfEngineWorker;
  }
  return sourceWorkerFactory.value;
}

async function withSourceWorker(run, options) {
  const worker = sourceWorkerFactory()(options);
  try {
    return await run(worker);
  } finally {
    await worker.close();
  }
}

after(() => rmSync(SOURCE_STAGE, { force: true, recursive: true }));

describe("PDF worker API", () => {
  test("boots font-free operations without assets and loads only required Unicode faces", async () => {
    const resolved = [];
    const fontProvider = {
      async resolve(file) {
        resolved.push(file);
        return { bytes: offlineFonts.readFont(file) };
      },
    };
    await withSourceWorker(async (worker) => {
      const pdf = mock("pdf", "file1.pdf");
      assert.equal(await worker.pageCount({ pdf }), 1);
      assert.equal(await worker.validatePdf({ pdf }), direct.validate_pdf(pdf));
      await worker.mergePdfs({ pdfs: [pdf] });
      await worker.textToPdf({ text: "ASCII needs no downloaded face" });
      assert.deepEqual(resolved, []);

      const text = "Καλημέρα العربية हिन्दी ไทย 中文 😀";
      const unicodePdf = await worker.textToPdf({ text });
      assert.deepEqual(unicodePdf, direct.text_to_pdf(text));
      assert.deepEqual(resolved, [
        "LiberationSans-Regular.ttf",
        "LiberationSans-Bold.ttf",
        "LiberationSans-Italic.ttf",
        "LiberationSans-BoldItalic.ttf",
        "LiberationMono-Regular.ttf",
        "NotoSansCJKsc-Regular.otf",
        "NotoSansArabic-Variable.ttf",
        "NotoSansDevanagari-Variable.ttf",
        "NotoSansThai-Variable.ttf",
        "NotoEmoji-Variable.ttf",
      ]);

      await worker.textToPdf({ text });
      assert.equal(resolved.length, 10, "warm operation must reuse parent-side font bytes");
      await assert.rejects(worker.__testTrap(), (error) => assertWorkerError(error, "trap"));
      assert.deepEqual(await worker.textToPdf({ text }), unicodePdf);
      assert.equal(resolved.length, 10, "worker replacement must reuse parent-side font bytes");
    }, { fontProvider });
  });

  test("font resolution obeys the operation deadline and aborts the provider", async () => {
    let providerSignal;
    const fontProvider = {
      async resolve(file, signal) {
        providerSignal = signal;
        await new Promise((resolve) => setTimeout(resolve, 200));
        return { bytes: offlineFonts.readFont(file) };
      },
    };
    await withSourceWorker(async (worker) => {
      const started = performance.now();
      await assert.rejects(
        worker.textToPdf({ text: "Καλημέρα", timeoutMs: 30 }),
        (error) => assertWorkerError(error, "deadline_exceeded"),
      );
      assert.ok(performance.now() - started < 150, "font provider ignored the operation deadline");
      assert.equal(providerSignal?.aborted, true);
    }, { fontProvider });
  });

  test("worker outputs match direct synchronous operations", async () => {
    await withWorker(async (worker) => {
      const a = mock("pdf", "file1.pdf");
      const b = mock("pdf", "file2.pdf");
      const csv = mock("csv", "file1.csv");
      const markdown = readFileSync(join(ROOT, "tests/fixtures/samples", "md", "file1.md"), "utf8");
      const text = "Worker parity\nline two";
      const watermarkJson = JSON.stringify({
        pattern: "grid",
        lines: ["DRAFT", "DO NOT COPY"],
        color: "#FF0000",
      });
      const areasJson = JSON.stringify([
        { pageIndex: 0, left: 0, top: 0, width: 100, height: 100 },
      ]);
      const redactInput = direct.text_to_pdf("WORKER-REDACT-SECRET");

      assert.deepEqual(
        await worker.mergePdfs({ pdfs: [a, b] }),
        direct.merge_pdfs([a, b]),
      );
      assert.deepEqual(
        await worker.applyWatermark({ pdf: a, watermarkJson }),
        direct.apply_watermark(a, watermarkJson),
      );
      assert.deepEqual(await worker.csvToPdf({ csv }), direct.csv_to_pdf(csv));
      assert.deepEqual(
        await worker.markdownToPdf({ markdown }),
        direct.markdown_to_pdf(markdown),
      );
      assert.deepEqual(await worker.textToPdf({ text }), direct.text_to_pdf(text));
      assert.deepEqual(
        await worker.redactPdf({ pdf: redactInput, areasJson }),
        direct.redact_pdf(redactInput, areasJson),
      );
      assert.deepEqual(
        await worker.redactPdfWithReport({ pdf: redactInput, areasJson }),
        direct.redact_pdf_with_report(redactInput, areasJson),
      );
      assert.equal(await worker.pageCount({ pdf: a }), direct.page_count(a));
      assert.equal(await worker.validatePdf({ pdf: a }), direct.validate_pdf(a));
      assert.equal(await worker.engineVersion(), direct.version());
    });
  });

  test("caller input remains attached and unchanged", async () => {
    await withWorker(async (worker) => {
      const input = new Uint8Array(mock("pdf", "file1.pdf"));
      const before = input.slice();
      await worker.pageCount({ pdf: input });
      assert.notEqual(input.byteLength, 0);
      assert.deepEqual(input, before);
    });
  });

  test("three concurrent operations run FIFO and resolve", async () => {
    await withWorker(async (worker) => {
      const [csv, markdown, text] = await Promise.all([
        worker.csvToPdf({ csv: mock("csv", "file1.csv") }),
        worker.markdownToPdf({ markdown: "# queued markdown" }),
        worker.textToPdf({ text: "queued text" }),
      ]);
      assert.deepEqual(csv, direct.csv_to_pdf(mock("csv", "file1.csv")));
      assert.deepEqual(markdown, direct.markdown_to_pdf("# queued markdown"));
      assert.deepEqual(text, direct.text_to_pdf("queued text"));
    });
  });

  test("busy-loop timeout terminates worker and next operation succeeds", async () => {
    await withWorker(async (worker) => {
      const started = performance.now();
      await assert.rejects(
        worker.__testSleep({ ms: 5_000, timeoutMs: 100 }),
        (error) => assertWorkerError(error, "deadline_exceeded"),
      );
      assert.ok(performance.now() - started < 1_000, "blocking worker was not terminated promptly");
      assert.equal(await worker.pageCount({ pdf: mock("pdf", "file1.pdf") }), 1);
    });
  });

  test("forced trap replaces worker and next operation succeeds", async () => {
    await withWorker(async (worker) => {
      const before = await worker.__testWorkerIdentity();
      await assert.rejects(worker.__testTrap(), (error) => assertWorkerError(error, "trap"));
      const after = await worker.__testWorkerIdentity();
      assert.notEqual(after.threadId, before.threadId);
      assert.equal(await worker.pageCount({ pdf: mock("pdf", "file1.pdf") }), 1);
    });
  });

  test("large CSV conversion times out, then replacement handles small input", { timeout: 30_000 }, async (t) => {
    let rows = 8_000;
    let csv;
    let directMs = 0;
    while (directMs < 500 && rows <= 2_000_000) {
      csv = Buffer.from(`name,value\n${"worker-timeout,1234567890\n".repeat(rows)}`);
      const started = performance.now();
      direct.csv_to_pdf(csv);
      directMs = performance.now() - started;
      if (directMs < 500) rows *= 2;
    }
    assert.ok(directMs >= 500, `direct conversion only took ${directMs.toFixed(1)} ms at ${rows} rows`);
    t.diagnostic(`large CSV: ${rows} rows; direct ${directMs.toFixed(1)} ms`);

    await withWorker(async (worker) => {
      const started = performance.now();
      await assert.rejects(
        worker.csvToPdf({ csv, timeoutMs: 50 }),
        (error) => assertWorkerError(error, "deadline_exceeded"),
      );
      assert.ok(performance.now() - started < 1_000, "timed-out CSV call exceeded one second");
      const small = mock("csv", "file1.csv");
      assert.deepEqual(await worker.csvToPdf({ csv: small }), direct.csv_to_pdf(small));
    });
  });

  test("boot failure rejects worker_boot_failed", async () => {
    await withWorker(async (worker) => {
      await assert.rejects(
        worker.pageCount({ pdf: mock("pdf", "file1.pdf") }),
        (error) => assertWorkerError(error, "worker_boot_failed"),
      );
    }, { __workerEntryPath: join(ROOT, "tests", "missing-worker-entry.cjs") });
  });

  test("input cap rejects before worker spawn", async () => {
    await withWorker(async (worker) => {
      const oversized = new Uint8Array(100 * 1024 * 1024 + 1);
      await assert.rejects(
        worker.pageCount({ pdf: oversized }),
        (error) => assertWorkerError(error, "resource_limit"),
      );
      await assert.rejects(
        worker.applyWatermark({
          pdf: mock("pdf", "file1.pdf"),
          watermarkJson: "{}",
          watermarkImage: oversized,
        }),
        (error) => assertWorkerError(error, "resource_limit"),
      );
    });
  });

  test("public input validation rejects accessors without executing them", async () => {
    await withSourceWorker(async (worker) => {
      await worker.engineVersion();
      let reads = 0;
      const input = {
        timeoutMs: 10,
        get pdf() {
          reads += 1;
          return mock("pdf", "file1.pdf");
        },
      };
      await assert.rejects(
        worker.pageCount(input),
        (error) => assertWorkerError(error, "invalid_input"),
      );
      assert.equal(reads, 0);
    }, { __workerEntryPath: join(ROOT, "tests", "node", "fixtures", "worker-client-stub.cjs") });
  });

  test("watermark JSON and redaction overlay caps fail closed before enqueue", async () => {
    await withSourceWorker(async (worker) => {
      const pdf = mock("pdf", "file1.pdf");
      const oversizedWatermark = JSON.stringify({ lines: ["x".repeat(1024 * 1024)] });
      const originalParse = JSON.parse;
      let oversizedParseCalls = 0;
      JSON.parse = (value, ...args) => {
        if (typeof value === "string" && Buffer.byteLength(value, "utf8") > 1024 * 1024) {
          oversizedParseCalls += 1;
        }
        return originalParse(value, ...args);
      };
      try {
        await assert.rejects(
          worker.applyWatermark({ pdf, watermarkJson: oversizedWatermark }),
          (error) => assertWorkerError(error, "resource_limit"),
        );
        await assert.rejects(
          worker.applyWatermark({ pdf, watermarkJson: "{" + "é".repeat(600_000) }),
          (error) => assertWorkerError(error, "resource_limit"),
        );
        const area = { pageIndex: 0, left: 0, top: 0, width: 1, height: 1 };
        await assert.rejects(
          worker.redactPdf({
            pdf,
            areasJson: JSON.stringify([{ ...area, padding: "é".repeat(600_000) }]),
          }),
          (error) => assertWorkerError(error, "resource_limit"),
        );
        await assert.rejects(
          worker.redactPdf({ pdf, areasJson: "[" + "é".repeat(600_000) }),
          (error) => assertWorkerError(error, "resource_limit"),
        );
        assert.equal(oversizedParseCalls, 0, "oversized JSON reached JSON.parse");
        await assert.rejects(
          worker.applyWatermark({ pdf, watermarkJson: "{bad" }),
          (error) => assertWorkerError(error, "invalid_input"),
        );
      } finally {
        JSON.parse = originalParse;
      }
      for (const key of ["pages", "ranges"]) {
        await assert.rejects(
          worker.applyWatermark({
            pdf,
            watermarkJson: JSON.stringify({ [key]: Array(10_001).fill(null) }),
          }),
          (error) => assertWorkerError(error, "resource_limit"),
        );
      }
      const areasJson = JSON.stringify(Array.from({ length: 801 }, () => ({
        pageIndex: 0, left: 0, top: 0, width: 1, height: 1,
      })));
      await assert.rejects(
        worker.redactPdf({ pdf, areasJson }),
        (error) => assertWorkerError(error, "resource_limit"),
      );
    });
  });

  test("valid capacity overflows use resource_limit", async () => {
    await withSourceWorker(async (worker) => {
      const pdf = mock("pdf", "file1.pdf");
      await assert.rejects(
        worker.applyWatermark({
          pdf,
          watermarkJson: JSON.stringify({ lines: Array(101).fill("line") }),
        }),
        (error) => assertWorkerError(error, "resource_limit"),
      );
      await assert.rejects(
        worker.applyWatermark({
          pdf,
          watermarkJson: JSON.stringify({ contentType: "image" }),
          watermarkImage: oversizedPixelPng(),
        }),
        (error) => assertWorkerError(error, "resource_limit"),
      );
    });
  });

  test("engine error codes travel as a machine envelope, not message prose", async () => {
    // The classifier regexes are gone: the wasm boundary serializes
    // {"dy":1,"code":…} and the client forwards the code verbatim.
    assert.deepEqual([...ENGINE_ERROR_CODES].sort(), [
      "internal_error",
      "invalid_input",
      "invalid_output",
      "malformed_container",
      "missing_asset",
      "missing_glyph",
      "password_protected",
      "resource_limit",
      "unsupported_feature",
      "unsupported_format",
    ]);

    await withWorker(async (worker) => {
      await assert.rejects(
        worker.pageCount({ pdf: Buffer.from("not a pdf") }),
        (error) => assertWorkerError(error, "malformed_container"),
      );
      // Ordinary engine errors keep the worker reusable.
      assert.equal(await worker.pageCount({ pdf: mock("pdf", "file1.pdf") }), 1);
    });
  });

  test("reports cold boot and warm-operation overhead", async (t) => {
    const csv = mock("csv", "file1.csv");
    const directRuns = 5;
    let started = performance.now();
    for (let index = 0; index < directRuns; index++) direct.csv_to_pdf(csv);
    const directMs = (performance.now() - started) / directRuns;

    await withWorker(async (worker) => {
      started = performance.now();
      await worker.engineVersion();
      const bootMs = performance.now() - started;
      started = performance.now();
      for (let index = 0; index < directRuns; index++) await worker.csvToPdf({ csv });
      const workerMs = (performance.now() - started) / directRuns;
      t.diagnostic(
        `timing: cold boot ${bootMs.toFixed(1)} ms; direct CSV ${directMs.toFixed(2)} ms; worker CSV ${workerMs.toFixed(2)} ms; overhead ${(workerMs - directMs).toFixed(2)} ms/op`,
      );
    });
  });
});
