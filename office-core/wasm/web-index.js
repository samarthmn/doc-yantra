import init, * as engine from './office_core_wasm.js';
import {
  IndexedDbFontCache,
  MemoryFontCache,
  createConvertOnDemand,
  mergeHostTelemetry,
  resolveAndRegisterFonts,
} from './font-provider.js';

export * from './office_core_wasm.js';
export {
  IndexedDbFontCache,
  MemoryFontCache,
  mergeHostTelemetry,
  resolveAndRegisterFonts,
};
export const convertOnDemand = createConvertOnDemand(engine);
export default init;
