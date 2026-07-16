import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const {
  DEFAULT_FONTSOURCE_ALIASES,
  FontsourceProvider,
} = require('../office-core/wasm/fontsource-provider.cjs');
const {
  MemoryFontCache,
  resolveAndRegisterFonts,
} = require('../office-core/wasm/font-provider.cjs');

function request(overrides = {}) {
  return {
    family: 'Ubuntu',
    altFamilies: [],
    weight: 400,
    italic: false,
    stretch: 5,
    slots: ['ascii'],
    panose: null,
    familyClass: null,
    pitch: null,
    charset: null,
    unicodeRanges: [],
    scripts: ['latin'],
    vfCoords: [],
    ...overrides,
  };
}

function bytesResponse(bytes, status = 200, headers = {}) {
  return new Response(Uint8Array.from(bytes), {
    status,
    headers: { 'content-type': 'font/ttf', ...headers },
  });
}

describe('FontsourceProvider', () => {
  test('requested family precedes font-table alternatives', () => {
    const provider = new FontsourceProvider();
    assert.equal(provider.mappedId(request({ altFamilies: ['Calibri'] })), 'ubuntu');
  });

  test('maps Georgia to Gelasio and classifies otherwise unknown families', () => {
    const provider = new FontsourceProvider();
    assert.equal(provider.mappedId(request({ family: 'Georgia' })), 'gelasio');
    assert.equal(provider.mappedId(request({
      family: 'Unknown Serif', familyClass: 'roman',
    })), 'tinos');
    assert.equal(provider.mappedId(request({
      family: 'Unknown Sans', panose: '020B0604020202020204',
    })), 'arimo');
    assert.equal(provider.mappedId(request({
      family: 'Unknown Mono', pitch: 'fixed',
    })), 'cousine');
  });

  test('maps every compatibility family to a fetchable allowlisted id', () => {
    for (const [family, id] of [
      ['Calibri', 'carlito'], ['Cambria', 'caladea'], ['Arial', 'arimo'],
      ['Helvetica', 'arimo'], ['Segoe UI', 'arimo'], ['Liberation Sans', 'arimo'],
      ['Times New Roman', 'tinos'], ['Liberation Serif', 'tinos'],
      ['Courier New', 'cousine'], ['Liberation Mono', 'cousine'],
      ['Georgia', 'gelasio'],
    ]) {
      assert.equal(DEFAULT_FONTSOURCE_ALIASES[family.toLowerCase()], id);
    }
  });

  test('uses the Ubuntu v21 catalog pin and immutable npm artifact path', async () => {
    const urls = [];
    const provider = new FontsourceProvider({
      fetch: async (url) => {
        urls.push(String(url));
        return bytesResponse([0, 1, 0, 0]);
      },
    });
    const [result] = await provider.resolve([request()], new AbortController().signal);

    assert.equal(result.status, 'found');
    assert.deepEqual(urls, [
      'https://cdn.jsdelivr.net/fontsource/fonts/ubuntu@5.2.8/latin-400-normal.ttf',
      'https://cdn.jsdelivr.net/fontsource/fonts/ubuntu@5.2.8/latin-ext-400-normal.ttf',
    ]);
    assert.equal(result.artifact.version, 'v21');
    assert.equal(result.artifact.expectedVersion, '0.83');
    assert.equal(result.artifact.license.id, 'UFL-1.0');
    assert.match(await provider.cacheKey(request()), /ubuntu\|v21\|5\.2\.8/);
  });

  test('fetches the default and every requested available script subset', async () => {
    const urls = [];
    const provider = new FontsourceProvider({
      concurrency: 2,
      fetch: async (url) => {
        urls.push(String(url));
        return bytesResponse([urls.length, 1, 0, 0]);
      },
    });
    const [result] = await provider.resolve([
      request({ scripts: ['latin', 'greek', 'cyrillic'] }),
    ], new AbortController().signal);

    assert.equal(result.status, 'found');
    assert.deepEqual(result.artifacts.map(({ subset }) => subset), [
      'latin', 'cyrillic', 'cyrillic-ext', 'greek', 'greek-ext', 'latin-ext',
    ]);
    assert.deepEqual(result.artifacts.map(({ coverageScripts }) => coverageScripts), [
      ['latin'], ['cyrillic'], [], ['greek'], [], [],
    ]);
    assert.deepEqual(result.artifacts.map(({ coverageCodepoints }) => coverageCodepoints), [
      [], [], [0x0460], [], [0x1f00], [0x0100],
    ]);
  });

  test('latin requests include latin-ext and Vietnamese subset artifacts when available', async () => {
    const provider = new FontsourceProvider({
      fetch: async () => bytesResponse([0, 1, 0, 0]),
    });
    const [result] = await provider.resolve([
      request({ family: 'Arial', scripts: ['latin'] }),
    ], new AbortController().signal);

    assert.equal(result.status, 'found');
    assert.deepEqual(result.artifacts.map(({ subset }) => subset), [
      'latin', 'latin-ext', 'vietnamese',
    ]);
    assert.deepEqual(result.artifacts.map(({ coverageCodepoints }) => coverageCodepoints), [
      [], [0x0100], [0x0102],
    ]);
  });

  test('maps 404 separately from network and 5xx failures', async () => {
    for (const [status, expected] of [[404, 'not_found'], [503, 'temporary_failure']]) {
      const provider = new FontsourceProvider({
        fetch: async () => bytesResponse([], status),
      });
      const [result] = await provider.resolve([request()], new AbortController().signal);
      assert.equal(result.status, expected);
    }
    const provider = new FontsourceProvider({ fetch: async () => { throw new Error('offline'); } });
    const [result] = await provider.resolve([request()], new AbortController().signal);
    assert.equal(result.status, 'temporary_failure');
    assert.match(result.reason, /offline/);
  });

  test('enforces HTTPS redirects, redirect depth, and response size', async () => {
    const insecure = new FontsourceProvider({
      fetch: async () => new Response(null, {
        status: 302,
        headers: { location: 'http://example.test/font.ttf' },
      }),
    });
    assert.equal(
      (await insecure.resolve([request()], new AbortController().signal))[0].status,
      'temporary_failure',
    );

    let redirects = 0;
    const looping = new FontsourceProvider({
      maxRedirects: 2,
      fetch: async () => {
        redirects += 1;
        return new Response(null, { status: 302, headers: { location: `https://example.test/${redirects}` } });
      },
    });
    assert.equal((await looping.resolve([request()], new AbortController().signal))[0].status, 'temporary_failure');
    assert.equal(redirects, 3);

    const oversized = new FontsourceProvider({
      maxFontBytes: 3,
      fetch: async () => bytesResponse([0, 1, 2, 3]),
    });
    assert.equal((await oversized.resolve([request()], new AbortController().signal))[0].status, 'temporary_failure');

    const html = new FontsourceProvider({
      fetch: async () => bytesResponse([0, 1, 0, 0], 200, { 'content-type': 'text/html' }),
    });
    const [htmlResult] = await html.resolve([request()], new AbortController().signal);
    assert.equal(htmlResult.status, 'temporary_failure');
    assert.match(htmlResult.reason, /content type/i);
  });

  test('uses a trusted HTTPS fallback source after a primary failure', async () => {
    const urls = [];
    const provider = new FontsourceProvider({
      fallbackSource: ({ id, subset }) =>
        `https://cdn.jsdelivr.net/gh/google/fonts@deadbeef/ofl/${id}/${subset}.ttf`,
      fetch: async (url) => {
        urls.push(String(url));
        return urls.length === 1 ? bytesResponse([], 503) : bytesResponse([0, 1, 0, 0]);
      },
    });
    const [result] = await provider.resolve([request()], new AbortController().signal);

    assert.equal(result.status, 'found');
    assert.equal(urls.length, 3);
    assert.match(result.artifact.source, /google\/fonts@deadbeef/);
  });

  test('never interpolates an unmapped document family into a URL', async () => {
    let calls = 0;
    const provider = new FontsourceProvider({ fetch: async () => { calls += 1; } });
    const [result] = await provider.resolve([
      request({ family: '../secret?document=text', altFamilies: [] }),
    ], new AbortController().signal);
    assert.equal(result.status, 'not_found');
    assert.equal(calls, 0);
    assert.throws(
      () => new FontsourceProvider({ aliases: { Unsafe: '../escape' } }),
      /Fontsource id/,
    );
  });

  test('bounds discovery metadata and rejects unsafe subset paths', async () => {
    const oversized = new FontsourceProvider({
      aliases: { Custom: 'custom-font' },
      maxMetadataBytes: 3,
      fetch: async () => new Response('{"family":"Custom"}', {
        headers: { 'content-length': '19', 'content-type': 'application/json' },
      }),
    });
    assert.equal((await oversized.resolve([
      request({ family: 'Custom' }),
    ], new AbortController().signal))[0].status, 'temporary_failure');

    assert.throws(() => new FontsourceProvider({
      aliases: { Custom: 'custom-font' },
      catalog: {
        'custom-font': {
          family: 'Custom',
          subsets: ['../../escape'],
          weights: [400],
          styles: ['normal'],
          defSubset: '../../escape',
        },
      },
    }), /unsafe values/);
  });

  test('fresh provider reuses a persistent cache for dynamically discovered families offline', async () => {
    const persistentCache = new MemoryFontCache();
    const customRequest = request({ family: 'Custom', scripts: ['latin'] });
    let calls = 0;
    const options = {
      aliases: { Custom: 'custom-font' },
      fetch: async (url) => {
        calls += 1;
        if (String(url).includes('api.fontsource.org')) {
          return new Response(JSON.stringify({
            family: 'Custom',
            subsets: ['latin'],
            weights: [400],
            styles: ['normal'],
            defSubset: 'latin',
            version: 'v1',
            npmVersion: '1.2.3',
          }), { headers: { 'content-type': 'application/json' } });
        }
        return bytesResponse([0, 1, 0, 0]);
      },
    };
    await resolveAndRegisterFonts([customRequest], async () => {}, {
      provider: new FontsourceProvider(options),
      memoryCache: new MemoryFontCache(),
      persistentCache,
    });
    const offlineProvider = new FontsourceProvider({
      aliases: { Custom: 'custom-font' },
      fetch: async () => {
        calls += 1;
        throw new Error('offline');
      },
    });
    const [warm] = await resolveAndRegisterFonts([customRequest], async () => {}, {
      provider: offlineProvider,
      memoryCache: new MemoryFontCache(),
      persistentCache,
    });

    assert.equal(warm.status, 'found');
    assert.equal(warm.cacheStatus, 'persistent_hit');
    assert.equal(calls, 2);
  });
});
