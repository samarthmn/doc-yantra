#!/usr/bin/env node

import { createRequire } from "node:module";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const {
  CDN_BASE_URL,
  createFontProvider,
  fontManifest,
} = require("../pdf-core/wasm/font-provider.cjs");

function option(name, fallback) {
  const at = process.argv.indexOf(name);
  return at === -1 ? fallback : process.argv[at + 1];
}

function assertCdnHeaders(response, url) {
  if (!response.ok) return;
  const cors = response.headers.get("access-control-allow-origin");
  if (cors !== "*") throw new Error(`${url}: expected Access-Control-Allow-Origin: *`);
  const cacheControl = response.headers.get("cache-control")?.toLowerCase() ?? "";
  if (!cacheControl.includes("immutable") || !/max-age=(?:31536000|[4-9]\d{7,})/u.test(cacheControl)) {
    throw new Error(`${url}: expected immutable one-year Cache-Control`);
  }
}

export async function verifyFontCdn({
  baseUrl = CDN_BASE_URL,
  timeoutMs = 30_000,
  fetchImplementation = globalThis.fetch,
} = {}) {
  if (typeof fetchImplementation !== "function") throw new Error("global fetch is unavailable");
  const provider = createFontProvider({
    baseUrl,
    timeoutMs,
    offlinePackage: false,
    memoryCache: {
      async getArtifact() { return undefined; },
      async putArtifact() {},
      async deleteArtifact() {},
    },
    fetch: async (url, options) => {
      const response = await fetchImplementation(url, options);
      assertCdnHeaders(response, url);
      return response;
    },
  });
  let bytes = 0;
  for (const entry of fontManifest.fonts) {
    const result = await provider.resolve(entry.file);
    if (result.entry.sha256 !== entry.sha256 || result.bytes.byteLength !== entry.byteLength) {
      throw new Error(`${entry.file}: CDN bytes differ from the canonical manifest`);
    }
    bytes += result.bytes.byteLength;
  }
  return Object.freeze({ baseUrl, fontCount: fontManifest.fonts.length, bytes });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const timeoutMs = Number(option("--timeout-ms", "30000"));
  const result = await verifyFontCdn({
    baseUrl: option("--base-url", CDN_BASE_URL),
    timeoutMs,
  });
  process.stdout.write(
    process.argv.includes("--json")
      ? `${JSON.stringify(result)}\n`
      : `verified ${result.fontCount} CDN fonts (${result.bytes} bytes) at ${result.baseUrl}\n`,
  );
}
