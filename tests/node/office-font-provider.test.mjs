import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { describe, test } from 'node:test';
import { createRequire } from 'node:module';
import { readFileSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const {
  MemoryFontCache,
  NodeFontCache,
  createConvertOnDemand,
  resolveAndRegisterFonts,
} = require('../../office-core/wasm/font-provider.cjs');
const { createMockOfficeFontProvider } = require('./fixtures/mock-office-font-provider.cjs');

const request = Object.freeze({
  family: 'Example Sans',
  altFamilies: [],
  weight: 400,
  italic: false,
  stretch: 5,
  slots: ['ascii'],
  panose: null,
  familyClass: 'swiss',
  pitch: 'variable',
  charset: '00',
  unicodeRanges: [1],
  scripts: ['latin'],
  vfCoords: [],
});

function artifact(bytes = Uint8Array.from([1, 2, 3])) {
  return {
    family: 'Example Sans',
    fullName: 'Example Sans Regular',
    postscriptName: 'ExampleSans-Regular',
    weight: 400,
    italic: false,
    stretch: 5,
    version: 'Version 1.0',
    faceIndex: 0,
    format: 'ttf',
    sha256: createHash('sha256').update(bytes).digest('hex'),
    source: 'mock',
    license: { id: 'OFL-1.1', embeddable: true, subsettable: true },
    bytes,
  };
}

function fakeEngine() {
  const registrations = [];
  return {
    registrations,
    office_font_requests: () => JSON.stringify([request]),
    office_register_font: (bytes, expectation) => {
      registrations.push({ bytes: Uint8Array.from(bytes), expectation: JSON.parse(expectation) });
      return JSON.stringify({ sha256: createHash('sha256').update(bytes).digest('hex') });
    },
    office_profile_with_options: (_bytes, _extension, options) => JSON.stringify({
      supported: true,
      options: JSON.parse(options),
      fonts: [],
    }),
    office_to_pdf_with_options: () => Uint8Array.from([0x25, 0x50, 0x44, 0x46]),
  };
}

describe('office font provider orchestration', () => {
  test('built fontless package converts through the deterministic mock provider', async () => {
    const engine = require('../../office-core/wasm/pkg');
    const bytes = readFileSync(join(HERE, '..', '..', 'tests/fixtures/samples', 'docx', 'file1.docx'));
    const before = JSON.parse(engine.office_profile(bytes, 'docx'));

    assert.equal(before.supported, false);
    assert.ok(before.reasons.some((reason) => reason.code === 'font_unresolved'));
    const output = await engine.convertOnDemand(bytes, 'docx', {
      provider: createMockOfficeFontProvider(),
      mode: 'compatible',
      memoryCache: new engine.MemoryFontCache(),
    });
    assert.equal(Buffer.from(output.pdf.subarray(0, 5)).toString(), '%PDF-');
    assert.equal(output.profile.supported, true);
    assert.ok(output.providerResults.every((result) => result.status === 'found'));
    assert.ok(output.fonts.every((font) => font.source === 'provider'));
    assert.ok(output.fonts.every((font) => font.cacheStatus === 'miss'));
    assert.ok(output.fonts.every((font) => font.events.includes('font_fetched')));
    assert.ok(output.fonts.every((font) => typeof font.selected.sha256 === 'string'));
  });

  test('cold conversion batches once and warm conversion uses the content cache', async () => {
    const engine = fakeEngine();
    const memoryCache = new MemoryFontCache();
    let calls = 0;
    const provider = {
      id: 'mock-v1',
      async resolve(requests) {
        calls += 1;
        assert.deepEqual(requests, [request]);
        return [{ status: 'found', request, artifact: artifact() }];
      },
    };
    const convertOnDemand = createConvertOnDemand(engine);

    const cold = await convertOnDemand(Uint8Array.from([9]), 'docx', {
      provider,
      mode: 'strict',
      memoryCache,
    });
    const warm = await convertOnDemand(Uint8Array.from([9]), 'docx', {
      provider,
      mode: 'strict',
      memoryCache,
    });

    assert.deepEqual(cold.pdf, Uint8Array.from([0x25, 0x50, 0x44, 0x46]));
    assert.equal(cold.profile.options.mode, 'strict');
    assert.equal(calls, 1);
    assert.equal(engine.registrations.length, 2);
    assert.equal(cold.providerResults[0].cacheStatus, 'miss');
    assert.equal(warm.providerResults[0].cacheStatus, 'memory_hit');
  });

  test('concurrent resolution shares one in-flight provider promise', async () => {
    const memoryCache = new MemoryFontCache();
    let calls = 0;
    let release;
    const provider = {
      id: 'mock-concurrent',
      resolve: async () => {
        calls += 1;
        await new Promise((resolve) => { release = resolve; });
        return [{ status: 'found', request, artifact: artifact() }];
      },
    };
    const registered = [];
    const register = async (bytes) => registered.push(bytes.byteLength);

    const first = resolveAndRegisterFonts([request], register, { provider, memoryCache });
    const second = resolveAndRegisterFonts([request], register, { provider, memoryCache });
    await new Promise((resolve) => setImmediate(resolve));
    release();
    await Promise.all([first, second]);

    assert.equal(calls, 1);
    assert.deepEqual(registered, [3, 3]);
  });

  test('a joining caller enforces its own timeout without cancelling shared resolution', async () => {
    let calls = 0;
    const provider = {
      id: 'mock-concurrent-timeout',
      async resolve(requests) {
        calls += 1;
        await new Promise((resolve) => setTimeout(resolve, 60));
        return requests.map((requested) => ({
          status: 'found', request: requested, artifact: artifact(),
        }));
      },
    };
    const first = resolveAndRegisterFonts([request], async () => {}, {
      provider,
      memoryCache: new MemoryFontCache(),
      timeoutMs: 200,
    });
    await new Promise((resolve) => setImmediate(resolve));
    const startedAt = Date.now();
    const [timedOut] = await resolveAndRegisterFonts([request], async () => {}, {
      provider,
      memoryCache: new MemoryFontCache(),
      timeoutMs: 10,
      negativeTtlMs: 0,
    });
    const timeoutElapsedMs = Date.now() - startedAt;
    const [resolved] = await first;

    assert.equal(timedOut.status, 'temporary_failure');
    assert.match(timedOut.reason, /timeout/i);
    assert.ok(timeoutElapsedMs < 55, 'joining timeout must not wait for the shared fetch');
    assert.equal(resolved.status, 'found');
    assert.equal(calls, 1);
  });

  test('an aborted joining caller does not poison shared resolution', async () => {
    let calls = 0;
    const provider = {
      id: 'mock-concurrent-abort',
      async resolve(requests) {
        calls += 1;
        await new Promise((resolve) => setTimeout(resolve, 40));
        return requests.map((requested) => ({
          status: 'found', request: requested, artifact: artifact(),
        }));
      },
    };
    const first = resolveAndRegisterFonts([request], async () => {}, {
      provider,
      memoryCache: new MemoryFontCache(),
    });
    await new Promise((resolve) => setImmediate(resolve));
    const controller = new AbortController();
    const joining = resolveAndRegisterFonts([request], async () => {}, {
      provider,
      memoryCache: new MemoryFontCache(),
      signal: controller.signal,
    });
    controller.abort(new Error('joining caller cancelled'));

    await assert.rejects(joining, /joining caller cancelled/);
    assert.equal((await first)[0].status, 'found');
    assert.equal(calls, 1);
  });

  test('not_found, denied, and temporary_failure remain distinct', async () => {
    for (const result of [
      { status: 'not_found', request },
      { status: 'denied', request, reason: 'license' },
      { status: 'temporary_failure', request, reason: 'timeout' },
    ]) {
      const provider = { id: `mock-${result.status}`, resolve: async () => [result] };
      const outcomes = await resolveAndRegisterFonts([request], async () => {
        assert.fail('non-found result must not register');
      }, { provider, memoryCache: new MemoryFontCache(), negativeTtlMs: 0 });
      assert.equal(outcomes[0].status, result.status);
    }
  });

  test('an aborted signal reaches the provider and rejects the caller', async () => {
    const controller = new AbortController();
    const provider = {
      id: 'mock-abort',
      resolve: (_requests, signal) => new Promise((_resolve, reject) => {
        signal.addEventListener('abort', () => reject(signal.reason), { once: true });
      }),
    };
    const pending = resolveAndRegisterFonts([request], async () => {}, {
      provider,
      memoryCache: new MemoryFontCache(),
      signal: controller.signal,
    });
    controller.abort(new Error('cancelled'));
    await assert.rejects(pending, /cancelled/);
  });

  test('catalog rotation invalidates request index and corrupt bytes are evicted', async () => {
    const memoryCache = new MemoryFontCache();
    let revision = 'one';
    let calls = 0;
    const provider = {
      id: 'mock-rotation',
      cacheKey: () => revision,
      resolve: async () => {
        calls += 1;
        return [{ status: 'found', request, artifact: artifact(Uint8Array.from([calls, 2, 3])) }];
      },
    };
    const register = async () => {};

    await resolveAndRegisterFonts([request], register, { provider, memoryCache });
    await resolveAndRegisterFonts([request], register, { provider, memoryCache });
    assert.equal(calls, 1);

    revision = 'two';
    await resolveAndRegisterFonts([request], register, { provider, memoryCache });
    assert.equal(calls, 2);

    const index = [...memoryCache.requests.values()][0];
    memoryCache.artifacts.set(index.artifacts[0].sha256, Uint8Array.from([0xff]));
    await resolveAndRegisterFonts([request], register, { provider, memoryCache });
    assert.equal(calls, 3);
  });

  test('one request registers and content-caches every supplied subset artifact', async () => {
    const memoryCache = new MemoryFontCache();
    const latin = artifact(Uint8Array.from([1, 2, 3]));
    const greek = {
      ...artifact(Uint8Array.from([4, 5, 6])),
      coverageScripts: ['greek'],
    };
    latin.coverageScripts = ['latin'];
    let calls = 0;
    const provider = {
      id: 'mock-subsets',
      resolve: async () => {
        calls += 1;
        return [{ status: 'found', request, artifact: latin, artifacts: [latin, greek] }];
      },
    };
    const registrations = [];
    const register = async (bytes, expectation) => registrations.push({
      bytes: [...bytes],
      expectation: JSON.parse(expectation),
    });

    const cold = await resolveAndRegisterFonts([request], register, { provider, memoryCache });
    const warm = await resolveAndRegisterFonts([request], register, { provider, memoryCache });

    assert.equal(calls, 1);
    assert.equal(cold[0].artifacts.length, 2);
    assert.equal(warm[0].cacheStatus, 'memory_hit');
    assert.deepEqual(registrations.map(({ bytes }) => bytes), [
      [1, 2, 3], [4, 5, 6], [1, 2, 3], [4, 5, 6],
    ]);
    assert.deepEqual(registrations.map(({ expectation }) => expectation.requiredScripts), [
      ['latin'], ['greek'], ['latin'], ['greek'],
    ]);
  });

  test('cache identity keeps different coverage requests separate without provider cacheKey', async () => {
    const memoryCache = new MemoryFontCache();
    const greekRequest = Object.freeze({
      ...request,
      slots: ['hAnsi'],
      unicodeRanges: [7],
      scripts: ['greek'],
    });
    let calls = 0;
    const provider = {
      id: 'mock-coverage-keys',
      resolve: async (requests) => {
        calls += 1;
        return requests.map((requested) => ({
          status: 'found',
          request: requested,
          artifact: artifact(Uint8Array.from([calls, 2, 3])),
        }));
      },
    };

    await resolveAndRegisterFonts([request], async () => {}, { provider, memoryCache });
    const second = await resolveAndRegisterFonts([greekRequest], async () => {}, {
      provider,
      memoryCache,
    });

    assert.equal(calls, 2);
    assert.equal(second[0].cacheStatus, 'miss');
  });

  test('offline warm succeeds while offline cold is a deterministic temporary failure', async () => {
    const memoryCache = new MemoryFontCache();
    let offline = false;
    let calls = 0;
    const provider = {
      id: 'mock-offline',
      resolve: async () => {
        calls += 1;
        if (offline) throw new Error('offline');
        return [{ status: 'found', request, artifact: artifact() }];
      },
    };
    await resolveAndRegisterFonts([request], async () => {}, { provider, memoryCache });
    offline = true;
    const warm = await resolveAndRegisterFonts([request], async () => {}, { provider, memoryCache });
    assert.equal(warm[0].status, 'found');
    assert.equal(calls, 1);

    const cold = await resolveAndRegisterFonts([request], async () => {}, {
      provider: { ...provider, id: 'mock-offline-cold' },
      memoryCache: new MemoryFontCache(),
      negativeTtlMs: 0,
    });
    assert.equal(cold[0].status, 'temporary_failure');
    assert.match(cold[0].reason, /offline/);
  });

  test('Node persistent cache serves a fresh process cache without calling an offline provider', async () => {
    const directory = join(HERE, '..', '..', 'tmp', `office-font-cache-${process.pid}`);
    rmSync(directory, { recursive: true, force: true });
    let calls = 0;
    let offline = false;
    const provider = {
      id: 'mock-persistent',
      cacheKey: () => 'catalog-v1',
      resolve: async () => {
        calls += 1;
        if (offline) throw new Error('offline');
        return [{ status: 'found', request, artifact: artifact() }];
      },
    };
    try {
      const persistentCache = new NodeFontCache(directory);
      await resolveAndRegisterFonts([request], async () => {}, {
        provider,
        memoryCache: new MemoryFontCache(),
        persistentCache,
      });
      offline = true;
      const warm = await resolveAndRegisterFonts([request], async () => {}, {
        provider,
        memoryCache: new MemoryFontCache(),
        persistentCache,
      });
      assert.equal(calls, 1);
      assert.equal(warm[0].cacheStatus, 'persistent_hit');
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  test('validated registration preserves identity mismatch as a machine failure code', async () => {
    const engine = require('../../office-core/wasm/pkg');
    const bytes = readFileSync(join(HERE, '..', '..', 'office-core', 'fonts', 'carlito', 'Carlito-Regular.ttf'));
    const provider = {
      id: 'mock-wrong-identity',
      resolve: async () => [{
        status: 'found',
        request,
        artifact: {
          ...artifact(bytes),
          family: request.family,
          fullName: `${request.family} Regular`,
          postscriptName: 'ExampleSans-Regular',
        },
      }],
    };
    const outcomes = await resolveAndRegisterFonts(
      [request],
      (fontBytes, expectation) => engine.office_register_font(fontBytes, expectation),
      { provider, memoryCache: new MemoryFontCache(), negativeTtlMs: 0 },
    );
    assert.equal(outcomes[0].status, 'temporary_failure');
    assert.equal(outcomes[0].failureCode, 'font_identity_mismatch');
  });

  test('artifact coverage rejects surrogate code points before registration', async () => {
    const provider = {
      id: 'mock-surrogate-coverage',
      resolve: async () => [{
        status: 'found',
        request,
        artifact: { ...artifact(), coverageCodepoints: [0xd800] },
      }],
    };
    let registrations = 0;
    const [outcome] = await resolveAndRegisterFonts([request], async () => {
      registrations += 1;
    }, { provider, memoryCache: new MemoryFontCache(), negativeTtlMs: 0 });

    assert.equal(outcome.status, 'temporary_failure');
    assert.match(outcome.reason, /Unicode scalar values/);
    assert.equal(registrations, 0);
  });

  test('provider timeout is reported as temporary_failure without changing mode', async () => {
    const provider = {
      id: 'mock-timeout',
      resolve: async (_requests, signal) => new Promise((_resolve, reject) => {
        signal.addEventListener('abort', () => reject(signal.reason), { once: true });
      }),
    };
    const outcomes = await resolveAndRegisterFonts([request], async () => {}, {
      provider,
      memoryCache: new MemoryFontCache(),
      timeoutMs: 10,
      negativeTtlMs: 0,
    });
    assert.equal(outcomes[0].status, 'temporary_failure');
    assert.match(outcomes[0].reason, /timeout/i);
  });
});
