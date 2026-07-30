"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.toFailureResult = exports.normalizeEngineError = exports.createEngineFailure = exports.engineErrorCodeSchema = void 0;
const zod_1 = require("zod");
const provider_interface_js_1 = require("./provider-interface.js");
exports.engineErrorCodeSchema = zod_1.z.enum(provider_interface_js_1.PROVIDER_ERROR_CODES);
const engineOperationSchema = zod_1.z.enum(provider_interface_js_1.PROVIDER_OPERATIONS);
const RETRYABLE_CODES = new Set([
    "deadline_exceeded",
    "queue_busy",
    "worker_boot_failed",
    "engine_unavailable",
    "trap",
]);
const DETAIL_MAX_DEPTH = 5;
const DETAIL_MAX_ITEMS = 32;
const DETAIL_MAX_KEY_LENGTH = 128;
const DETAIL_MAX_STRING_LENGTH = 2_048;
const boundedString = (value) => {
    const trimmed = value.trim();
    return trimmed ? trimmed.slice(0, DETAIL_MAX_STRING_LENGTH) : undefined;
};
const boundedContext = (value, depth, ancestors) => {
    if (value === null || typeof value === "boolean")
        return value;
    if (typeof value === "number")
        return Number.isFinite(value) ? value : undefined;
    if (typeof value === "string")
        return boundedString(value);
    if (depth >= DETAIL_MAX_DEPTH || typeof value !== "object")
        return undefined;
    if (ancestors.has(value))
        return undefined;
    const nextAncestors = new Set(ancestors);
    nextAncestors.add(value);
    if (Array.isArray(value)) {
        return value.slice(0, DETAIL_MAX_ITEMS).flatMap((item) => {
            const bounded = boundedContext(item, depth + 1, nextAncestors);
            return bounded === undefined ? [] : [bounded];
        });
    }
    const result = {};
    for (const [rawKey, item] of Object.entries(value).slice(0, DETAIL_MAX_ITEMS)) {
        const key = rawKey.slice(0, DETAIL_MAX_KEY_LENGTH);
        if (!key)
            continue;
        const bounded = boundedContext(item, depth + 1, nextAncestors);
        if (bounded !== undefined)
            result[key] = bounded;
    }
    return result;
};
const normalizeDetail = (value) => {
    if (typeof value === "string")
        return boundedString(value);
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
        return undefined;
    }
    const bounded = boundedContext(value, 0, new Set());
    if (!bounded || Array.isArray(bounded) || typeof bounded !== "object") {
        return undefined;
    }
    return bounded;
};
const failureSchema = zod_1.z
    .object({
    code: exports.engineErrorCodeSchema,
    message: zod_1.z.string().min(1).max(4_096),
    operation: engineOperationSchema,
    format: zod_1.z.string().trim().min(1).max(64).optional(),
    detail: zod_1.z.unknown().optional(),
})
    .transform((input) => {
    const detail = normalizeDetail(input.detail);
    return {
        code: input.code,
        message: input.message,
        operation: input.operation,
        ...(input.format ? { format: input.format } : {}),
        retryable: RETRYABLE_CODES.has(input.code),
        ...(detail ? { detail } : {}),
    };
});
const createEngineFailure = (input) => failureSchema.parse(input);
exports.createEngineFailure = createEngineFailure;
const workerErrorSchema = zod_1.z
    .object({
    code: exports.engineErrorCodeSchema,
    message: zod_1.z.string().min(1),
    detail: zod_1.z.unknown().optional(),
    engineMessage: zod_1.z.string().min(1).optional(),
})
    .loose();
const normalizeEngineError = (error, context) => {
    const parsed = workerErrorSchema.safeParse(error);
    if (parsed.success) {
        return (0, exports.createEngineFailure)({
            code: parsed.data.code,
            message: parsed.data.message,
            operation: context.operation,
            format: context.format,
            detail: normalizeDetail(parsed.data.detail ?? parsed.data.engineMessage),
        });
    }
    return (0, exports.createEngineFailure)({
        code: "internal_error",
        message: context.fallbackMessage,
        operation: context.operation,
        format: context.format,
        detail: error instanceof Error
            ? error.message
            : typeof error === "string"
                ? error
                : undefined,
    });
};
exports.normalizeEngineError = normalizeEngineError;
const toFailureResult = (failure) => ({ ok: false, ...failure });
exports.toFailureResult = toFailureResult;
//# sourceMappingURL=engine-errors.js.map