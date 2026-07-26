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
const { createMockOfficeFontProvider } = require("./fixtures/mock-office-font-provider.cjs");

const COMPLEX_INPUT_SHA256 =
  "8c2a8c4725e77bbfb4ddc7925674dc221611a39b876dbd7ea2efc829e70b8530";
// Includes the stable office-core-pdf-id-v1 trailer ID, not the package patch version.
const COMPLEX_OUTPUT_SHA256 =
  "c33fff476f56e75137907246c9ea2dca03383dd39b9ea5798a9fe7e9146ab8c8";
const MOCK_REGISTERED_FONT_SHA256S = [
  "51edbfa32d8af939913ae1f4ad0a5173e32083499218c133384638090295f0b0", // Carlito Bold
  "59faf27a90dee5c1b7813b12b26c725ea88adc1bfe37b0ac8e0b8b32a5732c2a", // Caladea Regular
  "ca019755404c45627a8566915df99068949dc32ee2bce48d6aeee7542d2a0a89", // Carlito Regular
].sort();

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

function withoutFetchTiming(profile) {
  return {
    ...profile,
    fonts: profile.fonts.map((font) => ({ ...font, fetchDurationMs: null })),
  };
}

async function withWorker(run, options) {
  const worker = createOfficeEngineWorker({
    fontProvider: createMockOfficeFontProvider(),
    notoFallback: false,
    ...options,
  });
  try {
    return await run(worker);
  } finally {
    await worker.close();
  }
}

describe("Office worker API", () => {
  test("on-demand convert and profile match the direct provider facade", async () => {
    await withWorker(async (worker) => {
      const bytes = docx();
      const directOutput = await direct.convertOnDemand(bytes, "docx", {
        provider: createMockOfficeFontProvider(),
        memoryCache: new direct.MemoryFontCache(),
      });
      const output = await worker.convertOnDemand({ bytes, extension: "docx" });
      assert.ok(output.pdf instanceof Uint8Array);
      assert.deepEqual(output.pdf, directOutput.pdf);
      assert.deepEqual(
        withoutFetchTiming(output.profile),
        withoutFetchTiming(directOutput.profile),
      );
      const warmProfile = await worker.profileOnDemand({ bytes, extension: "docx" });
      assert.ok(warmProfile.fonts.every((font) => font.source === "memory_cache"));
      assert.equal(await worker.engineVersion(), direct.version());
    });
  });

  test("complex DOCX direct and worker conversions match the six-page golden", async () => {
    const bytes = complexDocx();
    assert.equal(sha256(bytes), COMPLEX_INPUT_SHA256);
    const directOutput = (await direct.convertOnDemand(bytes, "docx", {
      provider: createMockOfficeFontProvider(),
      memoryCache: new direct.MemoryFontCache(),
    })).pdf;

    await withWorker(async (worker) => {
      // Give both isolated engines the same deterministic provider-resolved
      // face set before comparing their byte-for-byte output.
      const warmup = await worker.convertOnDemand({ bytes: docx(), extension: "docx" });
      const converted = await worker.convertOnDemand({ bytes, extension: "docx" });
      const workerOutput = converted.pdf;
      const registeredHashes = [...new Set(
        [...warmup.providerResults, ...converted.providerResults]
          .filter((result) => result.status === "found")
          .map((result) => result.artifact.sha256),
      )].sort();
      assert.deepEqual(registeredHashes, MOCK_REGISTERED_FONT_SHA256S);
      assert.ok(Buffer.from(workerOutput).equals(Buffer.from(directOutput)));
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
      await worker.profileOnDemand({ bytes, extension: "docx" });
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
      const profile = await worker.profileOnDemand({ bytes: docx(), extension: "docx" });
      assert.equal(profile.profile.supported, true);
    });
  });

  test("trap replaces worker and replacement converts", async () => {
    await withWorker(async (worker) => {
      await assert.rejects(worker.__testTrap(), (error) => assertWorkerError(error, "trap"));
      const output = await worker.convertOnDemand({ bytes: docx(), extension: "docx" });
      assert.equal(Buffer.from(output.pdf.subarray(0, 5)).toString(), "%PDF-");
    });
  });

  test("strict skips Noto while compatible boot-caches and restores it after replacement", async () => {
    let fallbackCalls = 0;
    const fixtureProvider = createMockOfficeFontProvider();
    const fallbackProvider = {
      id: "noto-worker-bootstrap-test",
      cacheKey(request) {
        return fixtureProvider.cacheKey(request);
      },
      async resolve(requests, signal) {
        fallbackCalls += 1;
        return fixtureProvider.resolve(requests, signal);
      },
    };
    await withWorker(async (worker) => {
      const bytes = docx();
      const strict = await worker.profileOnDemand({ bytes, extension: "docx", mode: "strict" });
      assert.equal(fallbackCalls, 0);
      assert.equal(strict.fallbackResults.length, 0);

      const compatible = await worker.profileOnDemand({ bytes, extension: "docx", mode: "compatible" });
      assert.equal(fallbackCalls, 1);
      assert.equal(compatible.fallbackResults.length, 6);
      assert.ok(compatible.fallbackResults.every(({ cacheStatus }) => cacheStatus === "miss"));

      await assert.rejects(worker.__testTrap(), (error) => assertWorkerError(error, "trap"));
      const replacement = await worker.profileOnDemand({ bytes, extension: "docx", mode: "compatible" });
      assert.equal(fallbackCalls, 1, "replacement should restore fallback from cache without fetching");
      assert.ok(replacement.fallbackResults.every(({ cacheStatus }) => cacheStatus === "memory_hit"));
    }, { notoFallback: fallbackProvider });
  });

  test("three concurrent profile calls serialize and resolve", async () => {
    await withWorker(async (worker) => {
      const bytes = docx();
      const profiles = await Promise.all([
        worker.profileOnDemand({ bytes, extension: "docx" }),
        worker.profileOnDemand({ bytes, extension: "docx" }),
        worker.profileOnDemand({ bytes, extension: "docx" }),
      ]);
      assert.ok(profiles.every(({ profile }) => profile.supported === true));
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
