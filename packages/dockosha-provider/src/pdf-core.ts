import { createRequire } from "node:module";
import { performance } from "node:perf_hooks";
import { z } from "zod";
import {
  DOCUMENT_CONVERSION_MAX_INPUT_BYTES,
  DOCUMENT_CONVERSION_TIMEOUT_MS,
  ENGINE_BOOT_PROBE_TIMEOUT_MS,
  PDF_MERGE_MAX_TOTAL_BYTES,
  PDF_PROCESSING_MAX_INPUT_BYTES,
} from "./constants.js";
import {
  createEngineFailure,
  normalizeEngineError,
  toFailureResult,
  type EngineOperation,
} from "./engine-errors.js";
import type {
  MergeAndWatermarkOptions,
  PageCountResult,
  PdfResult,
  ProbeResult,
  ProviderBinary,
  ProviderFailure,
  RedactionResult,
} from "./provider-interface.js";

type PdfWorkerClient = {
  mergePdfs(input: {
    pdfs: ProviderBinary[];
    watermarkJson?: string;
    watermarkImage?: ProviderBinary;
    timeoutMs?: number;
  }): Promise<unknown>;
  csvToPdf(input: {
    csv: ProviderBinary;
    timeoutMs?: number;
  }): Promise<unknown>;
  markdownToPdf(input: {
    markdown: string;
    timeoutMs?: number;
  }): Promise<unknown>;
  textToPdf(input: {
    text: string;
    timeoutMs?: number;
  }): Promise<unknown>;
  redactPdfWithReport(input: {
    pdf: ProviderBinary;
    areasJson: string;
    timeoutMs?: number;
  }): Promise<unknown>;
  pageCount(input: {
    pdf: ProviderBinary;
    timeoutMs?: number;
  }): Promise<unknown>;
  engineVersion(): Promise<unknown>;
  close(): Promise<void>;
};

type PdfWorkerModule = {
  createPdfEngineWorker(options?: {
    defaultTimeoutMs?: number;
  }): PdfWorkerClient;
};

const isRecord = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);

const isPdfWorkerModule = (value: unknown): value is PdfWorkerModule =>
  isRecord(value) && typeof value.createPdfEngineWorker === "function";

const binaryOutputSchema = z.instanceof(Uint8Array);
const engineVersionSchema = z.string().min(1);
const redactionReportSchema = z.object({
  pdf: z.instanceof(Uint8Array),
  pageCount: z.number().int().positive(),
  warnings: z
    .array(
      z.union([
        z.string().max(512),
        z.object({
          code: z.string().trim().min(1).max(64),
          page: z.number().int().nonnegative(),
          kind: z.string().trim().min(1).max(64).optional(),
          message: z.string().max(512).optional(),
        }),
      ]),
    )
    .max(1_024),
});

const requireCjs = createRequire(__filename);
const PDF_ENGINE_UNAVAILABLE_MESSAGE =
  "The PDF engine is temporarily unavailable.";
let worker: PdfWorkerClient | null = null;
let createWorker: ((defaultTimeoutMs?: number) => PdfWorkerClient) | null =
  null;
let startupFailure: ProviderFailure | null = null;
let hasReportedVersion = false;
let testIdleClose: ReturnType<typeof setTimeout> | null = null;

const unknownDetail = (error: unknown): string | undefined =>
  error instanceof Error
    ? error.message
    : typeof error === "string" && error.length > 0
      ? error
      : undefined;

try {
  const loaded: unknown = requireCjs("@samarthmn/doc-yantra/worker");
  if (!isPdfWorkerModule(loaded)) {
    throw new Error("PDF worker entrypoint has an invalid export contract");
  }
  createWorker = (defaultTimeoutMs) =>
    loaded.createPdfEngineWorker(
      defaultTimeoutMs === undefined ? {} : { defaultTimeoutMs },
    );
} catch (error) {
  startupFailure = createEngineFailure({
    code: "worker_boot_failed",
    message: PDF_ENGINE_UNAVAILABLE_MESSAGE,
    operation: "probe",
    format: "pdf",
    detail: unknownDetail(error),
  });
}

const scheduleTestIdleClose = (): void => {
  if (process.env.NODE_TEST_CONTEXT === undefined || !worker) return;
  if (testIdleClose) clearTimeout(testIdleClose);
  testIdleClose = setTimeout(() => {
    const current = worker;
    worker = null;
    hasReportedVersion = false;
    testIdleClose = null;
    void current?.close();
  }, 100);
  testIdleClose.unref();
};

const contextualFailure = (
  failure: ProviderFailure,
  operation: EngineOperation,
  format: string,
): ProviderFailure =>
  createEngineFailure({
    code: failure.code,
    message: failure.message,
    operation,
    format,
    detail: failure.detail,
  });

type WorkerResult =
  | { ok: true; client: PdfWorkerClient }
  | (ProviderFailure & { ok: false });

const getWorker = (
  operation: EngineOperation,
  format: string,
  defaultTimeoutMs?: number,
): WorkerResult => {
  if (testIdleClose) {
    clearTimeout(testIdleClose);
    testIdleClose = null;
  }
  if (worker) return { ok: true, client: worker };
  if (!createWorker) {
    return toFailureResult(
      contextualFailure(
        startupFailure ??
          createEngineFailure({
            code: "engine_unavailable",
            message: PDF_ENGINE_UNAVAILABLE_MESSAGE,
            operation,
            format,
          }),
        operation,
        format,
      ),
    );
  }

  try {
    worker = createWorker(defaultTimeoutMs);
    return { ok: true, client: worker };
  } catch (error) {
    return toFailureResult(
      createEngineFailure({
        code: "worker_boot_failed",
        message: PDF_ENGINE_UNAVAILABLE_MESSAGE,
        operation,
        format,
        detail: unknownDetail(error),
      }),
    );
  }
};

const recordLoadedVersion = async (client: PdfWorkerClient): Promise<void> => {
  if (hasReportedVersion) {
    scheduleTestIdleClose();
    return;
  }
  const version = engineVersionSchema.parse(await client.engineVersion());
  hasReportedVersion = true;
  console.info(`[pdf-core-wasm] worker loaded (version ${version})`);
  scheduleTestIdleClose();
};

const normalizeFailure = (
  error: unknown,
  context: {
    operation: EngineOperation;
    format: string;
    fallbackMessage: string;
  },
): ProviderFailure & { ok: false } => {
  scheduleTestIdleClose();
  return toFailureResult(normalizeEngineError(error, context));
};

const immediateDeadline = (
  operation: EngineOperation,
  format: string,
  timeoutMs: number | undefined,
  message: string,
): (ProviderFailure & { ok: false }) | null => {
  if (timeoutMs === undefined) return null;
  if (!Number.isFinite(timeoutMs)) {
    return toFailureResult(
      createEngineFailure({
        code: "invalid_input",
        message: "The PDF engine timeout must be a finite number.",
        operation,
        format,
      }),
    );
  }
  if (timeoutMs > 0) return null;
  return toFailureResult(
    createEngineFailure({
      code: "deadline_exceeded",
      message,
      operation,
      format,
    }),
  );
};

const toUint8 = (bytes: ProviderBinary): Uint8Array =>
  bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);

const toArrayBuffer = (bytes: Uint8Array): ArrayBuffer => {
  const buffer = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(buffer).set(bytes);
  return buffer;
};

export const mergeAndWatermark = async (
  options: MergeAndWatermarkOptions,
): Promise<PdfResult> => {
  const operation: EngineOperation = options.watermarkPayloadJson
    ? "watermark"
    : "merge";
  const totalBytes =
    options.pdfs.reduce((sum, pdf) => sum + pdf.byteLength, 0) +
    (options.watermarkImageBytes?.byteLength ?? 0);
  if (totalBytes > PDF_MERGE_MAX_TOTAL_BYTES) {
    return toFailureResult(
      createEngineFailure({
        code: "resource_limit",
        message: `Merge input (${totalBytes} bytes) exceeds the ${PDF_MERGE_MAX_TOTAL_BYTES}-byte processing limit.`,
        operation,
        format: "pdf",
      }),
    );
  }
  const deadline = immediateDeadline(
    operation,
    "pdf",
    options.timeoutMs,
    "PDF merge deadline exceeded.",
  );
  if (deadline) return deadline;
  const clientResult = getWorker(operation, "pdf");
  if (!clientResult.ok) return clientResult;

  try {
    const parsed = binaryOutputSchema.safeParse(
      await clientResult.client.mergePdfs({
        pdfs: options.pdfs.map(toUint8),
        watermarkJson: options.watermarkPayloadJson ?? undefined,
        watermarkImage: options.watermarkImageBytes ?? undefined,
        timeoutMs: options.timeoutMs,
      }),
    );
    if (!parsed.success) {
      return toFailureResult(
        createEngineFailure({
          code: "invalid_output",
          message: "The PDF engine returned invalid output.",
          operation,
          format: "pdf",
          detail: z.prettifyError(parsed.error),
        }),
      );
    }
    await recordLoadedVersion(clientResult.client);
    return { ok: true, pdf: toArrayBuffer(parsed.data) };
  } catch (error) {
    return normalizeFailure(error, {
      operation,
      format: "pdf",
      fallbackMessage: "The PDF engine failed to merge this document.",
    });
  }
};

export const csvToPdf = async (
  csvBytes: ProviderBinary,
  timeoutMs: number = DOCUMENT_CONVERSION_TIMEOUT_MS,
): Promise<PdfResult> => {
  const source = toUint8(csvBytes);
  if (source.byteLength > DOCUMENT_CONVERSION_MAX_INPUT_BYTES) {
    return toFailureResult(
      createEngineFailure({
        code: "resource_limit",
        message: `CSV (${source.byteLength} bytes) exceeds the ${DOCUMENT_CONVERSION_MAX_INPUT_BYTES}-byte processing limit.`,
        operation: "csv",
        format: "csv",
      }),
    );
  }
  const deadline = immediateDeadline(
    "csv",
    "csv",
    timeoutMs,
    "CSV conversion deadline exceeded.",
  );
  if (deadline) return deadline;
  const clientResult = getWorker("csv", "csv");
  if (!clientResult.ok) return clientResult;

  try {
    const text = new TextDecoder("utf-8").decode(source);
    const value =
      text.trim().length === 0
        ? await clientResult.client.textToPdf({ text: " ", timeoutMs })
        : await clientResult.client.csvToPdf({
            csv: new TextEncoder().encode(text),
            timeoutMs,
          });
    const parsed = binaryOutputSchema.safeParse(value);
    if (!parsed.success) {
      return toFailureResult(
        createEngineFailure({
          code: "invalid_output",
          message: "The PDF engine returned invalid CSV output.",
          operation: "csv",
          format: "csv",
          detail: z.prettifyError(parsed.error),
        }),
      );
    }
    await recordLoadedVersion(clientResult.client);
    return { ok: true, pdf: toArrayBuffer(parsed.data) };
  } catch (error) {
    return normalizeFailure(error, {
      operation: "csv",
      format: "csv",
      fallbackMessage: "The PDF engine failed to convert this CSV.",
    });
  }
};

export const markdownToPdf = async (
  markdown: string,
  timeoutMs: number = DOCUMENT_CONVERSION_TIMEOUT_MS,
): Promise<PdfResult> => {
  if (markdown.length > DOCUMENT_CONVERSION_MAX_INPUT_BYTES) {
    return toFailureResult(
      createEngineFailure({
        code: "resource_limit",
        message: `Markdown (${markdown.length} characters) exceeds the processing limit.`,
        operation: "markdown",
        format: "md",
      }),
    );
  }
  const deadline = immediateDeadline(
    "markdown",
    "md",
    timeoutMs,
    "Markdown conversion deadline exceeded.",
  );
  if (deadline) return deadline;
  const clientResult = getWorker("markdown", "md");
  if (!clientResult.ok) return clientResult;

  try {
    const parsed = binaryOutputSchema.safeParse(
      await clientResult.client.markdownToPdf({ markdown, timeoutMs }),
    );
    if (!parsed.success) {
      return toFailureResult(
        createEngineFailure({
          code: "invalid_output",
          message: "The PDF engine returned invalid Markdown output.",
          operation: "markdown",
          format: "md",
          detail: z.prettifyError(parsed.error),
        }),
      );
    }
    await recordLoadedVersion(clientResult.client);
    return { ok: true, pdf: toArrayBuffer(parsed.data) };
  } catch (error) {
    return normalizeFailure(error, {
      operation: "markdown",
      format: "md",
      fallbackMessage: "The PDF engine failed to convert this Markdown file.",
    });
  }
};

export const redactPdfWithReport = async (
  pdf: ProviderBinary,
  areasJson: string,
  timeoutMs?: number,
): Promise<RedactionResult> => {
  const source = toUint8(pdf);
  if (source.byteLength > PDF_PROCESSING_MAX_INPUT_BYTES) {
    return toFailureResult(
      createEngineFailure({
        code: "resource_limit",
        message: `PDF (${source.byteLength} bytes) exceeds the ${PDF_PROCESSING_MAX_INPUT_BYTES}-byte processing limit.`,
        operation: "redaction",
        format: "pdf",
      }),
    );
  }
  const deadline = immediateDeadline(
    "redaction",
    "pdf",
    timeoutMs,
    "PDF redaction deadline exceeded.",
  );
  if (deadline) return deadline;
  const clientResult = getWorker("redaction", "pdf");
  if (!clientResult.ok) return clientResult;

  try {
    const parsed = redactionReportSchema.safeParse(
      await clientResult.client.redactPdfWithReport({
        pdf: source,
        areasJson,
        timeoutMs,
      }),
    );
    if (!parsed.success) {
      return toFailureResult(
        createEngineFailure({
          code: "invalid_output",
          message: "The PDF engine returned an invalid redaction report.",
          operation: "redaction",
          format: "pdf",
          detail: z.prettifyError(parsed.error),
        }),
      );
    }
    await recordLoadedVersion(clientResult.client);
    return {
      ok: true,
      pdf: toArrayBuffer(parsed.data.pdf),
      pageCount: parsed.data.pageCount,
      warnings: parsed.data.warnings,
    };
  } catch (error) {
    return normalizeFailure(error, {
      operation: "redaction",
      format: "pdf",
      fallbackMessage: "The PDF engine failed to redact this document.",
    });
  }
};

const elapsedMs = (startedAt: number): number =>
  Number((performance.now() - startedAt).toFixed(2));

export const probePdfEngine = async (
  timeoutMs: number = ENGINE_BOOT_PROBE_TIMEOUT_MS,
): Promise<ProbeResult> => {
  const startedAt = performance.now();
  const deadline = immediateDeadline(
    "probe",
    "pdf",
    timeoutMs,
    "PDF engine availability probe deadline exceeded.",
  );
  if (deadline) return { ...deadline, bootDurationMs: elapsedMs(startedAt) };
  const clientResult = getWorker("probe", "pdf", timeoutMs);
  if (!clientResult.ok) {
    return { ...clientResult, bootDurationMs: elapsedMs(startedAt) };
  }
  try {
    await recordLoadedVersion(clientResult.client);
    return { ok: true, bootDurationMs: elapsedMs(startedAt) };
  } catch (error) {
    return {
      ...normalizeFailure(error, {
        operation: "probe",
        format: "pdf",
        fallbackMessage: PDF_ENGINE_UNAVAILABLE_MESSAGE,
      }),
      bootDurationMs: elapsedMs(startedAt),
    };
  }
};

export const pageCount = async (
  pdf: ProviderBinary,
  timeoutMs?: number,
): Promise<PageCountResult> => {
  const deadline = immediateDeadline(
    "page_count",
    "pdf",
    timeoutMs,
    "PDF page-count deadline exceeded.",
  );
  if (deadline) return deadline;
  const clientResult = getWorker("page_count", "pdf");
  if (!clientResult.ok) return clientResult;

  try {
    const parsed = z
      .number()
      .int()
      .positive()
      .safeParse(
        await clientResult.client.pageCount({ pdf: toUint8(pdf), timeoutMs }),
      );
    if (!parsed.success) {
      return toFailureResult(
        createEngineFailure({
          code: "invalid_output",
          message: "The PDF engine returned an invalid page count.",
          operation: "page_count",
          format: "pdf",
          detail: z.prettifyError(parsed.error),
        }),
      );
    }
    await recordLoadedVersion(clientResult.client);
    return { ok: true, pageCount: parsed.data };
  } catch (error) {
    return normalizeFailure(error, {
      operation: "page_count",
      format: "pdf",
      fallbackMessage: "The PDF engine failed to count document pages.",
    });
  }
};
