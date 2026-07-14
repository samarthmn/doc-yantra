// Keeps the P3.4 evidence surface visible to the Node-level package suite.
// Semantic extraction itself lives in pdf-core/tests/golden_comparison_test.rs,
// where pdfium can inspect both frozen and current PDF bytes.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const require = createRequire(import.meta.url);
const pdfCore = require(join(ROOT, "pdf-core", "wasm", "pkg"));

function fixtureId(goldenPath) {
  return goldenPath
    .replace("fidelity-corpus/legacy-goldens/", "")
    .replace(/\.(?:libreoffice|chromium|pymupdf-redacted)\.pdf$/, "");
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function frozenEvidencePath(relativePath) {
  if (
    process.env.DOC_YANTRA_DIGEST_PROBE_FILE
    && process.env.DOC_YANTRA_DIGEST_PROBE_TARGET === relativePath
  ) {
    return process.env.DOC_YANTRA_DIGEST_PROBE_FILE;
  }
  return join(ROOT, relativePath);
}

function assertFrozenDigest(relativePath, expected) {
  const bytes = readFileSync(frozenEvidencePath(relativePath));
  const actual = sha256(bytes);
  assert.equal(
    actual,
    expected,
    `frozen evidence digest mismatch for ${relativePath}: manifest expected ${expected}, disk bytes are ${actual}`,
  );
  return bytes;
}

function validateDisposition(entry, kind) {
  for (const field of [
    "fixture_id",
    "reason_code",
    "justification",
    "user_visible_effect",
  ]) {
    assert.equal(
      typeof entry[field],
      "string",
      `${kind} entry lacks ${field}`,
    );
    assert.ok(entry[field].trim(), `${kind} entry has empty ${field}`);
  }
  if (kind === "known-failure") {
    assert.equal(
      typeof entry.tracking_note,
      "string",
      "known-failure entry lacks tracking_note",
    );
    assert.ok(entry.tracking_note.trim(), "known-failure entry has empty tracking_note");
  }
}

test("P3.4 frozen golden comparison evidence and native runner are committed", () => {
  const runner = join(ROOT, "pdf-core/tests/golden_comparison_test.rs");
  const officeRunner = join(
    ROOT,
    "office-core/fidelity/tests/legacy_golden_parity_test.rs",
  );
  const redactionRunner = join(ROOT, "pdf-core/tests/redact_pure_test.rs");
  const allowlistPath = join(ROOT, "fidelity-corpus/legacy-parity-allowlist.json");
  const manifestPath = join(ROOT, "fidelity-corpus/legacy-goldens/manifest.json");
  const report = join(ROOT, "fidelity-corpus/legacy-goldens/comparison-report.md");
  assert.ok(existsSync(runner), "native pdfium semantic runner is present");
  assert.ok(existsSync(officeRunner), "office legacy-golden runner is present");
  assert.ok(existsSync(redactionRunner), "redaction legacy-golden runner is present");
  assert.ok(existsSync(allowlistPath), "typed parity allowlist is present");
  assert.ok(existsSync(report), "comparison report is present");

  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const allowlist = JSON.parse(readFileSync(allowlistPath, "utf8"));
  const evidence = readFileSync(report, "utf8");
  assert.equal(manifest.goldens.length, 34, "frozen manifest row-count drift");
  assert.equal(allowlist.schema_version, 2, "typed disposition schema drift");
  assert.ok(Array.isArray(allowlist.allowlisted), "allowlisted[] is required");
  assert.ok(Array.isArray(allowlist.known_failures), "known_failures[] is required");

  const manifestIds = new Set(manifest.goldens.map(({ golden_path }) => fixtureId(golden_path)));
  const dispositionKeys = new Set();
  for (const entry of allowlist.allowlisted) {
    validateDisposition(entry, "allowlisted");
    assert.ok(manifestIds.has(entry.fixture_id), `allowlisted fixture is absent: ${entry.fixture_id}`);
    const key = `${entry.fixture_id}\0${entry.reason_code}`;
    assert.ok(!dispositionKeys.has(key), `duplicate disposition: ${key}`);
    dispositionKeys.add(key);
  }
  for (const entry of allowlist.known_failures) {
    validateDisposition(entry, "known-failure");
    assert.ok(manifestIds.has(entry.fixture_id), `known-failure fixture is absent: ${entry.fixture_id}`);
    const key = `${entry.fixture_id}\0${entry.reason_code}`;
    assert.ok(!dispositionKeys.has(key), `duplicate disposition: ${key}`);
    dispositionKeys.add(key);
  }

  for (const row of manifest.goldens) {
    assert.match(row.input_sha256, /^[0-9a-f]{64}$/, `${row.input_path} input digest`);
    assert.match(row.golden_sha256, /^[0-9a-f]{64}$/, `${row.golden_path} golden digest`);
    assertFrozenDigest(row.input_path, row.input_sha256);
    assertFrozenDigest(row.golden_path, row.golden_sha256);
    const id = fixtureId(row.golden_path);
    assert.ok(evidence.includes(id), `comparison report covers ${id}`);
  }

  const redIds = new Set(allowlist.known_failures.map(({ fixture_id }) => fixture_id));
  const allAllowlistedIds = new Set(
    allowlist.allowlisted.map(({ fixture_id }) => fixture_id),
  );
  const allowlistedIds = new Set(
    [...allAllowlistedIds].filter((id) => !redIds.has(id)),
  );
  const greenRows = manifest.goldens.length - redIds.size - allowlistedIds.size;

  const auditRows = new Map(
    evidence
      .split("\n")
      .filter((line) => /^\|\s*\d+\s*\|/.test(line))
      .map((line) => {
        const cells = line.split("|").slice(1, -1).map((cell) => cell.trim());
        assert.equal(cells.length, 6, `malformed disposition row: ${line}`);
        return [cells[1], cells];
      }),
  );
  assert.equal(auditRows.size, manifest.goldens.length, "34-row disposition table drift");
  for (const id of manifestIds) {
    const cells = auditRows.get(id);
    assert.ok(cells, `disposition table lacks ${id}`);
    const greenMarked = cells[2] === "✓";
    const allowlistedMarked = cells[3].length > 0;
    const redMarked = cells[4].length > 0;
    assert.equal(redMarked, redIds.has(id), `${id} red classification drift`);
    assert.equal(
      allowlistedMarked,
      allAllowlistedIds.has(id),
      `${id} allowlisted classification drift`,
    );
    assert.equal(
      greenMarked,
      !redIds.has(id) && !allAllowlistedIds.has(id),
      `${id} green classification drift`,
    );
  }

  assert.match(evidence, new RegExp(`Green rows \\| ${greenRows}(?: \\||$)`));
  assert.match(
    evidence,
    new RegExp(`Allowlisted rows \\| ${allowlistedIds.size}(?: \\||$)`),
  );
  assert.match(
    evidence,
    new RegExp(`Red rows \\| ${redIds.size}(?: \\||$)`),
  );
});

test("long Markdown page count is derived from and compared with the frozen golden", () => {
  const manifest = JSON.parse(
    readFileSync(join(ROOT, "fidelity-corpus/legacy-goldens/manifest.json"), "utf8"),
  );
  const registry = JSON.parse(
    readFileSync(join(ROOT, "fidelity-corpus/legacy-parity-allowlist.json"), "utf8"),
  );
  const row = manifest.goldens.find(
    ({ golden_path }) => fixtureId(golden_path) === "markdown/long-pagination",
  );
  assert.ok(row, "long-pagination manifest row is required");
  const input = assertFrozenDigest(row.input_path, row.input_sha256);
  const golden = assertFrozenDigest(row.golden_path, row.golden_sha256);

  const current = pdfCore.markdown_to_pdf(input.toString("utf8"));
  const currentPages = pdfCore.page_count(current);
  const goldenPages = pdfCore.page_count(golden);
  const tracked = registry.known_failures.find(
    (entry) =>
      entry.fixture_id === "markdown/long-pagination"
      && entry.reason_code === "NODE_MD_PAGE_COUNT",
  );
  if (currentPages === goldenPages) {
    assert.equal(tracked, undefined, "resolved long-pagination page-count red is still tracked");
  } else {
    assert.ok(tracked, `untracked page-count red: golden=${goldenPages} current=${currentPages}`);
    process.stderr.write(
      `RED TRACKED markdown/long-pagination NODE_MD_PAGE_COUNT: golden=${goldenPages} current=${currentPages}\n`,
    );
  }
});
