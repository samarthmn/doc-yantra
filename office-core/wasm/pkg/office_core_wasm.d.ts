/* tslint:disable */
/* eslint-disable */

export function init(): void;

/**
 * The DocProfile JSON for a document. Never throws for well-formed calls;
 * container-level failures serialize via `DocProfile::from_error`. Oversized
 * input returns the `input_too_large` profile. The generated JS package
 * performs this check before copying the input into wasm memory.
 */
export function office_profile(bytes: Uint8Array, extension: string): string;

/**
 * PDF bytes for a document. Throws a `JsError` whose message begins with
 * `"unsupported:<code>: "` for envelope misses (DocKosha parses this prefix),
 * `"invalid: …"` / `"internal: …"` otherwise. Same 50 MiB pre-copy guard.
 */
export function office_to_pdf(bytes: Uint8Array, extension: string): Uint8Array;

/**
 * Adds a font to the process-global font database (brand fonts). Errors on
 * unparseable font bytes.
 */
export function register_font(bytes: Uint8Array): void;

/**
 * The engine crate version.
 */
export function version(): string;
