import { z } from "zod";
import {
  PROVIDER_ERROR_CODES,
  PROVIDER_OPERATIONS,
  type ProviderFailure,
  type ProviderOperation,
} from "./provider-interface.js";

export const engineErrorCodeSchema = z.enum(PROVIDER_ERROR_CODES);
export type EngineErrorCode = z.infer<typeof engineErrorCodeSchema>;
export type EngineOperation = ProviderOperation;

const engineOperationSchema = z.enum(PROVIDER_OPERATIONS);
const RETRYABLE_CODES: ReadonlySet<EngineErrorCode> = new Set([
  "deadline_exceeded",
  "queue_busy",
  "worker_boot_failed",
  "engine_unavailable",
  "trap",
]);

const DETAIL_MAX_DEPTH = 5;
const DETAIL_MAX_ITEMS = 32;
const DETAIL_MAX_KEY_LENGTH = 128;
const DETAIL_MAX_STRING_LENGTH = 2_048;

type EngineErrorContext =
  | null
  | boolean
  | number
  | string
  | EngineErrorContext[]
  | { [key: string]: EngineErrorContext };

const boundedString = (value: string): string | undefined => {
  const trimmed = value.trim();
  return trimmed ? trimmed.slice(0, DETAIL_MAX_STRING_LENGTH) : undefined;
};

const boundedContext = (
  value: unknown,
  depth: number,
  ancestors: ReadonlySet<object>,
): EngineErrorContext | undefined => {
  if (value === null || typeof value === "boolean") return value;
  if (typeof value === "number")
    return Number.isFinite(value) ? value : undefined;
  if (typeof value === "string") return boundedString(value);
  if (depth >= DETAIL_MAX_DEPTH || typeof value !== "object") return undefined;
  if (ancestors.has(value)) return undefined;

  const nextAncestors = new Set(ancestors);
  nextAncestors.add(value);
  if (Array.isArray(value)) {
    return value.slice(0, DETAIL_MAX_ITEMS).flatMap((item) => {
      const bounded = boundedContext(item, depth + 1, nextAncestors);
      return bounded === undefined ? [] : [bounded];
    });
  }

  const result: { [key: string]: EngineErrorContext } = {};
  for (const [rawKey, item] of Object.entries(value).slice(
    0,
    DETAIL_MAX_ITEMS,
  )) {
    const key = rawKey.slice(0, DETAIL_MAX_KEY_LENGTH);
    if (!key) continue;
    const bounded = boundedContext(item, depth + 1, nextAncestors);
    if (bounded !== undefined) result[key] = bounded;
  }
  return result;
};

const normalizeDetail = (value: unknown): ProviderFailure["detail"] => {
  if (typeof value === "string") return boundedString(value);
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return undefined;
  }
  const bounded = boundedContext(value, 0, new Set());
  if (!bounded || Array.isArray(bounded) || typeof bounded !== "object") {
    return undefined;
  }
  return bounded;
};

const failureSchema = z
  .object({
    code: engineErrorCodeSchema,
    message: z.string().min(1).max(4_096),
    operation: engineOperationSchema,
    format: z.string().trim().min(1).max(64).optional(),
    detail: z.unknown().optional(),
  })
  .transform((input): ProviderFailure => {
    const detail = normalizeDetail(input.detail);
    return {
      code: input.code,
      message: input.message,
      operation: input.operation,
      ...(input.format ? { format: input.format } : {}),
      retryable: RETRYABLE_CODES.has(input.code),
      ...(detail ? { detail } : {}),
    };
  });

type FailureInput = Omit<ProviderFailure, "retryable">;

export const createEngineFailure = (input: FailureInput): ProviderFailure =>
  failureSchema.parse(input);

const workerErrorSchema = z
  .object({
    code: engineErrorCodeSchema,
    message: z.string().min(1),
    detail: z.unknown().optional(),
    engineMessage: z.string().min(1).optional(),
  })
  .loose();

export const normalizeEngineError = (
  error: unknown,
  context: {
    operation: EngineOperation;
    format?: string;
    fallbackMessage: string;
  },
): ProviderFailure => {
  const parsed = workerErrorSchema.safeParse(error);
  if (parsed.success) {
    return createEngineFailure({
      code: parsed.data.code,
      message: parsed.data.message,
      operation: context.operation,
      format: context.format,
      detail: normalizeDetail(
        parsed.data.detail ?? parsed.data.engineMessage,
      ),
    });
  }

  return createEngineFailure({
    code: "internal_error",
    message: context.fallbackMessage,
    operation: context.operation,
    format: context.format,
    detail:
      error instanceof Error
        ? error.message
        : typeof error === "string"
          ? error
          : undefined,
  });
};

export const toFailureResult = (
  failure: ProviderFailure,
): ProviderFailure & { ok: false } => ({ ok: false, ...failure });
