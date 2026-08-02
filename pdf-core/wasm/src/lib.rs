//! WebAssembly bindings for pdf-core Zone 1 (assembly, watermark, generate,
//! meta). The watermark entry points accept the SAME JSON payload DocKosha
//! sends to DocYantra's `/docyantra/pdf/merge` endpoint — the parser below is
//! a faithful port of `parseWatermark` in
//! `doc-yantra/src/modules/pdf/pdf.service.ts` — so callers can switch
//! between the HTTP service and this module without reshaping data.

use base64::Engine;
use js_sys::{Array, Object, Reflect, Uint8Array};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

use pdf_core::watermark::{Pattern, WatermarkImage, WatermarkSpec};
use pdf_core::{PdfCoreError, redact_types::RedactArea};

const DEFAULT_COLOR: [u8; 3] = [0x4B, 0x55, 0x63];
const DEFAULT_ROTATION_DEG: f64 = -40.0;

/// Every recoverable failure crosses the wasm boundary as a machine-readable
/// JSON envelope in the `JsError` message: `{"dy":1,"code":…,"message":…}`.
/// `code` is the discriminant consumers branch on (never message prose);
/// `message` keeps the exact legacy text so message-matching callers stay
/// stable; `detail` carries the crate-internal sub-code when one exists.
fn envelope(code: &str, message: &str, detail: Option<&str>) -> JsError {
    let mut payload = serde_json::json!({ "dy": 1, "code": code, "message": message });
    if let Some(detail) = detail {
        payload["detail"] = serde_json::Value::from(detail);
    }
    JsError::new(&payload.to_string())
}

fn core_err(error: PdfCoreError) -> JsError {
    envelope(
        error.stable_code(),
        &error.to_string(),
        error.machine_detail(),
    )
}

fn invalid(message: &str) -> JsError {
    envelope("invalid_input", message, None)
}

fn internal_err(e: impl std::fmt::Display) -> JsError {
    envelope("internal_error", &e.to_string(), None)
}

fn redact_err(error: PdfCoreError) -> JsError {
    let code = error.stable_code();
    let detail = error.machine_detail();
    // Preserve the historical redaction message shapes exactly.
    let message = match &error {
        PdfCoreError::Unsupported(message) => format!("unsupported:{message}"),
        _ => error.to_string(),
    };
    envelope(code, &message, detail)
}

/// Hard ceilings on inputs handed to the wasm engine (Y-6). wasm32 linear
/// memory is capped and practical OOM happens well before that; a merge
/// additionally holds every input plus the merged copy at once. Oversized
/// inputs are rejected here as a NORMAL `JsError` (never an uncatchable trap)
/// instead of risking an OOM-abort that poisons the instance — see the
/// panic/trap contract in CONSUMING.md. These are backstops for direct
/// consumers; operations which still have a service implementation may use it
/// as their fallback, while redaction must be retried with a smaller input.
const MAX_INPUT_BYTES: usize = 100 * 1024 * 1024; // 100 MiB per document
const MAX_MERGE_TOTAL_BYTES: usize = 200 * 1024 * 1024; // 200 MiB per merge set
const MAX_FONT_BYTES: usize = 32 * 1024 * 1024; // 32 MiB per registered face
const MAX_REDACTION_AREAS: usize = 800;

fn guard_input_size(len: usize, limit: usize, what: &str) -> Result<(), JsError> {
    if len > limit {
        return Err(envelope(
            "resource_limit",
            &format!(
                "{what} is {len} bytes, over the {} MiB in-process limit",
                limit / (1024 * 1024)
            ),
            None,
        ));
    }
    Ok(())
}

/// Refuses a serialized result that exceeds the engine output budget before
/// the bytes cross the wasm boundary (the worker layer re-checks the same cap
/// before transferring to the main thread).
fn guard_output(bytes: Vec<u8>) -> Result<Vec<u8>, JsError> {
    pdf_core::output_limit::ensure_output_within_limit(
        bytes.len(),
        pdf_core::output_limit::MAX_OUTPUT_BYTES,
    )
    .map_err(core_err)?;
    Ok(bytes)
}

#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

/// Engine version, for logging/diagnostics on the JS side.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// ---------------------------------------------------------------------------
// DocYantra watermark payload (parseWatermark port)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct WatermarkPayload {
    pattern: Option<String>,
    rotation_deg: Option<f64>,
    x_spacing_pt: Option<f64>,
    y_spacing_pt: Option<f64>,
    offset_x_pt: Option<f64>,
    offset_y_pt: Option<f64>,
    lines: Option<Vec<serde_json::Value>>,
    color: Option<String>,
    font_size_pt: Option<f64>,
    opacity: Option<f64>,
    content_type: Option<String>,
    image: Option<ImagePayload>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct ImagePayload {
    data_url: Option<String>,
    width_pt: Option<f64>,
    height_pt: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RedactAreaPayload {
    page_index: u32,
    left: f64,
    top: f64,
    width: f64,
    height: f64,
}

impl From<RedactAreaPayload> for RedactArea {
    fn from(area: RedactAreaPayload) -> Self {
        Self {
            page_index: area.page_index,
            left: area.left,
            top: area.top,
            width: area.width,
            height: area.height,
        }
    }
}

/// Ports DocYantra's `parseHexColor`: 3- or 6-digit hex (with or without
/// leading '#'); anything else falls back to the default gray #4B5563.
fn parse_hex_color(value: Option<&str>) -> [u8; 3] {
    let raw = value.unwrap_or("").trim();
    if raw.is_empty() {
        return DEFAULT_COLOR;
    }
    let hex = raw.strip_prefix('#').unwrap_or(raw);
    let expanded: String = match hex.len() {
        3 if hex.chars().all(|c| c.is_ascii_hexdigit()) => {
            hex.chars().flat_map(|c| [c, c]).collect()
        }
        6 if hex.chars().all(|c| c.is_ascii_hexdigit()) => hex.to_string(),
        _ => return DEFAULT_COLOR,
    };
    let n = u32::from_str_radix(&expanded, 16).unwrap_or(0x4B5563);
    [(n >> 16) as u8, (n >> 8) as u8, n as u8]
}

fn parse_data_url(data_url: &str) -> Option<Vec<u8>> {
    // Mirrors: /^data:image\/[a-zA-Z+]+;base64,(.+)$/
    let rest = data_url.strip_prefix("data:image/")?;
    let (subtype, payload) = rest.split_once(";base64,")?;
    if subtype.is_empty() || !subtype.chars().all(|c| c.is_ascii_alphabetic() || c == '+') {
        return None;
    }
    if payload.is_empty() {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()
}

/// Builds a [WatermarkSpec] from the DocYantra payload JSON plus optional
/// out-of-band image bytes (the multipart `watermarkImage` equivalent, which
/// takes precedence over `image.dataUrl` exactly like DocYantra).
fn spec_from_payload(
    watermark_json: &str,
    watermark_image: Option<Vec<u8>>,
) -> Result<WatermarkSpec, String> {
    let payload: WatermarkPayload = serde_json::from_str(watermark_json)
        .map_err(|_| "Invalid watermark payload (must be JSON)".to_string())?;

    let lines: Vec<String> = payload
        .lines
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect();

    let pattern_raw = payload.pattern.as_deref().unwrap_or("single");
    let pattern = if pattern_raw == "grid" || pattern_raw == "diagonal_grid" {
        Pattern::Grid
    } else {
        Pattern::Single
    };
    let rotation_deg = payload
        .rotation_deg
        .unwrap_or(if pattern_raw == "diagonal_grid" {
            DEFAULT_ROTATION_DEG
        } else {
            0.0
        });

    let kind = match payload.content_type.as_deref() {
        Some("image") => "image",
        Some("hybrid") => "hybrid",
        _ => "text",
    };

    let (image_bytes, image_w, image_h) = {
        let (w, h) = payload
            .image
            .as_ref()
            .map(|i| (i.width_pt, i.height_pt))
            .unwrap_or((None, None));
        let bytes = match watermark_image {
            Some(bytes) if !bytes.is_empty() => Some(bytes),
            _ => payload
                .image
                .as_ref()
                .and_then(|i| i.data_url.as_deref())
                .and_then(parse_data_url),
        };
        (bytes, w, h)
    };

    if kind != "text" && image_bytes.is_none() {
        return Err("Image watermark type requires watermarkImage or image.dataUrl".to_string());
    }

    // parseWatermark's effectiveLines: image-only keeps lines empty; any
    // other kind falls back to CONFIDENTIAL when no lines are present.
    let effective_lines: Vec<String> = if kind == "image" {
        Vec::new()
    } else if lines.is_empty() {
        vec!["CONFIDENTIAL".to_string()]
    } else {
        lines
    };

    // pdf-core derives text/image/hybrid from (image, lines) presence, and
    // DocYantra never draws text for contentType=image — so image-only specs
    // pass empty lines, matching the TS drawAt behavior.
    let image = if kind == "text" {
        None
    } else {
        image_bytes.map(|bytes| WatermarkImage {
            bytes,
            // Absent payload values become NaN so pdf-core's clamp falls
            // back to DocYantra's 180pt default.
            width_pt: image_w.unwrap_or(f64::NAN),
            height_pt: image_h.unwrap_or(f64::NAN),
        })
    };

    Ok(WatermarkSpec {
        pattern,
        rotation_deg: if rotation_deg.is_finite() {
            rotation_deg
        } else {
            0.0
        },
        // Absent spacing = auto (page-proportional Letter-calibrated grid);
        // explicit payload values keep the historical fixed lattice.
        x_spacing_pt: payload.x_spacing_pt.unwrap_or(f64::NAN),
        y_spacing_pt: payload.y_spacing_pt.unwrap_or(f64::NAN),
        offset_x_pt: payload.offset_x_pt.unwrap_or(0.0),
        offset_y_pt: payload.offset_y_pt.unwrap_or(0.0),
        lines: effective_lines,
        color: parse_hex_color(payload.color.as_deref()),
        font_size_pt: payload.font_size_pt.unwrap_or(14.0),
        opacity: payload.opacity.unwrap_or(0.65),
        image,
    })
}

// ---------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------

/// Adds an embeddable font to the process-global registry. The generated JS
/// glue applies the same limit before copying `bytes` into wasm linear memory;
/// this Rust check is defense in depth for direct wasm callers.
#[wasm_bindgen]
pub fn register_font(bytes: &[u8]) -> Result<(), JsError> {
    if bytes.len() > MAX_FONT_BYTES {
        return Err(envelope(
            "resource_limit",
            &format!(
                "font is {} bytes, over the 32 MiB in-process limit",
                bytes.len()
            ),
            None,
        ));
    }
    pdf_core::embed_font::register_font(bytes).map_err(core_err)
}

/// Merges one or more PDFs and optionally applies a watermark — the local
/// equivalent of DocYantra's `POST /docyantra/pdf/merge`. `watermark_json`
/// is the same JSON string the app puts in the multipart `watermark` field;
/// `watermark_image` is the optional `watermarkImage` file's bytes.
#[wasm_bindgen]
pub fn merge_pdfs(
    pdfs: Vec<js_sys::Uint8Array>,
    watermark_json: Option<String>,
    watermark_image: Option<Vec<u8>>,
) -> Result<Vec<u8>, JsError> {
    if pdfs.is_empty() {
        return Err(invalid("At least one PDF is required"));
    }
    // Size-check BEFORE copying into linear memory (Y-6).
    let total: usize = pdfs.iter().map(|p| p.length() as usize).sum();
    guard_input_size(total, MAX_MERGE_TOTAL_BYTES, "merge input set")?;
    for (index, p) in pdfs.iter().enumerate() {
        guard_input_size(
            p.length() as usize,
            MAX_INPUT_BYTES,
            &format!("PDF {}", index + 1),
        )?;
    }
    if let Some(bytes) = watermark_image.as_ref() {
        guard_input_size(bytes.len(), MAX_INPUT_BYTES, "watermark image")?;
    }
    let buffers: Vec<Vec<u8>> = pdfs.iter().map(|p| p.to_vec()).collect();
    for (index, bytes) in buffers.iter().enumerate() {
        let pages = pdf_core::meta::page_count(bytes).map_err(|e| {
            envelope(
                e.stable_code(),
                &format!("PDF {} is not valid: {e}", index + 1),
                e.machine_detail(),
            )
        })?;
        if pages == 0 {
            return Err(invalid(&format!("PDF {} is empty", index + 1)));
        }
    }

    let merged: Vec<u8> = if buffers.len() == 1 {
        // Single input passes through untouched (DocYantra parity); the
        // watermark step below still applies when a payload is present.
        buffers.into_iter().next().unwrap_or_default()
    } else {
        let refs: Vec<&[u8]> = buffers.iter().map(Vec::as_slice).collect();
        pdf_core::assembly::merge(&refs).map_err(core_err)?
    };

    match watermark_json {
        Some(json) if !json.trim().is_empty() => {
            let spec = spec_from_payload(&json, watermark_image).map_err(|e| invalid(&e))?;
            pdf_core::watermark::apply_watermark(&merged, &spec)
                .map_err(core_err)
                .and_then(guard_output)
        }
        _ => guard_output(merged),
    }
}

/// Applies a DocYantra-payload watermark to a single PDF.
#[wasm_bindgen]
pub fn apply_watermark(
    pdf: &[u8],
    watermark_json: &str,
    watermark_image: Option<Vec<u8>>,
) -> Result<Vec<u8>, JsError> {
    guard_input_size(pdf.len(), MAX_INPUT_BYTES, "PDF")?;
    if let Some(bytes) = watermark_image.as_ref() {
        guard_input_size(bytes.len(), MAX_INPUT_BYTES, "watermark image")?;
    }
    let spec = spec_from_payload(watermark_json, watermark_image).map_err(|e| invalid(&e))?;
    pdf_core::watermark::apply_watermark(pdf, &spec)
        .map_err(core_err)
        .and_then(guard_output)
}

/// Permanently redacts service-compatible percentage areas from a PDF.
///
/// `areas_json` must be an array of
/// `{pageIndex,left,top,width,height}` objects measured on displayed pages.
#[wasm_bindgen]
pub fn redact_pdf(pdf: &[u8], areas_json: &str) -> Result<Vec<u8>, JsError> {
    let areas = parse_redact_areas(pdf, areas_json)?;
    let output = pdf_core::redact_pure::redact_pure(pdf, &areas).map_err(redact_err)?;
    guard_output(output.pdf)
}

/// Permanently redacts service-compatible percentage areas and returns the
/// complete core report, including conservative whole-object removal notices.
/// This additive export preserves `redact_pdf` compatibility for callers that
/// only consume rewritten PDF bytes.
#[wasm_bindgen]
pub fn redact_pdf_with_report(pdf: &[u8], areas_json: &str) -> Result<JsValue, JsError> {
    let areas = parse_redact_areas(pdf, areas_json)?;
    let output = pdf_core::redact_pure::redact_pure(pdf, &areas).map_err(redact_err)?;
    let pdf_bytes = guard_output(output.pdf)?;

    let report = Object::new();
    Reflect::set(
        &report,
        &JsValue::from_str("pdf"),
        &Uint8Array::from(pdf_bytes.as_slice()).into(),
    )
    .map_err(|_| JsError::new("could not construct redaction report"))?;
    Reflect::set(
        &report,
        &JsValue::from_str("pageCount"),
        &JsValue::from_f64(f64::from(output.page_count)),
    )
    .map_err(|_| JsError::new("could not construct redaction report"))?;
    let warnings = Array::new();
    for warning in output.warnings {
        let item = Object::new();
        Reflect::set(
            &item,
            &JsValue::from_str("code"),
            &JsValue::from_str(warning.code.as_str()),
        )
        .map_err(|_| JsError::new("could not construct redaction warning code"))?;
        Reflect::set(
            &item,
            &JsValue::from_str("page"),
            &JsValue::from_f64(f64::from(warning.page)),
        )
        .map_err(|_| JsError::new("could not construct redaction warning page"))?;
        Reflect::set(
            &item,
            &JsValue::from_str("kind"),
            &JsValue::from_str(warning.kind.as_str()),
        )
        .map_err(|_| JsError::new("could not construct redaction warning kind"))?;
        Reflect::set(
            &item,
            &JsValue::from_str("message"),
            &JsValue::from_str(&warning.message),
        )
        .map_err(|_| JsError::new("could not construct redaction warning message"))?;
        warnings.push(&item);
    }
    Reflect::set(&report, &JsValue::from_str("warnings"), &warnings.into())
        .map_err(|_| JsError::new("could not construct redaction report"))?;
    Ok(report.into())
}

fn parse_redact_areas(pdf: &[u8], areas_json: &str) -> Result<Vec<RedactArea>, JsError> {
    guard_input_size(pdf.len(), MAX_INPUT_BYTES, "PDF")?;
    guard_input_size(areas_json.len(), MAX_INPUT_BYTES, "redaction JSON")?;
    let payload: Vec<RedactAreaPayload> = serde_json::from_str(areas_json)
        .map_err(|_| invalid("Invalid redaction payload (must be a JSON array)"))?;
    if payload.is_empty() {
        return Err(invalid("redactions must be a non-empty array"));
    }
    guard_redact_area_count(payload.len()).map_err(core_err)?;
    for (index, area) in payload.iter().enumerate() {
        if area.left < 0.0 || area.top < 0.0 || area.width <= 0.0 || area.height <= 0.0 {
            return Err(invalid(&format!(
                "redactions[{index}] must have positive size and non-negative position"
            )));
        }
        if area.left > 100.0 || area.top > 100.0 || area.width > 100.0 || area.height > 100.0 {
            return Err(invalid(&format!(
                "redactions[{index}] values must be within 0..100"
            )));
        }
        if area.left + area.width > 100.0 || area.top + area.height > 100.0 {
            return Err(invalid(&format!("redactions[{index}] exceeds page bounds")));
        }
    }
    Ok(payload.into_iter().map(Into::into).collect())
}

fn guard_redact_area_count(count: usize) -> pdf_core::Result<()> {
    if count > MAX_REDACTION_AREAS {
        return Err(PdfCoreError::ResourceLimit(format!(
            "redaction area count {count} exceeds limit {MAX_REDACTION_AREAS}"
        )));
    }
    Ok(())
}

/// CSV bytes → paginated PDF table (replaces the csv→HTML→Chromium route,
/// which forced landscape:'true' — landscape Letter matches that output).
#[wasm_bindgen]
pub fn csv_to_pdf(csv: &[u8]) -> Result<Vec<u8>, JsError> {
    guard_input_size(csv.len(), MAX_INPUT_BYTES, "CSV")?;
    let opts = pdf_core::generate::DocOptions {
        page_size: pdf_core::generate::PageSize::LetterLandscape,
        ..pdf_core::generate::DocOptions::default()
    };
    pdf_core::generate::csv_to_pdf(csv, &opts)
        .map_err(core_err)
        .and_then(guard_output)
}

/// Markdown → PDF (replaces the md→HTML→Chromium route).
#[wasm_bindgen]
pub fn markdown_to_pdf(markdown: &str) -> Result<Vec<u8>, JsError> {
    guard_input_size(markdown.len(), MAX_INPUT_BYTES, "Markdown")?;
    pdf_core::generate::markdown_to_pdf(markdown, &pdf_core::generate::DocOptions::default())
        .map_err(core_err)
        .and_then(guard_output)
}

/// Plain text → PDF.
#[wasm_bindgen]
pub fn text_to_pdf(text: &str) -> Result<Vec<u8>, JsError> {
    guard_input_size(text.len(), MAX_INPUT_BYTES, "text")?;
    pdf_core::generate::text_to_pdf(text, &pdf_core::generate::DocOptions::default())
        .map_err(core_err)
        .and_then(guard_output)
}

/// Page count of a PDF (errors on invalid/encrypted documents).
#[wasm_bindgen]
pub fn page_count(pdf: &[u8]) -> Result<u32, JsError> {
    guard_input_size(pdf.len(), MAX_INPUT_BYTES, "PDF")?;
    pdf_core::meta::page_count(pdf).map_err(core_err)
}

/// Structural validation; returns `{"pageCount":n,"version":"1.7","encrypted":false}`.
#[wasm_bindgen]
pub fn validate_pdf(pdf: &[u8]) -> Result<String, JsError> {
    guard_input_size(pdf.len(), MAX_INPUT_BYTES, "PDF")?;
    let info = pdf_core::meta::validate(pdf).map_err(core_err)?;
    Ok(format!(
        "{{\"pageCount\":{},\"version\":{},\"encrypted\":{}}}",
        info.page_count,
        serde_json::to_string(&info.version).map_err(internal_err)?,
        info.encrypted
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_color_parsing_matches_docyantra() {
        assert_eq!(parse_hex_color(Some("#4B5563")), [0x4B, 0x55, 0x63]);
        assert_eq!(parse_hex_color(Some("f00")), [0xFF, 0x00, 0x00]);
        assert_eq!(parse_hex_color(Some("not-a-color")), DEFAULT_COLOR);
        assert_eq!(parse_hex_color(None), DEFAULT_COLOR);
    }

    #[test]
    fn data_url_parsing() {
        assert!(parse_data_url("data:image/png;base64,aGVsbG8=").is_some());
        assert!(parse_data_url("data:text/plain;base64,aGVsbG8=").is_none());
        assert!(parse_data_url("data:image/png;base64,").is_none());
    }

    #[test]
    fn diagonal_grid_defaults_rotation() {
        let spec = spec_from_payload(r#"{"pattern":"diagonal_grid"}"#, None).unwrap();
        assert_eq!(spec.rotation_deg, -40.0);
        assert!(matches!(spec.pattern, Pattern::Grid));
        assert_eq!(spec.lines, vec!["CONFIDENTIAL".to_string()]);
    }

    #[test]
    fn image_kind_requires_bytes_and_drops_lines() {
        let json = r#"{"contentType":"image","lines":["ignored"]}"#;
        assert!(spec_from_payload(json, None).is_err());
        let png: &[u8] = &[137, 80, 78, 71];
        let spec = spec_from_payload(json, Some(png.to_vec())).unwrap();
        assert!(spec.lines.is_empty());
        assert!(spec.image.is_some());
    }

    #[test]
    fn hybrid_empty_lines_falls_back_to_confidential() {
        let spec = spec_from_payload(r#"{"contentType":"hybrid"}"#, Some(vec![1, 2, 3])).unwrap();
        assert_eq!(spec.lines, vec!["CONFIDENTIAL".to_string()]);
        assert!(spec.image.is_some());
    }

    #[test]
    fn redaction_area_capacity_uses_resource_limit_code() {
        let error =
            guard_redact_area_count(MAX_REDACTION_AREAS + 1).expect_err("redaction area capacity");
        assert_eq!(error.stable_code(), "resource_limit");
    }
}
