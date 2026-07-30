/**
 * Vendored from DocKosha packages/provider-interface/src/index.ts.
 *
 * DocKosha is the source of truth while the interface package is local-only.
 * Before publishing this adapter, replace this vendored file with the
 * published @dockosha/provider-interface dependency and import its types.
 */

export const PROVIDER_ERROR_CODES = [
  "invalid_input",
  "unsupported_format",
  "unsupported_feature",
  "missing_glyph",
  "missing_asset",
  "password_protected",
  "malformed_container",
  "resource_limit",
  "invalid_output",
  "internal_error",
  "deadline_exceeded",
  "queue_busy",
  "worker_boot_failed",
  "engine_unavailable",
  "trap",
  "protocol_error",
] as const;

export type ProviderErrorCode = (typeof PROVIDER_ERROR_CODES)[number];

export const PROVIDER_OPERATIONS = [
  "merge",
  "watermark",
  "csv",
  "markdown",
  "redaction",
  "page_count",
  "conversion",
  "office_conversion",
  "probe",
] as const;

export type ProviderOperation = (typeof PROVIDER_OPERATIONS)[number];

export type ProviderErrorContext =
  | null
  | boolean
  | number
  | string
  | ProviderErrorContext[]
  | { [key: string]: ProviderErrorContext };

export type ProviderErrorDetail =
  | string
  | {
      reason?: string;
      context?: ProviderErrorContext;
      [key: string]: unknown;
    };

export interface ProviderFailure {
  code: ProviderErrorCode;
  message: string;
  operation: ProviderOperation;
  format?: string;
  retryable: boolean;
  detail?: ProviderErrorDetail;
}

export type ProviderFailureResult = ProviderFailure & { ok: false };

export type PdfResult = { ok: true; pdf: ArrayBuffer } | ProviderFailureResult;

export type OfficePdfResult =
  { ok: true; pdf: ArrayBuffer; ms: number } | ProviderFailureResult;

export type RedactionWarning =
  | string
  | {
      code: string;
      page: number;
      kind?: string;
      message?: string;
    };

export type RedactionResult =
  | {
      ok: true;
      pdf: ArrayBuffer;
      pageCount: number;
      warnings: RedactionWarning[];
    }
  | ProviderFailureResult;

export type PageCountResult =
  { ok: true; pageCount: number } | ProviderFailureResult;

export type ProbeResult =
  | { ok: true; bootDurationMs: number }
  | (ProviderFailureResult & { bootDurationMs: number });

export type ProviderBinary = ArrayBuffer | Uint8Array;

export interface MergeAndWatermarkOptions {
  pdfs: ProviderBinary[];
  watermarkPayloadJson?: string | null;
  watermarkImageBytes?: ProviderBinary | null;
  timeoutMs?: number;
}

export interface DocumentProcessingProvider {
  readonly id: string;
  readonly capabilities: ReadonlySet<ProviderOperation>;
  mergeAndWatermark(options: MergeAndWatermarkOptions): Promise<PdfResult>;
  pageCount(pdf: ProviderBinary, timeoutMs?: number): Promise<PageCountResult>;
  csvToPdf?(csvBytes: ProviderBinary, timeoutMs?: number): Promise<PdfResult>;
  markdownToPdf?(markdown: string, timeoutMs?: number): Promise<PdfResult>;
  officeToPdf?(
    bytes: ProviderBinary,
    extension: string,
    timeoutMs?: number,
  ): Promise<OfficePdfResult>;
  redactPdfWithReport?(
    pdf: ProviderBinary,
    areasJson: string,
    timeoutMs?: number,
  ): Promise<RedactionResult>;
  probe?(timeoutMs?: number): Promise<ProbeResult>;
}
