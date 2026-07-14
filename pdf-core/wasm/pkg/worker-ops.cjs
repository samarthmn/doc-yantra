'use strict';

function createOperations(engine) {
  return {
    mergePdfs: ({ pdfs, watermarkJson, watermarkImage }) =>
      engine.merge_pdfs(pdfs, watermarkJson, watermarkImage),
    applyWatermark: ({ pdf, watermarkJson, watermarkImage }) =>
      engine.apply_watermark(pdf, watermarkJson, watermarkImage),
    csvToPdf: ({ csv }) => engine.csv_to_pdf(csv),
    markdownToPdf: ({ markdown }) => engine.markdown_to_pdf(markdown),
    textToPdf: ({ text }) => engine.text_to_pdf(text),
    redactPdf: ({ pdf, areasJson }) => engine.redact_pdf(pdf, areasJson),
    redactPdfWithReport: ({ pdf, areasJson }) => engine.redact_pdf_with_report(pdf, areasJson),
    pageCount: ({ pdf }) => engine.page_count(pdf),
    validatePdf: ({ pdf }) => engine.validate_pdf(pdf),
  };
}

module.exports = { createOperations };
