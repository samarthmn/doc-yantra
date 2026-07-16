'use strict';

const { parentPort, workerData, threadId } = require('node:worker_threads');
const { readdirSync, readFileSync } = require('node:fs');
const { join } = require('node:path');
const {
  ERROR_CONTEXT_MAX_BYTES,
  processPayload,
  sanitizeWireError,
} = require('./worker-client.cjs');

const DEFAULT_MAX_OUTPUT_DEPTH = 64;
const DEFAULT_MAX_OUTPUT_ITEMS = 100_000;
const DEFAULT_MAX_OUTPUT_OVERLAY_COUNT = 800;
const DEFAULT_MAX_OUTPUT_PAGE_COUNT = 10_000;
const DEFAULT_MAX_OUTPUT_RANGE_COUNT = 10_000;

function isRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

// Engine failures arrive as a machine envelope the wasm boundary serialized
// into the error message: {"dy":1,"code":…,"message":…,"detail":…}. The code
// travels VERBATIM to the client — never inferred from message prose. Errors
// thrown by this entry itself carry an explicit `dyCode`. Message is capped at
// 4 KiB UTF-8; detail reason/string at 128 bytes and context at 1 KiB. Invalid
// or oversized detail is dropped. Anything else is `internal_error`.
function parseEnvelope(message) {
  if (typeof message !== 'string' || message[0] !== '{') return null;
  try {
    const value = JSON.parse(message);
    if (
      value !== null && typeof value === 'object' &&
      value.dy === 1 && typeof value.code === 'string' && typeof value.message === 'string'
    ) {
      return value;
    }
  } catch {
    // not an envelope
  }
  return null;
}

function codedError(code, message, detail) {
  const error = new Error(message);
  error.dyCode = code;
  if (detail !== undefined) error.dyDetail = detail;
  return error;
}

function postEngineError(id, kind, error) {
  const message = error && typeof error.message === 'string' ? error.message : String(error);
  if (kind !== 'engine') {
    parentPort.postMessage({ id, ok: false, error: sanitizeWireError({ kind, message }) });
    return;
  }
  const env = parseEnvelope(message);
  if (env) {
    parentPort.postMessage({
      id,
      ok: false,
      error: sanitizeWireError({ kind, code: env.code, message: env.message, detail: env.detail }),
    });
    return;
  }
  const code = error && typeof error.dyCode === 'string' ? error.dyCode : 'internal_error';
  parentPort.postMessage({
    id,
    ok: false,
    error: sanitizeWireError({ kind, code, message, detail: error?.dyDetail }),
  });
}

function optionalLimit(value, fallback, label) {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value <= 0) throw new Error(`invalid ${label}`);
  return value;
}

function boot() {
  if (
    !isRecord(workerData) ||
    typeof workerData.gluePath !== 'string' ||
    typeof workerData.opsPath !== 'string'
  ) {
    throw new Error('invalid workerData');
  }
  if (!Number.isSafeInteger(workerData.maxOutputBytes) || workerData.maxOutputBytes <= 0) {
    throw new Error('invalid maxOutputBytes');
  }
  const glue = require(workerData.gluePath);
  if (typeof glue.version !== 'function') throw new Error('engine glue has no version()');
  const registerFont = typeof glue.office_register_font === 'function'
    ? (bytes) => glue.office_register_font(bytes, undefined)
    : typeof glue.register_font === 'function'
      ? (bytes) => glue.register_font(bytes)
      : undefined;

  if (workerData.fontsDir !== undefined) {
    if (typeof workerData.fontsDir !== 'string' || registerFont === undefined) {
      throw new Error('invalid font registration configuration');
    }
    const availableFonts = readdirSync(workerData.fontsDir)
      .filter((item) => /\.(?:otf|ttf)$/i.test(item));
    const fontNames = workerData.fontOrder === undefined
      ? availableFonts.sort()
      : workerData.fontOrder;
    if (!Array.isArray(fontNames) || fontNames.some((name) =>
      typeof name !== 'string' || name.includes('/') || !availableFonts.includes(name))) {
      throw new Error('invalid font registration order');
    }
    for (const name of fontNames) {
      registerFont(readFileSync(join(workerData.fontsDir, name)));
    }
  }

  if (workerData.extraFontDirs !== undefined) {
    if (!Array.isArray(workerData.extraFontDirs) ||
        workerData.extraFontDirs.some((dir) => typeof dir !== 'string' || dir.length === 0) ||
        registerFont === undefined) {
      throw new Error('invalid extraFontDirs configuration');
    }
    const paths = workerData.extraFontDirs.flatMap((dir) =>
      readdirSync(dir)
        .filter((item) => /\.(?:otc|otf|ttc|ttf)$/i.test(item))
        .map((item) => join(dir, item)))
      .sort();
    for (const path of paths) {
      registerFont(readFileSync(path));
    }
  }

  const factory = require(workerData.opsPath);
  if (!factory || typeof factory.createOperations !== 'function') {
    throw new Error('worker op table has no createOperations()');
  }
  const operations = factory.createOperations(glue);
  if (!isRecord(operations)) throw new Error('worker op table is invalid');
  const version = glue.version();
  if (typeof version !== 'string') throw new Error('engine version is invalid');
  return { operations, version };
}

const { operations, version } = boot();
const outputLimits = {
  maxBytes: workerData.maxOutputBytes,
  maxDepth: optionalLimit(workerData.maxOutputDepth, DEFAULT_MAX_OUTPUT_DEPTH, 'maxOutputDepth'),
  maxItems: optionalLimit(workerData.maxOutputItems, DEFAULT_MAX_OUTPUT_ITEMS, 'maxOutputItems'),
  maxOverlayCount: optionalLimit(
    workerData.maxOutputOverlayCount,
    DEFAULT_MAX_OUTPUT_OVERLAY_COUNT,
    'maxOutputOverlayCount',
  ),
  maxPageCount: optionalLimit(
    workerData.maxOutputPageCount,
    DEFAULT_MAX_OUTPUT_PAGE_COUNT,
    'maxOutputPageCount',
  ),
  maxRangeCount: optionalLimit(
    workerData.maxOutputRangeCount,
    DEFAULT_MAX_OUTPUT_RANGE_COUNT,
    'maxOutputRangeCount',
  ),
};

function postSuccess(id, value) {
  const processed = processPayload(value, {
    kind: 'output',
    limits: outputLimits,
    makeError: codedError,
    materialize: true,
    op: 'worker',
  });
  parentPort.postMessage({ id, ok: true, result: processed.snapshot }, processed.transfers);
}

parentPort.postMessage({ type: 'ready', version });

parentPort.on('message', (message) => {
  if (
    !isRecord(message) ||
    !Number.isSafeInteger(message.id) ||
    message.id <= 0 ||
    typeof message.op !== 'string' ||
    !isRecord(message.args)
  ) {
    parentPort.postMessage({ id: 0, ok: false, error: { kind: 'protocol', message: 'malformed request' } });
    return;
  }

  const { id, op, args } = message;
  try {
    if (op === '__engineVersion') {
      postSuccess(id, version);
      return;
    }
    if (op === '__testTrap' || op === '__testSleep' || op === '__testWorkerIdentity' ||
        op === '__testLargeErrorEnvelope') {
      if (process.env.DOC_YANTRA_WORKER_TEST_HOOKS !== '1') {
        throw codedError('invalid_input', 'invalid: worker test hooks are disabled');
      }
      if (op === '__testTrap') throw new WebAssembly.RuntimeError('forced worker test trap');
      if (op === '__testLargeErrorEnvelope') {
        throw codedError(
          'missing_asset',
          'é'.repeat(100_000),
          { reason: 'font-missing', context: 'c'.repeat(ERROR_CONTEXT_MAX_BYTES + 1) },
        );
      }
      if (op === '__testWorkerIdentity') {
        postSuccess(id, { threadId });
        return;
      }
      if (!Number.isFinite(args.ms) || args.ms < 0) throw codedError('invalid_input', 'invalid: ms must be non-negative');
      const until = Date.now() + args.ms;
      while (Date.now() < until) {
        // Intentional blocking loop: proves parent-side termination interrupts WASM-like work.
      }
      postSuccess(id, null);
      return;
    }

    const operation = operations[op];
    if (typeof operation !== 'function') {
      throw codedError('invalid_input', `invalid: unknown worker operation ${op}`);
    }
    const value = operation(args);
    postSuccess(id, value);
  } catch (error) {
    postEngineError(id, error instanceof WebAssembly.RuntimeError ? 'trap' : 'engine', error);
  }
});
