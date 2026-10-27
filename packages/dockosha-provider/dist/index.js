"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.createProvider = exports.syncBrowserAssets = void 0;
const provider_interface_js_1 = require("./provider-interface.js");
const pdf_core_js_1 = require("./pdf-core.js");
const office_core_js_1 = require("./office-core.js");
var sync_browser_assets_js_1 = require("./sync-browser-assets.js");
Object.defineProperty(exports, "syncBrowserAssets", { enumerable: true, get: function () { return sync_browser_assets_js_1.syncBrowserAssets; } });
const createProvider = () => ({
    id: "docyantra",
    capabilities: new Set(provider_interface_js_1.PROVIDER_OPERATIONS),
    mergeAndWatermark: pdf_core_js_1.mergeAndWatermark,
    pageCount: pdf_core_js_1.pageCount,
    csvToPdf: pdf_core_js_1.csvToPdf,
    markdownToPdf: pdf_core_js_1.markdownToPdf,
    officeToPdf: office_core_js_1.officeToPdf,
    redactPdfWithReport: pdf_core_js_1.redactPdfWithReport,
    probe: pdf_core_js_1.probePdfEngine,
});
exports.createProvider = createProvider;
//# sourceMappingURL=index.js.map