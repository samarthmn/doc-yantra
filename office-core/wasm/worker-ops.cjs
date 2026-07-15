'use strict';

function createOperations(engine) {
  return {
    convert: ({ bytes, extension }) => engine.office_to_pdf(bytes, extension),
    profile: ({ bytes, extension }) => JSON.parse(engine.office_profile(bytes, extension)),
    fontRequests: ({ bytes, extension }) => JSON.parse(engine.office_font_requests(bytes, extension)),
    registerFont: ({ bytes, expectation }) => JSON.parse(
      engine.office_register_font(bytes, expectation),
    ),
    convertWithOptions: ({ bytes, extension, options }) =>
      engine.office_to_pdf_with_options(bytes, extension, options),
    profileWithOptions: ({ bytes, extension, options }) => JSON.parse(
      engine.office_profile_with_options(bytes, extension, options),
    ),
  };
}

module.exports = { createOperations };
