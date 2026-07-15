'use strict';

const engine = require('./office_core_wasm.js');
const {
  MemoryFontCache,
  NodeFontCache,
  createConvertOnDemand,
  mergeHostTelemetry,
  resolveAndRegisterFonts,
} = require('./font-provider.cjs');

module.exports = {
  ...engine,
  MemoryFontCache,
  NodeFontCache,
  convertOnDemand: createConvertOnDemand(engine),
  mergeHostTelemetry,
  resolveAndRegisterFonts,
};
