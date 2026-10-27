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
 * Permanently redacts service-compatible percentage areas from a PDF.
 *
 * `areas_json` must be an array of
 * `{pageIndex,left,top,width,height}` objects measured on displayed pages.
 */
export function redact_pdf(pdf: Uint8Array, areas_json: string): Uint8Array;

/**
 * Permanently redacts service-compatible percentage areas and returns the
 * complete core report, including conservative whole-object removal notices.
 * This additive export preserves `redact_pdf` compatibility for callers that
 * only consume rewritten PDF bytes.
 */
export function redact_pdf_with_report(pdf: Uint8Array, areas_json: string): any;

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

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly version: () => [number, number];
    readonly register_font: (a: number, b: number) => [number, number];
    readonly merge_pdfs: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly apply_watermark: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly redact_pdf: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly redact_pdf_with_report: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly csv_to_pdf: (a: number, b: number) => [number, number, number, number];
    readonly markdown_to_pdf: (a: number, b: number) => [number, number, number, number];
    readonly text_to_pdf: (a: number, b: number) => [number, number, number, number];
    readonly page_count: (a: number, b: number) => [number, number, number];
    readonly validate_pdf: (a: number, b: number) => [number, number, number, number];
    readonly init: () => void;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
