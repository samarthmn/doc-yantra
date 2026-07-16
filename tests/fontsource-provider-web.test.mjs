import assert from 'node:assert/strict';
import { describe, test } from 'node:test';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');

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

describe('browser FontsourceProvider', () => {
  test('uses the same pinned immutable paths and subset union as Node', async () => {
    const modulePath = join(ROOT, 'office-core', 'wasm', 'pkg', 'web', 'fontsource-provider.js');
    const { FontsourceProvider } = await import(pathToFileURL(modulePath));
    const urls = [];
    const provider = new FontsourceProvider({
      fetch: async (url) => {
        urls.push(String(url));
        return new Response(Uint8Array.of(0, 1, 0, urls.length), {
          headers: { 'content-type': 'font/ttf' },
        });
      },
    });
    const [result] = await provider.resolve([
      request({ scripts: ['latin', 'greek', 'cyrillic'] }),
    ], new AbortController().signal);

    assert.equal(result.status, 'found');
    assert.deepEqual(result.artifacts.map(({ subset }) => subset), [
      'latin', 'cyrillic', 'cyrillic-ext', 'greek', 'greek-ext', 'latin-ext',
    ]);
    assert.deepEqual(result.artifacts.map(({ coverageCodepoints }) => coverageCodepoints), [
      [], [], [0x0460], [], [0x1f00], [0x0100],
    ]);
    assert.equal(urls[0], 'https://cdn.jsdelivr.net/fontsource/fonts/ubuntu@5.2.8/latin-400-normal.ttf');
    assert.equal(result.artifact.expectedVersion, '0.83');
  });

  test('rejects an HTML 200 response before treating it as a font', async () => {
    const modulePath = join(ROOT, 'office-core', 'wasm', 'pkg', 'web', 'fontsource-provider.js');
    const { FontsourceProvider } = await import(pathToFileURL(modulePath));
    const provider = new FontsourceProvider({
      fetch: async () => new Response('<html>not a font</html>', {
        headers: { 'content-type': 'text/html' },
      }),
    });

    const [result] = await provider.resolve([request()], new AbortController().signal);
    assert.equal(result.status, 'temporary_failure');
    assert.match(result.reason, /content type/i);
  });

  test('browser joiners enforce independent timeouts on one shared fetch', async () => {
    const modulePath = join(ROOT, 'office-core', 'wasm', 'pkg', 'web', 'font-provider.js');
    const { MemoryFontCache, resolveAndRegisterFonts } = await import(pathToFileURL(modulePath));
    const bytes = Uint8Array.of(1, 2, 3);
    const digest = await globalThis.crypto.subtle.digest('SHA-256', bytes);
    const hash = [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, '0')).join('');
    let calls = 0;
    const provider = {
      id: 'web-concurrent-timeout',
      async resolve(requests) {
        calls += 1;
        await new Promise((resolve) => setTimeout(resolve, 60));
        return requests.map((requested) => ({
          status: 'found',
          request: requested,
          artifact: {
            family: 'Ubuntu', fullName: 'Ubuntu Regular', postscriptName: 'Ubuntu-Regular',
            weight: 400, italic: false, stretch: 5, version: 'fixture', faceIndex: 0,
            format: 'ttf', sha256: hash, source: 'fixture',
            license: { id: 'UFL-1.0', embeddable: true, subsettable: true }, bytes,
          },
        }));
      },
    };
    const first = resolveAndRegisterFonts([request()], async () => {}, {
      provider, memoryCache: new MemoryFontCache(), timeoutMs: 200,
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    const [timedOut] = await resolveAndRegisterFonts([request()], async () => {}, {
      provider, memoryCache: new MemoryFontCache(), timeoutMs: 10, negativeTtlMs: 0,
    });

    assert.equal(timedOut.status, 'temporary_failure');
    assert.match(timedOut.reason, /timeout/i);
    assert.equal((await first)[0].status, 'found');
    assert.equal(calls, 1);
  });

  test('browser joiner abort does not poison shared resolution', async () => {
    const modulePath = join(ROOT, 'office-core', 'wasm', 'pkg', 'web', 'font-provider.js');
    const { MemoryFontCache, resolveAndRegisterFonts } = await import(pathToFileURL(modulePath));
    const bytes = Uint8Array.of(4, 5, 6);
    const digest = await globalThis.crypto.subtle.digest('SHA-256', bytes);
    const hash = [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, '0')).join('');
    let calls = 0;
    const provider = {
      id: 'web-concurrent-abort',
      async resolve(requests) {
        calls += 1;
        await new Promise((resolve) => setTimeout(resolve, 40));
        return requests.map((requested) => ({
          status: 'found',
          request: requested,
          artifact: {
            family: 'Ubuntu', fullName: 'Ubuntu Regular', postscriptName: 'Ubuntu-Regular',
            weight: 400, italic: false, stretch: 5, version: 'fixture', faceIndex: 0,
            format: 'ttf', sha256: hash, source: 'fixture',
            license: { id: 'UFL-1.0', embeddable: true, subsettable: true }, bytes,
          },
        }));
      },
    };
    const first = resolveAndRegisterFonts([request()], async () => {}, {
      provider, memoryCache: new MemoryFontCache(),
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    const controller = new AbortController();
    const joining = resolveAndRegisterFonts([request()], async () => {}, {
      provider, memoryCache: new MemoryFontCache(), signal: controller.signal,
    });
    controller.abort(new Error('browser joiner cancelled'));

    await assert.rejects(joining, /browser joiner cancelled/);
    assert.equal((await first)[0].status, 'found');
    assert.equal(calls, 1);
  });
});
