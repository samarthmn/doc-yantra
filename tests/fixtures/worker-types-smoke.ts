import {
  EngineWorkerError as PdfWorkerError,
  createPdfEngineWorker,
  type RedactionWarning,
} from "../../pdf-core/wasm/pkg/worker.js";
import {
  createOfficeEngineWorker,
  type OfficeProfile,
} from "../../office-core/wasm/pkg/worker.js";

const bytes = new Uint8Array();
const pdf = createPdfEngineWorker({ defaultTimeoutMs: 5_000, maxInputDepth: 32 });
void pdf.mergePdfs({ pdfs: [bytes], timeoutMs: 100 });
void pdf.applyWatermark({ pdf: bytes, watermarkJson: "{}" });
void pdf.csvToPdf({ csv: bytes });
void pdf.markdownToPdf({ markdown: "# title" });
void pdf.textToPdf({ text: "text" });
void pdf.redactPdf({ pdf: bytes, areasJson: "[]" });
void pdf.redactPdfWithReport({ pdf: bytes, areasJson: "[]" }).then((report) => {
  const warning: RedactionWarning | undefined = report.warnings[0];
  if (warning) console.log(warning.code, warning.page);
});
void pdf.pageCount({ pdf: bytes });
void pdf.validatePdf({ pdf: bytes });
void pdf.engineVersion();
void pdf.close();

const office = createOfficeEngineWorker({ extraFontDirs: [], maxInputItems: 50_000 });
void office.convert({ bytes, extension: "docx", timeoutMs: 100 });
void office.profile({ bytes, extension: "docx" }).then((profile: OfficeProfile) => profile.supported);
void office.engineVersion();
void office.close();

const error = new PdfWorkerError("invalid_input", "bad input");
console.log(error.code, error.message, error.detail);
