import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { after, describe, test } from "node:test";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", "..");
const CACHE_DIRECTORY = join(ROOT, "tmp", `doc-yantra-font-cache-${process.pid}`);
const offlinePack = require("../../packages/doc-yantra-fonts");
const {
  FontProviderError,
  MemoryFontCache,
  NodeFontCache,
  createFontProvider,
  fontManifest,
  validateFontBytes,
} = require("../../pdf-core/wasm/font-provider.cjs");

const FILE = "LiberationSans-Regular.ttf";
const HASH = "76d04c18ea243f426b7de1f3ad208e927008f961dc5945e5aad352d0dfde8ee8";
const CDN_URL = `https://public.dockosha.com/doc-yantra-fonts/sha256/${HASH}/${FILE}`;
const BYTES = offlinePack.readFont(FILE);

after(() => rmSync(CACHE_DIRECTORY, { force: true, recursive: true }));

function response(bytes, init = {}) {
  const { headers = {}, ...responseInit } = init;
  return new Response(bytes, {
    status: 200,
    headers: {
      "content-length": String(bytes.byteLength),
      "content-type": "font/ttf",
      ...headers,
    },
    ...responseInit,
  });
}

function expectCode(code) {
  return (error) => {
    assert.ok(error instanceof FontProviderError, `expected FontProviderError, got ${error}`);
    assert.equal(error.code, code);
    return true;
  };
}

describe("DocYantra exact-font provider", () => {
  test("resolves offline first, validates exact bytes, then serves memory cache", async () => {
    let reads = 0;
    const telemetry = [];
    const provider = createFontProvider({
      offlinePackage: {
        manifest: offlinePack.manifest,
        readFont(file) {
          reads += 1;
          return offlinePack.readFont(file);
        },
      },
      fetch: async () => assert.fail("offline font must prevent a network request"),
      telemetry: (event) => telemetry.push(event),
    });

    const cold = await provider.resolve(FILE);
    const warm = await provider.resolve(FILE);

    assert.equal(cold.source, "offline");
    assert.equal(warm.source, "memory");
    assert.equal(createHash("sha256").update(cold.bytes).digest("hex"), HASH);
    assert.equal(reads, 1);
    assert.deepEqual(
      telemetry.map(({ source }) => source),
      ["offline", "memory"],
    );
    assert.ok(telemetry.every(({ bytes, durationMs }) => bytes === BYTES.byteLength && durationMs >= 0));
  });

  test("checks the persistent cache before offline and CDN sources", async () => {
    mkdirSync(CACHE_DIRECTORY, { recursive: true });
    const persistentCache = new NodeFontCache(CACHE_DIRECTORY);
    await persistentCache.putArtifact(HASH, BYTES);
    const provider = createFontProvider({
      memoryCache: new MemoryFontCache(),
      persistentCache,
      offlinePackage: {
        manifest: offlinePack.manifest,
        readFont: () => assert.fail("persistent hit must prevent an offline-pack read"),
      },
      fetch: async () => assert.fail("persistent hit must prevent a network request"),
    });

    const result = await provider.resolve(FILE);
    assert.equal(result.source, "persistent");
    assert.deepEqual(result.bytes, BYTES);
  });

  test("uses the immutable content-addressed CDN URL and caches a validated cold fetch", async () => {
    const requested = [];
    const provider = createFontProvider({
      offlinePackage: false,
      fetch: async (url, options) => {
        requested.push({ url, redirect: options.redirect });
        return response(BYTES);
      },
    });

    const cold = await provider.resolve(FILE);
    const warm = await provider.resolve(FILE);
    assert.equal(cold.source, "cdn");
    assert.equal(warm.source, "memory");
    assert.deepEqual(requested, [{ url: CDN_URL, redirect: "manual" }]);
  });

  test("fails closed on corrupt, mistyped, oversized, redirected, and unavailable CDN responses", async (t) => {
    const cases = [
      [
        "hash mismatch",
        "font_hash_mismatch",
        async () => response(Uint8Array.from(BYTES, (byte, index) => index === 10 ? byte ^ 1 : byte)),
      ],
      [
        "bad content type",
        "font_content_type",
        async () => response(BYTES, { headers: { "content-type": "text/html" } }),
      ],
      [
        "oversized declaration",
        "font_too_large",
        async () => response(BYTES, { headers: { "content-length": String(BYTES.byteLength + 1) } }),
      ],
      ["unavailable", "font_unavailable", async () => new Response("down", { status: 503 })],
    ];
    for (const [name, code, fetchImplementation] of cases) {
      await t.test(name, async () => {
        const provider = createFontProvider({ offlinePackage: false, fetch: fetchImplementation });
        await assert.rejects(provider.resolve(FILE), expectCode(code));
      });
    }

    await t.test("redirect overflow", async () => {
      const provider = createFontProvider({
        offlinePackage: false,
        maxRedirects: 1,
        fetch: async () => new Response(null, { status: 302, headers: { location: CDN_URL } }),
      });
      await assert.rejects(provider.resolve(FILE), expectCode("font_redirect_limit"));
    });

    await t.test("streamed response is stopped at the manifest byte limit", async () => {
      let reads = 0;
      let cancelled = false;
      const reader = {
        async read() {
          reads += 1;
          if (reads === 1) return { done: false, value: new Uint8Array(BYTES.byteLength) };
          if (reads === 2) return { done: false, value: Uint8Array.of(0) };
          throw new Error("provider read beyond the declared font size");
        },
        async cancel() { cancelled = true; },
        releaseLock() {},
      };
      const provider = createFontProvider({
        offlinePackage: false,
        fetch: async () => ({
          body: { getReader: () => reader },
          headers: new Headers({ "content-type": "font/ttf" }),
          ok: true,
          status: 200,
        }),
      });
      await assert.rejects(provider.resolve(FILE), expectCode("font_too_large"));
      assert.equal(reads, 2);
      assert.equal(cancelled, true);
    });

    await t.test("timeout", async () => {
      const provider = createFontProvider({
        offlinePackage: false,
        timeoutMs: 10,
        fetch: async (_url, { signal }) => new Promise((_resolve, reject) => {
          signal.addEventListener("abort", () => reject(signal.reason), { once: true });
        }),
      });
      await assert.rejects(provider.resolve(FILE), expectCode("font_timeout"));
    });
  });

  test("reports CDN and offline validation failures through font telemetry", async (t) => {
    const corrupted = Uint8Array.from(
      BYTES,
      (byte, index) => index === 10 ? byte ^ 1 : byte,
    );
    for (const [name, options, source] of [
      ["CDN", { offlinePackage: false, fetch: async () => response(corrupted) }, "cdn"],
      [
        "offline pack",
        {
          offlinePackage: {
            manifest: offlinePack.manifest,
            readFont: () => corrupted,
          },
        },
        "offline",
      ],
    ]) {
      await t.test(name, async () => {
        const telemetry = [];
        const provider = createFontProvider({
          ...options,
          telemetry: (event) => telemetry.push(event),
        });
        await assert.rejects(provider.resolve(FILE), expectCode("font_hash_mismatch"));
        assert.equal(telemetry.length, 1);
        assert.deepEqual(
          {
            type: telemetry[0].type,
            file: telemetry[0].file,
            source: telemetry[0].source,
            code: telemetry[0].code,
          },
          {
            type: "font_validation_failed",
            file: FILE,
            source,
            code: "font_hash_mismatch",
          },
        );
        assert.ok(telemetry[0].durationMs >= 0);
      });
    }
  });

  test("validates the OpenType family, version, face index, size, and hash", () => {
    const entry = fontManifest.fonts.find(({ file }) => file === FILE);
    for (const manifestEntry of fontManifest.fonts) {
      assert.doesNotThrow(
        () => validateFontBytes(offlinePack.readFont(manifestEntry.file), manifestEntry),
        manifestEntry.file,
      );
    }
    assert.throws(
      () => validateFontBytes(BYTES, { ...entry, family: "Wrong Family" }),
      expectCode("font_identity_mismatch"),
    );
    assert.throws(
      () => validateFontBytes(BYTES, { ...entry, faceIndex: 1 }),
      expectCode("font_face_index"),
    );
  });
});
