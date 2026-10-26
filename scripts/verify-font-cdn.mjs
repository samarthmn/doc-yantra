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

class FontCdnHeaderError extends Error {}

function option(name, fallback) {
  const at = process.argv.indexOf(name);
  return at === -1 ? fallback : process.argv[at + 1];
}

function expectedFontContentType(url) {
  const pathname = new URL(url).pathname.toLowerCase();
  if (pathname.endsWith(".otf")) return "font/otf";
  if (pathname.endsWith(".ttf")) return "font/ttf";
  throw new Error(`${url}: expected a .ttf or .otf font URL`);
}

function assertCdnHeaders(response, url) {
  if (!response.ok) return;
  const cors = response.headers.get("access-control-allow-origin");
  if (cors !== "*") {
    throw new FontCdnHeaderError(
      `${url}: expected Access-Control-Allow-Origin: *`,
    );
  }
  const cacheControl = response.headers.get("cache-control")?.toLowerCase() ?? "";
  const maxAgeMatch = cacheControl.match(/(?:^|,)\s*max-age=(\d+)/u);
  const maxAge = maxAgeMatch ? Number(maxAgeMatch[1]) : Number.NaN;
  if (
    !cacheControl.includes("immutable") ||
    !Number.isSafeInteger(maxAge) ||
    maxAge < 31_536_000
  ) {
    throw new FontCdnHeaderError(
      `${url}: expected immutable one-year Cache-Control`,
    );
  }
  const expectedContentType = expectedFontContentType(url);
  const contentType =
    response.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase() ??
    "";
  if (contentType !== expectedContentType) {
    throw new FontCdnHeaderError(
      `${url}: expected ${expectedContentType} Content-Type`,
    );
  }
  const contentTypeOptions =
    response.headers.get("x-content-type-options")?.trim().toLowerCase() ?? "";
  if (contentTypeOptions !== "nosniff") {
    throw new FontCdnHeaderError(
      `${url}: expected X-Content-Type-Options: nosniff`,
    );
  }
}

function findHeaderError(error) {
  const seen = new Set();
  let current = error;
  while (current instanceof Error && !seen.has(current)) {
    if (current instanceof FontCdnHeaderError) return current;
    seen.add(current);
    current = current.cause;
  }
  return null;
}

function requireHttpsBaseUrl(baseUrl) {
  const parsed = new URL(baseUrl);
  if (parsed.protocol !== "https:") {
    throw new Error("font CDN base URL must use HTTPS");
  }
  return parsed.toString().replace(/\/$/u, "");
}

function requireOrigin(origin) {
  const parsed = new URL(origin);
  if (
    (parsed.protocol !== "https:" && parsed.protocol !== "http:") ||
    parsed.origin !== origin
  ) {
    throw new Error("font CDN verification origin must be an HTTP(S) origin");
  }
  return parsed.origin;
}

export function createVerifiedCdnFetch({ origin, fetchImplementation }) {
  if (typeof fetchImplementation !== "function") {
    throw new Error("global fetch is unavailable");
  }
  const verifiedOrigin = requireOrigin(origin);
  return async (url, options) => {
    const headers = new Headers(options?.headers);
    headers.set("Origin", verifiedOrigin);
    const response = await fetchImplementation(url, {
      ...options,
      headers,
    });
    assertCdnHeaders(response, url);
    return response;
  };
}

export async function verifyFontCdn({
  baseUrl = CDN_BASE_URL,
  origin = "https://www.dockosha.com",
  timeoutMs = 30_000,
  fetchImplementation = globalThis.fetch,
} = {}) {
  const verifiedBaseUrl = requireHttpsBaseUrl(baseUrl);
  const verifiedOrigin = requireOrigin(origin);
  const verifiedFetch = createVerifiedCdnFetch({
    origin: verifiedOrigin,
    fetchImplementation,
  });
  const provider = createFontProvider({
    baseUrl: verifiedBaseUrl,
    timeoutMs,
    offlinePackage: false,
    memoryCache: {
      async getArtifact() { return undefined; },
      async putArtifact() {},
      async deleteArtifact() {},
    },
    fetch: verifiedFetch,
  });
  let bytes = 0;
  for (const entry of fontManifest.fonts) {
    let result;
    try {
      result = await provider.resolve(entry.file);
    } catch (error) {
      throw findHeaderError(error) ?? error;
    }
    if (result.entry.sha256 !== entry.sha256 || result.bytes.byteLength !== entry.byteLength) {
      throw new Error(`${entry.file}: CDN bytes differ from the canonical manifest`);
    }
    bytes += result.bytes.byteLength;
  }
  return Object.freeze({
    baseUrl: verifiedBaseUrl,
    origin: verifiedOrigin,
    fontCount: fontManifest.fonts.length,
    bytes,
  });
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  const timeoutMs = Number(option("--timeout-ms", "30000"));
  const result = await verifyFontCdn({
    baseUrl: option("--base-url", CDN_BASE_URL),
    origin: option("--origin", "https://www.dockosha.com"),
    timeoutMs,
  });
  process.stdout.write(
    process.argv.includes("--json")
      ? `${JSON.stringify(result)}\n`
      : `verified ${result.fontCount} CDN fonts (${result.bytes} bytes) at ${result.baseUrl}\n`,
  );
}
