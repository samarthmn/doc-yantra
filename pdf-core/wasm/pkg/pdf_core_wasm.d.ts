/* tslint:disable */
/* eslint-disable */

/**
 * Applies a DocYantra-payload watermark to a single PDF.
 */
export function apply_watermark(pdf: Uint8Array, watermark_json: string, watermark_image?: Uint8Array | null): Uint8Array;

/**
 * CSV bytes → paginated PDF table (replaces the csv→HTML→Chromium route,
 * which forced landscape:'true' — landscape Letter matches that output).
 */
export function csv_to_pdf(csv: Uint8Array): Uint8Array;

export function init(): void;

/**
 * Markdown → PDF (replaces the md→HTML→Chromium route).
 */
export function markdown_to_pdf(markdown: string): Uint8Array;

/**
 * Merges one or more PDFs and optionally applies a watermark — the local
 * equivalent of DocYantra's `POST /docyantra/pdf/merge`. `watermark_json`
 * is the same JSON string the app puts in the multipart `watermark` field;
 * `watermark_image` is the optional `watermarkImage` file's bytes.
 */
export function merge_pdfs(pdfs: Uint8Array[], watermark_json?: string | null, watermark_image?: Uint8Array | null): Uint8Array;

/**
 * Page count of a PDF (errors on invalid/encrypted documents).
 */
export function page_count(pdf: Uint8Array): number;

/**
 * Adds an embeddable font to the process-global registry. The generated JS
 * glue applies the same limit before copying `bytes` into wasm linear memory;
 * this Rust check is defense in depth for direct wasm callers.
 */
export function register_font(bytes: Uint8Array): void;

/**
 * Plain text → PDF.
 */
export function text_to_pdf(text: string): Uint8Array;

/**
 * Structural validation; returns `{"pageCount":n,"version":"1.7","encrypted":false}`.
 */
export function validate_pdf(pdf: Uint8Array): string;

/**
 * Engine version, for logging/diagnostics on the JS side.
 */
export function version(): string;
