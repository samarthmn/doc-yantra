import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { test } from "node:test";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

for (const [name, directory, factory] of [
  ["pdf", join(ROOT, "pdf-core", "wasm", "pkg"), "createPdfEngineWorker"],
  ["office", join(ROOT, "office-core", "wasm", "pkg"), "createOfficeEngineWorker"],
]) {
  test(`${name} worker package exposes complete TypeScript declarations`, () => {
    const packageJson = JSON.parse(readFileSync(join(directory, "package.json"), "utf8"));
    assert.ok(packageJson.files.includes("worker.d.ts"));
    assert.equal(packageJson.exports["./worker"].types, "./worker.d.ts");
    assert.ok(existsSync(join(directory, "worker.d.ts")));
    const declaration = readFileSync(join(directory, "worker.d.ts"), "utf8");
    assert.match(declaration, new RegExp(`function ${factory}\\b`));
    // detail preserves the structured envelope detail: a slug string or an
    // object whose `reason` carries the stable slug.
    assert.match(declaration, /type EngineErrorDetail\b/);
    assert.match(declaration, /detail\?: EngineErrorDetail/);
    assert.match(declaration, /defaultTimeoutMs\?: number/);
  });
}

test("source redaction declarations use the complete literal warning contract", () => {
  const kinds = [
    "text",
    "inline_image",
    "image_xobject",
    "form_xobject",
    "vector_path",
    "shading",
    "annotation",
    "widget",
  ];
  for (const root of ["pdf-core", "office-core"]) {
    const declaration = readFileSync(join(ROOT, root, "wasm", "worker.d.ts"), "utf8");
    const warning = declaration.match(/interface RedactionWarning\s*\{([\s\S]*?)\n\}/)?.[1];
    assert.ok(warning, `${root} declaration omitted RedactionWarning`);
    assert.match(warning, /code:\s*['"]over_redaction['"]/);
    assert.match(warning, /page:\s*number/);
    assert.doesNotMatch(warning, /kind\?:|page\?:/);
    for (const kind of kinds) assert.match(warning, new RegExp(`['"]${kind}['"]`));
  }
  const pdfDeclaration = readFileSync(join(ROOT, "pdf-core", "wasm", "worker.d.ts"), "utf8");
  assert.match(pdfDeclaration, /warnings: RedactionWarning\[\]/);
});

test("source declarations document inert input and error-envelope budgets", () => {
  for (const root of ["pdf-core", "office-core"]) {
    const declaration = readFileSync(join(ROOT, root, "wasm", "worker.d.ts"), "utf8");
    assert.match(declaration, /Accessor properties and detectable Proxies are rejected as `invalid_input`/);
    assert.match(declaration, /4,096 UTF-8 bytes/);
    assert.match(declaration, /128 UTF-8/);
    assert.match(declaration, /1,024 UTF-8/);
  }
});
