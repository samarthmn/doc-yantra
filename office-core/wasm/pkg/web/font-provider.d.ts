export * from '../font-provider';

export interface IndexedDbFontCacheOptions {
  indexedDB?: IDBFactory;
  databaseName?: string;
}

export declare class IndexedDbFontCache {
  constructor(options?: IndexedDbFontCacheOptions);
  getArtifact(sha256: string): Promise<Uint8Array | undefined>;
  putArtifact(sha256: string, bytes: Uint8Array): Promise<void>;
  deleteArtifact(sha256: string): Promise<void>;
  getRequest(key: string): Promise<Record<string, unknown> | undefined>;
  putRequest(key: string, value: Record<string, unknown>): Promise<void>;
  deleteRequest(key: string): Promise<void>;
  close(): Promise<void>;
}
