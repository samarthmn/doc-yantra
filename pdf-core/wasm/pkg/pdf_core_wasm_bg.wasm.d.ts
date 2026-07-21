/* tslint:disable */
/* eslint-disable */
export const memory: WebAssembly.Memory;
export const version: () => [number, number];
export const register_font: (a: number, b: number) => [number, number];
export const merge_pdfs: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
export const apply_watermark: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
export const redact_pdf: (a: number, b: number, c: number, d: number) => [number, number, number, number];
export const redact_pdf_with_report: (a: number, b: number, c: number, d: number) => [number, number, number];
export const csv_to_pdf: (a: number, b: number) => [number, number, number, number];
export const markdown_to_pdf: (a: number, b: number) => [number, number, number, number];
export const text_to_pdf: (a: number, b: number) => [number, number, number, number];
export const page_count: (a: number, b: number) => [number, number, number];
export const validate_pdf: (a: number, b: number) => [number, number, number, number];
export const init: () => void;
export const __wbindgen_free: (a: number, b: number, c: number) => void;
export const __wbindgen_exn_store: (a: number) => void;
export const __externref_table_alloc: () => number;
export const __wbindgen_externrefs: WebAssembly.Table;
export const __wbindgen_malloc: (a: number, b: number) => number;
export const __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
export const __externref_table_dealloc: (a: number) => void;
export const __wbindgen_start: () => void;
