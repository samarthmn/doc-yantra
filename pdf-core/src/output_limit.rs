//! Engine-side cap on serialized output size, enforced before result bytes
//! cross the wasm boundary (the JS worker layer enforces the same cap again
//! before transferring results to the main thread).

use crate::error::PdfCoreError;

/// Hard cap on any single serialized engine output. Mirrors the JS worker
/// layer's output cap so a violation is refused at the earliest layer that
/// can observe it.
pub const MAX_OUTPUT_BYTES: usize = 512 * 1024 * 1024;

/// Fails with [`PdfCoreError::ResourceLimit`] when a produced output exceeds
/// `limit` bytes. The input was valid, so this is a resource refusal, never
/// `invalid_input`.
pub fn ensure_output_within_limit(len: usize, limit: usize) -> Result<(), PdfCoreError> {
    if len > limit {
        return Err(PdfCoreError::ResourceLimit(format!(
            "engine output of {len} bytes exceeds the {limit} byte output limit"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_at_limit_is_allowed() {
        assert!(ensure_output_within_limit(8, 8).is_ok());
        assert!(ensure_output_within_limit(0, 0).is_ok());
    }

    #[test]
    fn output_over_limit_is_a_resource_limit() {
        let error = ensure_output_within_limit(9, 8).expect_err("must refuse");
        assert!(matches!(error, PdfCoreError::ResourceLimit(_)));
        assert_eq!(error.stable_code(), "resource_limit");
    }

    #[test]
    fn default_cap_matches_the_js_worker_layer() {
        assert_eq!(MAX_OUTPUT_BYTES, 512 * 1024 * 1024);
    }
}
