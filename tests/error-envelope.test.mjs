import assert from "node:assert/strict";
import { test } from "node:test";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// End-to-end contract for the structured error envelope: the Rust boundary
// serializes {"dy":1,"code":…,"message":…,"detail":…}; worker-entry forwards
// the code verbatim; the client exposes it as error.code. No layer infers
// identity from message prose.

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const PDF_PKG = join(ROOT, "pdf-core", "wasm", "pkg");
const OFFICE_PKG = join(ROOT, "office-core", "wasm", "pkg");
const { createPdfEngineWorker } = require(join(PDF_PKG, "worker.js"));
const { createOfficeEngineWorker } = require(join(OFFICE_PKG, "worker.js"));
const { EngineWorkerClient } = require(join(PDF_PKG, "worker-client.cjs"));

const mock = (kind, name) => readFileSync(join(ROOT, "mock-files", kind, name));

function expectCode(code, extra = {}) {
  return (error) => {
    assert.equal(error.name, "EngineWorkerError", `unexpected error type: ${error}`);
    assert.equal(error.code, code, `message was: ${error.message}`);
    assert.ok(typeof error.message === "string" && error.message.length > 0);
    if (extra.detail !== undefined) {
      // detail is the envelope detail verbatim: a machine slug string, or a
      // structured { reason, context } object whose reason is the slug.
      const reason =
        typeof error.detail === "string" ? error.detail : error.detail?.reason;
      assert.equal(reason, extra.detail);
      assert.equal(error.engineMessage, extra.detail);
    }
    if (extra.messageStart !== undefined) {
      assert.ok(
        error.message.startsWith(extra.messageStart),
        `expected "${extra.messageStart}…", got "${error.message}"`,
      );
    }
    return true;
  };
}

test("pdf worker: corrupt PDF surfaces malformed_container from the enum, not prose", async () => {
  const worker = createPdfEngineWorker();
  try {
    await assert.rejects(
      worker.pageCount({ pdf: Buffer.from("definitely not a pdf") }),
      expectCode("malformed_container"),
    );
  } finally {
    await worker.close();
  }
});

test("pdf worker: uncovered script is missing_glyph with the machine sub-code in detail", async () => {
  const worker = createPdfEngineWorker();
  try {
    // Ogham is not in the packaged font set; coverage preflight must reject
    // with the TextUnsupported glyph-coverage code.
    await assert.rejects(
      worker.markdownToPdf({ markdown: "ᚑᚌᚐᚋ ogham text" }),
      expectCode("missing_glyph", { detail: "glyph-coverage" }),
    );
  } finally {
    await worker.close();
  }
});

test("pdf worker: engine structural budget is resource_limit", async () => {
  // Payload spacing is clamped to [72,1440]pt so a watermark anchor bomb needs
  // an extreme MediaBox (cargo-tested in watermark_test.rs); the CSV cell
  // budget is the engine-side ResourceLimit reachable with plain inputs.
  const worker = createPdfEngineWorker();
  try {
    const row = "a,".repeat(199).concat("a\n"); // 200 cells per row
    const csv = Buffer.from(row.repeat(10_050)); // > MAX_CSV_CELLS (2M)
    await assert.rejects(
      worker.csvToPdf({ csv }),
      expectCode("resource_limit", { messageStart: "CSV cell count " }),
    );
  } finally {
    await worker.close();
  }
});

test("pdf worker: invalid redaction payload is invalid_input", async () => {
  const worker = createPdfEngineWorker();
  try {
    const pdf = await worker.textToPdf({ text: "redaction arg check" });
    await assert.rejects(
      worker.redactPdf({ pdf, areasJson: "{not-json" }),
      expectCode("invalid_input"),
    );
  } finally {
    await worker.close();
  }
});

test("office worker: legacy CFB container is unsupported_format", async () => {
  const worker = createOfficeEngineWorker();
  try {
    await assert.rejects(
      worker.convert({ bytes: mock("doc", "file1.doc"), extension: "docx" }),
      expectCode("unsupported_format", { detail: "legacy_format" }),
    );
  } finally {
    await worker.close();
  }
});

test("office worker: garbage bytes are malformed_container", async () => {
  const worker = createOfficeEngineWorker();
  try {
    await assert.rejects(
      worker.convert({ bytes: Buffer.from("not an OOXML package"), extension: "docx" }),
      expectCode("malformed_container", { messageStart: "invalid: " }),
    );
  } finally {
    await worker.close();
  }
});

test("worker entry forwards envelope codes verbatim and never classifies prose", async () => {
  const client = new EngineWorkerClient({
    entryPath: join(ROOT, "wasm-worker", "worker-entry.cjs"),
    workerData: {
      gluePath: join(HERE, "fixtures", "fake-glue.cjs"),
      opsPath: join(HERE, "fixtures", "error-shape-ops.cjs"),
    },
  });
  try {
    await assert.rejects(
      client.invoke("envelopeError", {}, 5000),
      expectCode("missing_asset", { detail: "font-missing", messageStart: "asset gone" }),
    );
    await assert.rejects(
      client.invoke("unknownCodeEnvelope", {}, 5000),
      expectCode("internal_error", { messageStart: "strange" }),
    );
    await assert.rejects(
      client.invoke("plainError", {}, 5000),
      expectCode("internal_error", {
        messageStart: "completely unstructured failure prose",
      }),
    );
  } finally {
    await client.close();
  }
});

test("pageCount and validatePdf honor caller deadlines from invocation", async () => {
  // Fresh worker per op: a 1 ms deadline is shorter than worker boot, so the
  // deadline can only fire if it is armed at invocation (boot time included).
  // A warm worker would finish these tiny ops inside 1 ms, so cold workers
  // make the assertion deterministic.
  {
    const worker = createPdfEngineWorker();
    try {
      await assert.rejects(
        worker.pageCount({ pdf: mock("pdf", "file1.pdf"), timeoutMs: 1 }),
        expectCode("deadline_exceeded"),
      );
      // The replacement worker serves the identical call.
      assert.equal(await worker.pageCount({ pdf: mock("pdf", "file1.pdf") }), 1);
    } finally {
      await worker.close();
    }
  }
  {
    const worker = createPdfEngineWorker();
    try {
      await assert.rejects(
        worker.validatePdf({ pdf: mock("pdf", "file1.pdf"), timeoutMs: 1 }),
        expectCode("deadline_exceeded"),
      );
      const info = JSON.parse(await worker.validatePdf({ pdf: mock("pdf", "file1.pdf") }));
      assert.equal(info.pageCount, 1);
    } finally {
      await worker.close();
    }
  }
});
