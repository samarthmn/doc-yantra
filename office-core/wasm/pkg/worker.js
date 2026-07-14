'use strict';

const { types: { isProxy } } = require('node:util');
const {
  EngineWorkerClient,
  EngineWorkerError,
  errorFor,
} = require('./worker-client.cjs');

const MAX_INPUT_BYTES = 50 * 1024 * 1024;

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

function requireStringArray(value, label) {
  if (!Array.isArray(value) || isProxy(value)) {
    throw errorFor('invalid_input', `${label} must be an array of non-empty strings`);
  }
  const snapshot = new Array(value.length);
  const descriptors = Object.getOwnPropertyDescriptors(value);
  for (const key of Reflect.ownKeys(descriptors)) {
    const descriptor = descriptors[key];
    if (!descriptor.enumerable) continue;
    if (typeof key !== 'string') {
      throw errorFor('invalid_input', `${label} must be an array of non-empty strings`);
    }
    const index = Number(key);
    if (!Number.isSafeInteger(index) || index < 0 || index >= value.length ||
        String(index) !== key || !Object.prototype.hasOwnProperty.call(descriptor, 'value') ||
        typeof descriptor.value !== 'string' || descriptor.value.length === 0) {
      throw errorFor('invalid_input', `${label} must be an array of non-empty strings`);
    }
    snapshot[index] = descriptor.value;
  }
  if (Object.keys(snapshot).length !== value.length) {
    throw errorFor('invalid_input', `${label} must be an array of non-empty strings`);
  }
  return snapshot;
}

function validateInput(input, label) {
  const value = requireRecord(input, `${label} input`);
  if (value.bytes !== null && typeof value.bytes === 'object' && isProxy(value.bytes)) {
    throw errorFor('invalid_input', 'bytes must not be a Proxy');
  }
  if (!(value.bytes instanceof ArrayBuffer) && !ArrayBuffer.isView(value.bytes)) {
    throw errorFor('invalid_input', 'bytes must be an ArrayBuffer or typed array');
  }
  if (value.bytes.byteLength > MAX_INPUT_BYTES) {
    throw errorFor('resource_limit', `input is ${value.bytes.byteLength} bytes, over the 50 MiB in-process limit`);
  }
  if (typeof value.extension !== 'string' || value.extension.length === 0) {
    throw errorFor('invalid_input', 'extension must be a non-empty string');
  }
  return value;
}

function invokeValidated(client, op, input, validate) {
  const startedAt = Date.now();
  let invocation;
  try {
    const snapshot = requireRecord(input, `${op} input`);
    invocation = client.createInvocation(op, snapshot.timeoutMs, startedAt);
    const args = validate(snapshot);
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

function createOfficeEngineWorker(options = {}) {
  options = requireRecord(options, 'options');
  const extraFontDirs = options.extraFontDirs === undefined
    ? []
    : requireStringArray(options.extraFontDirs, 'extraFontDirs');
  const entryPath = process.env.DOC_YANTRA_WORKER_TEST_HOOKS === '1' && options.__workerEntryPath
    ? options.__workerEntryPath
    : require.resolve('./worker-entry.cjs');
  const client = new EngineWorkerClient({
    entryPath,
    workerData: {
      gluePath: require.resolve('./office_core_wasm.js'),
      opsPath: require.resolve('./worker-ops.cjs'),
      extraFontDirs: [...extraFontDirs],
    },
  }, options);

  const api = {
    convert(input) {
      return invokeValidated(client, 'convert', input, (snapshot) => {
        const { bytes, extension } = validateInput(snapshot, 'convert');
        return { bytes, extension };
      });
    },
    profile(input) {
      return invokeValidated(client, 'profile', input, (snapshot) => {
        const { bytes, extension } = validateInput(snapshot, 'profile');
        return { bytes, extension };
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

module.exports = { createOfficeEngineWorker, EngineWorkerError };
