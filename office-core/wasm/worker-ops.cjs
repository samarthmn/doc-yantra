'use strict';

function createOperations(engine) {
  return {
    convert: ({ bytes, extension }) => engine.office_to_pdf(bytes, extension),
    profile: ({ bytes, extension }) => JSON.parse(engine.office_profile(bytes, extension)),
  };
}

module.exports = { createOperations };
