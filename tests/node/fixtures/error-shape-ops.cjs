'use strict';

// Op table that throws every error shape the entry must classify:
// a machine envelope (code travels verbatim), an unknown-code envelope
// (client downgrades to internal_error), and a plain prose Error
// (entry assigns internal_error — never classified from prose).
function createOperations() {
  return {
    envelopeError: () => {
      throw new Error(
        JSON.stringify({ dy: 1, code: 'missing_asset', message: 'asset gone', detail: 'font-missing' }),
      );
    },
    unknownCodeEnvelope: () => {
      throw new Error(JSON.stringify({ dy: 1, code: 'not_a_real_code', message: 'strange' }));
    },
    plainError: () => {
      throw new Error('completely unstructured failure prose');
    },
    largeOutput: () => new Uint8Array(5),
  };
}

module.exports = { createOperations };
