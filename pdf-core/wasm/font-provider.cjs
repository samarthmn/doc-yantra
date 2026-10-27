'use strict';

const { createHash, randomUUID } = require('node:crypto');
const { existsSync, readFileSync } = require('node:fs');
const { mkdir, readFile, rename, unlink, writeFile } = require('node:fs/promises');
const { isAbsolute, join } = require('node:path');

const FONT_PACKAGE_NAME = '@samarthmn/doc-yantra-fonts';
const CDN_BASE_URL = 'https://public.dockosha.com/doc-yantra-fonts/sha256';
const DEFAULT_TIMEOUT_MS = 15_000;
const DEFAULT_MAX_REDIRECTS = 2;
const MAX_FONT_BYTES = 32 * 1024 * 1024;
const REDIRECT_STATUSES = new Set([301, 302, 303, 307, 308]);
const CONTENT_TYPES = Object.freeze({
  otf: new Set(['font/otf', 'application/x-font-opentype', 'application/octet-stream']),
  ttf: new Set(['font/ttf', 'application/x-font-ttf', 'application/octet-stream']),
});
const VALIDATION_ERROR_CODES = new Set([
  'font_content_type',
  'font_face_index',
  'font_hash_mismatch',
  'font_identity_mismatch',
  'font_manifest_mismatch',
  'font_size_mismatch',
  'font_too_large',
]);

class FontProviderError extends Error {
  constructor(code, message, file, options) {
    super(message, options);
    this.name = 'FontProviderError';
    this.code = code;
    if (file !== undefined) this.file = file;
  }
}

function fail(code, message, entry, options) {
  throw new FontProviderError(code, message, entry?.file, options);
}

function loadBundledManifest() {
  const candidates = [
    join(__dirname, 'font-manifest.json'),
    join(__dirname, '..', '..', 'packages', 'doc-yantra-fonts', 'fonts', 'manifest.json'),
  ];
  const path = candidates.find((candidate) => existsSync(candidate));
  if (!path) throw new Error('DocYantra font manifest is missing');
  return JSON.parse(readFileSync(path, 'utf8'));
}

function validateManifest(value) {
  if (!value || typeof value !== 'object' || !Array.isArray(value.fonts)) {
    throw new TypeError('DocYantra font manifest must contain a fonts array');
  }
  const seen = new Set();
  const fonts = value.fonts.map((entry) => {
    if (!entry || typeof entry !== 'object' ||
        typeof entry.file !== 'string' || !/^[A-Za-z0-9._-]+$/.test(entry.file) ||
        typeof entry.family !== 'string' || entry.family.length === 0 ||
        typeof entry.postscriptName !== 'string' || entry.postscriptName.length === 0 ||
        typeof entry.version !== 'string' || entry.version.length === 0 ||
        typeof entry.sha256 !== 'string' || !/^[0-9a-f]{64}$/.test(entry.sha256) ||
        !Number.isSafeInteger(entry.byteLength) || entry.byteLength <= 0 ||
        entry.byteLength > MAX_FONT_BYTES || !['otf', 'ttf'].includes(entry.format) ||
        entry.faceIndex !== 0 || typeof entry.license !== 'string' || entry.license.length === 0) {
      throw new TypeError(`invalid DocYantra font manifest entry: ${entry?.file ?? 'unknown'}`);
    }
    if (seen.has(entry.file)) throw new TypeError(`duplicate DocYantra font: ${entry.file}`);
    seen.add(entry.file);
    return Object.freeze({ ...entry });
  });
  return Object.freeze({ fonts: Object.freeze(fonts) });
}

const fontManifest = validateManifest(loadBundledManifest());
const ENTRIES = new Map(fontManifest.fonts.map((entry) => [entry.file, entry]));

function bytesView(value) {
  if (value instanceof Uint8Array) return new Uint8Array(value);
  if (value instanceof ArrayBuffer) return new Uint8Array(value.slice(0));
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength));
  }
  throw new TypeError('font bytes must be an ArrayBuffer or typed array');
}

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function readU16(bytes, offset, entry) {
  if (offset < 0 || offset + 2 > bytes.byteLength) {
    fail('font_identity_mismatch', `invalid OpenType table bounds for ${entry.file}`, entry);
  }
  return (bytes[offset] << 8) | bytes[offset + 1];
}

function readU32(bytes, offset, entry) {
  if (offset < 0 || offset + 4 > bytes.byteLength) {
    fail('font_identity_mismatch', `invalid OpenType table bounds for ${entry.file}`, entry);
  }
  return ((bytes[offset] * 0x1000000) +
    (bytes[offset + 1] << 16) +
    (bytes[offset + 2] << 8) +
    bytes[offset + 3]) >>> 0;
}

function ascii(bytes, offset, length, entry) {
  if (offset < 0 || offset + length > bytes.byteLength) {
    fail('font_identity_mismatch', `invalid OpenType table bounds for ${entry.file}`, entry);
  }
  return Buffer.from(bytes.subarray(offset, offset + length)).toString('latin1');
}

function decodeUtf16Be(bytes) {
  if (bytes.byteLength % 2 !== 0) return '';
  let output = '';
  for (let offset = 0; offset < bytes.byteLength; offset += 2) {
    output += String.fromCharCode((bytes[offset] << 8) | bytes[offset + 1]);
  }
  return output.replaceAll('\u0000', '').trim();
}

function openTypeNames(bytes, entry) {
  const signature = ascii(bytes, 0, 4, entry);
  const expectedSignature = entry.format === 'otf' ? 'OTTO' : '\u0000\u0001\u0000\u0000';
  if (signature !== expectedSignature) {
    fail('font_identity_mismatch', `${entry.file} is not the declared ${entry.format} font`, entry);
  }
  const tableCount = readU16(bytes, 4, entry);
  if (tableCount === 0 || tableCount > 512 || 12 + tableCount * 16 > bytes.byteLength) {
    fail('font_identity_mismatch', `invalid OpenType directory for ${entry.file}`, entry);
  }
  let nameOffset;
  let nameLength;
  for (let index = 0; index < tableCount; index += 1) {
    const record = 12 + index * 16;
    if (ascii(bytes, record, 4, entry) !== 'name') continue;
    nameOffset = readU32(bytes, record + 8, entry);
    nameLength = readU32(bytes, record + 12, entry);
    break;
  }
  if (nameOffset === undefined || nameOffset + nameLength > bytes.byteLength) {
    fail('font_identity_mismatch', `OpenType name table is missing for ${entry.file}`, entry);
  }
  const count = readU16(bytes, nameOffset + 2, entry);
  const stringsAt = nameOffset + readU16(bytes, nameOffset + 4, entry);
  if (nameOffset + 6 + count * 12 > nameOffset + nameLength) {
    fail('font_identity_mismatch', `invalid OpenType name records for ${entry.file}`, entry);
  }
  const names = new Map();
  for (let index = 0; index < count; index += 1) {
    const record = nameOffset + 6 + index * 12;
    const platform = readU16(bytes, record, entry);
    const nameId = readU16(bytes, record + 6, entry);
    const length = readU16(bytes, record + 8, entry);
    const offset = stringsAt + readU16(bytes, record + 10, entry);
    if ((platform !== 0 && platform !== 3) || offset + length > nameOffset + nameLength) continue;
    const value = decodeUtf16Be(bytes.subarray(offset, offset + length));
    if (value.length === 0) continue;
    const values = names.get(nameId) ?? new Set();
    values.add(value);
    names.set(nameId, values);
  }
  return names;
}

function validateFontBytes(value, entry) {
  if (!entry || typeof entry !== 'object') throw new TypeError('font manifest entry is required');
  if (entry.faceIndex !== 0) {
    fail('font_face_index', `${entry.file} requires unsupported face index ${entry.faceIndex}`, entry);
  }
  const bytes = bytesView(value);
  if (bytes.byteLength !== entry.byteLength) {
    const code = bytes.byteLength > entry.byteLength ? 'font_too_large' : 'font_size_mismatch';
    fail(code, `${entry.file} is ${bytes.byteLength} bytes; expected ${entry.byteLength}`, entry);
  }
  const actualHash = sha256(bytes);
  if (actualHash !== entry.sha256) {
    fail('font_hash_mismatch', `${entry.file} hash ${actualHash} does not match ${entry.sha256}`, entry);
  }
  const names = openTypeNames(bytes, entry);
  const families = names.get(1) ?? new Set();
  const postscriptNames = names.get(6) ?? new Set();
  const versions = names.get(5) ?? new Set();
  if (!families.has(entry.family) || !postscriptNames.has(entry.postscriptName)) {
    fail('font_identity_mismatch', `${entry.file} OpenType identity does not match its manifest`, entry);
  }
  const expectedVersion = entry.version.match(/\d+(?:\.\d+)+/)?.[0];
  if (!expectedVersion || ![...versions].some((version) => version.includes(expectedVersion))) {
    fail('font_identity_mismatch', `${entry.file} OpenType version does not match ${entry.version}`, entry);
  }
  return bytes;
}

class MemoryFontCache {
  constructor() {
    this.artifacts = new Map();
  }

  async getArtifact(hash) {
    const bytes = this.artifacts.get(hash);
    return bytes === undefined ? undefined : new Uint8Array(bytes);
  }

  async putArtifact(hash, bytes) {
    this.artifacts.set(hash, bytesView(bytes));
  }

  async deleteArtifact(hash) {
    this.artifacts.delete(hash);
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

  async getArtifact(hash) {
    try {
      return new Uint8Array(await readFile(this.artifactPath(hash)));
    } catch (error) {
      if (error?.code === 'ENOENT') return undefined;
      throw error;
    }
  }

  async putArtifact(hash, bytes) {
    const path = this.artifactPath(hash);
    await mkdir(join(this.directory, 'artifacts'), { recursive: true });
    const temporary = `${path}.${process.pid}.${randomUUID()}.tmp`;
    await writeFile(temporary, bytesView(bytes), { flag: 'wx' });
    await rename(temporary, path);
  }

  async deleteArtifact(hash) {
    await unlink(this.artifactPath(hash)).catch((error) => {
      if (error?.code !== 'ENOENT') throw error;
    });
  }
}

function validateCache(cache, label) {
  if (!cache || typeof cache !== 'object' ||
      typeof cache.getArtifact !== 'function' ||
      typeof cache.putArtifact !== 'function' ||
      typeof cache.deleteArtifact !== 'function') {
    throw new TypeError(`${label} must implement getArtifact(), putArtifact(), and deleteArtifact()`);
  }
  return cache;
}

function optionalOfflinePackage(value) {
  if (value === false) return undefined;
  if (value !== undefined) return value;
  try {
    return require(FONT_PACKAGE_NAME);
  } catch (error) {
    if (error?.code === 'MODULE_NOT_FOUND' && String(error.message).includes(FONT_PACKAGE_NAME)) {
      return undefined;
    }
    throw error;
  }
}

function validateOfflinePackage(pack) {
  if (pack === undefined) return undefined;
  if (!pack || typeof pack !== 'object' || typeof pack.readFont !== 'function' ||
      !pack.manifest || !Array.isArray(pack.manifest.fonts)) {
    throw new TypeError('offlinePackage must expose manifest and readFont(file)');
  }
  return pack;
}

function cdnUrlFor(entry, baseUrl = CDN_BASE_URL) {
  return `${baseUrl.replace(/\/$/u, '')}/${entry.sha256}/${encodeURIComponent(entry.file)}`;
}

async function readBoundedResponse(response, entry) {
  if (!response.body || typeof response.body.getReader !== 'function') {
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.byteLength > entry.byteLength) {
      fail('font_too_large', `${entry.file} response exceeds ${entry.byteLength} bytes`, entry);
    }
    return bytes;
  }

  const reader = response.body.getReader();
  const chunks = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      const chunk = bytesView(value);
      total += chunk.byteLength;
      if (total > entry.byteLength) {
        await reader.cancel().catch(() => undefined);
        fail('font_too_large', `${entry.file} response exceeds ${entry.byteLength} bytes`, entry);
      }
      chunks.push(chunk);
    }
  } catch (error) {
    if (error instanceof FontProviderError) throw error;
    fail('font_unavailable', `font response failed while reading ${entry.file}`, entry, {
      cause: error,
    });
  } finally {
    reader.releaseLock();
  }

  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

function manifestEntryFor(pack, entry) {
  const candidate = pack.manifest.fonts.find((font) => font.file === entry.file);
  if (!candidate || candidate.sha256 !== entry.sha256 ||
      candidate.byteLength !== entry.byteLength || candidate.version !== entry.version ||
      candidate.license !== entry.license || candidate.faceIndex !== entry.faceIndex) {
    fail('font_manifest_mismatch', `${FONT_PACKAGE_NAME} manifest differs for ${entry.file}`, entry);
  }
  return candidate;
}

function emit(telemetry, event) {
  if (typeof telemetry !== 'function') return;
  try {
    telemetry(Object.freeze({ ...event }));
  } catch {
    // Telemetry must never change rendering behavior.
  }
}

function emitValidationFailure(telemetry, error, entry, source, startedAt) {
  if (!(error instanceof FontProviderError) || !VALIDATION_ERROR_CODES.has(error.code)) return;
  emit(telemetry, {
    type: 'font_validation_failed',
    file: entry.file,
    source,
    code: error.code,
    durationMs: Date.now() - startedAt,
  });
}

async function validatedCacheRead(cache, entry, source, telemetry) {
  const cached = await cache?.getArtifact(entry.sha256);
  if (cached === undefined) return undefined;
  try {
    return validateFontBytes(cached, entry);
  } catch (error) {
    await cache.deleteArtifact(entry.sha256);
    emit(telemetry, {
      type: 'font_validation_failed',
      file: entry.file,
      source,
      code: error?.code ?? 'font_validation_failed',
    });
    return undefined;
  }
}

async function cacheResolved(memoryCache, persistentCache, entry, bytes, telemetry) {
  await memoryCache.putArtifact(entry.sha256, bytes);
  if (persistentCache) {
    try {
      await persistentCache.putArtifact(entry.sha256, bytes);
    } catch (error) {
      emit(telemetry, {
        type: 'font_cache_write_failed',
        file: entry.file,
        source: 'persistent',
        code: error?.code ?? 'font_cache_write_failed',
      });
    }
  }
}

function validateOptions(options) {
  if (!options || typeof options !== 'object' || Array.isArray(options)) {
    throw new TypeError('font provider options must be an object');
  }
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  const maxRedirects = options.maxRedirects ?? DEFAULT_MAX_REDIRECTS;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new TypeError('font provider timeoutMs must be positive');
  }
  if (!Number.isSafeInteger(maxRedirects) || maxRedirects < 0) {
    throw new TypeError('font provider maxRedirects must be a non-negative integer');
  }
  if (options.baseUrl !== undefined) {
    const base = new URL(options.baseUrl);
    if (base.protocol !== 'https:') throw new TypeError('font provider baseUrl must use HTTPS');
  }
  return { timeoutMs, maxRedirects };
}

class DocYantraFontProvider {
  constructor(options = {}) {
    const { timeoutMs, maxRedirects } = validateOptions(options);
    this.memoryCache = validateCache(options.memoryCache ?? new MemoryFontCache(), 'memoryCache');
    this.persistentCache = options.persistentCache === undefined
      ? undefined
      : validateCache(options.persistentCache, 'persistentCache');
    this.offlinePackage = validateOfflinePackage(optionalOfflinePackage(options.offlinePackage));
    this.fetch = options.fetch ?? globalThis.fetch;
    this.baseUrl = options.baseUrl ?? CDN_BASE_URL;
    this.timeoutMs = timeoutMs;
    this.maxRedirects = maxRedirects;
    this.telemetry = options.telemetry;
  }

  async result(entry, bytes, source, startedAt, extra = {}) {
    const result = Object.freeze({
      bytes: new Uint8Array(bytes),
      entry,
      source,
      durationMs: Date.now() - startedAt,
      ...extra,
    });
    emit(this.telemetry, {
      type: 'font_resolved',
      file: entry.file,
      sha256: entry.sha256,
      source,
      bytes: bytes.byteLength,
      durationMs: result.durationMs,
    });
    return result;
  }

  async resolve(file, signal) {
    const entry = ENTRIES.get(file);
    if (!entry) fail('font_not_found', `unknown DocYantra font: ${file}`, { file });
    if (signal !== undefined &&
        (!signal || typeof signal !== 'object' || typeof signal.aborted !== 'boolean' ||
         typeof signal.addEventListener !== 'function')) {
      throw new TypeError('signal must be an AbortSignal');
    }
    if (signal?.aborted) {
      fail('font_aborted', `font resolution aborted for ${entry.file}`, entry, { cause: signal.reason });
    }
    const startedAt = Date.now();

    const memory = await validatedCacheRead(this.memoryCache, entry, 'memory', this.telemetry);
    if (memory) return this.result(entry, memory, 'memory', startedAt);

    const persistent = await validatedCacheRead(
      this.persistentCache,
      entry,
      'persistent',
      this.telemetry,
    );
    if (persistent) {
      await this.memoryCache.putArtifact(entry.sha256, persistent);
      return this.result(entry, persistent, 'persistent', startedAt);
    }

    if (this.offlinePackage) {
      try {
        manifestEntryFor(this.offlinePackage, entry);
        let offlineBytes;
        try {
          offlineBytes = await this.offlinePackage.readFont(entry.file);
        } catch (error) {
          fail('font_offline_corrupt', `offline font pack cannot read ${entry.file}`, entry, { cause: error });
        }
        const validated = validateFontBytes(offlineBytes, entry);
        await cacheResolved(this.memoryCache, this.persistentCache, entry, validated, this.telemetry);
        return this.result(entry, validated, 'offline', startedAt);
      } catch (error) {
        emitValidationFailure(this.telemetry, error, entry, 'offline', startedAt);
        throw error;
      }
    }

    try {
      const downloaded = await this.download(entry, signal);
      await cacheResolved(this.memoryCache, this.persistentCache, entry, downloaded.bytes, this.telemetry);
      return this.result(entry, downloaded.bytes, 'cdn', startedAt, { url: downloaded.url });
    } catch (error) {
      emitValidationFailure(this.telemetry, error, entry, 'cdn', startedAt);
      throw error;
    }
  }

  async resolveMany(files, signal) {
    const results = [];
    for (const file of files) results.push(await this.resolve(file, signal));
    return results;
  }

  async download(entry, parentSignal) {
    if (typeof this.fetch !== 'function') {
      fail('font_unavailable', `no HTTPS fetch implementation is available for ${entry.file}`, entry);
    }
    const controller = new AbortController();
    let timedOut = false;
    const timeout = setTimeout(() => {
      timedOut = true;
      controller.abort(new Error('font fetch timeout'));
    }, this.timeoutMs);
    const parentAborted = () => controller.abort(parentSignal.reason);
    parentSignal?.addEventListener('abort', parentAborted, { once: true });
    let url = cdnUrlFor(entry, this.baseUrl);
    let redirects = 0;
    try {
      for (;;) {
        let response;
        try {
          response = await this.fetch(url, {
            headers: { accept: entry.format === 'otf' ? 'font/otf' : 'font/ttf' },
            redirect: 'manual',
            signal: controller.signal,
          });
        } catch (error) {
          if (timedOut) fail('font_timeout', `font fetch timed out for ${entry.file}`, entry, { cause: error });
          if (parentSignal?.aborted) {
            fail('font_aborted', `font resolution aborted for ${entry.file}`, entry, { cause: parentSignal.reason });
          }
          fail('font_unavailable', `font fetch failed for ${entry.file}`, entry, { cause: error });
        }
        if (REDIRECT_STATUSES.has(response.status)) {
          if (redirects >= this.maxRedirects) {
            fail('font_redirect_limit', `font fetch exceeded ${this.maxRedirects} redirects`, entry);
          }
          const location = response.headers.get('location');
          if (!location) fail('font_redirect_invalid', `font redirect has no location for ${entry.file}`, entry);
          const redirected = new URL(location, url);
          if (redirected.protocol !== 'https:') {
            fail('font_redirect_invalid', `font redirect must remain HTTPS for ${entry.file}`, entry);
          }
          url = redirected.href;
          redirects += 1;
          continue;
        }
        if (!response.ok) {
          fail('font_unavailable', `font CDN returned HTTP ${response.status} for ${entry.file}`, entry);
        }
        const contentType = response.headers.get('content-type')?.split(';', 1)[0].trim().toLowerCase();
        if (!contentType || !CONTENT_TYPES[entry.format].has(contentType)) {
          fail('font_content_type', `font CDN returned ${contentType ?? 'no content type'} for ${entry.file}`, entry);
        }
        const contentLengthValue = response.headers.get('content-length');
        if (contentLengthValue !== null) {
          const contentLength = Number(contentLengthValue);
          if (!Number.isSafeInteger(contentLength) || contentLength < 0) {
            fail('font_size_mismatch', `font CDN returned an invalid length for ${entry.file}`, entry);
          }
          if (contentLength > entry.byteLength) {
            fail('font_too_large', `${entry.file} response exceeds ${entry.byteLength} bytes`, entry);
          }
          if (contentLength !== entry.byteLength) {
            fail('font_size_mismatch', `${entry.file} response length does not match its manifest`, entry);
          }
        }
        const bytes = await readBoundedResponse(response, entry);
        return { bytes: validateFontBytes(bytes, entry), url };
      }
    } finally {
      clearTimeout(timeout);
      parentSignal?.removeEventListener('abort', parentAborted);
    }
  }
}

function createFontProvider(options) {
  return new DocYantraFontProvider(options);
}

module.exports = Object.freeze({
  CDN_BASE_URL,
  DocYantraFontProvider,
  FontProviderError,
  MemoryFontCache,
  NodeFontCache,
  cdnUrlFor,
  createFontProvider,
  fontManifest,
  validateFontBytes,
});
