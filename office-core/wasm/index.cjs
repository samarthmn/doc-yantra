'use strict';

const engine = require('./office_core_wasm.js');
const {
  MemoryFontCache,
  NodeFontCache,
  createConvertOnDemand,
  mergeHostTelemetry,
  resolveAndRegisterFonts,
} = require('./font-provider.cjs');
const {
  DEFAULT_FONTSOURCE_ALIASES,
  DEFAULT_FONTSOURCE_CATALOG,
  FontsourceProvider,
} = require('./fontsource-provider.cjs');
const {
  FALLBACK_BASE_URL,
  NOTO_FALLBACK_MANIFEST,
  NOTO_FALLBACK_REQUESTS,
  NotoFallbackProvider,
  registerNotoFallbackFonts,
} = require('./noto-fallback.cjs');

module.exports = {
  ...engine,
  MemoryFontCache,
  NodeFontCache,
  DEFAULT_FONTSOURCE_ALIASES,
  DEFAULT_FONTSOURCE_CATALOG,
  FontsourceProvider,
  FALLBACK_BASE_URL,
  NOTO_FALLBACK_MANIFEST,
  NOTO_FALLBACK_REQUESTS,
  NotoFallbackProvider,
  convertOnDemand: createConvertOnDemand(engine),
  mergeHostTelemetry,
  resolveAndRegisterFonts,
  registerNotoFallbackFonts,
};
