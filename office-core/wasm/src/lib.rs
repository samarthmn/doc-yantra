//! WebAssembly bindings for office-core (Office → PDF).
//!
//! Mirrors `pdf-core-wasm`'s trap contract: recoverable failures are `JsError`
//! (the caller applies the app's original-file fallback rule); a panic/OOM
//! traps and the caller must discard the instance. `office_profile` NEVER throws for
//! well-formed calls — container failures serialize via `DocProfile::from_error`.

use office_core::{
    ConvertOptions, DocProfile, OfficeError, OfficeFormat, UnsupportedCode, convert, profile,
    register_font as register_core_font,
};
use wasm_bindgen::prelude::*;

/// Defense-in-depth limit after wasm entry. The generated JS package adds the
/// matching pre-copy guard so oversized input never reaches wasm allocation.
const MAX_INPUT_BYTES: usize = 50 * 1024 * 1024;

#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// The engine crate version.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Every recoverable failure crosses the wasm boundary as a machine-readable
/// JSON envelope in the `JsError` message: `{"dy":1,"code":…,"message":…}`.
/// `code` is the discriminant consumers branch on (never message prose);
/// `message` keeps the exact legacy text. Unsupported errors additionally
/// expose a stable `detail.reason` slug and human-readable `detail.context`.
fn envelope_payload(code: &str, message: &str, detail: Option<(&str, &str)>) -> String {
    let mut payload = serde_json::json!({ "dy": 1, "code": code, "message": message });
    if let Some((reason, context)) = detail {
        payload["detail"] = serde_json::json!({
            "reason": reason,
            "context": context,
        });
    }
    payload.to_string()
}

fn envelope(code: &str, message: &str, detail: Option<(&str, &str)>) -> JsError {
    JsError::new(&envelope_payload(code, message, detail))
}

/// Maps the engine's stable reason codes onto the cross-package error
/// taxonomy. Matching enum variants, not message prose.
fn taxonomy_code(code: UnsupportedCode) -> &'static str {
    match code {
        UnsupportedCode::MissingGlyph => "missing_glyph",
        UnsupportedCode::InputTooLarge
        | UnsupportedCode::DecompressionLimit
        | UnsupportedCode::ImageDimensionsExceedLimit
        | UnsupportedCode::ImageAllocationExceedsLimit
        | UnsupportedCode::OutputLimit => "resource_limit",
        UnsupportedCode::Encrypted => "password_protected",
        UnsupportedCode::InvalidFile
        | UnsupportedCode::ImageDecodeFailure
        | UnsupportedCode::ImageContentTypeMissing => "malformed_container",
        UnsupportedCode::LegacyFormat | UnsupportedCode::FormatNotImplemented => {
            "unsupported_format"
        }
        _ => "unsupported_feature",
    }
}

fn office_err(error: &OfficeError) -> JsError {
    match error {
        OfficeError::Unsupported { code, detail } => {
            let slug = code_slug(*code);
            envelope(
                taxonomy_code(*code),
                &format!("unsupported:{slug}: {detail}"),
                Some((&slug, detail)),
            )
        }
        OfficeError::Invalid(m) => envelope("malformed_container", &format!("invalid: {m}"), None),
        other => envelope("internal_error", &format!("internal: {other}"), None),
    }
}

fn internal_err(e: impl std::fmt::Display) -> JsError {
    envelope("internal_error", &e.to_string(), None)
}

fn format_for(extension: &str) -> Option<OfficeFormat> {
    OfficeFormat::from_extension(&extension.to_ascii_lowercase())
}

/// The DocProfile JSON for a document. Never throws for well-formed calls;
/// container-level failures serialize via `DocProfile::from_error`. Oversized
/// input returns the `input_too_large` profile. The generated JS package
/// performs this check before copying the input into wasm memory.
#[wasm_bindgen]
pub fn office_profile(bytes: &[u8], extension: &str) -> Result<String, JsError> {
    let Some(format) = format_for(extension) else {
        return Err(envelope(
            "invalid_input",
            &format!("invalid: unsupported extension {extension}"),
            None,
        ));
    };
    if bytes.len() > MAX_INPUT_BYTES {
        let e = OfficeError::unsupported(
            UnsupportedCode::InputTooLarge,
            format!("{} bytes exceeds the in-process limit", bytes.len()),
        );
        return serde_json::to_string(&DocProfile::from_error(format, &e)).map_err(internal_err);
    }
    let prof = match profile(bytes, format) {
        Ok(p) => p,
        Err(e) => DocProfile::from_error(format, &e),
    };
    serde_json::to_string(&prof).map_err(internal_err)
}

/// PDF bytes for a document. Throws a `JsError` whose message begins with
/// `"unsupported:<code>: "` for envelope misses (DocKosha parses this prefix),
/// `"invalid: …"` / `"internal: …"` otherwise. Same 50 MiB pre-copy guard.
#[wasm_bindgen]
pub fn office_to_pdf(bytes: &[u8], extension: &str) -> Result<Vec<u8>, JsError> {
    let Some(format) = format_for(extension) else {
        return Err(envelope(
            "invalid_input",
            &format!("invalid: unsupported extension {extension}"),
            None,
        ));
    };
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(envelope(
            "resource_limit",
            &format!(
                "invalid: input ({} bytes) exceeds the in-process limit",
                bytes.len()
            ),
            None,
        ));
    }
    let opts = ConvertOptions::default();
    let pdf = convert(bytes, format, &opts)
        .map(|out| out.pdf)
        .map_err(|e| office_err(&e))?;
    office_core::output_limit::ensure_output_within_limit(
        pdf.len(),
        office_core::output_limit::MAX_OUTPUT_BYTES,
    )
    .map_err(|e| office_err(&e))?;
    Ok(pdf)
}

/// Adds a font to the process-global font database (brand fonts). Errors on
/// unparseable font bytes.
#[wasm_bindgen]
pub fn register_font(bytes: &[u8]) -> Result<(), JsError> {
    register_core_font(bytes).map_err(|e| office_err(&e))
}

/// The stable `snake_case` slug for an unsupported code (matches profile reasons).
fn code_slug(code: UnsupportedCode) -> String {
    serde_json::to_string(&code)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_envelope_carries_reason_and_context_in_detail() {
        let payload = envelope_payload(
            "unsupported_feature",
            "unsupported:formula_cache_missing: sheet Sheet1: cell A1",
            Some((
                "formula_cache_missing",
                "sheet Sheet1: cell A1 has no usable formula cache",
            )),
        );
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

        assert_eq!(value["code"], "unsupported_feature");
        assert_eq!(value["detail"]["reason"], "formula_cache_missing");
        assert_eq!(
            value["detail"]["context"],
            "sheet Sheet1: cell A1 has no usable formula cache"
        );
    }
}
