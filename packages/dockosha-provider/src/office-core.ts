import { createRequire } from "node:module";
import { performance } from "node:perf_hooks";
import type {
  FontProvider,
  FontsourceCatalogEntry,
  FontsourceProviderOptions,
  MemoryFontCache,
} from "@samarthmn/doc-yantra-office";
import { z } from "zod";
import {
  DOCUMENT_CONVERSION_TIMEOUT_MS,
  OFFICE_ENGINE_MAX_INPUT_BYTES,
} from "./constants.js";
import {
  createEngineFailure,
  normalizeEngineError,
  toFailureResult,
} from "./engine-errors.js";
import { createPackagedOfficeFontProvider } from "./office-font-provider.js";
import type {
  OfficePdfResult,
  ProviderBinary,
  ProviderFailure,
} from "./provider-interface.js";

type OfficeWorkerClient = {
  convertOnDemand(input: {
    bytes: ProviderBinary;
    extension: string;
    mode?: "strict" | "compatible";
    timeoutMs?: number;
  }): Promise<unknown>;
  engineVersion(): Promise<unknown>;
  close(): Promise<void>;
};

type OfficeWorkerModule = {
  createOfficeEngineWorker(options?: {
    defaultTimeoutMs?: number;
    fontProvider?: FontProvider;
    fontMemoryCache?: MemoryFontCache;
    fontMode?: "strict" | "compatible";
  }): OfficeWorkerClient;
};

type OfficePackageModule = {
  DEFAULT_FONTSOURCE_CATALOG: Readonly<Record<string, FontsourceCatalogEntry>>;
  FontsourceProvider: new (options?: FontsourceProviderOptions) => FontProvider;
  MemoryFontCache: new () => MemoryFontCache;
};

const isRecord = (value: unknown): value is Record<string, unknown> =>
  value !== null && typeof value === "object" && !Array.isArray(value);

const isOfficeWorkerModule = (value: unknown): value is OfficeWorkerModule =>
  isRecord(value) && typeof value.createOfficeEngineWorker === "function";

const isOfficePackageModule = (value: unknown): value is OfficePackageModule =>
  isRecord(value) &&
  isRecord(value.DEFAULT_FONTSOURCE_CATALOG) &&
  typeof value.FontsourceProvider === "function" &&
  typeof value.MemoryFontCache === "function";

// Every metric-compatible family is version-pinned: "latest" let Fontsource
// ship an Arimo whose Windows metrics no longer matched Arial (2136/797 vs
// 1854/434, measured 2026-08-02), silently inflating line boxes. The pins
// are the fidelity-harness fonts.lock versions whose metrics are verified.
// (Arial-class requests are additionally served by packaged Liberation Sans —
// see office-font-provider.ts — so Arimo remains only for literal requests.)
const FONTSOURCE_PACKAGE_VERSION_PINS = {
  arimo: "5.2.8",
  caladea: "5.2.8",
  carlito: "5.2.8",
  cousine: "5.2.7",
  gelasio: "5.2.8",
  "noto-sans-math": "5.2.9",
  "noto-sans-symbols": "5.2.11",
  "noto-sans-symbols-2": "5.2.8",
  tinos: "5.2.7",
  ubuntu: "5.2.8",
  "ubuntu-mono": "5.2.8",
} as const;

const pinFontsourceCatalog = (
  catalog: Readonly<Record<string, FontsourceCatalogEntry>>,
): Record<string, FontsourceCatalogEntry> => {
  const pinned = { ...catalog };
  for (const [id, packageVersion] of Object.entries(
    FONTSOURCE_PACKAGE_VERSION_PINS,
  )) {
    const entry = catalog[id];
    if (!entry)
      throw new Error(`DocYantra Fontsource catalog is missing ${id}`);
    pinned[id] = { ...entry, packageVersion };
  }
  return pinned;
};

const requireCjs = createRequire(__filename);
let worker: OfficeWorkerClient | null = null;
let createWorker: ((defaultTimeoutMs?: number) => OfficeWorkerClient) | null =
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
  const workerModule: unknown = requireCjs(
    "@samarthmn/doc-yantra-office/worker",
  );
  const officePackage: unknown = requireCjs("@samarthmn/doc-yantra-office");
  if (!isOfficeWorkerModule(workerModule)) {
    throw new Error("Office worker entrypoint has an invalid export contract");
  }
  if (!isOfficePackageModule(officePackage)) {
    throw new Error("Office package has an invalid font-provider contract");
  }
  const fontProvider = createPackagedOfficeFontProvider(
    new officePackage.FontsourceProvider({
      catalog: pinFontsourceCatalog(officePackage.DEFAULT_FONTSOURCE_CATALOG),
    }),
  );
  const fontMemoryCache = new officePackage.MemoryFontCache();
  createWorker = (defaultTimeoutMs) =>
    workerModule.createOfficeEngineWorker({
      ...(defaultTimeoutMs === undefined ? {} : { defaultTimeoutMs }),
      fontProvider,
      fontMemoryCache,
      fontMode: "compatible",
    });
} catch (error) {
  startupFailure = createEngineFailure({
    code: "worker_boot_failed",
    message: "The Office engine is temporarily unavailable.",
    operation: "probe",
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
  format?: string,
): ProviderFailure =>
  createEngineFailure({
    code: failure.code,
    message: failure.message,
    operation: "office_conversion",
    format,
    detail: failure.detail,
  });

type WorkerResult =
  | { ok: true; client: OfficeWorkerClient }
  | (ProviderFailure & { ok: false });

const getWorker = (
  format: string | undefined,
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
            message: "The Office engine is temporarily unavailable.",
            operation: "office_conversion",
            format,
          }),
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
        message: "The Office engine is temporarily unavailable.",
        operation: "office_conversion",
        format,
        detail: unknownDetail(error),
      }),
    );
  }
};

const recordLoadedVersion = async (
  client: OfficeWorkerClient,
): Promise<void> => {
  if (hasReportedVersion) {
    scheduleTestIdleClose();
    return;
  }
  const version = z
    .string()
    .min(1)
    .parse(await client.engineVersion());
  hasReportedVersion = true;
  console.info(`[office-core-wasm] worker loaded (version ${version})`);
  scheduleTestIdleClose();
};

const normalizeFailure = (
  error: unknown,
  format: string | undefined,
  fallbackMessage: string,
): ProviderFailure & { ok: false } => {
  scheduleTestIdleClose();
  return toFailureResult(
    normalizeEngineError(error, {
      operation: "office_conversion",
      format,
      fallbackMessage,
    }),
  );
};

const immediateDeadline = (
  timeoutMs: number,
  format: string,
): (ProviderFailure & { ok: false }) | null => {
  if (!Number.isFinite(timeoutMs)) {
    return toFailureResult(
      createEngineFailure({
        code: "invalid_input",
        message: "The Office engine timeout must be a finite number.",
        operation: "office_conversion",
        format,
      }),
    );
  }
  if (timeoutMs > 0) return null;
  return toFailureResult(
    createEngineFailure({
      code: "deadline_exceeded",
      message: "Office conversion deadline exceeded.",
      operation: "office_conversion",
      format,
    }),
  );
};

const toArrayBuffer = (bytes: Uint8Array): ArrayBuffer => {
  const buffer = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(buffer).set(bytes);
  return buffer;
};

const toUint8 = (bytes: ProviderBinary): Uint8Array =>
  bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);

const officeOnDemandOutputSchema = z.object({
  pdf: z.instanceof(Uint8Array),
});

export const officeToPdf = async (
  bytes: ProviderBinary,
  extension: string,
  timeoutMs: number = DOCUMENT_CONVERSION_TIMEOUT_MS,
): Promise<OfficePdfResult> => {
  const format = extension.trim().toLowerCase();
  if (bytes.byteLength > OFFICE_ENGINE_MAX_INPUT_BYTES) {
    return toFailureResult(
      createEngineFailure({
        code: "resource_limit",
        message: `Office input (${bytes.byteLength} bytes) exceeds the in-process limit.`,
        operation: "office_conversion",
        format,
      }),
    );
  }
  const deadline = immediateDeadline(timeoutMs, format);
  if (deadline) return deadline;
  const clientResult = getWorker(format);
  if (!clientResult.ok) return clientResult;

  try {
    const startedAt = performance.now();
    const parsed = officeOnDemandOutputSchema.safeParse(
      await clientResult.client.convertOnDemand({
        bytes: toUint8(bytes),
        extension: format,
        mode: "compatible",
        timeoutMs,
      }),
    );
    const ms = performance.now() - startedAt;
    if (!parsed.success) {
      return toFailureResult(
        createEngineFailure({
          code: "invalid_output",
          message: "The Office engine returned invalid output.",
          operation: "office_conversion",
          format,
          detail: z.prettifyError(parsed.error),
        }),
      );
    }
    const output = parsed.data.pdf;
    if (
      output.length < 5 ||
      String.fromCharCode(...output.subarray(0, 5)) !== "%PDF-"
    ) {
      return toFailureResult(
        createEngineFailure({
          code: "invalid_output",
          message: "The Office engine returned non-PDF output.",
          operation: "office_conversion",
          format,
        }),
      );
    }
    await recordLoadedVersion(clientResult.client);
    return { ok: true, pdf: toArrayBuffer(output), ms };
  } catch (error) {
    return normalizeFailure(
      error,
      format,
      "The Office engine failed to convert this document.",
    );
  }
};
