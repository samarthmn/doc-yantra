//! Engine-side cap on serialized output size, enforced before result bytes
//! cross the wasm boundary (the JS worker layer enforces the same cap again
//! before transferring results to the main thread).

use crate::error::{OfficeError, UnsupportedCode};

/// Hard cap on any single serialized engine output. Mirrors the JS worker
/// layer's output cap so a violation is refused at the earliest layer that
/// can observe it.
pub const MAX_OUTPUT_BYTES: usize = 512 * 1024 * 1024;

/// Fails with [`UnsupportedCode::OutputLimit`] when a produced output exceeds
/// `limit` bytes. The input was valid, so this is a resource refusal, never
/// file corruption.
pub fn ensure_output_within_limit(len: usize, limit: usize) -> Result<(), OfficeError> {
    if len > limit {
        return Err(OfficeError::unsupported(
            UnsupportedCode::OutputLimit,
            format!("engine output of {len} bytes exceeds the {limit} byte output limit"),
        ));
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
    fn output_over_limit_is_an_output_limit_refusal() {
        let error = ensure_output_within_limit(9, 8).expect_err("must refuse");
        match error {
            OfficeError::Unsupported { code, .. } => {
                assert_eq!(code, UnsupportedCode::OutputLimit);
            }
            other => panic!("expected Unsupported(OutputLimit), got {other:?}"),
        }
    }

    #[test]
    fn default_cap_matches_the_js_worker_layer() {
        assert_eq!(MAX_OUTPUT_BYTES, 512 * 1024 * 1024);
    }
}
