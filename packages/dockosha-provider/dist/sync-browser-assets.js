"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.syncBrowserAssets = void 0;
const node_fs_1 = require("node:fs");
const node_module_1 = require("node:module");
const node_path_1 = require("node:path");
const requireCjs = (0, node_module_1.createRequire)(__filename);
/**
 * Copies the browser wasm-pack glue and binary to DocKosha's stable `/wasm/`
 * public path. The caller chooses the destination so this package never
 * assumes a particular application checkout layout.
 */
const syncBrowserAssets = (options = {}) => {
    const packageDirectory = (0, node_path_1.dirname)(requireCjs.resolve("@samarthmn/doc-yantra/package.json"));
    const webDirectory = (0, node_path_1.join)(packageDirectory, "web");
    const targetDirectory = options.targetDirectory ?? (0, node_path_1.join)(process.cwd(), "public", "wasm");
    (0, node_fs_1.mkdirSync)(targetDirectory, { recursive: true });
    (0, node_fs_1.copyFileSync)((0, node_path_1.join)(webDirectory, "pdf_core_wasm.js"), (0, node_path_1.join)(targetDirectory, "pdf_core_wasm.js"));
    (0, node_fs_1.copyFileSync)((0, node_path_1.join)(packageDirectory, "pdf_core_wasm_bg.wasm"), (0, node_path_1.join)(targetDirectory, "pdf_core_wasm_bg.wasm"));
};
exports.syncBrowserAssets = syncBrowserAssets;
//# sourceMappingURL=sync-browser-assets.js.map