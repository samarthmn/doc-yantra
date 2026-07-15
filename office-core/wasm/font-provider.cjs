'use strict';

const { createHash, randomUUID } = require('node:crypto');
const { mkdir, readFile, rename, unlink, writeFile } = require('node:fs/promises');
const { isAbsolute, join } = require('node:path');

const DEFAULT_MAX_FONT_BYTES = 10 * 1024 * 1024;
const DEFAULT_NEGATIVE_TTL_MS = 5_000;
const IN_FLIGHT = new Map();
const REQUEST_KEYS = new Set([
  'family', 'altFamilies', 'weight', 'italic', 'stretch', 'slots', 'panose',
  'familyClass', 'pitch', 'charset', 'unicodeRanges', 'scripts', 'vfCoords',
]);

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function requestKey(request, providerId) {
  return [
    providerId,
    request.family.trim().toLocaleLowerCase('en-US'),
    request.weight,
    request.italic ? 1 : 0,
    request.stretch,
  ].join('|');
}

function requestIdentityKey(request) {
  return [
    request.family.trim().toLocaleLowerCase('en-US'),
    request.weight,
    request.italic ? 1 : 0,
    request.stretch,
  ].join('|');
}

function canonicalRequest(value) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new TypeError('font request must be an object');
  }
  for (const key of Object.keys(value)) {
    if (!REQUEST_KEYS.has(key)) throw new TypeError(`font request contains unsupported field ${key}`);
  }
  if (typeof value.family !== 'string' || value.family.trim().length === 0 ||
      !Number.isInteger(value.weight) || value.weight < 1 || value.weight > 1_000 ||
      typeof value.italic !== 'boolean' ||
      !Number.isInteger(value.stretch) || value.stretch < 1 || value.stretch > 9) {
    throw new TypeError('font request identity is invalid');
  }
  const arrays = ['altFamilies', 'slots', 'unicodeRanges', 'scripts', 'vfCoords'];
  for (const key of arrays) {
    if (!Array.isArray(value[key])) throw new TypeError(`font request ${key} must be an array`);
  }
  return Object.freeze({
    family: value.family,
    altFamilies: [...value.altFamilies],
    weight: value.weight,
    italic: value.italic,
    stretch: value.stretch,
    slots: [...value.slots],
    panose: value.panose ?? null,
    familyClass: value.familyClass ?? null,
    pitch: value.pitch ?? null,
    charset: value.charset ?? null,
    unicodeRanges: [...value.unicodeRanges],
    scripts: [...value.scripts],
    vfCoords: [...value.vfCoords],
  });
}

function bytesView(value) {
  if (value instanceof Uint8Array) return new Uint8Array(value);
  if (value instanceof ArrayBuffer) return new Uint8Array(value.slice(0));
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength));
  }
  throw new TypeError('font artifact bytes must be an ArrayBuffer or typed array');
}

function artifactMetadata(artifact) {
  const { bytes: _bytes, url: _url, ...metadata } = artifact;
  return structuredClone(metadata);
}

function registrationFailureCode(error) {
  if (typeof error?.code === 'string') return error.code;
  if (typeof error?.message !== 'string' || error.message[0] !== '{') {
    return 'font_registration_failed';
  }
  try {
    const envelope = JSON.parse(error.message);
    if (envelope?.dy === 1 && typeof envelope.detail?.reason === 'string') {
      return envelope.detail.reason;
    }
  } catch {
    // Non-envelope errors keep the stable generic registration code.
  }
  return 'font_registration_failed';
}

function validateArtifact(value, maxFontBytes) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new TypeError('font artifact must be an object');
  }
  if (!['ttf', 'otf', 'ttc', 'otc'].includes(value.format)) {
    throw new TypeError(`unsupported provider font format ${value.format}`);
  }
  if (value.url !== undefined) {
    throw new TypeError('providers must return verified bytes; document-derived URLs are forbidden');
  }
  const bytes = bytesView(value.bytes);
  if (bytes.byteLength > maxFontBytes) {
    throw new RangeError(`font artifact is ${bytes.byteLength} bytes, over the ${maxFontBytes} byte limit`);
  }
  if (typeof value.sha256 !== 'string' || !/^[0-9a-f]{64}$/i.test(value.sha256)) {
    throw new TypeError('font artifact sha256 must be 64 hexadecimal characters');
  }
  const actual = sha256(bytes);
  if (actual !== value.sha256.toLowerCase()) {
    const error = new Error(`font artifact hash mismatch: expected ${value.sha256}, received ${actual}`);
    error.code = 'font_hash_mismatch';
    throw error;
  }
  if (!value.license || value.license.embeddable !== true) {
    const error = new Error('font artifact license does not permit embedding');
    error.code = 'font_license_restricted';
    throw error;
  }
  for (const [key, type] of [
    ['family', 'string'], ['fullName', 'string'], ['postscriptName', 'string'],
    ['version', 'string'], ['italic', 'boolean'],
  ]) {
    if (typeof value[key] !== type) throw new TypeError(`font artifact ${key} is invalid`);
  }
  if (!Number.isInteger(value.weight) || !Number.isInteger(value.faceIndex) ||
      !Number.isInteger(value.stretch ?? 5)) {
    throw new TypeError('font artifact numeric identity is invalid');
  }
  return { ...structuredClone(value), sha256: actual, bytes };
}

function expectationFor(artifact, request) {
  return {
    family: artifact.family,
    weight: artifact.weight,
    italic: artifact.italic,
    stretch: artifact.stretch ?? 5,
    version: artifact.version ?? null,
    sha256: artifact.sha256,
    faceIndex: artifact.faceIndex ?? 0,
    requiredScripts: [...request.scripts],
    requiredCodepoints: [],
    vfCoords: request.vfCoords.map((coordinate) => {
      const [axis, value] = String(coordinate).split('=');
      return { axis, value: Number(value) };
    }),
  };
}

class MemoryFontCache {
  constructor() {
    this.artifacts = new Map();
    this.requests = new Map();
    this.negatives = new Map();
  }

  async getArtifact(hash) {
    const bytes = this.artifacts.get(hash);
    return bytes ? new Uint8Array(bytes) : undefined;
  }

  async putArtifact(hash, bytes) {
    this.artifacts.set(hash, new Uint8Array(bytes));
  }

  async deleteArtifact(hash) {
    this.artifacts.delete(hash);
  }

  async getRequest(key) {
    return this.requests.has(key) ? structuredClone(this.requests.get(key)) : undefined;
  }

  async putRequest(key, value) {
    this.requests.set(key, structuredClone(value));
  }

  async deleteRequest(key) {
    this.requests.delete(key);
  }

  getNegative(key, now = Date.now()) {
    const value = this.negatives.get(key);
    if (!value || value.expiresAt <= now) {
      this.negatives.delete(key);
      return undefined;
    }
    return structuredClone(value.result);
  }

  putNegative(key, result, ttlMs, now = Date.now()) {
    if (ttlMs > 0) this.negatives.set(key, { result: structuredClone(result), expiresAt: now + ttlMs });
  }
}

class NodeFontCache {
  constructor(directory) {
    if (typeof directory !== 'string' || !isAbsolute(directory)) {
      throw new TypeError('NodeFontCache directory must be an absolute path');
    }
    this.directory = directory;
  }

  artifactPath(hash) {
    return join(this.directory, 'artifacts', `${hash}.sfnt`);
  }

  requestPath(key) {
    return join(this.directory, 'requests', `${sha256(Buffer.from(key))}.json`);
  }

  async getArtifact(hash) {
    try { return new Uint8Array(await readFile(this.artifactPath(hash))); } catch (error) {
      if (error?.code === 'ENOENT') return undefined;
      throw error;
    }
  }

  async putArtifact(hash, bytes) {
    await this.atomicWrite(this.artifactPath(hash), bytes);
  }

  async deleteArtifact(hash) {
    await unlink(this.artifactPath(hash)).catch((error) => {
      if (error?.code !== 'ENOENT') throw error;
    });
  }

  async getRequest(key) {
    try { return JSON.parse(await readFile(this.requestPath(key), 'utf8')); } catch (error) {
      if (error?.code === 'ENOENT') return undefined;
      if (error instanceof SyntaxError) {
        await this.deleteRequest(key);
        return undefined;
      }
      throw error;
    }
  }

  async putRequest(key, value) {
    await this.atomicWrite(this.requestPath(key), Buffer.from(`${JSON.stringify(value)}\n`));
  }

  async deleteRequest(key) {
    await unlink(this.requestPath(key)).catch((error) => {
      if (error?.code !== 'ENOENT') throw error;
    });
  }

  async atomicWrite(path, bytes) {
    await mkdir(join(this.directory, 'artifacts'), { recursive: true });
    await mkdir(join(this.directory, 'requests'), { recursive: true });
    const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
    await writeFile(temporary, bytes, { flag: 'wx' });
    await rename(temporary, path);
  }
}

async function cacheLookup(cache, persistentCache, key, catalogKey) {
  let index = await cache.getRequest(key);
  let cacheStatus = 'memory_hit';
  if (!index && persistentCache) {
    index = await persistentCache.getRequest(key);
    cacheStatus = 'persistent_hit';
  }
  if (!index) return undefined;
  if (catalogKey !== undefined && index.catalogKey !== catalogKey) {
    await cache.deleteRequest(key);
    await persistentCache?.deleteRequest(key);
    return undefined;
  }
  let bytes = await cache.getArtifact(index.sha256);
  if (!bytes && persistentCache) {
    bytes = await persistentCache.getArtifact(index.sha256);
    if (bytes) await cache.putArtifact(index.sha256, bytes);
  }
  if (!bytes || sha256(bytes) !== index.sha256) {
    await cache.deleteArtifact(index.sha256);
    await cache.deleteRequest(key);
    await persistentCache?.deleteArtifact(index.sha256);
    await persistentCache?.deleteRequest(key);
    return undefined;
  }
  await cache.putRequest(key, index);
  return {
    artifact: { ...index.artifact, bytes },
    cacheStatus,
  };
}

function validateProvider(provider) {
  if (!provider || typeof provider !== 'object' || typeof provider.resolve !== 'function' ||
      typeof provider.id !== 'string' || provider.id.length === 0) {
    throw new TypeError('provider must expose a non-empty id and resolve(requests, signal)');
  }
}

function withAbort(promise, signal) {
  if (signal.aborted) return Promise.reject(signal.reason ?? new Error('font resolution aborted'));
  return new Promise((resolve, reject) => {
    const aborted = () => reject(signal.reason ?? new Error('font resolution aborted'));
    signal.addEventListener('abort', aborted, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener('abort', aborted);
        resolve(value);
      },
      (error) => {
        signal.removeEventListener('abort', aborted);
        reject(error);
      },
    );
  });
}

function resultMap(results, requests, providerId) {
  if (!Array.isArray(results)) throw new TypeError('provider resolve() must return an array');
  const expected = new Map(requests.map((request) => [requestKey(request, providerId), request]));
  const mapped = new Map();
  for (const result of results) {
    if (!result || !['found', 'not_found', 'denied', 'temporary_failure'].includes(result.status)) {
      throw new TypeError('provider returned an invalid font status');
    }
    const sanitized = canonicalRequest(result.request);
    const key = requestKey(sanitized, providerId);
    if (!expected.has(key) || mapped.has(key)) throw new TypeError('provider returned an unexpected font request');
    mapped.set(key, { ...result, request: expected.get(key) });
  }
  for (const [key, request] of expected) {
    if (!mapped.has(key)) {
      mapped.set(key, { status: 'temporary_failure', request, reason: 'provider omitted result' });
    }
  }
  return mapped;
}

async function resolveAndRegisterFonts(requestValues, register, options = {}) {
  if (!Array.isArray(requestValues) || typeof register !== 'function') {
    throw new TypeError('font requests and register callback are required');
  }
  const requests = requestValues.map(canonicalRequest);
  const provider = options.provider;
  validateProvider(provider);
  const memoryCache = options.memoryCache ?? new MemoryFontCache();
  const persistentCache = options.persistentCache;
  const maxFontBytes = options.maxFontBytes ?? DEFAULT_MAX_FONT_BYTES;
  const negativeTtlMs = options.negativeTtlMs ?? DEFAULT_NEGATIVE_TTL_MS;
  const externalSignal = options.signal;
  if (externalSignal?.aborted) {
    throw externalSignal.reason ?? new Error('font resolution aborted');
  }
  let signal = externalSignal ?? new AbortController().signal;
  let timedOut = false;
  let timeout;
  let detachExternalAbort;
  if (options.timeoutMs !== undefined) {
    if (!Number.isFinite(options.timeoutMs) || options.timeoutMs <= 0) {
      throw new TypeError('font provider timeoutMs must be a positive finite number');
    }
    const controller = new AbortController();
    const forwardAbort = () => controller.abort(externalSignal.reason);
    if (externalSignal) {
      externalSignal.addEventListener('abort', forwardAbort, { once: true });
      detachExternalAbort = () => externalSignal.removeEventListener('abort', forwardAbort);
    }
    timeout = setTimeout(() => {
      timedOut = true;
      controller.abort(new Error(`font provider timeout after ${options.timeoutMs} ms`));
    }, options.timeoutMs);
    timeout.unref?.();
    signal = controller.signal;
  }
  try {
    if (signal.aborted) throw signal.reason ?? new Error('font resolution aborted');

  const outcomes = new Map();
  const providerWaits = new Map();
  const newRequests = [];
  for (const request of requests) {
    const key = requestKey(request, provider.id);
    const catalogKey = typeof provider.cacheKey === 'function'
      ? String(await provider.cacheKey(request))
      : undefined;
    const cached = await cacheLookup(memoryCache, persistentCache, key, catalogKey);
    if (cached) {
      outcomes.set(key, { status: 'found', request, ...cached });
      continue;
    }
    const negative = memoryCache.getNegative?.(key);
    if (negative) {
      outcomes.set(key, { ...negative, request, cacheStatus: 'negative_hit' });
      continue;
    }
    const inFlightKey = `${key}|${catalogKey ?? ''}`;
    const existing = IN_FLIGHT.get(inFlightKey);
    if (existing) {
      providerWaits.set(key, existing);
    } else {
      newRequests.push({ request, key, catalogKey, inFlightKey });
    }
  }

  if (newRequests.length > 0) {
    const startedAt = Date.now();
    const batchRequests = newRequests.map(({ request }) => request);
    let batch = withAbort(
      Promise.resolve().then(() => provider.resolve(batchRequests, signal)),
      signal,
    );
    batch = batch.then(
      (results) => resultMap(results, batchRequests, provider.id),
      (error) => {
        if (signal.aborted && !timedOut) throw signal.reason ?? error;
        return new Map(newRequests.map(({ key, request }) => [key, {
          status: 'temporary_failure', request, reason: String(error?.message ?? error),
        }]));
      },
    );
    for (const { key, inFlightKey } of newRequests) {
      const promise = batch.then((mapped) => ({
        ...mapped.get(key),
        cacheStatus: 'miss',
        fetchDurationMs: Date.now() - startedAt,
      }));
      IN_FLIGHT.set(inFlightKey, promise);
      promise.finally(() => {
        if (IN_FLIGHT.get(inFlightKey) === promise) IN_FLIGHT.delete(inFlightKey);
      }).catch(() => {});
      providerWaits.set(key, promise);
    }
  }

  for (const [key, promise] of providerWaits) outcomes.set(key, await promise);

  const ordered = [];
  for (const request of requests) {
    const key = requestKey(request, provider.id);
    const outcome = outcomes.get(key);
    if (outcome.status !== 'found') {
      memoryCache.putNegative?.(key, outcome, negativeTtlMs);
      ordered.push(outcome);
      continue;
    }
    let artifactValue;
    try {
      artifactValue = validateArtifact(outcome.artifact, maxFontBytes);
      await register(artifactValue.bytes, JSON.stringify(expectationFor(artifactValue, request)));
    } catch (error) {
      const failureCode = registrationFailureCode(error);
      const status = failureCode === 'font_license_restricted' ? 'denied' : 'temporary_failure';
      const failed = {
        status,
        request,
        reason: String(error?.message ?? error),
        failureCode,
        cacheStatus: outcome.cacheStatus,
      };
      memoryCache.putNegative?.(key, failed, negativeTtlMs);
      ordered.push(failed);
      continue;
    }
    if (outcome.cacheStatus === 'miss') {
      const catalogKey = typeof provider.cacheKey === 'function'
        ? String(await provider.cacheKey(request))
        : undefined;
      const index = {
        sha256: artifactValue.sha256,
        artifact: artifactMetadata(artifactValue),
        catalogKey,
      };
      await memoryCache.putArtifact(artifactValue.sha256, artifactValue.bytes);
      await memoryCache.putRequest(key, index);
      await persistentCache?.putArtifact(artifactValue.sha256, artifactValue.bytes);
      await persistentCache?.putRequest(key, index);
    }
    ordered.push({
      status: 'found',
      request,
      artifact: artifactMetadata(artifactValue),
      cacheStatus: outcome.cacheStatus,
      fetchDurationMs: outcome.fetchDurationMs,
    });
  }
  return ordered;
  } finally {
    if (timeout) clearTimeout(timeout);
    detachExternalAbort?.();
  }
}

function hostEventFor(result) {
  if (result.status === 'not_found') return 'font_not_found';
  if (result.status === 'denied') {
    return result.failureCode === 'font_license_restricted'
      ? 'font_license_restricted'
      : 'font_fetch_failed';
  }
  if (result.status === 'temporary_failure') {
    if (result.failureCode === 'font_hash_mismatch') return 'font_hash_mismatch';
    if (result.failureCode === 'font_identity_mismatch') return 'font_identity_mismatch';
    return 'font_fetch_failed';
  }
  if (result.cacheStatus === 'memory_hit') return 'font_memory_cache_hit';
  if (result.cacheStatus === 'persistent_hit') return 'font_persistent_cache_hit';
  return 'font_fetched';
}

function mergeHostTelemetry(profile, providerResults) {
  if (!profile || !Array.isArray(profile.fonts) || providerResults.length === 0) return profile;
  const results = new Map(providerResults.map((result) => [
    requestIdentityKey(result.request),
    result,
  ]));
  for (const resolution of profile.fonts) {
    const key = requestIdentityKey({
      family: resolution.requestedFamily,
      weight: resolution.requestedWeight,
      italic: resolution.requestedItalic,
      stretch: resolution.requestedStretch,
    });
    const result = results.get(key);
    if (!result) {
      resolution.events = resolution.event ? [resolution.event] : [];
      continue;
    }
    const hostEvent = hostEventFor(result);
    resolution.events = [...new Set([resolution.event, hostEvent].filter(Boolean))];
    resolution.cacheStatus = result.cacheStatus ?? null;
    resolution.fetchDurationMs = result.fetchDurationMs ?? null;
    resolution.providerStatus = result.status;
    if (result.status === 'found') {
      resolution.source = result.cacheStatus === 'memory_hit'
        ? 'memory_cache'
        : result.cacheStatus === 'persistent_hit'
          ? 'persistent_cache'
          : 'provider';
    } else {
      resolution.failureReason = result.reason ?? result.status;
    }
  }
  return profile;
}

function createConvertOnDemand(engine) {
  for (const name of [
    'office_font_requests', 'office_register_font',
    'office_profile_with_options', 'office_to_pdf_with_options',
  ]) {
    if (typeof engine?.[name] !== 'function') throw new TypeError(`engine export ${name} is required`);
  }
  const defaultMemoryCache = new MemoryFontCache();
  return async function convertOnDemand(bytes, extension, options = {}) {
    const mode = options.mode ?? 'compatible';
    if (!['strict', 'compatible'].includes(mode)) throw new TypeError(`unsupported mode ${mode}`);
    const requests = JSON.parse(engine.office_font_requests(bytes, extension));
    const providerResults = requests.length === 0 ? [] : await resolveAndRegisterFonts(
      requests,
      (fontBytes, expectation) => engine.office_register_font(fontBytes, expectation),
      {
        ...options,
        memoryCache: options.memoryCache ?? defaultMemoryCache,
        maxFontBytes: options.limits?.maxFontBytes ?? options.maxFontBytes,
      },
    );
    const optionsJson = JSON.stringify({ mode });
    const profile = mergeHostTelemetry(
      JSON.parse(engine.office_profile_with_options(bytes, extension, optionsJson)),
      providerResults,
    );
    const pdf = engine.office_to_pdf_with_options(bytes, extension, optionsJson);
    return { pdf, profile, fonts: profile.fonts ?? [], providerResults };
  };
}

module.exports = {
  MemoryFontCache,
  NodeFontCache,
  createConvertOnDemand,
  mergeHostTelemetry,
  resolveAndRegisterFonts,
};
