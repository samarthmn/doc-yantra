// PDF.js canvas paint probe for regression tests.
//
// Loads a PDF with pdfjs-dist (the exact version DocKosha ships, pinned in
// tests/package.json), renders a page to an @napi-rs/canvas surface with
// disableFontFace (glyphs rasterize through pdf.js's own font-program
// pipeline, not the OS font stack), and samples the device-space bounding box
// of each extracted text item for non-background "ink" pixels. A viewer that
// extracts text but compiles empty glyph paths reports inkRatio 0 here.
//
// Hermetic: no network, no system fonts; everything resolves from
// tests/node_modules and the PDF bytes under test.

import { createRequire } from "node:module";
import path from "node:path";

const require = createRequire(import.meta.url);

const SCALE = 2;
const INK_CHANNEL_THRESHOLD = 230; // any RGB channel below this => ink pixel

const CJK_RANGES = [
  [0x3000, 0x303f], // CJK Symbols and Punctuation
  [0x3040, 0x309f], // Hiragana
  [0x30a0, 0x30ff], // Katakana
  [0x4e00, 0x9fff], // CJK Unified Ideographs
  [0xac00, 0xd7af], // Hangul Syllables
];

export function hasCjk(str) {
  for (const ch of str) {
    const cp = ch.codePointAt(0);
    for (const [lo, hi] of CJK_RANGES) {
      if (cp >= lo && cp <= hi) return true;
    }
  }
  return false;
}

export function median(values) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1
    ? sorted[mid]
    : (sorted[mid - 1] + sorted[mid]) / 2;
}

/** Loads pdfjs + canvas, returning null when the optional deps are absent. */
export function loadPdfjsStack() {
  let napi;
  let pdfjsLib;
  try {
    napi = require("@napi-rs/canvas");
    if (napi.DOMMatrix && !globalThis.DOMMatrix) globalThis.DOMMatrix = napi.DOMMatrix;
    if (napi.Path2D && !globalThis.Path2D) globalThis.Path2D = napi.Path2D;
    if (napi.ImageData && !globalThis.ImageData) globalThis.ImageData = napi.ImageData;
    pdfjsLib = require("pdfjs-dist/legacy/build/pdf.js");
  } catch {
    return null;
  }
  pdfjsLib.GlobalWorkerOptions.workerSrc = require.resolve(
    "pdfjs-dist/legacy/build/pdf.worker.js",
  );
  return { napi, pdfjsLib };
}

class NapiCanvasFactory {
  constructor(createCanvas) {
    this.createCanvas = createCanvas;
  }
  create(width, height) {
    if (width <= 0 || height <= 0) throw new Error("Invalid canvas size");
    const canvas = this.createCanvas(width, height);
    return { canvas, context: canvas.getContext("2d") };
  }
  reset(canvasAndContext, width, height) {
    canvasAndContext.canvas.width = width;
    canvasAndContext.canvas.height = height;
  }
  destroy(canvasAndContext) {
    canvasAndContext.canvas.width = 0;
    canvasAndContext.canvas.height = 0;
    canvasAndContext.canvas = null;
    canvasAndContext.context = null;
  }
}

function deviceBBox(pdfjsLib, item, viewport) {
  const tx = pdfjsLib.Util.transform(viewport.transform, item.transform);
  const fontHeight = Math.hypot(tx[2], tx[3]);
  return {
    x: tx[4],
    y: tx[5] - fontHeight,
    width: item.width * viewport.scale,
    height: fontHeight * 1.25, // extend below the baseline for descenders
  };
}

function sampleInkRatio(context, canvas, bbox) {
  const x0 = Math.max(0, Math.floor(bbox.x));
  const y0 = Math.max(0, Math.floor(bbox.y));
  const x1 = Math.min(canvas.width, Math.ceil(bbox.x + bbox.width));
  const y1 = Math.min(canvas.height, Math.ceil(bbox.y + bbox.height));
  const w = x1 - x0;
  const h = y1 - y0;
  if (w <= 0 || h <= 0) return 0;
  const data = context.getImageData(x0, y0, w, h).data;
  let ink = 0;
  for (let i = 0; i < data.length; i += 4) {
    if (
      data[i] < INK_CHANNEL_THRESHOLD ||
      data[i + 1] < INK_CHANNEL_THRESHOLD ||
      data[i + 2] < INK_CHANNEL_THRESHOLD
    ) {
      ink += 1;
    }
  }
  return ink / (w * h);
}

/**
 * Renders one page and measures per-text-item ink.
 *
 * Returns { version, pageInkRatio, items: [{ str, cjk, inkRatio }], consoleMessages }.
 * `items` covers every non-empty extracted text item on the page.
 */
export async function paintProbe(stack, pdfBytes, pageNumber) {
  const { napi, pdfjsLib } = stack;
  const consoleMessages = [];
  const originals = {};
  for (const level of ["log", "warn", "error"]) {
    originals[level] = console[level];
    console[level] = (...args) => {
      consoleMessages.push(
        `[${level}] ${args.map((a) => (typeof a === "string" ? a : String(a))).join(" ")}`,
      );
    };
  }
  try {
    const distRoot = path.dirname(require.resolve("pdfjs-dist/package.json"));
    const canvasFactory = new NapiCanvasFactory(napi.createCanvas);
    const loadingTask = pdfjsLib.getDocument({
      data: new Uint8Array(pdfBytes),
      canvasFactory,
      standardFontDataUrl: path.join(distRoot, "standard_fonts") + path.sep,
      cMapUrl: path.join(distRoot, "cmaps") + path.sep,
      cMapPacked: true,
      useSystemFonts: false,
      disableFontFace: true,
      verbosity: pdfjsLib.VerbosityLevel.WARNINGS,
    });
    const doc = await loadingTask.promise;
    try {
      const page = await doc.getPage(pageNumber);
      const viewport = page.getViewport({ scale: SCALE });
      const { canvas, context } = canvasFactory.create(
        Math.ceil(viewport.width),
        Math.ceil(viewport.height),
      );
      await page.render({ canvasContext: context, viewport, canvasFactory })
        .promise;

      const textContent = await page.getTextContent();
      const items = textContent.items
        .filter(
          (it) =>
            typeof it.str === "string" &&
            it.str.trim().length > 0 &&
            it.width > 0,
        )
        .map((it) => ({
          str: it.str,
          cjk: hasCjk(it.str),
          inkRatio: sampleInkRatio(
            context,
            canvas,
            deviceBBox(pdfjsLib, it, viewport),
          ),
        }));
      const pageInkRatio = sampleInkRatio(context, canvas, {
        x: 0,
        y: 0,
        width: canvas.width,
        height: canvas.height,
      });
      return {
        version: pdfjsLib.version,
        pageInkRatio,
        items,
        consoleMessages,
      };
    } finally {
      await doc.destroy();
    }
  } finally {
    for (const level of ["log", "warn", "error"]) {
      console[level] = originals[level];
    }
  }
}
