import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import { describe, test } from 'node:test';

const require = createRequire(import.meta.url);
const { MemoryFontCache } = require('../office-core/wasm/font-provider.cjs');
const {
  FALLBACK_BASE_URL,
  NOTO_FALLBACK_MANIFEST,
  NOTO_FALLBACK_REQUESTS,
  NotoFallbackProvider,
  registerNotoFallbackFonts,
} = require('../office-core/wasm/noto-fallback.cjs');

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

describe('Noto fallback bootstrap', () => {
  test('pins six first-party faces and the 23-codepoint union probe', () => {
    assert.equal(FALLBACK_BASE_URL, 'https://public.dockosha.com/fallback-fonts/NotoSans/');
    assert.equal(NOTO_FALLBACK_MANIFEST.length, 6);
    assert.equal(NOTO_FALLBACK_REQUESTS.length, 6);
    assert.equal(new Set(NOTO_FALLBACK_MANIFEST.map(({ sha256: hash }) => hash)).size, 6);
    assert.ok(NOTO_FALLBACK_MANIFEST.every(({ sha256: hash }) => /^[0-9a-f]{64}$/.test(hash)));
    const probe = new Set(NOTO_FALLBACK_MANIFEST.flatMap(({ coverageCodepoints }) => coverageCodepoints));
    assert.equal(probe.size, 23);
    for (const codepoint of [0x25cf, 0x25aa, 0x25a0, 0x25cb, 0x2666, 0x25ba, 0x2192, 0x2022, 0x2070, 0x2079, 0x207b, 0x0416, 0x03a9, 0x2b9a]) {
      assert.ok(probe.has(codepoint), `missing fallback probe U+${codepoint.toString(16).toUpperCase()}`);
    }
  });

  test('uses only manifest filenames and rejects a changed first-party payload', async () => {
    const urls = [];
    const provider = new NotoFallbackProvider({
      fetch: async (url) => {
        urls.push(String(url));
        return new Response(Uint8Array.of(0, 1, 0, 0), {
          headers: { 'content-type': 'font/ttf' },
        });
      },
    });
    const [result] = await provider.resolve([NOTO_FALLBACK_REQUESTS[0]], new AbortController().signal);
    assert.equal(result.status, 'temporary_failure');
    assert.equal(result.failureCode, 'font_hash_mismatch');
    assert.deepEqual(urls, [`${FALLBACK_BASE_URL}Regular.ttf`]);
  });

  test('rejects an HTML 200 response before hash or font registration', async () => {
    const provider = new NotoFallbackProvider({
      fetch: async () => new Response('<html>not a font</html>', {
        headers: { 'content-type': 'text/html' },
      }),
    });
    const [result] = await provider.resolve(
      [NOTO_FALLBACK_REQUESTS[0]],
      new AbortController().signal,
    );
    assert.equal(result.status, 'temporary_failure');
    assert.match(result.reason, /content type/i);
  });

  test('warm cache performs zero fetches but re-registers all faces', async () => {
    let resolves = 0;
    let registrations = 0;
    const provider = {
      id: 'noto-fallback-cache-test',
      cacheKey(request) {
        return `fixture:${request.family}:${request.weight}:${request.italic}`;
      },
      async resolve(requests) {
        resolves += 1;
        return requests.map((request, index) => {
          const bytes = Uint8Array.of(0, 1, 0, index + 1);
          const family = `Fixture ${index}`;
          return {
            status: 'found',
            request,
            artifact: {
              family,
              fullName: `${family} Regular`,
              postscriptName: `${family.replaceAll(' ', '')}-Regular`,
              weight: 400,
              italic: false,
              stretch: 5,
              version: 'Fixture 1',
              faceIndex: 0,
              format: 'ttf',
              sha256: sha256(bytes),
              source: 'fixture',
              license: { id: 'OFL-1.1', embeddable: true, subsettable: true },
              bytes,
            },
          };
        });
      },
    };
    const memoryCache = new MemoryFontCache();
    const register = async () => {
      registrations += 1;
      return {};
    };

    const cold = await registerNotoFallbackFonts(register, { provider, memoryCache });
    const warm = await registerNotoFallbackFonts(register, { provider, memoryCache });

    assert.ok(cold.every(({ cacheStatus }) => cacheStatus === 'miss'));
    assert.ok(warm.every(({ cacheStatus }) => cacheStatus === 'memory_hit'));
    assert.equal(resolves, 1);
    assert.equal(registrations, 12);
  });
});
