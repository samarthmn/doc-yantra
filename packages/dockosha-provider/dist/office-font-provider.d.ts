import type { FontProvider } from "@samarthmn/doc-yantra-office";
type ExactFontResolution = {
    bytes: Uint8Array;
    source?: string;
    durationMs?: number;
    url?: string;
};
type ExactFontProvider = {
    resolve(file: string, signal?: AbortSignal): Promise<ExactFontResolution>;
};
export type PackagedOfficeFontProviderOptions = {
    fontProvider?: ExactFontProvider;
};
export declare const createPackagedOfficeFontProvider: (primary: FontProvider, options?: PackagedOfficeFontProviderOptions) => FontProvider;
export {};
//# sourceMappingURL=office-font-provider.d.ts.map