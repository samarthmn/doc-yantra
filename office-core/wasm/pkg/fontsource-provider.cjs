'use strict';

const { createHash } = require('node:crypto');

const DEFAULT_MAX_FONT_BYTES = 10 * 1024 * 1024;
const DEFAULT_MAX_METADATA_BYTES = 512 * 1024;
const DEFAULT_MAX_REDIRECTS = 3;
const DEFAULT_CONCURRENCY = 4;
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

const DEFAULT_ALIASES = Object.freeze({
  arial: 'arimo',
  arimo: 'arimo',
  caladea: 'caladea',
  calibri: 'carlito',
  'calibri light': 'carlito',
  cambria: 'caladea',
  carlito: 'carlito',
  cousine: 'cousine',
  'courier new': 'cousine',
  courier: 'cousine',
  georgia: 'gelasio',
  helvetica: 'arimo',
  'liberation mono': 'cousine',
  'liberation sans': 'arimo',
  'liberation serif': 'tinos',
  'segoe ui': 'arimo',
  times: 'tinos',
  'times new roman': 'tinos',
  tinos: 'tinos',
  ubuntu: 'ubuntu',
  'ubuntu mono': 'ubuntu-mono',
});

const DEFAULT_CATALOG = Object.freeze({
  arimo: catalog('Arimo', ['cyrillic', 'cyrillic-ext', 'greek', 'greek-ext', 'hebrew', 'latin', 'latin-ext', 'vietnamese'], [400, 500, 600, 700], 'v36', 'latest'),
  caladea: catalog('Caladea', ['latin', 'latin-ext'], [400, 700], 'v8', 'latest'),
  carlito: catalog('Carlito', ['cyrillic', 'cyrillic-ext', 'greek', 'greek-ext', 'latin', 'latin-ext', 'vietnamese'], [400, 700], 'v4', 'latest'),
  cousine: catalog('Cousine', ['cyrillic', 'cyrillic-ext', 'greek', 'greek-ext', 'hebrew', 'latin', 'latin-ext', 'vietnamese'], [400, 700], 'v30', 'latest'),
  gelasio: catalog('Gelasio', ['latin', 'latin-ext', 'vietnamese'], [400, 500, 600, 700], 'v14', '5.2.8'),
  tinos: catalog('Tinos', ['cyrillic', 'cyrillic-ext', 'greek', 'greek-ext', 'hebrew', 'latin', 'latin-ext', 'vietnamese'], [400, 700], 'v26', 'latest'),
  ubuntu: catalog('Ubuntu', ['cyrillic', 'cyrillic-ext', 'greek', 'greek-ext', 'latin', 'latin-ext'], [300, 400, 500, 700], 'v21', '5.2.8', 'UFL-1.0', '0.83'),
  'ubuntu-mono': catalog('Ubuntu Mono', ['cyrillic', 'cyrillic-ext', 'greek', 'greek-ext', 'latin', 'latin-ext'], [400, 700], 'v19', '5.2.8', 'UFL-1.0', 'Version 0.80'),
});

function catalog(
  family,
  subsets,
  weights,
  catalogVersion,
  packageVersion,
  license = 'OFL-1.1',
  expectedVersion,
) {
  return Object.freeze({
    family,
    subsets: Object.freeze(subsets),
    weights: Object.freeze(weights),
    styles: Object.freeze(['italic', 'normal']),
    defSubset: 'latin',
    catalogVersion,
    packageVersion,
    license,
    expectedVersion,
  });
}

function normalizeFamily(value) {
  return String(value).trim().toLocaleLowerCase('en-US').replace(/\s+/g, ' ');
}

function metadataClassId(request) {
  const pitch = String(request.pitch ?? '').trim().toLocaleLowerCase('en-US');
  if (['fixed', 'fixedpitch', 'mono', 'monospace', 'monospaced'].includes(pitch)) return 'cousine';
  const panose = String(request.panose ?? '').trim();
  if (/^[0-9a-fA-F]{20}$/.test(panose)) {
    const bytes = Array.from({ length: 10 }, (_, index) =>
      Number.parseInt(panose.slice(index * 2, index * 2 + 2), 16));
    if (bytes[3] === 9) return 'cousine';
    if (bytes[0] === 2 && bytes[1] >= 2 && bytes[1] <= 10) return 'tinos';
    if (bytes[0] === 2 && bytes[1] >= 11 && bytes[1] <= 15) return 'arimo';
  }
  const familyClass = String(request.familyClass ?? '').trim().toLocaleLowerCase('en-US');
  if (familyClass === 'roman') return 'tinos';
  if (familyClass === 'swiss') return 'arimo';
  if (familyClass === 'modern') return 'cousine';
  return undefined;
}

function validateId(value, label = 'Fontsource id') {
  if (typeof value !== 'string' || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(value)) {
    throw new TypeError(`${label} must contain lowercase ASCII letters, digits, and hyphens only`);
  }
  return value;
}

function httpsBase(value, label) {
  const url = new URL(value);
  if (url.protocol !== 'https:') throw new TypeError(`${label} must use HTTPS`);
  if (!url.pathname.endsWith('/')) url.pathname += '/';
  return url;
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function nearest(values, requested) {
  return [...values].sort((left, right) =>
    Math.abs(left - requested) - Math.abs(right - requested) || left - right)[0];
}

function coverageForSubset(subset) {
  if (subset === 'cyrillic') return { scripts: ['cyrillic'], codepoints: [] };
  if (subset === 'cyrillic-ext') return { scripts: [], codepoints: [0x0460] };
  if (subset === 'greek') return { scripts: ['greek'], codepoints: [] };
  if (subset === 'greek-ext') return { scripts: [], codepoints: [0x1f00] };
  if (subset === 'hebrew') return { scripts: ['hebrew'], codepoints: [] };
  if (subset === 'latin') return { scripts: ['latin'], codepoints: [] };
  if (subset === 'latin-ext') return { scripts: [], codepoints: [0x0100] };
  if (subset === 'vietnamese') return { scripts: [], codepoints: [0x0102] };
  return { scripts: [], codepoints: [] };
}

function subsetCoversScript(subset, script) {
  if (script === 'latin') {
    return subset === 'latin' || subset === 'latin-ext' || subset === 'vietnamese';
  }
  return subset === script || subset.startsWith(`${script}-`);
}

function subsetsFor(request, metadata) {
  const selected = new Set([metadata.defSubset]);
  for (const script of request.scripts) {
    const normalized = String(script).toLocaleLowerCase('en-US').replaceAll('_', '-');
    for (const subset of metadata.subsets) {
      if (subsetCoversScript(subset, normalized)) selected.add(subset);
    }
  }
  return [...selected].filter((subset) => metadata.subsets.includes(subset)).sort((left, right) =>
    left === metadata.defSubset ? -1 : right === metadata.defSubset ? 1 : left.localeCompare(right));
}

class HttpStatusError extends Error {
  constructor(status, url) {
    super(`font fetch returned HTTP ${status} for ${url}`);
    this.status = status;
  }
}

function assertContentType(response, kind) {
  const contentType = response.headers.get('content-type')?.split(';', 1)[0].trim().toLowerCase();
  const accepted = kind === 'json'
    ? contentType === 'application/json' || contentType?.endsWith('+json')
    : FONT_CONTENT_TYPES.has(contentType);
  if (!accepted) {
    throw new TypeError(`unexpected ${kind} response content type ${contentType ?? '(missing)'}`);
  }
}

async function responseBytes(response, maxBytes, kind = 'font') {
  assertContentType(response, kind);
  const declared = Number(response.headers.get('content-length'));
  if (Number.isFinite(declared) && declared > maxBytes) {
    throw new RangeError(`font response declares ${declared} bytes, over the ${maxBytes} byte limit`);
  }
  if (!response.body?.getReader) {
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.byteLength > maxBytes) {
      throw new RangeError(`font response is ${bytes.byteLength} bytes, over the ${maxBytes} byte limit`);
    }
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
      throw new RangeError(`font response exceeds the ${maxBytes} byte limit`);
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

async function fetchHttps(fetchImpl, initialUrl, signal, limits) {
  let url = new URL(initialUrl);
  for (let redirects = 0; ; redirects += 1) {
    if (url.protocol !== 'https:') throw new TypeError('font fetch redirects must remain HTTPS');
    const response = await fetchImpl(url, { signal, redirect: 'manual' });
    if (response.status >= 300 && response.status < 400) {
      if (redirects >= limits.maxRedirects) throw new Error('font fetch redirect limit exceeded');
      const location = response.headers.get('location');
      if (!location) throw new Error('font fetch redirect omitted Location');
      url = new URL(location, url);
      continue;
    }
    if (!response.ok) throw new HttpStatusError(response.status, url.href);
    return responseBytes(response, limits.maxFontBytes, 'font');
  }
}

async function boundedMap(values, concurrency, mapper) {
  const output = new Array(values.length);
  let next = 0;
  const workers = Array.from({ length: Math.min(concurrency, values.length) }, async () => {
    while (next < values.length) {
      const index = next++;
      output[index] = await mapper(values[index], index);
    }
  });
  await Promise.all(workers);
  return output;
}

class FontsourceProvider {
  constructor(options = {}) {
    this.id = options.id ?? 'fontsource-v1';
    if (typeof this.id !== 'string' || this.id.length === 0) {
      throw new TypeError('FontsourceProvider id must be a non-empty string');
    }
    this.fetch = options.fetch ?? globalThis.fetch;
    if (typeof this.fetch !== 'function') throw new TypeError('FontsourceProvider requires fetch');
    this.metadataBase = httpsBase(options.metadataBaseUrl ?? 'https://api.fontsource.org/v1/fonts/', 'metadataBaseUrl');
    this.cdnBase = httpsBase(options.cdnBaseUrl ?? 'https://cdn.jsdelivr.net/fontsource/fonts/', 'cdnBaseUrl');
    this.maxFontBytes = options.maxFontBytes ?? DEFAULT_MAX_FONT_BYTES;
    this.maxMetadataBytes = options.maxMetadataBytes ?? DEFAULT_MAX_METADATA_BYTES;
    this.maxRedirects = options.maxRedirects ?? DEFAULT_MAX_REDIRECTS;
    this.concurrency = options.concurrency ?? DEFAULT_CONCURRENCY;
    if (!Number.isSafeInteger(this.maxFontBytes) || this.maxFontBytes <= 0 ||
        !Number.isSafeInteger(this.maxMetadataBytes) || this.maxMetadataBytes <= 0 ||
        !Number.isSafeInteger(this.maxRedirects) || this.maxRedirects < 0 ||
        !Number.isSafeInteger(this.concurrency) || this.concurrency <= 0) {
      throw new TypeError('FontsourceProvider limits must be positive safe integers');
    }
    this.aliases = new Map(Object.entries({ ...DEFAULT_ALIASES, ...(options.aliases ?? {}) })
      .map(([family, id]) => [normalizeFamily(family), validateId(id)]));
    this.catalog = new Map(Object.entries({ ...DEFAULT_CATALOG, ...(options.catalog ?? {}) })
      .map(([id, metadata]) => [validateId(id), this.validateMetadata(id, metadata)]));
    this.fallbackSource = options.fallbackSource;
    if (this.fallbackSource !== undefined && typeof this.fallbackSource !== 'function') {
      throw new TypeError('fallbackSource must be a trusted callback');
    }
  }

  validateMetadata(id, value, discovered = false) {
    if (!value || typeof value.family !== 'string' || !Array.isArray(value.subsets) ||
        !Array.isArray(value.weights) || !Array.isArray(value.styles) ||
        typeof value.defSubset !== 'string') {
      throw new TypeError(`Fontsource catalog metadata for ${id} is invalid`);
    }
    if (value.family.trim().length === 0 ||
        value.subsets.length === 0 ||
        value.subsets.some((subset) =>
          typeof subset !== 'string' || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(subset)) ||
        value.weights.length === 0 ||
        value.weights.some((weight) =>
          !Number.isSafeInteger(weight) || weight < 1 || weight > 1_000) ||
        value.styles.length === 0 ||
        value.styles.some((style) => !['normal', 'italic'].includes(style)) ||
        !value.subsets.includes(value.defSubset)) {
      throw new TypeError(`Fontsource catalog metadata for ${id} contains unsafe values`);
    }
    const packageVersion = value.packageVersion ?? value.npmVersion ?? 'latest';
    if (!/^(?:latest|[0-9]+(?:\.[0-9]+){2})$/.test(packageVersion)) {
      throw new TypeError(`Fontsource package version for ${id} is invalid`);
    }
    return Object.freeze({
      family: value.family,
      subsets: Object.freeze([...value.subsets]),
      weights: Object.freeze([...value.weights]),
      styles: Object.freeze([...value.styles]),
      defSubset: value.defSubset,
      catalogVersion: value.catalogVersion ?? value.version ?? 'discovered',
      packageVersion,
      license: value.license ?? 'OFL-1.1',
      expectedVersion: value.expectedVersion,
      discovered,
    });
  }

  mappedId(request) {
    for (const family of [request.family, ...(request.altFamilies ?? [])]) {
      const id = this.aliases.get(normalizeFamily(family));
      if (id) return id;
    }
    return metadataClassId(request);
  }

  async metadataFor(id, signal) {
    const cached = this.catalog.get(id);
    if (cached) return cached;
    const url = new URL(id, this.metadataBase);
    const response = await this.fetch(url, { signal, redirect: 'error' });
    if (!response.ok) throw new HttpStatusError(response.status, url.href);
    const bytes = await responseBytes(response, this.maxMetadataBytes, 'json');
    const metadata = this.validateMetadata(id, JSON.parse(new TextDecoder().decode(bytes)), true);
    this.catalog.set(id, metadata);
    return metadata;
  }

  async cacheKey(request) {
    const id = this.mappedId(request);
    if (!id) return 'unmapped';
    const metadata = this.catalog.get(id);
    const catalogVersion = metadata && !metadata.discovered ? metadata.catalogVersion : 'discovery';
    const packageVersion = metadata && !metadata.discovered ? metadata.packageVersion : 'latest';
    return [id, catalogVersion, packageVersion,
      request.weight, request.italic ? 'italic' : 'normal', [...request.scripts].sort().join(',')].join('|');
  }

  async descriptorFor(request, signal) {
    const id = this.mappedId(request);
    if (!id) return undefined;
    const metadata = await this.metadataFor(id, signal);
    const weight = nearest(metadata.weights, request.weight);
    const requestedStyle = request.italic ? 'italic' : 'normal';
    const style = metadata.styles.includes(requestedStyle) ? requestedStyle : metadata.styles[0];
    return { id, metadata, weight, style, subsets: subsetsFor(request, metadata) };
  }

  artifact(descriptor, subset, bytes, source) {
    const { metadata, weight, style } = descriptor;
    const coverage = coverageForSubset(subset);
    return {
      family: metadata.family,
      fullName: `${metadata.family} ${weight} ${style}`,
      postscriptName: `${metadata.family.replaceAll(' ', '')}-${weight}-${style}`,
      weight,
      italic: style === 'italic',
      stretch: 5,
      version: metadata.catalogVersion,
      ...(metadata.expectedVersion ? { expectedVersion: metadata.expectedVersion } : {}),
      faceIndex: 0,
      format: 'ttf',
      sha256: sha256(bytes),
      source,
      coverageScripts: coverage.scripts,
      coverageCodepoints: coverage.codepoints,
      subset,
      license: { id: metadata.license, embeddable: true, subsettable: true },
      bytes,
    };
  }

  primaryUrl(descriptor, subset) {
    const filename = `${subset}-${descriptor.weight}-${descriptor.style}.ttf`;
    return new URL(`${descriptor.id}@${descriptor.metadata.packageVersion}/${filename}`, this.cdnBase);
  }

  async fetchArtifact(descriptor, subset, signal) {
    const primary = this.primaryUrl(descriptor, subset);
    try {
      const bytes = await fetchHttps(this.fetch, primary, signal, this);
      return this.artifact(descriptor, subset, bytes, primary.href);
    } catch (primaryError) {
      if (!this.fallbackSource) throw primaryError;
      const fallback = new URL(await this.fallbackSource({ ...descriptor, subset }));
      if (fallback.protocol !== 'https:') throw new TypeError('fallbackSource must return HTTPS URLs');
      const bytes = await fetchHttps(this.fetch, fallback, signal, this);
      return this.artifact(descriptor, subset, bytes, fallback.href);
    }
  }

  async resolve(requests, signal) {
    return boundedMap(requests, this.concurrency, async (request) => {
      try {
        const descriptor = await this.descriptorFor(request, signal);
        if (!descriptor) return { status: 'not_found', request };
        const artifacts = [];
        for (const subset of descriptor.subsets) {
          artifacts.push(await this.fetchArtifact(descriptor, subset, signal));
        }
        return { status: 'found', request, artifact: artifacts[0], artifacts };
      } catch (error) {
        if (signal.aborted) throw signal.reason ?? error;
        if (error instanceof HttpStatusError && error.status === 404) {
          return { status: 'not_found', request };
        }
        return {
          status: 'temporary_failure',
          request,
          reason: String(error?.message ?? error),
        };
      }
    });
  }
}

module.exports = {
  DEFAULT_FONTSOURCE_ALIASES: DEFAULT_ALIASES,
  DEFAULT_FONTSOURCE_CATALOG: DEFAULT_CATALOG,
  FontsourceProvider,
};
