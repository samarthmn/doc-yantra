"use strict";
/**
 * Vendored from DocKosha packages/provider-interface/src/index.ts.
 *
 * DocKosha is the source of truth while the interface package is local-only.
 * Before publishing this adapter, replace this vendored file with the
 * published @dockosha/provider-interface dependency and import its types.
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.PROVIDER_OPERATIONS = exports.PROVIDER_ERROR_CODES = void 0;
exports.PROVIDER_ERROR_CODES = [
    "invalid_input",
    "unsupported_format",
    "unsupported_feature",
    "missing_glyph",
    "missing_asset",
    "password_protected",
    "malformed_container",
    "resource_limit",
    "invalid_output",
    "internal_error",
    "deadline_exceeded",
    "queue_busy",
    "worker_boot_failed",
    "engine_unavailable",
    "trap",
    "protocol_error",
];
exports.PROVIDER_OPERATIONS = [
    "merge",
    "watermark",
    "csv",
    "markdown",
    "redaction",
    "page_count",
    "conversion",
    "office_conversion",
    "probe",
];
//# sourceMappingURL=provider-interface.js.map