import type { CanonicalFontRequest, FontProvider, FontResult } from './font-provider';

export interface FontsourceCatalogEntry {
  family: string;
  subsets: string[];
  weights: number[];
  styles: string[];
  defSubset: string;
  catalogVersion?: string;
  version?: string;
  packageVersion?: string;
  npmVersion?: string;
  license?: string;
  expectedVersion?: string;
}

export interface FontsourceProviderOptions {
  id?: string;
  aliases?: Record<string, string>;
  catalog?: Record<string, FontsourceCatalogEntry>;
  metadataBaseUrl?: string;
  cdnBaseUrl?: string;
  maxFontBytes?: number;
  maxMetadataBytes?: number;
  maxRedirects?: number;
  concurrency?: number;
  fetch?: typeof globalThis.fetch;
  fallbackSource?: (descriptor: Record<string, unknown>) => string | URL | Promise<string | URL>;
}

export declare const DEFAULT_FONTSOURCE_ALIASES: Readonly<Record<string, string>>;
export declare const DEFAULT_FONTSOURCE_CATALOG: Readonly<Record<string, FontsourceCatalogEntry>>;

export declare class FontsourceProvider implements FontProvider {
  readonly id: string;
  constructor(options?: FontsourceProviderOptions);
  cacheKey(request: CanonicalFontRequest): Promise<string>;
  resolve(requests: CanonicalFontRequest[], signal: AbortSignal): Promise<FontResult[]>;
}
