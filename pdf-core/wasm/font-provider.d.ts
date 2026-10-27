export type DocYantraFontEntry = Readonly<{
  file: string;
  family: string;
  version: string;
  postscriptName: string;
  byteLength: number;
  format: "ttf" | "otf";
  faceIndex: 0;
  sha256: string;
  license: string;
  coverageClass: string;
}>;

export type FontResolutionSource = "memory" | "persistent" | "offline" | "cdn";

export type FontTelemetryEvent = Readonly<{
  type: "font_resolved" | "font_validation_failed" | "font_cache_write_failed";
  file: string;
  source: FontResolutionSource | "persistent";
  sha256?: string;
  bytes?: number;
  durationMs?: number;
  code?: string;
}>;

export interface FontCache {
  getArtifact(sha256: string): Promise<Uint8Array | undefined>;
  putArtifact(sha256: string, bytes: Uint8Array): Promise<void>;
  deleteArtifact(sha256: string): Promise<void>;
}

export interface OfflineFontPackage {
  manifest: { fonts: readonly DocYantraFontEntry[] };
  readFont(file: string): Uint8Array | Promise<Uint8Array>;
}

export interface FontProviderOptions {
  memoryCache?: FontCache;
  persistentCache?: FontCache;
  offlinePackage?: OfflineFontPackage | false;
  fetch?: typeof globalThis.fetch;
  baseUrl?: string;
  timeoutMs?: number;
  maxRedirects?: number;
  telemetry?: (event: FontTelemetryEvent) => void;
}

export type FontResolution = Readonly<{
  bytes: Uint8Array;
  entry: DocYantraFontEntry;
  source: FontResolutionSource;
  durationMs: number;
  url?: string;
}>;

export declare class FontProviderError extends Error {
  readonly code: string;
  readonly file?: string;
}

export declare class MemoryFontCache implements FontCache {
  getArtifact(sha256: string): Promise<Uint8Array | undefined>;
  putArtifact(sha256: string, bytes: Uint8Array): Promise<void>;
  deleteArtifact(sha256: string): Promise<void>;
}

export declare class NodeFontCache implements FontCache {
  constructor(absoluteDirectory: string);
  getArtifact(sha256: string): Promise<Uint8Array | undefined>;
  putArtifact(sha256: string, bytes: Uint8Array): Promise<void>;
  deleteArtifact(sha256: string): Promise<void>;
}

export declare class DocYantraFontProvider {
  constructor(options?: FontProviderOptions);
  resolve(file: string, signal?: AbortSignal): Promise<FontResolution>;
  resolveMany(files: readonly string[], signal?: AbortSignal): Promise<FontResolution[]>;
}

export declare const CDN_BASE_URL: string;
export declare const fontManifest: Readonly<{ fonts: readonly DocYantraFontEntry[] }>;
export declare function cdnUrlFor(entry: DocYantraFontEntry, baseUrl?: string): string;
export declare function createFontProvider(options?: FontProviderOptions): DocYantraFontProvider;
export declare function validateFontBytes(
  bytes: Uint8Array | ArrayBuffer | ArrayBufferView,
  entry: DocYantraFontEntry,
): Uint8Array;
