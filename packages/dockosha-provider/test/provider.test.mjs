import assert from "node:assert/strict";
import { existsSync, rmSync } from "node:fs";
import path from "node:path";
import test from "node:test";

const adapter = await import("../dist/index.js");

test("createProvider exposes the complete DocYantra provider contract", async () => {
  const provider = adapter.createProvider();

  assert.equal(provider.id, "docyantra");
  assert.deepEqual([...provider.capabilities], [
    "merge",
    "watermark",
    "csv",
    "markdown",
    "redaction",
    "page_count",
    "conversion",
    "office_conversion",
    "probe",
  ]);

  const timedOut = await provider.mergeAndWatermark({
    pdfs: [],
    timeoutMs: 0,
  });
  assert.equal(timedOut.ok, false);
  assert.equal(timedOut.code, "deadline_exceeded");
  assert.equal(timedOut.operation, "merge");
  assert.equal(timedOut.format, "pdf");
  assert.equal(timedOut.retryable, true);
});

test("syncBrowserAssets copies runtime glue and wasm into the requested directory", () => {
  const targetDirectory = path.join(
    process.cwd(),
    "tmp",
    `dockosha-provider-assets-${process.pid}`,
  );
  rmSync(targetDirectory, { recursive: true, force: true });

  try {
    adapter.syncBrowserAssets({ targetDirectory });
    assert.equal(
      existsSync(path.join(targetDirectory, "pdf_core_wasm.js")),
      true,
    );
    assert.equal(
      existsSync(path.join(targetDirectory, "pdf_core_wasm_bg.wasm")),
      true,
    );
  } finally {
    rmSync(targetDirectory, { recursive: true, force: true });
  }
});
