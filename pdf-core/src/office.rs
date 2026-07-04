//! Office (DOCX / PPTX / XLSX) → PDF via the pure-Rust `office2pdf` crate
//! (Typst-based rendering; no LibreOffice). Feature-gated behind `office`.
//!
//! Scope: modern OOXML only. Legacy binary formats (`doc`, `ppt`, `xls`) and
//! macro workbooks (`xlsm`) are NOT supported here — callers route those to
//! the LibreOffice service. Output is re-validated with pdf-core's own parser
//! before being returned, so a rendering bug can never hand callers a
//! corrupt document (they fall back instead).

use crate::error::{PdfCoreError, Result};

/// Office formats convertible by the pure-Rust engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficeFormat {
    Docx,
    Pptx,
    Xlsx,
}

impl OfficeFormat {
    /// Maps a lowercase file extension (no dot) to a supported format.
    /// Returns `None` for anything the pure-Rust engine cannot convert
    /// (legacy `doc`/`ppt`/`xls`, macro `xlsm`, unknown extensions).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.trim().to_ascii_lowercase().as_str() {
            "docx" => Some(OfficeFormat::Docx),
            "pptx" => Some(OfficeFormat::Pptx),
            "xlsx" => Some(OfficeFormat::Xlsx),
            _ => None,
        }
    }

    fn to_office2pdf(self) -> office2pdf::config::Format {
        match self {
            OfficeFormat::Docx => office2pdf::config::Format::Docx,
            OfficeFormat::Pptx => office2pdf::config::Format::Pptx,
            OfficeFormat::Xlsx => office2pdf::config::Format::Xlsx,
        }
    }
}

/// Conversion output: the PDF plus any non-fatal fidelity warnings the
/// renderer emitted (unsupported elements it skipped, etc.). Callers should
/// surface warnings in logs — they are the early signal of fidelity gaps.
#[derive(Debug, Clone)]
pub struct OfficeOutput {
    pub pdf: Vec<u8>,
    pub warnings: Vec<String>,
}

/// The zip entry (prefix) that identifies each OOXML flavor. The docx main
/// part is usually `word/document.xml` but OPC permits e.g.
/// `word/document2.xml`, so the docx needle is a prefix.
fn signature_entry(format: OfficeFormat) -> &'static [u8] {
    match format {
        OfficeFormat::Docx => b"word/document",
        OfficeFormat::Pptx => b"ppt/presentation.xml",
        OfficeFormat::Xlsx => b"xl/workbook.xml",
    }
}

/// Cheap content/format agreement check before parsing. The underlying
/// parsers can spend MINUTES on bytes of the wrong flavor (e.g. an xlsx
/// renamed to .docx) — unacceptable for a synchronous in-process engine
/// with no kill switch. Entry names appear in local file headers (near the
/// start) AND the central directory (at the end); scanning a generous head
/// + tail window covers even media-heavy documents whose central directory
/// exceeds a small tail window (~6,500+ entries).
fn looks_like(bytes: &[u8], format: OfficeFormat) -> bool {
    if !bytes.starts_with(b"PK") {
        return false;
    }
    const WINDOW: usize = 4 * 1024 * 1024;
    let needle = signature_entry(format);
    let contains = |hay: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
    if bytes.len() <= 2 * WINDOW {
        return contains(bytes);
    }
    contains(&bytes[..WINDOW]) || contains(&bytes[bytes.len() - WINDOW..])
}

/// Converts an Office document to PDF entirely in Rust.
///
/// The output is validated with [`crate::meta::page_count`] before being
/// returned: an empty or structurally broken render is an error, never a
/// silently-corrupt success.
pub fn office_to_pdf(bytes: &[u8], format: OfficeFormat) -> Result<OfficeOutput> {
    if bytes.is_empty() {
        return Err(PdfCoreError::InvalidInput(
            "office document is empty".to_string(),
        ));
    }
    if !looks_like(bytes, format) {
        return Err(PdfCoreError::InvalidInput(format!(
            "file content does not look like a {format:?} document"
        )));
    }

    let result = office2pdf::convert_bytes(
        bytes,
        format.to_office2pdf(),
        &office2pdf::config::ConvertOptions::default(),
    )
    .map_err(|e| PdfCoreError::InvalidInput(format!("office conversion failed: {e}")))?;

    // Trust nothing: re-parse the render with our own engine.
    let pages = crate::meta::page_count(&result.pdf)?;
    if pages == 0 {
        return Err(PdfCoreError::InvalidInput(
            "office conversion produced an empty PDF".to_string(),
        ));
    }

    Ok(OfficeOutput {
        pdf: result.pdf,
        warnings: result.warnings.iter().map(|w| w.to_string()).collect(),
    })
}

/// Convenience: convert by file extension. Errors with `Unsupported` for
/// extensions outside the pure-Rust engine's scope so callers can route
/// them to LibreOffice.
pub fn office_to_pdf_by_extension(bytes: &[u8], ext: &str) -> Result<OfficeOutput> {
    let format = OfficeFormat::from_extension(ext).ok_or_else(|| {
        PdfCoreError::Unsupported(format!(
            "extension '{ext}' is not convertible by the pure-Rust office engine (docx/pptx/xlsx only)"
        ))
    })?;
    office_to_pdf(bytes, format)
}
