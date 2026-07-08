//! # office-core — DocKosha's pure-Rust Office → PDF engine
//!
//! Own OOXML parsing, own layout, own PDF emission. **No LibreOffice, Chromium,
//! pdfium, or Typst** in the conversion path — only small, permissive, pure-Rust
//! leaf crates. Mirrors `pdf-core`'s conventions (`&[u8]` in / `Vec<u8>` out,
//! `thiserror` errors, doc-comments on every public item).
//!
//! Two contracts anchor everything (see `hyper-agent/office-engine/README.md`):
//!
//! 1. **Fail-closed envelope.** [`convert`] returns
//!    [`OfficeError::Unsupported`] for anything outside the documented envelope,
//!    never a degraded page.
//! 2. **DocProfile.** [`profile`] predicts the same verdict *without* layout so
//!    the router can decide cheaply before converting.
#![deny(missing_docs)]

pub mod emit;
pub mod error;
pub mod fonts;
pub mod layout;
pub mod opc;
pub mod oxml;
pub mod pptx;
pub mod profile;
pub mod text;
pub mod xlsx;

/// Fixture builders shared by unit and integration tests (docx zip assembly).
pub mod testkit;

pub use error::{OfficeError, Result, UnsupportedCode};
pub use profile::DocProfile;

/// The Office document formats the engine understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficeFormat {
    /// WordprocessingML (`.docx`).
    Docx,
    /// PresentationML (`.pptx`).
    Pptx,
    /// SpreadsheetML (`.xlsx` / `.xlsm`).
    Xlsx,
}

impl OfficeFormat {
    /// Maps a lowercase file extension to a format. `xlsm` is [`OfficeFormat::Xlsx`]
    /// (macros never render). Returns `None` for anything else (legacy binary
    /// formats included — those are the router's concern, not the engine's).
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "docx" => Some(OfficeFormat::Docx),
            "pptx" => Some(OfficeFormat::Pptx),
            "xlsx" | "xlsm" => Some(OfficeFormat::Xlsx),
            _ => None,
        }
    }
}

/// Optional inputs to [`convert`].
#[derive(Debug, Clone, Default)]
pub struct ConvertOptions {
    /// Extra font directories to scan (native only; ignored on wasm).
    pub font_dirs: Vec<std::path::PathBuf>,
    /// Raw font files to register (brand fonts; the wasm `register_font` path).
    pub font_bytes: Vec<Vec<u8>>,
    /// The original input file name, used by fields such as XLSX `&F`.
    pub source_name: Option<String>,
    /// Restrict XLSX conversion to these sheet names (by exact name). `None` = all.
    pub sheet_filter: Option<Vec<String>>,
}

/// The successful result of [`convert`].
#[derive(Debug)]
pub struct ConvertOutput {
    /// The emitted PDF bytes.
    pub pdf: Vec<u8>,
    /// Number of pages in the emitted PDF (from layout, never re-parsed).
    pub page_count: u32,
    /// Non-fatal, documented-lossy warnings accumulated during conversion.
    pub warnings: Vec<profile::Warning>,
}

/// Convert an Office document to PDF.
///
/// Fail-closed: anything outside the documented envelope returns
/// [`OfficeError::Unsupported`] (the same verdict [`profile`] predicts). `format`
/// is the caller's declared format; the container is still validated.
pub fn convert(bytes: &[u8], format: OfficeFormat, opts: &ConvertOptions) -> Result<ConvertOutput> {
    match format {
        OfficeFormat::Docx => convert_docx(bytes, opts),
        OfficeFormat::Pptx => convert_pptx(bytes, opts),
        OfficeFormat::Xlsx => convert_xlsx(bytes, opts),
    }
}

/// Converts a `.xlsx`/`.xlsm`: enforce the envelope, then render + emit.
fn convert_xlsx(bytes: &[u8], opts: &ConvertOptions) -> Result<ConvertOutput> {
    let prof = profile::run(bytes, OfficeFormat::Xlsx)?;
    if !prof.supported {
        let (code, detail) = prof
            .reasons
            .first()
            .map(|r| (r.code, r.detail.clone()))
            .unwrap_or((
                UnsupportedCode::UnknownElement,
                "unsupported document".into(),
            ));
        return Err(OfficeError::unsupported(code, detail));
    }
    let doc = xlsx::XlsxDoc::open(bytes, opts)?;
    let (laid, mut warnings) = xlsx::render_xlsx(&doc);
    let page_count = laid.pages.len() as u32;
    let pdf = emit::pdf::emit(&laid, &doc.fonts, document_id(bytes), &mut warnings)?;
    Ok(ConvertOutput {
        pdf,
        page_count,
        warnings,
    })
}

/// Converts a `.pptx`: enforce the envelope, then render + emit.
fn convert_pptx(bytes: &[u8], opts: &ConvertOptions) -> Result<ConvertOutput> {
    let prof = profile::run(bytes, OfficeFormat::Pptx)?;
    if !prof.supported {
        let (code, detail) = prof
            .reasons
            .first()
            .map(|r| (r.code, r.detail.clone()))
            .unwrap_or((
                UnsupportedCode::UnknownElement,
                "unsupported document".into(),
            ));
        return Err(OfficeError::unsupported(code, detail));
    }
    let doc = pptx::PptxDoc::open(bytes, opts)?;
    let (laid, mut warnings) = pptx::render_pptx(&doc);
    let page_count = laid.pages.len() as u32;
    let pdf = emit::pdf::emit(&laid, &doc.fonts, document_id(bytes), &mut warnings)?;
    Ok(ConvertOutput {
        pdf,
        page_count,
        warnings,
    })
}

/// Converts a `.docx`: enforce the envelope (via [`profile`]), then layout + emit.
fn convert_docx(bytes: &[u8], opts: &ConvertOptions) -> Result<ConvertOutput> {
    // Fail closed: the profile is the single source of the envelope verdict.
    let prof = profile::run(bytes, OfficeFormat::Docx)?;
    if !prof.supported {
        let (code, detail) = prof
            .reasons
            .first()
            .map(|r| (r.code, r.detail.clone()))
            .unwrap_or((
                UnsupportedCode::UnknownElement,
                "unsupported document".into(),
            ));
        return Err(OfficeError::unsupported(code, detail));
    }

    let doc = layout::WordDoc::open(bytes, opts)?;
    let (laid, mut warnings) = layout::layout_docx(&doc);
    let page_count = laid.pages.len() as u32;
    let pdf = emit::pdf::emit(&laid, &doc.fonts, document_id(bytes), &mut warnings)?;
    Ok(ConvertOutput {
        pdf,
        page_count,
        warnings,
    })
}

/// Deterministic 16-byte document id: first half of `SHA-256(input ‖ version)`.
fn document_id(bytes: &[u8]) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.update(DocProfile::VERSION.as_bytes());
    let digest = hasher.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

/// Cheap envelope check *without* layout — the router contract.
///
/// See the plan README for the DocProfile JSON schema. Never returns `Err` for a
/// well-formed file (reasons carry the verdict); `Err` is reserved for
/// container-level failures (invalid/encrypted/legacy), which the wasm boundary
/// maps to JSON via [`DocProfile::from_error`].
pub fn profile(bytes: &[u8], format: OfficeFormat) -> Result<DocProfile> {
    profile::run(bytes, format)
}
