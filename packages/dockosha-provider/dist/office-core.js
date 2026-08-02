"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.officeToPdf = void 0;
const node_module_1 = require("node:module");
const node_perf_hooks_1 = require("node:perf_hooks");
const zod_1 = require("zod");
const constants_js_1 = require("./constants.js");
const engine_errors_js_1 = require("./engine-errors.js");
const office_font_provider_js_1 = require("./office-font-provider.js");
const isRecord = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const isOfficeWorkerModule = (value) => isRecord(value) && typeof value.createOfficeEngineWorker === "function";
const isOfficePackageModule = (value) => isRecord(value) &&
    isRecord(value.DEFAULT_FONTSOURCE_CATALOG) &&
    typeof value.FontsourceProvider === "function" &&
    typeof value.MemoryFontCache === "function";
// Every metric-compatible family is version-pinned: "latest" let Fontsource
// ship an Arimo whose Windows metrics no longer matched Arial (2136/797 vs
// 1854/434, measured 2026-08-02), silently inflating line boxes. The pins
// are the fidelity-harness fonts.lock versions whose metrics are verified.
// (Arial-class requests are additionally served by packaged Liberation Sans —
// see office-font-provider.ts — so Arimo remains only for literal requests.)
const FONTSOURCE_PACKAGE_VERSION_PINS = {
    arimo: "5.2.8",
    caladea: "5.2.8",
    carlito: "5.2.8",
    cousine: "5.2.7",
    gelasio: "5.2.8",
    "noto-sans-math": "5.2.9",
    "noto-sans-symbols": "5.2.11",
    "noto-sans-symbols-2": "5.2.8",
    tinos: "5.2.7",
    ubuntu: "5.2.8",
    "ubuntu-mono": "5.2.8",
};
const pinFontsourceCatalog = (catalog) => {
    const pinned = { ...catalog };
    for (const [id, packageVersion] of Object.entries(FONTSOURCE_PACKAGE_VERSION_PINS)) {
        const entry = catalog[id];
        if (!entry)
            throw new Error(`DocYantra Fontsource catalog is missing ${id}`);
        pinned[id] = { ...entry, packageVersion };
    }
    return pinned;
};
const requireCjs = (0, node_module_1.createRequire)(__filename);
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
    const workerModule = requireCjs("@samarthmn/doc-yantra-office/worker");
    const officePackage = requireCjs("@samarthmn/doc-yantra-office");
    if (!isOfficeWorkerModule(workerModule)) {
        throw new Error("Office worker entrypoint has an invalid export contract");
    }
    if (!isOfficePackageModule(officePackage)) {
        throw new Error("Office package has an invalid font-provider contract");
    }
    const fontProvider = (0, office_font_provider_js_1.createPackagedOfficeFontProvider)(new officePackage.FontsourceProvider({
        catalog: pinFontsourceCatalog(officePackage.DEFAULT_FONTSOURCE_CATALOG),
    }));
    const fontMemoryCache = new officePackage.MemoryFontCache();
    createWorker = (defaultTimeoutMs) => workerModule.createOfficeEngineWorker({
        ...(defaultTimeoutMs === undefined ? {} : { defaultTimeoutMs }),
        fontProvider,
        fontMemoryCache,
        fontMode: "compatible",
    });
}
catch (error) {
    startupFailure = (0, engine_errors_js_1.createEngineFailure)({
        code: "worker_boot_failed",
        message: "The Office engine is temporarily unavailable.",
        operation: "probe",
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
const contextualFailure = (failure, format) => (0, engine_errors_js_1.createEngineFailure)({
    code: failure.code,
    message: failure.message,
    operation: "office_conversion",
    format,
    detail: failure.detail,
});
const getWorker = (format, defaultTimeoutMs) => {
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
                message: "The Office engine is temporarily unavailable.",
                operation: "office_conversion",
                format,
            }), format));
    }
    try {
        worker = createWorker(defaultTimeoutMs);
        return { ok: true, client: worker };
    }
    catch (error) {
        return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
            code: "worker_boot_failed",
            message: "The Office engine is temporarily unavailable.",
            operation: "office_conversion",
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
    const version = zod_1.z
        .string()
        .min(1)
        .parse(await client.engineVersion());
    hasReportedVersion = true;
    console.info(`[office-core-wasm] worker loaded (version ${version})`);
    scheduleTestIdleClose();
};
const normalizeFailure = (error, format, fallbackMessage) => {
    scheduleTestIdleClose();
    return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.normalizeEngineError)(error, {
        operation: "office_conversion",
        format,
        fallbackMessage,
    }));
};
const immediateDeadline = (timeoutMs, format) => {
    if (!Number.isFinite(timeoutMs)) {
        return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
            code: "invalid_input",
            message: "The Office engine timeout must be a finite number.",
            operation: "office_conversion",
            format,
        }));
    }
    if (timeoutMs > 0)
        return null;
    return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
        code: "deadline_exceeded",
        message: "Office conversion deadline exceeded.",
        operation: "office_conversion",
        format,
    }));
};
const toArrayBuffer = (bytes) => {
    const buffer = new ArrayBuffer(bytes.byteLength);
    new Uint8Array(buffer).set(bytes);
    return buffer;
};
const toUint8 = (bytes) => bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
const officeOnDemandOutputSchema = zod_1.z.object({
    pdf: zod_1.z.instanceof(Uint8Array),
});
const officeToPdf = async (bytes, extension, timeoutMs = constants_js_1.DOCUMENT_CONVERSION_TIMEOUT_MS) => {
    const format = extension.trim().toLowerCase();
    if (bytes.byteLength > constants_js_1.OFFICE_ENGINE_MAX_INPUT_BYTES) {
        return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
            code: "resource_limit",
            message: `Office input (${bytes.byteLength} bytes) exceeds the in-process limit.`,
            operation: "office_conversion",
            format,
        }));
    }
    const deadline = immediateDeadline(timeoutMs, format);
    if (deadline)
        return deadline;
    const clientResult = getWorker(format);
    if (!clientResult.ok)
        return clientResult;
    try {
        const startedAt = node_perf_hooks_1.performance.now();
        const parsed = officeOnDemandOutputSchema.safeParse(await clientResult.client.convertOnDemand({
            bytes: toUint8(bytes),
            extension: format,
            mode: "compatible",
            timeoutMs,
        }));
        const ms = node_perf_hooks_1.performance.now() - startedAt;
        if (!parsed.success) {
            return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
                code: "invalid_output",
                message: "The Office engine returned invalid output.",
                operation: "office_conversion",
                format,
                detail: zod_1.z.prettifyError(parsed.error),
            }));
        }
        const output = parsed.data.pdf;
        if (output.length < 5 ||
            String.fromCharCode(...output.subarray(0, 5)) !== "%PDF-") {
            return (0, engine_errors_js_1.toFailureResult)((0, engine_errors_js_1.createEngineFailure)({
                code: "invalid_output",
                message: "The Office engine returned non-PDF output.",
                operation: "office_conversion",
                format,
            }));
        }
        await recordLoadedVersion(clientResult.client);
        return { ok: true, pdf: toArrayBuffer(output), ms };
    }
    catch (error) {
        return normalizeFailure(error, format, "The Office engine failed to convert this document.");
    }
};
exports.officeToPdf = officeToPdf;
//# sourceMappingURL=office-core.js.map