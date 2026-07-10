//! WebAssembly bindings for office-core (Office → PDF).
//!
//! Mirrors `pdf-core-wasm`'s trap contract: recoverable failures are `JsError`
//! (the caller can fall back to the LibreOffice service); a panic/OOM traps and
//! the caller must discard the instance. `office_profile` NEVER throws for
//! well-formed calls — container failures serialize via `DocProfile::from_error`.

use office_core::fonts::FontDb;
use office_core::{
    ConvertOptions, DocProfile, OfficeError, OfficeFormat, UnsupportedCode, convert, profile,
};
use std::sync::{Mutex, OnceLock};
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

/// Brand fonts registered via [`register_font`], applied to every conversion.
fn registered_fonts() -> &'static Mutex<Vec<Vec<u8>>> {
    static FONTS: OnceLock<Mutex<Vec<Vec<u8>>>> = OnceLock::new();
    FONTS.get_or_init(|| Mutex::new(Vec::new()))
}

fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
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
        return Err(JsError::new(&format!("invalid: unsupported extension {extension}")));
    };
    if bytes.len() > MAX_INPUT_BYTES {
        let e = OfficeError::unsupported(
            UnsupportedCode::InputTooLarge,
            format!("{} bytes exceeds the in-process limit", bytes.len()),
        );
        return serde_json::to_string(&DocProfile::from_error(format, &e)).map_err(err);
    }
    let prof = match profile(bytes, format) {
        Ok(p) => p,
        Err(e) => DocProfile::from_error(format, &e),
    };
    serde_json::to_string(&prof).map_err(err)
}

/// PDF bytes for a document. Throws a `JsError` whose message begins with
/// `"unsupported:<code>: "` for envelope misses (DocKosha parses this prefix),
/// `"invalid: …"` / `"internal: …"` otherwise. Same 50 MiB pre-copy guard.
#[wasm_bindgen]
pub fn office_to_pdf(bytes: &[u8], extension: &str) -> Result<Vec<u8>, JsError> {
    let Some(format) = format_for(extension) else {
        return Err(JsError::new(&format!("invalid: unsupported extension {extension}")));
    };
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(JsError::new(&format!(
            "invalid: input ({} bytes) exceeds the in-process limit",
            bytes.len()
        )));
    }
    let font_bytes = registered_fonts().lock().map(|g| g.clone()).unwrap_or_default();
    let opts = ConvertOptions {
        font_bytes,
        ..Default::default()
    };
    match convert(bytes, format, &opts) {
        Ok(out) => Ok(out.pdf),
        Err(OfficeError::Unsupported { code, detail }) => Err(JsError::new(&format!(
            "unsupported:{}: {detail}",
            code_slug(code)
        ))),
        Err(OfficeError::Invalid(m)) => Err(JsError::new(&format!("invalid: {m}"))),
        Err(e) => Err(JsError::new(&format!("internal: {e}"))),
    }
}

/// Adds a font to the process-global font database (brand fonts). Errors on
/// unparseable font bytes.
#[wasm_bindgen]
pub fn register_font(bytes: &[u8]) -> Result<(), JsError> {
    // Validate by parsing into a throwaway db before storing.
    let mut db = FontDb::new();
    db.register(bytes.to_vec()).map_err(err)?;
    registered_fonts()
        .lock()
        .map_err(|_| JsError::new("internal: font registry poisoned"))?
        .push(bytes.to_vec());
    Ok(())
}

/// The stable `snake_case` slug for an unsupported code (matches profile reasons).
fn code_slug(code: UnsupportedCode) -> String {
    serde_json::to_string(&code)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}
