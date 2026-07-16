const DEFAULT_MAX_FONT_BYTES = 10 * 1024 * 1024;
const DEFAULT_NEGATIVE_TTL_MS = 5_000;
const MAX_ARTIFACTS_PER_REQUEST = 16;
const IN_FLIGHT = new Map();
const REQUEST_KEYS = new Set([
  'family', 'altFamilies', 'weight', 'italic', 'stretch', 'slots', 'panose',
  'familyClass', 'pitch', 'charset', 'unicodeRanges', 'scripts', 'vfCoords',
]);

async function sha256(bytes) {
  const digest = await globalThis.crypto.subtle.digest('SHA-256', bytes);
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, '0'))
    .join('');
}

function requestIdentityKey(request) {
  return [
    request.family.trim().toLocaleLowerCase('en-US'),
    request.weight,
    request.italic ? 1 : 0,
    request.stretch,
  ].join('|');
}

function requestKey(request, providerId) {
  const sorted = (values) => [...values].sort((left, right) =>
    String(left).localeCompare(String(right), 'en-US'));
  return JSON.stringify([
    providerId,
    request.family.trim().toLocaleLowerCase('en-US'),
    sorted(request.altFamilies.map((family) => family.trim().toLocaleLowerCase('en-US'))),
    request.weight,
    request.italic ? 1 : 0,
    request.stretch,
    sorted(request.slots),
    request.panose,
    request.familyClass,
    request.pitch,
    request.charset,
    sorted(request.unicodeRanges),
    sorted(request.scripts.map((script) => script.trim().toLocaleLowerCase('en-US'))),
    sorted(request.vfCoords),
  ]);
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
  for (const key of ['altFamilies', 'slots', 'unicodeRanges', 'scripts', 'vfCoords']) {
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

async function validateArtifact(value, maxFontBytes) {
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
  const actual = await sha256(bytes);
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
  if (value.coverageScripts !== undefined &&
      (!Array.isArray(value.coverageScripts) ||
       value.coverageScripts.some((script) => typeof script !== 'string' || script.length === 0))) {
    throw new TypeError('font artifact coverageScripts must contain non-empty strings');
  }
  if (value.coverageCodepoints !== undefined &&
      (!Array.isArray(value.coverageCodepoints) ||
       value.coverageCodepoints.some((codepoint) =>
         !Number.isSafeInteger(codepoint) || codepoint < 0 || codepoint > 0x10ffff ||
         (codepoint >= 0xd800 && codepoint <= 0xdfff)))) {
    throw new TypeError('font artifact coverageCodepoints must contain Unicode scalar values');
  }
  if (value.expectedVersion !== undefined && typeof value.expectedVersion !== 'string') {
    throw new TypeError('font artifact expectedVersion must be a string');
  }
  return { ...structuredClone(value), sha256: actual, bytes };
}

function expectationFor(artifact, request) {
  return {
    family: artifact.family,
    weight: artifact.weight,
    italic: artifact.italic,
    stretch: artifact.stretch ?? 5,
    version: artifact.expectedVersion ?? null,
    sha256: artifact.sha256,
    faceIndex: artifact.faceIndex ?? 0,
    requiredScripts: [...(artifact.coverageScripts ?? request.scripts)],
    requiredCodepoints: [...(artifact.coverageCodepoints ?? [])],
    vfCoords: request.vfCoords.map((coordinate) => {
      const [axis, value] = String(coordinate).split('=');
      return { axis, value: Number(value) };
    }),
  };
}

export class MemoryFontCache {
  constructor() {
    this.artifacts = new Map();
    this.requests = new Map();
    this.negatives = new Map();
  }

  async getArtifact(hash) {
    const bytes = this.artifacts.get(hash);
    return bytes ? new Uint8Array(bytes) : undefined;
  }

  async putArtifact(hash, bytes) { this.artifacts.set(hash, new Uint8Array(bytes)); }
  async deleteArtifact(hash) { this.artifacts.delete(hash); }
  async getRequest(key) {
    return this.requests.has(key) ? structuredClone(this.requests.get(key)) : undefined;
  }
  async putRequest(key, value) { this.requests.set(key, structuredClone(value)); }
  async deleteRequest(key) { this.requests.delete(key); }

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

function idbRequest(request) {
  return new Promise((resolve, reject) => {
    request.addEventListener('success', () => resolve(request.result), { once: true });
    request.addEventListener('error', () => reject(request.error), { once: true });
  });
}

function transactionDone(transaction) {
  return new Promise((resolve, reject) => {
    transaction.addEventListener('complete', resolve, { once: true });
    transaction.addEventListener('abort', () => reject(transaction.error), { once: true });
    transaction.addEventListener('error', () => reject(transaction.error), { once: true });
  });
}

export class IndexedDbFontCache {
  constructor(options = {}) {
    this.factory = options.indexedDB ?? globalThis.indexedDB;
    this.databaseName = options.databaseName ?? 'doc-yantra-fonts-v1';
    if (!this.factory || typeof this.factory.open !== 'function') {
      throw new TypeError('IndexedDB is unavailable; pass an indexedDB factory');
    }
    this.database = this.open();
  }

  async open() {
    const request = this.factory.open(this.databaseName, 1);
    request.addEventListener('upgradeneeded', () => {
      if (!request.result.objectStoreNames.contains('artifacts')) {
        request.result.createObjectStore('artifacts');
      }
      if (!request.result.objectStoreNames.contains('requests')) {
        request.result.createObjectStore('requests');
      }
    });
    return idbRequest(request);
  }

  async read(storeName, key) {
    const database = await this.database;
    const transaction = database.transaction(storeName, 'readonly');
    return idbRequest(transaction.objectStore(storeName).get(key));
  }

  async write(storeName, key, value) {
    const database = await this.database;
    const transaction = database.transaction(storeName, 'readwrite');
    transaction.objectStore(storeName).put(value, key);
    await transactionDone(transaction);
  }

  async remove(storeName, key) {
    const database = await this.database;
    const transaction = database.transaction(storeName, 'readwrite');
    transaction.objectStore(storeName).delete(key);
    await transactionDone(transaction);
  }

  async getArtifact(hash) {
    const value = await this.read('artifacts', hash);
    return value === undefined ? undefined : bytesView(value);
  }
  async putArtifact(hash, bytes) { await this.write('artifacts', hash, bytesView(bytes)); }
  async deleteArtifact(hash) { await this.remove('artifacts', hash); }
  async getRequest(key) {
    const value = await this.read('requests', key);
    return value === undefined ? undefined : structuredClone(value);
  }
  async putRequest(key, value) { await this.write('requests', key, structuredClone(value)); }
  async deleteRequest(key) { await this.remove('requests', key); }
  async close() { (await this.database).close(); }
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
  const entries = Array.isArray(index.artifacts)
    ? index.artifacts
    : [{ sha256: index.sha256, artifact: index.artifact }];
  if (entries.length === 0 || entries.length > MAX_ARTIFACTS_PER_REQUEST) {
    await cache.deleteRequest(key);
    await persistentCache?.deleteRequest(key);
    return undefined;
  }
  const artifacts = [];
  for (const entry of entries) {
    let bytes = await cache.getArtifact(entry.sha256);
    if (!bytes && persistentCache) {
      bytes = await persistentCache.getArtifact(entry.sha256);
      if (bytes) await cache.putArtifact(entry.sha256, bytes);
    }
    if (!bytes || await sha256(bytes) !== entry.sha256) {
      await cache.deleteArtifact(entry.sha256);
      await cache.deleteRequest(key);
      await persistentCache?.deleteArtifact(entry.sha256);
      await persistentCache?.deleteRequest(key);
      return undefined;
    }
    artifacts.push({ ...entry.artifact, bytes });
  }
  await cache.putRequest(key, index);
  return { artifact: artifacts[0], artifacts, cacheStatus };
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
      (value) => { signal.removeEventListener('abort', aborted); resolve(value); },
      (error) => { signal.removeEventListener('abort', aborted); reject(error); },
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
    const request = canonicalRequest(result.request);
    const key = requestKey(request, providerId);
    if (!expected.has(key) || mapped.has(key)) throw new TypeError('provider returned an unexpected font request');
    mapped.set(key, { ...result, request: expected.get(key) });
  }
  for (const [key, request] of expected) {
    if (!mapped.has(key)) mapped.set(key, { status: 'temporary_failure', request, reason: 'provider omitted result' });
  }
  return mapped;
}

export async function resolveAndRegisterFonts(requestValues, register, options = {}) {
  if (!Array.isArray(requestValues) || typeof register !== 'function') {
    throw new TypeError('font requests and register callback are required');
  }
  const requests = requestValues.map(canonicalRequest);
  const provider = options.provider;
  validateProvider(provider);
  const memoryCache = options.memoryCache ?? new MemoryFontCache();
  const persistentCache = options.persistentCache;
  const maxFontBytes = options.limits?.maxFontBytes ?? options.maxFontBytes ?? DEFAULT_MAX_FONT_BYTES;
  const negativeTtlMs = options.negativeTtlMs ?? DEFAULT_NEGATIVE_TTL_MS;
  const externalSignal = options.signal;
  if (externalSignal?.aborted) throw externalSignal.reason ?? new Error('font resolution aborted');
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
    signal = controller.signal;
  }

  try {
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
      if (existing) providerWaits.set(key, withAbort(existing, signal));
      else newRequests.push({ request, key, catalogKey, inFlightKey });
    }

    if (newRequests.length > 0) {
      const startedAt = Date.now();
      const batchRequests = newRequests.map(({ request }) => request);
      let batch = withAbort(Promise.resolve().then(() => provider.resolve(batchRequests, signal)), signal);
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
          ...mapped.get(key), cacheStatus: 'miss', fetchDurationMs: Date.now() - startedAt,
        }));
        IN_FLIGHT.set(inFlightKey, promise);
        promise.finally(() => {
          if (IN_FLIGHT.get(inFlightKey) === promise) IN_FLIGHT.delete(inFlightKey);
        }).catch(() => {});
        providerWaits.set(key, promise);
      }
    }
    for (const [key, promise] of providerWaits) {
      try {
        outcomes.set(key, await promise);
      } catch (error) {
        if (signal.aborted && !timedOut) throw signal.reason ?? error;
        const request = requests.find((candidate) => requestKey(candidate, provider.id) === key);
        outcomes.set(key, {
          status: 'temporary_failure', request,
          reason: String(error?.message ?? error), cacheStatus: 'miss',
        });
      }
    }

    const ordered = [];
    for (const request of requests) {
      const key = requestKey(request, provider.id);
      const outcome = outcomes.get(key);
      if (outcome.status !== 'found') {
        memoryCache.putNegative?.(key, outcome, negativeTtlMs);
        ordered.push(outcome);
        continue;
      }
      let artifacts;
      try {
        const supplied = Array.isArray(outcome.artifacts) ? outcome.artifacts : [outcome.artifact];
        if (supplied.length === 0 || supplied.length > MAX_ARTIFACTS_PER_REQUEST) {
          throw new RangeError(`provider returned ${supplied.length} artifacts for one request`);
        }
        artifacts = await Promise.all(supplied.map((artifact) => validateArtifact(artifact, maxFontBytes)));
        for (const artifact of artifacts) {
          const registration = await register(
            artifact.bytes,
            JSON.stringify(expectationFor(artifact, request)),
          );
          let identity = registration;
          if (typeof identity === 'string') {
            try { identity = JSON.parse(identity); } catch { identity = undefined; }
          }
          if (identity && typeof identity === 'object') {
            artifact.fullName = identity.fullName ?? artifact.fullName;
            artifact.postscriptName = identity.postscriptName ?? artifact.postscriptName;
            artifact.version = identity.version ?? artifact.version;
          }
        }
      } catch (error) {
        const failureCode = registrationFailureCode(error);
        const status = failureCode === 'font_license_restricted' ? 'denied' : 'temporary_failure';
        const failed = {
          status, request, reason: String(error?.message ?? error),
          failureCode, cacheStatus: outcome.cacheStatus,
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
          artifacts: artifacts.map((artifact) => ({
            sha256: artifact.sha256,
            artifact: artifactMetadata(artifact),
          })),
          catalogKey,
        };
        for (const artifact of artifacts) {
          await memoryCache.putArtifact(artifact.sha256, artifact.bytes);
          await persistentCache?.putArtifact(artifact.sha256, artifact.bytes);
        }
        await memoryCache.putRequest(key, index);
        await persistentCache?.putRequest(key, index);
      }
      const metadata = artifacts.map(artifactMetadata);
      ordered.push({
        status: 'found', request, artifact: metadata[0],
        ...(metadata.length > 1 ? { artifacts: metadata } : {}),
        cacheStatus: outcome.cacheStatus, fetchDurationMs: outcome.fetchDurationMs,
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
  if (result.status === 'denied') return 'font_license_restricted';
  if (result.status === 'temporary_failure') {
    if (result.failureCode === 'font_hash_mismatch') return 'font_hash_mismatch';
    if (result.failureCode === 'font_identity_mismatch') return 'font_identity_mismatch';
    return 'font_fetch_failed';
  }
  if (result.cacheStatus === 'memory_hit') return 'font_memory_cache_hit';
  if (result.cacheStatus === 'persistent_hit') return 'font_persistent_cache_hit';
  return 'font_fetched';
}

export function mergeHostTelemetry(profile, providerResults) {
  if (!profile || !Array.isArray(profile.fonts) || providerResults.length === 0) return profile;
  const results = new Map(providerResults.map((result) => [requestIdentityKey(result.request), result]));
  for (const resolution of profile.fonts) {
    const result = results.get(requestIdentityKey({
      family: resolution.requestedFamily,
      weight: resolution.requestedWeight,
      italic: resolution.requestedItalic,
      stretch: resolution.requestedStretch,
    }));
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
        : result.cacheStatus === 'persistent_hit' ? 'persistent_cache' : 'provider';
    } else {
      resolution.failureReason = result.reason ?? result.status;
    }
  }
  return profile;
}

export function createConvertOnDemand(engine) {
  const defaultMemoryCache = new MemoryFontCache();
  return async function convertOnDemand(bytes, extension, options = {}) {
    const mode = options.mode ?? 'compatible';
    if (!['strict', 'compatible'].includes(mode)) throw new TypeError(`unsupported mode ${mode}`);
    const requests = JSON.parse(engine.office_font_requests(bytes, extension));
    const providerResults = requests.length === 0 ? [] : await resolveAndRegisterFonts(
      requests,
      (fontBytes, expectation) => engine.office_register_font(fontBytes, expectation),
      { ...options, memoryCache: options.memoryCache ?? defaultMemoryCache },
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
