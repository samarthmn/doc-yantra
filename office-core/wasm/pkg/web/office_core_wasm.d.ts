/* tslint:disable */
/* eslint-disable */

export function init(): void;

/**
 * Text-free canonical font requests for host resolution. Exact registered and
 * document-embedded faces are omitted.
 */
export function office_font_requests(bytes: Uint8Array, extension: string): string;

/**
 * The DocProfile JSON for a document. Never throws for well-formed calls;
 * container-level failures serialize via `DocProfile::from_error`. Oversized
 * input returns the `input_too_large` profile. The generated JS package
 * performs this check before copying the input into wasm memory.
 */
export function office_profile(bytes: Uint8Array, extension: string): string;

/**
 * Font-aware profile using JSON options (`{"mode":"strict|compatible"}`).
 */
export function office_profile_with_options(bytes: Uint8Array, extension: string, options_json: string): string;

/**
 * Validates and registers one font, returning its reproducible identity JSON.
 */
export function office_register_font(bytes: Uint8Array, expectation_json?: string | null): string;

/**
 * PDF bytes for a document. Throws a `JsError` whose message begins with
 * `"unsupported:<code>: "` for envelope misses (DocKosha parses this prefix),
 * `"invalid: …"` / `"internal: …"` otherwise. Same 50 MiB pre-copy guard.
 */
export function office_to_pdf(bytes: Uint8Array, extension: string): Uint8Array;

/**
 * Font-aware conversion using JSON options (`{"mode":"strict|compatible"}`).
 */
export function office_to_pdf_with_options(bytes: Uint8Array, extension: string, options_json: string): Uint8Array;

/**
 * Adds a font to the process-global font database (brand fonts). Errors on
 * unparseable font bytes.
 */
export function register_font(bytes: Uint8Array): void;

/**
 * The engine crate version.
 */
export function version(): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly office_font_requests: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly office_profile: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly office_profile_with_options: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly office_register_font: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly office_to_pdf: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly office_to_pdf_with_options: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly register_font: (a: number, b: number) => [number, number];
    readonly version: () => [number, number];
    readonly init: () => void;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
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
