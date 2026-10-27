import { copyFileSync, mkdirSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";

const requireCjs = createRequire(__filename);

export type SyncBrowserAssetsOptions = {
  targetDirectory?: string;
};

/**
 * Copies the browser wasm-pack glue and binary to DocKosha's stable `/wasm/`
 * public path. The caller chooses the destination so this package never
 * assumes a particular application checkout layout.
 */
export const syncBrowserAssets = (
  options: SyncBrowserAssetsOptions = {},
): void => {
  const packageDirectory = dirname(
    requireCjs.resolve("@samarthmn/doc-yantra/package.json"),
  );
  const webDirectory = join(packageDirectory, "web");
  const targetDirectory =
    options.targetDirectory ?? join(process.cwd(), "public", "wasm");

  mkdirSync(targetDirectory, { recursive: true });
  copyFileSync(
    join(webDirectory, "pdf_core_wasm.js"),
    join(targetDirectory, "pdf_core_wasm.js"),
  );
  copyFileSync(
    join(packageDirectory, "pdf_core_wasm_bg.wasm"),
    join(targetDirectory, "pdf_core_wasm_bg.wasm"),
  );
};
