import { z } from "zod";
import { type ProviderFailure, type ProviderOperation } from "./provider-interface.js";
export declare const engineErrorCodeSchema: z.ZodEnum<{
    invalid_input: "invalid_input";
    unsupported_format: "unsupported_format";
    unsupported_feature: "unsupported_feature";
    missing_glyph: "missing_glyph";
    missing_asset: "missing_asset";
    password_protected: "password_protected";
    malformed_container: "malformed_container";
    resource_limit: "resource_limit";
    invalid_output: "invalid_output";
    internal_error: "internal_error";
    deadline_exceeded: "deadline_exceeded";
    queue_busy: "queue_busy";
    worker_boot_failed: "worker_boot_failed";
    engine_unavailable: "engine_unavailable";
    trap: "trap";
    protocol_error: "protocol_error";
}>;
export type EngineErrorCode = z.infer<typeof engineErrorCodeSchema>;
export type EngineOperation = ProviderOperation;
type FailureInput = Omit<ProviderFailure, "retryable">;
export declare const createEngineFailure: (input: FailureInput) => ProviderFailure;
export declare const normalizeEngineError: (error: unknown, context: {
    operation: EngineOperation;
    format?: string;
    fallbackMessage: string;
}) => ProviderFailure;
export declare const toFailureResult: (failure: ProviderFailure) => ProviderFailure & {
    ok: false;
};
export {};
//# sourceMappingURL=engine-errors.d.ts.map