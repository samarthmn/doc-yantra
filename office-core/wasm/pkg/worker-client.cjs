'use strict';

const { Worker } = require('node:worker_threads');
const { types: { isProxy } } = require('node:util');

const ERROR_MESSAGE_MAX_BYTES = 4 * 1024;
const ERROR_REASON_MAX_BYTES = 128;
const ERROR_CONTEXT_MAX_BYTES = 1024;

function utf8Prefix(value, maxBytes) {
  let bytes = 0;
  let end = 0;
  while (end < value.length) {
    const codePoint = value.codePointAt(end);
    const width = codePoint <= 0x7f ? 1 : codePoint <= 0x7ff ? 2 : codePoint <= 0xffff ? 3 : 4;
    if (bytes + width > maxBytes) break;
    bytes += width;
    end += codePoint > 0xffff ? 2 : 1;
  }
  return value.slice(0, end);
}

function boundedString(value, maxBytes) {
  if (typeof value !== 'string') return undefined;
  const prefix = utf8Prefix(value, maxBytes);
  return prefix.length === value.length ? value : undefined;
}

function sanitizeDetail(detail) {
  if (typeof detail === 'string') return boundedString(detail, ERROR_REASON_MAX_BYTES);
  if (detail === null || typeof detail !== 'object' || Array.isArray(detail) || isProxy(detail)) {
    return undefined;
  }
  const descriptors = Object.getOwnPropertyDescriptors(detail);
  for (const key of Reflect.ownKeys(descriptors)) {
    const descriptor = descriptors[key];
    if (!descriptor.enumerable) continue;
    if (typeof key !== 'string' || !Object.prototype.hasOwnProperty.call(descriptor, 'value')) {
      return undefined;
    }
    if (key !== 'reason' && key !== 'context') return undefined;
  }
  const reason = descriptors.reason?.value;
  const context = descriptors.context?.value;
  if (typeof reason !== 'string' || boundedString(reason, ERROR_REASON_MAX_BYTES) === undefined) {
    return undefined;
  }
  if (context !== undefined &&
      (typeof context !== 'string' || boundedString(context, ERROR_CONTEXT_MAX_BYTES) === undefined)) {
    return undefined;
  }
  return context === undefined ? { reason } : { reason, context };
}

function sanitizeWireError(payload) {
  const message = typeof payload.message === 'string' ? payload.message : String(payload.message ?? '');
  const result = {
    kind: payload.kind,
    message: utf8Prefix(message, ERROR_MESSAGE_MAX_BYTES),
  };
  if (typeof payload.code === 'string') result.code = payload.code;
  const detail = sanitizeDetail(payload.detail);
  if (detail !== undefined) result.detail = detail;
  return result;
}

function isPlainObject(value) {
  if (value === null || typeof value !== 'object' || Array.isArray(value) || isProxy(value)) return false;
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

// Security policy: own enumerable data properties are snapshotted from their
// descriptors. Accessors and Node-detectable Proxies are rejected without
// executing getter, setter, or Proxy trap code. Accounting and transfer use
// this same inert snapshot, so caller state is never traversed twice.
function processPayload(value, options) {
  const {
    deadlineAt = Number.POSITIVE_INFINITY,
    kind = 'input',
    limits,
    makeError,
    materialize = false,
    op,
    timeoutMs,
  } = options;
  const transfers = [];
  const ancestors = new Set();
  let bytes = 0;
  let items = 0;
  const invalidCode = kind === 'input' ? 'invalid_input' : 'invalid_output';

  function fail(code, message) {
    throw makeError(code, message);
  }

  function checkDeadline() {
    if (Date.now() >= deadlineAt) {
      fail('deadline_exceeded', `operation ${op} exceeded its ${timeoutMs} ms deadline`);
    }
  }

  function addBytes(amount) {
    bytes += amount;
    if (bytes > limits.maxBytes) {
      fail('resource_limit', `operation ${op} ${kind} exceeded the ${limits.maxBytes} byte bound`);
    }
  }

  function addItems(amount) {
    items += amount;
    if (items > limits.maxItems) {
      fail('resource_limit', `operation ${op} ${kind} exceeds the ${limits.maxItems} property/item bound`);
    }
  }

  function checkCardinality(key, length) {
    if (typeof key !== 'string') return;
    const normalized = key.toLowerCase();
    let limit;
    let label;
    if (/^(?:areas|overlays?|redactions?)$/.test(normalized)) {
      limit = limits.maxOverlayCount;
      label = 'overlay';
    } else if (/^pages?$/.test(normalized)) {
      limit = limits.maxPageCount;
      label = 'page';
    } else if (/^ranges?$/.test(normalized)) {
      limit = limits.maxRangeCount;
      label = 'range';
    }
    if (limit !== undefined && length > limit) {
      fail('resource_limit', `operation ${op} ${label} count exceeds the ${limit} item bound`);
    }
  }

  function copyBinary(item) {
    if (!materialize) {
      addBytes(item.byteLength);
      return item;
    }
    if (item instanceof ArrayBuffer) {
      const copy = item.slice(0);
      addBytes(copy.byteLength);
      transfers.push(copy);
      return copy;
    }
    const source = new Uint8Array(item.buffer, item.byteOffset, item.byteLength);
    const copy = new Uint8Array(source.byteLength);
    copy.set(source);
    addBytes(copy.byteLength);
    transfers.push(copy.buffer);
    return copy;
  }

  function enumerableDescriptors(item) {
    if (isProxy(item)) fail(invalidCode, `operation ${op} ${kind} contains a Proxy`);
    const descriptors = Object.getOwnPropertyDescriptors(item);
    const entries = [];
    for (const key of Reflect.ownKeys(descriptors)) {
      const descriptor = descriptors[key];
      if (!descriptor.enumerable) continue;
      if (typeof key !== 'string') {
        fail(invalidCode, `operation ${op} ${kind} contains a symbol property`);
      }
      if (!Object.prototype.hasOwnProperty.call(descriptor, 'value')) {
        fail(invalidCode, `operation ${op} ${kind} contains an accessor property`);
      }
      entries.push([key, descriptor.value]);
    }
    return entries;
  }

  function visit(item, depth, key) {
    checkDeadline();
    if (depth > limits.maxDepth) {
      fail('resource_limit', `operation ${op} ${kind} exceeds the ${limits.maxDepth} level nesting bound`);
    }
    if (typeof item === 'string') {
      addBytes(Buffer.byteLength(item, 'utf8'));
      return item;
    }
    if (item instanceof ArrayBuffer || ArrayBuffer.isView(item)) return copyBinary(item);
    if (item === null || item === undefined || typeof item === 'number' ||
        typeof item === 'bigint' || typeof item === 'boolean') return item;
    if (typeof item !== 'object') {
      fail(invalidCode, `operation ${op} ${kind} contains an unsupported value`);
    }
    if (isProxy(item)) fail(invalidCode, `operation ${op} ${kind} contains a Proxy`);
    if (ancestors.has(item)) fail(invalidCode, `operation ${op} ${kind} contains a cycle`);
    ancestors.add(item);

    let copy;
    if (Array.isArray(item)) {
      checkCardinality(key, item.length);
      addItems(item.length);
      copy = materialize ? new Array(item.length) : item;
      for (const [childKey, child] of enumerableDescriptors(item)) {
        if (typeof childKey !== 'string') {
          fail(invalidCode, `operation ${op} ${kind} contains a non-index array property`);
        }
        const index = Number(childKey);
        if (!Number.isSafeInteger(index) || index < 0 || index >= item.length || String(index) !== childKey) {
          fail(invalidCode, `operation ${op} ${kind} contains a non-index array property`);
        }
        addBytes(Buffer.byteLength(childKey, 'utf8'));
        const childCopy = visit(child, depth + 1);
        if (materialize) copy[index] = childCopy;
      }
    } else if (isPlainObject(item)) {
      const entries = enumerableDescriptors(item);
      addItems(entries.length);
      copy = materialize ? Object.create(null) : item;
      for (const [childKey, child] of entries) {
        addBytes(Buffer.byteLength(childKey, 'utf8'));
        const childCopy = visit(child, depth + 1, childKey);
        if (materialize) {
          Object.defineProperty(copy, childKey, {
            configurable: true,
            enumerable: true,
            value: childCopy,
            writable: true,
          });
        }
      }
    } else {
      fail(invalidCode, `operation ${op} ${kind} contains a non-plain object`);
    }
    ancestors.delete(item);
    return copy;
  }

  const snapshot = visit(value, 0);
  checkDeadline();
  return { bytes, snapshot, transfers };
}

const DEFAULT_RESOURCE_LIMITS = Object.freeze({ maxOldGenerationSizeMb: 1536 });
const DEFAULT_TIMEOUT_MS = 120_000;
const DEFAULT_MAX_QUEUE_LENGTH = 32;
const DEFAULT_MAX_QUEUED_BYTES = 256 * 1024 * 1024;
const DEFAULT_MAX_INPUT_BYTES = 128 * 1024 * 1024;
const DEFAULT_MAX_OUTPUT_BYTES = 512 * 1024 * 1024;
const DEFAULT_MAX_INPUT_DEPTH = 64;
const DEFAULT_MAX_INPUT_ITEMS = 100_000;
const DEFAULT_MAX_OVERLAY_COUNT = 800;
const DEFAULT_MAX_PAGE_COUNT = 10_000;
const DEFAULT_MAX_RANGE_COUNT = 10_000;
const BOOT_FAILURE_THRESHOLD = 2;
const BOOT_BACKOFF_MS = 60_000;

class EngineWorkerError extends Error {
  constructor(code, message, detail) {
    const bounded = sanitizeWireError({ kind: 'engine', code, message, detail });
    super(bounded.message);
    this.name = 'EngineWorkerError';
    this.code = code;
    if (bounded.detail !== undefined) {
      // Structured envelope detail is preserved verbatim (string or object).
      // engineMessage stays a string compatibility alias: the string detail
      // itself, or the stable machine reason slug of a structured detail.
      this.detail = bounded.detail;
      if (typeof bounded.detail === 'string') {
        this.engineMessage = bounded.detail;
      } else {
        this.engineMessage = bounded.detail.reason;
      }
    }
  }
}

function errorFor(code, message, detail) {
  return new EngineWorkerError(code, message, detail);
}

// The stable engine-error taxonomy. The wasm boundary serializes each failure
// as {"dy":1,"code":…} and the worker entry forwards the code VERBATIM — the
// client never infers error identity from message prose. An unknown code
// downgrades to internal_error (fail-closed against taxonomy drift).
const ENGINE_ERROR_CODES = Object.freeze([
  'invalid_input',
  'unsupported_format',
  'unsupported_feature',
  'missing_glyph',
  'missing_asset',
  'password_protected',
  'malformed_container',
  'resource_limit',
  'invalid_output',
  'internal_error',
]);
const ENGINE_ERROR_CODE_SET = new Set(ENGINE_ERROR_CODES);

function engineError(payload) {
  const code = ENGINE_ERROR_CODE_SET.has(payload.code) ? payload.code : 'internal_error';
  return errorFor(code, payload.message, payload.detail);
}

function isRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function validateTimeout(timeoutMs, label) {
  if (timeoutMs === undefined || timeoutMs === null) return undefined;
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw errorFor('invalid_input', `${label} must be a positive finite number`);
  }
  return timeoutMs;
}

function deadlineError(op, timeoutMs) {
  return errorFor('deadline_exceeded', `operation ${op} exceeded its ${timeoutMs} ms deadline`);
}

function accountPayload(value, limits, deadlineAt, op, timeoutMs, kind = 'input') {
  return processPayload(value, {
    deadlineAt,
    kind,
    limits: {
      maxBytes: limits.maxInputBytes,
      maxDepth: limits.maxInputDepth,
      maxItems: limits.maxInputItems,
      maxOverlayCount: limits.maxOverlayCount,
      maxPageCount: limits.maxPageCount,
      maxRangeCount: limits.maxRangeCount,
    },
    makeError: errorFor,
    op,
    timeoutMs,
  }).bytes;
}

function validateLimit(value, fallback, label) {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw errorFor('invalid_input', `${label} must be a positive safe integer`);
  }
  return value;
}

class EngineWorkerClient {
  constructor(config, options = {}) {
    if (!isRecord(config) || typeof config.entryPath !== 'string' || !isRecord(config.workerData)) {
      throw new TypeError('invalid worker client configuration');
    }
    if (!isRecord(options)) throw errorFor('invalid_input', 'options must be an object');

    this.entryPath = config.entryPath;
    this.workerData = config.workerData;
    this.defaultTimeoutMs = validateTimeout(options.defaultTimeoutMs, 'defaultTimeoutMs') ?? DEFAULT_TIMEOUT_MS;
    this.maxQueueLength = validateLimit(options.maxQueueLength, DEFAULT_MAX_QUEUE_LENGTH, 'maxQueueLength');
    this.maxQueuedBytes = validateLimit(options.maxQueuedBytes, DEFAULT_MAX_QUEUED_BYTES, 'maxQueuedBytes');
    this.maxInputBytes = validateLimit(options.maxInputBytes, DEFAULT_MAX_INPUT_BYTES, 'maxInputBytes');
    this.maxOutputBytes = validateLimit(options.maxOutputBytes, DEFAULT_MAX_OUTPUT_BYTES, 'maxOutputBytes');
    this.maxInputDepth = validateLimit(options.maxInputDepth, DEFAULT_MAX_INPUT_DEPTH, 'maxInputDepth');
    this.maxInputItems = validateLimit(options.maxInputItems, DEFAULT_MAX_INPUT_ITEMS, 'maxInputItems');
    this.maxOverlayCount = validateLimit(options.maxOverlayCount, DEFAULT_MAX_OVERLAY_COUNT, 'maxOverlayCount');
    this.maxPageCount = validateLimit(options.maxPageCount, DEFAULT_MAX_PAGE_COUNT, 'maxPageCount');
    this.maxRangeCount = validateLimit(options.maxRangeCount, DEFAULT_MAX_RANGE_COUNT, 'maxRangeCount');
    if (options.resourceLimits !== undefined && !isRecord(options.resourceLimits)) {
      throw errorFor('invalid_input', 'resourceLimits must be an object');
    }
    this.resourceLimits = {
      ...DEFAULT_RESOURCE_LIMITS,
      ...(options.resourceLimits || {}),
    };

    this.worker = null;
    this.phase = 'idle';
    this.queue = [];
    this.queuedBytes = 0;
    this.active = null;
    this.nextId = 1;
    this.version = null;
    this.closed = false;
    this.consecutiveBootFailures = 0;
    this.retryAfter = 0;
    this.termination = null;
  }

  createInvocation(op, timeoutMs, startedAt = Date.now()) {
    const timeout = validateTimeout(timeoutMs, 'timeoutMs') ?? this.defaultTimeoutMs;
    const deadlineAt = startedAt + timeout;
    return Object.freeze({ op, timeoutMs: timeout, deadlineAt });
  }

  assertInvocationActive(invocation) {
    if (Date.now() >= invocation.deadlineAt) {
      throw deadlineError(invocation.op, invocation.timeoutMs);
    }
  }

  invoke(op, args, timeoutMsOrInvocation, startedAt = Date.now()) {
    if (this.closed) {
      return Promise.reject(errorFor('engine_unavailable', 'engine worker is closed'));
    }
    if (typeof op !== 'string' || op.length === 0) {
      return Promise.reject(errorFor('protocol_error', 'operation name must be a non-empty string'));
    }

    let invocation;
    try {
      invocation = isRecord(timeoutMsOrInvocation) &&
        timeoutMsOrInvocation.op === op &&
        Number.isFinite(timeoutMsOrInvocation.deadlineAt)
        ? timeoutMsOrInvocation
        : this.createInvocation(op, timeoutMsOrInvocation, startedAt);
    } catch (error) {
      return Promise.reject(error);
    }

    let inputBytes;
    let copiedArgs;
    let transfers;
    try {
      const processed = processPayload(args, {
        deadlineAt: invocation.deadlineAt,
        kind: 'input',
        limits: {
          maxBytes: this.maxInputBytes,
          maxDepth: this.maxInputDepth,
          maxItems: this.maxInputItems,
          maxOverlayCount: this.maxOverlayCount,
          maxPageCount: this.maxPageCount,
          maxRangeCount: this.maxRangeCount,
        },
        makeError: errorFor,
        materialize: true,
        op,
        timeoutMs: invocation.timeoutMs,
      });
      inputBytes = processed.bytes;
      copiedArgs = processed.snapshot;
      transfers = processed.transfers;
    } catch (error) {
      try {
        this.assertInvocationActive(invocation);
      } catch (deadline) {
        return Promise.reject(deadline);
      }
      return Promise.reject(error);
    }
    if (this.queue.length >= this.maxQueueLength || this.queuedBytes + inputBytes > this.maxQueuedBytes) {
      return Promise.reject(errorFor('queue_busy', 'engine worker queue capacity exceeded'));
    }

    return new Promise((resolve, reject) => {
      const job = {
        id: this.nextId++,
        op,
        args: copiedArgs,
        transfers,
        inputBytes,
        timeoutMs: invocation.timeoutMs,
        deadlineAt: invocation.deadlineAt,
        resolve,
        reject,
        timer: null,
      };
      this.queue.push(job);
      this.queuedBytes += inputBytes;
      job.timer = setTimeout(() => this._expire(job), Math.max(0, job.deadlineAt - Date.now()));
      this._pump();
    });
  }

  engineVersion() {
    if (this.version !== null && this.worker && this.phase === 'ready') {
      return Promise.resolve(this.version);
    }
    return this.invoke('__engineVersion', {}, undefined);
  }

  async close() {
    if (this.closed) return;
    this.closed = true;
    const error = errorFor('engine_unavailable', 'engine worker was closed');
    if (this.active) {
      const active = this.active;
      this.active = null;
      if (active.timer) clearTimeout(active.timer);
      active.reject(error);
    }
    for (const job of this.queue.splice(0)) {
      if (job.timer) clearTimeout(job.timer);
      job.reject(error);
    }
    this.queuedBytes = 0;
    const worker = this.worker;
    this.phase = 'closed';
    this.version = null;
    const pendingTermination = this.termination;
    if (worker) await this._terminateWorker(worker);
    if (pendingTermination) await pendingTermination;
  }

  _pump() {
    this._pumpInner();
    this._syncRef();
  }

  // An idle worker must never hold the host process open: short-lived callers
  // (isolated processing children, one-shot scripts) exit naturally once their
  // awaited ops settle, while an in-flight or queued job keeps the worker
  // ref()ed so the event loop cannot exit mid-operation.
  _syncRef() {
    if (!this.worker) return;
    if (this.active || this.queue.length > 0) this.worker.ref();
    else this.worker.unref();
  }

  _pumpInner() {
    if (this.closed || this.active || this.queue.length === 0 || this.phase === 'terminating') return;
    if (!this.worker) {
      if (Date.now() < this.retryAfter) {
        this._rejectQueuedBootFailure(
          'worker boot is temporarily disabled after repeated failures',
          'engine_unavailable',
        );
        return;
      }
      this._spawn();
      return;
    }
    if (this.phase !== 'ready') return;

    const job = this.queue.shift();
    this.queuedBytes -= job.inputBytes;
    this.active = job;

    try {
      this.worker.postMessage({ id: job.id, op: job.op, args: job.args }, job.transfers);
    } catch (error) {
      this._rejectActiveAndKill(errorFor('protocol_error', `failed to send worker request: ${error.message}`));
    }
  }

  _spawn() {
    this.phase = 'booting';
    let worker;
    try {
      worker = new Worker(this.entryPath, {
        workerData: {
          ...this.workerData,
          maxOutputBytes: this.maxOutputBytes,
          maxOutputDepth: this.maxInputDepth,
          maxOutputItems: this.maxInputItems,
          maxOutputOverlayCount: this.maxOverlayCount,
          maxOutputPageCount: this.maxPageCount,
          maxOutputRangeCount: this.maxRangeCount,
        },
        resourceLimits: this.resourceLimits,
      });
    } catch (error) {
      this.phase = 'idle';
      this._recordBootFailure();
      this._rejectQueuedBootFailure(`worker failed to start: ${error.message}`);
      return;
    }

    this.worker = worker;
    worker.on('message', (message) => this._onMessage(worker, message));
    worker.on('error', (error) => this._onWorkerError(worker, error));
    worker.on('exit', (code) => this._onWorkerExit(worker, code));
  }

  _onMessage(worker, message) {
    if (worker !== this.worker) return;
    if (this.phase === 'booting') {
      if (!isRecord(message) || message.type !== 'ready' || typeof message.version !== 'string') {
        this._failBoot('worker sent malformed ready message');
        return;
      }
      this.phase = 'ready';
      this.version = message.version;
      this.consecutiveBootFailures = 0;
      this.retryAfter = 0;
      this._pump();
      return;
    }

    const job = this.active;
    if (
      this.phase !== 'ready' ||
      !job ||
      !isRecord(message) ||
      message.id !== job.id ||
      typeof message.ok !== 'boolean'
    ) {
      this._rejectActiveAndKill(errorFor('protocol_error', 'worker sent a malformed response'));
      return;
    }

    if (message.ok) {
      if (!Object.prototype.hasOwnProperty.call(message, 'result')) {
        this._rejectActiveAndKill(errorFor('protocol_error', 'worker response omitted result'));
        return;
      }
      if (Date.now() >= job.deadlineAt) {
        this._rejectActiveAndKill(deadlineError(job.op, job.timeoutMs));
        return;
      }
      let outputBytes;
      try {
        outputBytes = accountPayload(message.result, {
          ...this,
          maxInputBytes: this.maxOutputBytes,
        }, job.deadlineAt, job.op, job.timeoutMs, 'output');
      } catch (error) {
        this._finishActive(error);
        return;
      }
      if (outputBytes > this.maxOutputBytes) {
        this._finishActive(errorFor(
          'resource_limit',
          `operation ${job.op} output exceeded the ${this.maxOutputBytes} byte bound (${outputBytes} bytes)`,
        ));
        return;
      }
      this._finishActive(null, message.result);
      return;
    }

    if (!isRecord(message.error) || typeof message.error.message !== 'string' || typeof message.error.kind !== 'string') {
      this._rejectActiveAndKill(errorFor('protocol_error', 'worker sent a malformed error response'));
      return;
    }
    if (message.error.kind === 'trap') {
      this._rejectActiveAndKill(errorFor('trap', message.error.message, message.error.message));
      return;
    }
    if (message.error.kind !== 'engine') {
      this._rejectActiveAndKill(errorFor('protocol_error', 'worker sent an unknown error kind'));
      return;
    }
    if (typeof message.error.code !== 'string') {
      this._rejectActiveAndKill(errorFor('protocol_error', 'worker engine error omitted its code'));
      return;
    }
    this._finishActive(engineError(message.error));
  }

  _onWorkerError(worker, error) {
    if (worker !== this.worker) return;
    if (this.phase === 'booting') {
      this._failBoot(`worker boot failed: ${error.message}`);
      return;
    }
    // Classification uses Node's machine code only; message prose is never an API.
    const resourceFailure = error?.code === 'ERR_WORKER_OUT_OF_MEMORY';
    if (this.active) {
      this._rejectActiveAndKill(errorFor(
        resourceFailure ? 'resource_limit' : 'trap',
        `worker failed during operation: ${error.message}`,
        error.message,
      ));
    } else {
      this._killWorker();
    }
  }

  _onWorkerExit(worker, code) {
    if (worker !== this.worker) return;
    if (this.phase === 'booting') {
      this._failBoot(`worker exited during boot with code ${code}`);
      return;
    }
    if (this.active) {
      this._rejectActiveAndKill(errorFor('trap', `worker exited during operation with code ${code}`));
    } else {
      this._killWorker();
      this._pump();
    }
  }

  _finishActive(error, result) {
    const job = this.active;
    if (!job) return;
    this.active = null;
    if (job.timer) clearTimeout(job.timer);
    if (error) job.reject(error);
    else job.resolve(result);
    this._pump();
  }

  _expire(job) {
    if (this.active === job) {
      this._rejectActiveAndKill(errorFor(
        'deadline_exceeded',
        `operation ${job.op} exceeded its ${job.timeoutMs} ms deadline`,
      ));
      return;
    }
    const index = this.queue.indexOf(job);
    if (index === -1) return;
    this.queue.splice(index, 1);
    this.queuedBytes -= job.inputBytes;
    if (job.timer) clearTimeout(job.timer);
    job.timer = null;
    job.reject(errorFor(
      'deadline_exceeded',
      `operation ${job.op} exceeded its ${job.timeoutMs} ms deadline`,
    ));
    if (this.queue.length === 0 && this.phase === 'booting') this._killWorker();
    this._pump();
  }

  _rejectActiveAndKill(error) {
    const job = this.active;
    this.active = null;
    if (job && job.timer) clearTimeout(job.timer);
    const termination = this._killWorker();
    termination.finally(() => {
      if (job) job.reject(error);
      this._pump();
    });
  }

  _killWorker() {
    const worker = this.worker;
    if (!worker) return this.termination || Promise.resolve();
    return this._terminateWorker(worker);
  }

  _terminateWorker(worker) {
    if (this.worker === worker) this.worker = null;
    this.phase = this.closed ? 'closed' : 'terminating';
    this.version = null;
    const termination = worker.terminate().catch(() => undefined).then(() => {
      if (this.termination !== termination) return;
      this.termination = null;
      this.phase = this.closed ? 'closed' : 'idle';
      this._pump();
    });
    this.termination = termination;
    return termination;
  }

  _recordBootFailure() {
    this.consecutiveBootFailures += 1;
    if (this.consecutiveBootFailures >= BOOT_FAILURE_THRESHOLD) {
      this.retryAfter = Date.now() + BOOT_BACKOFF_MS;
    }
  }

  _failBoot(message) {
    this._recordBootFailure();
    this._killWorker();
    this._rejectQueuedBootFailure(message);
  }

  _rejectQueuedBootFailure(message, code = 'worker_boot_failed') {
    for (const job of this.queue.splice(0)) {
      if (job.timer) clearTimeout(job.timer);
      job.reject(errorFor(code, message));
    }
    this.queuedBytes = 0;
  }
}

module.exports = {
  ERROR_CONTEXT_MAX_BYTES,
  ERROR_MESSAGE_MAX_BYTES,
  ERROR_REASON_MAX_BYTES,
  EngineWorkerClient,
  EngineWorkerError,
  ENGINE_ERROR_CODES,
  DEFAULT_TIMEOUT_MS,
  errorFor,
  processPayload,
  sanitizeWireError,
};
