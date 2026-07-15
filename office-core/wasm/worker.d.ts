import type {
  FontCache,
  FontProvider,
  FontResult,
  MemoryFontCache,
} from './font-provider';

export type WorkerBinary = ArrayBuffer | ArrayBufferView;

export type EngineWorkerErrorCode =
  | "invalid_input"
  | "unsupported_format"
  | "unsupported_feature"
  | "missing_glyph"
  | "missing_asset"
  | "password_protected"
  | "malformed_container"
  | "resource_limit"
  | "invalid_output"
  | "internal_error"
  | "deadline_exceeded"
  | "queue_busy"
  | "worker_boot_failed"
  | "engine_unavailable"
  | "protocol_error"
  | "trap";

/** Machine detail carried by the engine error envelope: a stable slug
 * string, or a structured object whose `reason` is the stable slug. Never
 * classify from `message` prose. Strings and `reason` are capped at 128 UTF-8
 * bytes; `context` is capped at 1,024 UTF-8 bytes. Oversized detail is dropped. */
export type EngineErrorDetail =
  | string
  | { reason: string; context?: string };

export interface EngineErrorEnvelope {
  code: EngineWorkerErrorCode;
  /** Capped at 4,096 UTF-8 bytes at both worker boundaries. */
  message: string;
  detail?: EngineErrorDetail;
}

export interface OfficeEngineWorkerOptions {
  /** Finite per-call deadline. Defaults to 120,000 ms. */
  defaultTimeoutMs?: number;
  resourceLimits?: Record<string, number>;
  maxQueueLength?: number;
  maxQueuedBytes?: number;
  maxInputBytes?: number;
  maxOutputBytes?: number;
  maxInputDepth?: number;
  maxInputItems?: number;
  maxOverlayCount?: number;
  maxPageCount?: number;
  maxRangeCount?: number;
  /** Font directories scanned once at worker boot in deterministic path order. */
  extraFontDirs?: string[];
  /** Parent-thread provider used only by the staged on-demand APIs. */
  fontProvider?: FontProvider;
  fontMemoryCache?: MemoryFontCache;
  fontPersistentCache?: FontCache;
  fontProviderTimeoutMs?: number;
  negativeFontTtlMs?: number;
  maxFontBytes?: number;
  fontMode?: 'strict' | 'compatible';
}

export interface OfficeWorkerInput {
  bytes: WorkerBinary;
  extension: string;
  /** Absolute operation budget, including validation, boot, queueing, execution, and transfer.
   * Invocation payload graphs may contain only own enumerable data properties.
   * Accessor properties and detectable Proxies are rejected as `invalid_input`
   * without executing their getters, setters, or traps. */
  timeoutMs?: number;
}

export interface OfficeWorkerOnDemandInput extends OfficeWorkerInput {
  mode?: 'strict' | 'compatible';
  signal?: AbortSignal;
}

export interface RedactionWarning {
  code: 'over_redaction';
  kind: 'text' | 'inline_image' | 'image_xobject' | 'form_xobject' | 'vector_path' | 'shading' | 'annotation' | 'widget';
  /** Zero-based page index; runtime guarantees a bounded integer >= 0. */
  page: number;
  message?: string;
}

export interface OfficeProfileWarning {
  code: string;
  detail: string;
}

export interface OfficeProfileReason {
  code: string;
  detail: string;
}

export interface OfficeProfileStats {
  paragraphs: number;
  tables: number;
  images: number;
  slides?: number;
  shapes?: number;
  sheets?: number;
  fontsUsed: string[];
  fontsMissing: string[];
}

export interface OfficeProfile {
  engine: "office-core";
  engineVersion: string;
  format: "docx" | "pptx" | "xlsx";
  supported: boolean;
  reasons: OfficeProfileReason[];
  warnings: OfficeProfileWarning[];
  stats: OfficeProfileStats;
  fonts: Array<Record<string, unknown>>;
}

export interface OfficeWorkerProfileOutput {
  profile: OfficeProfile;
  fonts: Array<Record<string, unknown>>;
  providerResults: Array<FontResult & { cacheStatus?: string; fetchDurationMs?: number }>;
}

export interface OfficeWorkerConvertOutput extends OfficeWorkerProfileOutput {
  pdf: Uint8Array;
}

export interface OfficeEngineWorker {
  convert(input: OfficeWorkerInput): Promise<Uint8Array>;
  profile(input: OfficeWorkerInput): Promise<OfficeProfile>;
  convertOnDemand(input: OfficeWorkerOnDemandInput): Promise<OfficeWorkerConvertOutput>;
  profileOnDemand(input: OfficeWorkerOnDemandInput): Promise<OfficeWorkerProfileOutput>;
  close(): Promise<void>;
  engineVersion(): Promise<string>;
}

export declare class EngineWorkerError extends Error implements EngineErrorEnvelope {
  constructor(code: EngineWorkerErrorCode, message: string, detail?: EngineErrorDetail);
  code: EngineWorkerErrorCode;
  detail?: EngineErrorDetail;
  /** @deprecated String alias: the string detail itself, or a structured
   * detail's `reason` slug. Use detail. */
  engineMessage?: string;
}

export declare function createOfficeEngineWorker(options?: OfficeEngineWorkerOptions): OfficeEngineWorker;
