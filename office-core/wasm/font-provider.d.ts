export type CanonicalFontSlot = 'ascii' | 'h_ansi' | 'east_asia' | 'cs';

export interface CanonicalFontRequest {
  family: string;
  altFamilies: string[];
  weight: number;
  italic: boolean;
  stretch: number;
  slots: CanonicalFontSlot[];
  panose: string | null;
  familyClass: string | null;
  pitch: string | null;
  charset: string | null;
  unicodeRanges: number[];
  scripts: string[];
  vfCoords: string[];
}

export interface FontArtifact {
  family: string;
  fullName: string;
  postscriptName: string;
  weight: number;
  italic: boolean;
  stretch?: number;
  version: string;
  faceIndex: number;
  format: 'ttf' | 'otf' | 'ttc' | 'otc';
  sha256: string;
  source: string;
  license: { id: string; embeddable: boolean; subsettable: boolean };
  bytes: Uint8Array;
}

export type FontResult =
  | { status: 'found'; request: CanonicalFontRequest; artifact: FontArtifact }
  | { status: 'not_found'; request: CanonicalFontRequest }
  | { status: 'denied'; request: CanonicalFontRequest; reason: string }
  | { status: 'temporary_failure'; request: CanonicalFontRequest; reason: string };

export interface FontProvider {
  readonly id: string;
  resolve(requests: CanonicalFontRequest[], signal: AbortSignal): Promise<FontResult[]>;
  cacheKey?(request: CanonicalFontRequest): string | Promise<string>;
}

export interface FontCache {
  getArtifact(sha256: string): Promise<Uint8Array | undefined>;
  putArtifact(sha256: string, bytes: Uint8Array): Promise<void>;
  deleteArtifact(sha256: string): Promise<void>;
  getRequest(key: string): Promise<Record<string, unknown> | undefined>;
  putRequest(key: string, value: Record<string, unknown>): Promise<void>;
  deleteRequest(key: string): Promise<void>;
}

export declare class MemoryFontCache implements FontCache {
  constructor();
  getArtifact(sha256: string): Promise<Uint8Array | undefined>;
  putArtifact(sha256: string, bytes: Uint8Array): Promise<void>;
  deleteArtifact(sha256: string): Promise<void>;
  getRequest(key: string): Promise<Record<string, unknown> | undefined>;
  putRequest(key: string, value: Record<string, unknown>): Promise<void>;
  deleteRequest(key: string): Promise<void>;
}

export declare class NodeFontCache implements FontCache {
  constructor(absoluteDirectory: string);
  getArtifact(sha256: string): Promise<Uint8Array | undefined>;
  putArtifact(sha256: string, bytes: Uint8Array): Promise<void>;
  deleteArtifact(sha256: string): Promise<void>;
  getRequest(key: string): Promise<Record<string, unknown> | undefined>;
  putRequest(key: string, value: Record<string, unknown>): Promise<void>;
  deleteRequest(key: string): Promise<void>;
}

export interface OnDemandOptions {
  provider: FontProvider;
  mode?: 'strict' | 'compatible';
  memoryCache?: MemoryFontCache;
  persistentCache?: FontCache;
  signal?: AbortSignal;
  timeoutMs?: number;
  negativeTtlMs?: number;
  maxFontBytes?: number;
  limits?: { maxFontBytes?: number };
}

export interface OnDemandOutput {
  pdf: Uint8Array;
  profile: Record<string, unknown>;
  fonts: Array<Record<string, unknown>>;
  providerResults: Array<FontResult & { cacheStatus?: string; fetchDurationMs?: number }>;
}

export declare function convertOnDemand(
  bytes: Uint8Array,
  extension: string,
  options: OnDemandOptions,
): Promise<OnDemandOutput>;

export declare function resolveAndRegisterFonts(
  requests: CanonicalFontRequest[],
  register: (bytes: Uint8Array, expectationJson: string) => unknown | Promise<unknown>,
  options: OnDemandOptions,
): Promise<Array<FontResult & { cacheStatus?: string; fetchDurationMs?: number }>>;

export declare function mergeHostTelemetry(
  profile: Record<string, unknown>,
  providerResults: Array<FontResult & { cacheStatus?: string; fetchDurationMs?: number }>,
): Record<string, unknown>;
