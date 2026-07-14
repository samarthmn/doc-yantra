'use strict';

const { join } = require('node:path');
const { types: { isProxy } } = require('node:util');
const {
  EngineWorkerClient,
  EngineWorkerError,
  errorFor,
} = require('./worker-client.cjs');

const MAX_INPUT_BYTES = 100 * 1024 * 1024;
const MAX_MERGE_TOTAL_BYTES = 200 * 1024 * 1024;
const MAX_WATERMARK_JSON_BYTES = 1024 * 1024;
const MAX_REDACTION_JSON_BYTES = 1024 * 1024;
const MAX_OVERLAY_COUNT = 800;
const MAX_JSON_DEPTH = 64;
const MAX_JSON_ITEMS = 100_000;
const MAX_PAGE_COUNT = 10_000;
const MAX_RANGE_COUNT = 10_000;
// First-covering-face wins in pdf-core. Keep Latin on Liberation and prefer
// Simplified Chinese for shared Han code points; never depend on dir ordering.
// fonts/manifest.json array order is the single source of truth so a font-pack
// change cannot silently miss registration; the static list is only the
// fallback for a package whose manifest is unreadable.
const FALLBACK_FONT_ORDER = [
  'LiberationSans-Regular.ttf',
  'LiberationSans-Bold.ttf',
  'LiberationMono-Regular.ttf',
  'NotoSansCJKsc-Regular.otf',
  'NotoSansCJKtc-Regular.otf',
  'NotoSansCJKjp-Regular.otf',
  'NotoSansCJKkr-Regular.otf',
  'NotoEmoji-Variable.ttf',
];

function fontRegistrationOrder() {
  try {
    const manifest = JSON.parse(
      require('node:fs').readFileSync(join(__dirname, 'fonts', 'manifest.json'), 'utf8'),
    );
    const files = manifest.fonts.map((font) => font.file);
    if (files.length > 0 && files.every((file) => typeof file === 'string')) {
      return files;
    }
  } catch {
    // fall through to the static list
  }
  return FALLBACK_FONT_ORDER;
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
    throw errorFor(
      'deadline_exceeded',
      `operation ${invocation.op} exceeded its ${invocation.timeoutMs} ms deadline`,
    );
  }
}

function createPdfEngineWorker(options = {}) {
  options = requireRecord(options, 'options');
  const entryPath = process.env.DOC_YANTRA_WORKER_TEST_HOOKS === '1' && options.__workerEntryPath
    ? options.__workerEntryPath
    : require.resolve('./worker-entry.cjs');
  const client = new EngineWorkerClient({
    entryPath,
    workerData: {
      gluePath: require.resolve('./pdf_core_wasm.js'),
      opsPath: require.resolve('./worker-ops.cjs'),
      fontsDir: join(__dirname, 'fonts'),
      fontOrder: fontRegistrationOrder(),
    },
  }, options);

  const api = {
    mergePdfs(input) {
      return invokeValidated(client, 'mergePdfs', input, (invocation, snapshot) => {
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
      });
    },
    applyWatermark(input) {
      return invokeValidated(client, 'applyWatermark', input, (invocation, snapshot) => {
      const { pdf, watermarkJson, watermarkImage } = snapshot;
      cap(binaryLength(pdf, 'pdf'), MAX_INPUT_BYTES, 'PDF');
      parseBoundedJson(watermarkJson, 'watermarkJson', MAX_WATERMARK_JSON_BYTES, invocation, 'object');
      optionalBinary(watermarkImage, 'watermark image');
      return { pdf, watermarkJson, watermarkImage };
      });
    },
    csvToPdf(input) {
      return invokeValidated(client, 'csvToPdf', input, (_invocation, snapshot) => {
      const { csv } = snapshot;
      cap(binaryLength(csv, 'csv'), MAX_INPUT_BYTES, 'CSV');
      return { csv };
      });
    },
    markdownToPdf(input) {
      return invokeValidated(client, 'markdownToPdf', input, (_invocation, snapshot) => {
      const { markdown } = snapshot;
      cap(stringBytes(markdown, 'markdown'), MAX_INPUT_BYTES, 'Markdown');
      return { markdown };
      });
    },
    textToPdf(input) {
      return invokeValidated(client, 'textToPdf', input, (_invocation, snapshot) => {
      const { text } = snapshot;
      cap(stringBytes(text, 'text'), MAX_INPUT_BYTES, 'text');
      return { text };
      });
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
