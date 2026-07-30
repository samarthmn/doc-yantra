import type { MergeAndWatermarkOptions, PageCountResult, PdfResult, ProbeResult, ProviderBinary, RedactionResult } from "./provider-interface.js";
export declare const mergeAndWatermark: (options: MergeAndWatermarkOptions) => Promise<PdfResult>;
export declare const csvToPdf: (csvBytes: ProviderBinary, timeoutMs?: number) => Promise<PdfResult>;
export declare const markdownToPdf: (markdown: string, timeoutMs?: number) => Promise<PdfResult>;
export declare const redactPdfWithReport: (pdf: ProviderBinary, areasJson: string, timeoutMs?: number) => Promise<RedactionResult>;
export declare const probePdfEngine: (timeoutMs?: number) => Promise<ProbeResult>;
export declare const pageCount: (pdf: ProviderBinary, timeoutMs?: number) => Promise<PageCountResult>;
//# sourceMappingURL=pdf-core.d.ts.map