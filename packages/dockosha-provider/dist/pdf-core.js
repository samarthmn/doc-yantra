"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.pageCount = exports.probePdfEngine = exports.redactPdfWithReport = exports.markdownToPdf = exports.csvToPdf = exports.mergeAndWatermark = void 0;
const node_module_1 = require("node:module");
const node_perf_hooks_1 = require("node:perf_hooks");
const zod_1 = require("zod");
const constants_js_1 = require("./constants.js");
const engine_errors_js_1 = require("./engine-errors.js");
const isRecord = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const isPdfWorkerModule = (value) => isRecord(value) && typeof value.createPdfEngineWorker === "function";
const binaryOutputSchema = zod_1.z.instanceof(Uint8Array);
const engineVersionSchema = zod_1.z.string().min(1);
const redactionReportSchema = zod_1.z.object({
    pdf: zod_1.z.instanceof(Uint8Array),
    pageCount: zod_1.z.number().int().positive(),
    warnings: zod_1.z
        .array(zod_1.z.union([
        zod_1.z.string().max(512),
        zod_1.z.object({
            code: zod_1.z.string().trim().min(1).max(64),
            page: zod_1.z.number().int().nonnegative(),
            kind: zod_1.z.string().trim().min(1).max(64).optional(),
            message: zod_1.z.string().max(512).optional(),
        }),
    ]))
        .max(1_024),
});
const requireCjs = (0, node_module_1.createRequire)(__filename);
const PDF_ENGINE_UNAVAILABLE_MESSAGE = "The PDF engine is temporarily unavailable.";
let worker = null;
let createWorker = null;
let startupFailure = null;
let hasReportedVersion = false;
let testIdleClose = null;
const unknownDetail = (error) => error instanceof Error
    ? error.message
    : typeof error === "string" && error.length > 0
        ? error
        : undefined;
try {
    const loaded = requireCjs("@samarthmn/doc-yantra/worker");
    if (!isPdfWorkerModule(loaded)) {
        throw new Error("PDF worker entrypoint has an invalid export contract");
    }
    createWorker = (defaultTimeoutMs) => loaded.createPdfEngineWorker(defaultTimeoutMs === undefined ? {} : { defaultTimeoutMs });
}
catch (error) {
    startupFailure = (0, engine_errors_js_1.createEngineFailure)({
        code: "worker_boot_failed",
        message: PDF_ENGINE_UNAVAILABLE_MESSAGE,
        operation: "probe",
        format: "pdf",
        detail: unknownDetail(error),
    });
}
const scheduleTestIdleClose = () => {
    if (process.env.NODE_TEST_CONTEXT === undefined || !worker)
        return;
    if (testIdleClose)
        clearTimeout(testIdleClose);
    testIdleClose = setTimeout(() => {
        const current = worker;
        worker = null;
        hasReportedVersion = false;
        testIdleClose = null;
        void current?.close();
    }, 100);
    testIdleClose.unref();
};
const contextualFailure = (failure, operation, format) => (0, engine_errors_js_1.createEngineFailure)({
    code: failure.code,
    message: failure.message,
    operation,
    format,
    detail: failure.detail,
});
const getWorker = (operation, format, defaultTimeoutMs) => {
    if (testIdleClose) {
        clearTimeout(testIdleClose);
        testIdleClose = null;
    }
    if (worker)
        return { ok: true, client: worker };
    if (!createWorker) {
        return (0, engine_errors_js_1.toFailureResult)(contextualFailure(startupFailure ??
            (0, engine_errors_js_1.createEngineFailure)({
                code: "engine_unavailable",
                message: PDF_ENGINE_UNAVAILABLE_MESSAGE,
                operation,
                format,
            }), operation, format));
    }
    try {
        worker = createWorker(defaultTimeoutMs);
        return { ok: true, client: worker };
    }
    catch (error) {
        return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
            code: "worker_boot_failed",
            message: PDF_ENGINE_UNAVAILABLE_MESSAGE,
            operation,
            format,
            detail: unknownDetail(error),
        }));
    }
};
const recordLoadedVersion = async (client) => {
    if (hasReportedVersion) {
        scheduleTestIdleClose();
        return;
    }
    const version = engineVersionSchema.parse(await client.engineVersion());
    hasReportedVersion = true;
    console.info(`[pdf-core-wasm] worker loaded (version ${version})`);
    scheduleTestIdleClose();
};
const normalizeFailure = (error, context) => {
    scheduleTestIdleClose();
    return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.normalizeEngineError)(error, context));
};
const immediateDeadline = (operation, format, timeoutMs, message) => {
    if (timeoutMs === undefined)
        return null;
    if (!Number.isFinite(timeoutMs)) {
        return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
            code: "invalid_input",
            message: "The PDF engine timeout must be a finite number.",
            operation,
            format,
        }));
    }
    if (timeoutMs > 0)
        return null;
    return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
        code: "deadline_exceeded",
        message,
        operation,
        format,
    }));
};
const toUint8 = (bytes) => bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
const toArrayBuffer = (bytes) => {
    const buffer = new ArrayBuffer(bytes.byteLength);
    new Uint8Array(buffer).set(bytes);
    return buffer;
};
const mergeAndWatermark = async (options) => {
    const operation = options.watermarkPayloadJson
        ? "watermark"
        : "merge";
    const totalBytes = options.pdfs.reduce((sum, pdf) => sum + pdf.byteLength, 0) +
        (options.watermarkImageBytes?.byteLength ?? 0);
    if (totalBytes > constants_js_1.PDF_MERGE_MAX_TOTAL_BYTES) {
        return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
            code: "resource_limit",
            message: `Merge input (${totalBytes} bytes) exceeds the ${constants_js_1.PDF_MERGE_MAX_TOTAL_BYTES}-byte processing limit.`,
            operation,
            format: "pdf",
        }));
    }
    const deadline = immediateDeadline(operation, "pdf", options.timeoutMs, "PDF merge deadline exceeded.");
    if (deadline)
        return deadline;
    const clientResult = getWorker(operation, "pdf");
    if (!clientResult.ok)
        return clientResult;
    try {
        const parsed = binaryOutputSchema.safeParse(await clientResult.client.mergePdfs({
            pdfs: options.pdfs.map(toUint8),
            watermarkJson: options.watermarkPayloadJson ?? undefined,
            watermarkImage: options.watermarkImageBytes ?? undefined,
            timeoutMs: options.timeoutMs,
        }));
        if (!parsed.success) {
            return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
                code: "invalid_output",
                message: "The PDF engine returned invalid output.",
                operation,
                format: "pdf",
                detail: zod_1.z.prettifyError(parsed.error),
            }));
        }
        await recordLoadedVersion(clientResult.client);
        return { ok: true, pdf: toArrayBuffer(parsed.data) };
    }
    catch (error) {
        return normalizeFailure(error, {
            operation,
            format: "pdf",
            fallbackMessage: "The PDF engine failed to merge this document.",
        });
    }
};
exports.mergeAndWatermark = mergeAndWatermark;
const csvToPdf = async (csvBytes, timeoutMs = constants_js_1.DOCUMENT_CONVERSION_TIMEOUT_MS) => {
    const source = toUint8(csvBytes);
    if (source.byteLength > constants_js_1.DOCUMENT_CONVERSION_MAX_INPUT_BYTES) {
        return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
            code: "resource_limit",
            message: `CSV (${source.byteLength} bytes) exceeds the ${constants_js_1.DOCUMENT_CONVERSION_MAX_INPUT_BYTES}-byte processing limit.`,
            operation: "csv",
            format: "csv",
        }));
    }
    const deadline = immediateDeadline("csv", "csv", timeoutMs, "CSV conversion deadline exceeded.");
    if (deadline)
        return deadline;
    const clientResult = getWorker("csv", "csv");
    if (!clientResult.ok)
        return clientResult;
    try {
        const text = new TextDecoder("utf-8").decode(source);
        const value = text.trim().length === 0
            ? await clientResult.client.textToPdf({ text: " ", timeoutMs })
            : await clientResult.client.csvToPdf({
                csv: new TextEncoder().encode(text),
                timeoutMs,
            });
        const parsed = binaryOutputSchema.safeParse(value);
        if (!parsed.success) {
            return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
                code: "invalid_output",
                message: "The PDF engine returned invalid CSV output.",
                operation: "csv",
                format: "csv",
                detail: zod_1.z.prettifyError(parsed.error),
            }));
        }
        await recordLoadedVersion(clientResult.client);
        return { ok: true, pdf: toArrayBuffer(parsed.data) };
    }
    catch (error) {
        return normalizeFailure(error, {
            operation: "csv",
            format: "csv",
            fallbackMessage: "The PDF engine failed to convert this CSV.",
        });
    }
};
exports.csvToPdf = csvToPdf;
const markdownToPdf = async (markdown, timeoutMs = constants_js_1.DOCUMENT_CONVERSION_TIMEOUT_MS) => {
    if (markdown.length > constants_js_1.DOCUMENT_CONVERSION_MAX_INPUT_BYTES) {
        return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
            code: "resource_limit",
            message: `Markdown (${markdown.length} characters) exceeds the processing limit.`,
            operation: "markdown",
            format: "md",
        }));
    }
    const deadline = immediateDeadline("markdown", "md", timeoutMs, "Markdown conversion deadline exceeded.");
    if (deadline)
        return deadline;
    const clientResult = getWorker("markdown", "md");
    if (!clientResult.ok)
        return clientResult;
    try {
        const parsed = binaryOutputSchema.safeParse(await clientResult.client.markdownToPdf({ markdown, timeoutMs }));
        if (!parsed.success) {
            return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
                code: "invalid_output",
                message: "The PDF engine returned invalid Markdown output.",
                operation: "markdown",
                format: "md",
                detail: zod_1.z.prettifyError(parsed.error),
            }));
        }
        await recordLoadedVersion(clientResult.client);
        return { ok: true, pdf: toArrayBuffer(parsed.data) };
    }
    catch (error) {
        return normalizeFailure(error, {
            operation: "markdown",
            format: "md",
            fallbackMessage: "The PDF engine failed to convert this Markdown file.",
        });
    }
};
exports.markdownToPdf = markdownToPdf;
const redactPdfWithReport = async (pdf, areasJson, timeoutMs) => {
    const source = toUint8(pdf);
    if (source.byteLength > constants_js_1.PDF_PROCESSING_MAX_INPUT_BYTES) {
        return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
            code: "resource_limit",
            message: `PDF (${source.byteLength} bytes) exceeds the ${constants_js_1.PDF_PROCESSING_MAX_INPUT_BYTES}-byte processing limit.`,
            operation: "redaction",
            format: "pdf",
        }));
    }
    const deadline = immediateDeadline("redaction", "pdf", timeoutMs, "PDF redaction deadline exceeded.");
    if (deadline)
        return deadline;
    const clientResult = getWorker("redaction", "pdf");
    if (!clientResult.ok)
        return clientResult;
    try {
        const parsed = redactionReportSchema.safeParse(await clientResult.client.redactPdfWithReport({
            pdf: source,
            areasJson,
            timeoutMs,
        }));
        if (!parsed.success) {
            return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
                code: "invalid_output",
                message: "The PDF engine returned an invalid redaction report.",
                operation: "redaction",
                format: "pdf",
                detail: zod_1.z.prettifyError(parsed.error),
            }));
        }
        await recordLoadedVersion(clientResult.client);
        return {
            ok: true,
            pdf: toArrayBuffer(parsed.data.pdf),
            pageCount: parsed.data.pageCount,
            warnings: parsed.data.warnings,
        };
    }
    catch (error) {
        return normalizeFailure(error, {
            operation: "redaction",
            format: "pdf",
            fallbackMessage: "The PDF engine failed to redact this document.",
        });
    }
};
exports.redactPdfWithReport = redactPdfWithReport;
const elapsedMs = (startedAt) => Number((node_perf_hooks_1.performance.now() - startedAt).toFixed(2));
const probePdfEngine = async (timeoutMs = constants_js_1.ENGINE_BOOT_PROBE_TIMEOUT_MS) => {
    const startedAt = node_perf_hooks_1.performance.now();
    const deadline = immediateDeadline("probe", "pdf", timeoutMs, "PDF engine availability probe deadline exceeded.");
    if (deadline)
        return { ...deadline, bootDurationMs: elapsedMs(startedAt) };
    const clientResult = getWorker("probe", "pdf", timeoutMs);
    if (!clientResult.ok) {
        return { ...clientResult, bootDurationMs: elapsedMs(startedAt) };
    }
    try {
        await recordLoadedVersion(clientResult.client);
        return { ok: true, bootDurationMs: elapsedMs(startedAt) };
    }
    catch (error) {
        return {
            ...normalizeFailure(error, {
                operation: "probe",
                format: "pdf",
                fallbackMessage: PDF_ENGINE_UNAVAILABLE_MESSAGE,
            }),
            bootDurationMs: elapsedMs(startedAt),
        };
    }
};
exports.probePdfEngine = probePdfEngine;
const pageCount = async (pdf, timeoutMs) => {
    const deadline = immediateDeadline("page_count", "pdf", timeoutMs, "PDF page-count deadline exceeded.");
    if (deadline)
        return deadline;
    const clientResult = getWorker("page_count", "pdf");
    if (!clientResult.ok)
        return clientResult;
    try {
        const parsed = zod_1.z
            .number()
            .int()
            .positive()
            .safeParse(await clientResult.client.pageCount({ pdf: toUint8(pdf), timeoutMs }));
        if (!parsed.success) {
            return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
                code: "invalid_output",
                message: "The PDF engine returned an invalid page count.",
                operation: "page_count",
                format: "pdf",
                detail: zod_1.z.prettifyError(parsed.error),
            }));
        }
        await recordLoadedVersion(clientResult.client);
        return { ok: true, pageCount: parsed.data };
    }
    catch (error) {
        return normalizeFailure(error, {
            operation: "page_count",
            format: "pdf",
            fallbackMessage: "The PDF engine failed to count document pages.",
        });
    }
};
exports.pageCount = pageCount;
//# sourceMappingURL=pdf-core.js.map