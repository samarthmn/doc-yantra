#!/usr/bin/env node
// LLM-based fidelity judge (dev-only).
//
// For each selected fidelity-corpus DOCX case: converts source.docx through
// the BUILT office wasm package (repo-registered fonts only — the same wiring
// DocKosha ships), renders both the Word ground truth and our output with
// pdftoppm, and asks the codex CLI (gpt-5.6-sol, medium effort, read-only
// sandbox) to score layout fidelity and suggest fixes. The judge only ever
// reads: it cannot change code, and the prompt scopes it to grading.
//
// Usage:
//   node scripts/llm-fidelity.mjs               # default representative subset
//   node scripts/llm-fidelity.mjs complex japanese
//
// Output: tmp/llm-fidelity/<case>.json + tmp/llm-fidelity/report.json and a
// summary table on stdout. Token-lean by design: low-DPI renders, capped page
// count, terse prompt, structured output schema.

import { execFileSync, spawnSync } from "node:child_process";
import {
  mkdirSync,
  readFileSync,
  readdirSync,
  writeFileSync,
  rmSync,
  existsSync,
} from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const ROOT = join(here, "..");
const CORPUS = join(ROOT, "fidelity-corpus/docx");
const OUT = join(ROOT, "tmp/llm-fidelity");
const RENDER_DPI = 96; // ~816x1056 px per US-Letter page: fine layout detail, still cheap
const MAX_PAGES = 4; // per case, both sides; overflow is disclosed to the judge

const DEFAULT_CASES = [
  "complex",
  "table-fixed",
  "table-merges",
  "lists-multilevel",
  "justify-long-text",
  "japanese",
  "chinese",
  "korean",
];

// Strict structured-output rules: every property must be required and only a
// conservative schema subset is accepted (no maxItems); list limits live in
// the prompt instead.
const SCHEMA = {
  type: "object",
  additionalProperties: false,
  required: ["score", "page_scores", "reasons", "fixes"],
  properties: {
    score: { type: "integer" },
    page_scores: { type: "array", items: { type: "integer" } },
    reasons: { type: "array", items: { type: "string" } },
    fixes: { type: "array", items: { type: "string" } },
  },
};

function loadEngine() {
  const engine = require(join(ROOT, "office-core/wasm/pkg"));
  const latin = join(ROOT, "office-core/fonts");
  for (const directory of ["carlito", "caladea", "liberation"]) {
    for (const file of readdirSync(join(latin, directory))) {
      if (file.endsWith(".ttf")) {
        engine.office_register_font(readFileSync(join(latin, directory, file)));
      }
    }
  }
  const cjk = join(ROOT, "packages/doc-yantra-fonts/fonts");
  for (const file of [
    "NotoSansCJKsc-Regular.otf",
    "NotoSansCJKtc-Regular.otf",
    "NotoSansCJKjp-Regular.otf",
    "NotoSansCJKkr-Regular.otf",
  ]) {
    engine.office_register_font(readFileSync(join(cjk, file)));
  }
  return engine;
}

function renderPages(pdfPath, prefix) {
  execFileSync("pdftoppm", [
    "-png",
    "-r",
    String(RENDER_DPI),
    "-l",
    String(MAX_PAGES),
    pdfPath,
    prefix,
  ]);
  return readdirSync(dirname(prefix))
    .filter((f) => f.startsWith(prefix.split("/").pop() + "-"))
    .sort()
    .map((f) => join(dirname(prefix), f));
}

function judgeCase(engine, name) {
  const dir = join(CORPUS, name);
  const truthPdf = join(dir, "ground-truth.pdf");
  const source = readFileSync(join(dir, "source.docx"));
  const caseOut = join(OUT, name);
  rmSync(caseOut, { recursive: true, force: true });
  mkdirSync(caseOut, { recursive: true });

  let ours;
  try {
    ours = engine.office_to_pdf_with_options(
      source,
      "docx",
      JSON.stringify({ mode: "compatible" }),
    );
  } catch (error) {
    return { case: name, error: `convert failed: ${error}` };
  }
  const oursPdf = join(caseOut, "ours.pdf");
  writeFileSync(oursPdf, Buffer.from(ours));

  const truthPages = renderPages(truthPdf, join(caseOut, "truth"));
  const ourPages = renderPages(oursPdf, join(caseOut, "ours"));
  const pages = Math.min(truthPages.length, ourPages.length, MAX_PAGES);
  const truthTotal = Number(
    execFileSync("pdfinfo", [truthPdf]).toString().match(/Pages:\s+(\d+)/)?.[1] ?? pages,
  );

  const images = [];
  for (let i = 0; i < pages; i++) {
    images.push("-i", truthPages[i], "-i", ourPages[i]);
  }
  const truncated =
    truthTotal > pages ? ` Only pages 1-${pages} of ${truthTotal} are shown.` : "";
  const prompt =
    `Grade PDF layout fidelity. Images alternate per page: Word ground truth, then our converter's output (T1,O1,T2,O2,...).${truncated} ` +
    `Score layout parity per page: start at 100, subtract for every visible deviation (pagination, line/page breaks, word wrap, spacing, indentation, alignment, table geometry, rule/border placement, image position+size, missing or extra content). ` +
    `Compare carefully — magnify mentally, check line endings and column edges; award 100 only when you cannot find any layout difference. Typical good conversions land 85-97. ` +
    `Metric-compatible font substitutes are expected (Calibri->Carlito, MS Gothic/Batang/SimSun->Noto Sans CJK, etc.) — ignore letterform/hinting/antialiasing differences, judge geometry only. ` +
    `You are a grader only: read nothing else, change nothing. page_scores: one 0-100 entry per shown page; score = min of page_scores. Reasons: at most 5 concrete visual defects with page numbers, most severe first. Fixes: at most 5 concise converter-side suggestions.`;

  const schemaPath = join(caseOut, "schema.json");
  writeFileSync(schemaPath, JSON.stringify(SCHEMA));
  const outPath = join(caseOut, "verdict.json");
  const result = spawnSync(
    "codex",
    [
      "exec",
      "--sandbox",
      "read-only",
      "-m",
      "gpt-5.6-sol",
      "-c",
      "model_reasoning_effort=xhigh",
      "--output-schema",
      schemaPath,
      "-o",
      outPath,
      ...images,
    ],
    // The prompt travels via stdin: codex's variadic -i would otherwise
    // swallow a trailing positional argument as another image path.
    { encoding: "utf8", timeout: 600_000, input: prompt },
  );
  if (result.status !== 0) {
    return {
      case: name,
      error: `codex exec failed (${result.status}): ${(result.stderr || "").slice(-400)}`,
    };
  }
  const verdict = JSON.parse(readFileSync(outPath, "utf8"));
  return { case: name, pages_judged: pages, truth_pages: truthTotal, ...verdict };
}

const cases = process.argv.slice(2).length ? process.argv.slice(2) : DEFAULT_CASES;
mkdirSync(OUT, { recursive: true });
const engine = loadEngine();
const results = [];
for (const name of cases) {
  if (!existsSync(join(CORPUS, name, "ground-truth.pdf"))) {
    results.push({ case: name, error: "no ground truth" });
    continue;
  }
  process.stderr.write(`judging ${name} ...\n`);
  const verdict = judgeCase(engine, name);
  results.push(verdict);
  writeFileSync(join(OUT, `${name}.json`), JSON.stringify(verdict, null, 2));
}
writeFileSync(join(OUT, "report.json"), JSON.stringify(results, null, 2));

const scored = results.filter((r) => typeof r.score === "number");
for (const r of results) {
  if (r.error) {
    console.log(`${r.case.padEnd(20)} ERROR ${r.error}`);
  } else {
    console.log(
      `${r.case.padEnd(20)} ${String(r.score).padStart(3)}  ${r.reasons?.[0] ?? ""}`,
    );
  }
}
if (scored.length) {
  const avg = scored.reduce((a, r) => a + r.score, 0) / scored.length;
  console.log(`${"AVERAGE".padEnd(20)} ${avg.toFixed(1)}  (${scored.length} cases)`);
}
