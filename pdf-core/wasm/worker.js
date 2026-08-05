'use strict';

const { types: { isProxy } } = require('node:util');
const {
  EngineWorkerClient,
  EngineWorkerError,
  errorFor,
} = require('./worker-client.cjs');
const {
  FontProviderError,
  createFontProvider,
  fontManifest,
  validateFontBytes,
} = require('./font-provider.cjs');

const MAX_INPUT_BYTES = 100 * 1024 * 1024;
const MAX_MERGE_TOTAL_BYTES = 200 * 1024 * 1024;
const MAX_WATERMARK_JSON_BYTES = 1024 * 1024;
const MAX_REDACTION_JSON_BYTES = 1024 * 1024;
const MAX_OVERLAY_COUNT = 800;
const MAX_JSON_DEPTH = 64;
const MAX_JSON_ITEMS = 100_000;
const MAX_PAGE_COUNT = 10_000;
const MAX_RANGE_COUNT = 10_000;
const MAX_FONT_BYTES = 32 * 1024 * 1024;
const FONT_ORDER = fontManifest.fonts.map(({ file }) => file);
const FONT_ENTRIES = new Map(fontManifest.fonts.map((entry) => [entry.file, entry]));
const LIBERATION_STYLE_FONTS = [
  'LiberationSans-Regular.ttf',
  'LiberationSans-Bold.ttf',
  'LiberationSans-Italic.ttf',
  'LiberationSans-BoldItalic.ttf',
  'LiberationMono-Regular.ttf',
];

function requiredFontFiles(text) {
  if (typeof text !== 'string' || !/[\u0100-\u{10ffff}]/u.test(text)) return [];
  const required = new Set(LIBERATION_STYLE_FONTS);
  if (/[\u0600-\u06ff\u0750-\u077f\u08a0-\u08ff\ufb50-\ufdff\ufe70-\ufeff]/u.test(text)) {
    required.add('NotoSansArabic-Variable.ttf');
  }
  if (/[\u0900-\u097f]/u.test(text)) required.add('NotoSansDevanagari-Variable.ttf');
  if (/[\u0e00-\u0e7f]/u.test(text)) required.add('NotoSansThai-Variable.ttf');
  if (/[\u2e80-\u9fff\uac00-\ud7af\uf900-\ufaff\u3040-\u30ff\u{20000}-\u{3134f}]/u.test(text)) {
    // First-covering-face order historically chose SC for shared Han glyphs.
    required.add('NotoSansCJKsc-Regular.otf');
  }
  if (/\p{Extended_Pictographic}|[\u2600-\u27bf]/u.test(text)) {
    required.add('NotoEmoji-Variable.ttf');
  }
  return FONT_ORDER.filter((file) => required.has(file));
}

function requireRecord(value, label) {
  if (value === null || typeof value !== 'object' || Array.isArray(value) || isProxy(value)) {
    throw errorFor('invalid_input', `${label} must be an object`);
  }
  const snapshot = Object.create(null);
  const descriptors = Object.getOwnPropertyDescriptors(value);
  for (const key of Reflect.ownKeys(descriptors)) {
    const descriptor = descriptors[key];
    if (!descriptor.enumerable) continue;
    if (typeof key !== 'string' || !Object.prototype.hasOwnProperty.call(descriptor, 'value')) {
      throw errorFor('invalid_input', `${label} must contain only data properties`);
    }
    Object.defineProperty(snapshot, key, {
      configurable: true,
      enumerable: true,
      value: descriptor.value,
      writable: true,
    });
  }
  return snapshot;
}

function requireDataArray(value, label) {
  if (!Array.isArray(value) || isProxy(value)) {
    throw errorFor('invalid_input', `${label} must be an array`);
  }
  const snapshot = new Array(value.length);
  const descriptors = Object.getOwnPropertyDescriptors(value);
  for (const key of Reflect.ownKeys(descriptors)) {
    const descriptor = descriptors[key];
    if (!descriptor.enumerable) continue;
    if (typeof key !== 'string') {
      throw errorFor('invalid_input', `${label} must contain only indexed data properties`);
    }
    const index = Number(key);
    if (!Number.isSafeInteger(index) || index < 0 ||
        index >= value.length || String(index) !== key ||
        !Object.prototype.hasOwnProperty.call(descriptor, 'value')) {
      throw errorFor('invalid_input', `${label} must contain only indexed data properties`);
    }
    snapshot[index] = descriptor.value;
  }
  return snapshot;
}

function binaryLength(value, label) {
  if (value !== null && typeof value === 'object' && isProxy(value)) {
    throw errorFor('invalid_input', `${label} must not be a Proxy`);
  }
  if (value instanceof ArrayBuffer || ArrayBuffer.isView(value)) return value.byteLength;
  throw errorFor('invalid_input', `${label} must be an ArrayBuffer or typed array`);
}

function stringBytes(value, label) {
  if (typeof value !== 'string') throw errorFor('invalid_input', `${label} must be a string`);
  return Buffer.byteLength(value, 'utf8');
}

function cap(length, limit, label) {
  if (length > limit) {
    throw errorFor('resource_limit', `${label} is ${length} bytes, over the ${limit / 1024 / 1024} MiB in-process limit`);
  }
}

function optionalBinary(value, label) {
  if (value === undefined || value === null) return undefined;
  cap(binaryLength(value, label), MAX_INPUT_BYTES, label);
  return value;
}

function optionalString(value, label) {
  if (value === undefined || value === null) return undefined;
  stringBytes(value, label);
  return value;
}

function invokeValidated(client, op, input, validate) {
  const startedAt = Date.now();
  let invocation;
  try {
    const snapshot = requireRecord(input, `${op} input`);
    invocation = client.createInvocation(op, snapshot.timeoutMs, startedAt);
    const args = validate(invocation, snapshot);
    client.assertInvocationActive(invocation);
    return client.invoke(op, args, invocation);
  } catch (error) {
    if (invocation) {
      try {
        client.assertInvocationActive(invocation);
      } catch (deadline) {
        return Promise.reject(deadline);
      }
    }
    return Promise.reject(error);
  }
}

function providerError(error, file) {
  if (error instanceof EngineWorkerError) return error;
  if (error instanceof FontProviderError && error.code === 'font_too_large') {
    return errorFor('resource_limit', error.message, { reason: error.code });
  }
  const reason = typeof error?.code === 'string' ? error.code : 'font_unavailable';
  return errorFor(
    'missing_asset',
    `required font ${file} could not be resolved`,
    { reason, context: String(error?.message ?? error).slice(0, 1024) },
  );
}

function operationDeadlineError(invocation) {
  return errorFor(
    'deadline_exceeded',
    `operation ${invocation.op} exceeded its ${invocation.timeoutMs} ms deadline`,
  );
}

async function resolveFontWithDeadline(provider, file, invocation) {
  const remainingMs = invocation.deadlineAt - Date.now();
  if (remainingMs <= 0) throw operationDeadlineError(invocation);
  const controller = new AbortController();
  let timer;
  const deadline = new Promise((_resolve, reject) => {
    timer = setTimeout(() => {
      const error = operationDeadlineError(invocation);
      controller.abort(error);
      reject(error);
    }, remainingMs);
  });
  try {
    return await Promise.race([
      Promise.resolve().then(() => provider.resolve(file, controller.signal)),
      deadline,
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function registerRequiredFonts(client, provider, resolvedFonts, files, invocation) {
  if (files.length === 0) return;
  if (provider === undefined) {
    throw errorFor('missing_asset', 'Unicode text requires a fontProvider', {
      reason: 'font_provider_missing',
    });
  }
  for (const file of files) {
    client.assertInvocationActive(invocation);
    let bytes = resolvedFonts.get(file);
    if (bytes === undefined) {
      try {
        const result = await resolveFontWithDeadline(provider, file, invocation);
        if (!result || typeof result !== 'object' || result.bytes === undefined) {
          throw new TypeError('fontProvider.resolve() must return an object containing bytes');
        }
        bytes = validateFontBytes(result.bytes, FONT_ENTRIES.get(file));
      } catch (error) {
        throw providerError(error, file);
      }
      if (bytes.byteLength > MAX_FONT_BYTES) {
        throw errorFor('resource_limit', `${file} exceeds the 32 MiB font limit`);
      }
      resolvedFonts.set(file, new Uint8Array(bytes));
    }
    client.assertInvocationActive(invocation);
    await client.invoke(
      'registerFont',
      { bytes, key: file },
      Math.max(1, invocation.deadlineAt - Date.now()),
    );
  }
}

function invokeWithFonts(client, provider, resolvedFonts, op, input, validate, textFor) {
  const startedAt = Date.now();
  let invocation;
  try {
    const snapshot = requireRecord(input, `${op} input`);
    invocation = client.createInvocation(op, snapshot.timeoutMs, startedAt);
    const args = validate(invocation, snapshot);
    const files = requiredFontFiles(textFor(snapshot));
    return registerRequiredFonts(client, provider, resolvedFonts, files, invocation)
      .then(() => {
        client.assertInvocationActive(invocation);
        return client.invoke(op, args, invocation);
      })
      .catch((error) => {
        client.assertInvocationActive(invocation);
        throw error;
      });
  } catch (error) {
    if (invocation) {
      try {
        client.assertInvocationActive(invocation);
      } catch (deadline) {
        return Promise.reject(deadline);
      }
    }
    return Promise.reject(error);
  }
}

function parseBoundedJson(value, label, maxBytes, invocation, expected) {
  const bytes = stringBytes(value, label);
  if (bytes > maxBytes) {
    throw errorFor('resource_limit', `${label} is ${bytes} bytes, over the ${maxBytes} byte bound`);
  }
  clientDeadlineCheck(invocation);
  let parsed;
  try {
    parsed = JSON.parse(value);
  } catch {
    throw errorFor('invalid_input', `${label} must be valid JSON`);
  }
  if ((expected === 'object' && !requireRecord(parsed, label)) ||
      (expected === 'array' && !Array.isArray(parsed))) {
    throw errorFor('invalid_input', `${label} must be a JSON ${expected}`);
  }

  let items = 0;
  function visit(item, depth, key) {
    clientDeadlineCheck(invocation);
    if (depth > MAX_JSON_DEPTH) {
      throw errorFor('resource_limit', `${label} exceeds the ${MAX_JSON_DEPTH} level nesting bound`);
    }
    if (item === null || typeof item !== 'object') return;
    const entries = Array.isArray(item) ? item.map((child) => [undefined, child]) : Object.entries(item);
    items += entries.length;
    if (items > MAX_JSON_ITEMS) {
      throw errorFor('resource_limit', `${label} exceeds the ${MAX_JSON_ITEMS} property/item bound`);
    }
    if (Array.isArray(item) && typeof key === 'string') {
      const normalized = key.toLowerCase();
      if (/^pages?$/.test(normalized) && item.length > MAX_PAGE_COUNT) {
        throw errorFor('resource_limit', `${label} page count exceeds ${MAX_PAGE_COUNT}`);
      }
      if (/^ranges?$/.test(normalized) && item.length > MAX_RANGE_COUNT) {
        throw errorFor('resource_limit', `${label} range count exceeds ${MAX_RANGE_COUNT}`);
      }
      if (/^(?:areas|overlays?|redactions?)$/.test(normalized) && item.length > MAX_OVERLAY_COUNT) {
        throw errorFor('resource_limit', `${label} overlay count exceeds ${MAX_OVERLAY_COUNT}`);
      }
    }
    for (const [childKey, child] of entries) visit(child, depth + 1, childKey);
  }
  visit(parsed, 0);
  clientDeadlineCheck(invocation);
  return parsed;
}

function clientDeadlineCheck(invocation) {
  if (Date.now() >= invocation.deadlineAt) {
    throw operationDeadlineError(invocation);
  }
}

function createPdfEngineWorker(options = {}) {
  options = requireRecord(options, 'options');
  let fontProvider;
  if (options.fontProvider === false) {
    fontProvider = undefined;
  } else if (options.fontProvider !== undefined) {
    if (!options.fontProvider || typeof options.fontProvider !== 'object' ||
        typeof options.fontProvider.resolve !== 'function' || isProxy(options.fontProvider)) {
      throw errorFor('invalid_input', 'fontProvider must expose resolve(file, signal) or be false');
    }
    fontProvider = options.fontProvider;
  } else {
    fontProvider = createFontProvider({
      memoryCache: options.fontMemoryCache,
      persistentCache: options.fontPersistentCache,
      offlinePackage: options.offlineFontPackage,
      fetch: options.fontFetch,
      baseUrl: options.fontCdnBaseUrl,
      timeoutMs: options.fontProviderTimeoutMs,
      maxRedirects: options.fontMaxRedirects,
      telemetry: options.fontTelemetry,
    });
  }
  const resolvedFonts = new Map();
  const entryPath = process.env.DOC_YANTRA_WORKER_TEST_HOOKS === '1' && options.__workerEntryPath
    ? options.__workerEntryPath
    : require.resolve('./worker-entry.cjs');
  const client = new EngineWorkerClient({
    entryPath,
    workerData: {
      gluePath: require.resolve('./pdf_core_wasm.js'),
      opsPath: require.resolve('./worker-ops.cjs'),
    },
  }, options);

  const api = {
    mergePdfs(input) {
      return invokeWithFonts(client, fontProvider, resolvedFonts, 'mergePdfs', input, (invocation, snapshot) => {
      const { watermarkJson, watermarkImage } = snapshot;
      const pdfs = requireDataArray(snapshot.pdfs, 'pdfs');
      if (pdfs.length === 0) {
        throw errorFor('invalid_input', 'pdfs must be a non-empty array');
      }
      let total = 0;
      for (let index = 0; index < pdfs.length; index++) {
        const length = binaryLength(pdfs[index], `pdfs[${index}]`);
        cap(length, MAX_INPUT_BYTES, `PDF ${index + 1}`);
        total += length;
      }
      cap(total, MAX_MERGE_TOTAL_BYTES, 'merge input set');
      if (watermarkJson !== undefined && watermarkJson !== null) {
        parseBoundedJson(watermarkJson, 'watermarkJson', MAX_WATERMARK_JSON_BYTES, invocation, 'object');
      }
      optionalBinary(watermarkImage, 'watermark image');
      return { pdfs, watermarkJson, watermarkImage };
      }, (snapshot) => snapshot.watermarkJson ?? '');
    },
    applyWatermark(input) {
      return invokeWithFonts(client, fontProvider, resolvedFonts, 'applyWatermark', input, (invocation, snapshot) => {
      const { pdf, watermarkJson, watermarkImage } = snapshot;
      cap(binaryLength(pdf, 'pdf'), MAX_INPUT_BYTES, 'PDF');
      parseBoundedJson(watermarkJson, 'watermarkJson', MAX_WATERMARK_JSON_BYTES, invocation, 'object');
      optionalBinary(watermarkImage, 'watermark image');
      return { pdf, watermarkJson, watermarkImage };
      }, (snapshot) => snapshot.watermarkJson);
    },
    csvToPdf(input) {
      return invokeWithFonts(client, fontProvider, resolvedFonts, 'csvToPdf', input, (_invocation, snapshot) => {
      const { csv } = snapshot;
      cap(binaryLength(csv, 'csv'), MAX_INPUT_BYTES, 'CSV');
      return { csv };
      }, (snapshot) => new TextDecoder().decode(snapshot.csv));
    },
    markdownToPdf(input) {
      return invokeWithFonts(client, fontProvider, resolvedFonts, 'markdownToPdf', input, (_invocation, snapshot) => {
      const { markdown } = snapshot;
      cap(stringBytes(markdown, 'markdown'), MAX_INPUT_BYTES, 'Markdown');
      return { markdown };
      }, (snapshot) => snapshot.markdown);
    },
    textToPdf(input) {
      return invokeWithFonts(client, fontProvider, resolvedFonts, 'textToPdf', input, (_invocation, snapshot) => {
      const { text } = snapshot;
      cap(stringBytes(text, 'text'), MAX_INPUT_BYTES, 'text');
      return { text };
      }, (snapshot) => snapshot.text);
    },
    redactPdf(input) {
      return invokeValidated(client, 'redactPdf', input, (invocation, snapshot) => {
      const { pdf, areasJson } = snapshot;
      cap(binaryLength(pdf, 'pdf'), MAX_INPUT_BYTES, 'PDF');
      const areas = parseBoundedJson(areasJson, 'areasJson', MAX_REDACTION_JSON_BYTES, invocation, 'array');
      if (areas.length > MAX_OVERLAY_COUNT) {
        throw errorFor('resource_limit', `redaction overlay count exceeds ${MAX_OVERLAY_COUNT}`);
      }
      return { pdf, areasJson };
      });
    },
    redactPdfWithReport(input) {
      return invokeValidated(client, 'redactPdfWithReport', input, (invocation, snapshot) => {
      const { pdf, areasJson } = snapshot;
      cap(binaryLength(pdf, 'pdf'), MAX_INPUT_BYTES, 'PDF');
      const areas = parseBoundedJson(areasJson, 'areasJson', MAX_REDACTION_JSON_BYTES, invocation, 'array');
      if (areas.length > MAX_OVERLAY_COUNT) {
        throw errorFor('resource_limit', `redaction overlay count exceeds ${MAX_OVERLAY_COUNT}`);
      }
      return { pdf, areasJson };
      });
    },
    pageCount(input) {
      return invokeValidated(client, 'pageCount', input, (_invocation, snapshot) => {
      const { pdf } = snapshot;
      cap(binaryLength(pdf, 'pdf'), MAX_INPUT_BYTES, 'PDF');
      return { pdf };
      });
    },
    validatePdf(input) {
      return invokeValidated(client, 'validatePdf', input, (_invocation, snapshot) => {
      const { pdf } = snapshot;
      cap(binaryLength(pdf, 'pdf'), MAX_INPUT_BYTES, 'PDF');
      return { pdf };
      });
    },
    close: () => client.close(),
    engineVersion: () => client.engineVersion(),
  };

  if (process.env.DOC_YANTRA_WORKER_TEST_HOOKS === '1') {
    api.__testTrap = () => client.invoke('__testTrap', {}, undefined);
    api.__testSleep = ({ ms, timeoutMs }) => client.invoke('__testSleep', { ms }, timeoutMs);
    api.__testWorkerIdentity = () => client.invoke('__testWorkerIdentity', {}, undefined);
  }
  return api;
}

module.exports = { createPdfEngineWorker, EngineWorkerError };
