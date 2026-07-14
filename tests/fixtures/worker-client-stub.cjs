'use strict';

const { parentPort, workerData, threadId } = require('node:worker_threads');

if (workerData.spawnCounter) Atomics.add(new Int32Array(workerData.spawnCounter), 0, 1);

setTimeout(() => {
  parentPort.postMessage({ type: 'ready', version: 'stub-1' });
}, workerData.readyDelayMs || 0);

parentPort.on('message', ({ id, op, args }) => {
  if (op === 'sleep' || op === 'late') {
    setTimeout(() => parentPort.postMessage({ id, ok: true, result: args.value ?? op }), args.ms);
    return;
  }
  if (op === 'output') {
    const result = new Uint8Array(args.size);
    parentPort.postMessage({ id, ok: true, result }, [result.buffer]);
    return;
  }
  if (op === 'outputArray') {
    parentPort.postMessage({ id, ok: true, result: Array(args.size).fill(0) });
    return;
  }
  if (op === 'spawnCount') {
    const result = workerData.spawnCounter
      ? Atomics.load(new Int32Array(workerData.spawnCounter), 0)
      : 1;
    parentPort.postMessage({ id, ok: true, result });
    return;
  }
  if (op === 'identity') {
    parentPort.postMessage({ id, ok: true, result: { threadId } });
    return;
  }
  parentPort.postMessage({ id, ok: true, result: args });
});
