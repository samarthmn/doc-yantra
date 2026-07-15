import { describe, test } from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { readFileSync } from "node:fs";
import { performance } from "node:perf_hooks";

process.env.DOC_YANTRA_WORKER_TEST_HOOKS = "1";

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const PKG = join(ROOT, "office-core", "wasm", "pkg");
const direct = require(PKG);
const pdf = require(join(ROOT, "pdf-core", "wasm", "pkg"));
const { createOfficeEngineWorker, EngineWorkerError } = require(join(PKG, "worker.js"));

const COMPLEX_INPUT_SHA256 =
  "8c2a8c4725e77bbfb4ddc7925674dc221611a39b876dbd7ea2efc829e70b8530";
const COMPLEX_OUTPUT_SHA256 =
  "0f2dd536a567ec52716dc95d85e822f8c48cda6c7e7171cbc75cc801e3ced60c";

function docx() {
  return readFileSync(join(ROOT, "mock-files", "docx", "file1.docx"));
}

function complexDocx() {
  return readFileSync(join(ROOT, "fidelity-corpus", "docx", "complex", "source.docx"));
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function assertWorkerError(error, code) {
  assert.ok(error instanceof EngineWorkerError, `expected EngineWorkerError, got ${error}`);
  assert.equal(error.code, code);
  return true;
}

async function withWorker(run, options) {
  const worker = createOfficeEngineWorker(options);
  try {
    return await run(worker);
  } finally {
    await worker.close();
  }
}

describe("Office worker API", () => {
  test("convert and profile match direct engine", async () => {
    await withWorker(async (worker) => {
      const bytes = docx();
      const output = await worker.convert({ bytes, extension: "docx" });
      assert.ok(output instanceof Uint8Array);
      assert.deepEqual(output, direct.office_to_pdf(bytes, "docx"));
      assert.deepEqual(
        await worker.profile({ bytes, extension: "docx" }),
        JSON.parse(direct.office_profile(bytes, "docx")),
      );
      assert.equal(await worker.engineVersion(), direct.version());
    });
  });

  test("complex DOCX direct and worker conversions match the six-page golden", async () => {
    const bytes = complexDocx();
    assert.equal(sha256(bytes), COMPLEX_INPUT_SHA256);
    const directOutput = direct.office_to_pdf(bytes, "docx");

    await withWorker(async (worker) => {
      const workerOutput = await worker.convert({ bytes, extension: "docx" });
      assert.deepEqual(workerOutput, directOutput);
      assert.equal(sha256(directOutput), COMPLEX_OUTPUT_SHA256);
      assert.equal(sha256(workerOutput), COMPLEX_OUTPUT_SHA256);
      assert.equal(Buffer.from(workerOutput.subarray(0, 5)).toString(), "%PDF-");
      assert.equal(pdf.page_count(workerOutput), 6);
    });
  });

  test("caller bytes remain attached and unchanged", async () => {
    await withWorker(async (worker) => {
      const bytes = new Uint8Array(docx());
      const before = bytes.slice();
      await worker.profile({ bytes, extension: "docx" });
      assert.notEqual(bytes.byteLength, 0);
      assert.deepEqual(bytes, before);
    });
  });

  test("busy-loop timeout terminates worker and replacement succeeds", async () => {
    await withWorker(async (worker) => {
      const started = performance.now();
      await assert.rejects(
        worker.__testSleep({ ms: 5_000, timeoutMs: 100 }),
        (error) => assertWorkerError(error, "deadline_exceeded"),
      );
      assert.ok(performance.now() - started < 1_000, "blocking worker was not terminated promptly");
      const profile = await worker.profile({ bytes: docx(), extension: "docx" });
      assert.equal(profile.supported, true);
    });
  });

  test("trap replaces worker and replacement converts", async () => {
    await withWorker(async (worker) => {
      await assert.rejects(worker.__testTrap(), (error) => assertWorkerError(error, "trap"));
      const output = await worker.convert({ bytes: docx(), extension: "docx" });
      assert.equal(Buffer.from(output.subarray(0, 5)).toString(), "%PDF-");
    });
  });

  test("three concurrent profile calls serialize and resolve", async () => {
    await withWorker(async (worker) => {
      const bytes = docx();
      const profiles = await Promise.all([
        worker.profile({ bytes, extension: "docx" }),
        worker.profile({ bytes, extension: "docx" }),
        worker.profile({ bytes, extension: "docx" }),
      ]);
      assert.ok(profiles.every((profile) => profile.supported === true));
    });
  });

  test("boot failure rejects worker_boot_failed", async () => {
    await withWorker(async (worker) => {
      await assert.rejects(
        worker.profile({ bytes: docx(), extension: "docx" }),
        (error) => assertWorkerError(error, "worker_boot_failed"),
      );
    }, { __workerEntryPath: join(ROOT, "tests", "missing-worker-entry.cjs") });
  });
});
