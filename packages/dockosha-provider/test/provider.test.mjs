import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
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

test("adapter initializes in a child process without the font package", () => {
  const scriptPath = path.join(
    process.cwd(),
    "tmp",
    "dockosha-provider-fontless-" + process.pid + ".cjs",
  );
  const script = [
    'const Module = require("node:module");',
    "const originalResolve = Module._resolveFilename;",
    "Module._resolveFilename = function (request, parent, isMain, options) {",
    '  if (request === "@samarthmn/doc-yantra-fonts" || request.startsWith("@samarthmn/doc-yantra-fonts/")) {',
    '    throw new Error("font package intentionally unavailable");',
    "  }",
    "  return originalResolve.call(this, request, parent, isMain, options);",
    "};",
    "(async () => {",
    "  const provider = require(" +
      JSON.stringify(path.join(process.cwd(), "dist", "index.js")) +
      ").createProvider();",
    '  if (provider.id !== "docyantra") throw new Error("adapter identity is unavailable");',
    "  const result = await provider.mergeAndWatermark({ pdfs: [], timeoutMs: 0 });",
    '  if (result.ok || result.code !== "deadline_exceeded") throw new Error("font-free operation did not remain portable");',
    "})().catch((error) => { console.error(error); process.exitCode = 1; });",
  ].join("\n");

  try {
    mkdirSync(path.dirname(scriptPath), { recursive: true });
    writeFileSync(scriptPath, script);
    execFileSync(process.execPath, [scriptPath], {
      cwd: process.cwd(),
      stdio: "pipe",
    });
  } finally {
    rmSync(scriptPath, { force: true });
  }
});
