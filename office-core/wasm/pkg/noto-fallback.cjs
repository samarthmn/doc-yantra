'use strict';

const { createHash } = require('node:crypto');
const { resolveAndRegisterFonts } = require('./font-provider.cjs');

const DEFAULT_MAX_FONT_BYTES = 2 * 1024 * 1024;
const DEFAULT_MAX_REDIRECTS = 3;
const FALLBACK_BASE_URL = 'https://public.dockosha.com/fallback-fonts/NotoSans/';
const FONT_CONTENT_TYPES = new Set([
  'application/font-sfnt',
  'application/octet-stream',
  'application/vnd.ms-opentype',
  'application/x-font-ttf',
  'binary/octet-stream',
  'font/otf',
  'font/sfnt',
  'font/ttf',
]);

// This is one logical fallback family. The Symbols companions are sibling
// coverage faces selected by the engine only when Noto Sans lacks a glyph.
// Hashes and name-table versions were verified against the first-party assets
// on 2026-07-15. No font bytes are bundled in the npm package.
const NOTO_FALLBACK_MANIFEST = Object.freeze([
  face('Noto Sans', 'Noto Sans Regular', 'NotoSans-Regular', 'Regular.ttf', 400, false,
    'Version 2.015', 'fe8c022f48d8dd29f17b744d16f9346f4357e16f7d4f7be58b000ae7c291b614',
    ['latin', 'cyrillic', 'greek'],
    [0x2022, 0x2013, 0x2014, 0x2018, 0x2019, 0x201c, 0x201d, 0x2070, 0x2079, 0x207b, 0x0416, 0x03a9, 0x2043]),
  face('Noto Sans', 'Noto Sans Bold', 'NotoSans-Bold', 'Bold.ttf', 700, false,
    'Version 2.015', '13a813c49624ae3ba3c5c6e72c5ebffc4b9e1e6ea32f421c04069b037c6ad431',
    ['latin', 'cyrillic', 'greek']),
  face('Noto Sans', 'Noto Sans Italic', 'NotoSans-Italic', 'Italic.ttf', 400, true,
    'Version 2.015', 'feb3e57888ce3acb604f270c278cfb304c674306518771619ddbdefca42a066b',
    ['latin', 'cyrillic', 'greek']),
  face('Noto Sans', 'Noto Sans Bold Italic', 'NotoSans-BoldItalic', 'BoldItalic.ttf', 700, true,
    'Version 2.015', '7e34616e092593c340449baf9a259075a054c6419511ccf21ff0d83a1c9dd0d0',
    ['latin', 'cyrillic', 'greek']),
  face('Noto Sans Symbols', 'Noto Sans Symbols Regular', 'NotoSansSymbols-Regular',
    'NotoSansSymbols-Regular.ttf', 400, false, 'Version 2.003',
    'aedeec1cd0514930aeeafc4a88a6deff83cda1e6b58086f0b9bb9c7dd0157578', [], [0x2192]),
  face('Noto Sans Symbols 2', 'Noto Sans Symbols 2 Regular', 'NotoSansSymbols2-Regular',
    'NotoSansSymbols2-Regular.ttf', 400, false,
    'Version 2.008; ttfautohint (v1.8.4.7-5d5b)',
    'f67f40c65f929f029827ab37cfdc4897dfd9ae3c28d9e98ba46e0d52e39eb97e', [],
    [0x25cf, 0x25aa, 0x25a0, 0x25cb, 0x2666, 0x25ba, 0x2b9a, 0x2219, 0x25e6]),
]);

function face(
  family,
  fullName,
  postscriptName,
  file,
  weight,
  italic,
  expectedVersion,
  sha256,
  coverageScripts = [],
  coverageCodepoints = [],
) {
  return Object.freeze({
    family,
    fullName,
    postscriptName,
    file,
    weight,
    italic,
    stretch: 5,
    expectedVersion,
    sha256,
    coverageScripts: Object.freeze(coverageScripts),
    coverageCodepoints: Object.freeze(coverageCodepoints),
  });
}

function requestFor(entry) {
  return Object.freeze({
    family: entry.family,
    altFamilies: [],
    weight: entry.weight,
    italic: entry.italic,
    stretch: entry.stretch,
    slots: ['ascii'],
    panose: null,
    familyClass: null,
    pitch: null,
    charset: null,
    unicodeRanges: [],
    scripts: [...entry.coverageScripts],
    vfCoords: [],
  });
}

const NOTO_FALLBACK_REQUESTS = Object.freeze(NOTO_FALLBACK_MANIFEST.map(requestFor));

class HttpStatusError extends Error {
  constructor(status, url) {
    super(`fallback font fetch returned HTTP ${status} for ${url}`);
    this.status = status;
  }
}

async function responseBytes(response, maxBytes) {
  const contentType = response.headers.get('content-type')?.split(';', 1)[0].trim().toLowerCase();
  if (!FONT_CONTENT_TYPES.has(contentType)) {
    throw new TypeError(`unexpected font response content type ${contentType ?? '(missing)'}`);
  }
  const declared = Number(response.headers.get('content-length'));
  if (Number.isFinite(declared) && declared > maxBytes) {
    throw new RangeError(`fallback font declares ${declared} bytes, over the ${maxBytes} byte limit`);
  }
  if (!response.body?.getReader) {
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.byteLength > maxBytes) throw new RangeError('fallback font exceeds the byte limit');
    return bytes;
  }
  const reader = response.body.getReader();
  const chunks = [];
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > maxBytes) {
      await reader.cancel();
      throw new RangeError('fallback font exceeds the byte limit');
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

async function fetchHttps(fetchImpl, initialUrl, signal, maxRedirects, maxBytes) {
  let url = new URL(initialUrl);
  for (let redirects = 0; ; redirects += 1) {
    if (url.protocol !== 'https:') throw new TypeError('fallback font URLs must remain HTTPS');
    const response = await fetchImpl(url, { signal, redirect: 'manual' });
    if (response.status >= 300 && response.status < 400) {
      if (redirects >= maxRedirects) throw new Error('fallback font redirect limit exceeded');
      const location = response.headers.get('location');
      if (!location) throw new Error('fallback font redirect omitted Location');
      url = new URL(location, url);
      continue;
    }
    if (!response.ok) throw new HttpStatusError(response.status, url.href);
    return { bytes: await responseBytes(response, maxBytes), source: url.href };
  }
}

class NotoFallbackProvider {
  constructor(options = {}) {
    this.id = options.id ?? 'dockosha-noto-fallback-2026-07-15';
    this.fetch = options.fetch ?? globalThis.fetch;
    this.baseUrl = new URL(options.baseUrl ?? FALLBACK_BASE_URL);
    this.maxFontBytes = options.maxFontBytes ?? DEFAULT_MAX_FONT_BYTES;
    this.maxRedirects = options.maxRedirects ?? DEFAULT_MAX_REDIRECTS;
    this.manifest = Object.freeze([...(options.manifest ?? NOTO_FALLBACK_MANIFEST)]);
    if (typeof this.id !== 'string' || this.id.length === 0 || typeof this.fetch !== 'function') {
      throw new TypeError('NotoFallbackProvider requires a non-empty id and fetch implementation');
    }
    if (this.baseUrl.protocol !== 'https:' ||
        !Number.isSafeInteger(this.maxFontBytes) || this.maxFontBytes <= 0 ||
        !Number.isSafeInteger(this.maxRedirects) || this.maxRedirects < 0) {
      throw new TypeError('NotoFallbackProvider requires HTTPS and valid limits');
    }
    if (this.manifest.length === 0 || this.manifest.some((entry) =>
      !entry || typeof entry.file !== 'string' || !/^[A-Za-z0-9-]+\.ttf$/.test(entry.file) ||
      typeof entry.sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(entry.sha256))) {
      throw new TypeError('Noto fallback manifest is invalid');
    }
    this.byIdentity = new Map(this.manifest.map((entry) => [identityKey(entry), entry]));
  }

  cacheKey(request) {
    return this.byIdentity.get(identityKey(request))?.sha256 ?? 'unmapped';
  }

  async resolve(requests, signal) {
    return Promise.all(requests.map(async (request) => {
      const entry = this.byIdentity.get(identityKey(request));
      if (!entry) return { status: 'not_found', request };
      try {
        const url = new URL(entry.file, this.baseUrl);
        const { bytes, source } = await fetchHttps(
          this.fetch, url, signal, this.maxRedirects, this.maxFontBytes,
        );
        const actual = createHash('sha256').update(bytes).digest('hex');
        if (actual !== entry.sha256) {
          const error = new Error(`fallback font hash mismatch: expected ${entry.sha256}, received ${actual}`);
          error.code = 'font_hash_mismatch';
          throw error;
        }
        const artifact = {
          family: entry.family,
          fullName: entry.fullName,
          postscriptName: entry.postscriptName,
          weight: entry.weight,
          italic: entry.italic,
          stretch: entry.stretch,
          version: entry.expectedVersion,
          expectedVersion: entry.expectedVersion,
          faceIndex: 0,
          format: 'ttf',
          sha256: entry.sha256,
          source,
          coverageScripts: [...entry.coverageScripts],
          coverageCodepoints: [...entry.coverageCodepoints],
          license: { id: 'OFL-1.1', embeddable: true, subsettable: true },
          bytes,
        };
        return { status: 'found', request, artifact };
      } catch (error) {
        if (signal.aborted) throw signal.reason ?? error;
        if (error instanceof HttpStatusError && error.status === 404) {
          return { status: 'not_found', request };
        }
        return {
          status: 'temporary_failure',
          request,
          reason: String(error?.message ?? error),
          ...(error?.code ? { failureCode: error.code } : {}),
        };
      }
    }));
  }
}

function identityKey(value) {
  return [
    String(value.family).trim().toLocaleLowerCase('en-US'),
    value.weight,
    value.italic ? 1 : 0,
    value.stretch ?? 5,
  ].join('|');
}

async function registerNotoFallbackFonts(register, options = {}) {
  const provider = options.provider ?? new NotoFallbackProvider(options.providerOptions);
  return resolveAndRegisterFonts(NOTO_FALLBACK_REQUESTS, register, {
    ...options,
    provider,
    maxFontBytes: options.maxFontBytes ?? DEFAULT_MAX_FONT_BYTES,
  });
}

module.exports = {
  FALLBACK_BASE_URL,
  NOTO_FALLBACK_MANIFEST,
  NOTO_FALLBACK_REQUESTS,
  NotoFallbackProvider,
  registerNotoFallbackFonts,
};
