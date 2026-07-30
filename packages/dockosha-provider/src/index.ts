import {
  PROVIDER_OPERATIONS,
  type DocumentProcessingProvider,
} from "./provider-interface.js";
import {
  csvToPdf,
  markdownToPdf,
  mergeAndWatermark,
  pageCount,
  probePdfEngine,
  redactPdfWithReport,
} from "./pdf-core.js";
import { officeToPdf } from "./office-core.js";

export { syncBrowserAssets } from "./sync-browser-assets.js";
export type { SyncBrowserAssetsOptions } from "./sync-browser-assets.js";

export const createProvider = (): DocumentProcessingProvider => ({
  id: "docyantra",
  capabilities: new Set(PROVIDER_OPERATIONS),
  mergeAndWatermark,
  pageCount,
  csvToPdf,
  markdownToPdf,
  officeToPdf,
  redactPdfWithReport,
  probe: probePdfEngine,
});
