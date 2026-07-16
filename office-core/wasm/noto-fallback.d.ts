import type {
  CanonicalFontRequest,
  FontCache,
  FontProvider,
  FontResult,
  MemoryFontCache,
} from './font-provider';

export interface NotoFallbackManifestEntry {
  family: string;
  fullName: string;
  postscriptName: string;
  file: string;
  weight: number;
  italic: boolean;
  stretch: number;
  expectedVersion: string;
  sha256: string;
  coverageScripts: readonly string[];
  coverageCodepoints: readonly number[];
}

export interface NotoFallbackProviderOptions {
  id?: string;
  baseUrl?: string;
  maxFontBytes?: number;
  maxRedirects?: number;
  manifest?: readonly NotoFallbackManifestEntry[];
  fetch?: typeof globalThis.fetch;
}

export interface RegisterNotoFallbackOptions {
  provider?: FontProvider;
  providerOptions?: NotoFallbackProviderOptions;
  memoryCache?: MemoryFontCache;
  persistentCache?: FontCache;
  signal?: AbortSignal;
  timeoutMs?: number;
  negativeTtlMs?: number;
  maxFontBytes?: number;
}

export declare const FALLBACK_BASE_URL: string;
export declare const NOTO_FALLBACK_MANIFEST: readonly NotoFallbackManifestEntry[];
export declare const NOTO_FALLBACK_REQUESTS: readonly CanonicalFontRequest[];

export declare class NotoFallbackProvider implements FontProvider {
  readonly id: string;
  constructor(options?: NotoFallbackProviderOptions);
  cacheKey(request: CanonicalFontRequest): string;
  resolve(requests: CanonicalFontRequest[], signal: AbortSignal): Promise<FontResult[]>;
}

export declare function registerNotoFallbackFonts(
  register: (bytes: Uint8Array, expectationJson: string) => unknown | Promise<unknown>,
  options?: RegisterNotoFallbackOptions,
): Promise<Array<FontResult & { cacheStatus?: string; fetchDurationMs?: number }>>;
