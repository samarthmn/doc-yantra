import { afterEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';
import { readFileSync } from 'node:fs';

const require = createRequire(import.meta.url);
const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '..');
const ENTRY = join(HERE, 'fixtures', 'worker-client-stub.cjs');
const {
  EngineWorkerClient,
  EngineWorkerError,
  DEFAULT_TIMEOUT_MS,
  errorFor,
} = require(join(ROOT, 'wasm-worker', 'worker-client.cjs'));
const {
  ERROR_CONTEXT_MAX_BYTES,
  ERROR_MESSAGE_MAX_BYTES,
  ERROR_REASON_MAX_BYTES,
  processPayload,
  sanitizeWireError,
} = require(join(ROOT, 'wasm-worker', 'worker-client.cjs'));
const clients = new Set();

function client(options = {}, workerData = {}) {
  const value = new EngineWorkerClient({ entryPath: ENTRY, workerData }, options);
  clients.add(value);
  return value;
}

function assertCode(code, messagePattern) {
  return (error) => {
    assert.ok(error instanceof EngineWorkerError, `expected EngineWorkerError, got ${error}`);
    assert.equal(error.code, code);
    if (messagePattern) assert.match(error.message, messagePattern);
    return true;
  };
}

afterEach(async () => {
  await Promise.all([...clients].map((value) => value.close()));
  clients.clear();
});

test('default deadline is finite when caller provides none', () => {
  const worker = client();
  assert.ok(Number.isFinite(DEFAULT_TIMEOUT_MS) && DEFAULT_TIMEOUT_MS > 0);
  assert.equal(worker.defaultTimeoutMs, DEFAULT_TIMEOUT_MS);
});

test('deadline includes elapsed synchronous work before invoke', async () => {
  const spawnCounter = new SharedArrayBuffer(4);
  const worker = client({}, { spawnCounter });
  await worker.engineVersion();
  await assert.rejects(
    worker.invoke('echo', { ok: true }, 10, Date.now() - 20),
    assertCode('deadline_exceeded'),
  );
  assert.equal(Atomics.load(new Int32Array(spawnCounter), 0), 1);
});

test('queued deadline starts at invocation under contention', { timeout: 4_000 }, async () => {
  const worker = client();
  await worker.engineVersion();
  const first = worker.invoke('sleep', { ms: 2_000, value: 'first' });
  const started = performance.now();
  await assert.rejects(
    worker.invoke('echo', { value: 'second' }, 300),
    assertCode('deadline_exceeded'),
  );
  assert.ok(performance.now() - started < 1_000, 'queued deadline did not include queue wait');
  assert.equal(await first, 'first');
});

test('deadline includes worker boot time', async () => {
  const worker = client({}, { readyDelayMs: 500 });
  const started = performance.now();
  await assert.rejects(worker.invoke('echo', {}, 200), assertCode('deadline_exceeded'));
  const elapsed = performance.now() - started;
  assert.ok(elapsed >= 150 && elapsed < 1_000, `deadline settled after ${elapsed.toFixed(1)} ms`);
});

test('queued expiry leaves active worker alive and reusable', async () => {
  const spawnCounter = new SharedArrayBuffer(4);
  const worker = client({}, { spawnCounter });
  await worker.engineVersion();
  const first = worker.invoke('sleep', { ms: 500, value: 'first' });
  await assert.rejects(worker.invoke('echo', {}, 100), assertCode('deadline_exceeded'));
  assert.equal(await first, 'first');
  assert.equal(await worker.invoke('spawnCount', {}), 1);
});

test('maxQueueLength rejects excess waiting work with queue_busy', async () => {
  const worker = client({ maxQueueLength: 1 });
  await worker.engineVersion();
  const active = worker.invoke('sleep', { ms: 300 });
  const queued = worker.invoke('echo', { value: 'queued' });
  await assert.rejects(worker.invoke('echo', { value: 'excess' }), assertCode('queue_busy'));
  await active;
  assert.deepEqual(await queued, { value: 'queued' });
});

test('maxQueuedBytes rejects excess waiting bytes with queue_busy', async () => {
  const worker = client({ maxQueuedBytes: 10 });
  await worker.engineVersion();
  const active = worker.invoke('sleep', { ms: 300 });
  await assert.rejects(
    worker.invoke('echo', { bytes: new Uint8Array(6) }),
    assertCode('queue_busy'),
  );
  await active;
});

test('maxInputBytes rejects before enqueue with resource_limit', async () => {
  const spawnCounter = new SharedArrayBuffer(4);
  const worker = client({ maxInputBytes: 4 }, { spawnCounter });
  await assert.rejects(
    worker.invoke('echo', { bytes: new Uint8Array(5) }),
    assertCode('resource_limit'),
  );
  assert.equal(Atomics.load(new Int32Array(spawnCounter), 0), 0);
});

test('input snapshot rejects accessors without executing them', async () => {
  const spawnCounter = new SharedArrayBuffer(4);
  const worker = client({ maxInputBytes: 4 }, { spawnCounter });
  let reads = 0;
  const payload = {};
  Object.defineProperty(payload, 'bytes', {
    enumerable: true,
    get() {
      reads += 1;
      return new Uint8Array(reads === 1 ? 1 : 1_000_000);
    },
  });
  await assert.rejects(worker.invoke('echo', payload), assertCode('invalid_input'));
  assert.equal(reads, 0, 'rejected getter must not execute');
  assert.equal(Atomics.load(new Int32Array(spawnCounter), 0), 0);
});

test('input snapshot rejects detectable Proxies before worker spawn', async () => {
  const spawnCounter = new SharedArrayBuffer(4);
  const worker = client({}, { spawnCounter });
  await assert.rejects(
    worker.invoke('echo', new Proxy({ value: 'hidden' }, {})),
    assertCode('invalid_input'),
  );
  assert.equal(Atomics.load(new Int32Array(spawnCounter), 0), 0);
});

test('input binary is copied into inert snapshot before caller can resize it', async (t) => {
  const buffer = new ArrayBuffer(1, { maxByteLength: 1024 });
  if (typeof buffer.resize !== 'function') {
    t.skip('ResizableArrayBuffer is unavailable in this Node build');
    return;
  }
  const worker = client();
  const result = worker.invoke('echo', { buffer });
  buffer.resize(1024);
  assert.equal((await result).buffer.byteLength, 1);
});

test('recursive accounting includes nested UTF-8 strings and object keys', async () => {
  const worker = client({ maxInputBytes: 8 });
  await assert.rejects(
    worker.invoke('echo', { key: { nested: 'é' } }),
    assertCode('resource_limit'),
  );
});

test('recursive accounting accepts DataView and counts its viewed bytes', async () => {
  const worker = client({ maxInputBytes: 3 });
  await assert.rejects(
    worker.invoke('echo', { view: new DataView(new ArrayBuffer(4)) }),
    assertCode('resource_limit'),
  );
});

test('cyclic payload fails before worker spawn with invalid_input', async () => {
  const spawnCounter = new SharedArrayBuffer(4);
  const worker = client({}, { spawnCounter });
  const cyclic = {};
  cyclic.self = cyclic;
  await assert.rejects(worker.invoke('echo', cyclic), assertCode('invalid_input'));
  assert.equal(Atomics.load(new Int32Array(spawnCounter), 0), 0);
});

test('nesting, item, and named cardinality bounds use resource_limit', async () => {
  await assert.rejects(
    client({ maxInputDepth: 2 }).invoke('echo', { a: { b: { c: true } } }),
    assertCode('resource_limit'),
  );
  await assert.rejects(
    client({ maxInputItems: 2 }).invoke('echo', { values: [1, 2] }),
    assertCode('resource_limit'),
  );
  await assert.rejects(
    client({ maxOverlayCount: 1 }).invoke('echo', { overlays: [{}, {}] }),
    assertCode('resource_limit'),
  );
  await assert.rejects(
    client({ maxPageCount: 1 }).invoke('echo', { pages: [{}, {}] }),
    assertCode('resource_limit'),
  );
  await assert.rejects(
    client({ maxRangeCount: 1 }).invoke('echo', { ranges: [{}, {}] }),
    assertCode('resource_limit'),
  );
});

test('maxOutputBytes rejects oversized output and keeps worker alive', async () => {
  const spawnCounter = new SharedArrayBuffer(4);
  const worker = client({ maxOutputBytes: 4 }, { spawnCounter });
  await assert.rejects(
    worker.invoke('output', { size: 5 }),
    assertCode('resource_limit', /output exceeded the 4 byte bound/),
  );
  assert.deepEqual(await worker.invoke('echo', { ok: true }), { ok: true });
  assert.equal(await worker.invoke('spawnCount', {}), 1);
});

test('late response after deadline settles once and cannot corrupt next operation', async () => {
  const worker = client();
  await worker.engineVersion();
  let settlements = 0;
  const late = worker.invoke('late', { ms: 200 }, 50).then(
    () => { settlements += 1; },
    (error) => {
      settlements += 1;
      assertCode('deadline_exceeded')(error);
    },
  );
  await late;
  await new Promise((resolve) => setTimeout(resolve, 300));
  assert.equal(settlements, 1);
  assert.deepEqual(await worker.invoke('echo', { next: true }), { next: true });
});

test('timed-out worker terminates before replacement and identity changes', async () => {
  const worker = client();
  const before = await worker.invoke('identity', {});
  await assert.rejects(
    worker.invoke('sleep', { ms: 5_000 }, 50),
    assertCode('deadline_exceeded'),
  );
  const after = await worker.invoke('identity', {});
  assert.notEqual(after.threadId, before.threadId);
});

test('boot stops when all queued jobs expire and next job uses replacement', async () => {
  const spawnCounter = new SharedArrayBuffer(4);
  const worker = client({}, { readyDelayMs: 500, spawnCounter });
  await assert.rejects(worker.invoke('identity', {}, 30), assertCode('deadline_exceeded'));
  const identity = await worker.invoke('identity', {}, 2_000);
  assert.ok(identity.threadId > 0);
  assert.equal(Atomics.load(new Int32Array(spawnCounter), 0), 2);
});

test('worker entry rejects oversized output before transfer', async () => {
  const worker = new EngineWorkerClient({
    entryPath: join(ROOT, 'wasm-worker', 'worker-entry.cjs'),
    workerData: {
      gluePath: join(HERE, 'fixtures', 'fake-glue.cjs'),
      opsPath: join(HERE, 'fixtures', 'error-shape-ops.cjs'),
    },
  }, { maxOutputBytes: 4 });
  clients.add(worker);
  await assert.rejects(
    worker.invoke('largeOutput', {}),
    assertCode('resource_limit', /worker output exceeded the 4 byte bound/),
  );
  await assert.rejects(worker.invoke('envelopeError', {}), assertCode('missing_asset'));
});

test('worker output snapshot enforces item, depth, and named cardinality bounds', () => {
  const limits = {
    maxBytes: 1024,
    maxDepth: 2,
    maxItems: 2,
    maxOverlayCount: 1,
    maxPageCount: 1,
    maxRangeCount: 1,
  };
  const process = (value) => processPayload(value, {
    kind: 'output',
    limits,
    makeError: errorFor,
    materialize: true,
    op: 'worker',
  });
  assert.throws(() => process([0, 1, 2]), assertCode('resource_limit'));
  assert.throws(() => process({ a: { b: { c: true } } }), assertCode('resource_limit'));
  assert.throws(() => process({ pages: [{}, {}] }), assertCode('resource_limit'));
  assert.throws(() => process({ ranges: [{}, {}] }), assertCode('resource_limit'));
  assert.throws(() => process({ overlays: [{}, {}] }), assertCode('resource_limit'));
});

test('error wire sanitizer preserves code and applies UTF-8 budgets', () => {
  const codeValue = 'missing_asset';
  const bounded = sanitizeWireError({
    kind: 'engine',
    code: codeValue,
    message: 'é'.repeat(ERROR_MESSAGE_MAX_BYTES),
    detail: {
      reason: 'r'.repeat(ERROR_REASON_MAX_BYTES),
      context: 'c'.repeat(ERROR_CONTEXT_MAX_BYTES),
    },
  });
  assert.equal(bounded.code, codeValue);
  assert.ok(Buffer.byteLength(bounded.message, 'utf8') <= ERROR_MESSAGE_MAX_BYTES);
  assert.deepEqual(bounded.detail, {
    reason: 'r'.repeat(ERROR_REASON_MAX_BYTES),
    context: 'c'.repeat(ERROR_CONTEXT_MAX_BYTES),
  });
  assert.equal(sanitizeWireError({
    kind: 'engine',
    code: codeValue,
    message: 'bounded',
    detail: 'x'.repeat(ERROR_REASON_MAX_BYTES + 1),
  }).detail, undefined);

  const parentError = new EngineWorkerError(
    codeValue,
    'é'.repeat(ERROR_MESSAGE_MAX_BYTES),
    { reason: 'x'.repeat(ERROR_REASON_MAX_BYTES + 1), context: 'small' },
  );
  assert.equal(parentError.code, codeValue);
  assert.ok(Buffer.byteLength(parentError.message, 'utf8') <= ERROR_MESSAGE_MAX_BYTES);
  assert.equal(parentError.detail, undefined);
});

test('worker entry returns bounded typed error for attacker-sized operation name', async () => {
  const worker = new EngineWorkerClient({
    entryPath: join(ROOT, 'wasm-worker', 'worker-entry.cjs'),
    workerData: {
      gluePath: join(HERE, 'fixtures', 'fake-glue.cjs'),
      opsPath: join(HERE, 'fixtures', 'error-shape-ops.cjs'),
    },
  });
  clients.add(worker);
  await assert.rejects(worker.invoke('x'.repeat(100_000), {}), (error) => {
    assertCode('invalid_input')(error);
    assert.ok(Buffer.byteLength(error.message, 'utf8') <= ERROR_MESSAGE_MAX_BYTES);
    return true;
  });
});

test('worker entry bounds engine message/detail without changing its code', async () => {
  const previous = process.env.DOC_YANTRA_WORKER_TEST_HOOKS;
  process.env.DOC_YANTRA_WORKER_TEST_HOOKS = '1';
  const worker = new EngineWorkerClient({
    entryPath: join(ROOT, 'wasm-worker', 'worker-entry.cjs'),
    workerData: {
      gluePath: join(HERE, 'fixtures', 'fake-glue.cjs'),
      opsPath: join(HERE, 'fixtures', 'error-shape-ops.cjs'),
    },
  });
  clients.add(worker);
  try {
    await assert.rejects(worker.invoke('__testLargeErrorEnvelope', {}), (error) => {
      assertCode('missing_asset')(error);
      assert.ok(Buffer.byteLength(error.message, 'utf8') <= ERROR_MESSAGE_MAX_BYTES);
      assert.equal(error.detail, undefined);
      return true;
    });
  } finally {
    if (previous === undefined) delete process.env.DOC_YANTRA_WORKER_TEST_HOOKS;
    else process.env.DOC_YANTRA_WORKER_TEST_HOOKS = previous;
  }
});

test('deadline covers a slow structured-clone result transfer', async () => {
  const worker = client({ maxInputItems: 1_000_000 });
  await worker.engineVersion();
  await assert.rejects(
    worker.invoke('outputArray', { size: 500_000 }, 5),
    assertCode('deadline_exceeded'),
  );
});

test('worker spawn failures use worker_boot_failed, then back off as engine_unavailable', async () => {
  const worker = new EngineWorkerClient({
    entryPath: join(HERE, 'fixtures', 'missing-worker.cjs'),
    workerData: {},
  });
  clients.add(worker);
  await assert.rejects(worker.invoke('echo', {}), assertCode('worker_boot_failed'));
  await assert.rejects(worker.invoke('echo', {}), assertCode('worker_boot_failed'));
  await assert.rejects(worker.invoke('echo', {}), assertCode('engine_unavailable'));
});

test('worker failures are never classified from message prose', () => {
  const source = readFileSync(join(ROOT, 'wasm-worker', 'worker-client.cjs'), 'utf8');
  assert.doesNotMatch(source, /out of memory\|heap limit/i);
  assert.match(source, /error\?\.code === 'ERR_WORKER_OUT_OF_MEMORY'/);
});
