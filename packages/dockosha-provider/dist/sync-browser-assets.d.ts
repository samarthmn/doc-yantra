export type SyncBrowserAssetsOptions = {
    targetDirectory?: string;
};
/**
 * Copies the browser wasm-pack glue and binary to DocKosha's stable `/wasm/`
 * public path. The caller chooses the destination so this package never
 * assumes a particular application checkout layout.
 */
export declare const syncBrowserAssets: (options?: SyncBrowserAssetsOptions) => void;
//# sourceMappingURL=sync-browser-assets.d.ts.map